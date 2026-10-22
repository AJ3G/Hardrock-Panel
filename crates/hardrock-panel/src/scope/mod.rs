//! The panadapter scope: spectrum, 3D FFT and waterfall from the DUO's I/Q,
//! on the DUO-ART's TFT motif (the "Scope" part of the ELAD-look mockup).
//!
//! # Using it
//!
//! ```ignore
//! // At start-up, with eframe's render state (the waterfall and 3D surface
//! // are drawn on the GPU):
//! let mut scope = Scope::new(cc.wgpu_render_state.as_ref(), saved.scope.clone());
//!
//! // Every frame:
//! let snap = pipeline.snapshot();
//! let out = scope.ui(ui, &snap, ScopeRig { rx_hz, mode, passband, transmitting, on_air });
//! for c in out.commands { pipeline.send(c) }          // FFT size, window, zoom FFT…
//! if let Some(hz) = out.tune_to { /* receiver to hz (the radio's own frequency) */ }
//! if let Some(pb) = out.passband { /* receiver passband */ }
//! if let Some(m) = out.mode { /* receiver mode (a data-mode button sets USB) */ }
//! if let Some(hz) = out.recentre { /* VFO A, and so the IQ centre, to hz */ }
//! if let Some(rate) = out.sample_rate { /* the SPAN key: the IQ rate */ }
//! if out.settings_changed { saved.scope = scope.settings().clone() }
//! ```
//!
//! Every frequency in and out is the radio's own (its IF when a transverter
//! is used); the scope adds the transverter offset and the ppm calibration
//! only for what it shows.
//!
//! # Behaviour (docs/design-decisions.md, 22 to 25)
//!
//! SDR Console's: the IQ centre (the DUO's VFO A) stays put while the panel's
//! receiver moves inside the span, and the scope asks for a new centre only
//! when the receiver goes past 0.45 of the span from it. Click a signal to
//! tune to it (snapped to the step), drag the receiver marker, drag a filter
//! edge, wheel to step, ctrl+wheel to zoom about the pointer, drag empty
//! space to pan, double-click for the whole span, right-click to place a
//! measurement marker, shift-click to drop the offset bar. Nothing tunes
//! while the radio transmits (safety interlock 9).
//!
//! # The skimmer's labels and the offset bar (decisions 54 and 55)
//!
//! [`Scope::skim`] takes DECODE's newest decodes each frame and labels each
//! station by its callsign at its frequency, on the waterfall's top and up
//! the spectrum's foot, fading over three slots ([`skim`]). The offset bar
//! ([`offset`]) is a measuring cursor that reads its distance from the dial
//! in DECODE's hertz; it retunes nothing.
//!
//! # The FFT on the GPU (decision 37)
//!
//! With a GPU that has compute shaders, and "FFT on GPU" (the default), the
//! raw I/Q goes to the GPU ([`Scope::gpu_iq_sink`], fed on the pipeline's
//! thread) and the spectrum, its waterfall and 3D rows and the trace are all
//! computed and drawn there; the pipeline's own spectrum thread is told to
//! stop. Only the trace's columns, the noise floor and the markers' peaks
//! come back, a frame or two late. "FFT on CPU" goes back to the
//! pipeline's spectrum, as before.

pub mod bandplan;
mod controls;
pub mod digimodes;
pub mod gpu;
pub mod gpu_fft;
pub mod gpu_iq;
mod gpu_run;
pub mod gpu_spec;
pub mod levels;
mod markers;
mod offset;
mod paint;
pub mod palette;
mod persistence;
pub mod pointer;
mod rows;
pub mod scf;
pub mod settings;
pub mod skim;
pub mod slotlines;
pub mod view;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use sdr::{SpectrumFrame, ZoomRequest};

pub use digimodes::Digi;
pub use settings::ScopeSettings;
pub use skim::{KeyTag, SkimFeed, SkimSpot};
pub use slotlines::DecodeSlots;

use gpu_run::{Ask, GpuRun, TraceGeom};
use markers::{Exact, ExactPeak, Markers};
use offset::OffsetBar;
use persistence::Persistence;
use pointer::{Drag, Wheel};
use rows::{Rows, Upload};
use view::{FreqMap, Span, View};

/// The GPU's I/Q tap: `sink(iq, centre Hz, rate)`, interleaved f32.
pub type IqSink = Box<dyn Fn(&[f32], f64, f64) + Send + Sync>;

/// What the scope needs to know about the radio each frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScopeRig {
    /// The panel's receiver, the radio's own frequency, Hz.
    pub rx_hz: f64,
    pub mode: sdr::Mode,
    /// `[low, high]` Hz from the dial.
    pub passband: (f32, f32),
    /// The radio is transmitting: the scope tunes nothing (interlock 9).
    pub transmitting: bool,
    /// The radio itself says it is on the air: the display is held over it
    /// (decision 40). Narrower than `transmitting`, which also waits for the
    /// amplifier's drive reading to fall to nothing; that reading decays over
    /// most of a second after the radio has unkeyed (AJ3G's log, 21 Oct
    /// 2026), and held the scope that much longer.
    pub on_air: bool,
}

/// What the operator asked for on the scope this frame.
#[derive(Default)]
pub struct ScopeOut {
    /// Tune the receiver here (the radio's own frequency, Hz).
    pub tune_to: Option<f64>,
    /// Set the receiver's passband, `[low, high]` Hz from the dial.
    pub passband: Option<(f32, f32)>,
    /// Set the receiver's mode (a data-mode button sets USB).
    pub mode: Option<sdr::Mode>,
    /// Move the IQ centre (the DUO's VFO A) here, Hz: the receiver has gone
    /// past 0.45 of the span from it.
    pub recentre: Option<f64>,
    /// A DATA key or a band-plan marker was pressed: `tune_to`, `passband`
    /// and `mode` carry what it sets on the receiver; this says which mode
    /// and where, for the radio's filter, the decoder and the note
    /// (decision 39).
    pub data_mode: Option<DataPick>,
    /// The SPAN key: ask the radio for this IQ sample rate.
    pub sample_rate: Option<u32>,
    /// For the pipeline: FFT size, window, overlap, averaging, peak hold,
    /// frame rate, history and the zoom FFT, as the settings change.
    pub commands: Vec<sdr::Command>,
    /// A setting changed: save [`Scope::settings`].
    pub settings_changed: bool,
    /// A skimmer label of DECODE's own was clicked: pick its line there.
    pub skim_pick: Option<u64>,
    /// A keyboard skimmer's label was clicked (decision 56): `tune_to`,
    /// `mode` and `passband` carry the receiver's part; DECODE takes this
    /// mode where it has it.
    pub key_pick: Option<skim::KeyMode>,
    /// A SKIM WIDE label was clicked: `tune_to` carries its dial; DECODE
    /// takes the mode it was heard in (FT4 heard from FT8, or the other way).
    pub wide_pick: Option<skim::WsjtMode>,
}

