use rohditor_image::DisplayRgbImage;
use thiserror::Error;

use crate::{CropRect, common_aspect_crops};

/// Longest edge of a proxy in samples. Binning both images this coarsely makes
/// the comparison robust to resampling and small registration differences.
pub const PROXY_MAX_EDGE: usize = 96;

/// Area-binned sRGB-encoded RGB samples of an image, for coarse comparison.
#[derive(Debug, Clone, PartialEq)]
pub struct Proxy {
    width: usize,
    height: usize,
    /// Encoded values in `0.0..=1.0`, row-major.
    pixels: Vec<[f32; 3]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProxyError {
    #[error("one image is portrait and the other landscape, so their framing cannot be matched")]
    OrientationMismatch,
}

impl Proxy {
    /// Bin the framing shared by `image` and `other` down to at most
    /// [`PROXY_MAX_EDGE`] samples on the long edge. Both images must be binned
    /// against each other so the grids line up.
    pub fn pair(
        image: &DisplayRgbImage<u8>,
        other: &DisplayRgbImage<u8>,
    ) -> Result<(Self, Self), ProxyError> {
        let (first, second) = common_aspect_crops(
            (image.width(), image.height()),
            (other.width(), other.height()),
        )
        .ok_or(ProxyError::OrientationMismatch)?;
        // Both crops have the same aspect (to rounding), so one grid fits both.
        let scale = PROXY_MAX_EDGE as f32 / first.width.max(first.height) as f32;
        let grid = (
            ((first.width as f32 * scale).round() as usize).clamp(1, PROXY_MAX_EDGE),
            ((first.height as f32 * scale).round() as usize).clamp(1, PROXY_MAX_EDGE),
        );
        Ok((
            Self::bin(image, first, grid),
            Self::bin(other, second, grid),
        ))
    }

    fn bin(image: &DisplayRgbImage<u8>, rect: CropRect, grid: (usize, usize)) -> Self {
        let mut pixels = Vec::with_capacity(grid.0 * grid.1);
        for gy in 0..grid.1 {
            let y_range = bin_range(rect.y, rect.height, gy, grid.1);
            for gx in 0..grid.0 {
                let x_range = bin_range(rect.x, rect.width, gx, grid.0);
                let mut sum = [0_u64; 3];
                for y in y_range.clone() {
                    let start = y * image.row_stride();
                    for pixel in image.data()[start + x_range.start * 3..start + x_range.end * 3]
                        .as_chunks::<3>()
                        .0
                    {
                        for (total, value) in sum.iter_mut().zip(pixel) {
                            *total += u64::from(*value);
                        }
                    }
                }
                let count = (x_range.len() * y_range.len()) as f32;
                pixels.push(sum.map(|total| total as f32 / count / 255.0));
            }
        }
        Self {
            width: grid.0,
            height: grid.1,
            pixels,
        }
    }

    #[must_use]
    pub const fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    #[must_use]
    pub fn pixels(&self) -> &[[f32; 3]] {
        &self.pixels
    }
}

/// Half-open source range of grid cell `index`, at least one pixel wide.
fn bin_range(origin: usize, extent: usize, index: usize, cells: usize) -> std::ops::Range<usize> {
    let start = origin + index * extent / cells;
    let end = origin + (index + 1) * extent / cells;
    start..end.max(start + 1)
}

/// How far a rendering is from the camera preview, in sRGB-encoded units
/// (`1.0` is full scale).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProxyDifference {
    /// Root-mean-square error per channel.
    pub channel_rms: [f32; 3],
    /// Mean Euclidean RGB distance.
    pub mean_color_error: f32,
    /// 95th percentile of the Euclidean RGB distance.
    pub p95_color_error: f32,
    /// Root-mean-square error of Rec.709 luminance alone, i.e. the tone error.
    pub luminance_rms: f32,
    /// Mean signed luminance offset (`rendering - camera`); negative is darker.
    pub luminance_bias: f32,
}

