// The paint-callback structure and the ring-buffer waterfall follow SDRoxide
// (https://github.com/dividebysandwich/sdroxide), crates/sdroxide-ui/src/
// waterfall_gpu.rs and shaders/waterfall.wgsl, GPL-3.0-or-later. The per-row
// band texture, the dBm rows, the 3D surface on the GPU and the rest are this
// panel's own.

//! The waterfall and the 3D surface, drawn on the GPU.
//!
//! # How
//!
//! Both are `egui_wgpu` paint callbacks drawing straight into egui's render
//! pass, reading history textures that stay on the GPU:
//!
//! * a rows texture, one spectrum per row in dBm as half floats
//!   ([`super::rows`]), written a row at a time into a ring;
//! * a 1-pixel-high band texture saying which frequencies each row covers.
//!
//! The waterfall is one triangle over its rectangle; its fragment shader finds
//! each pixel's row and the columns under it and takes the strongest, then
//! colours it through a 256-entry palette. The 3D surface is a grid of
//! vertices whose shader reads the same kind of texture for its heights: one
//! instanced draw for the solid surface, or a fill and a line per row for the
//! line rendering.
//!
//! # Why this rather than an egui texture
//!
//! An egui `TextureHandle` takes RGBA, so a new floor, ceiling, palette,
//! zoom or pan would mean recolouring and re-uploading the whole picture on
//! the CPU, and each new row means a partial upload of colours that are wrong
//! the moment the scale moves. Keeping levels (not colours) on the GPU makes
//! every one of those a uniform change: a row is uploaded once, 32 KB at
//! 16 384 columns, and the shader does the rest per pixel, max-pooling as many
//! columns as fall under a pixel. At 60 frames a second that is 2 MB/s of
//! upload and nothing else, whatever the window size. The 3D surface on the
//! GPU is a quarter of a million vertices a frame computed in the vertex
//! shader instead of tessellated by egui on the CPU, which in SDRoxide costs
//! up to 70 % of a core.
//!
//! # With compute shaders (any Vulkan, DX12 or Metal GPU)
//!
//! * The spectrum itself can be computed here ([`super::gpu_fft`]): its rows
//!   are pooled and copied into the history textures by the GPU, and the
//!   trace is drawn by [`TracePaint`] straight from the GPU's own columns.
//! * The 3D surface is lit, after SDR Console: its rows are kept as f32 with
//!   a max-mip chain across frequency (any zoom is sampled with no carrier
//!   lost between columns), a compute pass works out every point's height
//!   and normal once a frame, the front row moves smoothly from one
//!   spectrum to the next between frames, and it is drawn with per-pixel
//!   lighting into a multisampled target (4× where the GPU has it) that is
//!   then copied into egui's pass. SDRoxide's shape, fog and palette stay.
//!
//! Without them (the GL fallback) both are as before.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use eframe::egui;
use eframe::egui_wgpu::{
    self, CallbackResources, CallbackTrait, RenderState, ScreenDescriptor, wgpu,
};

use super::gpu_fft::{Engine, GpuWork, Readout, Targets};
use super::palette::Palette;
use super::rows::{SURFACE_ROWS, Upload, WF_ROWS};

/// The widest a history row is kept, columns: more than any screen has
/// pixels across a zoomed view and its margins.
pub const MAX_TEX_W: u32 = 16_384;

/// What the scope needs to know about the GPU.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpuInfo {
    /// History row width, columns.
    pub tex_w: u32,
    /// The adapter's name, for the status line.
    pub adapter: &'static str,
    /// Compute shaders and storage buffers in vertex shaders: the FFT on
    /// the GPU and the lit 3D surface.
    pub compute: bool,
    /// Samples per pixel of the lit 3D surface.
    pub msaa: u32,
}

/// What the GPU spectrum runs this repaint, with the I/Q ring it reads and
/// where its readout goes.
pub struct GpuFrame {
    pub work: GpuWork,
    pub iq: wgpu::Buffer,
    pub iq_len: u32,
    /// The newest readout lands here, a frame or two late.
    pub readout: Arc<Mutex<Option<Readout>>>,
}

