//! The SSCA on the GPU: buffers, pipelines and one block's dispatches
//! (shaders/ssca.wgsl). Rate-generic: everything here is in cycles per
//! sample; [`super`] turns that into Hz for the lane it runs on.
//!
//! One block is N + N' complex samples (the last N' of a block are the first
//! N' of the next). [`Engine::process`] uploads it with one `write_buffer` of
//! the interleaved `f32` the ingestion thread assembled (no per-sample
//! copy), records the passes, submits, and returns the cyclic-domain profile
//! of the block before: two input buffers and two read-back buffers, so the
//! CPU fills block i + 1 while the GPU runs block i.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytemuck::{Pod, Zeroable};
use eframe::egui_wgpu::wgpu;

/// The largest FFT one workgroup does in its own memory (16 KB of `vec2<f32>`,
/// WebGPU's default workgroup storage limit).
pub const SHARED_FFT: u32 = 2048;
/// N' × N is kept at or under this: the strip buffers are 8 bytes a point,
/// two of them, plus 4 for the average (80 MB at the cap).
pub const MAX_POINTS: u32 = 1 << 22;

const SSCA: &str = include_str!("shaders/ssca.wgsl");

/// The SCF itself, or the spectral coherence (normalised by the powers at
/// both frequencies: what detection wants, as it is flat for any noise
/// spectrum).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Measure {
    Scf,
    Coherence,
}

/// The data taper on the channelizer and on the strips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Taper {
    Hamming,
    /// Kaiser, β = 6: lower sidelobes (−44 dB) than Hamming's −43 with a
    /// faster fall-off, for a strong carrier beside a weak feature.
    Kaiser,
}

impl Taper {
    pub fn coefficients(self, n: usize) -> Vec<f64> {
        let m = (n.max(2) - 1) as f64;
        (0..n)
            .map(|i| {
                let x = i as f64 / m;
                match self {
                    Taper::Hamming => 0.54 - 0.46 * (std::f64::consts::TAU * x).cos(),
                    Taper::Kaiser => {
                        let beta = 6.0;
                        let t = 2.0 * x - 1.0;
                        bessel_i0(beta * (1.0 - t * t).max(0.0).sqrt()) / bessel_i0(beta)
                    }
                }
            })
            .collect()
    }
}

/// The modified Bessel function I₀, by its series (converges fast for the
/// β used here).
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..64 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// What the engine is built for. Changing `n`, `nc`, `grid_f`, `grid_a` or
/// `taper` needs a new engine; the rest goes in [`BlockParams`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EngineCfg {
    /// Strip FFT length N (a power of two): Δα = fs/N, and a block is N
    /// samples.
    pub n: u32,
    /// Channelizer length N' (64, 128 or 256): Δf = fs/N'.
    pub nc: u32,
    pub grid_f: u32,
    pub grid_a: u32,
    pub taper: Taper,
}

impl EngineCfg {
    /// Whether the sizes are ones the shaders handle.
    pub fn valid(&self) -> bool {
        self.n.is_power_of_two()
            && self.nc.is_power_of_two()
            && (16..=SHARED_FFT).contains(&self.nc)
            && self.n >= 4 * self.nc
            && self.n as u64 * self.nc as u64 <= MAX_POINTS as u64
            && self.grid_f >= 2
            && self.grid_a >= 2
    }

    fn four_step(&self) -> Option<(u32, u32)> {
        if self.n <= SHARED_FFT {
            return None;
        }
        let log = self.n.trailing_zeros();
        let n1 = 1u32 << log.div_ceil(2);
        Some((n1, self.n / n1))
    }
}

/// What may change from one block to the next.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockParams {
    pub conj: bool,
    pub measure: Measure,
    /// This block's weight in the running average (1 starts it again).
    pub beta: f32,
    /// The grid's α rows and f columns, cycles per sample.
    pub a_range: (f32, f32),
    pub f_range: (f32, f32),
}

