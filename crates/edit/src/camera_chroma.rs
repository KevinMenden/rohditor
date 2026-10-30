use serde::{Deserialize, Serialize};

use crate::EditError;

/// Number of evenly spaced luminance positions where the gain is stored.
pub const CAMERA_CHROMA_KNOTS: usize = 9;

/// Smallest and largest accepted saturation gain.
pub const CAMERA_CHROMA_GAIN_MIN: f32 = 0.25;
pub const CAMERA_CHROMA_GAIN_MAX: f32 = 4.0;

/// Per-photo colourfulness of the camera's rendering as a function of
/// brightness: cameras make shadows more colourful and let highlights bleach.
///
/// `gains[i]` is a saturation multiplier at sRGB-encoded luminance
/// `i / (CAMERA_CHROMA_KNOTS - 1)`, interpolated linearly. The multiplier
/// scales each channel's distance from the pixel's luminance, like the global
/// Saturation control, so hue is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CameraChroma {
    pub gains: [f32; CAMERA_CHROMA_KNOTS],
}

impl CameraChroma {
    /// Typical camera colourfulness, used together with the static fallback
    /// tone. Median of the fits on the Sony ILCE-6400 corpus, with the gains
    /// halved toward 1.0: a fallback should only be mildly opinionated.
    pub const STATIC_FALLBACK: Self = Self {
        gains: [1.0, 1.09, 1.06, 1.05, 1.04, 1.0, 0.81, 0.75, 0.75],
    };

    pub fn validate(&self) -> Result<(), EditError> {
        for gain in self.gains {
            if !(CAMERA_CHROMA_GAIN_MIN..=CAMERA_CHROMA_GAIN_MAX).contains(&gain) {
                return Err(EditError {
                    field: "rendering.camera_chroma.gains",
                    reason: format!(
                        "{gain} is outside {CAMERA_CHROMA_GAIN_MIN}..={CAMERA_CHROMA_GAIN_MAX}"
                    ),
                });
            }
        }
        Ok(())
    }

    /// Saturation gain at an sRGB-encoded luminance; inputs outside `0..=1`
    /// use the nearest end gain.
    #[must_use]
    pub fn evaluate(&self, encoded_luminance: f32) -> f32 {
        let position = encoded_luminance.clamp(0.0, 1.0) * (CAMERA_CHROMA_KNOTS - 1) as f32;
        let lower = (position.floor() as usize).min(CAMERA_CHROMA_KNOTS - 2);
        let fraction = position - lower as f32;
        self.gains[lower] + (self.gains[lower + 1] - self.gains[lower]) * fraction
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_between_evenly_spaced_gains() {
        let chroma = CameraChroma {
            gains: [1.5, 1.4, 1.3, 1.2, 1.0, 0.9, 0.8, 0.7, 0.6],
        };
        assert_eq!(chroma.evaluate(0.0), 1.5);
        assert_eq!(chroma.evaluate(1.0), 0.6);
        assert!((chroma.evaluate(0.5) - 1.0).abs() < 1.0e-6);
        // Quarter of the way between the knots at 0.5 and 0.625.
        assert!((chroma.evaluate(0.5 + 0.125 / 4.0) - 0.975).abs() < 1.0e-6);
        assert_eq!(chroma.evaluate(-1.0), 1.5);
        assert_eq!(chroma.evaluate(2.0), 0.6);
    }

    #[test]
    fn rejects_gains_outside_the_accepted_range_or_non_finite() {
        let mut chroma = CameraChroma {
            gains: [1.0; CAMERA_CHROMA_KNOTS],
        };
        assert!(chroma.validate().is_ok());
        chroma.gains[3] = 9.0;
        assert!(chroma.validate().is_err());
        chroma.gains[3] = f32::NAN;
        assert!(chroma.validate().is_err());
    }
}
