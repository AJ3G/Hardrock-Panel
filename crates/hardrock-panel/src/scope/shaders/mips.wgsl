// The 3D rows' max-mip chain: each texel of a level is the stronger of the
// two under it, so a one-bin carrier survives to the coarsest level. The
// rows are the layers of an array texture one texel high, so a level
// halves only across frequency. Only the rows written this frame (`slots`)
// are rebuilt.

struct Rows {
    n: u32,
    _a: u32,
    _b: u32,
    _c: u32,
    slots: array<vec4<u32>, 4>,
};

@group(0) @binding(0) var<uniform> rows: Rows;
@group(0) @binding(1) var src_lvl: texture_2d_array<f32>;
@group(0) @binding(2) var dst_lvl: texture_storage_2d_array<r32float, write>;

@compute @workgroup_size(64)
fn mip(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = g.x;
    let i = g.y;
    if (i >= rows.n || x >= textureDimensions(dst_lvl).x) {
        return;
    }
    let r = i32(rows.slots[i / 4u][i % 4u]);
    let a = textureLoad(src_lvl, vec2<i32>(2 * i32(x), 0), r, 0).r;
    let b = textureLoad(src_lvl, vec2<i32>(2 * i32(x) + 1, 0), r, 0).r;
    textureStore(dst_lvl, vec2<i32>(i32(x), 0), r, vec4<f32>(max(a, b), 0.0, 0.0, 1.0));
}
