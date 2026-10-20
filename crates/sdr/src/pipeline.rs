//! The DSP threads: I/Q in from an [`IqSource`], spectrum frames and receiver
//! audio out.
//!
//! Two threads. The pipeline thread reads the source and runs the receiver,
//! and hands each block of I/Q to a spectrum thread, which runs the spectrum
//! and the zoom FFT. A million-point FFT at 60 frames a second is most of a
//! core; on its own thread it can only slow the display, never starve the
//! receiver's audio or let the radio's USB ring overflow. If the spectrum
//! thread falls behind, blocks are dropped for it (and counted) rather than
//! queued without end.
//!
//! The UI talks to the pipeline over a crossbeam channel ([`Command`]) and
//! reads what it made from a [`Snapshot`] behind a mutex, which the threads
//! hold only long enough to swap in a finished frame — every frame is an
//! `Arc`, so a reader cloning the snapshot copies pointers, not spectra.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{
    Receiver as ChanRx, Sender, TryRecvError, TrySendError, bounded, unbounded,
};

use crate::agc::AgcParams;
use crate::audio::AudioFeed;
use crate::ddc::DecimatorKind;
use crate::passband::FilterShape;
use crate::receiver::Receiver;
use crate::source::IqSource;
use crate::spectrum::{Averaging, Overlap, Spectrum, SpectrumConfig, SpectrumFrame, Window};
use crate::types::{AgcMode, Mode};
use crate::zoom::{ZoomRequest, ZoomSpectrum};

/// Takes a copy of every block of I/Q the pipeline reads: interleaved
/// `I, Q, …`, the span's centre and the complex rate, on the pipeline's own
/// thread. It must return at once (hand the block to a queue that drops when
/// full), or the receiver's audio and the radio's USB ring fall behind.
pub type IqTap = Box<dyn FnMut(&[f32], f64, f64) + Send>;

/// Takes a copy of every block of the receiver's 48 kHz mono audio for the
/// decoders — levelled by the decoders' own AGC, not the listening one (see
/// [`Receiver::decoder_audio`]) — on the pipeline's own thread, under the
/// same rule as [`IqTap`].
pub type AudioTap = Box<dyn FnMut(&[f32]) + Send>;

/// What the UI can ask of the pipeline.
pub enum Command {
    /// Receiver dial, Hz from the centre.
    SetOffset(f64),
    SetMode(Mode),
    /// `[low, high]` Hz relative to the dial.
    SetPassband(f32, f32),
    SetAgc(AgcMode),
    /// The listening AGC's attack, decay, hang and target.
    SetAgcParams(AgcParams),
    /// The decoders' AGC.
    SetDecoderAgc(AgcParams),
    /// Fixed gain while the AGC is off, dB.
    SetGain(f32),
    /// How the receiver decimates to 48 kHz.
    SetDecimator(DecimatorKind),
    /// How steep the passband's skirts are.
    SetFilterShape(FilterShape),
    SetFftSize(usize),
    SetWindow(Window),
    SetOverlap(Overlap),
    SetAveraging(Averaging),
    SetPeakHold(bool),
    SetPeakDecay(f32),
    SetFps(f32),
    /// dBFS → dBm, for the spectrum and the S-meter alike.
    SetCalibration(f32),
    /// Ask the source to move the radio's centre.
    SetCenter(f64),
    /// Where the audio goes; `None` discards it.
    SetAudio(Option<AudioFeed>),
    /// How many frames of history to keep for the waterfall and 3D views.
    SetHistory(usize),
    /// Run a zoom FFT on one region (`None` stops it); see [`crate::zoom`].
    SetZoom(Option<ZoomRequest>),
    /// Copy the I/Q to decoders and outside programs; `None` stops it.
    SetIqTap(Option<IqTap>),
    /// Copy the receiver's decoder audio to decoders and outside programs;
    /// `None` stops it.
    SetAudioTap(Option<AudioTap>),
    /// Run the CPU spectrum (true, the default) or not: off while the
    /// scope computes its spectrum on the GPU from the I/Q tap, so the I/Q
    /// is not also queued for a spectrum nobody reads.
    SetSpectrumOn(bool),
    /// Start the CPU spectrum (and its zoom) afresh: the averages, the held
    /// peaks and the history are forgotten, and nothing already queued for
    /// it is used. The scope sends this when a transmission ends, so what
    /// the receiver heard during the over does not linger in the averages.
    /// [`Snapshot::spectrum_resets`] counts the ones done.
    ResetSpectrum,
    /// Stop reading the source (true) or start again.
    Pause(bool),
    Stop,
}

