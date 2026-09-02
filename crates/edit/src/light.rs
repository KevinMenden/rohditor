use crate::LightAdjustments;

/// Number of samples in the deterministic Light-control transfer function.
///
/// The CPU interpolates this table and the GPU uploads the same values, which
/// keeps the interactive preview tied to the reference implementation.
pub const LIGHT_TONE_LUT_SIZE: usize = 4_096;

const CONTRAST_PIVOT: f32 = 0.18;
const SHADOW_STRENGTH: f32 = 0.45;
const HIGHLIGHT_STRENGTH: f32 = 0.45;
const BLACK_STRENGTH: f32 = 0.25;
const WHITE_STRENGTH: f32 = 0.25;

/// Bounded, monotonic scene-to-display tonal transform for the Light controls.
///
/// Exposure is deliberately not part of this table. Values in `[0, 1]` are
/// mapped into `[0, 1]`; negative and HDR values use an identity extension so
/// the scene-linear working range is not silently discarded.
#[derive(Debug, Clone, PartialEq)]
pub struct LightToneLut {
    values: [f32; LIGHT_TONE_LUT_SIZE],
}

impl LightToneLut {
    /// Build the shared CPU/GPU transfer table for one validated recipe.
    #[must_use]
    pub fn new(light: &LightAdjustments) -> Self {
        let mut values = [0.0; LIGHT_TONE_LUT_SIZE];
        let denominator = (LIGHT_TONE_LUT_SIZE - 1) as f32;
        for (index, value) in values.iter_mut().enumerate() {
            let input = index as f32 / denominator;
            *value = evaluate_light_tone(light, input);
        }

        // Interacting regional controls must never invert tonal order. Do the
        // projection once while constructing the LUT so CPU and GPU consume
        // exactly the same monotonic result.
        let mut previous = values[0];
        for value in &mut values[1..] {
            *value = value.max(previous);
            previous = *value;
        }

        Self { values }
    }

    /// Evaluate the table with linear interpolation.
    #[must_use]
    pub fn sample(&self, input: f32) -> f32 {
        if !input.is_finite() || !(0.0..=1.0).contains(&input) {
            return input;
        }
        let position = input * (LIGHT_TONE_LUT_SIZE - 1) as f32;
        let lower = position.floor() as usize;
        let upper = (lower + 1).min(LIGHT_TONE_LUT_SIZE - 1);
        let fraction = position - lower as f32;
        self.values[lower] + (self.values[upper] - self.values[lower]) * fraction
    }

    /// Packed LUT samples suitable for a GPU storage-buffer upload.
    #[must_use]
    pub const fn values(&self) -> &[f32; LIGHT_TONE_LUT_SIZE] {
        &self.values
    }
}

fn evaluate_light_tone(light: &LightAdjustments, input: f32) -> f32 {
    let mut output = protected_contrast(input, light.contrast);

    // Shadows and highlights are interior bell-shaped regions. They vanish at
    // the endpoints, leaving black/white point behavior to their named
    // controls and greatly reducing unintended cross-region movement.
    let shadow_weight = smoothstep(0.0, 0.16, input) * (1.0 - smoothstep(0.38, 0.68, input));
    let highlight_weight = smoothstep(0.32, 0.62, input) * (1.0 - smoothstep(0.84, 1.0, input));
    output = bounded_move(output, light.shadows, SHADOW_STRENGTH * shadow_weight);
    output = bounded_move(
        output,
        light.highlights,
        HIGHLIGHT_STRENGTH * highlight_weight,
    );

    // Blacks and Whites own the toe and shoulder endpoints. Moving toward a
    // boundary is proportional to the remaining room, so no control can push
    // an in-range luminance below zero or above one.
    let black_weight = 1.0 - smoothstep(0.0, 0.32, input);
    let white_weight = smoothstep(0.68, 1.0, input);
    output = bounded_move(output, light.blacks, BLACK_STRENGTH * black_weight);
    output = bounded_move(output, light.whites, WHITE_STRENGTH * white_weight);
    output.clamp(0.0, 1.0)
}

