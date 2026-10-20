//! The GPU spectrum's buffers and kernels, and the recording of a frame's
//! [`Op`]s into compute passes (see `shaders/spectrum.wgsl` for the
//! kernels and [`super::gpu_spec`] for what the ops mean).
//!
//! One [`Engine`] per scope. Each op becomes a dispatch or two with its
//! settings in its own 256-byte slot of one uniform buffer, written once a
//! frame, so a frame's whole list is recorded into egui's encoder in order
//! and runs before egui's render pass reads the rows it wrote. What comes
//! back to the CPU is a few kilobytes a frame (the noise floors, the
//! markers' peaks and the trace on screen), copied into one of three
//! staging buffers that are mapped when the frame is submitted and read a
//! frame or two later; nothing waits for the GPU.

use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use eframe::egui_wgpu::wgpu;
use sdr::Window;

use super::gpu_spec::{Decim, Dst, Inst, InstCfg, Op, split, twiddles, zoom_filter, zoom_taps};

/// Bytes a dispatch's settings take in the uniform buffer.
const SLOT: u64 = 256;
/// Dispatches a frame, at most.
const MAX_SLOTS: usize = 1024;
/// Complex samples between the two FFT passes: segments are batched up to
/// this many points in all.
pub const SCRATCH: u64 = 1 << 21;
/// The zoom's decimated ring, complex samples.
pub const ZRING: u32 = 1 << 21;
/// The decimator's partial sums.
const PART: u64 = 1 << 20;
/// Filter taps a decimator thread sums.
const CHUNK: u32 = 4096;
/// The widest trace read back, columns.
pub const MAX_TRACE: u32 = 8192;
/// The widest row, columns (super::gpu::MAX_TEX_W).
pub const MAX_ROW: u32 = 16_384;
/// Marker queries a frame.
pub const MAX_QUERIES: usize = 8;

// The row buffer, in floats: results (noise floors at 0 and 1, the queries'
// peaks from 8), the trace and its peak hold, the 3D row and the waterfall
// row being gathered. The 3D row is copied to a texture from its offset,
// which must be a multiple of 256 bytes.
const RES_PEAKS: u32 = 8;
const TRACE_OFF: u32 = 64;
const PEAK_OFF: u32 = TRACE_OFF + MAX_TRACE;
const S3_OFF: u32 = PEAK_OFF + MAX_TRACE;
const PEND_OFF: u32 = S3_OFF + MAX_ROW;
const OUT_LEN: u32 = PEND_OFF + MAX_ROW;
/// What is read back: the results and both traces.
const READ_LEN: u32 = S3_OFF;

const F_LATEST: u32 = 1;
const F_PRIMED: u32 = 2;
const F_PEAK: u32 = 4;
const F_PEAK_RESET: u32 = 8;
const F_MAX: u32 = 16;

/// What one spectrum needs besides its buffers, this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InstRun {
    pub cfg: InstCfg,
    /// Exponential averaging weight once primed (1 / frames averaged).
    pub alpha: f32,
    /// How far held peaks fall a frame, dB.
    pub fall: f32,
    /// dBFS to dBm.
    pub cal: f32,
}

/// What the CPU wants read back this frame, and what it needs to make
/// sense of it when it comes back.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadMeta {
    pub seq: u64,
    pub inst: Inst,
    /// The frame's band, Hz, and its true bin width and ENBW.
    pub center_hz: f64,
    pub span_hz: f64,
    pub n: u32,
    pub enbw: f32,
    /// The trace's columns, the band they cover, and whether the peak hold
    /// trace was made too.
    pub lo_hz: f64,
    pub hi_hz: f64,
    pub cols: u32,
    pub peak: bool,
    /// The markers' queries, Hz.
    pub queries: Vec<(f64, f64)>,
}

/// One frame's work.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuWork {
    pub main: InstRun,
    pub zoom: Option<(InstRun, u32)>,
    pub ops: Vec<Op>,
    pub readback: Option<ReadMeta>,
}

/// What came back.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Readout {
    pub meta: Option<ReadMeta>,
    /// Main and zoom noise floors, dBm per bin.
    pub noise: [f32; 2],
    /// Per query: bin, and the levels at bin - 1, bin, bin + 1.
    pub peaks: Vec<[f32; 4]>,
    pub trace: Vec<f32>,
    pub peak_trace: Vec<f32>,
}

/// Where the rows go: the waterfall's and the 3D surface's textures.
pub struct Targets<'a> {
    pub wf: &'a wgpu::Texture,
    pub wf_w: u32,
    pub s3: &'a wgpu::Texture,
    pub s3_w: u32,
}

#[derive(Clone, Copy)]
enum Bind {
    Uniform,
    Ro,
    Rw,
}

struct Kernel {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

fn kernel(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    entry: &str,
    binds: &[(u32, Bind)],
) -> Kernel {
    let entries: Vec<wgpu::BindGroupLayoutEntry> = binds
        .iter()
        .map(|&(binding, b)| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: match b {
                    Bind::Uniform => wgpu::BufferBindingType::Uniform,
                    Bind::Ro => wgpu::BufferBindingType::Storage { read_only: true },
                    Bind::Rw => wgpu::BufferBindingType::Storage { read_only: false },
                },
                has_dynamic_offset: matches!(b, Bind::Uniform),
                min_binding_size: None,
            },
            count: None,
        })
        .collect();
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(entry),
        entries: &entries,
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(entry),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(entry),
        layout: Some(&pl),
        module,
        entry_point: Some(entry),
        compilation_options: Default::default(),
        cache: None,
    });
    Kernel { pipeline, layout }
}

