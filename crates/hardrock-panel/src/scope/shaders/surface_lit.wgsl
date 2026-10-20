// The 3D surface with SDR Console's fluidity: SDRoxide's shape (rows
// receding and narrowing, curtains, fog), heights and normals from the
// field pass, lit per pixel, drawn into a multisampled target.

@group(0) @binding(1) var<storage, read> nrm: array<vec4<f32>>;
@group(0) @binding(2) var lut_tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) base: vec3<f32>,
    @location(1) normal: vec3<f32>,
    // How much the light shows here (0 on curtains and lines), and fog.
    @location(2) lit: f32,
    @location(3) fog: f32,
};

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

fn target_colour(c: vec3<f32>) -> vec3<f32> {
    return select(c, srgb_to_linear(c), u.srgb > 0.5);
}

fn at(k: u32, c: u32) -> vec4<f32> {
    return nrm[k * u32(u.cols) + c];
}

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

fn row_of(ii: u32) -> u32 {
    return u32(u.count) - 1u - ii;
}

fn palette(lvl: f32) -> vec3<f32> {
    return textureLoad(lut_tex, vec2<i32>(clamp(i32(lvl * 255.0 + 0.5), 0, 255), 0), 0).rgb;
}

@vertex
fn vs_fill(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VsOut {
    let k = row_of(ii);
    let seg = vi / 12u;
    let j = vi % 12u;
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
            which = 0u;
            col = seg;
        } else {
            kk = k + 1u;
        }
    }
    let p = at(kk, col);
    var out: VsOut;
    out.pos = place(kk, col, p.x, which == 1u);
    out.normal = p.yzw;
    out.fog = fog_of(kk);
    if (u.solid > 0.5) {
        out.base = palette(p.x);
        out.lit = select(u.light, 0.0, which == 1u);
        if (which == 1u) {
            // The curtain falls away to almost black, which is what
            // separates one row from the one behind without a line.
            out.fog = out.fog * 0.10;
        }
    } else {
        let lift = 14.0 / 255.0 * depth_of(kk);
        out.base = u.bg.rgb + vec3<f32>(lift);
        out.fog = 1.0;
        out.lit = 0.0;
    }
    return out;
}

@vertex
fn vs_line(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VsOut {
    let k = row_of(ii);
    let p = at(k, vi);
    var out: VsOut;
    out.pos = place(k, vi, p.x, false);
    let t = 0.35 + 0.65 * fog_of(k);
    out.base = mix(u.bg.rgb, u.ink.rgb, t);
    out.normal = p.yzw;
    out.fog = 1.0;
    out.lit = 0.0;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Light from high on the left and in front, a little specular.
    let l = normalize(vec3<f32>(-0.35, 0.85, -0.40));
    let v = normalize(vec3<f32>(0.0, 0.6, -1.0));
    let n = normalize(in.normal);
    let diff = max(dot(n, l), 0.0);
    let spec = pow(max(dot(n, normalize(l + v)), 0.0), 32.0);
    let shade = 0.35 + 0.75 * diff;
    var c = in.base * mix(1.0, shade, in.lit) + vec3<f32>(spec * 0.30 * in.lit);
    c = c * in.fog;
    return vec4<f32>(target_colour(c), 1.0);
}
