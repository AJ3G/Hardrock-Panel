// The waterfall: newest row at the top, one history row per `rows_vis`th of
// the height, sliding down smoothly between rows.

struct Wf {
    view_lo: f32,
    view_hi: f32,
    floor: f32,
    inv_range: f32,
    newest: f32,
    rows_vis: f32,
    count: f32,
    px_w: f32,
    tex_w: f32,
    tex_h: f32,
    smooth: f32,
    srgb: f32,
    // How far the newest row has slid in, 0 to 1.
    scroll_frac: f32,
    contrast: f32,
    brightness: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> u: Wf;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One triangle over the callback's viewport.
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var out: VsOut;
    let x = f32(i32(vi & 1u) * 4 - 1);
    let y = f32(i32(vi >> 1u) * 4 - 1);
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let srgb = u.srgb > 0.5;
    let empty = palette(0.0, srgb);
    let age_f = in.uv.y * u.rows_vis + (1.0 - u.scroll_frac);
    if (age_f >= u.count) {
        return vec4<f32>(empty, 1.0);
    }
    let h = u32(u.tex_h);
    let age = u32(floor(age_f));
    let row = (u32(u.newest) + h - (age % h)) % h;
    let f = mix(u.view_lo, u.view_hi, in.uv.x);
    let half = (u.view_hi - u.view_lo) / max(u.px_w, 1.0) * 0.5;
    let v = level_at(row, f, half, u.tex_w, u.smooth > 0.5, -1.0e9);
    if (v < -1.0e8) {
        return vec4<f32>(empty, 1.0);
    }
    var t = (v - u.floor) * u.inv_range;
    t = (t - 0.5) * u.contrast + 0.5 + u.brightness;
    return vec4<f32>(palette(clamp(t, 0.0, 1.0), srgb), 1.0);
}
