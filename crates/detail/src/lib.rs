//! Shared detail filters on single-channel `f32` planes.
//!
//! Noise reduction, false-colour suppression, defringe and output sharpening
//! all need the same few building blocks: a wavelet decomposition, a median,
//! a Gaussian and a noise estimate. This crate provides them without knowing
//! anything about RAW metadata, edit recipes or UI state.

#![cfg_attr(test, allow(clippy::unwrap_used))]

mod atrous;
mod defringe;
mod denoise;
mod false_color;
mod gaussian;
mod median;
mod noise;
mod planes;
#[cfg(test)]
mod test_support;

pub use atrous::{Decomposition, atrous_decompose, atrous_halo, edge_avoiding_decompose};
pub use defringe::{FringeColour, FringeRemoval, defringe_scratch_bytes, remove_fringes};
pub use denoise::{
    ChromaDenoise, LumaDenoise, chroma_scratch_bytes, denoise_chroma, denoise_luma,
    estimate_luma_noise, luma_scratch_bytes,
};
pub use false_color::{FalseColor, false_color_scratch_bytes, suppress_false_color};
pub use gaussian::{gaussian_blur, gaussian_kernel};
pub use median::{median3, median5};
pub use noise::{NoiseProfile, estimate_noise};
pub use planes::{OpponentPlanes, Plane, luma_plane, merge_opponent, split_opponent};

use rohditor_image::ImageError;
use thiserror::Error;

/// Cancellation contract shared with callers that run detail filters in a
/// background worker. Checked once per row, so latency is one row of work.
pub trait CancellationCheck: Sync {
    fn is_cancelled(&self) -> bool;
}

impl<F> CancellationCheck for F
where
    F: Fn() -> bool + Sync,
{
    fn is_cancelled(&self) -> bool {
        self()
    }
}

/// A [`CancellationCheck`] that never cancels.
#[derive(Debug, Clone, Copy, Default)]
pub struct NeverCancelled;

impl CancellationCheck for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum DetailError {
    #[error("detail processing was cancelled")]
    Cancelled,
    #[error("plane dimensions {width}x{height} overflow or are empty")]
    InvalidDimensions { width: usize, height: usize },
    #[error("plane data has {actual} samples but {width}x{height} needs {expected}")]
    DataLength {
        width: usize,
        height: usize,
        expected: usize,
        actual: usize,
    },
    #[error("planes must have equal dimensions")]
    DimensionMismatch,
    #[error("invalid parameter {name}: {value}")]
    InvalidParameter { name: &'static str, value: f32 },
    #[error(transparent)]
    Image(#[from] ImageError),
}

/// Fold `index` into `0..length` by reflecting about the outer pixel
/// boundary (the edge pixel is repeated), also for 1-pixel planes.
pub(crate) fn mirror(index: isize, length: usize) -> usize {
    let period = (length * 2) as isize;
    let folded = index.rem_euclid(period) as usize;
    if folded < length {
        folded
    } else {
        length * 2 - 1 - folded
    }
}

/// Run `fill(y, row)` for every output row in parallel, honouring
/// cancellation per row. Returns `Cancelled` if any row was skipped.
pub(crate) fn for_each_row(
    out: &mut [f32],
    width: usize,
    cancellation: &dyn CancellationCheck,
    fill: impl Fn(usize, &mut [f32]) + Sync,
) -> Result<(), DetailError> {
    use rayon::prelude::*;
    out.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        if !cancellation.is_cancelled() {
            fill(y, row);
        }
    });
    if cancellation.is_cancelled() {
        Err(DetailError::Cancelled)
    } else {
        Ok(())
    }
}
