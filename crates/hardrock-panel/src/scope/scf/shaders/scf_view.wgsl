// The (f, α) plane, drawn straight from the SCF engine's grid buffer: as a
// terrain (f across, α receding, the level lifting the surface, in the 3D
// FFT view's perspective and palette), or as a flat heat map (f across, α
// up). The grid is `grid_a` rows of `grid_f` levels, dB.

struct View3 {
    grid_f: u32,
    grid_a: u32,
    // 1: the target is sRGB (colours are converted to linear).
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
};

@group(0) @binding(0) var<uniform> u: View3;
@group(0) @binding(1) var<storage, read> grid: array<f32>;
@group(0) @binding(2) var lut_tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) colour: vec3<f32>,
    @location(1) uv: vec2<f32>,
};

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

fn palette(t: f32) -> vec3<f32> {
    let i = clamp(i32(t * 255.0 + 0.5), 0, 255);
    let c = textureLoad(lut_tex, vec2<i32>(i, 0), 0).rgb;
    return select(c, srgb_to_linear(c), u.srgb == 1u);
}

// The level of row `a` (α), column `f`, 0..1 of the scale.
fn level(a: u32, f: u32) -> f32 {
    let v = grid[a * u.grid_f + f];
    return clamp((v - u.floor) * u.inv_range, 0.0, 1.0);
}

fn depth_of(a: u32) -> f32 {
    return f32(a) / max(f32(u.grid_a) - 1.0, 1.0);
}

// Row `a`, column `f` at level `lvl` (or its foot), in clip space: the same
// projection as the 3D FFT surface, the front row at the bottom full width,
// the back row narrower and higher.
fn place(a: u32, f: u32, lvl: f32, foot: bool) -> vec4<f32> {
    let d = depth_of(a);
    let s = 1.0 / (1.0 + u.persp * d);
    let far = 1.0 / (1.0 + u.persp);
    let depth_h = u.depth_frac / u.fill;
    let amp_h = (1.0 - u.depth_frac) / u.fill;
    let base_y = 1.0 - depth_h * (1.0 - s) / (1.0 - far);
    let x_lin = f32(f) / max(f32(u.grid_f) - 1.0, 1.0);
    let x = 0.5 + (x_lin - 0.5) * s;
    var y = base_y - lvl * amp_h * s;
    if (foot) {
        y = base_y;
    }
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - 2.0 * y, 0.0, 1.0);
}

// Instance ii is the ii-th row from the back: the painter's algorithm, in
// one instanced draw. Each row is a strip of surface from the row behind's
// crest to its own, then a curtain down to its own foot.
@vertex
fn vs_terrain(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VsOut {
    let a = u.grid_a - 1u - ii;
    let seg = vi / 12u;
    let j = vi % 12u;
    var col = seg;
    // 0: own crest, 1: own foot, 2: the crest of the row behind.
    var which = 0u;
    switch j {
        case 0u: { col = seg; which = 2u; }
        case 1u: { col = seg + 1u; which = 2u; }
        case 2u: { col = seg; which = 0u; }
        case 3u: { col = seg; which = 0u; }
        case 4u: { col = seg + 1u; which = 2u; }
        case 5u: { col = seg + 1u; which = 0u; }
        case 6u: { col = seg; which = 0u; }
        case 7u: { col = seg; which = 1u; }
        case 8u: { col = seg + 1u; which = 0u; }
        case 9u: { col = seg + 1u; which = 0u; }
        case 10u: { col = seg; which = 1u; }
        default: { col = seg + 1u; which = 1u; }
    }
    var row = a;
    if (which == 2u) {
        if (a + 1u >= u.grid_a) {
            which = 0u;
            col = seg;
        } else {
            row = a + 1u;
        }
    }
    let lvl = level(row, col);
    var out: VsOut;
    out.pos = place(row, col, lvl, which == 1u);
    var f = 1.0 - u.fog * depth_of(row);
    if (which == 1u) {
        f = f * 0.10;
    }
    out.colour = palette(lvl) * f;
    out.uv = vec2<f32>(0.0);
    return out;
}

@fragment
fn fs_terrain(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.colour, 1.0);
}

// The heat map: one triangle over the rectangle.
@vertex
fn vs_heat(@builtin(vertex_index) vi: u32) -> VsOut {
    let p = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var out: VsOut;
    out.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(p.x, p.y);
    out.colour = vec3<f32>(0.0);
    return out;
}

@fragment
fn fs_heat(in: VsOut) -> @location(0) vec4<f32> {
    // uv.y is 0 at the bottom: α grows upward.
    let f = min(u32(in.uv.x * f32(u.grid_f)), u.grid_f - 1u);
    let a = min(u32(in.uv.y * f32(u.grid_a)), u.grid_a - 1u);
    return vec4<f32>(palette(level(a, f)), 1.0);
}
