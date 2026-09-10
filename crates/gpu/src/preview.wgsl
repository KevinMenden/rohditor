struct PreviewParameters {
    exposure_gain: f32,
    rendering_profile: u32,
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
    crop_origin: vec2<u32>,
    output_policy: u32,
    output_policy_version: u32,
    chroma_search_iterations: u32,
    chroma_epsilon: f32,
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

@group(0) @binding(5)
var<storage, read> base_rendering_lut: array<f32, 4096>;

const LUMINANCE_RATIO_TRANSITION: f32 = 0.02;

fn source_coordinate(output: vec2<u32>) -> vec2<u32> {
    let oriented = output + parameters.crop_origin;
    switch parameters.orientation {
        case 0u: { return oriented; }
        case 1u: { return vec2<u32>(parameters.source_width - 1u - oriented.x, oriented.y); }
        case 2u: {
            return vec2<u32>(
                parameters.source_width - 1u - oriented.x,
                parameters.source_height - 1u - oriented.y,
            );
        }
        case 3u: { return vec2<u32>(oriented.x, parameters.source_height - 1u - oriented.y); }
        case 4u: { return vec2<u32>(oriented.y, oriented.x); }
        case 5u: {
            return vec2<u32>(oriented.y, parameters.source_height - 1u - oriented.x);
        }
        case 6u: {
            return vec2<u32>(
                parameters.source_width - 1u - oriented.y,
                parameters.source_height - 1u - oriented.x,
            );
        }
        default: {
            return vec2<u32>(parameters.source_width - 1u - oriented.y, oriented.x);
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

fn finite_value(value: f32) -> bool {
    return value == value && abs(value) <= 3.402823e38;
}

fn finite_rgb(value: vec3<f32>) -> bool {
    return finite_value(value.r) && finite_value(value.g) && finite_value(value.b);
}

fn in_srgb_gamut(value: vec3<f32>) -> bool {
    return all(value >= vec3<f32>(0.0)) && all(value <= vec3<f32>(1.0));
}

fn sanitize_and_clip(value: f32) -> f32 {
    if !finite_value(value) {
        return select(0.0, 1.0, value > 0.0);
    }
    return clamp(value, 0.0, 1.0);
}

fn signed_cbrt(value: f32) -> f32 {
    return sign(value) * pow(abs(value), 1.0 / 3.0);
}

fn linear_srgb_to_oklab(rgb: vec3<f32>) -> vec3<f32> {
    let lms = vec3<f32>(
        dot(vec3<f32>(0.41222146, 0.53633255, 0.051445995), rgb),
        dot(vec3<f32>(0.2119035, 0.6806995, 0.10739696), rgb),
        dot(vec3<f32>(0.08830246, 0.28171885, 0.6299787), rgb),
    );
    let roots = vec3<f32>(signed_cbrt(lms.r), signed_cbrt(lms.g), signed_cbrt(lms.b));
    return vec3<f32>(
        dot(vec3<f32>(0.21045426, 0.7936178, -0.004072047), roots),
        dot(vec3<f32>(1.9779985, -2.4285922, 0.4505937), roots),
        dot(vec3<f32>(0.025904037, 0.78277177, -0.80867577), roots),
    );
}

fn oklab_to_linear_srgb(lab: vec3<f32>) -> vec3<f32> {
    let roots = vec3<f32>(
        lab.x + 0.39633778 * lab.y + 0.21580376 * lab.z,
        lab.x - 0.105561346 * lab.y - 0.06385417 * lab.z,
        lab.x - 0.08948418 * lab.y - 1.2914855 * lab.z,
    );
    let lms = roots * roots * roots;
    return vec3<f32>(
        dot(vec3<f32>(4.0767417, -3.3077116, 0.23096994), lms),
        dot(vec3<f32>(-1.268438, 2.6097574, -0.34131938), lms),
        dot(vec3<f32>(-0.0041960863, -0.7034186, 1.7076147), lms),
    );
}

fn chroma_compress_to_srgb(rgb: vec3<f32>) -> vec3<f32> {
    if !finite_rgb(rgb) {
        return vec3<f32>(
            sanitize_and_clip(rgb.r),
            sanitize_and_clip(rgb.g),
            sanitize_and_clip(rgb.b),
        );
    }
    if in_srgb_gamut(rgb) {
        return rgb;
    }
    let lab = linear_srgb_to_oklab(rgb);
    if !finite_rgb(lab) {
        return clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    let lightness = clamp(lab.x, 0.0, 1.0);
    let chroma = length(lab.yz);
    if chroma <= parameters.chroma_epsilon {
        return clamp(oklab_to_linear_srgb(vec3<f32>(lightness, 0.0, 0.0)), vec3<f32>(0.0), vec3<f32>(1.0));
    }
    var lower = 0.0;
    var upper = 1.0;
    for (var iteration = 0u; iteration < parameters.chroma_search_iterations; iteration += 1u) {
        let scale = (lower + upper) * 0.5;
        let candidate = oklab_to_linear_srgb(vec3<f32>(lightness, lab.yz * scale));
        if finite_rgb(candidate) && in_srgb_gamut(candidate) {
            lower = scale;
        } else {
            upper = scale;
        }
    }
    let mapped = oklab_to_linear_srgb(vec3<f32>(lightness, lab.yz * lower));
    if !finite_rgb(mapped) {
        return clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    return clamp(mapped, vec3<f32>(0.0), vec3<f32>(1.0));
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

fn apply_base_rendering(pixel: vec3<f32>) -> vec3<f32> {
    if parameters.rendering_profile == 0u {
        return pixel;
    }
    let current = dot(pixel, vec3<f32>(0.2627, 0.6780, 0.0593));
    if current <= 0.0 {
        return pixel;
    }
    let coordinate = current / (1.0 + current);
    let position = coordinate * 4095.0;
    let lower = min(u32(floor(position)), 4095u);
    let upper = min(lower + 1u, 4095u);
    let desired = mix(
        base_rendering_lut[lower],
        base_rendering_lut[upper],
        position - f32(lower),
    );
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
    let rendered = apply_base_rendering(exposed);
    let toned = apply_tone_curve(apply_light_tone(rendered));
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
    var mapped_linear_srgb = linear_srgb;
    if parameters.output_policy == 1u && parameters.output_policy_version == 1u {
        mapped_linear_srgb = chroma_compress_to_srgb(linear_srgb);
    }
    let encoded = vec3<f32>(
        linear_srgb_to_srgb(mapped_linear_srgb.r),
        linear_srgb_to_srgb(mapped_linear_srgb.g),
        linear_srgb_to_srgb(mapped_linear_srgb.b),
    );
    textureStore(display_srgb, vec2<i32>(output), vec4<f32>(encoded, 1.0));
}
