struct NormalizeParameters {
    tile_width: u32,
    tile_height: u32,
    tile_left: u32,
    tile_top: u32,
    crop_origin_x: u32,
    crop_origin_y: u32,
    destination_layer: u32,
    black_repeat_width: u32,
    black_repeat_height: u32,
    white_mode: u32,
    bayer_pattern: u32,
    _padding: u32,
};

@group(0) @binding(0) var raw_input: texture_2d<u32>;
@group(0) @binding(1) var normalized_output: texture_storage_2d_array<r32float, write>;
@group(0) @binding(2) var<storage, read> black_levels: array<f32>;
@group(0) @binding(3) var<storage, read> white_levels: array<f32>;
@group(0) @binding(4) var<uniform> parameters: NormalizeParameters;

fn bayer_channel(pattern: u32, x: u32, y: u32) -> u32 {
    let index = ((y & 1u) << 1u) | (x & 1u);
    // 0 = red, 1 = green, 2 = blue. Values mirror BayerPattern::color_at.
    if pattern == 0u { // RGGB
        return array<u32, 4>(0u, 1u, 1u, 2u)[index];
    }
    if pattern == 1u { // BGGR
        return array<u32, 4>(2u, 1u, 1u, 0u)[index];
    }
    if pattern == 2u { // GRBG
        return array<u32, 4>(1u, 0u, 2u, 1u)[index];
    }
    // GBRG
    return array<u32, 4>(1u, 2u, 0u, 1u)[index];
}

@compute @workgroup_size(8, 8, 1)
fn normalize(@builtin(global_invocation_id) invocation: vec3<u32>) {
    if invocation.x >= parameters.tile_width || invocation.y >= parameters.tile_height {
        return;
    }
    let sensor_x = parameters.crop_origin_x + parameters.tile_left + invocation.x;
    let sensor_y = parameters.crop_origin_y + parameters.tile_top + invocation.y;
    let black_index = (sensor_y % parameters.black_repeat_height)
        * parameters.black_repeat_width
        + (sensor_x % parameters.black_repeat_width);
    let black = black_levels[black_index];
    var white: f32;
    if parameters.white_mode == 0u {
        white = white_levels[0u];
    } else if parameters.white_mode == 1u {
        let crop_x = parameters.tile_left + invocation.x;
        let crop_y = parameters.tile_top + invocation.y;
        white = white_levels[bayer_channel(parameters.bayer_pattern, crop_x, crop_y)];
    } else {
        white = white_levels[black_index];
    }
    let sample = f32(textureLoad(raw_input, vec2<i32>(invocation.xy), 0).x);
    textureStore(
        normalized_output,
        vec2<i32>(invocation.xy),
        i32(parameters.destination_layer),
        vec4<f32>((sample - black) / (white - black), 0.0, 0.0, 1.0),
    );
}
