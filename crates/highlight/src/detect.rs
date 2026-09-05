use rayon::prelude::*;
use rohditor_image::MosaicImage;

use crate::{ChannelClipLevels, ClippingMask, HighlightError, checkpoint};

pub(crate) fn is_affected(sample: f32, limit: f32) -> bool {
    sample >= limit
}

pub(crate) fn is_changed(sample: f32, limit: f32) -> bool {
    sample > limit
}

/// Materialize the same affected-site classification used by [`crate::clip`].
pub fn detect_clipping(
    mosaic: &MosaicImage<f32>,
    levels: ChannelClipLevels,
) -> Result<ClippingMask, HighlightError> {
    detect_clipping_cancellable(mosaic, levels, &|| false)
}

/// Cancellable form of [`detect_clipping`].
pub fn detect_clipping_cancellable(
    mosaic: &MosaicImage<f32>,
    levels: ChannelClipLevels,
    cancellation: &dyn crate::CancellationCheck,
) -> Result<ClippingMask, HighlightError> {
    levels.validate()?;
    checkpoint(cancellation)?;
    let elements = mosaic
        .row_stride()
        .checked_mul(mosaic.height())
        .ok_or_else(|| {
            HighlightError::Image(rohditor_image::ImageError::InvalidDimensions {
                width: mosaic.width(),
                height: mosaic.height(),
                row_stride: mosaic.row_stride(),
                reason: "clipping mask sample count overflowed".to_owned(),
            })
        })?;
    let mut data = Vec::new();
    data.try_reserve_exact(elements)
        .map_err(|_| HighlightError::Image(rohditor_image::ImageError::Allocation { elements }))?;
    data.resize(elements, false);

    data.par_chunks_mut(mosaic.row_stride())
        .enumerate()
        .try_for_each(|(y, output_row)| -> Result<(), HighlightError> {
            checkpoint(cancellation)?;
            for (x, marked) in output_row.iter_mut().take(mosaic.width()).enumerate() {
                let sample = *mosaic.sample(x, y);
                if !sample.is_finite() {
                    return Err(HighlightError::NonFiniteSample { x, y });
                }
                *marked = is_affected(sample, levels.for_color(mosaic.pattern().color_at(x, y)));
            }
            Ok(())
        })?;
    checkpoint(cancellation)?;
    Ok(ClippingMask::new(
        mosaic.width(),
        mosaic.height(),
        mosaic.row_stride(),
        data,
    ))
}
