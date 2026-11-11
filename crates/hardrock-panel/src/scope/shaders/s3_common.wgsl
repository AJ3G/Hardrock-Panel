// Shared by the lit 3D surface's compute and render passes (prepended to
// each): its settings, and where each row stands.

struct S3L {
    view_lo: f32,
    view_hi: f32,
    floor: f32,
    inv_range: f32,
    // The live row's slot, how many rows are drawn, how far the rows have
    // glided since the last one was laid down, and columns across.
    live: f32,
    count: f32,
    carry: f32,
    cols: f32,
    tex_w: f32,
    tex_h: f32,
    solid: f32,
    srgb: f32,
    depth_frac: f32,
    persp: f32,
    fog: f32,
    fill: f32,
    bg: vec4<f32>,
    ink: vec4<f32>,
    // How far the front row has come from the previous spectrum to the
    // newest (0 to 1), the previous one's band, the mip levels, how much
    // lighting, and whether there is a previous spectrum.
    live_t: f32,
    prev_lo: f32,
    prev_hi: f32,
    lods: f32,
    light: f32,
    has_prev: f32,
    _a: f32,
    _b: f32,
    // The band's quiet level and spread, dB, for the made-up noise where a
    // row has no data (noise.wgsl), and the live row's number (to 2^24).
    noise: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: S3L;

const DEPTH: f32 = 48.0;

// Depth 0 (front) to 1 (back) of the row `k` rows behind the live one.
fn depth_of(k: u32) -> f32 {
    if (k == 0u) {
        return 0.0;
    }
    return min((f32(k) - 1.0 + u.carry) / DEPTH, 1.0);
}

fn slot_of(k: u32) -> u32 {
    let h = u32(u.tex_h);
    return (u32(u.live) + h - (k % h)) % h;
}
