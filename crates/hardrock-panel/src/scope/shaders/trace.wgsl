// The spectrum trace, drawn from the columns the GPU pooled from its own
// spectrum this frame: a fill under the trace fading towards the floor,
// the trace as a line of quads, and the peak-hold trace.

struct Tr {
    floor: f32,
    inv_range: f32,
    cols: f32,
    // Where the columns run across the rectangle, 0 to 1.
    x0: f32,
    x1: f32,
    w_px: f32,
    h_px: f32,
    line_w: f32,
    // Fill alpha at the top (0 for none), the line's alpha.
    fill_a: f32,
    line_a: f32,
    srgb: f32,
    _a: f32,
    accent: vec4<f32>,
    ink: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Tr;
@group(0) @binding(1) var<storage, read> outb: array<f32>;

const TRACE_OFF: u32 = 64u;
const PEAK_OFF: u32 = 8256u;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) colour: vec4<f32>,
};

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

// A colour at alpha `a`, premultiplied, as the target wants it.
fn premul(c: vec3<f32>, a: f32) -> vec4<f32> {
    return vec4<f32>(select(c, srgb_to_linear(c), u.srgb > 0.5) * a, a);
}

fn level(off: u32, i: u32) -> f32 {
    return clamp((outb[off + i] - u.floor) * u.inv_range, 0.0, 1.0);
}

// Column i's point, 0..1 across and down.
fn point(off: u32, i: u32) -> vec2<f32> {
    let x = u.x0 + (u.x1 - u.x0) * (f32(i) + 0.5) / u.cols;
    return vec2<f32>(x, 1.0 - level(off, i));
}

fn clip(p: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(p.x * 2.0 - 1.0, 1.0 - 2.0 * p.y, 0.0, 1.0);
}

@vertex
fn vs_fill(@builtin(vertex_index) vi: u32) -> VsOut {
    let i = vi / 2u;
    let p = point(TRACE_OFF, i);
    var out: VsOut;
    if ((vi & 1u) == 0u) {
        let t = 1.0 - p.y;
        out.pos = clip(p);
        out.colour = premul(u.accent.rgb, (0.03 + (0.45 - 0.03) * t) * u.fill_a);
    } else {
        out.pos = clip(vec2<f32>(p.x, 1.0));
        out.colour = premul(u.accent.rgb, 0.03 * u.fill_a);
    }
    return out;
}

// A line through the columns at `off` as quads `line_w` pixels wide.
fn line(off: u32, vi: u32, colour: vec4<f32>) -> VsOut {
    let seg = vi / 6u;
    let j = vi % 6u;
    let a = point(off, seg);
    let b = point(off, seg + 1u);
    let px = vec2<f32>(u.w_px, u.h_px);
    let d = (b - a) * px;
    let len = max(length(d), 1e-3);
    let nrm = vec2<f32>(-d.y, d.x) / len * (0.5 * u.line_w) / px;
    var p = a;
    var side = 1.0;
    switch j {
        case 0u: { p = a; side = 1.0; }
        case 1u: { p = a; side = -1.0; }
        case 2u: { p = b; side = 1.0; }
        case 3u: { p = b; side = 1.0; }
        case 4u: { p = a; side = -1.0; }
        default: { p = b; side = -1.0; }
    }
    var out: VsOut;
    out.pos = clip(p + nrm * side);
    out.colour = colour;
    return out;
}

@vertex
fn vs_line(@builtin(vertex_index) vi: u32) -> VsOut {
    return line(TRACE_OFF, vi, premul(u.accent.rgb, u.line_a));
}

@vertex
fn vs_peak(@builtin(vertex_index) vi: u32) -> VsOut {
    return line(PEAK_OFF, vi, premul(u.ink.rgb, 0.35));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.colour;
}
