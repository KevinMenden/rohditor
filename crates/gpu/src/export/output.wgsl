struct ExportParameters {
    first_row: u32,
    row_count: u32,
    maximum: u32,
    ordered_dither: u32,
};
@group(0) @binding(1) var<storage, read_write> export_samples: array<u32>;
@group(0) @binding(2) var<uniform> export_parameters: ExportParameters;

const BAYER_8X8 = array<u32, 64>(
    0,32,8,40,2,34,10,42, 48,16,56,24,50,18,58,26,
    12,44,4,36,14,46,6,38, 60,28,52,20,62,30,54,22,
    3,35,11,43,1,33,9,41, 51,19,59,27,49,17,57,25,
    15,47,7,39,13,45,5,37, 63,31,55,23,61,29,53,21,
);

@compute @workgroup_size(16, 16, 1)
fn develop_export(@builtin(global_invocation_id) invocation: vec3<u32>) {
    if invocation.x >= parameters.output_width || invocation.y >= export_parameters.row_count {
        return;
    }
    // Geometry and dither use global output coordinates across every band.
    let output = vec2<u32>(invocation.x, invocation.y + export_parameters.first_row);
    let encoded = encode_output(develop_color(source_coordinate(output)));
    var dither = 0.0;
    if export_parameters.ordered_dither != 0u {
        dither = (f32(BAYER_8X8[(output.y & 7u) * 8u + (output.x & 7u)]) + 0.5) / 64.0 - 0.5;
    }
    let maximum = f32(export_parameters.maximum);
    // CPU round() rounds nonnegative halves upward; WGSL round() uses ties
    // to even. Clamp first and use floor(x + .5) to preserve CPU semantics.
    let samples = vec3<u32>(floor(clamp(encoded * maximum + vec3<f32>(dither), vec3<f32>(0.0), vec3<f32>(maximum)) + vec3<f32>(0.5)));
    let offset = (invocation.y * parameters.output_width + invocation.x) * 3u;
    export_samples[offset] = samples.r;
    export_samples[offset + 1u] = samples.g;
    export_samples[offset + 2u] = samples.b;
}
