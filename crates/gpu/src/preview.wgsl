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
    white_balance: vec4<f32>,
    camera_to_rec2020_row0: vec4<f32>,
    camera_to_rec2020_row1: vec4<f32>,
    camera_to_rec2020_row2: vec4<f32>,
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

@group(0) @binding(4)
var<storage, read> light_tone_lut: array<f32, 4096>;

const LUMINANCE_RATIO_TRANSITION: f32 = 0.02;

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

fn apply_luminance_delta(pixel: vec3<f32>, current: f32, desired: f32) -> vec3<f32> {
    if (current > 0.000001 && desired >= LUMINANCE_RATIO_TRANSITION)
        || (current < -0.000001 && desired <= -LUMINANCE_RATIO_TRANSITION) {
        return pixel * (desired / current);
    }
    let delta = desired - current;
    let additive = pixel + vec3<f32>(delta);
    if abs(current) <= 0.000001 || sign(current) != sign(desired) {
        return additive;
    }
    let ratio_weight = smoothstep(0.0, LUMINANCE_RATIO_TRANSITION, abs(desired));
    if ratio_weight <= 0.000001 {
        return additive;
    }
    let scaled = pixel * (desired / current);
    return mix(additive, scaled, ratio_weight);
}

fn apply_light_tone(pixel: vec3<f32>) -> vec3<f32> {
    let current = dot(pixel, vec3<f32>(0.2627, 0.6780, 0.0593));
    if current < 0.0 || current > 1.0 {
        return pixel;
    }
    let position = current * 4095.0;
    let lower = u32(floor(position));
    let upper = min(lower + 1u, 4095u);
    let desired = mix(light_tone_lut[lower], light_tone_lut[upper], position - f32(lower));
    if desired == current {
        return pixel;
    }
    return apply_luminance_delta(pixel, current, desired);
}

fn tone_curve_value(input: f32) -> f32 {
    if input < 0.0 || input > 1.0 {
        return input;
    }
    var y0 = 0.0;
    var y1 = clamp(0.12 + parameters.tone_shadows, 0.0, 1.0);
    var y2 = clamp(0.35 + parameters.tone_darks, 0.0, 1.0);
    var y3 = clamp(0.65 + parameters.tone_lights, 0.0, 1.0);
    var y4 = clamp(0.88 + parameters.tone_highlights, 0.0, 1.0);
    var y5 = 1.0;
    y1 = max(y1, y0);
    y2 = max(y2, y1);
    y3 = max(y3, y2);
    y4 = max(y4, y3);
    y5 = max(y5, y4);
    if input <= 0.12 {
        return mix(y0, y1, input / 0.12);
    }
    if input <= 0.35 {
        return mix(y1, y2, (input - 0.12) / 0.23);
    }
    if input <= 0.65 {
        return mix(y2, y3, (input - 0.35) / 0.30);
    }
    if input <= 0.88 {
        return mix(y3, y4, (input - 0.65) / 0.23);
    }
    return mix(y4, y5, (input - 0.88) / 0.12);
}

fn apply_tone_curve(pixel: vec3<f32>) -> vec3<f32> {
    let current = dot(pixel, vec3<f32>(0.2627, 0.6780, 0.0593));
    let adjusted_luminance = tone_curve_value(current);
    if adjusted_luminance == current {
        return pixel;
    }
    return apply_luminance_delta(pixel, current, adjusted_luminance);
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
    let camera_native = textureLoad(source_base, vec2<i32>(source), 0).rgb;
    let balanced = camera_native * parameters.white_balance.xyz;
    let base = vec3<f32>(
        dot(parameters.camera_to_rec2020_row0.xyz, balanced),
        dot(parameters.camera_to_rec2020_row1.xyz, balanced),
        dot(parameters.camera_to_rec2020_row2.xyz, balanced),
    );
    let exposed = base * parameters.exposure_gain;
    let toned = apply_tone_curve(apply_light_tone(exposed));
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
