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
//! let out = scope.ui(ui, &snap, ScopeRig { rx_hz, mode, passband, transmitting });
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
//! measurement marker. Nothing tunes while the radio transmits (safety
//! interlock 9).

pub mod bandplan;
mod controls;
pub mod gpu;
pub mod levels;
mod markers;
mod paint;
pub mod palette;
mod persistence;
pub mod pointer;
mod rows;
pub mod settings;
pub mod view;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use sdr::{SpectrumFrame, ZoomRequest};

pub use bandplan::DataMode;
pub use settings::ScopeSettings;

use markers::Markers;
use persistence::Persistence;
use pointer::{Drag, Wheel};
use rows::{Rows, Upload};
use view::{FreqMap, Span, View};

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
    /// A data-mode button was pressed: `tune_to`, `passband` and `mode` carry
    /// what it sets; this says which, for the panel's own display.
    pub data_mode: Option<DataMode>,
    /// The SPAN key: ask the radio for this IQ sample rate.
    pub sample_rate: Option<u32>,
    /// For the pipeline: FFT size, window, overlap, averaging, peak hold,
    /// frame rate, history and the zoom FFT, as the settings change.
    pub commands: Vec<sdr::Command>,
    /// A setting changed: save [`Scope::settings`].
    pub settings_changed: bool,
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
    digi: Option<DataMode>,
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
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

impl Scope {
    /// A scope with `settings`, drawing its waterfall and 3D surface on the
    /// GPU when `render_state` is given (eframe's wgpu backend).
    pub fn new(render_state: Option<&RenderState>, mut settings: ScopeSettings) -> Scope {
        settings.sanitise();
        let gpu = render_state.map(gpu::init);
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
            cursor: String::new(),
            row_at: 0.0,
            row_interval: 1.0 / 60.0,
            cols: Vec::new(),
            peak_cols: Vec::new(),
            frame_times: VecDeque::new(),
            xvtr_text,
        }
    }

    /// The settings to save.
    pub fn settings(&self) -> &ScopeSettings {
        &self.settings
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
        }
    }

    fn map(&self) -> FreqMap {
        FreqMap { ppm: self.settings.ppm, transverter_hz: self.settings.transverter_hz }
    }

    /// Draw the scope and its controls, the whole width of `ui`.
    pub fn ui(&mut self, ui: &mut egui::Ui, snap: &sdr::Snapshot, rig: ScopeRig) -> ScopeOut {
        let mut out = ScopeOut::default();
        let now = ui.input(|i| i.time);
        let before = self.settings.clone();

        let span = span_of(snap);
        let cur_view = view::view(span, self.want_view);
        let upload = self.take_frames(snap, span, cur_view, now);

        // The receiver past 0.45 of the span: ask for a new centre, once a
        // second at most while it is on its way.
        self.follow(span, &rig, now, &mut out);
        let span = span_of(snap);
        let v = view::view(span, self.want_view);

        let cx = paint::Ctx { span, view: v, map: self.map(), rig, now };
        paint::tft(self, ui, &cx, snap, upload, &mut out);
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

    /// Take the frames that came since last time: the trace, and rows for
    /// the waterfall and the surface. Nothing is taken while paused.
    fn take_frames(
        &mut self,
        snap: &sdr::Snapshot,
        span: Span,
        v: View,
        now: f64,
    ) -> Option<Arc<Upload>> {
        let mut up = Upload::default();
        let new_main = fresh(&snap.history, snap.generation, &mut self.last_gen);
        let new_zoom = fresh(&snap.zoom_history, snap.zoom_generation, &mut self.last_zgen);
        if self.paused {
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
            // SAFETY INTERLOCK 8: the centre is the DUO's VFO A; never moved
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
        let v = view::view(span, self.want_view);
        if self.drag.is_none()
            && let Some(k) = view::keep_in_view(span, v, rig.rx_hz)
        {
            self.want_view = Some(k);
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
}
