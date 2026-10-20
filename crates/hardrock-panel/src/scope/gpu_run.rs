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
/// The shortest time without a frame, while samples keep coming, that is
/// taken for a stall (see [`stalled`]).
const STALL_SECS: f64 = 1.0;

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
    /// When the last frame of the whole span was made, and how far the
    /// stream had got then: the stall watchdog's.
    frame_at: (f64, u64),
    /// Resynchronisations the watchdog has done, for the status line.
    pub resyncs: u32,
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
            frame_at: (f64::NAN, 0),
            resyncs: 0,
        }
    }

    /// Start again from the newest samples: the next repaint builds the
    /// frame timing afresh, so the averages and the held peaks start from
    /// the first frame after now and nothing older is transformed. With
    /// `rebuild`, the GPU's buffers are made again too (the watchdog's
    /// remedy, for whatever wedged it).
    pub fn resync(&mut self, rebuild: bool) {
        self.epoch = None;
        self.main = None;
        self.zoom = None;
        self.frame_at = (f64::NAN, 0);
        if rebuild {
            self.setup = None;
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
        let newest = (written > 0).then(|| self.iq.epoch_at(written - 1)).flatten();
        // Paused (or held over a transmission) the spectrum stays where it
        // was, even if the centre moved meanwhile.
        let held = self.epoch.filter(|_| a.paused && self.main.is_some());
        let Some(epoch) = held.or(newest) else {
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
        if a.paused || !self.frame_at.0.is_finite() {
            self.frame_at = (a.now, written);
        }
        if !a.paused {
            let main = self.main.as_mut().expect("made above");
            main.catch_up(written, keep(main.hop(), main.n()));
            for _ in 0..MAX_FRAMES {
                if main.advance(written, Inst::Main, peak, 1, &mut ops).is_empty() {
                    break;
                }
                self.ready = true;
                out.main_frames += 1;
                self.frame_at = (a.now, written);
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

        // Samples are coming but no frame has been made for a while: the
        // timing is somehow out of step with the stream. Start again.
        if !a.paused {
            let main = self.main.as_ref().expect("made above");
            let (t, w) = self.frame_at;
            if stalled(a.now - t, written.saturating_sub(w), main.hop(), main.n(), main.rate()) {
                self.resyncs += 1;
                self.resync(true);
                return out;
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

/// Whether the whole span's spectrum has stalled: `secs` since its last
/// frame, in which `came` samples arrived, at `rate` with frames every `hop`
/// samples of `n`. Only when the samples for two frames' worth have come and
/// gone with none made, and never under [`STALL_SECS`], so a slow frame rate
/// or a big FFT is not mistaken for one, nor is a stream that stopped.
pub(super) fn stalled(secs: f64, came: u64, hop: f64, n: usize, rate: f64) -> bool {
    let due = 2.0 * (hop + n as f64);
    secs >= STALL_SECS.max(due / rate.max(1.0)) && came as f64 >= due
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stall_is_samples_without_frames_not_a_quiet_stream() {
        let rate = 192_000.0;
        let (hop, n) = (rate / 30.0, 65_536);
        // A second and a half of samples and no frame: stalled.
        assert!(stalled(1.5, (1.5 * rate) as u64, hop, n, rate));
        // The stream stopped (an over): nothing came, nothing is wrong.
        assert!(!stalled(30.0, 0, hop, n, rate));
        // Not long enough yet.
        assert!(!stalled(0.5, (0.5 * rate) as u64, hop, n, rate));
        // A 1M FFT at 192 kHz takes 5.5 s to fill: not a stall at 3 s.
        assert!(!stalled(3.0, 3 * 192_000, hop, 1 << 20, rate));
        assert!(stalled(12.0, 12 * 192_000, hop, 1 << 20, rate));
        // One frame every ten seconds (0.1 fps) is slow, not stalled.
        let slow = rate / 0.1;
        assert!(!stalled(8.0, 8 * 192_000, slow, 4096, rate));
    }

    use super::super::TxHold;
    use super::super::gpu_fft::{Engine, Targets};
    use sdr::{IqSource, SynthSignal, SyntheticSource, TxEffects, TxSim};
    use std::sync::atomic::Ordering;

    /// What the scope shows at the end of a simulated over, through the
    /// whole GPU path: the I/Q ring and its uploader, the scheduler, the
    /// compute passes and the readback, repainted 60 times a second of
    /// stream time, with the scope's hold over the transmission (decision
    /// 40) or without it. Returns the noise floor and the strongest of the
    /// trace and of the peak trace, before the over and at the end.
    fn over_on_the_gpu(fx: TxEffects, hold_tx: bool) -> Option<[(f32, f32, f32); 2]> {
        let (device, queue) = super::super::gpu_fft::tests::device()?;
        let rate = 192_000.0;
        let centre = 14.2e6;
        let mut run = GpuRun::new(&device, &queue);
        let sink = run.iq.sink();
        let mut engine = Engine::new(&device, run.iq.buffer(), RING);
        let tex = |f| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: 1024,
                    height: super::super::rows::WF_ROWS,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: f,
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let (wf, s3) = (tex(wgpu::TextureFormat::R16Float), tex(wgpu::TextureFormat::R32Float));
        let mut rows = Rows::new(1024);
        let settings = ScopeSettings {
            fft_size: 16_384,
            fps: 30.0,
            avg: true,
            peak: true,
            ..ScopeSettings::default()
        };
        let src =
            SyntheticSource::new(rate, centre, -80.0, vec![SynthSignal::carrier(20e3, -50.0)]);
        // Keyed from 2 s to 4 s of stream time; the end is looked at 1.5 s
        // after the over.
        let mut sim = TxSim::new(src, 2.0, 4.0, fx);
        let keyed = sim.keyed();
        let mut hold = TxHold::default();
        let mut buf = vec![0.0f32; 2 * 3200];
        let mut pushed = 0u64;
        let mut before = None;
        let mut last = (0.0f32, 0.0f32, 0.0f32);
        for frame in 0..(5.5 * 60.0) as u32 {
            let now = frame as f64 / 60.0;
            // A repaint's worth of samples (or none, in a gap).
            let n = sim.read(&mut buf);
            if n > 0 {
                sink(&buf[..n], sim.center_hz(), rate);
                pushed += (n / 2) as u64;
            }
            let t = std::time::Instant::now();
            while run.iq.written() < pushed && t.elapsed().as_secs() < 5 {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let tx = keyed.load(Ordering::Relaxed);
            let (held, resume) = if hold_tx { hold.step(tx, now) } else { (false, false) };
            if resume {
                run.resync(false);
            }
            let span = run.epoch.map_or(centre, |e| e.center_hz);
            let view = View { lo: span - rate / 2.0, hi: span + rate / 2.0 };
            let ask = Ask {
                settings: &settings,
                cal_db: 0.0,
                view,
                zoom: None,
                paused: held,
                trace_cols: 1024,
                queries: vec![(view.lo, view.hi)],
                now,
                utc: now,
            };
            let mut up = Upload::default();
            run.plan(&ask, &mut rows, &mut up);
            if let Some(g) = up.gpu.take() {
                let mut enc = device.create_command_encoder(&Default::default());
                let t = Targets { wf: &wf, wf_w: 1024, s3: &s3, s3_w: 1024 };
                engine.record(&device, &queue, &mut enc, &g.0.work, &t);
                queue.submit([enc.finish()]);
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                if let Some(r) = engine.collect() {
                    *run.readout.lock().unwrap() = Some(r.clone());
                }
            }
            if let Some(r) = run.take_readout() {
                let max = |v: &[f32]| v.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let pk = r.frame.peak.as_deref().map_or(f32::NAN, max);
                last = (r.frame.noise_dbm, max(&r.frame.db), pk);
            }
            if now >= 1.9 && before.is_none() {
                before = Some(last);
            }
        }
        Some([before.unwrap(), last])
    }

    fn assert_recovered(what: &str, [before, after]: [(f32, f32, f32); 2]) {
        let (nf0, max0, pk0) = before;
        let (nf, max, pk) = after;
        assert!((nf - nf0).abs() < 2.0, "{what}: noise floor {nf} after, {nf0} before");
        assert!((max - max0).abs() < 3.0, "{what}: strongest {max} after, {max0} before");
        assert!(pk.is_finite() && pk < pk0 + 3.0, "{what}: peak hold {pk} after, {pk0} before");
    }

    #[test]
    fn after_an_over_the_gpu_spectrum_is_what_the_receiver_hears() {
        let cases = [
            ("the stream stops", TxEffects { gap: true, ..Default::default() }),
            ("leakage at full scale", TxEffects { strong_dbfs: Some(0.0), ..Default::default() }),
            ("the receiver muted", TxEffects { zeros: true, ..Default::default() }),
            ("samples not numbers", TxEffects { nan: true, ..Default::default() }),
            (
                "the dial reads VFO B and stops",
                TxEffects { gap: true, centre_hz: Some(14.25e6), ..Default::default() },
            ),
            (
                "the dial reads 0 under leakage",
                TxEffects { strong_dbfs: Some(-1.0), centre_hz: Some(0.0), ..Default::default() },
            ),
        ];
        for (what, fx) in cases {
            let Some(r) = over_on_the_gpu(fx, true) else { return };
            assert_recovered(what, r);
        }
    }

    /// Without the hold, full-scale leakage is still in the held peaks well
    /// after the over: what the hold is for.
    #[test]
    fn without_the_hold_an_over_lingers_in_the_peaks() {
        let fx = TxEffects { strong_dbfs: Some(0.0), ..Default::default() };
        let Some([before, after]) = over_on_the_gpu(fx, false) else { return };
        assert!(after.2 > before.2 + 20.0, "{after:?} vs {before:?}");
    }

    /// Unheld, samples that are not numbers go through the shader's
    /// averages and peaks; they must not stay there once the over is done.
    #[test]
    fn a_nan_does_not_stay_in_the_gpu_averages() {
        let fx = TxEffects { nan: true, ..Default::default() };
        let Some(r) = over_on_the_gpu(fx, false) else { return };
        assert_recovered("samples not numbers, unheld", r);
    }
}
