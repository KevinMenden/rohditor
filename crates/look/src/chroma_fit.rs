use rohditor_color::srgb_to_linear_srgb;
use rohditor_edit::{CAMERA_CHROMA_KNOTS, CameraChroma};
use rohditor_image::DisplayRgbImage;
use thiserror::Error;

use crate::CropRect;

/// Gains outside this band are clamped: the fit explains the camera's
/// colourfulness, not tone-curve artefacts or clipped primaries.
const GAIN_LIMITS: (f32, f32) = (0.5, 2.0);
/// A knot needs this share of each image's pixels around it (hat-weighted) to
/// be trusted; sparse brightness ranges inherit the nearest trusted gain.
const MIN_KNOT_WEIGHT_SHARE: f64 = 0.002;
/// If the camera image is this much less colourful than the rendering overall
/// it is a black-and-white style, and only its tone is worth matching.
const MONOCHROME_RATIO: f64 = 0.1;

/// The chroma curve must reduce the mean colour error against the camera
/// preview by at least this fraction, or it is not worth applying.
const MIN_ERROR_REDUCTION: f32 = 0.05;

/// The fitted chroma curve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChromaFit {
    pub chroma: CameraChroma,
}

/// Why no chroma curve was produced. The caller keeps the tone-only look.
#[derive(Debug, Clone, Copy, PartialEq, Error)]
pub enum ChromaFitError {
    #[error("the camera preview is monochrome, so only its tone is matched")]
    Monochrome,
    #[error("the images share no brightness range with measurable colour")]
    NoMeasurableColor,
    #[error(
        "the chroma curve did not bring the colour closer to the camera ({before:.4} -> {after:.4})"
    )]
    NoImprovement { before: f32, after: f32 },
}

/// Acceptance gate for a fitted chroma curve, given the mean colour error
/// against the camera preview (see `compare_proxies`) before and after
/// applying it. Distribution matching cannot see hue shifts or local colour,
/// so a curve that does not measurably help on the actual pixels is refused.
pub fn check_chroma_improvement(before: f32, after: f32) -> Result<(), ChromaFitError> {
    if after <= before * (1.0 - MIN_ERROR_REDUCTION) {
        Ok(())
    } else {
        Err(ChromaFitError::NoImprovement { before, after })
    }
}

/// Fit the per-brightness saturation gain that makes `rendering` as colourful
/// as `camera`.
///
/// `rendering` must already carry the camera tone, so equal brightness bins
/// hold comparable pixels in both images. For each of the
/// [`CAMERA_CHROMA_KNOTS`] brightness positions the gain is the camera's
/// linear-light chroma-to-luminance ratio divided by the rendering's. Like the
/// tone fit it compares distributions, so no pixel alignment is needed.
pub fn fit_camera_chroma(
    rendering: &DisplayRgbImage<u8>,
    rendering_crop: CropRect,
    camera: &DisplayRgbImage<u8>,
    camera_crop: CropRect,
) -> Result<ChromaFit, ChromaFitError> {
    let linear: [f32; 256] = std::array::from_fn(|value| srgb_to_linear_srgb(value as f32 / 255.0));
    let rendering = ChromaStatistics::measure(rendering, rendering_crop, &linear);
    let camera = ChromaStatistics::measure(camera, camera_crop, &linear);

    let overall = |statistics: &ChromaStatistics| {
        statistics.chroma.iter().sum::<f64>() / statistics.luminance.iter().sum::<f64>().max(1e-12)
    };
    let (rendering_overall, camera_overall) = (overall(&rendering), overall(&camera));
    if rendering_overall <= 0.0 {
        return Err(ChromaFitError::NoMeasurableColor);
    }
    if camera_overall < MONOCHROME_RATIO * rendering_overall {
        return Err(ChromaFitError::Monochrome);
    }

    let mut gains: [Option<f32>; CAMERA_CHROMA_KNOTS] = [None; CAMERA_CHROMA_KNOTS];
    for (knot, gain) in gains.iter_mut().enumerate() {
        if rendering.trusted(knot) && camera.trusted(knot) {
            let rendering_ratio = rendering.chroma[knot] / rendering.luminance[knot];
            let camera_ratio = camera.chroma[knot] / camera.luminance[knot];
            if rendering_ratio > 0.0 {
                *gain = Some(
                    ((camera_ratio / rendering_ratio) as f32).clamp(GAIN_LIMITS.0, GAIN_LIMITS.1),
                );
            }
        }
    }
    let trusted: Vec<usize> = (0..CAMERA_CHROMA_KNOTS)
        .filter(|&knot| gains[knot].is_some())
        .collect();
    if trusted.is_empty() {
        return Err(ChromaFitError::NoMeasurableColor);
    }
    let filled = std::array::from_fn(|knot| {
        let nearest = trusted
            .iter()
            .copied()
            .min_by_key(|&other| other.abs_diff(knot))
            .unwrap_or(knot);
        gains[nearest].unwrap_or(1.0)
    });
    Ok(ChromaFit {
        chroma: CameraChroma { gains: filled },
    })
}

/// Chroma and luminance sums per brightness knot, splatted with hat weights
/// so neighbouring knots share the pixels between them.
struct ChromaStatistics {
    chroma: [f64; CAMERA_CHROMA_KNOTS],
    luminance: [f64; CAMERA_CHROMA_KNOTS],
    weight: [f64; CAMERA_CHROMA_KNOTS],
    pixels: f64,
}

