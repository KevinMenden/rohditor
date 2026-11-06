//! What a pixel of the working space (linear Rec.2020/D65) looks like to a range mask.
//!
//! Luminance ranges select on sRGB-encoded luminance, which is how the photographer judges
//! "bright" and "dark"; colour ranges select on OkLab distance, which tracks perceived colour
//! difference. The mask crate only sees these measured values (see `rohditor_mask::ReferenceRow`).

use std::sync::OnceLock;

use crate::REC2020_LUMINANCE;
use crate::matrix::{LINEAR_REC2020_TO_XYZ_D65, Matrix3, XYZ_D65_TO_LINEAR_SRGB};
use crate::oklab::linear_srgb_to_oklab;
use crate::transfer::linear_srgb_to_srgb;

/// sRGB-encoded luminance of a linear Rec.2020 pixel, clamped to `0..=1`.
///
/// Values above 1 (highlights after exposure) count as white and negatives as black. Non-finite
/// input gives NaN, which range masks treat as outside every range.
#[must_use]
pub fn rec2020_encoded_luminance([red, green, blue]: [f32; 3]) -> f32 {
    let luminance =
        red * REC2020_LUMINANCE[0] + green * REC2020_LUMINANCE[1] + blue * REC2020_LUMINANCE[2];
    if !luminance.is_finite() {
        f32::NAN
    } else if luminance >= 1.0 {
        // The transfer curve returns 0.99999994 at 1.0; white must be exactly 1 so that a
        // range ending at 1 includes it.
        1.0
    } else {
        linear_srgb_to_srgb(luminance.max(0.0))
    }
}

/// OkLab `[L, a, b]` of a linear Rec.2020 pixel, through linear sRGB. Colours outside the sRGB
/// gamut keep their (negative) sRGB components, so they stay distinct instead of being clipped.
#[must_use]
pub fn rec2020_to_oklab(rgb: [f32; 3]) -> [f32; 3] {
    static REC2020_TO_LINEAR_SRGB: OnceLock<Matrix3> = OnceLock::new();
    let matrix = REC2020_TO_LINEAR_SRGB
        .get_or_init(|| LINEAR_REC2020_TO_XYZ_D65.then(XYZ_D65_TO_LINEAR_SRGB));
    linear_srgb_to_oklab(matrix.transform(rgb))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutrals_have_no_chroma_and_encoded_luminance_follows_the_srgb_curve() {
        for level in [0.0, 0.18, 0.5, 1.0] {
            let [lightness, a, b] = rec2020_to_oklab([level; 3]);
            assert!(a.abs() < 1e-3 && b.abs() < 1e-3, "{level}: {a} {b}");
            assert!(
                (lightness - level.cbrt()).abs() < 2e-3,
                "{level}: {lightness}"
            );
            let encoded = rec2020_encoded_luminance([level; 3]);
            assert!((encoded - linear_srgb_to_srgb(level)).abs() < 1e-6);
        }
        // 18 % grey is about 46 % in sRGB encoding.
        assert!((rec2020_encoded_luminance([0.18; 3]) - 0.461).abs() < 0.005);
    }

    #[test]
    fn luminance_is_clamped_and_non_finite_input_is_not_selectable() {
        assert_eq!(rec2020_encoded_luminance([4.0, 4.0, 4.0]), 1.0);
        assert_eq!(rec2020_encoded_luminance([-1.0, -1.0, -1.0]), 0.0);
        assert!(rec2020_encoded_luminance([f32::NAN, 0.5, 0.5]).is_nan());
        assert!(rec2020_encoded_luminance([f32::INFINITY, 0.5, 0.5]).is_nan());
    }

    #[test]
    fn rec2020_primaries_keep_their_hue_ordering() {
        // The working-space red is more saturated than sRGB red but keeps its hue angle's side.
        let red = rec2020_to_oklab([1.0, 0.0, 0.0]);
        let green = rec2020_to_oklab([0.0, 1.0, 0.0]);
        let blue = rec2020_to_oklab([0.0, 0.0, 1.0]);
        assert!(red[1] > 0.2 && red[2] > 0.0, "{red:?}");
        assert!(green[1] < -0.2 && green[2] > 0.0, "{green:?}");
        assert!(blue[2] < -0.2, "{blue:?}");
        assert!(red.iter().chain(&green).chain(&blue).all(|v| v.is_finite()));
    }
}
