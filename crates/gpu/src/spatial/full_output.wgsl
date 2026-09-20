@group(1) @binding(7)
var spatial_display_srgb: texture_storage_2d<rgba8unorm, write>;

// Source 1:1 is dispatched in bounded row bands by the worker. Keeping the
// offset in an explicit uniform lets each completed band observe cancellation
// before the next command buffer is submitted.
struct FullOutputBand {
    first_row: u32,
    row_count: u32,
};

@group(1) @binding(8)
var<uniform> full_output_band: FullOutputBand;

@compute @workgroup_size(16, 16, 1)
fn develop_spatial_full(@builtin(global_invocation_id) invocation: vec3<u32>) {
    if invocation.x >= parameters.output_width || invocation.y >= full_output_band.row_count {
        return;
    }
    let output = vec2<u32>(invocation.x, invocation.y + full_output_band.first_row);
    let source = source_coordinate(output);
    let camera_native = vec3<f32>(
        corrected_channel(source, 0u),
        corrected_channel(source, 1u),
        corrected_channel(source, 2u),
    );
    let adjusted = develop_camera_color(camera_native);
    textureStore(
        spatial_display_srgb,
        vec2<i32>(output),
        vec4<f32>(encode_output(adjusted), 1.0),
    );
}