/// How a pipeline starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PipelineConfig {
    pub spectrum: SpectrumConfig,
    pub offset_hz: f64,
    pub mode: Mode,
    /// `None` takes the mode's default.
    pub passband: Option<(f32, f32)>,
    pub agc: AgcMode,
    /// `None` takes the AGC mode's preset.
    pub agc_params: Option<AgcParams>,
    pub decoder_agc: AgcParams,
    pub gain_db: f32,
    pub decimator: DecimatorKind,
    pub filter_shape: FilterShape,
    /// Frames of history kept.
    pub history: usize,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        PipelineConfig {
            spectrum: SpectrumConfig::default(),
            offset_hz: 0.0,
            mode: Mode::Usb,
            passband: None,
            agc: AgcMode::Med,
            agc_params: None,
            decoder_agc: AgcParams::DECODER,
            gain_db: 20.0,
            decimator: DecimatorKind::default(),
            filter_shape: FilterShape::default(),
            history: 256,
        }
    }
}

/// What the UI reads each frame.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// The newest spectrum.
    pub latest: Option<Arc<SpectrumFrame>>,
    /// Recent spectra, oldest first, newest last (the newest is `latest`).
    pub history: VecDeque<Arc<SpectrumFrame>>,
    /// Counts frames published; a UI can repaint only when it moves.
    pub generation: u64,
    /// The newest zoom-FFT spectrum, while a zoom is running.
    pub zoom_latest: Option<Arc<SpectrumFrame>>,
    /// Recent zoom spectra, as `history`.
    pub zoom_history: VecDeque<Arc<SpectrumFrame>>,
    /// Counts zoom frames published.
    pub zoom_generation: u64,
    /// The zoom the spectrum thread is running now.
    pub zoom: Option<ZoomRequest>,
    /// The receiver's passband level, dBm.
    pub level_dbm: f32,
    pub sample_rate: f64,
    pub center_hz: f64,
    /// Receiver settings as the thread has them now.
    pub offset_hz: f64,
    pub mode: Mode,
    pub passband: (f32, f32),
    pub agc: AgcMode,
    pub agc_params: AgcParams,
    pub decoder_agc: AgcParams,
    pub decimator: DecimatorKind,
    pub filter_shape: FilterShape,
    /// The source is still delivering.
    pub alive: bool,
    pub paused: bool,
    /// A short description of the source.
    pub source: String,
    /// Share of real time the receiver took over the last second (1.0 = a
    /// whole core's worth of falling behind).
    pub load: f32,
    /// The same for the spectrum thread.
    pub spectrum_load: f32,
    /// Blocks of I/Q the spectrum thread was too busy to take, since start.
    pub spectrum_drops: u64,
    /// Complex samples processed since start.
    pub samples: u64,
    /// Samples that came from the source as something other than a number
    /// (NaN or infinite), replaced with zero before anything used them.
    pub nonfinite: u64,
    /// [`Command::ResetSpectrum`]s the spectrum thread has carried out.
    pub spectrum_resets: u64,
}

/// A running pipeline. Dropping it stops the threads.
pub struct Pipeline {
    tx: Sender<Command>,
    shared: Arc<Mutex<Snapshot>>,
    join: Option<JoinHandle<Box<dyn IqSource>>>,
}

