// CPU reference: core/src/cpu/stages.rs, adjustments module.
// Operate on signed/HDR Rec.2020, preserving its range around bounded HSL math.
fn rgb_to_hsl(rgb: vec3<f32>) -> vec3<f32> {
    let maximum = max(rgb.r, max(rgb.g, rgb.b));
    let minimum = min(rgb.r, min(rgb.g, rgb.b));
    let lightness = (maximum + minimum) * 0.5;
    let chroma = maximum - minimum;
    if chroma <= parameters.color_options.w {
        return vec3<f32>(0.0, 0.0, lightness);
    }
    let saturation = chroma / (1.0 - abs(2.0 * lightness - 1.0));
    var hue: f32;
    if maximum == rgb.r {
        hue = (rgb.g - rgb.b) / chroma;
        hue = (hue - floor(hue / 6.0) * 6.0) / 6.0;
    } else if maximum == rgb.g {
        hue = ((rgb.b - rgb.r) / chroma + 2.0) / 6.0;
    } else {
        hue = ((rgb.r - rgb.g) / chroma + 4.0) / 6.0;
    }
    return vec3<f32>(hue, saturation, lightness);
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    if hsl.y <= parameters.color_options.w {
        return vec3<f32>(hsl.z);
    }
    let chroma = (1.0 - abs(2.0 * hsl.z - 1.0)) * hsl.y;
    let hue = hsl.x * 6.0;
    let secondary = chroma * (1.0 - abs((hue - floor(hue / 2.0) * 2.0) - 1.0));
    var rgb: vec3<f32>;
    if hue < 1.0 { rgb = vec3<f32>(chroma, secondary, 0.0); }
    else if hue < 2.0 { rgb = vec3<f32>(secondary, chroma, 0.0); }
    else if hue < 3.0 { rgb = vec3<f32>(0.0, chroma, secondary); }
    else if hue < 4.0 { rgb = vec3<f32>(0.0, secondary, chroma); }
    else if hue < 5.0 { rgb = vec3<f32>(secondary, 0.0, chroma); }
    else { rgb = vec3<f32>(chroma, 0.0, secondary); }
    return rgb + vec3<f32>(hsl.z - chroma * 0.5);
}

fn apply_hsl_adjustments(pixel: vec3<f32>) -> vec3<f32> {
    if parameters.color_options.x == 0.0 || !finite_rgb(pixel) { return pixel; }
    let offset = max(-min(pixel.r, min(pixel.g, pixel.b)), 0.0);
    let shifted = pixel + vec3<f32>(offset);
    if !finite_rgb(shifted) { return pixel; }
    let scale = max(shifted.r, max(shifted.g, shifted.b));
    if !finite_value(scale) || scale <= parameters.color_options.w { return pixel; }
    let hsl = rgb_to_hsl(shifted / scale);
    let chroma_weight = smoothstep(0.0, 0.05, hsl.y);
    if chroma_weight <= parameters.color_options.w { return pixel; }

    // Match the CPU accumulation order, including the Magenta/Red wraparound.
    let hue = hsl.x - floor(hsl.x);
    var left = 7u;
    var right = 0u;
    var start = parameters.hsl[7].w;
    var end = 1.0;
    for (var i = 0u; i < 7u; i += 1u) {
        if hue >= parameters.hsl[i].w && hue < parameters.hsl[i + 1u].w {
            left = i;
            right = i + 1u;
            start = parameters.hsl[i].w;
            end = parameters.hsl[i + 1u].w;
            break;
        }
    }
    let fraction = clamp((hue - start) / (end - start), 0.0, 1.0);
    var shift = vec3<f32>(0.0);
    for (var i = 0u; i < 8u; i += 1u) {
        var weight = 0.0;
        if i == left { weight = 1.0 - fraction; }
        if i == right { weight = fraction; }
        weight *= chroma_weight;
        shift += parameters.hsl[i].xyz * vec3<f32>(parameters.color_options.z, 0.5, 0.25) * weight;
    }
    if all(shift == vec3<f32>(0.0)) { return pixel; }
    let shifted_hue = hsl.x + shift.x;
    let converted = hsl_to_rgb(vec3<f32>(
        shifted_hue - floor(shifted_hue),
        clamp(hsl.y + shift.y, 0.0, 1.0),
        clamp(hsl.z + shift.z, 0.0, 1.0),
    ));
    let restored = converted * scale - vec3<f32>(offset);
    if finite_rgb(restored) { return restored; }
    return pixel;
}

fn apply_color_grading(pixel: vec3<f32>) -> vec3<f32> {
    if parameters.color_options.y == 0.0 { return pixel; }
    let coefficients = vec3<f32>(0.2627, 0.6780, 0.0593);
    let source_luminance = dot(pixel, coefficients);
    let value = clamp(source_luminance, 0.0, 1.0);
    let shadows = 1.0 - smoothstep(0.0, 0.5, value);
    let midtones = smoothstep(0.15, 0.45, value) * (1.0 - smoothstep(0.55, 0.85, value));
    let highlights = smoothstep(0.5, 1.0, value);
    let grade = parameters.grading_shadows.xyz * shadows
        + parameters.grading_midtones.xyz * midtones
        + parameters.grading_highlights.xyz * highlights;
    let tinted = pixel * (vec3<f32>(1.0) + 0.25 * grade);
    if !finite_rgb(tinted) { return pixel; }
    let target_luminance = dot(tinted, coefficients);
    if abs(source_luminance) > 1.0e-6 && finite_value(source_luminance)
        && abs(target_luminance) > 1.0e-6 && finite_value(target_luminance)
        && sign(source_luminance) == sign(target_luminance) {
        let graded = tinted * (source_luminance / target_luminance);
        if finite_rgb(graded) { return graded; }
        return pixel;
    }
    return tinted;
}
