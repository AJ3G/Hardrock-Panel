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

use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui;
use eframe::egui_wgpu::{
    self, CallbackResources, CallbackTrait, RenderState, ScreenDescriptor, wgpu,
};

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
        let widest = adapter.limits().max_texture_dimension_2d;
        wgpu::DeviceDescriptor {
            label: Some("panel wgpu device"),
            required_limits: wgpu::Limits {
                max_texture_dimension_2d: widest.min(MAX_TEX_W),
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
    let tex_w = rs.device.limits().max_texture_dimension_2d.min(MAX_TEX_W);
    let adapter: &'static str = Box::leak(rs.adapter.get_info().name.into_boxed_str());
    let info = GpuInfo { tex_w, adapter };
    if rs.renderer.read().callback_resources.get::<Registry>().is_some() {
        return info;
    }
    let shared = Shared::new(&rs.device, rs.target_format);
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
}

const COMMON: &str = include_str!("shaders/common.wgsl");

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
}

fn texture(
    device: &wgpu::Device,
    label: &str,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

impl Resources {
    fn new(device: &wgpu::Device, shared: &Shared, tex_w: u32) -> Resources {
        use wgpu::TextureFormat as F;
        let wf_rows = texture(device, "scope-wf-rows", tex_w, WF_ROWS, F::R16Float);
        let wf_geom = texture(device, "scope-wf-geom", WF_ROWS, 1, F::Rg32Float);
        let s3_rows = texture(device, "scope-s3-rows", tex_w, SURFACE_ROWS, F::R16Float);
        let s3_geom = texture(device, "scope-s3-geom", SURFACE_ROWS, 1, F::Rg32Float);
        let lut = texture(device, "scope-lut", 256, 1, F::Rgba8Unorm);
        let buf = |label, size| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let wf_uni = buf("scope-wf-uniforms", (WF_UNIFORMS * 4) as u64);
        let s3_uni = buf("scope-s3-uniforms", (S3_UNIFORMS * 4) as u64);
        let lut_view = lut.create_view(&Default::default());
        let bg = |label, uni: &wgpu::Buffer, rows: &wgpu::Texture, geom: &wgpu::Texture| {
            let rv = rows.create_view(&Default::default());
            let gv = geom.create_view(&Default::default());
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &shared.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: uni.as_entire_binding() },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&rv),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&gv),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&lut_view),
                    },
                ],
            })
        };
        let wf_bg = bg("scope-wf-bg", &wf_uni, &wf_rows, &wf_geom);
        let s3_bg = bg("scope-s3-bg", &s3_uni, &s3_rows, &s3_geom);
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
        }
    }

    /// Write this repaint's rows, once, whichever callback gets here first.
    fn apply(&mut self, queue: &wgpu::Queue, up: &Upload) {
        if up.seq <= self.applied {
            return;
        }
        self.applied = up.seq;
        if up.clear {
            let zeros = vec![0u8; WF_ROWS as usize * 8];
            write(queue, &self.wf_geom, 0, 0, WF_ROWS, &zeros, WF_ROWS * 8);
            write(queue, &self.s3_geom, 0, 0, SURFACE_ROWS, &zeros, SURFACE_ROWS * 8);
        }
        for (rows, geom, list) in
            [(&self.wf_rows, &self.wf_geom, &up.wf), (&self.s3_rows, &self.s3_geom, &up.surface)]
        {
            for r in list {
                if r.bins.len() != self.tex_w as usize {
                    continue;
                }
                let bytes: &[u8] = bytemuck::cast_slice(&r.bins);
                write(queue, rows, 0, r.slot, self.tex_w, bytes, self.tex_w * 2);
                let g = [r.lo, r.hi];
                write(queue, geom, r.slot, 0, 1, bytemuck::cast_slice(&g), 8);
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
}

fn resources<'a>(
    device: &wgpu::Device,
    reg: &'a mut Registry,
    id: u64,
    tex_w: u32,
) -> &'a mut Resources {
    let Registry { shared, per } = reg;
    let r = per.entry(id).or_insert_with(|| Resources::new(device, shared, tex_w));
    if r.tex_w != tex_w {
        *r = Resources::new(device, shared, tex_w);
    }
    r
}

impl CallbackTrait for WaterfallPaint {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        res: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(reg) = res.get_mut::<Registry>() else { return Vec::new() };
        let srgb = reg.shared.srgb;
        let r = resources(device, reg, self.id, self.tex_w);
        if let Some(up) = &self.upload {
            r.apply(queue, up);
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
            if srgb { 1.0 } else { 0.0 },
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

impl CallbackTrait for SurfacePaint {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        res: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(reg) = res.get_mut::<Registry>() else { return Vec::new() };
        let srgb = reg.shared.srgb;
        let r = resources(device, reg, self.id, self.tex_w);
        if let Some(up) = &self.upload {
            r.apply(queue, up);
        }
        r.set_palette(queue, self.palette);
        let p = &self.params;
        let u: [f32; S3_UNIFORMS] = [
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
        ];
        queue.write_buffer(&r.s3_uni, 0, bytemuck::cast_slice(&u));
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
        let p = &self.params;
        if p.count == 0 || p.cols < 2 {
            return;
        }
        pass.set_bind_group(0, &r.s3_bg, &[]);
        let fill_verts = (p.cols - 1) * 12;
        if p.solid {
            pass.set_pipeline(&reg.shared.fill);
            pass.draw(0..fill_verts, 0..p.count);
        } else {
            // Back to front, each row's fill then its line, so a near row's
            // fill hides the lines behind it.
            for ii in 0..p.count {
                pass.set_pipeline(&reg.shared.fill);
                pass.draw(0..fill_verts, ii..ii + 1);
                pass.set_pipeline(&reg.shared.line);
                pass.draw(0..p.cols, ii..ii + 1);
            }
        }
    }
}

/// A paint callback for egui.
pub fn callback(rect: egui::Rect, cb: impl CallbackTrait + 'static) -> egui::Shape {
    egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(rect, cb))
}