/// eframe's wgpu set-up for a window with a scope in it: the discrete GPU
/// (on a Framework 16 the RTX rather than the integrated Radeon), and history
/// textures as wide as the adapter allows up to [`MAX_TEX_W`] instead of
/// egui's 8192, so a million-point spectrum keeps 16 384 columns of detail.
pub fn wgpu_options() -> egui_wgpu::WgpuConfiguration {
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.power_preference = wgpu::PowerPreference::HighPerformance;
    setup.device_descriptor = Arc::new(|adapter| {
        let base = if adapter.get_info().backend == wgpu::Backend::Gl {
            wgpu::Limits::downlevel_webgl2_defaults()
        } else {
            wgpu::Limits::default()
        };
        let al = adapter.limits();
        let widest = al.max_texture_dimension_2d;
        wgpu::DeviceDescriptor {
            label: Some("panel wgpu device"),
            required_limits: wgpu::Limits {
                max_texture_dimension_2d: widest.min(MAX_TEX_W),
                // The I/Q ring is 64 MB; ask for what the GPU has, up to 1 GB.
                max_storage_buffer_binding_size: al
                    .max_storage_buffer_binding_size
                    .min(1 << 30)
                    .max(base.max_storage_buffer_binding_size),
                max_buffer_size: al.max_buffer_size.min(1 << 30).max(base.max_buffer_size),
                ..base
            },
            ..Default::default()
        }
    });
    egui_wgpu::WgpuConfiguration {
        wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

/// Compile the pipelines and register them with egui's renderer. Call once,
/// at start-up, with eframe's render state; a second call is harmless.
pub fn init(rs: &RenderState) -> GpuInfo {
    let limits = rs.device.limits();
    let tex_w = limits.max_texture_dimension_2d.min(MAX_TEX_W);
    let adapter: &'static str = Box::leak(rs.adapter.get_info().name.into_boxed_str());
    let flags = rs.adapter.get_downlevel_capabilities().flags;
    let compute = flags
        .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS | wgpu::DownlevelFlags::VERTEX_STORAGE)
        && limits.max_storage_buffers_per_shader_stage >= 4
        && limits.max_compute_workgroup_storage_size >= 16_384
        && limits.max_storage_buffer_binding_size >= super::gpu_iq::RING as u64 * 8
        && limits.max_storage_textures_per_shader_stage >= 1;
    let features = rs.adapter.get_texture_format_features(rs.target_format).flags;
    let msaa = if features.sample_count_supported(4) { 4 } else { 1 };
    let info = GpuInfo { tex_w, adapter, compute, msaa };
    if rs.renderer.read().callback_resources.get::<Registry>().is_some() {
        return info;
    }
    let mut shared = Shared::new(&rs.device, rs.target_format);
    if compute {
        shared.lit = Some(Lit::new(&rs.device, rs.target_format, msaa));
    }
    rs.renderer.write().callback_resources.insert(Registry { shared, per: HashMap::new() });
    info
}

/// Free one scope's textures.
pub fn retire(rs: &RenderState, id: u64) {
    if let Some(r) = rs.renderer.write().callback_resources.get_mut::<Registry>() {
        r.per.remove(&id);
    }
}

struct Registry {
    shared: Shared,
    per: HashMap<u64, Resources>,
}

struct Shared {
    layout: wgpu::BindGroupLayout,
    waterfall: wgpu::RenderPipeline,
    fill: wgpu::RenderPipeline,
    line: wgpu::RenderPipeline,
    srgb: bool,
    format: wgpu::TextureFormat,
    lit: Option<Lit>,
}

const COMMON: &str = include_str!("shaders/common.wgsl");
const S3_COMMON: &str = include_str!("shaders/s3_common.wgsl");
const FIELD: &str = include_str!("shaders/field.wgsl");
const SURFACE_LIT: &str = include_str!("shaders/surface_lit.wgsl");
const MIPS: &str = include_str!("shaders/mips.wgsl");
const BLIT: &str = include_str!("shaders/blit.wgsl");
const TRACE: &str = include_str!("shaders/trace.wgsl");

/// Bytes in the lit surface's uniforms (`S3L`), the mip pass's and the
/// trace's.
const S3L_BYTES: u64 = 128;
const MIP_BYTES: u64 = 80;
const TRACE_BYTES: u64 = 80;
/// The widest the 3D surface is drawn, columns.
const MAX_S3_COLS: u32 = 4096;
/// The coarsest mip level kept is this many texels wide.
const MIP_FLOOR: u32 = 32;

fn entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
    ty: wgpu::BindingType,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility, ty, count: None }
}

fn uniform_ty() -> wgpu::BindingType {
    wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Uniform,
        has_dynamic_offset: false,
        min_binding_size: None,
    }
}

fn storage_ty(read_only: bool) -> wgpu::BindingType {
    wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Storage { read_only },
        has_dynamic_offset: false,
        min_binding_size: None,
    }
}

fn tex_ty() -> wgpu::BindingType {
    wgpu::BindingType::Texture {
        sample_type: wgpu::TextureSampleType::Float { filterable: false },
        view_dimension: wgpu::TextureViewDimension::D2,
        multisampled: false,
    }
}

fn layout(
    device: &wgpu::Device,
    label: &str,
    entries: &[wgpu::BindGroupLayoutEntry],
) -> wgpu::BindGroupLayout {
    device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries })
}

fn pipeline_layout(device: &wgpu::Device, bgl: &wgpu::BindGroupLayout) -> wgpu::PipelineLayout {
    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(bgl)],
        immediate_size: 0,
    })
}

fn module(device: &wgpu::Device, label: &str, src: String) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(src.into()),
    })
}

#[allow(clippy::too_many_arguments)]
fn render_pipeline(
    device: &wgpu::Device,
    label: &str,
    pl: &wgpu::PipelineLayout,
    m: &wgpu::ShaderModule,
    vs: &str,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
    topology: wgpu::PrimitiveTopology,
    samples: u32,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(pl),
        vertex: wgpu::VertexState {
            module: m,
            entry_point: Some(vs),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: m,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState { topology, ..Default::default() },
        depth_stencil: None,
        multisample: wgpu::MultisampleState { count: samples, ..Default::default() },
        multiview_mask: None,
        cache: None,
    })
}

fn compute_pipeline(
    device: &wgpu::Device,
    pl: &wgpu::PipelineLayout,
    m: &wgpu::ShaderModule,
    entry: &str,
) -> wgpu::ComputePipeline {
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(entry),
        layout: Some(pl),
        module: m,
        entry_point: Some(entry),
        compilation_options: Default::default(),
        cache: None,
    })
}

/// The pipelines that need compute shaders: the lit surface and the GPU
/// trace.
struct Lit {
    field_layout: wgpu::BindGroupLayout,
    levels: wgpu::ComputePipeline,
    normals: wgpu::ComputePipeline,
    mip_layout: wgpu::BindGroupLayout,
    mip: wgpu::ComputePipeline,
    draw_layout: wgpu::BindGroupLayout,
    fill: wgpu::RenderPipeline,
    line: wgpu::RenderPipeline,
    blit_layout: wgpu::BindGroupLayout,
    blit: wgpu::RenderPipeline,
    trace_layout: wgpu::BindGroupLayout,
    t_fill: wgpu::RenderPipeline,
    t_line: wgpu::RenderPipeline,
    t_peak: wgpu::RenderPipeline,
    msaa: u32,
}

