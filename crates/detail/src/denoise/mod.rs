//! Noise reduction filters. Each works on interleaved RGB images and leaves
//! the parts of the signal it is not responsible for untouched.
//!
//! Run order is chroma, then luminance. Both are driven by the noise
//! profiles of the image the detail stage started from, so a caller
//! estimates them once with [`estimate_noise_profiles`] and hands them to
//! each filter. Estimating them from the stage input (rather than from the
//! output of the previous step) lets executors that process the image in
//! tiles profile it once up front.

mod chroma;
mod luma;

pub use chroma::{
    CHROMA_HALO, CHROMA_LEVELS, CHROMA_RANGE_SIGMAS, ChromaDenoise, ChromaLevel, chroma_levels,
    chroma_scratch_bytes, denoise_chroma,
};
pub use luma::{
    LUMA_FLOOR, LUMA_HALO, LUMA_LEVELS, LUMA_MAX_GAIN, LUMA_SIGNAL_FLOOR, LUMA_WINDOW_RADIUS,
    LumaDenoise, LumaLevel, denoise_luma, luma_levels, luma_scratch_bytes,
};

use std::borrow::Cow;
use std::ops::Range;

use rohditor_image::LinearRgbImage;

use crate::{CancellationCheck, DetailError, NoiseProfile, Plane, estimate_noise, luma_plane};

/// Noise of the balanced luma and of the two colour differences `R-G`, `B-G`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoiseProfiles {
    pub luma: NoiseProfile,
    pub chroma: [NoiseProfile; 2],
}

/// Noise profile of the balanced luma of `image` (`balance` as for the
/// filters that will use it). `None` when the image is too small to profile;
/// the noise filters then leave it alone.
pub fn estimate_luma_noise(
    image: &LinearRgbImage<f32>,
    balance: [f32; 3],
    cancellation: &dyn CancellationCheck,
) -> Result<Option<NoiseProfile>, DetailError> {
    let sampled = sample_rows(image)?;
    let luma = luma_plane(&sampled, balance)?;
    profile_or_none(estimate_noise(&luma, &luma, cancellation))
}

/// Luma and colour-difference noise profiles of `image`, or `None` when it
/// is too small to profile (the noise filters then leave it alone).
pub fn estimate_noise_profiles(
    image: &LinearRgbImage<f32>,
    balance: [f32; 3],
    cancellation: &dyn CancellationCheck,
) -> Result<Option<NoiseProfiles>, DetailError> {
    let sampled = sample_rows(image)?;
    estimate_noise_profiles_sampled(&sampled, balance, cancellation)
}

/// [`estimate_noise_profiles`] for rows already gathered with
/// [`noise_sample_rows`] (stacked in order). Executors whose image lives
/// elsewhere read back just those rows.
pub fn estimate_noise_profiles_sampled(
    sampled: &LinearRgbImage<f32>,
    balance: [f32; 3],
    cancellation: &dyn CancellationCheck,
) -> Result<Option<NoiseProfiles>, DetailError> {
    let planes = crate::split_opponent(sampled, balance)?;
    // Non-finite samples would poison the estimate; the filters leave such
    // pixels alone, so profile them as zero.
    let finite = |mut plane: Plane| {
        plane
            .data_mut()
            .iter_mut()
            .filter(|v| !v.is_finite())
            .for_each(|v| *v = 0.0);
        plane
    };
    let (luma, c1, c2) = (finite(planes.luma), finite(planes.c1), finite(planes.c2));
    let estimates = (
        profile_or_none(estimate_noise(&luma, &luma, cancellation))?,
        profile_or_none(estimate_noise(&c1, &luma, cancellation))?,
        profile_or_none(estimate_noise(&c2, &luma, cancellation))?,
    );
    Ok(match estimates {
        (Some(luma), Some(c1), Some(c2)) => Some(NoiseProfiles {
            luma,
            chroma: [c1, c2],
        }),
        _ => None,
    })
}

fn profile_or_none(
    estimate: Result<NoiseProfile, DetailError>,
) -> Result<Option<NoiseProfile>, DetailError> {
    match estimate {
        Ok(profile) => Ok(Some(profile)),
        Err(DetailError::InvalidDimensions { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Rows per band and band period of [`noise_sample_rows`].
const BAND_ROWS: usize = 64;
const BAND_PERIOD: usize = 256;

/// The rows of a `height`-row image the noise estimate reads: every fourth
/// 64-row band. Small images are read whole. Band seams only disturb the few
/// rows the first à-trous level reaches across, which the estimator's
/// flat-tile selection ignores.
#[must_use]
#[allow(clippy::single_range_in_vec_init)] // one band of rows, not a list of indices
pub fn noise_sample_rows(height: usize) -> Vec<Range<usize>> {
    if height < 4 * BAND_PERIOD {
        return vec![0..height];
    }
    (0..height)
        .step_by(BAND_PERIOD)
        .map(|start| start..(start + BAND_ROWS).min(height))
        .collect()
}

/// The [`noise_sample_rows`] of `image`, stacked (borrowed when that is the
/// whole image).
fn sample_rows(image: &LinearRgbImage<f32>) -> Result<Cow<'_, LinearRgbImage<f32>>, DetailError> {
    let rows = noise_sample_rows(image.height());
    if rows.len() == 1 && rows[0].len() == image.height() {
        return Ok(Cow::Borrowed(image));
    }
    let width = image.width() * 3;
    let mut data = Vec::with_capacity(rows.iter().map(Range::len).sum::<usize>() * width);
    for y in rows.into_iter().flatten() {
        data.extend_from_slice(&image.data()[y * image.row_stride()..][..width]);
    }
    let height = data.len() / width;
    Ok(Cow::Owned(LinearRgbImage::new(
        image.width(),
        height,
        width,
        image.space(),
        data,
    )?))
}
