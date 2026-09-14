@group(0) @binding(1)
var working_linear: texture_storage_2d<rgba16float, write>;

@group(0) @binding(2)
var display_srgb: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(16, 16, 1)
fn develop_preview(@builtin(global_invocation_id) invocation: vec3<u32>) {
    let output = invocation.xy;
    if output.x >= parameters.output_width || output.y >= parameters.output_height {
        return;
    }
    let source = source_coordinate(output);
    let adjusted = develop_color(source);
    textureStore(working_linear, vec2<i32>(source), vec4<f32>(adjusted, 1.0));
    textureStore(display_srgb, vec2<i32>(output), vec4<f32>(encode_output(adjusted), 1.0));
}