impl Lit {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat, msaa: u32) -> Lit {
        use wgpu::ShaderStages as S;
        let c = S::COMPUTE;
        let field_layout = layout(
            device,
            "scope-s3-field",
            &[
                entry(0, c, uniform_ty()),
                entry(1, c, tex_ty()),
                entry(2, c, tex_ty()),
                entry(3, c, tex_ty()),
                entry(4, c, storage_ty(false)),
                entry(5, c, storage_ty(false)),
            ],
        );
        let fm = module(device, "scope-s3-field", format!("{S3_COMMON}\n{FIELD}"));
        let fpl = pipeline_layout(device, &field_layout);
        let mip_layout = layout(
            device,
            "scope-s3-mip",
            &[
                entry(0, c, uniform_ty()),
                entry(1, c, tex_ty()),
                entry(
                    2,
                    c,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::R32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                ),
            ],
        );
        let mm = module(device, "scope-s3-mip", MIPS.to_string());
        let draw_layout = layout(
            device,
            "scope-s3-lit",
            &[
                entry(0, S::VERTEX_FRAGMENT, uniform_ty()),
                entry(1, S::VERTEX, storage_ty(true)),
                entry(2, S::VERTEX, tex_ty()),
            ],
        );
        let dm = module(device, "scope-s3-lit", format!("{S3_COMMON}\n{SURFACE_LIT}"));
        let dpl = pipeline_layout(device, &draw_layout);
        let blit_layout = layout(device, "scope-s3-blit", &[entry(0, S::FRAGMENT, tex_ty())]);
        let bm = module(device, "scope-s3-blit", BLIT.to_string());
        let trace_layout = layout(
            device,
            "scope-trace",
            &[entry(0, S::VERTEX, uniform_ty()), entry(1, S::VERTEX, storage_ty(true))],
        );
        let tm = module(device, "scope-trace", TRACE.to_string());
        let tpl = pipeline_layout(device, &trace_layout);
        let pre = Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING);
        use wgpu::PrimitiveTopology as T;
        Lit {
            levels: compute_pipeline(device, &fpl, &fm, "levels"),
            normals: compute_pipeline(device, &fpl, &fm, "normals"),
            mip: compute_pipeline(device, &pipeline_layout(device, &mip_layout), &mm, "mip"),
            fill: render_pipeline(
                device,
                "scope-s3-lit-fill",
                &dpl,
                &dm,
                "vs_fill",
                format,
                None,
                T::TriangleList,
                msaa,
            ),
            line: render_pipeline(
                device,
                "scope-s3-lit-line",
                &dpl,
                &dm,
                "vs_line",
                format,
                None,
                T::LineStrip,
                msaa,
            ),
            blit: render_pipeline(
                device,
                "scope-s3-blit",
                &pipeline_layout(device, &blit_layout),
                &bm,
                "vs_main",
                format,
                None,
                T::TriangleList,
                1,
            ),
            t_fill: render_pipeline(
                device,
                "scope-trace-fill",
                &tpl,
                &tm,
                "vs_fill",
                format,
                pre,
                T::TriangleStrip,
                1,
            ),
            t_line: render_pipeline(
                device,
                "scope-trace-line",
                &tpl,
                &tm,
                "vs_line",
                format,
                pre,
                T::TriangleList,
                1,
            ),
            t_peak: render_pipeline(
                device,
                "scope-trace-peak",
                &tpl,
                &tm,
                "vs_peak",
                format,
                pre,
                T::TriangleList,
                1,
            ),
            field_layout,
            mip_layout,
            draw_layout,
            blit_layout,
            trace_layout,
            msaa,
        }
    }
}

impl Shared {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Shared {
        let tex = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scope-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex(1),
                tex(2),
                tex(3),
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scope-pl"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let module = |label, body: &str| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(format!("{COMMON}\n{body}").into()),
            })
        };
        let wf = module("scope-waterfall", include_str!("shaders/waterfall.wgsl"));
        let s3 = module("scope-surface", include_str!("shaders/surface.wgsl"));
        let pipeline = |label, m: &wgpu::ShaderModule, vs, fs, topology| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: m,
                    entry_point: Some(vs),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: m,
                    entry_point: Some(fs),
                    targets: &[Some(format.into())],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState { topology, ..Default::default() },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        Shared {
            waterfall: pipeline(
                "scope-waterfall",
                &wf,
                "vs_main",
                "fs_main",
                wgpu::PrimitiveTopology::TriangleList,
            ),
            fill: pipeline(
                "scope-surface-fill",
                &s3,
                "vs_fill",
                "fs_main",
                wgpu::PrimitiveTopology::TriangleList,
            ),
            line: pipeline(
                "scope-surface-line",
                &s3,
                "vs_line",
                "fs_main",
                wgpu::PrimitiveTopology::LineStrip,
            ),
            layout,
            srgb: format.is_srgb(),
            format,
            lit: None,
        }
    }
}

/// One scope's textures and bind groups.
struct Resources {
    tex_w: u32,
    wf_rows: wgpu::Texture,
    wf_geom: wgpu::Texture,
    s3_rows: wgpu::Texture,
    s3_geom: wgpu::Texture,
    lut: wgpu::Texture,
    wf_uni: wgpu::Buffer,
    s3_uni: wgpu::Buffer,
    wf_bg: wgpu::BindGroup,
    s3_bg: wgpu::BindGroup,
    applied: u64,
    palette: Option<Palette>,
    lit: Option<LitRes>,
    /// The GPU spectrum, once one has run, and the last readout handed on.
    engine: Option<Engine>,
    read_seq: u64,
    trace_uni: wgpu::Buffer,
    trace_bg: Option<wgpu::BindGroup>,
}

