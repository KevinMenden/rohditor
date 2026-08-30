struct PreviewParameters {
    exposure_gain: f32,
    contrast_gain: f32,
    saturation: f32,
    orientation: u32,
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
    rec2020_to_srgb_row0: vec4<f32>,
    rec2020_to_srgb_row1: vec4<f32>,
    rec2020_to_srgb_row2: vec4<f32>,
};

@group(0) @binding(0)
var source_base: texture_2d<f32>;

@group(0) @binding(1)
var working_linear: texture_storage_2d<rgba16float, write>;

@group(0) @binding(2)
var display_srgb: texture_storage_2d<rgba8unorm, write>;

@group(0) @binding(3)
var<uniform> parameters: PreviewParameters;

fn source_coordinate(output: vec2<u32>) -> vec2<u32> {
    switch parameters.orientation {
        case 0u: { return output; }
        case 1u: { return vec2<u32>(parameters.source_width - 1u - output.x, output.y); }
        case 2u: {
            return vec2<u32>(
                parameters.source_width - 1u - output.x,
                parameters.source_height - 1u - output.y,
            );
        }
        case 3u: { return vec2<u32>(output.x, parameters.source_height - 1u - output.y); }
        case 4u: { return vec2<u32>(output.y, output.x); }
        case 5u: {
            return vec2<u32>(output.y, parameters.source_height - 1u - output.x);
        }
        case 6u: {
            return vec2<u32>(
                parameters.source_width - 1u - output.y,
                parameters.source_height - 1u - output.x,
            );
        }
        default: {
            return vec2<u32>(parameters.source_width - 1u - output.y, output.x);
        }
    }
}

fn linear_srgb_to_srgb(value: f32) -> f32 {
    let clipped = clamp(value, 0.0, 1.0);
    if clipped <= 0.0031308 {
        return 12.92 * clipped;
    }
    return 1.055 * pow(clipped, 1.0 / 2.4) - 0.055;
}

@compute @workgroup_size(16, 16, 1)
fn develop_preview(@builtin(global_invocation_id) invocation: vec3<u32>) {
    let output = invocation.xy;
    if output.x >= parameters.output_width || output.y >= parameters.output_height {
        return;
    }

    let source = source_coordinate(output);
    let base = textureLoad(source_base, vec2<i32>(source), 0).rgb;
    let exposed = base * parameters.exposure_gain;
    let contrasted = vec3<f32>(0.18) + (exposed - vec3<f32>(0.18)) * parameters.contrast_gain;
    let luminance = dot(contrasted, vec3<f32>(0.2627, 0.6780, 0.0593));
    let adjusted = vec3<f32>(luminance) + parameters.saturation * (contrasted - vec3<f32>(luminance));

    // Retain the linear working result for future GPU stages while producing
    // the display texture in the same dispatch. This avoids an extra full-frame
    // pass for the current fixed pipeline.
    textureStore(working_linear, vec2<i32>(source), vec4<f32>(adjusted, 1.0));

    let linear_srgb = vec3<f32>(
        dot(parameters.rec2020_to_srgb_row0.xyz, adjusted),
        dot(parameters.rec2020_to_srgb_row1.xyz, adjusted),
        dot(parameters.rec2020_to_srgb_row2.xyz, adjusted),
    );
    let encoded = vec3<f32>(
        linear_srgb_to_srgb(linear_srgb.r),
        linear_srgb_to_srgb(linear_srgb.g),
        linear_srgb_to_srgb(linear_srgb.b),
    );
    textureStore(display_srgb, vec2<i32>(output), vec4<f32>(encoded, 1.0));
}