/// One α row of the cyclic-domain profile: the strongest cell (dB), its
/// column, and the row's mean and mean square (dB, dB²).
pub type ProfileRow = [f32; 4];

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct Params {
    n: u32,
    nc: u32,
    log_nc: u32,
    hop: u32,
    conj: u32,
    measure: u32,
    grid_f: u32,
    grid_a: u32,
    beta: f32,
    a_lo: f32,
    a_hi: f32,
    f_lo: f32,
    f_hi: f32,
    norm_scf: f32,
    norm_psd: f32,
    ntw: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable, Default)]
struct PassCfg {
    log_m: u32,
    m: u32,
    per_wg: u32,
    batches: u32,
    per_ch: u32,
    in_j: u32,
    in_i: u32,
    out_j: u32,
    out_i: u32,
    twiddle: u32,
    product: u32,
    groups_x: u32,
    ch_stride: u32,
    count: u32,
    _pad0: u32,
    _pad1: u32,
}

/// Uniform block sizes, bytes (checked against the WGSL in the tests).
pub const PARAMS_BYTES: usize = std::mem::size_of::<Params>();
pub const PASS_BYTES: usize = std::mem::size_of::<PassCfg>();

/// A dispatch of `groups` workgroups folded into x × y under the 65 535 a
/// dimension allows: (x, y).
fn fold(groups: u32) -> (u32, u32) {
    let x = groups.clamp(1, 32_768);
    (x, groups.div_ceil(x).max(1))
}

/// A batched FFT pass: `batches` FFTs of `m` points, as many to a workgroup
/// as fill its memory.
fn fft_pass(m: u32, batches: u32) -> PassCfg {
    let per_wg = (SHARED_FFT / m).max(1);
    let (gx, _) = fold(batches.div_ceil(per_wg));
    PassCfg {
        log_m: m.trailing_zeros(),
        m,
        per_wg,
        batches,
        per_ch: 1,
        groups_x: gx,
        ..Default::default()
    }
}

fn groups_of(c: &PassCfg) -> (u32, u32) {
    let groups = if c.m > 0 { c.batches.div_ceil(c.per_wg) } else { c.count.div_ceil(256) };
    fold(groups)
}

struct Pass {
    pipeline: wgpu::ComputePipeline,
    /// One bind group per input slot (most passes use the same one twice).
    bind: [wgpu::BindGroup; 2],
    groups: (u32, u32),
}

/// The engine: everything one configuration needs on the GPU.
pub struct Engine {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pub cfg: EngineCfg,
    params_buf: wgpu::Buffer,
    xin: [wgpu::Buffer; 2],
    acc: wgpu::Buffer,
    psd: wgpu::Buffer,
    /// The (f, α) grid, dB, row-major by α: `grid_a` rows of `grid_f`. It
    /// lives as long as the engine; the display binds it directly.
    pub grid: wgpu::Buffer,
    prof: wgpu::Buffer,
    readback: [wgpu::Buffer; 2],
    mapped: [Arc<AtomicBool>; 2],
    pending: [Option<wgpu::SubmissionIndex>; 2],
    passes: Vec<Pass>,
    /// Where |S|² ends up (X for the four-step FFT, T for the one-pass).
    s_in_x: bool,
    sum_a: f64,
    sum_g: f64,
    slot: usize,
    pub blocks: u64,
}

/// Whether this device can run the engine and show its grid: compute
/// shaders, and storage buffers readable in a vertex shader.
pub fn supported(adapter: &wgpu::Adapter, device: &wgpu::Device) -> bool {
    let caps = adapter.get_downlevel_capabilities();
    let l = device.limits();
    caps.flags.contains(wgpu::DownlevelFlags::COMPUTE_SHADERS)
        && caps.flags.contains(wgpu::DownlevelFlags::VERTEX_STORAGE)
        && l.max_storage_buffers_per_shader_stage >= 5
        && l.max_compute_workgroup_storage_size >= SHARED_FFT * 8
        && l.max_compute_invocations_per_workgroup >= 256
        && l.max_storage_buffer_binding_size >= MAX_POINTS as u64 * 8
}