/// The lit surface's own resources.
struct LitRes {
    lods: u32,
    /// The live row before the newest spectrum, every mip level.
    prev: wgpu::Texture,
    /// CPU rows on their way into `s3_rows`, by slot, f32.
    staging: wgpu::Buffer,
    uni: wgpu::Buffer,
    mip_uni: wgpu::Buffer,
    /// One bind group per level from 1: level - 1 in, level out.
    mip_bgs: Vec<wgpu::BindGroup>,
    field_bg: wgpu::BindGroup,
    draw_bg: wgpu::BindGroup,
    /// The multisampled target and what it resolves into, by size.
    target: Option<Target>,
}

struct Target {
    size: [u32; 2],
    msaa: Option<wgpu::TextureView>,
    resolved: wgpu::TextureView,
    blit_bg: wgpu::BindGroup,
}

fn texture(
    device: &wgpu::Device,
    label: &str,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    texture_ex(device, label, w, h, format, 1, wgpu::TextureUsages::empty())
}

fn texture_ex(
    device: &wgpu::Device,
    label: &str,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
    mips: u32,
    extra: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: mips,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | extra,
        view_formats: &[],
    })
}

fn level_view(t: &wgpu::Texture, level: u32) -> wgpu::TextureView {
    t.create_view(&wgpu::TextureViewDescriptor {
        base_mip_level: level,
        mip_level_count: Some(1),
        ..Default::default()
    })
}

fn bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    res: &[wgpu::BindingResource<'_>],
) -> wgpu::BindGroup {
    let entries: Vec<wgpu::BindGroupEntry> = res
        .iter()
        .enumerate()
        .map(|(i, r)| wgpu::BindGroupEntry { binding: i as u32, resource: r.clone() })
        .collect();
    device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout, entries: &entries })
}

