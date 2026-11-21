//! Highlight fallback: near saturation, use the classic demosaic instead.
//!
//! The model was trained with saturated photosites at exactly 1.0 and never
//! saw reconstructed values above it, so near clipping (and wherever highlight
//! reconstruction pushed samples past 1) it is not trusted. Pixels whose
//! neighbourhood reaches [`FALLBACK_START`] blend towards the classic result,
//! reaching it fully at [`FALLBACK_FULL`].

use rohditor_image::{LinearRgbImage, MosaicImage};

use crate::NeuralError;

/// Normalized photosite level where the blend towards classic starts.
pub const FALLBACK_START: f32 = 0.90;
/// Level from which the classic result is used alone.
pub const FALLBACK_FULL: f32 = 0.97;
/// Neighbourhood radius: a pixel follows the brightest photosite within it,
/// which covers the network's own 3x3 bilinear support with a margin.
const RADIUS: usize = 2;

/// Whether any photosite reaches the fallback; if not, no classic demosaic is needed.
#[must_use]
pub fn needs_highlight_fallback(mosaic: &MosaicImage<f32>) -> bool {
    mosaic
        .data()
        .chunks(mosaic.row_stride())
        .any(|row| row[..mosaic.width()].iter().any(|&v| v >= FALLBACK_START))
}

/// Blend `neural` towards `classic` around near-saturated photosites of `mosaic`.
/// All three must have the same dimensions.
pub fn blend_highlights(
    neural: &mut LinearRgbImage<f32>,
    classic: &LinearRgbImage<f32>,
    mosaic: &MosaicImage<f32>,
) -> Result<(), NeuralError> {
    let (width, height) = (mosaic.width(), mosaic.height());
    if (neural.width(), neural.height()) != (width, height)
        || (classic.width(), classic.height()) != (width, height)
    {
        return Err(NeuralError::invalid_input(
            "highlight blending needs equally sized images",
        ));
    }
    let weights = fallback_weights(mosaic);
    let (stride, classic_stride) = (neural.row_stride(), classic.row_stride());
    let classic = classic.data();
    let neural = neural.data_mut();
    for y in 0..height {
        for x in 0..width {
            let weight = weights[y * width + x];
            if weight == 0.0 {
                continue;
            }
            for channel in 0..3 {
                let out = &mut neural[y * stride + 3 * x + channel];
                let fallback = classic[y * classic_stride + 3 * x + channel];
                *out += (fallback - *out) * weight;
            }
        }
    }
    Ok(())
}

/// Per-pixel classic weight in `[0, 1]`: a smoothstep of the brightest
/// photosite in the `(2 * RADIUS + 1)^2` neighbourhood (separable max).
fn fallback_weights(mosaic: &MosaicImage<f32>) -> Vec<f32> {
    let (width, height) = (mosaic.width(), mosaic.height());
    let stride = mosaic.row_stride();
    let data = mosaic.data();
    let window = |i: usize, n: usize| i.saturating_sub(RADIUS)..(i + RADIUS + 1).min(n);
    let mut row_max = vec![f32::NEG_INFINITY; width * height];
    for y in 0..height {
        let row = &data[y * stride..y * stride + width];
        for x in 0..width {
            row_max[y * width + x] = row[window(x, width)]
                .iter()
                .copied()
                .fold(f32::MIN, f32::max);
        }
    }
    let mut weights = vec![0.0; width * height];
    for y in 0..height {
        for x in 0..width {
            let peak = window(y, height)
                .map(|yy| row_max[yy * width + x])
                .fold(f32::MIN, f32::max);
            weights[y * width + x] = smoothstep(peak);
        }
    }
    weights
}

fn smoothstep(level: f32) -> f32 {
    let t = ((level - FALLBACK_START) / (FALLBACK_FULL - FALLBACK_START)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rohditor_image::{BayerPattern, LinearRgbSpace};

    fn rgb(width: usize, height: usize, value: f32) -> LinearRgbImage<f32> {
        LinearRgbImage::new(
            width,
            height,
            3 * width,
            LinearRgbSpace::CameraNative,
            vec![value; 3 * width * height],
        )
        .expect("rgb")
    }

    #[test]
    fn only_the_neighbourhood_of_clipped_photosites_falls_back() {
        let (width, height) = (12, 7);
        let mut samples = vec![0.3; width * height];
        samples[3 * width + 4] = 1.0; // one saturated photosite at (4, 3)
        let mosaic =
            MosaicImage::new(width, height, width, BayerPattern::Rggb, samples).expect("mosaic");
        assert!(needs_highlight_fallback(&mosaic));
        let mut neural = rgb(width, height, 0.2);
        blend_highlights(&mut neural, &rgb(width, height, 0.8), &mosaic).expect("blend");
        let value = |x: usize, y: usize| neural.pixel(x, y).expect("pixel")[1];
        assert_eq!(value(4, 3), 0.8);
        assert_eq!(value(6, 5), 0.8, "radius 2 reaches the corner");
        assert_eq!(value(7, 3), 0.2, "outside the radius");
        assert_eq!(value(4, 6), 0.2);
    }

    #[test]
    fn the_blend_ramps_between_start_and_full() {
        assert_eq!(smoothstep(0.5), 0.0);
        assert_eq!(smoothstep(FALLBACK_START), 0.0);
        assert!((smoothstep((FALLBACK_START + FALLBACK_FULL) / 2.0) - 0.5).abs() < 1e-5);
        assert_eq!(smoothstep(FALLBACK_FULL), 1.0);
        assert_eq!(smoothstep(3.0), 1.0, "reconstructed highlights above 1");
    }

    #[test]
    fn dark_images_need_no_fallback() {
        let mosaic = MosaicImage::new(4, 4, 4, BayerPattern::Rggb, vec![0.5; 16]).expect("mosaic");
        assert!(!needs_highlight_fallback(&mosaic));
    }
}