/// A data mode picked on the scope.
#[derive(Debug, Clone, PartialEq)]
pub struct DataPick {
    pub digi: &'static Digi,
    /// Its dial (or channel centre), as shown: after the converter offset.
    pub dial: u64,
    /// Said when it went to another band than the receiver's.
    pub note: Option<String>,
}

/// The spectrum settings last sent to the pipeline.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Sent {
    fft_size: usize,
    window: sdr::Window,
    overlap: sdr::Overlap,
    averaging: sdr::Averaging,
    peak: bool,
    fps: f32,
}

/// Frames of history the scope asks the pipeline to keep: enough to catch up
/// after a slow repaint, few enough that a million-point FFT does not hold a
/// gigabyte of old spectra.
pub const PIPELINE_HISTORY: usize = 16;

/// How long after the radio says it has stopped transmitting the display
/// stays held: its receiver's front end and AGC settle, and the read-back of
/// the dial comes home, in about that.
pub const TX_GUARD_SECS: f64 = 0.3;
/// The longest the CPU spectrum is waited for after asking it to start
/// again (decision 40).
const RESET_WAIT_SECS: f64 = 2.0;

/// SDR Console's way with a transmission (decision 40): the display holds
/// what it showed while the radio transmits and for [`TX_GUARD_SECS`]
/// after, and then starts again from the newest samples with the averages
/// and peaks forgotten, so whatever the radio's I/Q did meanwhile (stopped,
/// flooded with the transmitter's own signal, moved with the dial) is never
/// drawn and never lingers.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct TxHold {
    keyed: bool,
    /// Held until then, after the end of an over.
    until: Option<f64>,
}

impl TxHold {
    /// This frame: whether the display is held, and whether it starts
    /// again now.
    pub(crate) fn step(&mut self, transmitting: bool, now: f64) -> (bool, bool) {
        if transmitting {
            self.keyed = true;
            self.until = None;
            return (true, false);
        }
        if std::mem::take(&mut self.keyed) {
            self.until = Some(now + TX_GUARD_SECS);
        }
        match self.until {
            Some(t) if now < t => (true, false),
            Some(_) => {
                self.until = None;
                (false, true)
            }
            None => (false, false),
        }
    }
}

/// The scope. One per window that shows it.
pub struct Scope {
    settings: ScopeSettings,
    id: u64,
    gpu: Option<gpu::GpuInfo>,
    rows: Rows,
    /// The zoom (`None`: the whole span), the radio's own frequency.
    want_view: Option<View>,
    drag: Option<Drag>,
    /// A drag of a measurement marker (its index).
    marker_drag: Option<usize>,
    /// A drag of the split or the height grip: the value it started from and
    /// the pointer's y then.
    grip: Option<(bool, f32, f32)>,
    wheel: Wheel,
    last_gen: u64,
    last_zgen: u64,
    /// The main and zoom frames on screen (held while paused).
    main: Option<Arc<SpectrumFrame>>,
    zoom: Option<Arc<SpectrumFrame>>,
    /// The display is frozen (PAUSE); the stream still runs.
    paused: bool,
    digi: Option<&'static Digi>,
    digi_note: Option<String>,
    fit_at: f64,
    wf_levels: Option<(f32, f32)>,
    markers: Markers,
    persist: Persistence,
    persist_tex: Option<egui::TextureHandle>,
    persist_at: f64,
    sent: Option<Sent>,
    history_sent: bool,
    zoom_sent: Option<Option<ZoomRequest>>,
    recentre_asked: Option<(f64, f64)>,
    /// The receiver the zoomed view last followed: it follows again only
    /// when the receiver moves from here, never after a pan or a zoom.
    followed_rx: Option<f64>,
    cursor: String,
    /// When the newest waterfall row came, and the time between rows lately,
    /// for the smooth scroll.
    row_at: f64,
    row_interval: f64,
    /// Trace columns of the frame on screen, kept for FIT and the persistence.
    cols: Vec<f32>,
    peak_cols: Vec<f32>,
    /// Recent frame times, for the frame-rate readout.
    frame_times: VecDeque<f64>,
    xvtr_text: String,
    scf: scf::Scf,
    /// The GPU spectrum, when the GPU can run one.
    gpu_run: Option<GpuRun>,
    /// Its newest peaks at full resolution, its real bin width, and where
    /// its trace columns are this repaint.
    exact: Vec<ExactPeak>,
    bin_hz: f64,
    trace_geom: Option<TraceGeom>,
    /// The trace's width last drawn, physical pixels.
    trace_px: u32,
    /// The zoom FFT the GPU spectrum runs (worked out after each repaint).
    gpu_zoom: Option<ZoomRequest>,
    spectrum_on_sent: Option<bool>,
    cal_db: f32,
    /// The 3D surface's floor and ceiling, easing to the scale's.
    s3_levels: Option<(f32, f32)>,
    s3_levels_at: f64,
    /// Held over a transmission (decision 40), and held on after it until
    /// the CPU spectrum has started again (its count of resets to reach,
    /// and since when).
    tx_hold: TxHold,
    held: bool,
    /// The span last drawn before a hold, and kept through it.
    held_span: Option<Span>,
    reset_wait: Option<(u64, f64)>,
    reset_ask: bool,
    /// The samples the pipeline had read: fewer means a new pipeline.
    samples_seen: u64,
    /// The GPU watchdog's resyncs already told, and a note for the status
    /// line until a time.
    resyncs_told: u32,
    notice: Option<(String, f64)>,
    /// DECODE's runs, for the slot lines.
    slot_lines: slotlines::SlotLines,
    /// DECODE's newest decodes, for the skimmer's labels; the labels shown
    /// this frame, with their opacity; where their boxes were drawn on the
    /// waterfall; and the one a press went down on.
    skim: SkimFeed,
    skim_shown: Vec<(SkimSpot, f32)>,
    skim_boxes: Vec<(egui::Rect, usize)>,
    skim_press: Option<usize>,
    /// The offset bar, whether it is being dragged, and its readout's
    /// remove button as drawn.
    offset: Option<OffsetBar>,
    offset_drag: bool,
    offset_close: Option<egui::Rect>,
    /// The label the offset bar is on this frame, in `skim_shown`.
    skim_on_bar: Option<usize>,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

impl Scope {
    /// A scope with `settings`, drawing its waterfall and 3D surface on the
    /// GPU when `render_state` is given (eframe's wgpu backend).
    pub fn new(render_state: Option<&RenderState>, mut settings: ScopeSettings) -> Scope {
        settings.sanitise();
        let gpu = render_state.map(gpu::init);
        let gpu_run = render_state
            .filter(|_| gpu.is_some_and(|g| g.compute))
            .map(|rs| GpuRun::new(&rs.device, &rs.queue));
        let tex_w = gpu.map_or(4096, |g| g.tex_w) as usize;
        let xvtr_text = view::fmt_freq(settings.transverter_hz);
        Scope {
            settings,
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            gpu,
            rows: Rows::new(tex_w),
            want_view: None,
            drag: None,
            marker_drag: None,
            grip: None,
            wheel: Wheel::default(),
            last_gen: 0,
            last_zgen: 0,
            main: None,
            zoom: None,
            paused: false,
            digi: None,
            digi_note: None,
            fit_at: f64::NEG_INFINITY,
            wf_levels: None,
            markers: Markers::default(),
            persist: Persistence::default(),
            persist_tex: None,
            persist_at: 0.0,
            sent: None,
            history_sent: false,
            zoom_sent: None,
            recentre_asked: None,
            followed_rx: None,
            cursor: String::new(),
            row_at: 0.0,
            row_interval: 1.0 / 60.0,
            cols: Vec::new(),
            peak_cols: Vec::new(),
            frame_times: VecDeque::new(),
            xvtr_text,
            scf: scf::Scf::new(render_state),
            gpu_run,
            exact: Vec::new(),
            bin_hz: 1.0,
            trace_geom: None,
            trace_px: 0,
            gpu_zoom: None,
            spectrum_on_sent: None,
            cal_db: 0.0,
            s3_levels: None,
            s3_levels_at: 0.0,
            tx_hold: TxHold::default(),
            held: false,
            held_span: None,
            reset_wait: None,
            reset_ask: false,
            samples_seen: 0,
            resyncs_told: 0,
            notice: None,
            slot_lines: slotlines::SlotLines::default(),
            skim: SkimFeed::default(),
            skim_shown: Vec::new(),
            skim_boxes: Vec::new(),
            skim_press: None,
            offset: None,
            offset_drag: false,
            offset_close: None,
            skim_on_bar: None,
        }
    }

