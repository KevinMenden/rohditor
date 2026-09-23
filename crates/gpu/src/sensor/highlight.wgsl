struct ClipParameters {
    tile_width: u32,
    tile_height: u32,
    tile_left: u32,
    tile_top: u32,
    destination_layer: u32,
    bayer_pattern: u32,
    _padding0: u32,
    _padding1: u32,
    // Bit patterns of the resolved red, green, and blue Clip ceilings.
    level_bits: vec4<u32>,
};

@group(0) @binding(0) var normalized_input: texture_2d_array<f32>;
@group(0) @binding(1) var clipped_output: texture_storage_2d_array<r32float, write>;
@group(0) @binding(2) var<storage, read_write> diagnostics: array<atomic<u32>, 6>;
@group(0) @binding(3) var<uniform> parameters: ClipParameters;

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
fn clip(@builtin(global_invocation_id) invocation: vec3<u32>) {
    if invocation.x >= parameters.tile_width || invocation.y >= parameters.tile_height {
        return;
    }
    let crop_x = parameters.tile_left + invocation.x;
    let crop_y = parameters.tile_top + invocation.y;
    let channel = bayer_channel(parameters.bayer_pattern, crop_x, crop_y);
    let limit = bitcast<f32>(parameters.level_bits[channel]);
    let sample = textureLoad(
        normalized_input,
        vec2<i32>(invocation.xy),
        i32(parameters.destination_layer),
        0,
    ).x;

    if sample >= limit {
        atomicAdd(&diagnostics[0], 1u);
        atomicAdd(&diagnostics[3u + channel], 1u);
    }
    if sample > 1.0 {
        atomicAdd(&diagnostics[2], 1u);
    }
    var result = sample;
    if sample > limit {
        atomicAdd(&diagnostics[1], 1u);
        result = limit;
    }
    textureStore(
        clipped_output,
        vec2<i32>(invocation.xy),
        i32(parameters.destination_layer),
        vec4<f32>(result, 0.0, 0.0, 1.0),
    );
}
