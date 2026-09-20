struct ScatterParameters {
    input_width: u32,
    input_x: u32,
    input_y: u32,
    core_left: u32,
    core_top: u32,
    core_width: u32,
    core_height: u32,
    tile_width: u32,
    tile_height: u32,
    tile_columns: u32,
    _padding0: u32,
    _padding1: u32,
};

@group(0) @binding(0) var<storage, read> captured_rgb: array<f32>;
@group(0) @binding(1) var resident_red: texture_storage_2d_array<r32float, write>;
@group(0) @binding(2) var resident_green: texture_storage_2d_array<r32float, write>;
@group(0) @binding(3) var resident_blue: texture_storage_2d_array<r32float, write>;
@group(0) @binding(4) var<uniform> scatter: ScatterParameters;

@compute @workgroup_size(16, 16, 1)
fn scatter_capture_core(@builtin(global_invocation_id) invocation: vec3<u32>) {
    if invocation.x >= scatter.core_width || invocation.y >= scatter.core_height {
        return;
    }
    let global = vec2<u32>(scatter.core_left, scatter.core_top) + invocation.xy;
    let input = vec2<u32>(global.x - scatter.input_x, global.y - scatter.input_y);
    let offset = (input.y * scatter.input_width + input.x) * 3u;
    let tile = global / vec2<u32>(scatter.tile_width, scatter.tile_height);
    let local = global % vec2<u32>(scatter.tile_width, scatter.tile_height);
    let layer = tile.y * scatter.tile_columns + tile.x;
    textureStore(resident_red, vec2<i32>(local), i32(layer), vec4<f32>(captured_rgb[offset], 0.0, 0.0, 0.0));
    textureStore(resident_green, vec2<i32>(local), i32(layer), vec4<f32>(captured_rgb[offset + 1u], 0.0, 0.0, 0.0));
    textureStore(resident_blue, vec2<i32>(local), i32(layer), vec4<f32>(captured_rgb[offset + 2u], 0.0, 0.0, 0.0));
}
