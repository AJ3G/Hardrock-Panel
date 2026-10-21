//! Cyclostationary feature detection: the spectral correlation function
//! (SCF) of the I/Q by the strip spectral correlation analyser, on the GPU,
//! drawn as the scope's "SCF" view (docs/design-decisions.md, 36).
//!
//! Stationary noise has no correlation between frequencies f + α/2 and
//! f − α/2 for any α ≠ 0; a digital signal does, at cycle frequencies set
//! by its symbol rate (α = k·Rs) and, for x·x, its carrier (α = 2fc). The
//! (f, α) plane shows what is there and its parameters, even under the
//! noise; it does not decode anything.
//!
//! # Lanes
//!
//! The SSCA resolves Δf = fs/N' in frequency and Δα = fs/N in cycle
//! frequency. On the whole span (192 kS/s to 6.144 MS/s) that is kHz-wide
//! channels and tens of Hz in α: right for finding PSK and FSK data with
//! kHz symbol rates anywhere in the span (the wide lane), useless for FT8's
//! 6.25 Hz. The narrow lane takes a few kHz of complex baseband around the
//! receiver's passband ([`lane::Ddc`], on the ingestion thread) so that
//! N = 4096 gives Δα = 0.73 Hz at 3 kS/s.
//!
//! Δf matters as much as Δα. A channel wider than the signal averages
//! S^α(f) over the whole signal, which leaves only R^α(0), the cyclic part
//! of |x|²: zero for a constant-envelope signal (FT8's GFSK) and nearly so
//! for rectangular keying. So the narrow lane wants Δf well under the
//! signal's bandwidth: N' = 1024 (2.9 Hz at 3 kS/s), the default. Measured
//! (`scf_sensitivity`, 12 s of averaging, 5σ, SNR in 2.5 kHz): at N' = 64
//! (47 Hz) FT8-like and 20 WPM CW were not found even at 0 dB; at 1024,
//! BPSK31 at −12 dB, CW at −10 dB, FT8-like only at 0 dB (−2 dB in 60 s):
//! GFSK's cyclic features are weak, so this is not a weak-signal FT8 tool.
//!
//! # Threads
//!
//! The pipeline's I/Q tap hands each block to a bounded queue (dropping when
//! full); an ingestion thread down-converts (narrow lane) and cuts blocks;
//! a GPU thread uploads each block with one `write_buffer` and runs the
//! passes, two blocks in flight, and reads back only the α profile (a few
//! KB). The UI thread never waits on any of it: it draws the grid buffer the
//! GPU thread writes, straight from VRAM.

pub mod draw;
pub mod engine;
pub mod features;
pub mod lane;
mod ui;

#[cfg(test)]
mod reference;
#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use eframe::egui_wgpu::{RenderState, wgpu};
use serde::{Deserialize, Serialize};

use engine::{BlockParams, Engine, EngineCfg, Measure, ProfileRow, Taper};
use features::{Axes, Feature};

/// Which I/Q the SCF runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LaneKind {
    /// The whole span, for wideband signal discovery.
    Wide,
    /// A few kHz around the receiver's passband, for FT8, FT4 and CW.
    #[default]
    Narrow,
}

/// Narrow-lane rates on offer, Hz (complex).
pub const LANE_RATES: [u32; 4] = [3000, 6000, 12_000, 24_000];
/// Channelizer lengths N'.
pub const CHANNELS: [u32; 5] = [64, 128, 256, 512, 1024];
/// The grid's f columns at most (cells pool the channels under them).
pub const MAX_GRID_F: u32 = 256;
/// Strip lengths N.
pub const STRIPS: [u32; 7] = [1024, 2048, 4096, 8192, 16_384, 32_768, 65_536];
/// α rows in the grid.
pub const GRID_A: u32 = 256;

/// The shortest strip N the engine runs with channelizer length `nc`
/// (N ≥ 4 × N').
pub fn min_strip(nc: u32) -> u32 {
    nc.saturating_mul(4)
}