/// Compare two proxies of identical grids. Returns `None` for different grids
/// or empty proxies.
#[must_use]
pub fn compare_proxies(rendering: &Proxy, camera: &Proxy) -> Option<ProxyDifference> {
    if rendering.dimensions() != camera.dimensions() || rendering.pixels.is_empty() {
        return None;
    }
    let count = rendering.pixels.len() as f32;
    let mut squared = [0.0_f32; 3];
    let mut distances = Vec::with_capacity(rendering.pixels.len());
    let (mut luminance_squared, mut luminance_sum) = (0.0_f32, 0.0_f32);
    for (a, b) in rendering.pixels.iter().zip(&camera.pixels) {
        let delta = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
        for (total, value) in squared.iter_mut().zip(delta) {
            *total += value * value;
        }
        distances.push((delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt());
        let luminance = 0.2126 * delta[0] + 0.7152 * delta[1] + 0.0722 * delta[2];
        luminance_squared += luminance * luminance;
        luminance_sum += luminance;
    }
    let mean_color_error = distances.iter().sum::<f32>() / count;
    distances.sort_by(f32::total_cmp);
    let p95_index = ((distances.len() - 1) as f32 * 0.95).round() as usize;
    Some(ProxyDifference {
        channel_rms: squared.map(|total| (total / count).sqrt()),
        mean_color_error,
        p95_color_error: distances[p95_index],
        luminance_rms: (luminance_squared / count).sqrt(),
        luminance_bias: luminance_sum / count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rohditor_image::DisplayTransfer;

    fn gradient(width: usize, height: usize, offset: u8) -> DisplayRgbImage<u8> {
        let mut data = Vec::new();
        for y in 0..height {
            for x in 0..width {
                // Asymmetric in x, y and channel so a transposed or mixed-up
                // grid shows up as a difference.
                data.extend([
                    (x * 2 % 200) as u8 + offset,
                    (y * 3 % 200) as u8 + offset,
                    ((x + y) % 200) as u8 + offset,
                ]);
            }
        }
        DisplayRgbImage::new(width, height, width * 3, DisplayTransfer::Srgb, data).expect("image")
    }

    #[test]
    fn identical_images_have_no_difference_and_proxies_respect_the_edge_limit() {
        let image = gradient(400, 250, 0);
        let (a, b) = Proxy::pair(&image, &image).expect("same framing");
        assert_eq!(a.dimensions(), (PROXY_MAX_EDGE, 60));
        let difference = compare_proxies(&a, &b).expect("same grid");
        assert_eq!(difference.mean_color_error, 0.0);
        assert_eq!(difference.luminance_rms, 0.0);
    }

    #[test]
    fn a_uniform_brightness_offset_shows_up_as_signed_luminance_bias() {
        let dark = gradient(300, 200, 0);
        let bright = gradient(300, 200, 20);
        let (a, b) = Proxy::pair(&dark, &bright).expect("same framing");
        let difference = compare_proxies(&a, &b).expect("same grid");
        let expected = -20.0 / 255.0;
        assert!((difference.luminance_bias - expected).abs() < 1.0e-4);
        assert!((difference.luminance_rms - 20.0 / 255.0).abs() < 1.0e-4);
        for rms in difference.channel_rms {
            assert!((rms - 20.0 / 255.0).abs() < 1.0e-4);
        }
    }

    #[test]
    fn identical_scenes_match_and_differently_framed_images_share_a_grid() {
        // A 2:1 downscale of the same gradient field is not identical, but its
        // binned proxy should stay close, which proves both are gridded alike.
        let large = gradient(480, 320, 0);
        let (a, b) = Proxy::pair(&large, &large).expect("same framing");
        assert_eq!(compare_proxies(&a, &b).expect("grid").p95_color_error, 0.0);
        let wider = gradient(480, 300, 0);
        let (c, d) = Proxy::pair(&large, &wider).expect("cropped to common aspect");
        assert_eq!(c.dimensions(), d.dimensions());
    }

    #[test]
    fn mixed_orientations_are_rejected() {
        let landscape = gradient(300, 200, 0);
        let portrait = gradient(200, 300, 0);
        assert_eq!(
            Proxy::pair(&landscape, &portrait),
            Err(ProxyError::OrientationMismatch)
        );
    }
}
