// Shared by the waterfall and the 3D surface (prepended to each).
//
// A history row is one spectrum in dBm, `tex_w` half floats over the band
// stored for it in `geom` (lo, hi, Hz from the reference). Any view can be
// drawn from any row: the row's own band says where its columns fall.

@group(0) @binding(1) var rows_tex: texture_2d<f32>;
@group(0) @binding(2) var geom_tex: texture_2d<f32>;
@group(0) @binding(3) var lut_tex: texture_2d<f32>;

// The level over [f - half, f + half] (Hz from the reference) in history row
// `row`: the strongest column it covers, so a one-bin carrier never falls
// between pixels; or, when a pixel is narrower than a column, the column
// under it, interpolated if `smoothing`. `empty` when the row has nothing there.
fn level_at(row: u32, f: f32, half: f32, tex_w: f32, smoothing: bool, empty: f32) -> f32 {
    let g = textureLoad(geom_tex, vec2<i32>(i32(row), 0), 0).xy;
    let span = g.y - g.x;
    if (span <= 0.0) {
        return empty;
    }
    let scale = tex_w / span;
    let u0 = (f - half - g.x) * scale;
    let u1 = (f + half - g.x) * scale;
    if (u1 <= 0.0 || u0 >= tex_w) {
        return empty;
    }
    let last = i32(tex_w) - 1;
    let r = i32(row);
    if (u1 - u0 > 1.0) {
        let a = clamp(i32(floor(u0)), 0, last);
        let b = clamp(i32(ceil(u1)) - 1, 0, last);
        // At most 96 loads a pixel; wider than that, every n-th column.
        let n = b - a + 1;
        let stride = max(1, (n + 95) / 96);
        var m = -1.0e9;
        var i = a;
        loop {
            if (i > b) { break; }
            m = max(m, textureLoad(rows_tex, vec2<i32>(i, r), 0).r);
            i = i + stride;
        }
        return m;
    }
    if (smoothing) {
        let x = (f - g.x) * scale - 0.5;
        let i0 = clamp(i32(floor(x)), 0, last);
        let i1 = min(i0 + 1, last);
        let t = clamp(x - floor(x), 0.0, 1.0);
        let v0 = textureLoad(rows_tex, vec2<i32>(i0, r), 0).r;
        let v1 = textureLoad(rows_tex, vec2<i32>(i1, r), 0).r;
        return mix(v0, v1, t);
    }
    let i = clamp(i32(floor((f - g.x) * scale)), 0, last);
    return textureLoad(rows_tex, vec2<i32>(i, r), 0).r;
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

// The palette's own (sRGB) colour at t in 0..1.
fn palette_raw(t: f32) -> vec3<f32> {
    let i = clamp(i32(t * 255.0 + 0.5), 0, 255);
    return textureLoad(lut_tex, vec2<i32>(i, 0), 0).rgb;
}

// The palette at t in 0..1, as the target wants it.
fn palette(t: f32, srgb: bool) -> vec3<f32> {
    return target_colour(palette_raw(t), srgb);
}

fn target_colour(c: vec3<f32>, srgb: bool) -> vec3<f32> {
    return select(c, srgb_to_linear(c), srgb);
}
