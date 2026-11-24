//! Which colour of the mixer a point of the photo belongs to.

use rohditor_edit::{
    ColorGradingAdjustments, EditRecipe, HslAdjustments, LocalAdjustments, PresenceAdjustments,
};
use rohditor_image::{LinearRgbImage, Orientation};

use super::patch::developed_patch;
use crate::cpu::{apply_adjustments, hsl_channel_weights_from_working};

/// The mixer channel that affects the image most at a point.
///
/// `image` is the linear Rec.2020 image as it enters the adjustment stage (a
/// preview base) and `display_position` a position `0..1` over the image as shown.
/// The colour is judged as it enters the mixer: judging the displayed colour would
/// select another channel once the mixer has shifted its hue. Masks are left out.
///
/// `None` outside the image and where the colour is too neutral to have a hue.
#[must_use]
pub fn sample_mixer_channel(
    image: &LinearRgbImage<f32>,
    source_orientation: Orientation,
    recipe: &EditRecipe,
    display_position: (f64, f64),
) -> Option<usize> {
    let (columns, rows) = developed_patch(
        (image.width(), image.height()),
        source_orientation,
        &recipe.geometry,
        display_position,
    )?;
    let (width, height) = (columns.clone().count(), rows.clone().count());
    let mut pixels = Vec::with_capacity(width * height * 3);
    for row in rows {
        for column in columns.clone() {
            pixels.extend_from_slice(image.pixel(column, row)?);
        }
    }
    let mut patch = LinearRgbImage::new(width, height, width * 3, image.space(), pixels).ok()?;
    let mut before_mixer = recipe.clone();
    before_mixer.color.hsl = HslAdjustments::default();
    before_mixer.color.grading = ColorGradingAdjustments::default();
    before_mixer.presence = PresenceAdjustments::default();
    before_mixer.local = LocalAdjustments::default();
    apply_adjustments(&mut patch, &before_mixer).ok()?;

    let mut mean = [0.0; 3];
    for pixel in patch.data().as_chunks::<3>().0 {
        for (mean, value) in mean.iter_mut().zip(pixel) {
            *mean += value / (width * height) as f32;
        }
    }
    let weights = hsl_channel_weights_from_working(mean)?;
    (0..weights.len()).max_by(|left, right| weights[*left].total_cmp(&weights[*right]))
}

#[cfg(test)]
mod tests {
    use rohditor_edit::RenderingProfileSelection;
    use rohditor_image::LinearRgbSpace;

    use super::*;

    /// Orange on the left half, blue on the right.
    fn image() -> LinearRgbImage<f32> {
        let (width, height) = (20, 10);
        let row = (0..width).flat_map(|x| {
            if x < width / 2 {
                [0.6, 0.35, 0.1]
            } else {
                [0.1, 0.1, 0.6]
            }
        });
        let row: Vec<f32> = row.collect();
        let data = row.repeat(height);
        LinearRgbImage::new(width, height, width * 3, LinearRgbSpace::Rec2020D65, data)
            .expect("valid image")
    }

    fn neutral() -> EditRecipe {
        let mut recipe = EditRecipe::default();
        recipe.rendering.profile = RenderingProfileSelection::NEUTRAL;
        recipe
    }

    fn sample(recipe: &EditRecipe, x: f64) -> Option<usize> {
        sample_mixer_channel(&image(), Orientation::Normal, recipe, (x, 0.5))
    }

    #[test]
    fn the_channel_under_the_point_is_found() {
        assert_eq!(sample(&neutral(), 0.2), Some(1));
        assert_eq!(sample(&neutral(), 0.8), Some(5));
        assert_eq!(sample(&neutral(), 1.2), None);
    }

    #[test]
    fn a_shifted_hue_still_selects_the_channel_that_shifted_it() {
        let mut recipe = neutral();
        recipe.color.hsl.channels[1].hue = 1.0;
        recipe.color.grading.midtones = [-0.5, -0.5, 1.0];
        assert_eq!(sample(&recipe, 0.2), Some(1));
    }

    #[test]
    fn a_neutral_color_has_no_channel() {
        let mut recipe = neutral();
        recipe.color.saturation = 0.0;
        assert_eq!(sample(&recipe, 0.2), None);
    }
}