    /// The tap that puts the raw I/Q on the GPU, for the receiver's I/Q
    /// taps (it is called on the pipeline's thread and never waits): `None`
    /// without a GPU that can compute the spectrum. Take it once; each call
    /// starts the GPU's stream afresh.
    pub fn gpu_iq_sink(&self) -> Option<IqSink> {
        let run = self.gpu_run.as_ref()?;
        Some(Box::new(run.iq.sink()))
    }

    /// The I/Q ring on the GPU, for other GPU views (see [`gpu_iq`]).
    pub(crate) fn gpu_iq(&self) -> Option<&Arc<gpu_iq::GpuIq>> {
        self.gpu_run.as_ref().map(|r| &r.iq)
    }

    /// The spectrum is computed on the GPU.
    pub fn gpu_fft(&self) -> bool {
        self.settings.fft_gpu && self.gpu_run.is_some()
    }

    /// Bytes a second of I/Q going to the GPU lately.
    pub fn gpu_iq_bytes_per_sec(&self) -> f64 {
        self.gpu_run.as_ref().map_or(0.0, |r| r.samples_per_sec * 8.0)
    }

    /// The dBFS to dBm calibration the GPU spectrum uses (the pipeline's
    /// own comes from [`Scope::spectrum_config`]).
    pub fn set_calibration_db(&mut self, db: f32) {
        self.cal_db = db;
    }

    /// What the markers read: the frame on screen, with the GPU's peaks.
    fn bins<'a>(&'a self, f: &'a SpectrumFrame) -> Exact<'a> {
        if self.gpu_fft() {
            Exact { frame: f, peaks: &self.exact, bin_hz: self.bin_hz }
        } else {
            Exact { frame: f, peaks: &[], bin_hz: f.bin_width() }
        }
    }

    /// The 3D surface's floor and ceiling: the scale's, eased over a
    /// fifth of a second so a FIT or a new level grows the surface rather
    /// than jumps it.
    fn s3_levels(&mut self, now: f64) -> (f32, f32) {
        let want = (self.settings.floor_dbm, self.settings.ceil_dbm);
        let dt = (now - self.s3_levels_at).clamp(0.0, 0.25) as f32;
        self.s3_levels_at = now;
        let k = 1.0 - (-dt / 0.2).exp();
        let (f, c) = self.s3_levels.unwrap_or(want);
        let eased = (f + (want.0 - f) * k, c + (want.1 - c) * k);
        self.s3_levels = Some(eased);
        eased
    }

    /// The settings to save.
    pub fn settings(&self) -> &ScopeSettings {
        &self.settings
    }

    /// Light a DATA key and say what was done, for a data mode picked
    /// elsewhere (DECODE's snap); `None` puts the keys out.
    pub fn show_data(&mut self, d: Option<&'static Digi>, note: Option<String>) {
        self.digi = d;
        self.digi_note = note;
    }

    /// Set the transverter LO from elsewhere (the radio window's offset), Hz.
    pub fn set_transverter_hz(&mut self, hz: f64) {
        if self.settings.transverter_hz != hz {
            self.settings.transverter_hz = hz;
            self.xvtr_text = view::fmt_freq(hz.abs());
        }
    }

    /// The pipeline's starting spectrum settings, for `PipelineConfig`.
    pub fn spectrum_config(&self, calibration_db: f32) -> sdr::SpectrumConfig {
        let s = &self.settings;
        sdr::SpectrumConfig {
            fft_size: s.fft_size,
            window: s.window.to_sdr(),
            overlap: s.overlap.to_sdr(),
            averaging: s.averaging(),
            peak_hold: s.peak,
            peak_decay_db_per_s: 10.0,
            fps: s.fps,
            calibration_db,
        }
    }

    /// Free the GPU textures (when the scope is closed for good).
    pub fn retire(&self, render_state: Option<&RenderState>) {
        if let Some(rs) = render_state {
            gpu::retire(rs, self.id);
            scf::draw::retire(rs, self.id);
        }
    }

    /// Run the SCF view's engine while it is shown, on the pipeline's I/Q
    /// tap (`set_tap` installs or, with `None`, removes the sink). Call every
    /// frame the scope is drawn.
    pub fn scf_tap(&mut self, set_tap: &mut dyn FnMut(Option<scf::IqSink>)) {
        self.scf.run(self.settings.top == settings::TopView::Scf, set_tap);
    }

    /// Each frame: what DECODE is doing with slots (`None` when it is not
    /// running a mode that has them), for the slot lines.
    pub fn decode_slots(&mut self, slots: Option<DecodeSlots>) {
        self.slot_lines.feed(slots, utc_now());
    }

    /// Each frame: DECODE's newest decodes for the skimmer's labels (`None`
    /// when SKIM is off or no slotted mode runs: no labels).
    pub fn skim(&mut self, feed: Option<SkimFeed>) {
        self.skim = feed.unwrap_or_default();
    }

    fn map(&self) -> FreqMap {
        FreqMap { ppm: self.settings.ppm, transverter_hz: self.settings.transverter_hz }
    }