struct Kernels {
    cols: Kernel,
    rows: Kernel,
    acc: Kernel,
    frame: Kernel,
    noise: Kernel,
    pool: Kernel,
    pack: Kernel,
    peaks: Kernel,
    dpart: Kernel,
    dsum: Kernel,
}

/// The kernels' source.
pub const SPECTRUM_WGSL: &str = include_str!("shaders/spectrum.wgsl");

impl Kernels {
    fn new(device: &wgpu::Device) -> Kernels {
        use Bind::*;
        let m = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scope-spectrum"),
            source: wgpu::ShaderSource::Wgsl(SPECTRUM_WGSL.into()),
        });
        let k = |e, b: &[(u32, Bind)]| kernel(device, &m, e, b);
        Kernels {
            cols: k("fft_cols", &[(0, Uniform), (1, Ro), (2, Ro), (3, Ro), (4, Rw)]),
            rows: k("fft_rows", &[(0, Uniform), (3, Ro), (4, Rw), (5, Rw)]),
            acc: k("accumulate", &[(0, Uniform), (5, Rw), (6, Rw)]),
            frame: k("frame", &[(0, Uniform), (5, Rw), (6, Rw), (7, Rw)]),
            noise: k("noise", &[(0, Uniform), (7, Rw), (8, Rw)]),
            pool: k("pool", &[(0, Uniform), (6, Rw), (8, Rw)]),
            pack: k("pack", &[(0, Uniform), (8, Rw), (9, Rw)]),
            peaks: k("peaks", &[(0, Uniform), (6, Rw), (8, Rw)]),
            dpart: k("decim_partial", &[(0, Uniform), (1, Ro), (10, Ro), (12, Rw)]),
            dsum: k("decim_sum", &[(0, Uniform), (11, Rw), (12, Rw)]),
        }
    }
}

fn buffer(
    device: &wgpu::Device,
    label: &str,
    size: u64,
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size.max(16),
        usage,
        mapped_at_creation: false,
    })
}

fn bind(
    device: &wgpu::Device,
    k: &Kernel,
    res: &[(u32, wgpu::BindingResource<'_>)],
) -> wgpu::BindGroup {
    let entries: Vec<wgpu::BindGroupEntry> = res
        .iter()
        .map(|(binding, r)| wgpu::BindGroupEntry { binding: *binding, resource: r.clone() })
        .collect();
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &k.layout,
        entries: &entries,
    })
}

/// One spectrum's own buffers and bind groups.
struct Spec {
    cfg: InstCfg,
    n: u32,
    norm: f32,
    st: wgpu::Buffer,
    cols: wgpu::BindGroup,
    rows: wgpu::BindGroup,
    acc: wgpu::BindGroup,
    frame: wgpu::BindGroup,
    noise: wgpu::BindGroup,
    pool: wgpu::BindGroup,
    peaks: wgpu::BindGroup,
}

/// The window's coefficients, `1/(Σw)²` and its ENBW in bins: exactly the
/// CPU spectrum's.
pub fn window(w: Window, n: usize) -> (Vec<f32>, f32, f32) {
    let c = w.coefficients(n);
    let gain: f64 = c.iter().map(|&x| x as f64).sum();
    let norm = (1.0 / (gain * gain)) as f32;
    let enbw = sdr::window::enbw_bins(&c);
    (c, norm, enbw)
}

/// The GPU spectrum of one scope.
pub struct Engine {
    k: Kernels,
    params: wgpu::Buffer,
    work: wgpu::Buffer,
    pw: wgpu::Buffer,
    hist: wgpu::Buffer,
    outb: wgpu::Buffer,
    bits: wgpu::Buffer,
    zring: wgpu::Buffer,
    part: wgpu::Buffer,
    iq: wgpu::Buffer,
    iq_len: u32,
    pack: wgpu::BindGroup,
    dsum: wgpu::BindGroup,
    dpart: Option<(u32, wgpu::BindGroup)>,
    main: Option<Spec>,
    zoom: Option<Spec>,
    staging: Vec<Staging>,
    pub last: Option<Readout>,
}

struct Staging {
    buf: wgpu::Buffer,
    /// 0 free, 1 copied and waiting to map, 2 mapped.
    state: Arc<AtomicU8>,
    meta: Option<ReadMeta>,
}

/// One recorded step.
enum Cmd {
    D { k: K, inst: Inst, slot: u32, x: u32, y: u32 },
    Wf { slot: u32 },
    S3 { slot: u32 },
    Read { idx: usize },
}

#[derive(Clone, Copy)]
enum K {
    Cols,
    Rows,
    Acc,
    Frame,
    Noise,
    Pool,
    Pack,
    Peaks,
    DPart,
    DSum,
}

/// A dispatch's settings, in the layout of `P` in spectrum.wgsl.
#[derive(Clone, Copy)]
struct Par([u32; 40]);

impl Default for Par {
    fn default() -> Par {
        Par([0; 40])
    }
}

impl Par {
    const N: usize = 0;
    const N1: usize = 1;
    const N2: usize = 2;
    const LOG1: usize = 3;
    const LOG2: usize = 4;
    const SRC_LEN: usize = 5;
    const START: usize = 6;
    const STEP: usize = 7;
    const SEGS: usize = 8;
    const FLAGS: usize = 9;
    const COUNT: usize = 10;
    const COLS: usize = 11;
    const OFF_A: usize = 12;
    const OFF_B: usize = 13;
    const OFF_C: usize = 14;
    const NQ: usize = 15;
    const NORM: usize = 16;
    const ALPHA: usize = 17;
    const CAL: usize = 18;
    const FALL: usize = 19;
    const A: usize = 20;
    const B: usize = 21;
    const INV_COUNT: usize = 22;
    const PITCH: usize = 23;
    const Q: usize = 24;

