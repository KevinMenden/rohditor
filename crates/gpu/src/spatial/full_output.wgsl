@group(1) @binding(7)
var spatial_display_srgb: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(16, 16, 1)
fn develop_spatial_full(@builtin(global_invocation_id) invocation: vec3<u32>) {
    let output = invocation.xy;
    if output.x >= parameters.output_width || output.y >= parameters.output_height {
        return;
    }
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
