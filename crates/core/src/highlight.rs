//! Pipeline adapter for normalized RAW highlight handling.

use rohditor_demosaic::WhiteBalanceGains;
use rohditor_edit::{HighlightAdjustments, HighlightMethod};
use rohditor_highlight::{
    ChannelClipLevels, ChannelDetectionLevels, ClipOutput, ClipStats, ReconstructionStats,
};
use rohditor_image::MosaicImage;

use crate::{CancellationToken, PipelineError};

pub(crate) struct HighlightOutput {
    pub mosaic: MosaicImage<f32>,
    pub diagnostics: HighlightDiagnostics,
}

/// Method-tagged diagnostics for the RAW-stage highlight operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HighlightDiagnostics {
    #[default]
    Off,
    Clip(ClipStats),
    LocalRatios(ReconstructionStats),
}

impl HighlightDiagnostics {
    /// Compatibility projection for callers that only understand Clip's
    /// original counters. Local-ratio diagnostics remain available through
    /// [`Self::local_ratios`] and are never presented as Clip statistics by
    /// the current diagnostics path.
    #[must_use]
    pub const fn legacy_clip_stats(self) -> ClipStats {
        match self {
            Self::Off => ClipStats {
                affected_sites: 0,
                changed_sites: 0,
                nominal_over_white_sites: 0,
                affected_by_channel: [0; 3],
            },
            Self::Clip(stats) => stats,
            Self::LocalRatios(stats) => ClipStats {
                affected_sites: stats.suspected_clipped_sites,
                changed_sites: stats.changed_sites,
                nominal_over_white_sites: 0,
                affected_by_channel: stats.suspected_by_channel,
            },
        }
    }

    #[must_use]
    pub const fn local_ratios(self) -> Option<ReconstructionStats> {
        match self {
            Self::LocalRatios(stats) => Some(stats),
            Self::Off | Self::Clip(_) => None,
        }
    }
}

/// Apply the selected RAW-stage highlight method. Clip uses limits that
/// produce a common post-white-balance ceiling; Local ratios uses independent
/// camera-native detection levels before white balance.
pub(crate) fn apply_cancellable(
    mosaic: MosaicImage<f32>,
    adjustments: HighlightAdjustments,
    gains: WhiteBalanceGains,
    cancellation: &CancellationToken,
) -> Result<HighlightOutput, PipelineError> {
    let span = tracing::info_span!(
        "cpu.highlight_processing",
        width = mosaic.width(),
        height = mosaic.height(),
        method = ?adjustments.method,
        clip_threshold = adjustments.clip.threshold,
        detection_threshold = adjustments.local_ratios.detection_threshold
    );
    let _guard = span.enter();

    if adjustments.method == HighlightMethod::Off {
        cancellation.checkpoint()?;
        return Ok(HighlightOutput {
            mosaic,
            diagnostics: HighlightDiagnostics::Off,
        });
    }

    match adjustments.method {
        HighlightMethod::Off => unreachable!("Off returned before method dispatch"),
        HighlightMethod::Clip => {
            let common_ceiling =
                adjustments.clip.threshold * gains.red.min(gains.green).min(gains.blue);
            let levels = ChannelClipLevels {
                red: common_ceiling / gains.red,
                green: common_ceiling / gains.green,
                blue: common_ceiling / gains.blue,
            };
            let ClipOutput { mosaic, stats } =
                rohditor_highlight::clip_cancellable(mosaic, levels, &|| {
                    cancellation.is_cancelled()
                })?;
            Ok(HighlightOutput {
                mosaic,
                diagnostics: HighlightDiagnostics::Clip(stats),
            })
        }
        HighlightMethod::LocalRatios => {
            let level = adjustments.local_ratios.detection_threshold;
            let levels = ChannelDetectionLevels {
                red: level,
                green: level,
                blue: level,
            };
            let output = rohditor_highlight::reconstruct_local_ratios_cancellable(
                mosaic,
                rohditor_highlight::LocalRatioOptions {
                    detection_levels: levels,
                },
                &|| cancellation.is_cancelled(),
            )?;
            Ok(HighlightOutput {
                mosaic: output.mosaic,
                diagnostics: HighlightDiagnostics::LocalRatios(output.stats),
            })
        }
    }
}