fn uniform_buf(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Mip levels of a lit history row `tex_w` wide.
fn lods(tex_w: u32) -> u32 {
    (tex_w / MIP_FLOOR).max(1).ilog2() + 1
}

impl Resources {
    fn new(device: &wgpu::Device, shared: &Shared, tex_w: u32) -> Resources {
        use wgpu::TextureFormat as F;
        use wgpu::TextureUsages as TU;
        let wf_rows = texture(device, "scope-wf-rows", tex_w, WF_ROWS, F::R16Float);
        let wf_geom = texture(device, "scope-wf-geom", WF_ROWS, 1, F::Rg32Float);
        let lods = lods(tex_w);
        let s3_rows = match shared.lit {
            Some(_) => texture_ex(
                device,
                "scope-s3-rows",
                tex_w,
                SURFACE_ROWS,
                F::R32Float,
                lods,
                TU::STORAGE_BINDING | TU::COPY_SRC,
            ),
            None => texture(device, "scope-s3-rows", tex_w, SURFACE_ROWS, F::R16Float),
        };
        let s3_geom = texture(device, "scope-s3-geom", SURFACE_ROWS, 1, F::Rg32Float);
        let lut = texture(device, "scope-lut", 256, 1, F::Rgba8Unorm);
        let wf_uni = uniform_buf(device, "scope-wf-uniforms", (WF_UNIFORMS * 4) as u64);
        let s3_uni = uniform_buf(device, "scope-s3-uniforms", (S3_UNIFORMS * 4) as u64);
        let lut_view = lut.create_view(&Default::default());
        let geom_s3 = s3_geom.create_view(&Default::default());
        let rows_s3 = s3_rows.create_view(&Default::default());
        let bg = |uni: &wgpu::Buffer, rv: &wgpu::TextureView, gv: &wgpu::TextureView| {
            bind_group(
                device,
                &shared.layout,
                &[
                    uni.as_entire_binding(),
                    wgpu::BindingResource::TextureView(rv),
                    wgpu::BindingResource::TextureView(gv),
                    wgpu::BindingResource::TextureView(&lut_view),
                ],
            )
        };
        let wf_bg = bg(
            &wf_uni,
            &wf_rows.create_view(&Default::default()),
            &wf_geom.create_view(&Default::default()),
        );
        // The legacy surface reads level 0 only (an R16 texture has no other).
        let s3_bg = bg(&s3_uni, &level_view(&s3_rows, 0), &geom_s3);
        let lit = shared.lit.as_ref().map(|l| {
            let prev =
                texture_ex(device, "scope-s3-prev", tex_w, 1, F::R32Float, lods, TU::empty());
            let staging = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("scope-s3-staging"),
                size: SURFACE_ROWS as u64 * tex_w as u64 * 4,
                usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let uni = uniform_buf(device, "scope-s3-lit-uniforms", S3L_BYTES);
            let mip_uni = uniform_buf(device, "scope-s3-mip-uniforms", MIP_BYTES);
            let mip_bgs = (1..lods)
                .map(|lv| {
                    bind_group(
                        device,
                        &l.mip_layout,
                        &[
                            mip_uni.as_entire_binding(),
                            wgpu::BindingResource::TextureView(&level_view(&s3_rows, lv - 1)),
                            wgpu::BindingResource::TextureView(&level_view(&s3_rows, lv)),
                        ],
                    )
                })
                .collect();
            let cells = MAX_S3_COLS as u64 * SURFACE_ROWS as u64;
            let storage = |label, size| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size,
                    usage: wgpu::BufferUsages::STORAGE,
                    mapped_at_creation: false,
                })
            };
            let lv = storage("scope-s3-levels", cells * 4);
            let nrm = storage("scope-s3-normals", cells * 16);
            let field_bg = bind_group(
                device,
                &l.field_layout,
                &[
                    uni.as_entire_binding(),
                    wgpu::BindingResource::TextureView(&rows_s3),
                    wgpu::BindingResource::TextureView(&geom_s3),
                    wgpu::BindingResource::TextureView(&prev.create_view(&Default::default())),
                    lv.as_entire_binding(),
                    nrm.as_entire_binding(),
                ],
            );
            let draw_bg = bind_group(
                device,
                &l.draw_layout,
                &[
                    uni.as_entire_binding(),
                    nrm.as_entire_binding(),
                    wgpu::BindingResource::TextureView(&lut_view),
                ],
            );
            LitRes { lods, prev, staging, uni, mip_uni, mip_bgs, field_bg, draw_bg, target: None }
        });
        Resources {
            tex_w,
            wf_rows,
            wf_geom,
            s3_rows,
            s3_geom,
            lut,
            wf_uni,
            s3_uni,
            wf_bg,
            s3_bg,
            applied: 0,
            palette: None,
            lit,
            engine: None,
            read_seq: 0,
            trace_uni: uniform_buf(device, "scope-trace-uniforms", TRACE_BYTES),
            trace_bg: None,
        }
    }

    /// Write this repaint's rows and run its GPU spectrum, once, whichever
    /// callback gets here first. Everything that must happen in order (the
    /// previous live row kept, then new rows, then their mips) goes into
    /// `enc`; `Queue::write_*` would all land before any of it.
    fn apply(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        shared: &Shared,
        up: &Upload,
    ) {
        if up.seq <= self.applied {
            return;
        }
        self.applied = up.seq;
        if up.clear {
            let zeros = vec![0u8; WF_ROWS as usize * 8];
            write(queue, &self.wf_geom, 0, 0, WF_ROWS, &zeros, WF_ROWS * 8);
            write(queue, &self.s3_geom, 0, 0, SURFACE_ROWS, &zeros, SURFACE_ROWS * 8);
        }
        let w = self.tex_w;
        for r in &up.wf {
            if r.bins.len() == w as usize {
                write(queue, &self.wf_rows, 0, r.slot, w, bytemuck::cast_slice(&r.bins), w * 2);
            }
            let g = [r.lo, r.hi];
            write(queue, &self.wf_geom, r.slot, 0, 1, bytemuck::cast_slice(&g), 8);
        }
        for r in &up.surface {
            let g = [r.lo, r.hi];
            write(queue, &self.s3_geom, r.slot, 0, 1, bytemuck::cast_slice(&g), 8);
        }
        match &self.lit {
            None => {
                for r in up.surface.iter().filter(|r| r.bins.len() == w as usize) {
                    write(queue, &self.s3_rows, 0, r.slot, w, bytemuck::cast_slice(&r.bins), w * 2);
                }
            }
            Some(l) => {
                if let Some(p) = up.prev {
                    for lv in 0..l.lods {
                        enc.copy_texture_to_texture(
                            wgpu::TexelCopyTextureInfo {
                                texture: &self.s3_rows,
                                mip_level: lv,
                                origin: wgpu::Origin3d { x: 0, y: p, z: 0 },
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::TexelCopyTextureInfo {
                                texture: &l.prev,
                                mip_level: lv,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::Extent3d {
                                width: (w >> lv).max(1),
                                height: 1,
                                depth_or_array_layers: 1,
                            },
                        );
                    }
                }
                for r in up.surface.iter().filter(|r| r.bins.len() == w as usize) {
                    let row: Vec<f32> =
                        r.bins.iter().map(|&b| half::f16::from_bits(b).to_f32()).collect();
                    let off = r.slot as u64 * w as u64 * 4;
                    queue.write_buffer(&l.staging, off, bytemuck::cast_slice(&row));
                    enc.copy_buffer_to_texture(
                        wgpu::TexelCopyBufferInfo {
                            buffer: &l.staging,
                            layout: wgpu::TexelCopyBufferLayout {
                                offset: off,
                                bytes_per_row: Some(w * 4),
                                rows_per_image: None,
                            },
                        },
                        wgpu::TexelCopyTextureInfo {
                            texture: &self.s3_rows,
                            mip_level: 0,
                            origin: wgpu::Origin3d { x: 0, y: r.slot, z: 0 },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::Extent3d { width: w, height: 1, depth_or_array_layers: 1 },
                    );
                }
            }
        }
        if let (Some(g), Some(lit)) = (&up.gpu, &shared.lit) {
            let g = &g.0;
            if self.engine.as_ref().is_none_or(|e| e.iq() != &g.iq) {
                let e = Engine::new(device, &g.iq, g.iq_len);
                self.trace_bg = Some(bind_group(
                    device,
                    &lit.trace_layout,
                    &[self.trace_uni.as_entire_binding(), e.out().as_entire_binding()],
                ));
                self.engine = Some(e);
            }
            if let Some(e) = self.engine.as_mut() {
                let t = Targets { wf: &self.wf_rows, wf_w: w, s3: &self.s3_rows, s3_w: w };
                e.record(device, queue, enc, &g.work, &t);
                let _ = device.poll(wgpu::PollType::Poll);
                if let Some(r) = e.collect() {
                    let seq = r.meta.as_ref().map_or(0, |m| m.seq);
                    if seq > self.read_seq {
                        self.read_seq = seq;
                        *g.readout.lock().unwrap_or_else(|e| e.into_inner()) = Some(r.clone());
                    }
                }
            }
        }
        // The new surface rows' mip chains.
        if let (Some(l), Some(lit)) = (&self.lit, &shared.lit)
            && !up.surface.is_empty()
            && l.lods > 1
        {
            let mut slots: Vec<u32> = up.surface.iter().map(|r| r.slot).collect();
            slots.sort_unstable();
            slots.dedup();
            slots.truncate(16);
            let mut u = [0u32; 20];
            u[0] = slots.len() as u32;
            u[4..4 + slots.len()].copy_from_slice(&slots);
            queue.write_buffer(&l.mip_uni, 0, bytemuck::cast_slice(&u));
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("scope-s3-mips"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&lit.mip);
            for (i, bg) in l.mip_bgs.iter().enumerate() {
                let lw = (w >> (i + 1)).max(1);
                pass.set_bind_group(0, bg, &[]);
                pass.dispatch_workgroups(lw.div_ceil(64), slots.len() as u32, 1);
            }
        }
    }

    fn set_palette(&mut self, queue: &wgpu::Queue, p: Palette) {
        if self.palette != Some(p) {
            write(queue, &self.lut, 0, 0, 256, &p.lut(), 256 * 4);
            self.palette = Some(p);
        }
    }
}

/// Write one row of `w` texels at (`x`, `y`).
fn write(queue: &wgpu::Queue, tex: &wgpu::Texture, x: u32, y: u32, w: u32, data: &[u8], bpr: u32) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: None },
        wgpu::Extent3d { width: w, height: 1, depth_or_array_layers: 1 },
    );
}

