use std::sync::OnceLock;

/// Number of samples in the shared Rohditor Standard transfer function.
pub const BASE_RENDERING_LUT_SIZE: usize = 4_096;

/// Middle-gray anchor shared by the curve contract and its tests.
pub const ROHDITOR_STANDARD_MIDDLE_GRAY: f32 = 0.18;

// The midpoint of the range proposed for v1. Changing this changes pixels and
// therefore requires a new serialized Standard process version.
const ROHDITOR_STANDARD_V1_EXPONENT: f32 = 1.45;

/// Sampled, deterministic Rohditor Standard luminance transfer.
#[derive(Debug, Clone, PartialEq)]
pub struct BaseRenderingLut {
    values: [f32; BASE_RENDERING_LUT_SIZE],
}

impl BaseRenderingLut {
    fn standard_v1() -> Self {
        let mut values = [0.0; BASE_RENDERING_LUT_SIZE];
        let denominator = (BASE_RENDERING_LUT_SIZE - 1) as f32;
        for (index, value) in values.iter_mut().enumerate() {
            if index == BASE_RENDERING_LUT_SIZE - 1 {
                *value = 1.0;
                continue;
            }
            let coordinate = index as f32 / denominator;
            let input = coordinate / (1.0 - coordinate);
            *value = evaluate_standard_v1(input);
        }
        Self { values }
    }

    /// Evaluate the table using the frozen positive-luminance companding and
    /// linear-interpolation contract. Finite negative samples and non-finite
    /// inputs are preserved rather than converted into new invalid values.
    #[must_use]
    pub fn sample(&self, input: f32) -> f32 {
        if !input.is_finite() || input <= 0.0 {
            return input;
        }
        let coordinate = input / (1.0 + input);
        let position = coordinate * (BASE_RENDERING_LUT_SIZE - 1) as f32;
        let lower = (position.floor() as usize).min(BASE_RENDERING_LUT_SIZE - 1);
        let upper = (lower + 1).min(BASE_RENDERING_LUT_SIZE - 1);
        let fraction = position - lower as f32;
        self.values[lower] + (self.values[upper] - self.values[lower]) * fraction
    }

    /// Packed samples uploaded unchanged to the GPU preview processor.
    #[must_use]
    pub const fn values(&self) -> &[f32; BASE_RENDERING_LUT_SIZE] {
        &self.values
    }
}

/// Process-wide Standard v1 table, constructed once and shared by CPU/GPU.
#[must_use]
pub fn standard_base_rendering_lut() -> &'static BaseRenderingLut {
    static LUT: OnceLock<BaseRenderingLut> = OnceLock::new();
    LUT.get_or_init(BaseRenderingLut::standard_v1)
}

fn evaluate_standard_v1(input: f32) -> f32 {
    if input == 0.0 {
        return 0.0;
    }
    if !input.is_finite() || input < 0.0 {
        return input;
    }
    let numerator = input.powf(ROHDITOR_STANDARD_V1_EXPONENT);
    let anchor = ROHDITOR_STANDARD_MIDDLE_GRAY.powf(ROHDITOR_STANDARD_V1_EXPONENT - 1.0)
        * (1.0 - ROHDITOR_STANDARD_MIDDLE_GRAY);
    numerator / (numerator + anchor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_v1_has_exact_endpoints_and_middle_gray_anchor() {
        let lut = standard_base_rendering_lut();
        assert_eq!(lut.values()[0], 0.0);
        assert_eq!(lut.values()[BASE_RENDERING_LUT_SIZE - 1], 1.0);
        assert!(
            (lut.sample(ROHDITOR_STANDARD_MIDDLE_GRAY) - ROHDITOR_STANDARD_MIDDLE_GRAY).abs()
                < 2.0e-6
        );
    }

    #[test]
    fn standard_v1_is_bounded_finite_and_monotonic_over_positive_values() {
        let lut = standard_base_rendering_lut();
        let mut previous = 0.0;
        for index in 0..=100_000 {
            let stops = -16.0 + index as f32 * 32.0 / 100_000.0;
            let input = ROHDITOR_STANDARD_MIDDLE_GRAY * stops.exp2();
            let output = lut.sample(input);
            assert!(output.is_finite());
            assert!((0.0..=1.0).contains(&output));
            assert!(
                output >= previous,
                "{input} mapped below the previous sample"
            );
            previous = output;
        }
    }

    #[test]
    fn interpolation_tracks_the_frozen_curve_at_tonal_anchors() {
        let lut = standard_base_rendering_lut();
        for input in [
            1.0e-6,
            0.01,
            ROHDITOR_STANDARD_MIDDLE_GRAY,
            1.0,
            2.0,
            4.0,
            16.0,
        ] {
            let expected = evaluate_standard_v1(input);
            assert!(
                (lut.sample(input) - expected).abs() < 2.0e-5,
                "interpolation error at {input}"
            );
        }
    }

    #[test]
    fn highlights_progress_smoothly_toward_the_asymptote() {
        let lut = standard_base_rendering_lut();
        let values = [1.0, 2.0, 4.0, 16.0, 256.0].map(|input| lut.sample(input));
        assert!(values.windows(2).all(|pair| pair[1] > pair[0]));
        assert!(values.into_iter().all(|value| value < 1.0));
    }

    #[test]
    fn invalid_and_negative_inputs_follow_the_identity_safe_policy() {
        let lut = standard_base_rendering_lut();
        assert_eq!(lut.sample(-0.25), -0.25);
        assert_eq!(lut.sample(f32::NEG_INFINITY), f32::NEG_INFINITY);
        assert_eq!(lut.sample(f32::INFINITY), f32::INFINITY);
        assert!(lut.sample(f32::NAN).is_nan());
        assert!(lut.sample(f32::MAX).is_finite());
    }
}
