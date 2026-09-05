use rayon::prelude::*;
use rohditor_image::MosaicImage;

use crate::detect::{is_affected, is_changed};
use crate::{ChannelClipLevels, ClipStats, HighlightError, checkpoint};

/// Output of the in-place destructive Clip operation.
#[derive(Debug, PartialEq)]
pub struct ClipOutput {
    pub mosaic: MosaicImage<f32>,
    pub stats: ClipStats,
}

/// Apply channel-aware highlight clipping without allocating another image-sized
/// buffer. Padding after the visible width is deliberately ignored.
pub fn clip(
    mosaic: MosaicImage<f32>,
    levels: ChannelClipLevels,
) -> Result<ClipOutput, HighlightError> {
    clip_cancellable(mosaic, levels, &|| false)
}

/// Cancellable form of [`clip`].
pub fn clip_cancellable(
    mut mosaic: MosaicImage<f32>,
    levels: ChannelClipLevels,
    cancellation: &dyn crate::CancellationCheck,
) -> Result<ClipOutput, HighlightError> {
    levels.validate()?;
    checkpoint(cancellation)?;
    let width = mosaic.width();
    let row_stride = mosaic.row_stride();
    let pattern = mosaic.pattern();
    let stats = mosaic
        .data_mut()
        .par_chunks_mut(row_stride)
        .enumerate()
        .try_fold(
            ClipStats::default,
            |mut row_stats, (y, output_row)| -> Result<ClipStats, HighlightError> {
                checkpoint(cancellation)?;
                for (x, sample) in output_row[..width].iter_mut().enumerate() {
                    if !sample.is_finite() {
                        return Err(HighlightError::NonFiniteSample { x, y });
                    }
                    let color = pattern.color_at(x, y);
                    let limit = levels.for_color(color);
                    if is_affected(*sample, limit) {
                        row_stats.affected_sites += 1;
                        row_stats.affected_by_channel[color.channel_index()] += 1;
                    }
                    if *sample > 1.0 {
                        row_stats.nominal_over_white_sites += 1;
                    }
                    if is_changed(*sample, limit) {
                        row_stats.changed_sites += 1;
                        *sample = limit;
                    }
                }
                Ok(row_stats)
            },
        )
        .try_reduce(
            ClipStats::default,
            |mut left, right| -> Result<ClipStats, HighlightError> {
                left.add_assign(right);
                Ok(left)
            },
        )?;
    checkpoint(cancellation)?;
    Ok(ClipOutput { mosaic, stats })
}