/// Floats in the waterfall's uniforms (see waterfall.wgsl).
pub const WF_UNIFORMS: usize = 16;
/// Floats in the surface's uniforms (see surface.wgsl).
pub const S3_UNIFORMS: usize = 24;

/// The waterfall's settings for one repaint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WfParams {
    /// The view, Hz from the reference.
    pub view_lo: f32,
    pub view_hi: f32,
    pub floor: f32,
    pub ceil: f32,
    pub newest: u32,
    pub count: u32,
    /// History rows across the waterfall's height.
    pub rows_vis: f32,
    /// The waterfall's width, points.
    pub width_pts: f32,
    pub smooth: bool,
    pub scroll_frac: f32,
    pub contrast: f32,
    pub brightness: f32,
}

/// The 3D surface's settings for one repaint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceParams {
    pub view_lo: f32,
    pub view_hi: f32,
    pub floor: f32,
    pub ceil: f32,
    pub live: u32,
    pub count: u32,
    pub carry: f32,
    pub cols: u32,
    pub solid: bool,
    pub bg: [f32; 3],
    pub ink: [f32; 3],
    /// How far the front row has come from the previous spectrum (0 to 1),
    /// and that spectrum's band, Hz from the reference (lit surface only).
    pub live_t: f32,
    pub prev: Option<(f32, f32)>,
    /// How much the lighting shows, 0 to 1.
    pub light: f32,
}

/// The GPU trace's settings for one repaint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TraceParams {
    pub floor: f32,
    pub ceil: f32,
    /// Columns the GPU pooled, and where the first's left and the last's
    /// right edge fall across the rectangle, 0 to 1.
    pub cols: u32,
    pub x0: f32,
    pub x1: f32,
    /// The rectangle, physical pixels.
    pub px: [f32; 2],
    pub line_w: f32,
    /// The fill under the trace (off under the persistence), the line's
    /// alpha, and the peak-hold trace.
    pub fill: bool,
    pub line_a: f32,
    pub peak: bool,
    pub accent: [f32; 3],
    pub ink: [f32; 3],
}

/// SDRoxide's surface geometry (widgets/spectrum3d.rs).
pub const DEPTH_FRAC: f32 = 0.46;
pub const PERSP: f32 = 0.85;
pub const FOG: f32 = 0.55;
pub const FILL: f32 = DEPTH_FRAC + (1.0 - DEPTH_FRAC) / (1.0 + PERSP);

/// The height, as a share of the surface's rectangle, the front row's
/// amplitude axis takes: what the markers on the surface are kept inside.
pub fn front_plane(h: f32) -> f32 {
    h * (1.0 - DEPTH_FRAC) / FILL
}

/// Where a point on the front plane at `x_lin` (0..1 across) lands at depth
/// `d` (0 front, 1 back), and the baseline there, both as shares of the
/// rectangle: for drawing the receding floor grid over the surface.
pub fn project(x_lin: f32, d: f32) -> (f32, f32) {
    let s = 1.0 / (1.0 + PERSP * d);
    let far = 1.0 / (1.0 + PERSP);
    let depth_h = DEPTH_FRAC / FILL;
    (0.5 + (x_lin - 0.5) * s, 1.0 - depth_h * (1.0 - s) / (1.0 - far))
}

fn inv_range(floor: f32, ceil: f32) -> f32 {
    1.0 / (ceil - floor).max(1.0)
}

/// The waterfall's paint callback.
pub struct WaterfallPaint {
    pub id: u64,
    pub tex_w: u32,
    pub upload: Option<Arc<Upload>>,
    pub palette: Palette,
    pub params: WfParams,
}

/// The 3D surface's paint callback.
pub struct SurfacePaint {
    pub id: u64,
    pub tex_w: u32,
    pub upload: Option<Arc<Upload>>,
    pub palette: Palette,
    pub params: SurfaceParams,
    /// The rectangle, physical pixels: the lit surface's target.
    pub px: [u32; 2],
}

/// The spectrum trace drawn from the GPU spectrum's own columns.
pub struct TracePaint {
    pub id: u64,
    pub tex_w: u32,
    pub upload: Option<Arc<Upload>>,
    pub params: TraceParams,
}

fn resources<'a>(
    device: &wgpu::Device,
    reg: &'a mut Registry,
    id: u64,
    tex_w: u32,
) -> (&'a Shared, &'a mut Resources) {
    let Registry { shared, per } = reg;
    let r = per.entry(id).or_insert_with(|| Resources::new(device, shared, tex_w));
    if r.tex_w != tex_w {
        *r = Resources::new(device, shared, tex_w);
    }
    (shared, r)
}

