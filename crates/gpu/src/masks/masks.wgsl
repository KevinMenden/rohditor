// Local adjustments (mask layers) in the colour pass. CPU reference:
// rohditor-core `cpu/local_stage.rs`, `cpu/range_reference.rs` and rohditor-mask.
//
// `adjustment_tables` (binding 4) holds, in f32 words (counts and codes are
// plain numbers, never bit patterns, so no device can flush them as denormals):
//   0                       global Light tone LUT
//   4096 * (1 + layer)      that layer's Light tone LUT
//   MASK_LAYERS_OFFSET      one record per layer, MASK_LAYER_WORDS each:
//                           opacity, exposure, temperature, tint,
//                           saturation scale, has tone, component count
//   MASK_COMPONENTS_OFFSET  MASK_COMPONENTS_PER_LAYER records per layer,
//                           MASK_COMPONENT_WORDS each:
//                           shape, combine mode, invert, then the shape's
//                           parameters (see `masks/tables.rs`)
// The storage buffer is shared because the staged export already uses every
// storage binding a shader stage is guaranteed.
const MASK_LAYERS_OFFSET: u32 = 135168u;
const MASK_LAYER_WORDS: u32 = 8u;
const MASK_COMPONENTS_OFFSET: u32 = 135424u;
const MASK_COMPONENT_WORDS: u32 = 31u;
const MASK_COMPONENTS_PER_LAYER: u32 = 16u;
const MASK_MAX_LAYERS: u32 = 32u;
const MASK_PARAMETERS: u32 = 3u;

const MASK_LINEAR: u32 = 0u;
const MASK_RADIAL: u32 = 1u;
const MASK_BRUSH: u32 = 2u;
const MASK_LUMINANCE: u32 = 3u;
const MASK_COLOR: u32 = 4u;

const MASK_ADD: u32 = 0u;
const MASK_SUBTRACT: u32 = 1u;

const MASK_NEEDS_LUMINANCE: u32 = 1u;
const MASK_NEEDS_OKLAB: u32 = 2u;

// Brush coverage: one 16-bit plane per brush component, for the rectangle of
// the developed frame starting at `parameters.coverage_origin`.
@group(0) @binding(7)
var mask_coverage: texture_2d_array<u32>;

// What range components select on at the current pixel.
struct MaskReference {
    valid: bool,
    luminance: f32,
    oklab: vec3<f32>,
};

fn mask_word(index: u32) -> f32 {
    return adjustment_tables[index];
}

fn mask_code(index: u32) -> u32 {
    return u32(mask_word(index));
}

