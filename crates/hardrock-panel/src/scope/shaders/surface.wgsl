// The 3D surface, after SDRoxide's widgets/spectrum3d.rs: the live spectrum
// across the front, older rows receding and narrowing behind it, drawn back
// to front so a near ridge hides the far ones (the painter's algorithm is the
// depth test). Each row is a curtain from its crest down to its own baseline,
// joined to the crest of the row behind by a strip of surface.

struct S3 {
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
};

@group(0) @binding(0) var<uniform> u: S3;

const DEPTH: f32 = 48.0;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) colour: vec3<f32>,
};

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

// The level, 0..1 of the scale, of row `k` at column `c`.
fn level(k: u32, c: u32) -> f32 {
    let x = f32(c) / max(u.cols - 1.0, 1.0);
    let w = u.view_hi - u.view_lo;
    let f = u.view_lo + x * w;
    let half = w / max(u.cols - 1.0, 1.0) * 0.5;
    let v = level_at(slot_of(k), f, half, u.tex_w, true, -1.0e9);
    if (v < -1.0e8) {
        return 0.0;
    }
    return clamp((v - u.floor) * u.inv_range, 0.0, 1.0);
}

// Where row `k`'s column `c` stands, crest (foot = false) or foot, in the
// viewport's clip space.
fn place(k: u32, c: u32, lvl: f32, foot: bool) -> vec4<f32> {
    let d = depth_of(k);
    let s = 1.0 / (1.0 + u.persp * d);
    let far = 1.0 / (1.0 + u.persp);
    let depth_h = u.depth_frac / u.fill;
    let amp_h = (1.0 - u.depth_frac) / u.fill;
    let base_y = 1.0 - depth_h * (1.0 - s) / (1.0 - far);
    let x_lin = f32(c) / max(u.cols - 1.0, 1.0);
    let x = 0.5 + (x_lin - 0.5) * s;
    var y = base_y - lvl * amp_h * s;
    if (foot) {
        y = base_y;
    }
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - 2.0 * y, 0.0, 1.0);
}

fn fog_of(k: u32) -> f32 {
    return 1.0 - u.fog * depth_of(k);
}

// Instance `ii` is drawn `ii`th, so the farthest row is instance 0.
fn row_of(ii: u32) -> u32 {
    return u32(u.count) - 1u - ii;
}

@vertex
fn vs_fill(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VsOut {
    let k = row_of(ii);
    let seg = vi / 12u;
    let j = vi % 12u;
    // Two triangles of surface (behind crest to own crest), then two of
    // curtain (own crest to own foot). which: 0 own crest, 1 own foot,
    // 2 crest of the row behind.
    var col = seg;
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
    var kk = k;
    if (which == 2u) {
        if (k + 1u >= u32(u.count)) {
            // The farthest row has nothing behind it: no surface strip.
            which = 0u;
            col = seg;
        } else {
            kk = k + 1u;
        }
    }
    let lvl = level(kk, col);
    var out: VsOut;
    out.pos = place(kk, col, lvl, which == 1u);
    let srgb = u.srgb > 0.5;
    let fog = fog_of(kk);
    if (u.solid > 0.5) {
        var c = textureLoad(lut_tex, vec2<i32>(clamp(i32(lvl * 255.0 + 0.5), 0, 255), 0), 0).rgb;
        // The curtain falls away to almost black, which is what separates
        // one row from the one behind without a line between them.
        var f = fog;
        if (which == 1u) {
            f = fog * 0.10;
        }
        out.colour = target_colour(c * f, srgb);
    } else {
        // The background, lifting a shade towards the back.
        let lift = 14.0 / 255.0 * depth_of(kk);
        out.colour = target_colour(u.bg.rgb + vec3<f32>(lift), srgb);
    }
    return out;
}

@vertex
fn vs_line(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VsOut {
    let k = row_of(ii);
    let lvl = level(k, vi);
    var out: VsOut;
    out.pos = place(k, vi, lvl, false);
    let t = 0.35 + 0.65 * fog_of(k);
    out.colour = target_colour(mix(u.bg.rgb, u.ink.rgb, t), u.srgb > 0.5);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.colour, 1.0);
}
