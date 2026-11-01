// Appended to spatial.wgsl: optics output of one rectangular region as
// interleaved RGB, the tile input of the GPU defringe.
@group(1) @binding(7) var<storage, read_write> region_rgb: array<f32>;
// x, y, width, height of the region in optics-output coordinates.
@group(1) @binding(8) var<uniform> region: vec4<u32>;

@compute @workgroup_size(8, 8, 1)
fn materialize_region(@builtin(global_invocation_id) invocation: vec3<u32>) {
    if invocation.x >= region.z || invocation.y >= region.w {
        return;
    }
    let output = region.xy + invocation.xy;
    let index = (invocation.y * region.z + invocation.x) * 3u;
    region_rgb[index] = corrected_channel(output, 0u);
    region_rgb[index + 1u] = corrected_channel(output, 1u);
    region_rgb[index + 2u] = corrected_channel(output, 2u);
}
