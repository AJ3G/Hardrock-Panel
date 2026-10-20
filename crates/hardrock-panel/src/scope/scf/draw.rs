//! Drawing the (f, α) plane: an `egui_wgpu` paint callback whose vertex
//! shader reads the engine's grid buffer directly (shaders/scf_view.wgsl),
//! as a terrain or a heat map. Nothing about the grid goes through the CPU.
//!
//! Its pipelines and per-scope bind groups live in egui's callback
//! resources under their own type, beside (not inside) the waterfall's, so
//! the scope's GPU code can change without this.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use eframe::egui;
use eframe::egui_wgpu::{
    self, CallbackResources, CallbackTrait, RenderState, ScreenDescriptor, wgpu,
};

use super::super::palette::Palette;

const VIEW: &str = include_str!("shaders/scf_view.wgsl");

/// The terrain's perspective: the 3D FFT view's (SDRoxide's spectrum3d).
pub const DEPTH_FRAC: f32 = 0.46;
pub const PERSP: f32 = 0.85;
pub const FOG: f32 = 0.55;
pub const FILL: f32 = DEPTH_FRAC + (1.0 - DEPTH_FRAC) / (1.0 + PERSP);

/// Where a point on the front plane at `x_lin` (0..1 across) lands at depth
/// `d` (0 front, 1 back), and the floor there, as shares of the rectangle:
/// for the axes drawn over the terrain.
pub fn project(x_lin: f32, d: f32) -> (f32, f32) {
    let s = 1.0 / (1.0 + PERSP * d);
    let far = 1.0 / (1.0 + PERSP);
    let depth_h = DEPTH_FRAC / FILL;
    (0.5 + (x_lin - 0.5) * s, 1.0 - depth_h * (1.0 - s) / (1.0 - far))
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct View3 {
    grid_f: u32,
    grid_a: u32,
    srgb: u32,
    _pad0: u32,
    floor: f32,
    inv_range: f32,
    depth_frac: f32,
    persp: f32,
    fog: f32,
    fill: f32,
    _pad1: f32,
    _pad2: f32,
}

pub const VIEW_BYTES: usize = std::mem::size_of::<View3>();

/// How the plane is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Terrain,
    Heat,
}

struct Pipelines {
    layout: wgpu::BindGroupLayout,
    terrain: wgpu::RenderPipeline,
    heat: wgpu::RenderPipeline,
    srgb: bool,
}

struct PerScope {
    uni: wgpu::Buffer,
    lut: wgpu::Texture,
    palette: Option<Palette>,
    /// The bind group for grid buffer `version`.
    bind: Option<(u64, wgpu::BindGroup)>,
}

struct Registry {
    pipes: Pipelines,
    per: HashMap<u64, PerScope>,
}

/// Compile the pipelines (once, at start-up; a second call is harmless).
pub fn init(rs: &RenderState) {
    if rs.renderer.read().callback_resources.get::<Registry>().is_some() {
        return;
    }
    let d = &rs.device;
    let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("scf-view-bgl"),
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
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ],
    });
    let pl = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("scf-view-pl"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let module = d.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("scf-view"),
        source: wgpu::ShaderSource::Wgsl(VIEW.into()),
    });
    let pipeline = |label, vs, fs| {
        d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some(vs),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some(fs),
                targets: &[Some(rs.target_format.into())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        })
    };
    let pipes = Pipelines {
        terrain: pipeline("scf-terrain", "vs_terrain", "fs_terrain"),
        heat: pipeline("scf-heat", "vs_heat", "fs_heat"),
        layout,
        srgb: rs.target_format.is_srgb(),
    };
    rs.renderer.write().callback_resources.insert(Registry { pipes, per: HashMap::new() });
}

/// Free one scope's resources.
pub fn retire(rs: &RenderState, id: u64) {
    if let Some(r) = rs.renderer.write().callback_resources.get_mut::<Registry>() {
        r.per.remove(&id);
    }
}

/// One repaint of the plane.
pub struct PlanePaint {
    pub id: u64,
    pub grid: wgpu::Buffer,
    /// Changes whenever `grid` is a new buffer.
    pub version: u64,
    pub grid_f: u32,
    pub grid_a: u32,
    pub look: Look,
    pub palette: Palette,
    /// The level scale, dB.
    pub floor: f32,
    pub ceil: f32,
}

impl CallbackTrait for PlanePaint {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        res: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(reg) = res.get_mut::<Registry>() else { return Vec::new() };
        let Registry { pipes, per } = reg;
        let ps = per.entry(self.id).or_insert_with(|| PerScope {
            uni: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("scf-view-uniforms"),
                size: VIEW_BYTES as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            lut: device.create_texture(&wgpu::TextureDescriptor {
                label: Some("scf-lut"),
                size: wgpu::Extent3d { width: 256, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            }),
            palette: None,
            bind: None,
        });
        if ps.palette != Some(self.palette) {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &ps.lut,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &self.palette.lut(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256 * 4),
                    rows_per_image: None,
                },
                wgpu::Extent3d { width: 256, height: 1, depth_or_array_layers: 1 },
            );
            ps.palette = Some(self.palette);
        }
        if ps.bind.as_ref().map(|b| b.0) != Some(self.version) {
            let lut_view = ps.lut.create_view(&Default::default());
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("scf-view-bg"),
                layout: &pipes.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: ps.uni.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: self.grid.as_entire_binding() },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&lut_view),
                    },
                ],
            });
            ps.bind = Some((self.version, bg));
        }
        let u = View3 {
            grid_f: self.grid_f,
            grid_a: self.grid_a,
            srgb: pipes.srgb as u32,
            _pad0: 0,
            floor: self.floor,
            inv_range: 1.0 / (self.ceil - self.floor).max(1.0),
            depth_frac: DEPTH_FRAC,
            persp: PERSP,
            fog: FOG,
            fill: FILL,
            _pad1: 0.0,
            _pad2: 0.0,
        };
        queue.write_buffer(&ps.uni, 0, bytemuck::bytes_of(&u));
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        res: &CallbackResources,
    ) {
        let Some(reg) = res.get::<Registry>() else { return };
        let Some((_, bg)) = reg.per.get(&self.id).and_then(|p| p.bind.as_ref()) else { return };
        if self.grid_f < 2 || self.grid_a < 2 {
            return;
        }
        pass.set_bind_group(0, bg, &[]);
        match self.look {
            Look::Terrain => {
                pass.set_pipeline(&reg.pipes.terrain);
                pass.draw(0..(self.grid_f - 1) * 12, 0..self.grid_a);
            }
            Look::Heat => {
                pass.set_pipeline(&reg.pipes.heat);
                pass.draw(0..3, 0..1);
            }
        }
    }
}

/// The paint callback as an egui shape over `rect`.
pub fn shape(rect: egui::Rect, cb: PlanePaint) -> egui::Shape {
    egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(rect, cb))
}