fn protected_contrast(input: f32, contrast: f32) -> f32 {
    if contrast == 0.0 {
        return input;
    }
    let exponent = contrast.exp2();
    if input <= CONTRAST_PIVOT {
        CONTRAST_PIVOT * (input / CONTRAST_PIVOT).powf(exponent)
    } else {
        1.0 - (1.0 - CONTRAST_PIVOT) * ((1.0 - input) / (1.0 - CONTRAST_PIVOT)).powf(exponent)
    }
}

fn bounded_move(value: f32, amount: f32, weight: f32) -> f32 {
    if amount >= 0.0 {
        value + (1.0 - value) * amount * weight
    } else {
        value + value * amount * weight
    }
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let normalized = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    normalized * normalized * (3.0 - 2.0 * normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_lut_is_identity_with_identity_extensions() {
        let lut = LightToneLut::new(&LightAdjustments::default());
        for input in [0.0, 0.01, 0.18, 0.5, 0.91, 1.0] {
            assert!((lut.sample(input) - input).abs() < 1.0e-6);
        }
        assert_eq!(lut.sample(-0.25), -0.25);
        assert_eq!(lut.sample(1.5), 1.5);
        assert!(lut.sample(f32::NAN).is_nan());
    }

    #[test]
    fn extreme_combination_is_bounded_and_monotonic() {
        let light = LightAdjustments {
            contrast: 1.0,
            highlights: 1.0,
            shadows: -1.0,
            whites: -1.0,
            blacks: 1.0,
            ..LightAdjustments::default()
        };
        let lut = LightToneLut::new(&light);
        let mut previous = 0.0;
        for &value in lut.values() {
            assert!((0.0..=1.0).contains(&value));
            assert!(value >= previous);
            previous = value;
        }
    }

    #[test]
    fn small_signed_changes_move_without_creating_new_clipping() {
        let samples = [0.02, 0.09, 0.22, 0.47, 0.73, 0.91, 0.98];
        for select in [
            |light: &mut LightAdjustments, value| light.contrast = value,
            |light: &mut LightAdjustments, value| light.shadows = value,
            |light: &mut LightAdjustments, value| light.highlights = value,
            |light: &mut LightAdjustments, value| light.blacks = value,
            |light: &mut LightAdjustments, value| light.whites = value,
        ] {
            for amount in [-0.05, 0.05] {
                let mut light = LightAdjustments::default();
                select(&mut light, amount);
                let lut = LightToneLut::new(&light);
                for input in samples {
                    let output = lut.sample(input);
                    assert!(output > 0.0 && output < 1.0, "{input} mapped to {output}");
                }
            }
        }
    }

    #[test]
    fn shadow_and_highlight_masks_are_local() {
        let shadows = LightAdjustments {
            shadows: 0.5,
            ..LightAdjustments::default()
        };
        let shadows = LightToneLut::new(&shadows);
        assert!(shadows.sample(0.2) - 0.2 > 0.05);
        assert!((shadows.sample(0.9) - 0.9).abs() < 1.0e-5);

        let highlights = LightAdjustments {
            highlights: -0.5,
            ..LightAdjustments::default()
        };
        let highlights = LightToneLut::new(&highlights);
        assert!(0.8 - highlights.sample(0.8) > 0.05);
        assert!((highlights.sample(0.1) - 0.1).abs() < 1.0e-5);
    }

    #[test]
    fn black_and_white_controls_own_the_endpoints() {
        let raised_blacks = LightAdjustments {
            blacks: 1.0,
            ..LightAdjustments::default()
        };
        let raised_blacks = LightToneLut::new(&raised_blacks);
        assert!((raised_blacks.sample(0.0) - BLACK_STRENGTH).abs() < 1.0e-6);
        assert!((raised_blacks.sample(0.8) - 0.8).abs() < 1.0e-5);

        let lowered_whites = LightAdjustments {
            whites: -1.0,
            ..LightAdjustments::default()
        };
        let lowered_whites = LightToneLut::new(&lowered_whites);
        assert!(lowered_whites.sample(1.0) < 1.0);
        assert!((lowered_whites.sample(0.2) - 0.2).abs() < 1.0e-5);
    }
}