    fn u(mut self, i: usize, v: u32) -> Par {
        self.0[i] = v;
        self
    }
    fn f(mut self, i: usize, v: f32) -> Par {
        self.0[i] = v.to_bits();
        self
    }
}

fn groups(n: u32, size: u32) -> u32 {
    n.div_ceil(size).max(1)
}

impl Engine {
    pub fn new(device: &wgpu::Device, iq: &wgpu::Buffer, iq_len: u32) -> Engine {
        use wgpu::BufferUsages as U;
        let k = Kernels::new(device);
        let rw = U::STORAGE | U::COPY_DST | U::COPY_SRC;
        let params =
            buffer(device, "scope-fft-params", SLOT * MAX_SLOTS as u64, U::UNIFORM | U::COPY_DST);
        let work = buffer(device, "scope-fft-work", SCRATCH * 8, rw);
        let pw = buffer(device, "scope-fft-power", SCRATCH * 4, rw);
        let hist = buffer(device, "scope-fft-hist", 2048 * 4, rw);
        let outb = buffer(device, "scope-fft-out", OUT_LEN as u64 * 4, rw | U::COPY_SRC);
        let bits = buffer(device, "scope-fft-bits", MAX_ROW as u64 * 2, rw | U::COPY_SRC);
        let zring = buffer(device, "scope-zoom-ring", ZRING as u64 * 8, rw);
        let part = buffer(device, "scope-zoom-part", PART * 8, rw);
        let uni = wgpu::BindingResource::Buffer(wgpu::BufferBinding {
            buffer: &params,
            offset: 0,
            size: NonZeroU64::new(160),
        });
        let pack = bind(
            device,
            &k.pack,
            &[(0, uni.clone()), (8, outb.as_entire_binding()), (9, bits.as_entire_binding())],
        );
        let dsum = bind(
            device,
            &k.dsum,
            &[(0, uni), (11, zring.as_entire_binding()), (12, part.as_entire_binding())],
        );
        let staging = (0..3)
            .map(|_| Staging {
                buf: buffer(
                    device,
                    "scope-fft-readback",
                    READ_LEN as u64 * 4,
                    U::MAP_READ | U::COPY_DST,
                ),
                state: Arc::new(AtomicU8::new(0)),
                meta: None,
            })
            .collect();
        Engine {
            k,
            params,
            work,
            pw,
            hist,
            outb,
            bits,
            zring,
            part,
            iq: iq.clone(),
            iq_len,
            pack,
            dsum,
            dpart: None,
            main: None,
            zoom: None,
            staging,
            last: None,
        }
    }

