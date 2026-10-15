//! Channel balancing around reconstruction.
//!
//! Gradient- and ratio-based algorithms (RCD, AMaZE) build red and blue from
//! green plus smoothed colour differences, and compare gradients across
//! channels. On raw data red and blue typically sit at 0.4-0.5x of green, so
//! green noise leaks into them and direction decisions are skewed. RawTherapee
//! and darktable therefore scale the mosaic by the white-balance multipliers
//! before demosaicing; Rohditor does the same and scales back afterwards, so
//! the result stays camera-native and the pipeline can apply the user's white
//! balance as a separate, cheap stage.
//!
//! The gains are normalized so the largest is 1 (darktable's convention). All
//! scaled samples then stay within the original range, which the algorithms'
//! `[0, 1]` limits rely on, and the channel that saturates first (smallest
//! gain) clips at `min(gain)` instead of at 1.

use rayon::prelude::*;
use rohditor_image::{MosaicImage, allocate_zeroed_f32};

use crate::{CancellationCheck, DemosaicError, WhiteBalanceGains, checkpoint};

#[derive(Debug, Clone, Copy)]
/// Channel gains normalized for reconstruction. Shared with GPU executors so
/// they scale exactly like the CPU reference.
pub struct ChannelBalance {
    /// Multipliers applied to the mosaic before reconstruction, largest = 1.
    forward: [f32; 3],
}

impl ChannelBalance {
    /// `gains` must be finite and positive (see [`WhiteBalanceGains::validate`]).
    #[must_use]
    pub fn new(gains: WhiteBalanceGains) -> Self {
        let largest = gains.red.max(gains.green).max(gains.blue);
        Self {
            forward: [
                gains.red / largest,
                gains.green / largest,
                gains.blue / largest,
            ],
        }
    }

    /// Multipliers applied to the mosaic before reconstruction; the largest is 1.
    #[must_use]
    pub const fn forward(self) -> [f32; 3] {
        self.forward
    }

    /// Multipliers that undo [`Self::forward`] on reconstructed RGB.
    #[must_use]
    pub fn inverse(self) -> [f32; 3] {
        self.forward.map(f32::recip)
    }

    pub(crate) fn is_identity(self) -> bool {
        self.forward == [1.0; 3]
    }

    /// Level at which the first channel saturates in the balanced mosaic.
    #[must_use]
    pub fn clip_point(self) -> f32 {
        self.forward[0].min(self.forward[1]).min(self.forward[2])
    }

    /// A copy of `mosaic` with each sample multiplied by its channel gain.
    pub(crate) fn scale_mosaic(
        self,
        mosaic: &MosaicImage<f32>,
        cancellation: &dyn CancellationCheck,
    ) -> Result<MosaicImage<f32>, DemosaicError> {
        let (width, height) = (mosaic.width(), mosaic.height());
        let mut data = allocate_zeroed_f32(width.checked_mul(height).ok_or_else(|| {
            DemosaicError::InvalidDimensions {
                width,
                height,
                row_stride: width,
                reason: "balanced mosaic sample count overflowed".to_owned(),
            }
        })?)?;
        let pattern = mosaic.pattern();
        data.par_chunks_mut(width)
            .zip(mosaic.data().par_chunks(mosaic.row_stride()))
            .enumerate()
            .try_for_each(|(y, (output, input))| {
                checkpoint(cancellation)?;
                for (x, (out, value)) in output.iter_mut().zip(input).enumerate() {
                    *out = value * self.forward[pattern.color_at(x, y).channel_index()];
                }
                Ok::<(), DemosaicError>(())
            })?;
        Ok(MosaicImage::new(width, height, width, pattern, data)?)
    }
}