    /// Draw the scope and its controls, the whole width of `ui`.
    pub fn ui(&mut self, ui: &mut egui::Ui, snap: &sdr::Snapshot, rig: ScopeRig) -> ScopeOut {
        let mut out = ScopeOut::default();
        let now = ui.input(|i| i.time);
        let before = self.settings.clone();

        self.hold_over_tx(snap, rig.on_air, now);
        let span = self.span(snap);
        let cur_view = view::view(span, self.want_view);
        let scf_on = self.settings.top == settings::TopView::Scf;
        let upload = if scf_on { None } else { self.take_frames(snap, span, cur_view, now) };

        // The receiver past 0.45 of the span: ask for a new centre, once a
        // second at most while it is on its way.
        self.follow(span, &rig, now, &mut out);
        let span = self.span(snap);
        let v = view::view(span, self.want_view);

        let cx = paint::Ctx { span, view: v, map: self.map(), rig, now };
        if scf_on {
            let f = scf::Frame {
                snap,
                palette: self.settings.palette,
                height: self.settings.height,
                map: self.map(),
                id: self.id,
            };
            self.scf.ui(ui, &mut self.settings.scf, f);
        } else {
            paint::tft(self, ui, &cx, snap, upload, &mut out);
        }
        controls::soft_keys(self, ui, snap, &mut out);
        controls::rows(self, ui, &cx, &mut out);

        self.sync_pipeline(snap, &cx, ui.available_width(), &mut out);
        if self.settings != before {
            self.settings.sanitise();
            out.settings_changed = true;
        }
        if snap.alive && !self.paused {
            ui.ctx().request_repaint();
        }
        out
    }

    /// The span drawn: the snapshot's, or while held over a transmission
    /// the one drawn before the hold began, so a centre the radio reports
    /// during the over (even one that comes before the radio says it is
    /// transmitting) does not move what is held.
    fn span(&mut self, snap: &sdr::Snapshot) -> Span {
        if !self.held {
            let s = span_of(snap);
            self.held_span = Some(s);
            return s;
        }
        *self.held_span.get_or_insert_with(|| span_of(snap))
    }

    /// Whether the display is held for a transmission (decision 40), and
    /// what starting again after one takes: the GPU spectrum from the
    /// newest samples, or the pipeline's spectrum reset (and waited for).
    fn hold_over_tx(&mut self, snap: &sdr::Snapshot, transmitting: bool, now: f64) {
        if snap.samples < self.samples_seen {
            // A new pipeline: it has none of what was sent to the old one.
            self.sent = None;
            self.history_sent = false;
            self.zoom_sent = None;
            self.spectrum_on_sent = None;
            self.reset_wait = None;
        }
        self.samples_seen = snap.samples;
        let (hold, resume) = self.tx_hold.step(transmitting, now);
        if resume {
            if self.gpu_fft() {
                if let Some(r) = self.gpu_run.as_mut() {
                    r.resync(false);
                }
            } else {
                self.reset_ask = true;
                self.reset_wait = Some((snap.spectrum_resets + 1, now));
            }
        }
        if let Some((want, at)) = self.reset_wait
            && (snap.spectrum_resets >= want || now - at > RESET_WAIT_SECS || self.gpu_fft())
        {
            self.reset_wait = None;
        }
        self.held = hold || self.reset_wait.is_some();
    }

    /// Take the frames that came since last time: the trace, and rows for
    /// the waterfall and the surface. Nothing is taken while paused, or
    /// while held over a transmission.
    fn take_frames(
        &mut self,
        snap: &sdr::Snapshot,
        span: Span,
        v: View,
        now: f64,
    ) -> Option<Arc<Upload>> {
        if self.gpu_fft() {
            return self.take_gpu(v, now);
        }
        let mut up = Upload::default();
        let new_main = fresh(&snap.history, snap.generation, &mut self.last_gen);
        let new_zoom = fresh(&snap.zoom_history, snap.zoom_generation, &mut self.last_zgen);
        if self.paused || self.held {
            return None;
        }
        if snap.latest.is_some() {
            self.main = snap.latest.clone();
        }
        let zoom_ok =
            self.settings.zoom_fft && snap.zoom_latest.as_ref().is_some_and(|z| covers(z, v));
        self.zoom = if zoom_ok { snap.zoom_latest.clone() } else { None };
        let source: Vec<Arc<SpectrumFrame>> = if zoom_ok { new_zoom } else { new_main.clone() };

        let utc = utc_now();
        let had = self.rows.wf_count();
        for f in &source {
            self.rows.push_waterfall(
                &f.db,
                Span { center: f.center_hz, width: f.span_hz },
                v,
                self.settings.wf_speed.frames_per_row(),
                utc,
                &mut up,
            );
        }
        if !up.wf.is_empty() {
            let gap = (now - self.row_at).clamp(0.0, 0.5);
            if had > 0 && gap > 0.0 {
                self.row_interval += (gap / up.wf.len() as f64 - self.row_interval) * 0.2;
            }
            self.row_at = now;
        }
        for _ in &new_main {
            self.frame_times.push_back(now);
        }
        while self.frame_times.front().is_some_and(|&t| now - t > 1.0) {
            self.frame_times.pop_front();
        }

        if self.settings.top.has_surface() {
            let latest =
                source.last().map(|f| (&f.db[..], Span { center: f.center_hz, width: f.span_hz }));
            self.rows.push_surface(latest, v, now, self.settings.surface_rows_per_sec, &mut up);
        } else {
            self.rows.clear_surface();
        }
        let _ = span;
        let up = self.rows.seal(up);
        (!up.is_empty()).then(|| Arc::new(up))
    }

    /// [`Scope::take_frames`] with the FFT on the GPU: plan what it runs
    /// this repaint, and take what came back.
    fn take_gpu(&mut self, v: View, now: f64) -> Option<Arc<Upload>> {
        let run = self.gpu_run.as_mut()?;
        if run.resyncs != self.resyncs_told {
            self.resyncs_told = run.resyncs;
            self.notice = Some((
                "FFT resynchronised: no frames came from a running stream".into(),
                now + 8.0,
            ));
        }
        if let Some(r) = run.take_readout() {
            self.cols.clone_from(&r.frame.db);
            self.peak_cols = r.frame.peak.clone().unwrap_or_default();
            self.exact = r.peaks;
            self.bin_hz = r.bin_hz;
            let f = Arc::new(r.frame);
            self.zoom = r.zoom.then(|| f.clone());
            self.main = Some(f);
        }
        // The view's strongest bin, and each tracking marker's.
        let window = (v.width() / 200.0).max(self.bin_hz * 3.0);
        let mut queries = vec![(v.lo, v.hi)];
        for m in self.markers.m.iter().flatten().filter(|m| m.track) {
            queries.push((m.hz - window, m.hz + window));
        }
        let ask = Ask {
            settings: &self.settings,
            cal_db: self.cal_db,
            view: v,
            zoom: self.gpu_zoom,
            paused: self.paused || self.held,
            trace_cols: self.trace_px,
            queries,
            now,
            utc: utc_now(),
        };
        let mut up = Upload::default();
        let had = self.rows.wf_count();
        let planned = run.plan(&ask, &mut self.rows, &mut up);
        self.trace_geom = planned.trace;
        if planned.wf_rows > 0 {
            let gap = (now - self.row_at).clamp(0.0, 0.5);
            if had > 0 && gap > 0.0 {
                self.row_interval += (gap / planned.wf_rows as f64 - self.row_interval) * 0.2;
            }
            self.row_at = now;
        }
        for _ in 0..planned.main_frames {
            self.frame_times.push_back(now);
        }
        while self.frame_times.front().is_some_and(|&t| now - t > 1.0) {
            self.frame_times.pop_front();
        }
        let up = self.rows.seal(up);
        (!up.is_empty()).then(|| Arc::new(up))
    }

