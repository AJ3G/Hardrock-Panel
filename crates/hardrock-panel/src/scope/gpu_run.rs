//! The FFT on the GPU, from the scope's side: once a repaint, look at how
//! far the I/Q ring has got, work out what the GPU spectrum should do with
//! it (the frames due, their waterfall and 3D rows, the trace on screen,
//! the markers' peaks), and turn what came back a frame or two ago into
//! what the rest of the scope reads.
//!
//! Nothing here touches the samples or waits for the GPU: the timing is
//! [`super::gpu_spec`]'s, the slots and bands are [`Rows`]', and the GPU
//! side records it all into egui's encoder.

use std::sync::{Arc, Mutex};

use eframe::egui_wgpu::wgpu;
use sdr::{Peak, SpectrumFrame, Window, ZoomRequest};

use super::gpu::GpuFrame;
use super::gpu_fft::{GpuWork, InstRun, MAX_QUERIES, MAX_TRACE, ReadMeta, Readout};
use super::gpu_iq::{Epoch, GpuIq, RING, SAFE};
use super::gpu_spec::{Dst, Inst, InstCfg, Op, Sched, ZoomSched};
use super::markers::ExactPeak;
use super::rows::{GpuPart, Rows, Upload};
use super::settings::ScopeSettings;
use super::view::{Span, View, bin_range};

/// The most frames of each spectrum made in one repaint: a stall is caught
/// up by skipping, not by a burst.
const MAX_FRAMES: usize = 8;
/// Readbacks a second at most.
const READS_PER_SEC: f64 = 30.0;

/// What the scope asks of the GPU spectrum this repaint.
pub(super) struct Ask<'a> {
    pub settings: &'a ScopeSettings,
    pub cal_db: f32,
    pub view: View,
    /// The zoom FFT the view wants (see `Scope::zoom_request`).
    pub zoom: Option<ZoomRequest>,
    pub paused: bool,
    /// Columns across the trace, physical pixels.
    pub trace_cols: u32,
    /// Bands to find the strongest bin in, Hz: the view, and each tracking
    /// marker's window.
    pub queries: Vec<(f64, f64)>,
    pub now: f64,
    pub utc: f64,
}

/// What the scope gets back from planning a repaint.
#[derive(Default)]
pub(super) struct Planned {
    /// Frames of the whole span's spectrum made.
    pub main_frames: u32,
    /// Waterfall rows completed.
    pub wf_rows: u32,
    /// The trace on screen: which band its columns cover, Hz, and how many.
    pub trace: Option<TraceGeom>,
}

/// Where the GPU's trace columns are.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct TraceGeom {
    pub lo: f64,
    pub hi: f64,
    pub cols: u32,
    pub peak: bool,
    pub zoom: bool,
}

/// One spectrum's set-up as last used.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Setup {
    n: usize,
    window: Window,
    overlap: sdr::Overlap,
    fps: f32,
}

struct ZoomRun {
    sched: ZoomSched,
    cfg: InstCfg,
    fft_size: usize,
    /// The zoom's band, Hz.
    span: Span,
    ready: bool,
}

/// The scope's GPU spectrum state.
pub(super) struct GpuRun {
    pub iq: Arc<GpuIq>,
    stream: u64,
    epoch: Option<Epoch>,
    setup: Option<Setup>,
    main: Option<Sched>,
    cfg: InstCfg,
    /// A frame has been made since the buffers were last built, so the
    /// GPU's spectrum holds something.
    ready: bool,
    zoom: Option<ZoomRun>,
    generation: u64,
    seq: u64,
    readout: Arc<Mutex<Option<Readout>>>,
    seen: u64,
    read_at: f64,
    /// Samples a second into the ring lately, for the status line.
    rate_at: (f64, u64),
    pub samples_per_sec: f64,
}

impl GpuRun {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> GpuRun {
        GpuRun {
            iq: Arc::new(GpuIq::new(device, queue)),
            stream: 0,
            epoch: None,
            setup: None,
            main: None,
            cfg: InstCfg { n: 0, window: Window::Hann, generation: 0 },
            ready: false,
            zoom: None,
            generation: 0,
            seq: 0,
            readout: Arc::new(Mutex::new(None)),
            seen: 0,
            read_at: f64::NEG_INFINITY,
            rate_at: (f64::NAN, 0),
            samples_per_sec: 0.0,
        }
    }