/// Whether strip length `n` can go with channelizer length `nc`: at least
/// 4 × N', and N × N' within [`engine::MAX_POINTS`].
pub fn strip_ok(n: u32, nc: u32) -> bool {
    n >= min_strip(nc) && n as u64 * nc as u64 <= engine::MAX_POINTS as u64
}

/// What the SCF view keeps between runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScfSettings {
    pub lane: LaneKind,
    /// The narrow lane's rate, Hz.
    pub lane_hz: u32,
    /// N', the channelizer's length.
    pub channels: u32,
    /// N, the strip FFT's length (and a block's new samples).
    pub strip: u32,
    /// Blocks averaged: linear for the first this many, then exponential
    /// with this time constant.
    pub avg_blocks: u32,
    /// Show the spectral coherence (true) or the SCF.
    pub coherence: bool,
    /// The conjugate SCF (x·x: carriers at α = 2fc) rather than x·x*.
    pub conjugate: bool,
    /// The α axis runs to this, Hz (from 0, or from −this for x·x).
    pub alpha_max_hz: f64,
    /// A flat heat map rather than the terrain.
    pub heat_map: bool,
    /// A Kaiser channelizer taper (low leakage) rather than Hamming (fine
    /// Δf).
    pub kaiser: bool,
}

impl Default for ScfSettings {
    fn default() -> Self {
        ScfSettings {
            lane: LaneKind::Narrow,
            lane_hz: 3000,
            channels: 1024,
            strip: 4096,
            avg_blocks: 16,
            coherence: true,
            conjugate: false,
            alpha_max_hz: 50.0,
            heat_map: false,
            kaiser: true,
        }
    }
}

impl ScfSettings {
    /// Put anything out of range back in it.
    pub fn sanitise(&mut self) {
        if !LANE_RATES.contains(&self.lane_hz) {
            self.lane_hz = 3000;
        }
        if !CHANNELS.contains(&self.channels) {
            self.channels = 1024;
        }
        if !STRIPS.contains(&self.strip) {
            self.strip = 4096;
        }
        // A pair the engine cannot run (N under 4 × N', or too many points)
        // is put right here, so a bad pair kept from an earlier run cannot
        // stop the engine on every start.
        self.strip = self.strip.max(min_strip(self.channels));
        while self.strip as u64 * self.channels as u64 > engine::MAX_POINTS as u64 {
            self.strip /= 2;
        }
        self.avg_blocks = self.avg_blocks.clamp(1, 256);
        if !self.alpha_max_hz.is_finite() || self.alpha_max_hz <= 0.0 {
            self.alpha_max_hz = 50.0;
        }
    }

    /// Set N', raising N to the smallest it may be with it when it is under
    /// that (and lowering it when N × N' would be too many points).
    pub fn set_channels(&mut self, c: u32) {
        self.channels = c;
        self.sanitise();
    }

    /// The engine this wants.
    pub fn engine(&self) -> EngineCfg {
        EngineCfg {
            n: self.strip,
            nc: self.channels,
            grid_f: self.channels.min(MAX_GRID_F),
            grid_a: GRID_A,
            taper: if self.kaiser { Taper::Kaiser } else { Taper::Hamming },
        }
    }

    /// The α axis's top at lane rate `fs`: what was asked, kept between
    /// 16 Δα and fs/2 (α aliases modulo fs, and x·x*'s plane is symmetric).
    pub fn alpha_max(&self, fs: f64) -> f64 {
        let d_alpha = fs / self.strip as f64;
        self.alpha_max_hz.clamp((16.0 * d_alpha).min(fs / 2.0), fs / 2.0)
    }

    /// The grid's axes at lane rate `fs`, Hz.
    pub fn axes(&self, fs: f64) -> Axes {
        let top = self.alpha_max(fs);
        Axes {
            a_lo: if self.conjugate { -top } else { 0.0 },
            a_hi: top,
            f_lo: -fs / 2.0,
            f_hi: fs / 2.0,
            grid_f: self.channels.min(MAX_GRID_F),
            fs,
            d_alpha: fs / self.strip as f64,
        }
    }
}