impl Pipeline {
    pub fn spawn(source: Box<dyn IqSource>, cfg: PipelineConfig) -> Pipeline {
        let (tx, rx) = unbounded();
        let shared = Arc::new(Mutex::new(Snapshot {
            sample_rate: source.sample_rate(),
            center_hz: source.center_hz(),
            alive: true,
            source: source.label(),
            ..Default::default()
        }));
        let s = shared.clone();
        let join = std::thread::Builder::new()
            .name("sdr-pipeline".into())
            .spawn(move || {
                elad_iq::priority::raise_current_thread(elad_iq::priority::Priority::Highest);
                run(source, cfg, rx, s)
            })
            .expect("spawn the DSP thread");
        Pipeline { tx, shared, join: Some(join) }
    }

    pub fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }

    pub fn set_offset_hz(&self, hz: f64) {
        self.send(Command::SetOffset(hz));
    }
    pub fn set_mode(&self, m: Mode) {
        self.send(Command::SetMode(m));
    }
    pub fn set_passband(&self, lo: f32, hi: f32) {
        self.send(Command::SetPassband(lo, hi));
    }
    pub fn set_audio(&self, feed: Option<AudioFeed>) {
        self.send(Command::SetAudio(feed));
    }

    /// A copy of the snapshot (frames shared, not copied).
    pub fn snapshot(&self) -> Snapshot {
        self.lock().clone()
    }

    /// Borrow the snapshot in place, for a reader that wants only part of it.
    pub fn lock(&self) -> MutexGuard<'_, Snapshot> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn latest(&self) -> Option<Arc<SpectrumFrame>> {
        self.lock().latest.clone()
    }

    /// Stop the threads and get the source back (to release a radio, say).
    pub fn stop(mut self) -> Option<Box<dyn IqSource>> {
        self.send(Command::Stop);
        self.join.take().and_then(|j| j.join().ok())
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Stop);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Floats read per pass: about 5 ms at 6144 kHz, 85 ms at 192 kHz.
const BLOCK: usize = 2 * 32_768;

/// Blocks the spectrum thread may have waiting: about a second at 6144 kHz.
const SPECTRUM_QUEUE: usize = 192;

/// One block of I/Q for the spectrum thread.
/// The fewest complex samples handed to the spectrum thread at once: a
/// source that trickles a few samples per read would otherwise send a flood
/// of tiny blocks, each a channel message, and overflow the queue.
const MIN_SPECTRUM_BLOCK: usize = 8192;

struct IqBlock {
    iq: Vec<f32>,
    center: f64,
    rate: f64,
    /// The spectrum resets asked for before it was read: a block from
    /// before the newest reset is not used.
    resets: u64,
}

/// What the spectrum thread is told besides I/Q.
enum SpecMsg {
    Config(SpectrumConfig),
    History(usize),
    Zoom(Option<ZoomRequest>),
    /// Start again; blocks from before reset number `.0` are dropped.
    Reset(u64),
}

/// Replace anything in `iq` that is not a number with zero, and say how
/// many there were. One NaN in an exponential average stays there for good
/// (`a + α(x − a)` is NaN once `a` is), which would blank the spectrum and
/// silence the receiver until a restart.
pub(crate) fn sanitise(iq: &mut [f32]) -> usize {
    if iq.iter().all(|x| x.is_finite()) {
        return 0;
    }
    let mut bad = 0;
    for x in iq.iter_mut().filter(|x| !x.is_finite()) {
        *x = 0.0;
        bad += 1;
    }
    bad
}