impl Engine {
    /// Build for `cfg` (which must be [`EngineCfg::valid`]).
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, cfg: EngineCfg) -> Engine {
        assert!(cfg.valid(), "SCF sizes {cfg:?}");
        let d = device;
        let n = cfg.n;
        let nc = cfg.nc;
        let points = (n * nc) as u64;
        let storage = |label: &str, bytes: u64, extra: wgpu::BufferUsages| {
            d.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes.max(16),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | extra,
                mapped_at_creation: false,
            })
        };
        let none = wgpu::BufferUsages::empty();
        let src = wgpu::BufferUsages::COPY_SRC;
        let xin = [
            storage("scf-xin-0", (n + nc) as u64 * 8, none),
            storage("scf-xin-1", (n + nc) as u64 * 8, none),
        ];
        let win_a = storage("scf-win-a", nc as u64 * 4, none);
        let win_g = storage("scf-win-g", n as u64 * 4, none);
        let tw = storage("scf-twiddles", n as u64 * 8, none);
        let x = storage("scf-x", points * 8, src);
        let t = storage("scf-t", points * 8, src);
        let acc = storage("scf-acc", points * 4, src);
        let psd = storage("scf-psd", nc as u64 * 4, src);
        let grid = storage("scf-grid", (cfg.grid_f * cfg.grid_a) as u64 * 4, src);
        let prof = storage("scf-profile", cfg.grid_a as u64 * 16, src);
        let readback = [0, 1].map(|i| {
            d.create_buffer(&wgpu::BufferDescriptor {
                label: Some(if i == 0 { "scf-readback-0" } else { "scf-readback-1" }),
                size: cfg.grid_a as u64 * 16,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });

        // Windows and twiddles, computed in f64 and uploaded once.
        let a = cfg.taper.coefficients(nc as usize);
        let g = cfg.taper.coefficients(n as usize);
        let a32: Vec<f32> = a.iter().map(|&v| v as f32).collect();
        let g32: Vec<f32> = g.iter().map(|&v| v as f32).collect();
        let tw32: Vec<f32> = (0..n as usize)
            .flat_map(|j| {
                let ph = -std::f64::consts::TAU * j as f64 / n as f64;
                [ph.cos() as f32, ph.sin() as f32]
            })
            .collect();
        queue.write_buffer(&win_a, 0, bytemuck::cast_slice(&a32));
        queue.write_buffer(&win_g, 0, bytemuck::cast_slice(&g32));
        queue.write_buffer(&tw, 0, bytemuck::cast_slice(&tw32));

        let uniform = |label: &str, bytes: usize| {
            d.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let params_buf = uniform("scf-params", PARAMS_BYTES);

        let module = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scf-ssca"),
            source: wgpu::ShaderSource::Wgsl(SSCA.into()),
        });
        let pipeline = |entry: &str| {
            d.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: None,
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };

        // The pass shapes.
        let mut chan = fft_pass(nc, n);
        chan.ch_stride = n;
        // One workgroup a channel.
        let psd_cfg = PassCfg {
            m: 1,
            per_wg: 1,
            batches: nc,
            count: nc,
            groups_x: fold(nc).0,
            ..Default::default()
        };
        let mut strips: Vec<PassCfg> = Vec::new();
        let four = cfg.four_step();
        match four {
            None => {
                let mut c = fft_pass(n, nc);
                c.in_i = 1;
                c.out_i = 1;
                c.product = 1;
                c.ch_stride = n;
                strips.push(c);
            }
            Some((n1, n2)) => {
                let mut a = fft_pass(n1, nc * n2);
                a.per_ch = n2;
                a.in_j = 1;
                a.in_i = n2;
                a.out_j = 1;
                a.out_i = n2;
                a.twiddle = 1;
                a.product = 1;
                a.ch_stride = n;
                let mut b = fft_pass(n2, nc * n1);
                b.per_ch = n1;
                b.in_j = n2;
                b.in_i = 1;
                b.out_j = 1;
                b.out_i = n1;
                b.ch_stride = n;
                strips.push(a);
                strips.push(b);
            }
        }
        let count_pass = |count: u32| {
            let mut c = PassCfg { count, ..Default::default() };
            c.groups_x = fold(count.div_ceil(256)).0;
            c
        };
        let accum = count_pass(n * nc);
        let map = count_pass(cfg.grid_f * cfg.grid_a);
        let profile = count_pass(cfg.grid_a);

        let mut passes = Vec::new();
        let mut add = |entry: &str, c: PassCfg, binds: &[(u32, [&wgpu::Buffer; 2])]| {
            let pipeline = pipeline(entry);
            let layout = pipeline.get_bind_group_layout(0);
            let cbuf = uniform(entry, PASS_BYTES);
            queue.write_buffer(&cbuf, 0, bytemuck::bytes_of(&c));
            let bind = [0, 1].map(|slot| {
                let mut entries = vec![
                    wgpu::BindGroupEntry { binding: 0, resource: params_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: cbuf.as_entire_binding() },
                ];
                for (binding, bufs) in binds {
                    entries.push(wgpu::BindGroupEntry {
                        binding: *binding,
                        resource: bufs[slot].as_entire_binding(),
                    });
                }
                d.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(entry),
                    layout: &layout,
                    entries: &entries,
                })
            });
            passes.push(Pass { pipeline, bind, groups: groups_of(&c) });
        };
        fn both(b: &wgpu::Buffer) -> [&wgpu::Buffer; 2] {
            [b, b]
        }
        let xs = [&xin[0], &xin[1]];
        add("chan_main", chan, &[(2, xs), (3, both(&win_a)), (5, both(&tw)), (7, both(&x))]);
        add("psd_main", psd_cfg, &[(6, both(&x)), (9, both(&psd))]);
        let s_in_x = four.is_some();
        type Binds<'a> = [(u32, [&'a wgpu::Buffer; 2]); 5];
        fn strip<'a>(
            xs: [&'a wgpu::Buffer; 2],
            g: &'a wgpu::Buffer,
            tw: &'a wgpu::Buffer,
            from: &'a wgpu::Buffer,
            to: &'a wgpu::Buffer,
        ) -> Binds<'a> {
            [(2, xs), (4, both(g)), (5, both(tw)), (6, both(from)), (7, both(to))]
        }
        add("strip_main", strips[0], &strip(xs, &win_g, &tw, &x, &t));
        if s_in_x {
            add("strip_main", strips[1], &strip(xs, &win_g, &tw, &t, &x));
        }
        let s_buf = if s_in_x { &x } else { &t };
        add("accum_main", accum, &[(6, both(s_buf)), (8, both(&acc))]);
        add("map_main", map, &[(8, both(&acc)), (9, both(&psd)), (10, both(&grid))]);
        add("profile_main", profile, &[(10, both(&grid)), (11, both(&prof))]);

        Engine {
            device: device.clone(),
            queue: queue.clone(),
            cfg,
            params_buf,
            xin,
            acc,
            psd,
            grid,
            prof,
            readback,
            mapped: [Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false))],
            pending: [None, None],
            passes,
            s_in_x,
            sum_a: a.iter().sum(),
            sum_g: g.iter().sum(),
            slot: 0,
            blocks: 0,
        }
    }

    /// Complex samples a block takes: N new ones and N' carried over.
    pub fn block_len(&self) -> usize {
        (self.cfg.n + self.cfg.nc) as usize
    }

    fn params(&self, bp: &BlockParams) -> Params {
        let c = &self.cfg;
        Params {
            n: c.n,
            nc: c.nc,
            log_nc: c.nc.trailing_zeros(),
            hop: 1,
            conj: bp.conj as u32,
            measure: (bp.measure == Measure::Coherence) as u32,
            grid_f: c.grid_f,
            grid_a: c.grid_a,
            beta: bp.beta,
            a_lo: bp.a_range.0,
            a_hi: bp.a_range.1,
            f_lo: bp.f_range.0,
            f_hi: bp.f_range.1,
            norm_scf: (1.0 / (self.sum_g * self.sum_g * self.sum_a * self.sum_a)) as f32,
            norm_psd: (1.0 / (c.n as f64 * self.sum_a * self.sum_a)) as f32,
            ntw: c.n,
        }
    }

    /// Run one block (`2 × block_len()` interleaved floats) and return the
    /// profile of the block before it, once the GPU has finished that one
    /// (`None` for the first, or if its read-back failed).
    pub fn process(&mut self, iq: &[f32], bp: &BlockParams) -> Option<Vec<ProfileRow>> {
        assert_eq!(iq.len(), 2 * self.block_len(), "an SCF block");
        let slot = self.slot;
        self.slot ^= 1;
        // The read-back buffer this block will use must be free again.
        let mine = self.finish(slot);
        let q = &self.queue;
        q.write_buffer(&self.xin[slot], 0, bytemuck::cast_slice(iq));
        q.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&self.params(bp)));
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("scf-block") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("scf-ssca"),
                timestamp_writes: None,
            });
            for p in &self.passes {
                pass.set_pipeline(&p.pipeline);
                pass.set_bind_group(0, &p.bind[slot], &[]);
                pass.dispatch_workgroups(p.groups.0, p.groups.1, 1);
            }
        }
        let bytes = self.cfg.grid_a as u64 * 16;
        enc.copy_buffer_to_buffer(&self.prof, 0, &self.readback[slot], 0, bytes);
        let idx = q.submit([enc.finish()]);
        let flag = self.mapped[slot].clone();
        flag.store(false, Ordering::Release);
        self.readback[slot].slice(..).map_async(wgpu::MapMode::Read, move |r| {
            flag.store(r.is_ok(), Ordering::Release);
        });
        self.pending[slot] = Some(idx);
        self.blocks += 1;
        // The other slot's block was submitted before this one.
        mine.or_else(|| self.finish(slot ^ 1))
    }

    /// Wait for the block in `slot` (if any) and read its profile.
    fn finish(&mut self, slot: usize) -> Option<Vec<ProfileRow>> {
        let idx = self.pending[slot].take()?;
        let _ =
            self.device.poll(wgpu::PollType::Wait { submission_index: Some(idx), timeout: None });
        let buf = &self.readback[slot];
        let out = if self.mapped[slot].load(Ordering::Acquire) {
            buf.slice(..)
                .get_mapped_range()
                .ok()
                .map(|view| bytemuck::cast_slice::<u8, ProfileRow>(&view).to_vec())
        } else {
            None
        };
        buf.unmap();
        out
    }

    /// Wait for everything submitted and return the newest profile.
    pub fn flush(&mut self) -> Option<Vec<ProfileRow>> {
        let older = self.slot;
        let a = self.finish(older);
        let b = self.finish(older ^ 1);
        b.or(a)
    }

    /// Copy a GPU buffer back, blocking: for the tests and measurements.
    pub fn read_f32(&self, which: Readable) -> Vec<f32> {
        let buf = match which {
            Readable::Acc => &self.acc,
            Readable::Psd => &self.psd,
            Readable::Grid => &self.grid,
        };
        let size = buf.size();
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scf-read"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_buffer_to_buffer(buf, 0, &staging, 0, size);
        let idx = self.queue.submit([enc.finish()]);
        staging.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        let _ =
            self.device.poll(wgpu::PollType::Wait { submission_index: Some(idx), timeout: None });
        let v = staging
            .slice(..)
            .get_mapped_range()
            .map(|view| bytemuck::cast_slice::<u8, f32>(&view).to_vec())
            .unwrap_or_default();
        staging.unmap();
        v
    }

    /// Whether |S|² sits in the X buffer (four-step) or T (one pass).
    pub fn four_step(&self) -> bool {
        self.s_in_x
    }
}

/// A buffer the tests read back.
#[derive(Debug, Clone, Copy)]
pub enum Readable {
    Acc,
    Psd,
    Grid,
}