/// What the UI wants the threads to run on.
#[derive(Debug, Clone)]
struct Want {
    s: ScfSettings,
    /// The narrow lane's centre, Hz from the span's centre.
    shift_hz: f64,
    /// Bumped by RESET.
    reset: u64,
}

/// What the threads report, for the UI.
#[derive(Debug, Clone, Default)]
pub struct Status {
    /// The lane's rate, Hz (0 before the first block).
    pub lane_rate: f64,
    /// The lane's centre, Hz from the span's centre.
    pub shift_hz: f64,
    pub cfg: Option<EngineCfg>,
    pub axes: Option<Axes>,
    pub conj: bool,
    pub coherence: bool,
    /// Blocks in the running average since it last started.
    pub blocks: u64,
    /// GPU time per block lately (wall clock of the dispatch and the wait
    /// for the block before), ms.
    pub ms_per_block: f32,
    /// Blocks dropped because the GPU was behind, and I/Q chunks the tap
    /// dropped because the ingestion thread was.
    pub gpu_drops: u64,
    pub tap_drops: u64,
    pub profile: Vec<ProfileRow>,
    pub features: Vec<Feature>,
    /// The level scale the plane is drawn with, dB.
    pub floor_db: f32,
    pub ceil_db: f32,
    /// The grid buffer and its version (new with each engine).
    pub grid: Option<(wgpu::Buffer, u64)>,
    pub error: Option<String>,
}

struct Shared {
    want: Mutex<Want>,
    status: Mutex<Status>,
    tap_drops: AtomicU64,
    stop: AtomicBool,
    /// When the UI last drew the view (ms since `epoch`): the threads idle
    /// when nobody is looking.
    seen_ms: AtomicU64,
    epoch: Instant,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn seen_recently(&self) -> bool {
        let now = self.epoch.elapsed().as_millis() as u64;
        now.saturating_sub(self.seen_ms.load(Ordering::Relaxed)) < 2000
    }
}

/// What the SCF takes from the pipeline's I/Q tap: each block, interleaved,
/// and its complex rate. It returns at once (the block is queued, or
/// dropped when the queue is full).
pub type IqSink = Box<dyn Fn(&[f32], f64) + Send + Sync>;

/// A chunk from the tap: interleaved I/Q and its rate.
type Chunk = (Vec<f32>, f64);

/// A block for the GPU thread.
struct Job {
    iq: Vec<f32>,
    s: ScfSettings,
    lane_rate: f64,
    shift_hz: f64,
    /// Changes when the average must start again (a new lane, a retune
    /// further than a channel, RESET).
    epoch: u64,
}

/// The running engine: its two threads and the tap's queue.
struct Runtime {
    tap_tx: SyncSender<Chunk>,
    shared: Arc<Shared>,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // The threads see this, or the closed queues, within 200 ms and end.
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

impl Runtime {
    fn start(device: wgpu::Device, queue: wgpu::Queue, want: Want) -> Runtime {
        let shared = Arc::new(Shared {
            want: Mutex::new(want),
            status: Mutex::new(Status::default()),
            tap_drops: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            seen_ms: AtomicU64::new(0),
            epoch: Instant::now(),
        });
        let (tap_tx, tap_rx) = mpsc::sync_channel::<Chunk>(64);
        let (job_tx, job_rx) = mpsc::sync_channel::<Job>(2);
        let sh = shared.clone();
        let _ = std::thread::Builder::new()
            .name("scf-ingest".into())
            .spawn(move || ingest(tap_rx, job_tx, &sh));
        let sh = shared.clone();
        let _ = std::thread::Builder::new().name("scf-gpu".into()).spawn(move || {
            // A panic stops the engine; it is started again once by itself
            // (a fresh engine, the same device) before the view gives up.
            let mut restarts = 0;
            loop {
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    gpu_loop(&device, &queue, &job_rx, &sh)
                }));
                let Err(payload) = run else { break };
                let why = crate::logfile::panic_text(&*payload);
                tracing::error!("the SCF engine stopped: {why}");
                let restart = restarts < MAX_RESTARTS && !sh.stop.load(Ordering::Relaxed);
                lock(&sh.status).error = Some(stop_message(&why, restart));
                if !restart {
                    break;
                }
                restarts += 1;
                tracing::info!("starting the SCF engine again ({restarts} of {MAX_RESTARTS})");
            }
        });
        Runtime { tap_tx, shared }
    }
}