fn run(
    mut source: Box<dyn IqSource>,
    cfg: PipelineConfig,
    rx: ChanRx<Command>,
    shared: Arc<Mutex<Snapshot>>,
) -> Box<dyn IqSource> {
    let mut rate = source.sample_rate();
    let mut spectrum_cfg = cfg.spectrum;
    let mut receiver = Receiver::new(rate);
    receiver.set_decimator(cfg.decimator);
    receiver.set_filter_shape(cfg.filter_shape);
    receiver.set_mode(cfg.mode);
    if let Some((lo, hi)) = cfg.passband {
        receiver.set_passband(lo, hi);
    }
    receiver.set_offset_hz(cfg.offset_hz);
    receiver.set_agc(cfg.agc);
    if let Some(p) = cfg.agc_params {
        receiver.set_agc_params(p);
    }
    receiver.set_decoder_agc(cfg.decoder_agc);
    receiver.set_gain_db(cfg.gain_db);
    receiver.set_calibration_db(spectrum_cfg.calibration_db);
    let mut audio_feed: Option<AudioFeed> = None;
    let mut iq_tap: Option<IqTap> = None;
    let mut audio_tap: Option<AudioTap> = None;
    let mut paused = false;
    let mut spectrum_on = true;

    let (block_tx, block_rx) = bounded::<IqBlock>(SPECTRUM_QUEUE);
    let (msg_tx, msg_rx) = unbounded::<SpecMsg>();
    let spec_shared = shared.clone();
    let history = cfg.history.max(1);
    let spec_join = std::thread::Builder::new()
        .name("sdr-spectrum".into())
        .spawn(move || run_spectrum(spectrum_cfg, history, rate, block_rx, msg_rx, spec_shared))
        .expect("spawn the spectrum thread");
    let mut drops = 0u64;
    let mut resets = 0u64;
    let mut nonfinite = 0u64;
    let mut pending = IqBlock { iq: Vec::new(), center: 0.0, rate, resets };

    let mut buf = vec![0.0f32; BLOCK];
    let mut audio = Vec::with_capacity(4096);
    let mut samples = 0u64;
    let (mut busy, mut window_start, mut window_samples) = (Duration::ZERO, Instant::now(), 0u64);
    let mut load = 0.0f32;

    'outer: loop {
        // Commands first, all of them.
        loop {
            match rx.try_recv() {
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => break 'outer,
                Ok(c) => {
                    let mut sc = spectrum_cfg;
                    match c {
                        Command::SetOffset(hz) => receiver.set_offset_hz(hz),
                        Command::SetMode(m) => receiver.set_mode(m),
                        Command::SetPassband(lo, hi) => receiver.set_passband(lo, hi),
                        Command::SetAgc(a) => receiver.set_agc(a),
                        Command::SetAgcParams(p) => receiver.set_agc_params(p),
                        Command::SetDecoderAgc(p) => receiver.set_decoder_agc(p),
                        Command::SetGain(g) => receiver.set_gain_db(g),
                        Command::SetDecimator(k) => receiver.set_decimator(k),
                        Command::SetFilterShape(f) => receiver.set_filter_shape(f),
                        Command::SetFftSize(n) => sc.fft_size = n,
                        Command::SetWindow(w) => sc.window = w,
                        Command::SetOverlap(o) => sc.overlap = o,
                        Command::SetAveraging(a) => sc.averaging = a,
                        Command::SetPeakHold(on) => sc.peak_hold = on,
                        Command::SetPeakDecay(d) => sc.peak_decay_db_per_s = d,
                        Command::SetFps(f) => sc.fps = f,
                        Command::SetCalibration(db) => {
                            sc.calibration_db = db;
                            receiver.set_calibration_db(db);
                        }
                        Command::SetCenter(hz) => source.set_center_hz(hz),
                        Command::SetAudio(f) => audio_feed = f,
                        Command::SetHistory(n) => {
                            let _ = msg_tx.send(SpecMsg::History(n.max(1)));
                        }
                        Command::SetZoom(z) => {
                            let _ = msg_tx.send(SpecMsg::Zoom(z));
                        }
                        Command::SetIqTap(t) => iq_tap = t,
                        Command::SetAudioTap(t) => audio_tap = t,
                        Command::SetSpectrumOn(on) => {
                            spectrum_on = on;
                            if !on {
                                pending.iq.clear();
                            }
                        }
                        Command::ResetSpectrum => {
                            resets += 1;
                            pending.iq.clear();
                            let _ = msg_tx.send(SpecMsg::Reset(resets));
                        }
                        Command::Pause(p) => paused = p,
                        Command::Stop => unreachable!(),
                    }
                    if sc != spectrum_cfg {
                        spectrum_cfg = sc;
                        let _ = msg_tx.send(SpecMsg::Config(sc));
                    }
                }
                Err(TryRecvError::Empty) => break,
            }
        }

        if paused {
            // Keep the source drained so a radio's ring does not overflow.
            let _ = source.read(&mut buf);
            publish(&shared, |s| s.paused = true);
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }

        let n = source.read(&mut buf);
        nonfinite += sanitise(&mut buf[..n]) as u64;
        if n == 0 {
            if !source.is_alive() {
                publish(&shared, |s| s.alive = false);
                std::thread::sleep(Duration::from_millis(50));
            } else {
                std::thread::sleep(Duration::from_millis(2));
            }
            continue;
        }

        let new_rate = source.sample_rate();
        if new_rate != rate {
            rate = new_rate;
            receiver.set_sample_rate(rate);
        }
        let center = source.center_hz();
        let t = Instant::now();
        // Gather the I/Q into blocks of at least MIN_SPECTRUM_BLOCK, each
        // all at one centre and rate.
        // Nothing is queued while the scope draws its spectrum from the
        // I/Q tap on the GPU.
        if spectrum_on
            && !pending.iq.is_empty()
            && (pending.center != center || pending.rate != rate)
        {
            let full = std::mem::take(&mut pending.iq);
            match block_tx.try_send(IqBlock {
                iq: full,
                center: pending.center,
                rate: pending.rate,
                resets,
            }) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => drops += 1,
                Err(TrySendError::Disconnected(_)) => break 'outer,
            }
        }
        pending.center = center;
        pending.rate = rate;
        if spectrum_on {
            pending.iq.extend_from_slice(&buf[..n]);
        }
        if pending.iq.len() >= MIN_SPECTRUM_BLOCK * 2 {
            let full = std::mem::replace(&mut pending.iq, Vec::with_capacity(BLOCK));
            match block_tx.try_send(IqBlock { iq: full, center, rate, resets }) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => drops += 1,
                Err(TrySendError::Disconnected(_)) => break 'outer,
            }
        }
        if let Some(tap) = iq_tap.as_mut() {
            tap(&buf[..n], center, rate);
        }
        audio.clear();
        receiver.process(&buf[..n], &mut audio);
        if let Some(f) = audio_feed.as_mut() {
            f.push(&audio);
        }
        if let Some(tap) = audio_tap.as_mut()
            && !receiver.decoder_audio().is_empty()
        {
            tap(receiver.decoder_audio());
        }
        busy += t.elapsed();
        samples += (n / 2) as u64;
        window_samples += (n / 2) as u64;
        if window_start.elapsed() >= Duration::from_secs(1) && window_samples > 0 {
            load = (busy.as_secs_f64() / (window_samples as f64 / rate)) as f32;
            busy = Duration::ZERO;
            window_samples = 0;
            window_start = Instant::now();
        }

        let level = receiver.level_dbm();
        let alive = source.is_alive();
        publish(&shared, |s| {
            s.level_dbm = level;
            s.sample_rate = rate;
            s.center_hz = center;
            s.offset_hz = receiver.offset_hz();
            s.mode = receiver.mode();
            s.passband = receiver.passband();
            s.agc = receiver.agc();
            s.agc_params = receiver.agc_params();
            s.decoder_agc = receiver.decoder_agc();
            s.decimator = receiver.decimator();
            s.filter_shape = receiver.filter_shape();
            s.alive = alive;
            s.paused = false;
            s.load = load;
            s.samples = samples;
            s.spectrum_drops = drops;
            s.nonfinite = nonfinite;
        });
    }
    drop(block_tx);
    drop(msg_tx);
    let _ = spec_join.join();
    source
}

