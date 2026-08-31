struct PreviewParameters {
    exposure_gain: f32,
    contrast_gain: f32,
    saturation: f32,
    vibrance: f32,
    highlights: f32,
    shadows: f32,
    whites: f32,
    blacks: f32,
    tone_shadows: f32,
    tone_darks: f32,
    tone_lights: f32,
    tone_highlights: f32,
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

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let normalized = clamp((value - edge0) / (edge1 - edge0), 0.0, 1.0);
    return normalized * normalized * (3.0 - 2.0 * normalized);
}

fn apply_light_tone(pixel: vec3<f32>) -> vec3<f32> {
    let current = dot(pixel, vec3<f32>(0.2627, 0.6780, 0.0593));
    let normalized = clamp(current, 0.0, 1.0);
    let shadow_weight = 1.0 - smoothstep(0.0, 0.55, normalized);
    let highlight_weight = smoothstep(0.45, 1.0, normalized);
    let black_weight = 1.0 - smoothstep(0.0, 0.30, normalized);
    let white_weight = smoothstep(0.70, 1.0, normalized);
    let delta = parameters.shadows * 0.25 * shadow_weight
        + parameters.highlights * 0.25 * highlight_weight
        + parameters.blacks * 0.15 * black_weight
        + parameters.whites * 0.15 * white_weight;
    if delta == 0.0 {
        return pixel;
    }
    let adjusted_luminance = current + delta;
    if abs(current) > 0.000001 {
        return pixel * (adjusted_luminance / current);
    }
    return pixel + vec3<f32>(delta);
}

fn apply_tone_curve(pixel: vec3<f32>) -> vec3<f32> {
    let current = dot(pixel, vec3<f32>(0.2627, 0.6780, 0.0593));
    let normalized = clamp(current, 0.0, 1.0);
    let shadows = 1.0 - smoothstep(0.0, 0.45, normalized);
    let darks = smoothstep(0.0, 0.12, normalized) * (1.0 - smoothstep(0.35, 0.55, normalized));
    let lights = smoothstep(0.45, 0.65, normalized) * (1.0 - smoothstep(0.88, 1.0, normalized));
    let highlights = smoothstep(0.60, 1.0, normalized);
    let delta = parameters.tone_shadows * shadows
        + parameters.tone_darks * darks
        + parameters.tone_lights * lights
        + parameters.tone_highlights * highlights;
    if delta == 0.0 {
        return pixel;
    }
    let adjusted_luminance = current + delta;
    if abs(current) > 0.000001 {
        return pixel * (adjusted_luminance / current);
    }
    return pixel + vec3<f32>(delta);
}

fn color_saturation(pixel: vec3<f32>, luminance: f32) -> f32 {
    let chroma = max(abs(pixel.r - luminance), max(abs(pixel.g - luminance), abs(pixel.b - luminance)));
    return clamp(chroma / max(abs(luminance), 0.000001), 0.0, 1.0);
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
    let toned = apply_tone_curve(apply_light_tone(contrasted));
    let luminance = dot(toned, vec3<f32>(0.2627, 0.6780, 0.0593));
    let saturation = parameters.saturation
        * (1.0 + parameters.vibrance * (1.0 - color_saturation(toned, luminance)));
    let adjusted = vec3<f32>(luminance) + saturation * (toned - vec3<f32>(luminance));

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
