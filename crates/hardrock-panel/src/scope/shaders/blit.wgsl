// The 3D surface's resolved image into egui's pass, texel for pixel.

@group(0) @binding(0) var img: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

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
    let d = vec2<f32>(textureDimensions(img));
    let p = clamp(vec2<i32>(in.uv * d), vec2<i32>(0), vec2<i32>(d) - vec2<i32>(1));
    return vec4<f32>(textureLoad(img, p, 0).rgb, 1.0);
}
