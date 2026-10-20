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

use crate::audio::AudioFeed;
use crate::receiver::Receiver;
use crate::source::IqSource;
use crate::spectrum::{Averaging, Overlap, Spectrum, SpectrumConfig, SpectrumFrame, Window};
use crate::types::{AgcMode, Mode};
use crate::zoom::{ZoomRequest, ZoomSpectrum};

/// What the UI can ask of the pipeline.
pub enum Command {
    /// Receiver dial, Hz from the centre.
    SetOffset(f64),
    SetMode(Mode),
    /// `[low, high]` Hz relative to the dial.
    SetPassband(f32, f32),
    SetAgc(AgcMode),
    /// Fixed gain while the AGC is off, dB.
    SetGain(f32),
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
    pub gain_db: f32,
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
            gain_db: 20.0,
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
            .spawn(move || run(source, cfg, rx, s))
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
}

/// What the spectrum thread is told besides I/Q.
enum SpecMsg {
    Config(SpectrumConfig),
    History(usize),
    Zoom(Option<ZoomRequest>),
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
    receiver.set_mode(cfg.mode);
    if let Some((lo, hi)) = cfg.passband {
        receiver.set_passband(lo, hi);
    }
    receiver.set_offset_hz(cfg.offset_hz);
    receiver.set_agc(cfg.agc);
    receiver.set_gain_db(cfg.gain_db);
    receiver.set_calibration_db(spectrum_cfg.calibration_db);
    let mut audio_feed: Option<AudioFeed> = None;
    let mut paused = false;

    let (block_tx, block_rx) = bounded::<IqBlock>(SPECTRUM_QUEUE);
    let (msg_tx, msg_rx) = unbounded::<SpecMsg>();
    let spec_shared = shared.clone();
    let history = cfg.history.max(1);
    let spec_join = std::thread::Builder::new()
        .name("sdr-spectrum".into())
        .spawn(move || run_spectrum(spectrum_cfg, history, rate, block_rx, msg_rx, spec_shared))
        .expect("spawn the spectrum thread");
    let mut drops = 0u64;
    let mut pending = IqBlock { iq: Vec::new(), center: 0.0, rate };

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
                        Command::SetGain(g) => receiver.set_gain_db(g),
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
        if !pending.iq.is_empty() && (pending.center != center || pending.rate != rate) {
            let full = std::mem::take(&mut pending.iq);
            match block_tx.try_send(IqBlock {
                iq: full,
                center: pending.center,
                rate: pending.rate,
            }) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => drops += 1,
                Err(TrySendError::Disconnected(_)) => break 'outer,
            }
        }
        pending.center = center;
        pending.rate = rate;
        pending.iq.extend_from_slice(&buf[..n]);
        if pending.iq.len() >= MIN_SPECTRUM_BLOCK * 2 {
            let full = std::mem::replace(&mut pending.iq, Vec::with_capacity(BLOCK));
            match block_tx.try_send(IqBlock { iq: full, center, rate }) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => drops += 1,
                Err(TrySendError::Disconnected(_)) => break 'outer,
            }
        }
        audio.clear();
        receiver.process(&buf[..n], &mut audio);
        if let Some(f) = audio_feed.as_mut() {
            f.push(&audio);
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
            s.alive = alive;
            s.paused = false;
            s.load = load;
            s.samples = samples;
            s.spectrum_drops = drops;
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
            }
        }
        let block = match blocks.recv_timeout(Duration::from_millis(50)) {
            Ok(b) => b,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        };
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

        let s = wait_for(&p, |s| s.history.len() == 8 && s.generation > 20);
        let f = s.latest.clone().unwrap();
        assert_eq!(f.center_hz, 7.1e6);
        let k = f.bin_of(7.1125e6).unwrap();
        // A third of a bin off the grid: up to 0.4 dB of scalloping.
        assert!(f.db[k] < -36.95 && f.db[k] > -37.6, "{} dBm", f.db[k]);
        assert!((s.level_dbm - -37.0).abs() < 0.5, "meter {}", s.level_dbm);
        assert!(Arc::ptr_eq(s.history.back().unwrap(), &f));
        assert!(audio_rx.slots() > 1000, "audio is flowing");

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
}