    fn uni(&self) -> wgpu::BindingResource<'_> {
        wgpu::BindingResource::Buffer(wgpu::BufferBinding {
            buffer: &self.params,
            offset: 0,
            size: NonZeroU64::new(160),
        })
    }

    /// Build a spectrum's buffers for `cfg`, reading from `src`.
    fn spec(&self, device: &wgpu::Device, queue: &wgpu::Queue, cfg: InstCfg, zoom: bool) -> Spec {
        use wgpu::BufferUsages as U;
        let n = cfg.n;
        let (w, norm, _) = window(cfg.window, n as usize);
        let win = buffer(device, "scope-fft-window", n as u64 * 4, U::STORAGE | U::COPY_DST);
        queue.write_buffer(&win, 0, bytemuck::cast_slice(&w));
        let tw = buffer(device, "scope-fft-twiddles", n as u64 * 8, U::STORAGE | U::COPY_DST);
        queue.write_buffer(&tw, 0, bytemuck::cast_slice(&twiddles(n)));
        let st = buffer(
            device,
            "scope-fft-state",
            n as u64 * 16,
            U::STORAGE | U::COPY_DST | U::COPY_SRC,
        );
        let src = if zoom { &self.zring } else { &self.iq };
        let k = &self.k;
        let u = self.uni();
        let cols = bind(
            device,
            &k.cols,
            &[
                (0, u.clone()),
                (1, src.as_entire_binding()),
                (2, win.as_entire_binding()),
                (3, tw.as_entire_binding()),
                (4, self.work.as_entire_binding()),
            ],
        );
        let rows = bind(
            device,
            &k.rows,
            &[
                (0, u.clone()),
                (3, tw.as_entire_binding()),
                (4, self.work.as_entire_binding()),
                (5, self.pw.as_entire_binding()),
            ],
        );
        let acc = bind(
            device,
            &k.acc,
            &[(0, u.clone()), (5, self.pw.as_entire_binding()), (6, st.as_entire_binding())],
        );
        let frame = bind(
            device,
            &k.frame,
            &[
                (0, u.clone()),
                (5, self.pw.as_entire_binding()),
                (6, st.as_entire_binding()),
                (7, self.hist.as_entire_binding()),
            ],
        );
        let noise = bind(
            device,
            &k.noise,
            &[
                (0, u.clone()),
                (7, self.hist.as_entire_binding()),
                (8, self.outb.as_entire_binding()),
            ],
        );
        let pool = bind(
            device,
            &k.pool,
            &[(0, u.clone()), (6, st.as_entire_binding()), (8, self.outb.as_entire_binding())],
        );
        let peaks = bind(
            device,
            &k.peaks,
            &[(0, u), (6, st.as_entire_binding()), (8, self.outb.as_entire_binding())],
        );
        Spec { cfg, n, norm, st, cols, rows, acc, frame, noise, pool, peaks }
    }

    fn spec_of(&self, inst: Inst) -> Option<&Spec> {
        match inst {
            Inst::Main => self.main.as_ref(),
            Inst::Zoom => self.zoom.as_ref(),
        }
    }

    /// Make sure the buffers match `work`'s settings.
    fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, work: &GpuWork) {
        if self.main.as_ref().map(|s| s.cfg) != Some(work.main.cfg) {
            self.main = Some(self.spec(device, queue, work.main.cfg, false));
        }
        match work.zoom {
            None => self.zoom = None,
            Some((z, factor)) => {
                if self.zoom.as_ref().map(|s| s.cfg) != Some(z.cfg) {
                    self.zoom = Some(self.spec(device, queue, z.cfg, true));
                }
                if self.dpart.as_ref().map(|d| d.0) != Some(factor) {
                    let h = zoom_filter(factor);
                    let taps = buffer(
                        device,
                        "scope-zoom-taps",
                        h.len() as u64 * 4,
                        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    );
                    queue.write_buffer(&taps, 0, bytemuck::cast_slice(&h));
                    let bg = bind(
                        device,
                        &self.k.dpart,
                        &[
                            (0, self.uni()),
                            (1, self.iq.as_entire_binding()),
                            (10, taps.as_entire_binding()),
                            (12, self.part.as_entire_binding()),
                        ],
                    );
                    self.dpart = Some((factor, bg));
                }
            }
        }
    }

    /// Pick up any readback that has arrived; the newest is kept in `last`.
    pub fn collect(&mut self) -> Option<&Readout> {
        let mut newest: Option<Readout> = None;
        for s in &mut self.staging {
            if s.state.load(Ordering::Acquire) != 2 {
                continue;
            }
            let r = match s.buf.slice(..).get_mapped_range() {
                Ok(view) => parse(bytemuck::cast_slice(&view), s.meta.take()),
                Err(_) => Readout::default(),
            };
            s.buf.unmap();
            s.state.store(0, Ordering::Release);
            let seq = r.meta.as_ref().map_or(0, |m| m.seq);
            if newest.as_ref().and_then(|n| n.meta.as_ref()).is_none_or(|m| m.seq < seq) {
                newest = Some(r);
            }
        }
        if let Some(r) = newest
            && self
                .last
                .as_ref()
                .and_then(|l| l.meta.as_ref())
                .is_none_or(|l| r.meta.as_ref().is_some_and(|m| m.seq > l.seq))
        {
            self.last = Some(r);
        }
        self.last.as_ref()
    }

    /// Record `work` into `enc`: the spectra's ops in order, rows into the
    /// targets, and a readback if asked for.
    pub fn record(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        work: &GpuWork,
        t: &Targets<'_>,
    ) {
        self.prepare(device, queue, work);
        let mut pars: Vec<Par> = Vec::new();
        let mut cmds: Vec<Cmd> = Vec::new();
        let mut push = |pars: &mut Vec<Par>, cmds: &mut Vec<Cmd>, k: K, inst, p: Par, x, y| {
            if pars.len() < MAX_SLOTS {
                cmds.push(Cmd::D { k, inst, slot: pars.len() as u32, x, y });
                pars.push(p);
            }
        };
        for op in &work.ops {
            match op {
                Op::Segments { inst, start, step, count } => {
                    let Some(s) = self.spec_of(*inst) else { continue };
                    let per = (SCRATCH / s.n as u64).max(1) as u32;
                    let mut done = 0u32;
                    while done < *count {
                        let b = per.min(count - done);
                        let at = start + done as u64 * *step as u64;
                        self.transform(&mut pars, &mut cmds, *inst, s, at, *step, b, &mut push);
                        let p = Par::default().u(Par::N, s.n).u(Par::SEGS, b);
                        push(&mut pars, &mut cmds, K::Acc, *inst, p, groups(s.n, 256), 1);
                        done += b;
                    }
                }
                Op::Frame { inst, latest, count, primed, peak, peak_reset } => {
                    let Some(s) = self.spec_of(*inst) else { continue };
                    let run = match inst {
                        Inst::Main => work.main,
                        Inst::Zoom => work.zoom.map_or(work.main, |z| z.0),
                    };
                    let mut flags = 0;
                    if let Some(at) = latest {
                        self.transform(&mut pars, &mut cmds, *inst, s, *at, 0, 1, &mut push);
                        flags |= F_LATEST;
                    }
                    if *primed {
                        flags |= F_PRIMED;
                    }
                    if *peak {
                        flags |= F_PEAK;
                    }
                    if *peak_reset {
                        flags |= F_PEAK_RESET;
                    }
                    let p = Par::default()
                        .u(Par::N, s.n)
                        .u(Par::FLAGS, flags)
                        .f(Par::INV_COUNT, 1.0 / (*count).max(1) as f32)
                        .f(Par::ALPHA, run.alpha)
                        .f(Par::FALL, run.fall)
                        .f(Par::CAL, run.cal);
                    push(&mut pars, &mut cmds, K::Frame, *inst, p, groups(s.n, 256), 1);
                    let p = Par::default()
                        .u(Par::N, s.n)
                        .u(Par::OFF_A, if *inst == Inst::Main { 0 } else { 1 })
                        .f(Par::CAL, run.cal);
                    push(&mut pars, &mut cmds, K::Noise, *inst, p, 1, 1);
                }
                Op::Pool { inst, peak, a, b, cols, dst, max } => {
                    let Some(s) = self.spec_of(*inst) else { continue };
                    let n = s.n as f64;
                    let first = a.clamp(0.0, n);
                    let last = b.clamp(first, n);
                    let per = (last - first) / (*cols).max(1) as f64;
                    let ai = first.floor();
                    let off_b = match dst {
                        Dst::Pend => PEND_OFF,
                        Dst::Surface => S3_OFF,
                        Dst::Trace => TRACE_OFF,
                        Dst::PeakTrace => PEAK_OFF,
                    };
                    let cap = if matches!(dst, Dst::Trace | Dst::PeakTrace) {
                        MAX_TRACE
                    } else {
                        MAX_ROW
                    };
                    let cols = (*cols).min(cap);
                    let p = Par::default()
                        .u(Par::N, s.n)
                        .u(Par::COLS, cols)
                        .u(Par::OFF_A, if *peak { 2 * s.n } else { 3 * s.n })
                        .u(Par::OFF_B, off_b)
                        .u(Par::OFF_C, ai as u32)
                        .u(Par::FLAGS, if *max { F_MAX } else { 0 })
                        .f(Par::A, (first - ai) as f32)
                        .f(Par::B, per as f32);
                    push(&mut pars, &mut cmds, K::Pool, *inst, p, groups(cols, 64), 1);
                }
                Op::WfRow { slot } => {
                    let p = Par::default().u(Par::COLS, t.wf_w).u(Par::OFF_B, PEND_OFF);
                    push(&mut pars, &mut cmds, K::Pack, Inst::Main, p, groups(t.wf_w / 2, 64), 1);
                    cmds.push(Cmd::Wf { slot: *slot });
                }
                Op::SurfaceRow { slots } => {
                    for &slot in slots {
                        cmds.push(Cmd::S3 { slot });
                    }
                }
                Op::Decim(d) => self.decim(&mut pars, &mut cmds, d, &mut push),
                Op::Peaks { inst, queries } => {
                    let Some(s) = self.spec_of(*inst) else { continue };
                    let nq = queries.len().min(MAX_QUERIES);
                    if nq == 0 {
                        continue;
                    }
                    let mut p = Par::default()
                        .u(Par::N, s.n)
                        .u(Par::NQ, nq as u32)
                        .u(Par::OFF_A, 3 * s.n)
                        .u(Par::OFF_B, RES_PEAKS);
                    for (i, &(lo, hi)) in queries.iter().take(nq).enumerate() {
                        p = p.u(Par::Q + 2 * i, lo).u(Par::Q + 2 * i + 1, hi.max(lo));
                    }
                    push(&mut pars, &mut cmds, K::Peaks, *inst, p, nq as u32, 1);
                }
            }
        }
        if let Some(meta) = &work.readback
            && let Some(idx) =
                self.staging.iter().position(|s| s.state.load(Ordering::Acquire) == 0)
        {
            self.staging[idx].meta = Some(meta.clone());
            cmds.push(Cmd::Read { idx });
        }

        // One write of every slot, then the passes.
        let mut bytes = vec![0u8; pars.len() * SLOT as usize];
        for (i, p) in pars.iter().enumerate() {
            let at = i * SLOT as usize;
            bytes[at..at + 160].copy_from_slice(bytemuck::cast_slice(&p.0));
        }
        if !bytes.is_empty() {
            queue.write_buffer(&self.params, 0, &bytes);
        }
        let mut i = 0;
        while i < cmds.len() {
            if let Cmd::D { .. } = cmds[i] {
                let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("scope-spectrum"),
                    timestamp_writes: None,
                });
                while let Some(Cmd::D { k, inst, slot, x, y }) = cmds.get(i) {
                    let (kern, bg) = self.binding(*k, *inst);
                    if let Some(bg) = bg {
                        pass.set_pipeline(&kern.pipeline);
                        pass.set_bind_group(0, bg, &[(*slot as u64 * SLOT) as u32]);
                        pass.dispatch_workgroups(*x, *y, 1);
                    }
                    i += 1;
                }
                continue;
            }
            match cmds[i] {
                Cmd::Wf { slot } => copy_row(enc, &self.bits, 0, t.wf, slot, t.wf_w, 2),
                Cmd::S3 { slot } => {
                    copy_row(enc, &self.outb, S3_OFF as u64 * 4, t.s3, slot, t.s3_w, 4)
                }
                Cmd::Read { idx } => {
                    let s = &self.staging[idx];
                    enc.copy_buffer_to_buffer(&self.outb, 0, &s.buf, 0, READ_LEN as u64 * 4);
                    s.state.store(1, Ordering::Release);
                    let state = s.state.clone();
                    enc.map_buffer_on_submit(&s.buf, wgpu::MapMode::Read, .., move |r| {
                        state.store(if r.is_ok() { 2 } else { 0 }, Ordering::Release);
                    });
                }
                Cmd::D { .. } => unreachable!(),
            }
            i += 1;
        }
    }

    fn binding(&self, k: K, inst: Inst) -> (&Kernel, Option<&wgpu::BindGroup>) {
        let s = self.spec_of(inst);
        match k {
            K::Cols => (&self.k.cols, s.map(|s| &s.cols)),
            K::Rows => (&self.k.rows, s.map(|s| &s.rows)),
            K::Acc => (&self.k.acc, s.map(|s| &s.acc)),
            K::Frame => (&self.k.frame, s.map(|s| &s.frame)),
            K::Noise => (&self.k.noise, s.map(|s| &s.noise)),
            K::Pool => (&self.k.pool, s.map(|s| &s.pool)),
            K::Peaks => (&self.k.peaks, s.map(|s| &s.peaks)),
            K::Pack => (&self.k.pack, Some(&self.pack)),
            K::DPart => (&self.k.dpart, self.dpart.as_ref().map(|d| &d.1)),
            K::DSum => (&self.k.dsum, Some(&self.dsum)),
        }
    }

    /// The two FFT passes over `segs` segments from stream position `at`,
    /// `step` apart, into `pw`.
    #[allow(clippy::too_many_arguments)]
    fn transform(
        &self,
        pars: &mut Vec<Par>,
        cmds: &mut Vec<Cmd>,
        inst: Inst,
        s: &Spec,
        at: u64,
        step: u32,
        segs: u32,
        push: &mut impl FnMut(&mut Vec<Par>, &mut Vec<Cmd>, K, Inst, Par, u32, u32),
    ) {
        let (n1, n2, l1, l2) = split(s.n);
        let len = if inst == Inst::Main { self.iq_len } else { ZRING };
        let p = Par::default()
            .u(Par::N, s.n)
            .u(Par::N1, n1)
            .u(Par::N2, n2)
            .u(Par::LOG1, l1)
            .u(Par::LOG2, l2)
            .u(Par::SRC_LEN, len)
            .u(Par::START, (at % len as u64) as u32)
            .u(Par::STEP, step)
            .u(Par::SEGS, segs)
            .f(Par::NORM, s.norm);
        push(pars, cmds, K::Cols, inst, p, n2, segs);
        push(pars, cmds, K::Rows, inst, p, n1, segs);
    }

    fn decim(
        &self,
        pars: &mut Vec<Par>,
        cmds: &mut Vec<Cmd>,
        d: &Decim,
        push: &mut impl FnMut(&mut Vec<Par>, &mut Vec<Cmd>, K, Inst, Par, u32, u32),
    ) {
        let l = zoom_taps(d.factor);
        let c = l.div_ceil(CHUNK);
        let per = ((PART / c as u64).max(1) as u32).min(1 << 20);
        let mut j0 = 0u32;
        while j0 < d.outs {
            let outs = per.min(d.outs - j0);
            let threads = outs * c;
            let wg = groups(threads, 64);
            let x = wg.min(65_535);
            let y = wg.div_ceil(x);
            let q = d.q_first + j0 as u64 * d.factor as u64;
            let p = Par::default()
                .u(Par::N, l)
                .u(Par::SRC_LEN, self.iq_len)
                .u(Par::START, (q % self.iq_len as u64) as u32)
                .u(Par::STEP, d.factor)
                .u(Par::COUNT, outs)
                .u(Par::COLS, c)
                .u(Par::OFF_A, CHUNK)
                .u(Par::Q, d.tap_step)
                .f(Par::PITCH, (x * 64) as f32);
            push(pars, cmds, K::DPart, Inst::Zoom, p, x, y);
            let p = Par::default()
                .u(Par::SRC_LEN, ZRING)
                .u(Par::COUNT, outs)
                .u(Par::COLS, c)
                .u(Par::OFF_C, ((d.m_first + j0 as u64) % ZRING as u64) as u32)
                .u(Par::Q + 1, d.phase0.wrapping_add(j0.wrapping_mul(d.phase_step)))
                .u(Par::Q + 2, d.phase_step);
            push(pars, cmds, K::DSum, Inst::Zoom, p, groups(outs, 64), 1);
            j0 += outs;
        }
    }
}

