//! Presence: texture, clarity and dehaze on a developed linear RGB image.
//!
//! These are the spatial counterparts of the tone sliders: they look at a pixel's surroundings
//! rather than only its own value, so they run on the whole image after every per-pixel
//! adjustment. All radii scale with the image's long edge, so a downscaled preview and a
//! full-size export of the same picture look alike.

mod blur;
mod dehaze;
mod local_contrast;

use rayon::prelude::*;
use rohditor_image::LinearRgbImage;

use crate::{CancellationCheck, DetailError, Plane};

/// The three sliders, each from -1 (soften / add haze) to +1, with 0 a no-op.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Presence {
    pub texture: f32,
    pub clarity: f32,
    pub dehaze: f32,
}

impl Presence {
    fn validate(self) -> Result<(), DetailError> {
        for (name, value) in [
            ("texture", self.texture),
            ("clarity", self.clarity),
            ("dehaze", self.dehaze),
        ] {
            if !value.is_finite() || !(-1.0..=1.0).contains(&value) {
                return Err(DetailError::InvalidParameter { name, value });
            }
        }
        Ok(())
    }
}

/// Scratch bytes [`apply_presence`] needs beyond the image itself: at its peak the encoded luma
/// plane plus the blur's working planes. `None` on overflow.
#[must_use]
pub fn presence_scratch_bytes(width: usize, height: usize) -> Option<usize> {
    width.checked_mul(height)?.checked_mul(6 * size_of::<f32>())
}

/// Apply `presence` to `image` in place, in the order dehaze, clarity, texture (coarse to
/// fine, so each stage sees the previous one's result). `luma_weights` are the image's
/// luminance coefficients. On error the image may be partly processed.
pub fn apply_presence(
    image: &mut LinearRgbImage<f32>,
    presence: Presence,
    luma_weights: [f32; 3],
    cancellation: &dyn CancellationCheck,
) -> Result<(), DetailError> {
    presence.validate()?;
    if presence.dehaze != 0.0 {
        dehaze::apply_dehaze(image, luma_weights, presence.dehaze, cancellation)?;
    }
    if presence.clarity != 0.0 {
        local_contrast::apply_band(
            image,
            luma_weights,
            local_contrast::CLARITY,
            presence.clarity,
            cancellation,
        )?;
    }
    if presence.texture != 0.0 {
        local_contrast::apply_band(
            image,
            luma_weights,
            local_contrast::TEXTURE,
            presence.texture,
            cancellation,
        )?;
    }
    Ok(())
}