impl ChromaStatistics {
    fn measure(image: &DisplayRgbImage<u8>, rect: CropRect, linear: &[f32; 256]) -> Self {
        let mut statistics = Self {
            chroma: [0.0; CAMERA_CHROMA_KNOTS],
            luminance: [0.0; CAMERA_CHROMA_KNOTS],
            weight: [0.0; CAMERA_CHROMA_KNOTS],
            pixels: 0.0,
        };
        let x_end = (rect.x + rect.width).min(image.width());
        let y_end = (rect.y + rect.height).min(image.height());
        for row in rect.y..y_end {
            let start = row * image.row_stride() + rect.x * 3;
            let end = row * image.row_stride() + x_end * 3;
            for pixel in image.data()[start..end].as_chunks::<3>().0 {
                statistics.add(*pixel, linear);
            }
        }
        statistics
    }

    fn add(&mut self, pixel: [u8; 3], linear: &[f32; 256]) {
        let [r, g, b] = pixel.map(|value| linear[usize::from(value)]);
        let luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let chroma =
            ((r - luminance).powi(2) + (g - luminance).powi(2) + (b - luminance).powi(2)).sqrt();
        // Bin by display brightness, the domain the gains are stored in.
        let encoded = (0.2126 * f32::from(pixel[0])
            + 0.7152 * f32::from(pixel[1])
            + 0.0722 * f32::from(pixel[2]))
            / 255.0;
        let position = encoded * (CAMERA_CHROMA_KNOTS - 1) as f32;
        let lower = (position.floor() as usize).min(CAMERA_CHROMA_KNOTS - 2);
        let upper_share = f64::from(position - lower as f32);
        for (knot, share) in [(lower, 1.0 - upper_share), (lower + 1, upper_share)] {
            self.chroma[knot] += share * f64::from(chroma);
            self.luminance[knot] += share * f64::from(luminance);
            self.weight[knot] += share;
        }
        self.pixels += 1.0;
    }

    fn trusted(&self, knot: usize) -> bool {
        self.weight[knot] >= MIN_KNOT_WEIGHT_SHARE * self.pixels && self.luminance[knot] > 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rohditor_color::linear_srgb_to_srgb;
    use rohditor_image::DisplayTransfer;

    /// Gradient over brightness with a fixed, asymmetric colour direction.
    /// `gain` scales the colour at a given linear luminance, as the camera would.
    fn gradient(gain: impl Fn(f32) -> f32) -> DisplayRgbImage<u8> {
        let (width, height) = (256, 8);
        let mut data = Vec::with_capacity(width * height * 3);
        for _ in 0..height {
            for x in 0..width {
                let encoded = 0.04 + 0.92 * x as f32 / (width - 1) as f32;
                let luminance = rohditor_color::srgb_to_linear_srgb(encoded);
                let direction = [0.45, -0.1, -0.35];
                let scale = gain(encoded);
                for channel in direction {
                    let value = luminance * (1.0 + 0.5 * channel * scale);
                    data.push((linear_srgb_to_srgb(value.clamp(0.0, 1.0)) * 255.0).round() as u8);
                }
            }
        }
        DisplayRgbImage::new(width, height, width * 3, DisplayTransfer::Srgb, data)
            .expect("gradient image")
    }

    #[test]
    fn recovers_a_brightness_dependent_gain() {
        // The camera is 1.6x as colourful in the shadows and 1.0x in highlights.
        let rendering = gradient(|_| 1.0);
        let camera = gradient(|encoded| 1.6 - 0.6 * encoded);
        let fit = fit_camera_chroma(
            &rendering,
            CropRect::full(&rendering),
            &camera,
            CropRect::full(&camera),
        )
        .expect("chroma fit");
        for (knot, gain) in fit.chroma.gains.iter().enumerate().take(8).skip(1) {
            let encoded = knot as f32 / 8.0;
            let expected = 1.6 - 0.6 * encoded;
            assert!(
                (gain - expected).abs() < 0.12,
                "knot {knot}: expected {expected}, got {gain}"
            );
        }
        assert!(fit.chroma.gains[1] > fit.chroma.gains[7]);
    }

    #[test]
    fn improvement_gate_requires_a_measurable_reduction() {
        assert!(check_chroma_improvement(0.10, 0.08).is_ok());
        assert!(check_chroma_improvement(0.10, 0.098).is_err());
        assert!(check_chroma_improvement(0.10, 0.12).is_err());
        assert!(check_chroma_improvement(0.10, f32::NAN).is_err());
    }

    #[test]
    fn identical_images_fit_unit_gain() {
        let image = gradient(|_| 1.0);
        let fit = fit_camera_chroma(
            &image,
            CropRect::full(&image),
            &image,
            CropRect::full(&image),
        )
        .expect("chroma fit");
        for gain in fit.chroma.gains {
            assert!((gain - 1.0).abs() < 1.0e-4);
        }
    }

    #[test]
    fn monochrome_camera_images_are_left_to_the_tone_fit() {
        let rendering = gradient(|_| 1.0);
        let grey = gradient(|_| 0.0);
        assert_eq!(
            fit_camera_chroma(
                &rendering,
                CropRect::full(&rendering),
                &grey,
                CropRect::full(&grey)
            ),
            Err(ChromaFitError::Monochrome)
        );
    }

    #[test]
    fn gains_are_clamped_to_a_modest_band() {
        let rendering = gradient(|_| 1.0);
        let camera = gradient(|_| 5.0);
        let fit = fit_camera_chroma(
            &rendering,
            CropRect::full(&rendering),
            &camera,
            CropRect::full(&camera),
        )
        .expect("chroma fit");
        assert!(fit.chroma.gains.iter().all(|gain| *gain <= GAIN_LIMITS.1));
    }
}
