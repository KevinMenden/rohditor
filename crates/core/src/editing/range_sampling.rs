use rohditor_color::rec2020_to_oklab;
use rohditor_edit::{COLOR_SAMPLE_AB_RANGE, COLOR_SAMPLE_LIGHTNESS_RANGE, ColorSample, EditRecipe};
use rohditor_image::{LinearRgbImage, Orientation};

use crate::OutputGeometry;
use crate::cpu::ReferenceChain;

/// Half-width of the sampled patch: the patch is `2 * RADIUS + 1` pixels wide.
const PATCH_RADIUS: usize = 2;

/// The colour a colour-range mask should select at a point of the displayed image.
///
/// `image` is the linear Rec.2020 image as it enters the adjustment stage (a preview base),
/// `source_orientation` the file's, and `display_position` a position `0..1` over the image as
/// shown (oriented and cropped by `recipe`). The colour is read from the same reference the
/// masks select on (after the global exposure and base rendering, before Light and Color), so
/// the picked colour is exactly what the mask will compare against. The mean of a small patch is
/// used, so sensor noise does not pick a speckle.
///
/// `None` if the position is outside the image or the patch has no finite pixel.
#[must_use]
pub fn sample_color_range(
    image: &LinearRgbImage<f32>,
    source_orientation: Orientation,
    recipe: &EditRecipe,
    display_position: (f64, f64),
) -> Option<ColorSample> {
    if !(0.0..=1.0).contains(&display_position.0) || !(0.0..=1.0).contains(&display_position.1) {
        return None;
    }
    let (width, height) = (image.width(), image.height());
    let orientation = recipe
        .geometry
        .orientation_override
        .unwrap_or(source_orientation);
    let geometry = OutputGeometry::new(width, height, orientation, recipe.geometry.crop).ok()?;
    let (x, y) = geometry.display_to_developed(display_position);
    let centre_x = ((x * width as f64) as usize).min(width - 1);
    let centre_y = ((y * height as f64) as usize).min(height - 1);

    let chain = ReferenceChain::new(recipe);
    let mut total = [0.0f32; 3];
    let mut count = 0u32;
    for row in centre_y.saturating_sub(PATCH_RADIUS)..=(centre_y + PATCH_RADIUS).min(height - 1) {
        for column in
            centre_x.saturating_sub(PATCH_RADIUS)..=(centre_x + PATCH_RADIUS).min(width - 1)
        {
            let start = row * image.row_stride() + column * 3;
            let mut pixel = [
                image.data()[start],
                image.data()[start + 1],
                image.data()[start + 2],
            ];
            chain.apply(&mut pixel);
            if pixel.iter().all(|value| value.is_finite()) {
                for (total, value) in total.iter_mut().zip(pixel) {
                    *total += value;
                }
                count += 1;
            }
        }
    }
    if count == 0 {
        return None;
    }
    // Average in linear light, then convert: the mean of OkLab values is not the OkLab of the mean.
    let [lightness, a, b] = rec2020_to_oklab(total.map(|value| value / count as f32));
    let clamp = |value: f32, range: rohditor_edit::ParameterRange| {
        value.clamp(range.minimum, range.maximum)
    };
    Some(ColorSample {
        lightness: clamp(lightness, COLOR_SAMPLE_LIGHTNESS_RANGE),
        a: clamp(a, COLOR_SAMPLE_AB_RANGE),
        b: clamp(b, COLOR_SAMPLE_AB_RANGE),
    })
}

#[cfg(test)]
mod tests {
    use rohditor_edit::{NormalizedCropRect, RenderingProfileSelection};
    use rohditor_image::LinearRgbSpace;

    use super::*;

    const WIDTH: usize = 40;
    const HEIGHT: usize = 20;
    const RED: [f32; 3] = [0.5, 0.02, 0.02];
    const BLUE: [f32; 3] = [0.02, 0.05, 0.5];

    /// Red on the left half, blue on the right, with a wider row stride than the width.
    fn image() -> LinearRgbImage<f32> {
        let stride = WIDTH * 3 + 6;
        let mut data = vec![0.0; stride * HEIGHT];
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let colour = if x < WIDTH / 2 { RED } else { BLUE };
                data[y * stride + x * 3..y * stride + x * 3 + 3].copy_from_slice(&colour);
            }
        }
        LinearRgbImage::new(WIDTH, HEIGHT, stride, LinearRgbSpace::Rec2020D65, data)
            .expect("valid test image")
    }

    fn neutral() -> EditRecipe {
        let mut recipe = EditRecipe::default();
        recipe.rendering.profile = RenderingProfileSelection::NEUTRAL;
        recipe
    }

    fn sample(recipe: &EditRecipe, orientation: Orientation, at: (f64, f64)) -> ColorSample {
        sample_color_range(&image(), orientation, recipe, at).expect("sample")
    }

    fn sample_of(colour: [f32; 3]) -> ColorSample {
        let [lightness, a, b] = rec2020_to_oklab(colour);
        ColorSample { lightness, a, b }
    }

    fn close(left: ColorSample, right: ColorSample) -> bool {
        (left.lightness - right.lightness).abs() < 1e-4
            && (left.a - right.a).abs() < 1e-4
            && (left.b - right.b).abs() < 1e-4
    }

    #[test]
    fn picks_the_colour_under_the_pointer_through_orientation_and_crop() {
        let recipe = neutral();
        assert!(close(
            sample(&recipe, Orientation::Normal, (0.1, 0.5)),
            sample_of(RED)
        ));
        assert!(close(
            sample(&recipe, Orientation::Normal, (0.9, 0.5)),
            sample_of(BLUE)
        ));

        // Mirrored: the displayed left edge is the developed right edge.
        let mirrored = sample(&recipe, Orientation::HorizontalFlip, (0.1, 0.5));
        assert!(close(mirrored, sample_of(BLUE)), "{mirrored:?}");

        // Cropped to the right half: display x = 0.3 is developed x = 0.65, well inside the blue half.
        let mut cropped = neutral();
        cropped.geometry.crop = Some(NormalizedCropRect {
            left: 0.5,
            top: 0.0,
            right: 1.0,
            bottom: 1.0,
        });
        let right_half = sample(&cropped, Orientation::Normal, (0.3, 0.5));
        assert!(close(right_half, sample_of(BLUE)), "{right_half:?}");
    }

    #[test]
    fn reads_the_reference_not_the_final_look() {
        // The global exposure is part of the reference, Light and Color are not.
        let mut recipe = neutral();
        recipe.light.exposure_ev = 1.0;
        let lifted = sample(&recipe, Orientation::Normal, (0.1, 0.5));
        assert!(close(lifted, sample_of(RED.map(|v| v * 2.0))));
        recipe.light.contrast = 0.8;
        recipe.color.saturation = 1.8;
        assert!(close(
            sample(&recipe, Orientation::Normal, (0.1, 0.5)),
            lifted
        ));
    }

    #[test]
    fn rejects_positions_outside_the_image() {
        let recipe = neutral();
        for at in [(-0.1, 0.5), (0.5, 1.2), (f64::NAN, 0.5)] {
            assert!(sample_color_range(&image(), Orientation::Normal, &recipe, at).is_none());
        }
    }
}