impl CallbackTrait for WaterfallPaint {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        res: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(reg) = res.get_mut::<Registry>() else { return Vec::new() };
        let (shared, r) = resources(device, reg, self.id, self.tex_w);
        if let Some(up) = &self.upload {
            r.apply(device, queue, encoder, shared, up);
        }
        r.set_palette(queue, self.palette);
        let p = &self.params;
        // The shader pools the columns under each physical pixel.
        let px_w = p.width_pts * screen.pixels_per_point;
        let u: [f32; WF_UNIFORMS] = [
            p.view_lo,
            p.view_hi,
            p.floor,
            inv_range(p.floor, p.ceil),
            p.newest as f32,
            p.rows_vis,
            p.count as f32,
            px_w,
            self.tex_w as f32,
            WF_ROWS as f32,
            if p.smooth { 1.0 } else { 0.0 },
            if shared.srgb { 1.0 } else { 0.0 },
            p.scroll_frac,
            p.contrast,
            p.brightness,
            0.0,
        ];
        queue.write_buffer(&r.wf_uni, 0, bytemuck::cast_slice(&u));
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        res: &CallbackResources,
    ) {
        let Some(reg) = res.get::<Registry>() else { return };
        let Some(r) = reg.per.get(&self.id) else { return };
        pass.set_pipeline(&reg.shared.waterfall);
        pass.set_bind_group(0, &r.wf_bg, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// An sRGB colour as the target wants it.
fn linear(c: f32, srgb: bool) -> f64 {
    let c = c as f64;
    if !srgb {
        c
    } else if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

impl SurfacePaint {
    fn legacy_uniforms(&self, srgb: bool) -> [f32; S3_UNIFORMS] {
        let p = &self.params;
        [
            p.view_lo,
            p.view_hi,
            p.floor,
            inv_range(p.floor, p.ceil),
            p.live as f32,
            p.count as f32,
            p.carry,
            p.cols as f32,
            self.tex_w as f32,
            SURFACE_ROWS as f32,
            if p.solid { 1.0 } else { 0.0 },
            if srgb { 1.0 } else { 0.0 },
            DEPTH_FRAC,
            PERSP,
            FOG,
            FILL,
            p.bg[0],
            p.bg[1],
            p.bg[2],
            1.0,
            p.ink[0],
            p.ink[1],
            p.ink[2],
            1.0,
        ]
    }
}

/// Draw the surface's rows back to front, `cols` across: one instanced draw
/// of solid surface, or each row's fill then its line, so a near row's fill
/// hides the lines behind it.
fn draw_rows(
    pass: &mut wgpu::RenderPass<'_>,
    p: &SurfaceParams,
    cols: u32,
    fill: &wgpu::RenderPipeline,
    line: &wgpu::RenderPipeline,
) {
    let fill_verts = (cols - 1) * 12;
    if p.solid {
        pass.set_pipeline(fill);
        pass.draw(0..fill_verts, 0..p.count);
    } else {
        for ii in 0..p.count {
            pass.set_pipeline(fill);
            pass.draw(0..fill_verts, ii..ii + 1);
            pass.set_pipeline(line);
            pass.draw(0..cols, ii..ii + 1);
        }
    }
}

impl CallbackTrait for SurfacePaint {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        res: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(reg) = res.get_mut::<Registry>() else { return Vec::new() };
        let (shared, r) = resources(device, reg, self.id, self.tex_w);
        if let Some(up) = &self.upload {
            r.apply(device, queue, encoder, shared, up);
        }
        r.set_palette(queue, self.palette);
        let base = self.legacy_uniforms(shared.srgb);
        let (Some(lit), Some(l)) = (&shared.lit, r.lit.as_mut()) else {
            queue.write_buffer(&r.s3_uni, 0, bytemuck::cast_slice(&base));
            return Vec::new();
        };
        let p = &self.params;
        let cols = p.cols.min(MAX_S3_COLS);
        let mut u = [0f32; S3L_BYTES as usize / 4];
        u[..S3_UNIFORMS].copy_from_slice(&base);
        u[7] = cols as f32;
        let (plo, phi) = p.prev.unwrap_or((0.0, 0.0));
        u[24..32].copy_from_slice(&[
            p.live_t,
            plo,
            phi,
            l.lods as f32,
            if p.solid { p.light } else { 0.0 },
            if p.prev.is_some() { 1.0 } else { 0.0 },
            0.0,
            0.0,
        ]);
        queue.write_buffer(&l.uni, 0, bytemuck::cast_slice(&u));

        // The target, the rectangle's size in pixels.
        let size = [self.px[0].clamp(1, 8192), self.px[1].clamp(1, 8192)];
        if l.target.as_ref().is_none_or(|t| t.size != size) {
            let tex = |label, samples, usage| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: size[0],
                        height: size[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: samples,
                    dimension: wgpu::TextureDimension::D2,
                    format: shared.format,
                    usage,
                    view_formats: &[],
                })
            };
            use wgpu::TextureUsages as TU;
            let resolved = tex("scope-s3-image", 1, TU::RENDER_ATTACHMENT | TU::TEXTURE_BINDING)
                .create_view(&Default::default());
            let msaa = (lit.msaa > 1).then(|| {
                tex("scope-s3-msaa", lit.msaa, TU::RENDER_ATTACHMENT)
                    .create_view(&Default::default())
            });
            let blit_bg = bind_group(
                device,
                &lit.blit_layout,
                &[wgpu::BindingResource::TextureView(&resolved)],
            );
            l.target = Some(Target { size, msaa, resolved, blit_bg });
        }
        let Some(t) = &l.target else { return Vec::new() };
        let drawn = p.count > 0 && cols >= 2;
        if drawn {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("scope-s3-field"),
                timestamp_writes: None,
            });
            pass.set_bind_group(0, &l.field_bg, &[]);
            pass.set_pipeline(&lit.levels);
            pass.dispatch_workgroups(cols.div_ceil(64), p.count, 1);
            pass.set_pipeline(&lit.normals);
            pass.dispatch_workgroups(cols.div_ceil(64), p.count, 1);
        }
        let bg = wgpu::Color {
            r: linear(p.bg[0], shared.srgb),
            g: linear(p.bg[1], shared.srgb),
            b: linear(p.bg[2], shared.srgb),
            a: 1.0,
        };
        let (view, resolve) = match &t.msaa {
            Some(m) => (m, Some(&t.resolved)),
            None => (&t.resolved, None),
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scope-s3-lit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: resolve,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(bg),
                    store: if resolve.is_some() {
                        wgpu::StoreOp::Discard
                    } else {
                        wgpu::StoreOp::Store
                    },
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if drawn {
            pass.set_bind_group(0, &l.draw_bg, &[]);
            draw_rows(&mut pass, p, cols, &lit.fill, &lit.line);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        res: &CallbackResources,
    ) {
        let Some(reg) = res.get::<Registry>() else { return };
        let Some(r) = reg.per.get(&self.id) else { return };
        if let (Some(lit), Some(t)) =
            (&reg.shared.lit, r.lit.as_ref().and_then(|l| l.target.as_ref()))
        {
            pass.set_pipeline(&lit.blit);
            pass.set_bind_group(0, &t.blit_bg, &[]);
            pass.draw(0..3, 0..1);
            return;
        }
        let p = &self.params;
        if p.count == 0 || p.cols < 2 {
            return;
        }
        pass.set_bind_group(0, &r.s3_bg, &[]);
        draw_rows(pass, p, p.cols, &reg.shared.fill, &reg.shared.line);
    }
}