/// How many times the GPU thread starts the engine again by itself after a
/// panic before it gives up.
const MAX_RESTARTS: u32 = 1;

/// What the view says when the engine has stopped on `why` (the panic's
/// text): that it was started again, or that it stopped for good and where
/// the log is.
fn stop_message(why: &str, restarted: bool) -> String {
    if restarted {
        format!("The SCF engine stopped ({why}) and was started again")
    } else {
        format!("The SCF engine stopped: {why}. {}", crate::logfile::where_it_is())
    }
}

/// The ingestion thread: tap chunks in, the narrow lane's DDC, blocks out.
fn ingest(rx: Receiver<Chunk>, tx: SyncSender<Job>, sh: &Shared) {
    let mut ddc: Option<(lane::Ddc, u32)> = None;
    let mut blocks: Option<(lane::Blocks, (u32, u32, u64, bool))> = None;
    let mut lane_buf = Vec::new();
    let mut epoch = 0u64;
    let mut last: Option<(u64, LaneKind, u64, f64)> = None;
    while !sh.stop.load(Ordering::Relaxed) {
        let (chunk, rate) = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(c) => c,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if !sh.seen_recently() || rate <= 0.0 {
            continue;
        }
        let w = lock(&sh.want).clone();
        let s = &w.s;
        let (lane_rate, data): (f64, &[f32]) = match s.lane {
            LaneKind::Wide => {
                ddc = None;
                (rate, &chunk)
            }
            LaneKind::Narrow => {
                let fresh =
                    ddc.as_ref().is_none_or(|(d, hz)| d.in_rate() != rate || *hz != s.lane_hz);
                if fresh {
                    ddc = Some((lane::Ddc::new(rate, s.lane_hz as f64, w.shift_hz), s.lane_hz));
                }
                let (d, _) = ddc.as_mut().expect("just made");
                if d.shift_hz() != w.shift_hz {
                    d.set_shift(w.shift_hz);
                }
                lane_buf.clear();
                d.process(&chunk, &mut lane_buf);
                (d.rate(), &lane_buf)
            }
        };
        // A new lane, a retune by more than half a channel, or RESET: start
        // the average again.
        let shift = if s.lane == LaneKind::Narrow { w.shift_hz } else { 0.0 };
        let moved = last.is_none_or(|(r, k, rb, sft)| {
            r != w.reset
                || k != s.lane
                || rb != lane_rate.to_bits()
                || (sft - shift).abs() > lane_rate / s.channels as f64 / 2.0
        });
        if moved {
            epoch += 1;
            last = Some((w.reset, s.lane, lane_rate.to_bits(), shift));
        }
        let key = (s.strip, s.channels, lane_rate.to_bits(), s.conjugate);
        if blocks.as_ref().is_none_or(|(_, k)| *k != key) {
            let len = (s.strip + s.channels) as usize;
            blocks = Some((lane::Blocks::new(len, s.channels as usize), key));
        }
        let (b, _) = blocks.as_mut().expect("just made");
        b.push(data);
        while let Some(iq) = b.next() {
            let job = Job { iq, s: s.clone(), lane_rate, shift_hz: shift, epoch };
            match tx.try_send(job) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    lock(&sh.status).gpu_drops += 1;
                    b.keep_latest();
                }
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
    }
}

