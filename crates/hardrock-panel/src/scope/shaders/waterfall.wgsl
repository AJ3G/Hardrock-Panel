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
    smoothing: f32,
    srgb: f32,
    // How far the newest row has slid in, 0 to 1.
    scroll_frac: f32,
    contrast: f32,
    brightness: f32,
    // The band's quiet level and spread, dB, for the made-up noise where
    // there is no history (noise.wgsl), and the newest row's number (to 2^24).
    noise_dbm: f32,
    noise_spread: f32,
    serial: f32,
    _pad1: f32,
    _pad2: f32,
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
    let age_f = in.uv.y * u.rows_vis + (1.0 - u.scroll_frac);
    let age = u32(floor(age_f));
    let f = mix(u.view_lo, u.view_hi, in.uv.x);
    let half = (u.view_hi - u.view_lo) / max(u.px_w, 1.0) * 0.5;
    var v = -1.0e9;
    if (age_f < u.count) {
        let h = u32(u.tex_h);
        let row = (u32(u.newest) + h - (age % h)) % h;
        v = level_at(row, f, half, u.tex_w, u.smoothing > 0.5, -1.0e9);
    }
    // No history here (never measured, or not filled yet): made-up noise
    // like the quiet band's, coloured as a measured pixel is, rather than a
    // flat colour; real rows draw over it as they come.
    if (v < -1.0e8) {
        let n = (u32(u.serial) - age) & 0xffffffu;
        v = noise_dbm(n, f, 2.0 * half, u.noise_dbm, u.noise_spread);
    }
    var t = (v - u.floor) * u.inv_range;
    t = (t - 0.5) * u.contrast + 0.5 + u.brightness;
    return vec4<f32>(palette(clamp(t, 0.0, 1.0), srgb), 1.0);
}
