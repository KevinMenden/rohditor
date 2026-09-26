struct Params {
    image: vec4<u32>, // source tile width, tile height, columns, unused
    region: vec4<u32>, // global x/y, width/height
};
@group(0) @binding(0) var red: texture_2d_array<f32>;
@group(0) @binding(1) var green: texture_2d_array<f32>;
@group(0) @binding(2) var blue: texture_2d_array<f32>;
@group(0) @binding(3) var<storage, read_write> rgb: array<f32>;
@group(0) @binding(4) var<uniform> p: Params;

@compute @workgroup_size(8, 8)
fn load_resident(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= p.region.zw) { return; }
    let pos = p.region.xy + id.xy;
    let local = vec2<i32>(pos % p.image.xy);
    let layer = i32((pos.y / p.image.y) * p.image.z + pos.x / p.image.x);
    let index = (id.y * p.region.z + id.x) * 3u;
    rgb[index] = textureLoad(red, local, layer, 0).x;
    rgb[index + 1u] = textureLoad(green, local, layer, 0).x;
    rgb[index + 2u] = textureLoad(blue, local, layer, 0).x;
}