impl CallbackTrait for TracePaint {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        res: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(reg) = res.get_mut::<Registry>() else { return Vec::new() };
        let (shared, r) = resources(device, reg, self.id, self.tex_w);
        if let Some(up) = &self.upload {
            r.apply(device, queue, encoder, shared, up);
        }
        let p = &self.params;
        let u: [f32; TRACE_BYTES as usize / 4] = [
            p.floor,
            inv_range(p.floor, p.ceil),
            p.cols as f32,
            p.x0,
            p.x1,
            p.px[0],
            p.px[1],
            p.line_w,
            if p.fill { 1.0 } else { 0.0 },
            p.line_a,
            if shared.srgb { 1.0 } else { 0.0 },
            0.0,
            p.accent[0],
            p.accent[1],
            p.accent[2],
            1.0,
            p.ink[0],
            p.ink[1],
            p.ink[2],
            1.0,
        ];
        queue.write_buffer(&r.trace_uni, 0, bytemuck::cast_slice(&u));
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        res: &CallbackResources,
    ) {
        let Some(reg) = res.get::<Registry>() else { return };
        let (Some(lit), Some(r)) = (&reg.shared.lit, reg.per.get(&self.id)) else { return };
        let Some(bg) = &r.trace_bg else { return };
        let p = &self.params;
        if p.cols < 2 {
            return;
        }
        pass.set_bind_group(0, bg, &[]);
        if p.fill {
            pass.set_pipeline(&lit.t_fill);
            pass.draw(0..2 * p.cols, 0..1);
        }
        if p.peak {
            pass.set_pipeline(&lit.t_peak);
            pass.draw(0..6 * (p.cols - 1), 0..1);
        }
        pass.set_pipeline(&lit.t_line);
        pass.draw(0..6 * (p.cols - 1), 0..1);
    }
}

/// A paint callback for egui.
pub fn callback(rect: egui::Rect, cb: impl CallbackTrait + 'static) -> egui::Shape {
    egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(rect, cb))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse and validate a shader as `Shared::new` builds it, and return the
    /// size of its uniform block, bytes.
    fn check(body: &str) -> u32 {
        validate(&format!("{COMMON}\n{body}")).expect("a uniform block")
    }

    /// Validate a whole shader, and return its uniform block's size.
    fn validate(src: &str) -> Option<u32> {
        let module = naga::front::wgsl::parse_str(src)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e:?}"));
        let mut layouter = naga::proc::Layouter::default();
        layouter.update(module.to_ctx()).unwrap();
        let (_, uniform) =
            module.global_variables.iter().find(|(_, g)| g.space == naga::AddressSpace::Uniform)?;
        Some(layouter[uniform.ty].size)
    }

    #[test]
    fn shaders_compile_and_match_their_uniforms() {
        assert_eq!(check(include_str!("shaders/waterfall.wgsl")), (WF_UNIFORMS * 4) as u32);
        assert_eq!(check(include_str!("shaders/surface.wgsl")), (S3_UNIFORMS * 4) as u32);
        assert_eq!(validate(&format!("{S3_COMMON}\n{FIELD}")), Some(S3L_BYTES as u32));
        assert_eq!(validate(&format!("{S3_COMMON}\n{SURFACE_LIT}")), Some(S3L_BYTES as u32));
        assert_eq!(validate(MIPS), Some(MIP_BYTES as u32));
        assert_eq!(validate(BLIT), None);
        assert_eq!(validate(TRACE), Some(TRACE_BYTES as u32));
    }

    #[test]
    fn the_trace_reads_the_columns_where_the_spectrum_writes_them() {
        assert!(
            TRACE.contains(&format!("TRACE_OFF: u32 = {}u;", super::super::gpu_fft::TRACE_OFF))
        );
        assert!(TRACE.contains(&format!("PEAK_OFF: u32 = {}u;", super::super::gpu_fft::PEAK_OFF)));
    }

    #[test]
    fn mip_chains_stop_at_a_few_dozen_texels() {
        assert_eq!(lods(16_384), 10);
        assert_eq!(16_384 >> (lods(16_384) - 1), MIP_FLOOR);
        assert_eq!(lods(32), 1);
    }
}