/// The luma of every pixel, passed through `map`. Non-finite samples count as black.
fn luma_of(
    image: &LinearRgbImage<f32>,
    weights: [f32; 3],
    map: impl Fn(f32) -> f32 + Sync,
) -> Result<Plane, DetailError> {
    let (width, height, stride) = (image.width(), image.height(), image.row_stride());
    let mut luma = vec![0.0_f32; width * height];
    luma.par_chunks_mut(width).enumerate().for_each(|(y, out)| {
        let row = &image.data()[y * stride..y * stride + width * 3];
        for (out, pixel) in out.iter_mut().zip(row.as_chunks::<3>().0) {
            let value = pixel[0] * weights[0] + pixel[1] * weights[1] + pixel[2] * weights[2];
            *out = map(if value.is_finite() { value } else { 0.0 });
        }
    });
    Plane::new(width, height, luma)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NeverCancelled;
    use rohditor_image::LinearRgbSpace;

    const WEIGHTS: [f32; 3] = [0.2627, 0.6780, 0.0593];

    fn image(
        width: usize,
        height: usize,
        f: impl Fn(usize, usize) -> [f32; 3],
    ) -> LinearRgbImage<f32> {
        let data = (0..width * height)
            .flat_map(|i| f(i % width, i / width))
            .collect();
        LinearRgbImage::new(width, height, width * 3, LinearRgbSpace::Rec2020D65, data).unwrap()
    }

    /// A scene with real structure: blobs of colour and a dark floor, as dehaze needs.
    fn scene(x: usize, y: usize) -> [f32; 3] {
        let cell = ((x / 9) * 31 + (y / 7) * 17) % 7;
        let base = 0.02 + 0.5 * (cell as f32 / 6.0);
        [base, base * 0.8, if cell == 0 { 0.0 } else { base * 0.6 }]
    }

    fn hazy(clear: &LinearRgbImage<f32>, transmission: f32, airlight: f32) -> LinearRgbImage<f32> {
        let mut veiled = clear.clone();
        for value in veiled.data_mut().iter_mut() {
            *value = *value * transmission + airlight * (1.0 - transmission);
        }
        veiled
    }

    fn contrast(image: &LinearRgbImage<f32>) -> f32 {
        let lumas: Vec<f32> = image
            .data()
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| p[0] * WEIGHTS[0] + p[1] * WEIGHTS[1] + p[2] * WEIGHTS[2])
            .collect();
        let mean = lumas.iter().sum::<f32>() / lumas.len() as f32;
        (lumas.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / lumas.len() as f32).sqrt()
    }

    #[test]
    fn neutral_presence_changes_nothing() {
        let mut picture = image(50, 31, scene);
        let before = picture.clone();
        apply_presence(&mut picture, Presence::default(), WEIGHTS, &NeverCancelled).unwrap();
        assert_eq!(picture, before);
    }

    #[test]
    fn rejects_out_of_range_sliders() {
        let mut picture = image(8, 8, scene);
        for bad in [1.5, -1.5, f32::NAN] {
            let presence = Presence {
                clarity: bad,
                ..Presence::default()
            };
            assert!(apply_presence(&mut picture, presence, WEIGHTS, &NeverCancelled).is_err());
        }
    }

    #[test]
    fn dehaze_restores_contrast_lost_to_a_uniform_veil() {
        let clear = image(160, 96, scene);
        let veiled = hazy(&clear, 0.6, 0.8);
        let mut restored = veiled.clone();
        let presence = Presence {
            dehaze: 1.0,
            ..Presence::default()
        };
        apply_presence(&mut restored, presence, WEIGHTS, &NeverCancelled).unwrap();
        assert!(contrast(&restored) > contrast(&veiled) * 1.3);
        // The veil lifted the blacks; dehaze brings them back down.
        let darkest =
            |image: &LinearRgbImage<f32>| image.data().iter().copied().fold(f32::MAX, f32::min);
        assert!(darkest(&restored) < darkest(&veiled) * 0.7);
        assert!(restored.data().iter().all(|v| v.is_finite()));
    }

    #[test]
    fn negative_dehaze_adds_a_veil_that_lowers_contrast_and_lifts_blacks() {
        let source = image(160, 96, scene);
        let mut hazier = source.clone();
        let presence = Presence {
            dehaze: -1.0,
            ..Presence::default()
        };
        apply_presence(&mut hazier, presence, WEIGHTS, &NeverCancelled).unwrap();
        assert!(contrast(&hazier) < contrast(&source) * 0.8);
        let darkest =
            |image: &LinearRgbImage<f32>| image.data().iter().copied().fold(f32::MAX, f32::min);
        assert!(darkest(&hazier) > darkest(&source) + 0.05);
    }

    #[test]
    fn dehaze_is_equivariant_under_transposition() {
        // Asymmetric size so a swapped width/height anywhere in the stage shows up.
        let (w, h) = (130, 41);
        let mut original = hazy(&image(w, h, scene), 0.6, 0.8);
        let mut transposed = hazy(&image(h, w, |x, y| scene(y, x)), 0.6, 0.8);
        let presence = Presence {
            dehaze: 0.7,
            ..Presence::default()
        };
        apply_presence(&mut original, presence, WEIGHTS, &NeverCancelled).unwrap();
        apply_presence(&mut transposed, presence, WEIGHTS, &NeverCancelled).unwrap();
        for y in 0..h {
            for x in 0..w {
                let (a, b) = (
                    original.pixel(x, y).unwrap(),
                    transposed.pixel(y, x).unwrap(),
                );
                for c in 0..3 {
                    assert!(
                        (a[c] - b[c]).abs() < 2e-3,
                        "({x},{y}) channel {c}: {} vs {}",
                        a[c],
                        b[c]
                    );
                }
            }
        }
    }

    /// The same picture (defined on the unit square) at a given width, 1/100 as tall.
    fn unit_scene(width: usize) -> LinearRgbImage<f32> {
        let height = width / 100;
        image(width, height, |x, _| {
            let u = (x as f32 + 0.5) / width as f32;
            let tau = std::f32::consts::TAU;
            // Broad shapes, mid-scale ripples and fine grain, in unit-square coordinates.
            let v = 0.2
                + 0.12 * (3.0 * tau * u).sin()
                + 0.04 * (12.0 * tau * u).sin()
                + 0.02 * (40.0 * tau * u).sin();
            [v, v * 0.8, v * 0.6]
        })
    }

    /// Average each 2x2 block, so a double-size result can be compared with the smaller one.
    fn halve(image: &LinearRgbImage<f32>) -> Vec<f32> {
        let (w, h) = (image.width() / 2, image.height() / 2);
        let mut out = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                for c in 0..3 {
                    let sum: f32 = [(0, 0), (1, 0), (0, 1), (1, 1)]
                        .iter()
                        .map(|(dx, dy)| image.pixel(2 * x + dx, 2 * y + dy).unwrap()[c])
                        .sum();
                    out.push(sum / 4.0);
                }
            }
        }
        out
    }

    /// Render the unit scene `width` wide and at twice that, and require the effects (result
    /// minus source) to agree within `tolerance` of the effect's size.
    fn assert_same_look_at_double_size(presence: Presence, width: usize, tolerance: f32) {
        let source = unit_scene(width);
        let mut small = source.clone();
        apply_presence(&mut small, presence, WEIGHTS, &NeverCancelled).unwrap();
        let mut large = unit_scene(width * 2);
        apply_presence(&mut large, presence, WEIGHTS, &NeverCancelled).unwrap();

        let large = halve(&large);
        let (mut difference, mut effect) = (0.0_f32, 0.0_f32);
        for ((small, large), source) in small.data().iter().zip(&large).zip(source.data()) {
            difference += (small - large).abs();
            effect += (small - source).abs();
        }
        assert!(effect > 0.0, "{presence:?} did nothing");
        assert!(
            difference < tolerance * effect,
            "{presence:?} at {width} px: sizes differ by {difference} against an effect of {effect}"
        );
    }

    #[test]
    fn a_picture_looks_the_same_at_half_and_full_size() {
        // Radii are fractions of the long edge, so the effect of a preview must match that of
        // an export of twice the size.
        for presence in [
            Presence {
                texture: 0.8,
                ..Presence::default()
            },
            Presence {
                clarity: 0.8,
                ..Presence::default()
            },
            Presence {
                dehaze: 0.8,
                ..Presence::default()
            },
        ] {
            assert_same_look_at_double_size(presence, 3_200, 0.15);
        }
    }

    #[test]
    fn small_previews_keep_the_look_despite_the_minimum_blur() {
        // At 1000 px the texture blur would be 0.7 px; the 1 px floor is compensated by amount. The
        // box blur is coarse at this sigma, so the match is looser than at export sizes.
        let texture = Presence {
            texture: 0.8,
            ..Presence::default()
        };
        assert_same_look_at_double_size(texture, 1_000, 0.3);
    }

    #[test]
    fn all_three_together_stay_finite_and_honour_cancellation() {
        let mut picture = image(70, 45, scene);
        let everything = Presence {
            texture: 1.0,
            clarity: -0.5,
            dehaze: 0.5,
        };
        apply_presence(&mut picture, everything, WEIGHTS, &NeverCancelled).unwrap();
        assert!(picture.data().iter().all(|v| v.is_finite()));

        let mut picture = image(70, 45, scene);
        assert_eq!(
            apply_presence(&mut picture, everything, WEIGHTS, &|| true),
            Err(DetailError::Cancelled)
        );
    }
}