/// The spectrum thread: the main spectrum and, when asked for, the zoom.
fn run_spectrum(
    mut cfg: SpectrumConfig,
    mut history_len: usize,
    mut rate: f64,
    blocks: ChanRx<IqBlock>,
    msgs: ChanRx<SpecMsg>,
    shared: Arc<Mutex<Snapshot>>,
) {
    let mut spectrum = Spectrum::new(cfg, rate);
    let mut zoom_req: Option<ZoomRequest> = None;
    let mut zoom: Option<ZoomSpectrum> = None;
    let mut frames = Vec::new();
    let mut zframes = Vec::new();
    let mut resets = 0u64;
    let (mut busy, mut window_start, mut window_samples) = (Duration::ZERO, Instant::now(), 0u64);
    let mut load = 0.0f32;
    loop {
        while let Ok(m) = msgs.try_recv() {
            match m {
                SpecMsg::Config(c) => {
                    spectrum.set_config(c);
                    cfg = spectrum.config();
                    if let Some(z) = zoom.as_mut() {
                        z.set_config(cfg);
                    }
                }
                SpecMsg::History(n) => history_len = n,
                SpecMsg::Zoom(z) => zoom_req = z,
                SpecMsg::Reset(n) => {
                    resets = n;
                    spectrum.reset();
                    zoom = None;
                    publish(&shared, |s| {
                        s.latest = None;
                        s.history.clear();
                        s.zoom_latest = None;
                        s.zoom_history.clear();
                        s.spectrum_resets = n;
                    });
                }
            }
        }
        let block = match blocks.recv_timeout(Duration::from_millis(50)) {
            Ok(b) => b,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        };
        if block.resets < resets {
            // Heard before the reset (during the over): not wanted.
            continue;
        }
        let t = Instant::now();
        if block.rate != rate {
            rate = block.rate;
            spectrum.set_sample_rate(rate);
        }
        // Build, move or drop the zoom to match what was asked.
        match zoom_req {
            None => zoom = None,
            // A zoom that would not decimate gains nothing over the main
            // spectrum.
            Some(req) if crate::zoom::zoom_factor(rate, req.width_hz) <= 1 => zoom = None,
            Some(req) => match zoom.as_mut() {
                Some(z) if z.fits(req, rate) => z.set_center(req.center_hz),
                _ => zoom = Some(ZoomSpectrum::new(req, rate, cfg)),
            },
        }
        spectrum.process(&block.iq, block.center, &mut frames);
        if let Some(z) = zoom.as_mut() {
            z.process(&block.iq, block.center, &mut zframes);
        }
        busy += t.elapsed();
        window_samples += (block.iq.len() / 2) as u64;
        if window_start.elapsed() >= Duration::from_secs(1) && window_samples > 0 {
            load = (busy.as_secs_f64() / (window_samples as f64 / rate)) as f32;
            busy = Duration::ZERO;
            window_samples = 0;
            window_start = Instant::now();
        }
        let made: Vec<Arc<SpectrumFrame>> = frames.drain(..).map(Arc::new).collect();
        let zmade: Vec<Arc<SpectrumFrame>> = zframes.drain(..).map(Arc::new).collect();
        let zoom_now = zoom.as_ref().map(|z| z.request());
        publish(&shared, |s| {
            for f in made {
                s.history.push_back(f.clone());
                s.latest = Some(f);
                s.generation += 1;
            }
            while s.history.len() > history_len {
                s.history.pop_front();
            }
            for f in zmade {
                s.zoom_history.push_back(f.clone());
                s.zoom_latest = Some(f);
                s.zoom_generation += 1;
            }
            if zoom_now.is_none() {
                s.zoom_latest = None;
                s.zoom_history.clear();
            }
            while s.zoom_history.len() > history_len {
                s.zoom_history.pop_front();
            }
            s.zoom = zoom_now;
            s.spectrum_load = load;
        });
    }
}

