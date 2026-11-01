//! Noise reduction filters. Each works on interleaved RGB images and leaves
//! the parts of the signal it is not responsible for untouched.
//!
//! Run order is chroma, then luminance. Both are driven by the same luma
//! noise profile, so a caller estimates it once with
//! [`estimate_luma_noise`] and hands it to each filter.

mod chroma;
mod luma;

pub use chroma::{ChromaDenoise, chroma_scratch_bytes, denoise_chroma};
pub use luma::{LumaDenoise, denoise_luma, luma_scratch_bytes};

use rohditor_image::LinearRgbImage;

use crate::{CancellationCheck, DetailError, NoiseProfile, Plane, estimate_noise, luma_plane};

/// Noise profile of the balanced luma of `image` (`balance` as for the
/// filters that will use it). `None` when the image is too small to profile;
/// the noise filters then leave it alone.
pub fn estimate_luma_noise(
    image: &LinearRgbImage<f32>,
    balance: [f32; 3],
    cancellation: &dyn CancellationCheck,
) -> Result<Option<NoiseProfile>, DetailError> {
    let luma = sample_bands(&luma_plane(image, balance)?)?;
    match estimate_noise(&luma, &luma, cancellation) {
        Ok(profile) => Ok(Some(profile)),
        Err(DetailError::InvalidDimensions { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Rows per band and band period of [`sample_bands`].
const BAND_ROWS: usize = 64;
const BAND_PERIOD: usize = 256;

/// Every fourth 64-row band of `plane`, stacked. Small planes are returned
/// whole. Band seams only disturb the few rows the first à-trous level
/// reaches across, which the estimator's flat-tile selection ignores.
pub(crate) fn sample_bands(plane: &Plane) -> Result<Plane, DetailError> {
    let (w, h) = (plane.width(), plane.height());
    if h < 4 * BAND_PERIOD {
        return Ok(plane.clone());
    }
    let mut data = Vec::with_capacity(w * (h / BAND_PERIOD + 1) * BAND_ROWS);
    for start in (0..h).step_by(BAND_PERIOD) {
        let end = (start + BAND_ROWS).min(h);
        data.extend_from_slice(&plane.data()[start * w..end * w]);
    }
    let rows = data.len() / w;
    Plane::new(w, rows, data)
}