/// The GPU thread: one engine (rebuilt when its sizes change), a block at
/// a time, the profile into the readout.
fn gpu_loop(device: &wgpu::Device, queue: &wgpu::Queue, rx: &Receiver<Job>, sh: &Shared) {
    let mut engine: Option<Engine> = None;
    // Sizes the engine refused, and the message shown for them.
    let mut refused: Option<(EngineCfg, String)> = None;
    let mut version = 0u64;
    let mut since = 0u64;
    let mut epoch = u64::MAX;
    let mut conj = false;
    // The settings the block before was run with: its profile comes back
    // with the next one.
    let mut prev: Option<(ScfSettings, f64, f64)> = None;
    let mut ms = 0.0f32;
    while !sh.stop.load(Ordering::Relaxed) {
        let job = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(j) => j,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let cfg = job.s.engine();
        if refused.as_ref().is_some_and(|(c, _)| *c == cfg) {
            continue;
        }
        if engine.as_ref().is_none_or(|e| e.cfg != cfg) {
            engine = None;
            match Engine::new(device, queue, cfg) {
                Ok(e) => engine = Some(e),
                Err(why) => {
                    let msg = format!("The SCF cannot run: {why}");
                    tracing::warn!("{msg} ({cfg:?})");
                    let mut st = lock(&sh.status);
                    st.error = Some(msg.clone());
                    st.grid = None;
                    refused = Some((cfg, msg));
                    continue;
                }
            }
            // A refusal's message goes once sizes that run are chosen.
            if let Some((_, msg)) = refused.take() {
                let mut st = lock(&sh.status);
                if st.error.as_ref() == Some(&msg) {
                    st.error = None;
                }
            }
            version += 1;
            since = 0;
            prev = None;
            let e = engine.as_ref().expect("just made");
            lock(&sh.status).grid = Some((e.grid.clone(), version));
        }
        if job.epoch != epoch || job.s.conjugate != conj {
            epoch = job.epoch;
            conj = job.s.conjugate;
            since = 0;
        }
        let e = engine.as_mut().expect("made above");
        let ax = job.s.axes(job.lane_rate);
        let fs = job.lane_rate;
        let bp = BlockParams {
            conj: job.s.conjugate,
            measure: if job.s.coherence { Measure::Coherence } else { Measure::Scf },
            beta: average_weight(since, job.s.avg_blocks),
            a_range: ((ax.a_lo / fs) as f32, (ax.a_hi / fs) as f32),
            f_range: ((ax.f_lo / fs) as f32, (ax.f_hi / fs) as f32),
            a_mask: (3.0 / job.s.strip as f64) as f32,
        };
        let t = Instant::now();
        let prof = e.process(&job.iq, &bp);
        let dt = t.elapsed().as_secs_f32() * 1000.0;
        ms = if ms == 0.0 { dt } else { ms + (dt - ms) * 0.1 };
        since += 1;
        let done = prev.replace((job.s.clone(), job.lane_rate, job.shift_hz));
        let mut st = lock(&sh.status);
        st.ms_per_block = ms;
        st.blocks = since;
        st.cfg = Some(cfg);
        st.tap_drops = sh.tap_drops.load(Ordering::Relaxed);
        if let (Some(p), Some((s, rate, shift))) = (prof, done) {
            let ax = s.axes(rate);
            // The σ of a few blocks' average is not yet a σ: noise passes 5σ
            // too often. No readout until min(avg, 8) blocks are in.
            st.features = if since >= s.avg_blocks.min(8) as u64 {
                features::features(&p, &ax, s.conjugate, 5.0, 6)
            } else {
                Vec::new()
            };
            let (floor, ceil) = scale(&p, &ax, s.conjugate);
            st.floor_db = floor;
            st.ceil_db = ceil;
            st.profile = p;
            st.axes = Some(ax);
            st.lane_rate = rate;
            st.shift_hz = shift;
            st.conj = s.conjugate;
            st.coherence = s.coherence;
        }
    }
}

/// The running average's weight for block `b` (from 0): linear for the
/// first `avg`, then exponential with that time constant.
pub fn average_weight(b: u64, avg: u32) -> f32 {
    (1.0 / (b + 1) as f64).max(1.0 / avg.max(1) as f64) as f32
}