fn publish(shared: &Mutex<Snapshot>, f: impl FnOnce(&mut Snapshot)) {
    let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut s);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::audio_ring;
    use crate::source::{SynthSignal, SyntheticSource};

    fn wait_for(p: &Pipeline, what: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let t = Instant::now();
        loop {
            let s = p.snapshot();
            if what(&s) {
                return s;
            }
            assert!(t.elapsed() < Duration::from_secs(20), "timed out: {s:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn the_cpu_spectrum_can_be_switched_off_for_the_gpu() {
        let src = SyntheticSource::new(192_000.0, 7.1e6, -80.0, vec![]);
        let cfg = PipelineConfig {
            spectrum: SpectrumConfig { fft_size: 4096, fps: 60.0, ..Default::default() },
            history: 4,
            ..Default::default()
        };
        let p = Pipeline::spawn(Box::new(src), cfg);
        wait_for(&p, |s| s.generation > 3);
        p.send(Command::SetSpectrumOn(false));
        std::thread::sleep(Duration::from_millis(300));
        let g = p.snapshot().generation;
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(p.snapshot().generation, g, "no frames while it is off");
        p.send(Command::SetSpectrumOn(true));
        wait_for(&p, |s| s.generation > g + 2);
    }

    #[test]
    fn frames_history_audio_and_commands_flow() {
        let rate = 192_000.0;
        let src =
            SyntheticSource::new(rate, 7.1e6, -80.0, vec![SynthSignal::carrier(12_500.0, -30.0)]);
        let cfg = PipelineConfig {
            spectrum: SpectrumConfig {
                fft_size: 4096,
                fps: 60.0,
                calibration_db: -7.0,
                ..Default::default()
            },
            offset_hz: 11_500.0,
            agc: AgcMode::Off,
            gain_db: 0.0,
            history: 8,
            ..Default::default()
        };
        let p = Pipeline::spawn(Box::new(src), cfg);
        let (feed, mut audio_rx, _) = audio_ring();
        p.set_audio(Some(feed));
        // Taps see the same I/Q and audio, with the span's centre and rate.
        let iq_seen = Arc::new(Mutex::new((0usize, 0.0f64, 0.0f64)));
        let audio_seen = Arc::new(Mutex::new(0usize));
        let (i2, a2) = (iq_seen.clone(), audio_seen.clone());
        p.send(Command::SetIqTap(Some(Box::new(move |iq, c, r| {
            let mut g = i2.lock().unwrap();
            *g = (g.0 + iq.len(), c, r);
        }))));
        p.send(Command::SetAudioTap(Some(Box::new(move |a| *a2.lock().unwrap() += a.len()))));

        let s = wait_for(&p, |s| s.history.len() == 8 && s.generation > 20);
        let f = s.latest.clone().unwrap();
        assert_eq!(f.center_hz, 7.1e6);
        let k = f.bin_of(7.1125e6).unwrap();
        // A third of a bin off the grid: up to 0.4 dB of scalloping.
        assert!(f.db[k] < -36.95 && f.db[k] > -37.6, "{} dBm", f.db[k]);
        assert!((s.level_dbm - -37.0).abs() < 0.5, "meter {}", s.level_dbm);
        assert!(Arc::ptr_eq(s.history.back().unwrap(), &f));
        assert!(audio_rx.slots() > 1000, "audio is flowing");
        let (floats, c, r) = *iq_seen.lock().unwrap();
        assert!(floats > 0 && floats % 2 == 0);
        assert_eq!((c, r), (7.1e6, rate));
        // 48 kHz audio for 4 times fewer complex samples at 192 kHz.
        let heard = *audio_seen.lock().unwrap();
        assert!(heard > 1000, "the audio tap is fed");
        p.send(Command::SetIqTap(None));

        // Move the receiver off the carrier: the meter drops to the noise.
        p.set_offset_hz(-50_000.0);
        p.send(Command::SetFftSize(16_384));
        let s = wait_for(&p, |s| {
            s.offset_hz == -50_000.0
                && s.level_dbm < -70.0
                && s.latest.as_ref().unwrap().db.len() == 16_384
        });
        assert!(s.alive);
        // A zoom on the carrier: frames a few kHz wide with fine bins.
        p.send(Command::SetZoom(Some(ZoomRequest {
            center_hz: 7.1125e6,
            width_hz: 4000.0,
            fft_size: 4096,
        })));
        let s = wait_for(&p, |s| s.zoom_latest.is_some() && s.zoom_generation > 2);
        let z = s.zoom_latest.unwrap();
        assert_eq!(z.span_hz, rate / 32.0);
        let pk = z.peak_in(7.1124e6, 7.1126e6).unwrap();
        assert!((pk.hz - 7.1125e6).abs() < 0.5, "{}", pk.hz);
        p.send(Command::SetZoom(None));
        wait_for(&p, |s| s.zoom.is_none() && s.zoom_latest.is_none());
        p.send(Command::Pause(true));
        wait_for(&p, |s| s.paused);
        while audio_rx.pop().is_ok() {}
        let src = p.stop().expect("the source comes back");
        assert_eq!(src.center_hz(), 7.1e6);
    }

    /// A transmission whose I/Q is not numbers (the worst a converter could
    /// hand on), then a reset as the scope sends at the end of an over: the
    /// receiver and the spectrum carry on, and nothing from before the reset
    /// is shown after it.
    #[test]
    fn garbage_during_an_over_does_not_outlast_it() {
        use crate::source::{TxEffects, TxSim};
        let rate = 192_000.0;
        let src = SyntheticSource::new(rate, 7.1e6, -80.0, vec![SynthSignal::carrier(10e3, -40.0)])
            .paced();
        let fx = TxEffects { nan: true, ..Default::default() };
        let sim = TxSim::new(src, 0.3, 0.8, fx);
        let keyed = sim.keyed();
        let cfg = PipelineConfig {
            spectrum: SpectrumConfig {
                fft_size: 4096,
                fps: 30.0,
                averaging: crate::Averaging::X8,
                peak_hold: true,
                ..Default::default()
            },
            offset_hz: 10e3,
            history: 4,
            ..Default::default()
        };
        let p = Pipeline::spawn(Box::new(sim), cfg);
        wait_for(&p, |_| keyed.load(std::sync::atomic::Ordering::Relaxed));
        let s =
            wait_for(&p, |s| s.nonfinite > 0 && !keyed.load(std::sync::atomic::Ordering::Relaxed));
        assert!(s.level_dbm.is_finite(), "the meter is a number: {}", s.level_dbm);
        p.send(Command::ResetSpectrum);
        let s = wait_for(&p, |s| s.spectrum_resets == 1 && s.latest.is_some());
        let f = s.latest.unwrap();
        assert!(f.db.iter().all(|d| d.is_finite()));
        let k = f.bin_of(7.11e6).unwrap();
        assert!((f.db[k] - -40.0).abs() < 2.0, "the carrier is back: {} dBFS", f.db[k]);
        assert!(f.noise_dbm < -90.0, "and the noise: {}", f.noise_dbm);
        let pk = f.peak.as_ref().unwrap();
        assert!(pk.iter().all(|d| d.is_finite() && *d < -30.0), "no peak held from the over");
    }

    #[test]
    fn samples_that_are_not_numbers_become_zero() {
        let mut v = [1.0, f32::NAN, -0.5, f32::INFINITY, f32::NEG_INFINITY, 0.25];
        assert_eq!(sanitise(&mut v), 3);
        assert_eq!(v, [1.0, 0.0, -0.5, 0.0, 0.0, 0.25]);
        assert_eq!(sanitise(&mut v), 0);
    }
}
