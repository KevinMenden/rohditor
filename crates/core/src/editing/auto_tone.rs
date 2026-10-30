use rohditor_color::srgb_to_linear_srgb;
use rohditor_edit::{EXPOSURE_EV_RANGE, EditRecipe};

use crate::Histogram;

const EXPOSURE_EPSILON_EV: f32 = 0.01;

/// Heuristic exposure/blacks/highlights correction from the display histogram
/// of the current render. Returns `None` when the histogram has no usable
/// midtone (e.g. an all-black image).
#[must_use]
pub fn auto_tone(recipe: &EditRecipe, histogram: &Histogram) -> Option<EditRecipe> {
    let midpoint = histogram.luminance_percentile(0.5);
    let midpoint_linear = srgb_to_linear_srgb(f32::from(midpoint) / 255.0);
    if midpoint_linear <= 0.0 {
        return None;
    }
    let mut next = recipe.clone();
    // The histogram already includes the current recipe. Apply a correction
    // relative to the current exposure; assigning the correction as an
    // absolute value would make Auto tone move in the wrong direction after a
    // manual exposure edit.
    let exposure_correction = (0.18 / midpoint_linear).log2();
    if exposure_correction.abs() > EXPOSURE_EPSILON_EV {
        next.light.exposure_ev = (recipe.light.exposure_ev + exposure_correction)
            .clamp(EXPOSURE_EV_RANGE.minimum, EXPOSURE_EV_RANGE.maximum);
    }
    let low = histogram.luminance_percentile(0.01);
    let high = histogram.luminance_percentile(0.99);
    next.light.blacks = if low > 24 {
        -0.1
    } else if low == 0 {
        0.1
    } else {
        0.0
    };
    next.light.highlights = if high > 245 {
        -0.1
    } else if high < 220 {
        0.1
    } else {
        0.0
    };
    Some(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rohditor_edit::{BLACKS_RANGE, HIGHLIGHTS_RANGE};
    use rohditor_image::{DisplayRgbImage, DisplayTransfer};

    fn histogram_of(value: u8, width: usize) -> Histogram {
        let image = DisplayRgbImage::new(
            width,
            1,
            width * 3,
            DisplayTransfer::Srgb,
            vec![value; width * 3],
        )
        .expect("valid histogram fixture");
        Histogram::from_display_rgb8(&image)
    }

    #[test]
    fn applies_a_relative_exposure_correction() {
        let mut recipe = EditRecipe::default();
        recipe.light.exposure_ev = 1.0;

        let next = auto_tone(&recipe, &histogram_of(128, 4)).expect("usable midtone");
        let midpoint = srgb_to_linear_srgb(128.0 / 255.0);
        let expected = (1.0 + (0.18 / midpoint).log2())
            .clamp(EXPOSURE_EV_RANGE.minimum, EXPOSURE_EV_RANGE.maximum);
        assert!((next.light.exposure_ev - expected).abs() < 1.0e-6);
    }

    #[test]
    fn stays_finite_and_clamped_for_extreme_display_histograms() {
        for value in [0_u8, 8, 32, 240, 255] {
            let recipe = EditRecipe::default();
            let next = auto_tone(&recipe, &histogram_of(value, 8));
            assert_eq!(next.is_none(), value == 0);
            let next = next.unwrap_or(recipe);
            assert!(next.light.exposure_ev.is_finite());
            assert!(EXPOSURE_EV_RANGE.contains(next.light.exposure_ev));
            assert!(HIGHLIGHTS_RANGE.contains(next.light.highlights));
            assert!(BLACKS_RANGE.contains(next.light.blacks));
        }
    }

    #[test]
    fn is_stable_when_the_histogram_is_already_neutral() {
        let histogram = histogram_of(118, 4);
        let first = auto_tone(&EditRecipe::default(), &histogram).expect("usable midtone");
        assert_eq!(auto_tone(&first, &histogram).as_ref(), Some(&first));
    }
}