/// Copy one row of `w` texels (`bpt` bytes each) from `buf` at `off` into
/// row `slot` of `tex`.
fn copy_row(
    enc: &mut wgpu::CommandEncoder,
    buf: &wgpu::Buffer,
    off: u64,
    tex: &wgpu::Texture,
    slot: u32,
    w: u32,
    bpt: u32,
) {
    enc.copy_buffer_to_texture(
        wgpu::TexelCopyBufferInfo {
            buffer: buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: off,
                bytes_per_row: Some(w * bpt),
                rows_per_image: None,
            },
        },
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d { x: 0, y: slot, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::Extent3d { width: w, height: 1, depth_or_array_layers: 1 },
    );
}

fn parse(v: &[f32], meta: Option<ReadMeta>) -> Readout {
    let (cols, peak, nq) = meta
        .as_ref()
        .map_or((0, false, 0), |m| (m.cols.min(MAX_TRACE) as usize, m.peak, m.queries.len()));
    let t0 = TRACE_OFF as usize;
    let p0 = PEAK_OFF as usize;
    let peaks = (0..nq.min(MAX_QUERIES))
        .map(|q| {
            let o = RES_PEAKS as usize + 4 * q;
            [v[o], v[o + 1], v[o + 2], v[o + 3]]
        })
        .collect();
    Readout {
        noise: [v[0], v[1]],
        peaks,
        trace: v[t0..t0 + cols].to_vec(),
        peak_trace: if peak { v[p0..p0 + cols].to_vec() } else { Vec::new() },
        meta,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::scope::gpu_spec::{Sched, ZoomSched};
    use rustfft::FftPlanner;
    use rustfft::num_complex::Complex32;

    /// A headless device (lavapipe in CI), or `None` with a note.
    pub(crate) fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN | wgpu::Backends::METAL | wgpu::Backends::DX12,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            ..Default::default()
        }));
        let Ok(adapter) = adapter else {
            eprintln!("no wgpu adapter: the GPU tests only check the shaders");
            return None;
        };
        let limits = wgpu::Limits {
            max_storage_buffer_binding_size: adapter.limits().max_storage_buffer_binding_size,
            max_buffer_size: adapter.limits().max_buffer_size,
            ..wgpu::Limits::default()
        };
        let r = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("test"),
            required_limits: limits,
            ..Default::default()
        }));
        r.ok()
    }

    fn read(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        src: &wgpu::Buffer,
        off: u64,
        len: u64,
    ) -> Vec<f32> {
        let rb =
            buffer(device, "rb", len, wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_buffer_to_buffer(src, off, &rb, 0, len);
        queue.submit([enc.finish()]);
        rb.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let view = rb.slice(..).get_mapped_range().unwrap();
        bytemuck::cast_slice(&view).to_vec()
    }

    struct Rig {
        device: wgpu::Device,
        queue: wgpu::Queue,
        iq: wgpu::Buffer,
        engine: Engine,
        wf: wgpu::Texture,
        s3: wgpu::Texture,
    }

    const LEN: u32 = 1 << 22;

    fn rig() -> Option<Rig> {
        let (device, queue) = device()?;
        let iq = buffer(
            &device,
            "iq",
            LEN as u64 * 8,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let engine = Engine::new(&device, &iq, LEN);
        let tex = |f| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d { width: 1024, height: 4, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: f,
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let wf = tex(wgpu::TextureFormat::R16Float);
        let s3 = tex(wgpu::TextureFormat::R32Float);
        Some(Rig { device, queue, iq, engine, wf, s3 })
    }

    impl Rig {
        fn run(&mut self, work: &GpuWork) {
            let mut enc = self.device.create_command_encoder(&Default::default());
            let t = Targets { wf: &self.wf, wf_w: 1024, s3: &self.s3, s3_w: 1024 };
            self.engine.record(&self.device, &self.queue, &mut enc, work, &t);
            self.queue.submit([enc.finish()]);
            self.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        }

        /// The state buffer's dB frame (or another of its four parts).
        fn state(&self, inst: Inst, part: u64) -> Vec<f32> {
            let s = self.engine.spec_of(inst).unwrap();
            read(&self.device, &self.queue, &s.st, part * s.n as u64 * 4, s.n as u64 * 4)
        }
    }

    fn carriers(rate: f64, sigs: &[(f64, f32)], n: usize, noise: f32, seed: u64) -> Vec<f32> {
        let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut rnd = || {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((x >> 33) as f32 / (1u64 << 31) as f32) - 0.5
        };
        let mut v = Vec::with_capacity(2 * n);
        for i in 0..n {
            let (mut re, mut im) = (noise * rnd(), noise * rnd());
            for &(hz, a) in sigs {
                let ph = std::f64::consts::TAU * hz * i as f64 / rate;
                re += a * ph.cos() as f32;
                im += a * ph.sin() as f32;
            }
            v.push(re);
            v.push(im);
        }
        v
    }

    #[test]
    fn the_spectrum_shader_validates_and_its_settings_fit_a_slot() {
        let module = naga::front::wgsl::parse_str(SPECTRUM_WGSL)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(SPECTRUM_WGSL)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e:?}"));
        let mut layouter = naga::proc::Layouter::default();
        layouter.update(module.to_ctx()).unwrap();
        let (_, u) = module
            .global_variables
            .iter()
            .find(|(_, g)| g.space == naga::AddressSpace::Uniform)
            .unwrap();
        assert_eq!(layouter[u.ty].size, 160);
        assert_eq!(std::mem::size_of::<Par>(), 160);
        assert_eq!(S3_OFF % 64, 0, "the 3D row is copied from a 256-byte boundary");
    }

    fn cfg(n: u32, window: Window) -> InstRun {
        InstRun { cfg: InstCfg { n, window, generation: 1 }, alpha: 1.0, fall: 0.0, cal: 0.0 }
    }

    #[test]
    fn the_gpu_fft_matches_rustfft_from_4k_to_1m() {
        let Some(mut r) = rig() else { return };
        for (n, win) in [
            (4096u32, Window::Rectangular),
            (8192, Window::Hann),
            (65_536, Window::BlackmanHarris),
            (1 << 20, Window::FlatTop),
        ] {
            let x =
                carriers(1e6, &[(123_456.7, 0.3), (-250_000.0, 0.01)], n as usize, 0.02, n as u64);
            let start = 12_345u64;
            r.queue.write_buffer(&r.iq, start * 8, bytemuck::cast_slice(&x));
            let work = GpuWork {
                main: cfg(n, win),
                zoom: None,
                ops: vec![Op::Frame {
                    inst: Inst::Main,
                    latest: Some(start),
                    count: 0,
                    primed: false,
                    peak: false,
                    peak_reset: false,
                }],
                readback: None,
            };
            r.run(&work);
            let gpu_pw = read(&r.device, &r.queue, &r.engine.pw, 0, n as u64 * 4);
            // rustfft on the same windowed samples.
            let (w, norm, _) = window(win, n as usize);
            let mut buf: Vec<Complex32> = x
                .chunks_exact(2)
                .zip(&w)
                .map(|(p, &c)| Complex32::new(p[0] * c, p[1] * c))
                .collect();
            FftPlanner::new().plan_fft_forward(n as usize).process(&mut buf);
            let half = n as usize / 2;
            let want: Vec<f32> =
                (0..n as usize).map(|d| buf[(d + half) % n as usize].norm_sqr() * norm).collect();
            let peak = want.iter().copied().fold(0.0f32, f32::max);
            let mut worst = 0.0f32;
            for (g, w) in gpu_pw.iter().zip(&want) {
                worst = worst.max((g.sqrt() - w.sqrt()).abs());
            }
            // Magnitude error against the strongest bin's magnitude.
            let rel = worst / peak.sqrt();
            assert!(rel < 2e-5, "N = {n}: worst magnitude error {rel:e} of the peak");
            // And the dB frame is the same as the CPU's arithmetic.
            let db = r.state(Inst::Main, 3);
            for d in (0..n as usize).step_by(97) {
                let cpu = 10.0 * (want[d] + 1e-20).log10();
                if cpu > -120.0 {
                    assert!((db[d] - cpu).abs() < 0.01, "N = {n} bin {d}: {} vs {cpu}", db[d]);
                }
            }
        }
    }

    /// The whole GPU spectrum (Welch, averaging, peak hold, noise floor,
    /// pooling, the readback) against sdr::Spectrum on the same samples.
    #[test]
    fn the_gpu_spectrum_matches_the_cpu_spectrum() {
        let Some(mut r) = rig() else { return };
        let rate = 1_536_000.0;
        let n = 16_384u32;
        let total = 600_000usize;
        let x = carriers(rate, &[(200_010.0, 0.05), (-300_000.0, 0.001)], total, 0.01, 7);
        r.queue.write_buffer(&r.iq, 0, bytemuck::cast_slice(&x));
        let ov = sdr::Overlap::ThreeQuarters;
        let scfg = sdr::SpectrumConfig {
            fft_size: n as usize,
            window: Window::BlackmanHarris,
            overlap: ov,
            averaging: sdr::Averaging::X4,
            peak_hold: true,
            peak_decay_db_per_s: 10.0,
            fps: 30.0,
            calibration_db: -7.0,
        };
        let mut cpu = sdr::Spectrum::new(scfg, rate);
        let mut frames = Vec::new();
        cpu.process(&x, 0.0, &mut frames);
        let mut s = Sched::new(n as usize, rate, 30.0, ov, 0);
        let mut ops = Vec::new();
        let made = s.advance(total as u64, Inst::Main, true, 1000, &mut ops);
        assert_eq!(made.len(), frames.len(), "the same frames");
        let fall = 10.0 * (s.hop() / rate) as f32;
        let run = InstRun {
            cfg: InstCfg { n, window: Window::BlackmanHarris, generation: 1 },
            alpha: 0.25,
            fall,
            cal: -7.0,
        };
        let f = frames.last().unwrap();
        let k = f.bin_of(200_010.0).unwrap() as u32;
        ops.push(Op::Pool {
            inst: Inst::Main,
            peak: false,
            a: 0.0,
            b: n as f64,
            cols: 512,
            dst: Dst::Trace,
            max: false,
        });
        ops.push(Op::Peaks { inst: Inst::Main, queries: vec![(k - 20, k + 20)] });
        let meta = ReadMeta {
            seq: 1,
            inst: Inst::Main,
            center_hz: 0.0,
            span_hz: rate,
            n,
            enbw: 2.0,
            lo_hz: -rate / 2.0,
            hi_hz: rate / 2.0,
            cols: 512,
            peak: false,
            queries: vec![(0.0, 1.0)],
        };
        r.run(&GpuWork { main: run, zoom: None, ops, readback: Some(meta) });
        let db = r.state(Inst::Main, 3);
        let pk = r.state(Inst::Main, 2);
        let mut worst = 0.0f32;
        for d in 0..n as usize {
            if f.db[d] > -140.0 {
                worst = worst.max((db[d] - f.db[d]).abs());
            }
        }
        assert!(worst < 0.02, "dB frames differ by up to {worst} dB");
        let cpk = f.peak.as_ref().unwrap();
        assert!((pk[k as usize] - cpk[k as usize]).abs() < 0.02);
        // The readback arrives without anyone waiting for it.
        let got = r.engine.collect().cloned().expect("a readout");
        assert!((got.noise[0] - f.noise_dbm).abs() < 0.2, "{} vs {}", got.noise[0], f.noise_dbm);
        let cols = sdr::pool_max(&f.db, 512);
        for (g, c) in got.trace.iter().zip(&cols) {
            assert!((g - c).abs() < 0.02, "{g} vs {c}");
        }
        let [bin, a, b, c] = got.peaks[0];
        assert_eq!(bin as u32, k);
        assert!((b - f.db[k as usize]).abs() < 0.02 && a < b && c < b);
    }

    #[test]
    fn the_gpu_zoom_resolves_what_the_main_spectrum_cannot() {
        let Some(mut r) = rig() else { return };
        // As sdr's zoom test: two carriers 30 Hz apart at 1536 kHz.
        let rate = 1_536_000.0;
        let total = 1_536_000usize;
        let (f1, f2) = (200_010.0, 200_040.0);
        let x = carriers(rate, &[(f1, 0.01), (f2, 0.01)], total, 0.0, 1);
        r.queue.write_buffer(&r.iq, 0, bytemuck::cast_slice(&x));
        let factor = 128;
        let n = 8192u32;
        let mut z =
            ZoomSched::new(factor, 200_000.0, rate, n as usize, 4.0, sdr::Overlap::Latest, 0);
        let mut ops = Vec::new();
        let made = z.advance(total as u64, 1 << 22, 1 << 22, false, 100, &mut ops);
        assert!(!made.is_empty());
        let zr = cfg(n, Window::BlackmanHarris);
        r.run(&GpuWork {
            main: cfg(4096, Window::Hann),
            zoom: Some((zr, factor)),
            ops,
            readback: None,
        });
        let db = r.state(Inst::Zoom, 3);
        let out_rate = rate / factor as f64;
        let frame = sdr::SpectrumFrame {
            center_hz: 200_000.0,
            span_hz: out_rate,
            db,
            peak: None,
            seq: 1,
            enbw: 2.0,
            noise_dbm: 0.0,
        };
        let p1 = frame.peak_in(f1 - 10.0, f1 + 10.0).unwrap();
        let p2 = frame.peak_in(f2 - 10.0, f2 + 10.0).unwrap();
        assert!((p1.hz - f1).abs() < 0.2, "{} vs {f1}", p1.hz);
        assert!((p2.hz - f2).abs() < 0.2, "{} vs {f2}", p2.hz);
        assert!((p1.dbm - -40.0).abs() < 1.0, "{}", p1.dbm);
        let mid = frame.peak_in(f1 + 12.0, f2 - 12.0).unwrap();
        assert!(mid.dbm < -80.0, "a deep gap between them: {}", mid.dbm);
    }
}