/// The level scale for the plane: the floor a little under the median
/// cell, the ceiling over the strongest cell (at least 12 dB above).
fn scale(p: &[ProfileRow], ax: &Axes, conj: bool) -> (f32, f32) {
    let rows = p.len();
    let mask = 3.0 * ax.d_alpha;
    let keep = |i: usize| conj || ax.alpha_of_row(i, rows).abs() > mask;
    let mut means: Vec<f32> = p
        .iter()
        .enumerate()
        .filter(|(i, r)| keep(*i) && r[2] > -250.0)
        .map(|(_, r)| r[2])
        .collect();
    if means.is_empty() {
        return (-40.0, 0.0);
    }
    means.sort_by(f32::total_cmp);
    let floor = means[means.len() / 2] - 2.0;
    let top =
        p.iter().enumerate().filter(|(i, _)| keep(*i)).map(|(_, r)| r[3]).fold(floor, f32::max);
    (floor, top.max(floor + 12.0) + 1.0)
}

/// The SCF view: the engine's threads while it is shown, and its drawing.
pub struct Scf {
    gpu: Option<(wgpu::Device, wgpu::Queue)>,
    /// Why the engine cannot run here, if it cannot.
    unsupported: Option<&'static str>,
    runtime: Option<Runtime>,
    tapped: bool,
    reset: u64,
    /// The level scale on screen, eased towards the threads'.
    shown_scale: Option<(f32, f32)>,
}

impl Scf {
    /// The view, with eframe's render state when there is one.
    pub fn new(rs: Option<&RenderState>) -> Scf {
        let Some(rs) = rs else {
            return Scf::without("the SCF needs the GPU renderer");
        };
        if !engine::supported(&rs.adapter, &rs.device) {
            return Scf::without("this GPU has no compute shaders or vertex storage buffers");
        }
        draw::init(rs);
        Scf {
            gpu: Some((rs.device.clone(), rs.queue.clone())),
            unsupported: None,
            runtime: None,
            tapped: false,
            reset: 0,
            shown_scale: None,
        }
    }

    fn without(why: &'static str) -> Scf {
        Scf {
            gpu: None,
            unsupported: Some(why),
            runtime: None,
            tapped: false,
            reset: 0,
            shown_scale: None,
        }
    }

    /// Start the threads (`on`) or stop them, and keep the pipeline's I/Q
    /// tap in step through `set_tap` (which installs a sink, or with `None`
    /// removes it). Call every frame with whether the view is showing.
    pub fn run(&mut self, on: bool, set_tap: &mut dyn FnMut(Option<IqSink>)) {
        let on = on && self.gpu.is_some();
        if on && self.runtime.is_none() {
            let (d, q) = self.gpu.clone().expect("checked");
            let want = Want { s: ScfSettings::default(), shift_hz: 0.0, reset: self.reset };
            self.runtime = Some(Runtime::start(d, q, want));
            self.tapped = false;
        }
        if !on {
            if self.tapped {
                set_tap(None);
                self.tapped = false;
            }
            self.runtime = None;
            self.shown_scale = None;
            return;
        }
        if !self.tapped
            && let Some(rt) = &self.runtime
        {
            let tx = rt.tap_tx.clone();
            let sh = rt.shared.clone();
            set_tap(Some(Box::new(move |iq, rate| {
                // One copy of the block, then back to the pipeline; none
                // while nobody is looking.
                if sh.seen_recently() && tx.try_send((iq.to_vec(), rate)).is_err() {
                    sh.tap_drops.fetch_add(1, Ordering::Relaxed);
                }
            })));
            self.tapped = true;
        }
    }

    /// Tell the threads what to run on: the settings and the narrow lane's
    /// centre (Hz from the span's centre). Marks the view as seen.
    fn want(&mut self, s: &ScfSettings, shift_hz: f64) -> Option<Status> {
        let rt = self.runtime.as_ref()?;
        let sh = &rt.shared;
        sh.seen_ms.store(sh.epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
        {
            let mut w = lock(&sh.want);
            if w.s != *s || w.shift_hz != shift_hz || w.reset != self.reset {
                *w = Want { s: s.clone(), shift_hz, reset: self.reset };
            }
        }
        Some(lock(&sh.status).clone())
    }

    /// Start the average again.
    fn reset(&mut self) {
        self.reset += 1;
    }
}

pub use ui::Frame;