    fn next_gen(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    /// Plan this repaint: the ops into `up.gpu`, the rows' slots into `up`.
    pub fn plan(&mut self, a: &Ask, rows: &mut Rows, up: &mut Upload) -> Planned {
        let mut out = Planned::default();
        let s = a.settings;
        if self.iq.stream() != self.stream {
            self.stream = self.iq.stream();
            self.epoch = None;
            self.main = None;
            self.zoom = None;
        }
        let written = self.iq.written();
        self.meter(a.now, written);
        let Some(epoch) = (written > 0).then(|| self.iq.epoch_at(written - 1)).flatten() else {
            return out;
        };
        let setup = Setup {
            n: s.fft_size,
            window: s.window.to_sdr(),
            overlap: s.overlap.to_sdr(),
            fps: s.fps,
        };
        let new_epoch = self.epoch != Some(epoch);
        let old = self.setup;
        if new_epoch
            || self.main.is_none()
            || old.is_none_or(|o| o.n != setup.n || o.window != setup.window)
        {
            if old.is_none_or(|o| o.n != setup.n || o.window != setup.window) {
                self.cfg = InstCfg {
                    n: setup.n as u32,
                    window: setup.window,
                    generation: self.next_gen(),
                };
                self.ready = false;
            }
            let start = epoch.start.max(written.saturating_sub(setup.n as u64));
            self.main = Some(Sched::new(setup.n, epoch.rate, setup.fps, setup.overlap, start));
            if new_epoch {
                self.zoom = None;
            }
        }
        self.epoch = Some(epoch);
        self.setup = Some(setup);
        let Some(main) = self.main.as_mut() else { return out };
        main.set_fps(setup.fps);
        main.set_overlap(setup.overlap);
        if !s.peak {
            main.forget_peaks();
        }
        let span = Span { center: epoch.center_hz, width: epoch.rate };
        let keep =
            |hop: f64, n: usize| ((hop * 8.0) as u64 + n as u64).min(RING as u64 - SAFE - n as u64);

        // The zoom FFT, made or moved as the view asks.
        self.zoom_to(a.zoom, span, written, setup);

        let zoom_shown = self.zoom.as_ref().is_some_and(|z| z.ready && covers(z.span, a.view));
        let mut ops = Vec::new();
        let peak = s.peak;
        let frames_per_row = s.wf_speed.frames_per_row();
        let mut src_made = false;
        if !a.paused {
            let main = self.main.as_mut().expect("made above");
            main.catch_up(written, keep(main.hop(), main.n()));
            for _ in 0..MAX_FRAMES {
                if main.advance(written, Inst::Main, peak, 1, &mut ops).is_empty() {
                    break;
                }
                self.ready = true;
                out.main_frames += 1;
                if !zoom_shown {
                    src_made = true;
                    wf_ops(
                        rows,
                        Inst::Main,
                        span,
                        setup.n,
                        a,
                        frames_per_row,
                        &mut ops,
                        up,
                        &mut out,
                    );
                }
            }
            if let Some(z) = self.zoom.as_mut() {
                let zkeep = ((epoch.rate * 0.25) as u64).min(RING as u64 - SAFE);
                let max_in = (epoch.rate * 0.2) as u64;
                for _ in 0..MAX_FRAMES {
                    let made = z.sched.advance(written, zkeep, max_in, peak, 1, &mut ops);
                    if made.is_empty() {
                        break;
                    }
                    z.ready = true;
                    if zoom_shown {
                        src_made = true;
                        let n = z.fft_size;
                        wf_ops(
                            rows,
                            Inst::Zoom,
                            z.span,
                            n,
                            a,
                            frames_per_row,
                            &mut ops,
                            up,
                            &mut out,
                        );
                    }
                }
            }
        }

        // The 3D surface's new row, from the last frame made.
        let (src_inst, src_span, src_n) = match &self.zoom {
            Some(z) if zoom_shown => (Inst::Zoom, z.span, z.fft_size),
            _ => (Inst::Main, span, setup.n),
        };
        if s.top.has_surface() {
            let frame = (src_made && !a.paused).then_some(src_span);
            if let Some((cov, slots)) =
                rows.push_surface_gpu(frame, a.view, a.now, s.surface_rows_per_sec, up)
            {
                let (lo, hi) = bin_range(src_span.lo(), src_span.width, src_n, cov.lo, cov.hi);
                let cols = rows.tex_w() as u32;
                ops.push(Op::Pool {
                    inst: src_inst,
                    peak: false,
                    a: lo,
                    b: hi,
                    cols,
                    dst: Dst::Surface,
                    max: false,
                });
                ops.push(Op::SurfaceRow { slots });
            }
        } else {
            rows.clear_surface();
        }

        // The trace on screen, every repaint, so a pan or zoom shows at once.
        let shown_ready = if zoom_shown { true } else { self.ready };
        let mut readback = None;
        if shown_ready && a.trace_cols >= 2 {
            let lo = a.view.lo.max(src_span.lo());
            let hi = a.view.hi.min(src_span.hi());
            if hi > lo {
                let share = (hi - lo) / a.view.width().max(1e-9);
                let cols = ((a.trace_cols as f64 * share).round() as u32).clamp(2, MAX_TRACE);
                let (b0, b1) = bin_range(src_span.lo(), src_span.width, src_n, lo, hi);
                for (pk, dst) in [(false, Dst::Trace), (true, Dst::PeakTrace)] {
                    if pk && !peak {
                        continue;
                    }
                    ops.push(Op::Pool {
                        inst: src_inst,
                        peak: pk,
                        a: b0,
                        b: b1,
                        cols,
                        dst,
                        max: false,
                    });
                }
                let bin_w = src_span.width / src_n as f64;
                let queries: Vec<(f64, f64)> = a
                    .queries
                    .iter()
                    .copied()
                    .filter(|(l, h)| h > l && *h > src_span.lo() && *l < src_span.hi())
                    .take(MAX_QUERIES)
                    .collect();
                let bins: Vec<(u32, u32)> = queries
                    .iter()
                    .map(|&(l, h)| {
                        let k0 = ((l - src_span.lo()) / bin_w).floor().max(0.0) as u32;
                        let k1 =
                            (((h - src_span.lo()) / bin_w).ceil() as u32).min(src_n as u32 - 1);
                        (k0.min(k1), k1)
                    })
                    .collect();
                if !bins.is_empty() {
                    ops.push(Op::Peaks { inst: src_inst, queries: bins });
                }
                out.trace = Some(TraceGeom { lo, hi, cols, peak, zoom: zoom_shown });
                if a.now - self.read_at >= 1.0 / READS_PER_SEC {
                    self.read_at = a.now;
                    self.seq += 1;
                    let (_, _, enbw) = super::gpu_fft::window(setup.window, src_n);
                    readback = Some(ReadMeta {
                        seq: self.seq,
                        inst: src_inst,
                        center_hz: src_span.center,
                        span_hz: src_span.width,
                        n: src_n as u32,
                        enbw,
                        lo_hz: lo,
                        hi_hz: hi,
                        cols,
                        peak,
                        queries,
                    });
                }
            }
        }

        let main = self.main.as_ref().expect("made above");
        let run = |cfg: InstCfg, sched: &Sched| InstRun {
            cfg,
            alpha: 1.0 / s.averaging().frames() as f32,
            fall: 10.0 * (sched.hop() / sched.rate()) as f32,
            cal: a.cal_db,
        };
        let work = GpuWork {
            main: run(self.cfg, main),
            zoom: self.zoom.as_ref().map(|z| (run(z.cfg, &z.sched.spec), z.sched.factor)),
            ops,
            readback,
        };
        up.gpu = Some(GpuPart(Arc::new(GpuFrame {
            work,
            iq: self.iq.buffer().clone(),
            iq_len: self.iq.ring_len(),
            readout: self.readout.clone(),
        })));
        out
    }

    /// Start, move or stop the zoom FFT for `want`.
    fn zoom_to(&mut self, want: Option<ZoomRequest>, span: Span, written: u64, setup: Setup) {
        let Some(req) = want else {
            self.zoom = None;
            return;
        };
        let factor = sdr::zoom_factor(span.width, req.width_hz);
        if factor < 2 {
            self.zoom = None;
            return;
        }
        let offset = req.center_hz - span.center;
        let same = self.zoom.as_ref().is_some_and(|z| {
            z.sched.factor == factor && z.fft_size == req.fft_size && z.cfg.window == setup.window
        });
        if same {
            let z = self.zoom.as_mut().expect("checked");
            if z.sched.offset() != offset {
                z.sched.set_offset(offset);
                z.ready = false;
            }
            z.span = Span { center: req.center_hz, width: span.width / factor as f64 };
            z.sched.spec.set_fps(setup.fps);
            z.sched.spec.set_overlap(setup.overlap);
            return;
        }
        let generation = self.next_gen();
        let sched = ZoomSched::new(
            factor,
            offset,
            span.width,
            req.fft_size,
            setup.fps,
            setup.overlap,
            written,
        );
        self.zoom = Some(ZoomRun {
            sched,
            cfg: InstCfg { n: req.fft_size as u32, window: setup.window, generation },
            fft_size: req.fft_size,
            span: Span { center: req.center_hz, width: span.width / factor as f64 },
            ready: false,
        });
    }

    fn meter(&mut self, now: f64, written: u64) {
        let (t, w) = self.rate_at;
        if !t.is_finite() || written < w {
            self.rate_at = (now, written);
        } else if now - t >= 0.5 {
            self.samples_per_sec = (written - w) as f64 / (now - t);
            self.rate_at = (now, written);
        }
    }

    /// The readout that came since last asked, for the rest of the scope.
    pub fn take_readout(&mut self) -> Option<Taken> {
        let r = {
            let g = self.readout.lock().unwrap_or_else(|e| e.into_inner());
            let seq = g.as_ref()?.meta.as_ref()?.seq;
            if seq <= self.seen {
                return None;
            }
            self.seen = seq;
            g.clone()?
        };
        Some(taken(&r))
    }
}

/// A readout as the scope uses it.
pub(super) struct Taken {
    /// A frame whose bins are the trace's columns, with the real
    /// resolution kept where it matters: `bin_width() * enbw` is the real
    /// RBW and `noise_dbm` is per real bin.
    pub frame: SpectrumFrame,
    /// The strongest bins in the bands asked for, at full resolution.
    pub peaks: Vec<ExactPeak>,
    /// The real bin width, Hz.
    pub bin_hz: f64,
    /// It is the zoom FFT's.
    pub zoom: bool,
}

fn taken(r: &Readout) -> Taken {
    let m = r.meta.as_ref().expect("checked");
    let zoom = m.inst == Inst::Zoom;
    let cols = r.trace.len().max(1);
    let col_w = (m.hi_hz - m.lo_hz) / cols as f64;
    let bin_w = m.span_hz / m.n.max(1) as f64;
    let frame = SpectrumFrame {
        center_hz: (m.lo_hz + m.hi_hz) / 2.0,
        span_hz: m.hi_hz - m.lo_hz,
        db: r.trace.clone(),
        peak: m.peak.then(|| r.peak_trace.clone()),
        seq: m.seq,
        enbw: (m.enbw as f64 * bin_w / col_w.max(1e-9)) as f32,
        noise_dbm: r.noise[usize::from(zoom)],
    };
    let start = m.center_hz - m.span_hz / 2.0;
    let peaks = m
        .queries
        .iter()
        .zip(&r.peaks)
        .map(|(&(lo, hi), p)| {
            let k = p[0].max(0.0) as usize;
            let peak = if k == 0 || k + 1 >= m.n as usize {
                Peak { bin: k, hz: start + k as f64 * bin_w, dbm: p[2] }
            } else {
                let mut pk =
                    sdr::interpolate_peak(&p[1..4], 1, start + (k as f64 - 1.0) * bin_w, bin_w);
                pk.bin = k;
                pk
            };
            ExactPeak { lo, hi, peak }
        })
        .collect();
    Taken { frame, peaks, bin_hz: bin_w, zoom }
}

/// Whether the zoom's clean part holds the view.
fn covers(z: Span, v: View) -> bool {
    let half = z.width * sdr::zoom::USABLE / 2.0;
    z.center - half <= v.lo && z.center + half >= v.hi
}

/// One frame's waterfall row ops, from `inst` over `span` (`n` bins).
#[allow(clippy::too_many_arguments)]
fn wf_ops(
    rows: &mut Rows,
    inst: Inst,
    span: Span,
    n: usize,
    a: &Ask,
    frames_per_row: u32,
    ops: &mut Vec<Op>,
    up: &mut Upload,
    out: &mut Planned,
) {
    let Some(step) = rows.push_waterfall_gpu(span, a.view, frames_per_row, a.utc, up) else {
        return;
    };
    let (lo, hi) = bin_range(span.lo(), span.width, n, step.cov.lo, step.cov.hi);
    ops.push(Op::Pool {
        inst,
        peak: false,
        a: lo,
        b: hi,
        cols: rows.tex_w() as u32,
        dst: Dst::Pend,
        max: step.max,
    });
    if let Some(slot) = step.slot {
        ops.push(Op::WfRow { slot });
        out.wf_rows += 1;
    }
}