// rohditor_mask::math::smoothstep, which unlike the helper in preview.wgsl
// degenerates to a hard step at edge1 (a feather of 0).
fn mask_smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    if edge0 >= edge1 {
        return select(0.0, 1.0, value >= edge1);
    }
    let t = clamp((value - edge0) / (edge1 - edge0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

// `measure` in range_reference.rs: after the global exposure and base rendering,
// before Light, Color and every local term, so masks never select on each other.
fn measure_mask_reference(look: vec3<f32>) -> MaskReference {
    var measured = MaskReference(false, 0.0, vec3<f32>(0.0));
    if parameters.mask_reference == 0u {
        return measured;
    }
    let reference = apply_camera_chroma(apply_base_rendering(look * parameters.exposure_gain));
    measured.valid = finite_rgb(reference);
    if !measured.valid {
        return measured;
    }
    if (parameters.mask_reference & MASK_NEEDS_LUMINANCE) != 0u {
        let luminance = dot(reference, vec3<f32>(0.2627, 0.6780, 0.0593));
        // White must be exactly 1 so that a range ending at 1 includes it.
        measured.luminance = select(
            linear_srgb_to_srgb(max(luminance, 0.0)),
            1.0,
            luminance >= 1.0,
        );
    }
    if (parameters.mask_reference & MASK_NEEDS_OKLAB) != 0u {
        measured.oklab = linear_srgb_to_oklab(vec3<f32>(
            dot(parameters.rec2020_to_srgb_row0.xyz, reference),
            dot(parameters.rec2020_to_srgb_row1.xyz, reference),
            dot(parameters.rec2020_to_srgb_row2.xyz, reference),
        ));
    }
    return measured;
}

fn mask_linear_alpha(p: u32, pixel: vec2<f32>) -> f32 {
    let t = (pixel.x - mask_word(p)) * mask_word(p + 2u)
        + (pixel.y - mask_word(p + 1u)) * mask_word(p + 3u);
    return 1.0 - mask_smoothstep(0.0, 1.0, t);
}

fn mask_radial_alpha(p: u32, pixel: vec2<f32>) -> f32 {
    let delta = pixel - vec2<f32>(mask_word(p), mask_word(p + 1u));
    let cosine = mask_word(p + 2u);
    let sine = mask_word(p + 3u);
    // Rotate into the ellipse's own axes, then scale to unit radii.
    let u = (delta.x * cosine + delta.y * sine) * mask_word(p + 4u);
    let v = (-delta.x * sine + delta.y * cosine) * mask_word(p + 5u);
    return 1.0 - mask_smoothstep(1.0 - mask_word(p + 6u), 1.0, length(vec2<f32>(u, v)));
}

fn mask_brush_alpha(p: u32, source: vec2<u32>) -> f32 {
    let texel = vec2<i32>(source) - vec2<i32>(parameters.coverage_origin);
    return f32(textureLoad(mask_coverage, texel, i32(mask_code(p)), 0).r) / 65535.0;
}

fn mask_luminance_alpha(p: u32, reference: MaskReference) -> f32 {
    if !reference.valid {
        return 0.0;
    }
    let value = reference.luminance;
    let low = mask_word(p);
    let high = mask_word(p + 1u);
    if value < low {
        return mask_smoothstep(low - mask_word(p + 2u), low, value);
    }
    if value > high {
        return 1.0 - mask_smoothstep(high, high + mask_word(p + 3u), value);
    }
    return 1.0;
}

fn mask_color_alpha(p: u32, reference: MaskReference) -> f32 {
    let count = mask_code(p);
    if !reference.valid || count == 0u {
        return 0.0;
    }
    let weight = mask_word(p + 1u);
    var nearest = 3.0e38;
    for (var sample = 0u; sample < count; sample += 1u) {
        let s = p + 4u + sample * 3u;
        let delta = reference.oklab - vec3<f32>(mask_word(s), mask_word(s + 1u), mask_word(s + 2u));
        let lightness = weight * delta.x;
        nearest = min(nearest, lightness * lightness + delta.y * delta.y + delta.z * delta.z);
    }
    return 1.0 - mask_smoothstep(mask_word(p + 2u), mask_word(p + 3u), sqrt(nearest));
}

// PreparedMask::evaluate_row for one pixel, without the opacity.
fn mask_layer_alpha(layer: u32, source: vec2<u32>, reference: MaskReference) -> f32 {
    let pixel = vec2<f32>(source) + vec2<f32>(0.5);
    let count = mask_code(MASK_LAYERS_OFFSET + layer * MASK_LAYER_WORDS + 6u);
    var accumulated = 0.0;
    for (var index = 0u; index < count; index += 1u) {
        let record = MASK_COMPONENTS_OFFSET
            + (layer * MASK_COMPONENTS_PER_LAYER + index) * MASK_COMPONENT_WORDS;
        let p = record + MASK_PARAMETERS;
        var alpha = 0.0;
        switch mask_code(record) {
            case MASK_LINEAR: { alpha = mask_linear_alpha(p, pixel); }
            case MASK_RADIAL: { alpha = mask_radial_alpha(p, pixel); }
            case MASK_BRUSH: { alpha = mask_brush_alpha(p, source); }
            case MASK_LUMINANCE: { alpha = mask_luminance_alpha(p, reference); }
            case MASK_COLOR: { alpha = mask_color_alpha(p, reference); }
            default: {}
        }
        if mask_word(record + 2u) != 0.0 {
            alpha = 1.0 - alpha;
        }
        switch mask_code(record + 1u) {
            case MASK_ADD: { accumulated = max(accumulated, alpha); }
            case MASK_SUBTRACT: { accumulated *= 1.0 - alpha; }
            default: { accumulated *= alpha; }
        }
    }
    return accumulated;
}

// rohditor_color::local_white_balance_gains.
fn local_white_balance_gains(temperature: f32, tint: f32) -> vec3<f32> {
    let raw = exp2(
        temperature * vec3<f32>(0.5, 0.0, -0.5) + tint * vec3<f32>(0.25, -0.5, 0.25),
    );
    return raw / dot(raw, vec3<f32>(0.2627, 0.6780, 0.0593));
}

// The scene-referred terms: where a mask has exposure, one exp2 of the summed
// stops replaces the global gain.
fn apply_mask_exposure(
    look: vec3<f32>,
    alphas: ptr<function, array<f32, MASK_MAX_LAYERS>>,
) -> vec3<f32> {
    var stops = 0.0;
    var temperature = 0.0;
    var tint = 0.0;
    for (var layer = 0u; layer < parameters.mask_layer_count; layer += 1u) {
        let alpha = (*alphas)[layer];
        if !(alpha > 0.0) {
            continue;
        }
        let record = MASK_LAYERS_OFFSET + layer * MASK_LAYER_WORDS;
        stops += alpha * mask_word(record + 1u);
        temperature += alpha * mask_word(record + 2u);
        tint += alpha * mask_word(record + 3u);
    }
    var gain = parameters.exposure_gain;
    if stops != 0.0 {
        gain = exp2(parameters.light_exposure_ev + stops);
    }
    if temperature != 0.0 || tint != 0.0 {
        return look * gain * local_white_balance_gains(temperature, tint);
    }
    return look * gain;
}

// The display-referred terms, in layer order: a later layer sees the result of
// the earlier ones.
fn apply_mask_display(
    toned: vec3<f32>,
    alphas: ptr<function, array<f32, MASK_MAX_LAYERS>>,
) -> vec3<f32> {
    var pixel = toned;
    for (var layer = 0u; layer < parameters.mask_layer_count; layer += 1u) {
        let alpha = (*alphas)[layer];
        let record = MASK_LAYERS_OFFSET + layer * MASK_LAYER_WORDS;
        let has_tone = mask_word(record + 5u) != 0.0;
        let saturation = mask_word(record + 4u);
        if !(alpha > 0.0) || (!has_tone && saturation == 1.0) {
            continue;
        }
        var edited = pixel;
        if has_tone {
            edited = apply_light_tone_table(edited, (1u + layer) * 4096u);
        }
        if saturation != 1.0 {
            let luminance = dot(edited, vec3<f32>(0.2627, 0.6780, 0.0593));
            edited = vec3<f32>(luminance) + saturation * (edited - vec3<f32>(luminance));
        }
        if !finite_rgb(edited) {
            continue;
        }
        if alpha >= 1.0 {
            pixel = edited;
        } else {
            pixel += alpha * (edited - pixel);
        }
    }
    return pixel;
}

// The colour chain up to the global colour controls, with every local term. Its
// state lives in this function, not in private variables: those are initialized
// for every invocation, which would slow recipes without masks.
fn develop_masked(look: vec3<f32>, source: vec2<u32>) -> vec3<f32> {
    let reference = measure_mask_reference(look);
    var alphas: array<f32, MASK_MAX_LAYERS>;
    for (var layer = 0u; layer < parameters.mask_layer_count; layer += 1u) {
        alphas[layer] = mask_layer_alpha(layer, source, reference)
            * mask_word(MASK_LAYERS_OFFSET + layer * MASK_LAYER_WORDS);
    }
    let rendered = apply_camera_chroma(apply_base_rendering(apply_mask_exposure(look, &alphas)));
    return apply_mask_display(apply_tone_curve(apply_light_tone(rendered)), &alphas);
}
