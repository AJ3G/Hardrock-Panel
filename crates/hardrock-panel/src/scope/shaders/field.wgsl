// The lit 3D surface's height field, once a frame: every row's level at
// every column (max-mipmapped across frequency, so any zoom is sampled
// without aliasing or a carrier dropping out), the front row moving
// smoothly from the previous spectrum to the newest, then each point's
// normal for the lighting. The vertex shader only reads the result.

// The rows, one layer each (one texel high), and the previous live row.
@group(0) @binding(1) var rows_tex: texture_2d_array<f32>;
@group(0) @binding(2) var geom_tex: texture_2d<f32>;
@group(0) @binding(3) var prev_tex: texture_2d<f32>;
@group(0) @binding(4) var<storage, read_write> lv: array<f32>;
@group(0) @binding(5) var<storage, read_write> nrm: array<vec4<f32>>;

const EMPTY: f32 = -1.0e9;

// Texel `x` at mip level `l` of row `r`, or of the previous live row.
fn ld(prev: bool, r: i32, x: i32, l: i32) -> f32 {
    if (prev) {
        return textureLoad(prev_tex, vec2<i32>(x, 0), l).r;
    }
    return textureLoad(rows_tex, vec2<i32>(x, 0), r, l).r;
}

// Row `r` (or the previous live row) over the band lo..hi, Hz, sampled over
// [f - half, f + half]: at a level of the max-mip chain where that span is
// one to two texels, the strongest texel it touches; narrower than a
// texel, interpolated.
fn sample(prev: bool, r: i32, lo: f32, hi: f32, f: f32, half: f32) -> f32 {
    let span = hi - lo;
    if (span <= 0.0) {
        return EMPTY;
    }
    let scale = u.tex_w / span;
    let uc = (f - lo) * scale;
    let w = 2.0 * half * scale;
    if (uc + 0.5 * w < 0.0 || uc - 0.5 * w > u.tex_w) {
        return EMPTY;
    }
    let lods = i32(u.lods);
    if (w <= 1.0 || lods <= 1) {
        let last = i32(u.tex_w) - 1;
        let x = uc - 0.5;
        let i0 = clamp(i32(floor(x)), 0, last);
        let i1 = min(i0 + 1, last);
        let fr = clamp(x - floor(x), 0.0, 1.0);
        if (w > 1.0) {
            // No mips (a GPU without compute never gets here): the
            // strongest of up to 96 texels.
            let a = clamp(i32(floor(uc - 0.5 * w)), 0, last);
            let b = clamp(i32(ceil(uc + 0.5 * w)) - 1, 0, last);
            let stride = max(1, (b - a + 96) / 96);
            var m = EMPTY;
            for (var i = a; i <= b; i = i + stride) {
                m = max(m, ld(prev, r, i, 0));
            }
            return m;
        }
        return mix(ld(prev, r, i0, 0), ld(prev, r, i1, 0), fr);
    }
    let l = clamp(i32(ceil(log2(w))) - 1, 0, lods - 1);
    let s = exp2(f32(l));
    let wl = max(i32(u.tex_w) >> u32(l), 1);
    let a = clamp(i32(floor((uc - 0.5 * w) / s)), 0, wl - 1);
    let b = clamp(i32(floor((uc + 0.5 * w) / s)), 0, wl - 1);
    var m = EMPTY;
    for (var i = a; i <= b; i = i + 1) {
        m = max(m, ld(prev, r, i, l));
    }
    return m;
}

fn to_level(v: f32) -> f32 {
    if (v < -1.0e8) {
        return 0.0;
    }
    return clamp((v - u.floor) * u.inv_range, 0.0, 1.0);
}

@compute @workgroup_size(64)
fn levels(@builtin(global_invocation_id) g: vec3<u32>) {
    let c = g.x;
    let k = g.y;
    let cols = u32(u.cols);
    if (c >= cols || k >= u32(u.count)) {
        return;
    }
    let x = f32(c) / max(u.cols - 1.0, 1.0);
    let w = u.view_hi - u.view_lo;
    let f = u.view_lo + x * w;
    let half = w / max(u.cols - 1.0, 1.0) * 0.5;
    let slot = slot_of(k);
    let gb = textureLoad(geom_tex, vec2<i32>(i32(slot), 0), 0).xy;
    var lvl = to_level(sample(false, i32(slot), gb.x, gb.y, f, half));
    if (k == 0u && u.has_prev > 0.5) {
        let was = to_level(sample(true, 0, u.prev_lo, u.prev_hi, f, half));
        lvl = mix(was, lvl, clamp(u.live_t, 0.0, 1.0));
    }
    lv[k * cols + c] = lvl;
}

@compute @workgroup_size(64)
fn normals(@builtin(global_invocation_id) g: vec3<u32>) {
    let c = g.x;
    let k = g.y;
    let cols = u32(u.cols);
    let count = u32(u.count);
    if (c >= cols || k >= count) {
        return;
    }
    let row = k * cols;
    let h = lv[row + c];
    // Slopes over a few columns either side and a row either way: the
    // surface's shape, not each bin's noise.
    let reach = 4u;
    let cl = select(0u, c - reach, c >= reach);
    let cr = min(c + reach, cols - 1u);
    let sx = (lv[row + cr] - lv[row + cl]) / f32(max(cr - cl, 1u));
    var hf = h;
    var hb = h;
    if (k > 0u) {
        hf = lv[row - cols + c];
    }
    if (k + 1u < count) {
        hb = lv[row + cols + c];
    }
    let sz = (hb - hf) * 0.5;
    let n = normalize(vec3<f32>(-sx * 60.0, 1.0, -sz * 6.0));
    nrm[row + c] = vec4<f32>(h, n);
}