    /// The frame the trace is drawn from: the zoom FFT's when it covers the
    /// view, else the whole span's.
    fn shown(&self) -> Option<&Arc<SpectrumFrame>> {
        self.zoom.as_ref().or(self.main.as_ref())
    }

    /// SDR Console's fixed centre, and a zoomed view that follows the
    /// receiver.
    fn follow(&mut self, span: Span, rig: &ScopeRig, now: f64, out: &mut ScopeOut) {
        if span.width <= 0.0 {
            return;
        }
        if let Some(c) = view::needs_recentre(span, rig.rx_hz) {
            let asked = self.recentre_asked.is_some_and(|(hz, t)| hz == c && now - t < 1.0);
            // SAFETY INTERLOCK 9: the centre is the DUO's VFO A; never moved
            // while it transmits.
            if !asked && !rig.transmitting {
                out.recentre = Some(c);
                self.recentre_asked = Some((c, now));
                // Keep the zoom's width, centred on the receiver, in the new
                // span.
                if let Some(w) = self.want_view {
                    let half = w.width() / 2.0;
                    self.want_view = Some(View { lo: rig.rx_hz - half, hi: rig.rx_hz + half });
                }
            }
        }
        // Follow only a receiver that moved (tuning, a mode snap, a click to
        // tune); a view panned or zoomed away from it stays put. A move during
        // a drag is followed once the button is up.
        if self.drag.is_none() {
            let v = view::view(span, self.want_view);
            if let Some(k) = view::follow_rx(span, v, rig.rx_hz, self.followed_rx) {
                self.want_view = Some(k);
            }
            self.followed_rx = Some(rig.rx_hz);
        }
    }

    /// Tell the pipeline about any setting that changed, and keep the zoom
    /// FFT on the view.
    fn sync_pipeline(
        &mut self,
        snap: &sdr::Snapshot,
        cx: &paint::Ctx,
        px: f32,
        out: &mut ScopeOut,
    ) {
        let s = &self.settings;
        let want = Sent {
            fft_size: s.fft_size,
            window: s.window.to_sdr(),
            overlap: s.overlap.to_sdr(),
            averaging: s.averaging(),
            peak: s.peak,
            fps: s.fps,
        };
        let old = self.sent;
        if old.map(|o| o.fft_size) != Some(want.fft_size) {
            out.commands.push(sdr::Command::SetFftSize(want.fft_size));
        }
        if old.map(|o| o.window) != Some(want.window) {
            out.commands.push(sdr::Command::SetWindow(want.window));
        }
        if old.map(|o| o.overlap) != Some(want.overlap) {
            out.commands.push(sdr::Command::SetOverlap(want.overlap));
        }
        if old.map(|o| o.averaging) != Some(want.averaging) {
            out.commands.push(sdr::Command::SetAveraging(want.averaging));
        }
        if old.map(|o| o.peak) != Some(want.peak) {
            out.commands.push(sdr::Command::SetPeakHold(want.peak));
        }
        if old.map(|o| o.fps) != Some(want.fps) {
            out.commands.push(sdr::Command::SetFps(want.fps));
        }
        self.sent = Some(want);
        if !self.history_sent {
            out.commands.push(sdr::Command::SetHistory(PIPELINE_HISTORY));
            self.history_sent = true;
        }

        let req = self.zoom_request(snap, cx, px);
        let on_gpu = self.gpu_fft();
        if let Some(r) = &self.gpu_run {
            r.iq.set_enabled(on_gpu);
        }
        if self.spectrum_on_sent != Some(!on_gpu) {
            out.commands.push(sdr::Command::SetSpectrumOn(!on_gpu));
            self.spectrum_on_sent = Some(!on_gpu);
            if !on_gpu {
                // Back to the pipeline's frames: take them from now.
                self.last_gen = snap.generation;
                self.last_zgen = snap.zoom_generation;
            }
        }
        if std::mem::take(&mut self.reset_ask) {
            out.commands.push(sdr::Command::ResetSpectrum);
        }
        // The GPU runs its own zoom FFT; the pipeline's is not needed then.
        self.gpu_zoom = req;
        let req = if on_gpu { None } else { req };
        if self.zoom_sent != Some(req) {
            out.commands.push(sdr::Command::SetZoom(req));
            self.zoom_sent = Some(req);
        }
    }

