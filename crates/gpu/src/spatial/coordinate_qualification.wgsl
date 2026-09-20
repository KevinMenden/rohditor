@group(3) @binding(0) var<storage, read> qualification_points: array<vec2<u32>>;
@group(3) @binding(1) var<storage, read_write> qualification_coordinates: array<vec2<f32>>;

@compute @workgroup_size(64, 1, 1)
fn qualify_coordinates(@builtin(global_invocation_id) invocation: vec3<u32>) {
    if invocation.x >= arrayLength(&qualification_points) {
        return;
    }
    let point = qualification_points[invocation.x];
    let first = invocation.x * 3u;
    qualification_coordinates[first] = mapped_coordinate(point, 0u);
    qualification_coordinates[first + 1u] = mapped_coordinate(point, 1u);
    qualification_coordinates[first + 2u] = mapped_coordinate(point, 2u);
}
