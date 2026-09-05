//! Pipeline adapter for normalized RAW highlight handling.

use rohditor_demosaic::WhiteBalanceGains;
use rohditor_edit::{HighlightAdjustments, HighlightMethod};
use rohditor_highlight::{ChannelClipLevels, ClipOutput, ClipStats};
use rohditor_image::MosaicImage;

use crate::{CancellationToken, PipelineError};

pub(crate) struct HighlightOutput {
    pub mosaic: MosaicImage<f32>,
    pub stats: ClipStats,
}

/// Apply the selected RAW-stage highlight method using limits that produce a
/// common post-white-balance ceiling.
pub(crate) fn apply_cancellable(
    mosaic: MosaicImage<f32>,
    adjustments: HighlightAdjustments,
    gains: WhiteBalanceGains,
    cancellation: &CancellationToken,
) -> Result<HighlightOutput, PipelineError> {
    let span = tracing::info_span!(
        "cpu.highlight_clipping",
        width = mosaic.width(),
        height = mosaic.height(),
        method = ?adjustments.method,
        threshold = adjustments.threshold
    );
    let _guard = span.enter();

    if adjustments.method == HighlightMethod::Off {
        cancellation.checkpoint()?;
        return Ok(HighlightOutput {
            mosaic,
            stats: ClipStats::default(),
        });
    }

    let common_ceiling = adjustments.threshold * gains.red.min(gains.green).min(gains.blue);
    let levels = ChannelClipLevels {
        red: common_ceiling / gains.red,
        green: common_ceiling / gains.green,
        blue: common_ceiling / gains.blue,
    };
    let ClipOutput { mosaic, stats } =
        rohditor_highlight::clip_cancellable(mosaic, levels, &|| cancellation.is_cancelled())?;
    Ok(HighlightOutput { mosaic, stats })
}