    /// The zoom FFT this view wants: one when the view is zoomed so far that
    /// the whole span's bins are wider than a pixel and a decimation would
    /// help. Its centre moves in steps of 1/32 of the view, so a pan does not
    /// ask for a new one every frame.
    fn zoom_request(&self, snap: &sdr::Snapshot, cx: &paint::Ctx, px: f32) -> Option<ZoomRequest> {
        if self.paused {
            return self.zoom_sent.flatten();
        }
        if !self.settings.zoom_fft {
            return None;
        }
        let v = cx.view;
        let span = cx.span;
        let main_bin = span.width / self.settings.fft_size as f64;
        if !v.zoomed(span) || main_bin * 1.5 < v.width() / px.max(1.0) as f64 {
            return None;
        }
        let width = v.width() * 1.5;
        let factor = sdr::zoom_factor(span.width, width);
        if factor < 2 {
            return None;
        }
        let rate = span.width / factor as f64;
        let quantum = v.width() / 32.0;
        let center = (v.center() / quantum).round() * quantum;
        let fft_size = view::zoom_fft_size(rate, v.width(), px, 0.05);
        let _ = snap;
        Some(ZoomRequest { center_hz: center, width_hz: width, fft_size })
    }
}

/// The span the snapshot describes, falling back to its newest frame's.
fn span_of(snap: &sdr::Snapshot) -> Span {
    if snap.sample_rate > 0.0 {
        Span { center: snap.center_hz, width: snap.sample_rate }
    } else if let Some(f) = &snap.latest {
        Span { center: f.center_hz, width: f.span_hz }
    } else {
        Span { center: snap.center_hz, width: 192_000.0 }
    }
}

/// The frames in `history` newer than `*last` (by generation), oldest first.
fn fresh(
    history: &VecDeque<Arc<SpectrumFrame>>,
    generation: u64,
    last: &mut u64,
) -> Vec<Arc<SpectrumFrame>> {
    if generation < *last {
        // The pipeline was restarted.
        *last = 0;
    }
    let n = (generation - *last).min(history.len() as u64) as usize;
    *last = generation;
    history.iter().skip(history.len() - n).cloned().collect()
}

/// Whether a zoom frame holds all of `v` in its clean part.
fn covers(z: &SpectrumFrame, v: View) -> bool {
    let half = z.span_hz * sdr::zoom::USABLE / 2.0;
    z.center_hz - half <= v.lo && z.center_hz + half >= v.hi
}

/// The system clock, UTC seconds since 1970 (NTP/GPS-disciplined at AJ3G's).
fn utc_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seq: u64) -> Arc<SpectrumFrame> {
        Arc::new(SpectrumFrame {
            center_hz: 0.0,
            span_hz: 1.0,
            db: vec![],
            peak: None,
            seq,
            enbw: 1.0,
            noise_dbm: 0.0,
        })
    }

    #[test]
    fn the_centre_follows_the_receiver_but_not_while_transmitting() {
        let mut sc = Scope::new(None, ScopeSettings::default());
        let span = Span { center: 14_200_000.0, width: 192_000.0 };
        let mut rig = ScopeRig {
            rx_hz: 14_290_400.0,
            mode: sdr::Mode::Usb,
            passband: (100.0, 2900.0),
            transmitting: true,
            on_air: true,
        };
        let mut out = ScopeOut::default();
        sc.follow(span, &rig, 0.0, &mut out);
        assert_eq!(out.recentre, None);
        rig.transmitting = false;
        sc.follow(span, &rig, 0.1, &mut out);
        assert_eq!(out.recentre, Some(14_290_000.0));
        // Inside 0.45 of the span it stays put.
        let mut out = ScopeOut::default();
        rig.rx_hz = 14_280_000.0;
        sc.follow(span, &rig, 5.0, &mut out);
        assert_eq!(out.recentre, None);
    }

    #[test]
    fn only_new_frames_are_taken() {
        let h: VecDeque<_> = (1..=5).map(frame).collect();
        let mut last = 3;
        let f = fresh(&h, 5, &mut last);
        assert_eq!(f.iter().map(|f| f.seq).collect::<Vec<_>>(), [4, 5]);
        assert_eq!(last, 5);
        assert!(fresh(&h, 5, &mut last).is_empty());
        // More new than kept: what is kept.
        let mut last = 0;
        assert_eq!(fresh(&h, 100, &mut last).len(), 5);
        // A restarted pipeline counts from zero again.
        let mut last = 100;
        assert_eq!(fresh(&h, 2, &mut last).len(), 2);
    }

    #[test]
    fn a_zoom_frame_must_hold_the_view_in_its_clean_part() {
        let z = SpectrumFrame {
            center_hz: 1000.0,
            span_hz: 100.0,
            db: vec![],
            peak: None,
            seq: 1,
            enbw: 1.0,
            noise_dbm: 0.0,
        };
        assert!(covers(&z, View { lo: 970.0, hi: 1030.0 }));
        assert!(!covers(&z, View { lo: 955.0, hi: 1030.0 }));
    }

    #[test]
    fn the_display_holds_over_an_over_and_starts_again_after_it() {
        let mut h = TxHold::default();
        assert_eq!(h.step(false, 0.0), (false, false));
        assert_eq!(h.step(true, 1.0), (true, false));
        assert_eq!(h.step(true, 5.0), (true, false));
        // Unkeyed: still held for the guard, then started again once.
        assert_eq!(h.step(false, 5.1), (true, false));
        assert_eq!(h.step(false, 5.1 + TX_GUARD_SECS), (false, true));
        assert_eq!(h.step(false, 6.5), (false, false));
        // Keyed again inside the guard: held on, no start in between.
        h.step(true, 7.0);
        h.step(false, 7.1);
        assert_eq!(h.step(true, 7.2), (true, false));
        assert_eq!(h.step(false, 7.3), (true, false));
        assert_eq!(h.step(false, 9.0), (false, true));
    }

    /// The scope drawn by egui with no window over a still snapshot, and the
    /// pointer's events for each frame; what it asked for each frame.
    fn frames(sc: &mut Scope, rig: ScopeRig, events: Vec<Vec<egui::Event>>) -> Vec<ScopeOut> {
        thread_local! {
            static CTX: (egui::Context, std::cell::Cell<f64>) =
                (egui::Context::default(), std::cell::Cell::new(0.0));
        }
        let (ctx, clock) = CTX.with(|c| (c.0.clone(), c.1.get()));
        let snap =
            sdr::Snapshot { center_hz: 14_100_000.0, sample_rate: 192_000.0, ..Default::default() };
        let mut outs = Vec::new();
        for (i, ev) in events.into_iter().enumerate() {
            let t = clock + (i + 1) as f64 * 0.05;
            CTX.with(|c| c.1.set(t));
            let input = egui::RawInput {
                time: Some(t),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 900.0),
                )),
                events: ev,
                ..Default::default()
            };
            let mut out = None;
            let mut full = ctx.run_ui(input, |ui| out = Some(sc.ui(ui, &snap, rig)));
            full.textures_delta.clear();
            outs.push(out.unwrap());
        }
        outs
    }

    fn press(pos: egui::Pos2, pressed: bool, shift: bool) -> Vec<egui::Event> {
        let modifiers = egui::Modifiers { shift, ..Default::default() };
        vec![
            egui::Event::ModifiersChanged(modifiers),
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers,
            },
        ]
    }

    fn spot(row: Option<u64>, call: &str, hz: f64, dial: f64) -> SkimSpot {
        SkimSpot {
            row,
            hz,
            audio_hz: (hz - dial) as f32,
            dial_hz: dial,
            call: call.into(),
            text: format!("CQ {call} FN42"),
            snr: -8.0,
            // Heard in the slot that has just ended.
            utc: utc_now() - 16.0,
            wsjt: Some(skim::WsjtMode::Ft8),
            key: None,
        }
    }

    fn ft8_rig(transmitting: bool) -> ScopeRig {
        ScopeRig {
            rx_hz: 14_074_000.0,
            mode: sdr::Mode::Usb,
            passband: (200.0, 3000.0),
            transmitting,
            on_air: transmitting,
        }
    }

    #[test]
    fn the_offset_bar_is_dropped_dragged_and_removed_and_tunes_nothing() {
        let mut sc = Scope::new(None, ScopeSettings::default());
        let rig = ft8_rig(false);
        let first = frames(&mut sc, rig, vec![vec![]]);
        assert!(first[0].tune_to.is_none());
        // The OFFSET chip's place: the middle of the passband.
        assert_eq!(OffsetBar::at_receiver(rig.rx_hz, rig.passband).hz, 14_075_600.0);
        // Shift-click well away from the receiver, in the spectrum, then
        // drag; transmitting or not, nothing is tuned.
        for tx in [false, true] {
            let rig = ft8_rig(tx);
            let (a, b) = (egui::pos2(300.0, 120.0), egui::pos2(340.0, 120.0));
            let outs = frames(
                &mut sc,
                rig,
                vec![
                    press(a, true, true),
                    vec![egui::Event::PointerMoved(b)],
                    press(b, false, false),
                ],
            );
            assert!(outs.iter().all(|o| o.tune_to.is_none() && o.passband.is_none()));
            let bar = sc.offset.expect("the bar is placed");
            assert!(!sc.offset_drag);
            // Where it ended is where the pointer let go, and it reads its
            // distance from the dial.
            let v = view::view(Span { center: 14_100_000.0, width: 192_000.0 }, None);
            let hz_b = v.lo + (v.hi - v.lo) * ((340.0 - 0.0) / 1280.0);
            assert!((bar.hz - hz_b).abs() < 3000.0, "{} against {hz_b}", bar.hz);
            assert_eq!(bar.offset_hz(rig.rx_hz, false), bar.hz - 14_074_000.0);
        }
        // Its × takes it away.
        let close = sc.offset_close.expect("the readout's ×").center();
        frames(
            &mut sc,
            ft8_rig(false),
            vec![press(close, true, false), press(close, false, false)],
        );
        assert!(sc.offset.is_none());
    }

    #[test]
    fn a_skimmer_label_picks_its_line_or_tunes_to_its_dial_but_not_in_tx() {
        let mut sc = Scope::new(None, ScopeSettings::default());
        let feed = SkimFeed {
            period_s: 15.0,
            spots: vec![
                spot(Some(7), "K1ABC", 14_075_200.0, 14_074_000.0),
                spot(None, "W9XYZ", 14_131_000.0, 14_130_000.0),
            ],
            ..Default::default()
        };
        sc.skim(Some(feed.clone()));
        frames(&mut sc, ft8_rig(false), vec![vec![]]);
        assert_eq!(sc.skim_shown.len(), 2);
        let at = |sc: &Scope, call: &str| {
            sc.skim_boxes
                .iter()
                .find(|(_, i)| sc.skim_shown[*i].0.call == call)
                .map(|(r, _)| r.center())
                .expect("the label is drawn")
        };
        // Shift-clicked a pixel or two off a labelled station, the offset bar
        // goes onto it and reads DECODE's AF for it.
        let k = at(&sc, "K1ABC");
        // The signal is a leader's length left of its box.
        let x_k = sc
            .skim_boxes
            .iter()
            .find(|(_, i)| sc.skim_shown[*i].0.call == "K1ABC")
            .map(|(r, _)| r.left() - paint::SKIM_LEAD)
            .unwrap();
        let near = egui::pos2(x_k + 2.0, k.y + 60.0);
        frames(&mut sc, ft8_rig(false), vec![press(near, true, true), press(near, false, false)]);
        let bar = sc.offset.take().expect("the bar");
        assert_eq!(bar.hz, 14_075_200.0);
        assert_eq!(bar.offset_hz(14_074_000.0, false), 1200.0);
        // DECODE's own: picks its line, tunes nothing.
        let k = at(&sc, "K1ABC");
        let outs =
            frames(&mut sc, ft8_rig(false), vec![press(k, true, false), press(k, false, false)]);
        assert_eq!(outs[1].skim_pick, Some(7));
        assert!(outs.iter().all(|o| o.tune_to.is_none()));
        // The wide skimmer's: tunes to its dial, but not while transmitting
        // (interlock 9).
        let w = at(&sc, "W9XYZ");
        let outs =
            frames(&mut sc, ft8_rig(true), vec![press(w, true, false), press(w, false, false)]);
        assert!(outs.iter().all(|o| o.tune_to.is_none() && o.skim_pick.is_none()));
        let outs =
            frames(&mut sc, ft8_rig(false), vec![press(w, true, false), press(w, false, false)]);
        assert_eq!(outs[1].tune_to, Some(14_130_000.0));
        // SKIM off: no labels.
        sc.skim(None);
        frames(&mut sc, ft8_rig(false), vec![vec![]]);
        assert!(sc.skim_shown.is_empty() && sc.skim_boxes.is_empty());
    }

    #[test]
    fn a_keyboard_label_tunes_onto_its_signal_in_its_mode_but_not_in_tx() {
        let mut sc = Scope::new(None, ScopeSettings::default());
        let key = |call: &str, hz: f64, mode: skim::KeyMode| SkimSpot {
            row: None,
            dial_hz: hz,
            audio_hz: 0.0,
            utc: utc_now(),
            key: Some(KeyTag { mode, wpm: 22, active: true }),
            ..spot(None, call, hz, hz)
        };
        let feed = SkimFeed {
            period_s: 15.0,
            spots: vec![
                key("W2XYZ", 14_090_000.0, skim::KeyMode::Cw),
                key("KB1XYZ", 14_110_000.0, skim::KeyMode::Psk),
            ],
            key_audio_hz: 1500.0,
        };
        sc.skim(Some(feed));
        frames(&mut sc, ft8_rig(false), vec![vec![]]);
        let at = |sc: &Scope, call: &str| {
            sc.skim_boxes
                .iter()
                .find(|(_, i)| sc.skim_shown[*i].0.call == call)
                .map(|(r, _)| r.center())
                .expect("the label is drawn")
        };
        // SAFETY INTERLOCK 9: nothing while transmitting.
        let c = at(&sc, "W2XYZ");
        let outs =
            frames(&mut sc, ft8_rig(true), vec![press(c, true, false), press(c, false, false)]);
        assert!(
            outs.iter().all(|o| o.tune_to.is_none() && o.mode.is_none() && o.key_pick.is_none())
        );
        // CW: the receiver in CW with the carrier at the passband's middle.
        let outs =
            frames(&mut sc, ft8_rig(false), vec![press(c, true, false), press(c, false, false)]);
        assert_eq!(outs[1].mode, Some(sdr::Mode::Cw));
        assert_eq!(outs[1].tune_to, Some(14_090_000.0 - 600.0));
        assert_eq!(outs[1].key_pick, Some(skim::KeyMode::Cw));
        // PSK31: USB, the carrier at DECODE's audio frequency.
        let p = at(&sc, "KB1XYZ");
        let outs =
            frames(&mut sc, ft8_rig(false), vec![press(p, true, false), press(p, false, false)]);
        assert_eq!(outs[1].mode, Some(sdr::Mode::Usb));
        assert_eq!(outs[1].tune_to, Some(14_110_000.0 - 1500.0));
        assert_eq!(outs[1].passband, Some((1250.0, 1750.0)));
        assert_eq!(outs[1].key_pick, Some(skim::KeyMode::Psk));
        assert!(sc.skim_shown.iter().any(|(s, _)| s.tag() == "KB1XYZ PSK"));
    }

    /// The whole scope on the CPU spectrum (no GPU), drawn by egui with no
    /// window, over a pipeline whose source simulates a transmission: the
    /// leakage at full scale, the dial read back as 0, and the panel's
    /// `transmitting` from 1 to 2.5 s. During the over the frame on screen
    /// does not change and the span stays; after it the spectrum is the
    /// receiver's again at once, with no peak or average left from the over
    /// and FIT where it was.
    #[test]
    fn the_cpu_scope_comes_back_from_an_over_as_it_went_in() {
        use sdr::{
            Command, Pipeline, PipelineConfig, SynthSignal, SyntheticSource, TxEffects, TxSim,
        };
        use std::sync::atomic::Ordering as O;
        let rate = 192_000.0;
        let centre = 14.2e6;
        let src =
            SyntheticSource::new(rate, centre, -80.0, vec![SynthSignal::carrier(20e3, -50.0)])
                .paced();
        let fx = TxEffects { strong_dbfs: Some(0.0), centre_hz: Some(0.0), ..Default::default() };
        let sim = TxSim::new(src, 1.0, 2.5, fx);
        let keyed = sim.keyed();
        let settings = ScopeSettings {
            fft_size: 4096,
            fps: 30.0,
            avg: true,
            peak: true,
            fit: true,
            fft_gpu: false,
            ..ScopeSettings::default()
        };
        let mut sc = Scope::new(None, settings);
        let p = Pipeline::spawn(
            Box::new(sim),
            PipelineConfig { spectrum: sc.spectrum_config(0.0), history: 16, ..Default::default() },
        );
        let ctx = egui::Context::default();
        let start = std::time::Instant::now();
        let mut held_seq = None;
        let mut levels = None;
        let mut seen_tx = false;
        let mut after = None;
        while start.elapsed().as_secs_f64() < 4.5 {
            std::thread::sleep(std::time::Duration::from_millis(15));
            let now = start.elapsed().as_secs_f64();
            let tx = keyed.load(O::Relaxed);
            seen_tx |= tx;
            let snap = p.snapshot();
            let rig = ScopeRig {
                rx_hz: centre + 20e3,
                mode: sdr::Mode::Usb,
                passband: (100.0, 2900.0),
                transmitting: tx,
                on_air: tx,
            };
            let input = egui::RawInput {
                time: Some(now),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 900.0),
                )),
                ..Default::default()
            };
            let mut out = None;
            let mut full = ctx.run_ui(input, |ui| out = Some(sc.ui(ui, &snap, rig)));
            full.textures_delta.clear();
            for c in out.unwrap().commands {
                p.send(c);
            }
            let seq = sc.main.as_ref().map(|f| f.seq);
            if tx {
                // Held: the frame from before the over, and its span.
                let s = *held_seq.get_or_insert(seq);
                assert_eq!(seq, s, "a frame was taken during the over");
                assert!(sc.held);
                assert_eq!(sc.span(&snap).center, centre);
                levels.get_or_insert((sc.settings.floor_dbm, sc.settings.ceil_dbm));
            } else if seen_tx && now > 2.5 + TX_GUARD_SECS + 0.5 {
                after = sc.main.clone();
            }
        }
        assert!(seen_tx, "the over happened");
        let f = after.expect("frames after the over");
        assert_eq!(f.center_hz, centre);
        assert!(f.noise_dbm < -100.0, "noise floor {} after the over", f.noise_dbm);
        let k = f.bin_of(centre + 20e3).unwrap();
        assert!((f.db[k] - -50.0).abs() < 3.0, "the carrier at {} dBFS", f.db[k]);
        let pk = f.peak.as_ref().unwrap().iter().copied().fold(f32::MIN, f32::max);
        assert!(pk < -40.0, "a peak of {pk} dBFS held from the over");
        // FIT never saw the over: the scale still has the noise on it and
        // its top nowhere near full scale.
        let (_, ceil) = levels.unwrap();
        let (f_now, c_now) = (sc.settings.floor_dbm, sc.settings.ceil_dbm);
        assert!(c_now <= ceil.max(-40.0), "FIT dragged up to {c_now} (from {ceil})");
        assert!(f_now < f.noise_dbm, "FIT floor {f_now} over the noise {}", f.noise_dbm);
        p.send(Command::Stop);
    }

    /// How soon after the radio says it is back in receive the CPU scope has
    /// a fresh spectrum and a fresh waterfall row (AJ3G's next-build item 6:
    /// timed modes want it inside a second). Measured on a paced synthetic
    /// stream keyed from 1 to 2 s, repainted every 15 ms.
    #[test]
    fn after_an_over_fresh_frames_and_rows_come_within_a_second() {
        use sdr::{
            Command, Pipeline, PipelineConfig, SynthSignal, SyntheticSource, TxEffects, TxSim,
        };
        use std::sync::atomic::Ordering as O;
        let (rate, centre) = (192_000.0, 14.2e6);
        let src =
            SyntheticSource::new(rate, centre, -80.0, vec![SynthSignal::carrier(20e3, -50.0)])
                .paced();
        let fx = TxEffects { strong_dbfs: Some(0.0), ..Default::default() };
        let sim = TxSim::new(src, 1.0, 2.0, fx);
        let keyed = sim.keyed();
        let settings = ScopeSettings {
            fft_size: 16_384,
            fps: 30.0,
            avg: true,
            peak: true,
            fft_gpu: false,
            ..ScopeSettings::default()
        };
        let mut sc = Scope::new(None, settings);
        let p = Pipeline::spawn(
            Box::new(sim),
            PipelineConfig { spectrum: sc.spectrum_config(0.0), history: 16, ..Default::default() },
        );
        let ctx = egui::Context::default();
        let start = std::time::Instant::now();
        let (mut seen_tx, mut rx_at) = (false, None);
        let (mut held, mut frame_at, mut row_at) = ((None, 0), None, None);
        while start.elapsed().as_secs_f64() < 4.0 && row_at.is_none() {
            std::thread::sleep(std::time::Duration::from_millis(15));
            let now = start.elapsed().as_secs_f64();
            let tx = keyed.load(O::Relaxed);
            seen_tx |= tx;
            let snap = p.snapshot();
            let rig = ScopeRig {
                rx_hz: centre + 20e3,
                mode: sdr::Mode::Usb,
                passband: (100.0, 2900.0),
                transmitting: tx,
                on_air: tx,
            };
            let input = egui::RawInput {
                time: Some(now),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 900.0),
                )),
                ..Default::default()
            };
            let mut out = None;
            let mut full = ctx.run_ui(input, |ui| out = Some(sc.ui(ui, &snap, rig)));
            full.textures_delta.clear();
            for c in out.unwrap().commands {
                p.send(c);
            }
            let seq = sc.main.as_ref().map(|f| f.seq);
            if tx {
                held = (seq, sc.rows.wf_head());
            } else if seen_tx {
                let t0 = *rx_at.get_or_insert(now);
                if frame_at.is_none() && seq != held.0 {
                    frame_at = Some(now - t0);
                }
                if row_at.is_none() && sc.rows.wf_head() != held.1 {
                    row_at = Some(now - t0);
                }
            }
        }
        p.send(Command::Stop);
        assert!(seen_tx, "the over happened");
        let frame = frame_at.expect("a fresh frame after the over");
        let row = row_at.expect("a fresh waterfall row after the over");
        eprintln!("after the over: fresh spectrum in {frame:.3} s, waterfall row in {row:.3} s");
        assert!(frame >= TX_GUARD_SECS - 0.05, "nothing fresh inside the guard: {frame}");
        assert!(frame < 1.0, "fresh spectrum {frame} s after the over");
        assert!(row < 1.0, "fresh waterfall row {row} s after the over");
    }
}
