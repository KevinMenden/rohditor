//! Pipeline adapter for normalized RAW highlight handling.

use rohditor_highlight::{
    ClipStats, HighlightExecution, HighlightExecutionOutput, OpposedStats, ReconstructionStats,
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
    Opposed(OpposedStats),
}

impl HighlightDiagnostics {
    /// Compatibility projection for callers that only understand Clip's
    /// original counters. Reconstruction diagnostics remain available through
    /// [`Self::local_ratios`] and [`Self::opposed`] and are never presented as
    /// Clip statistics by the current diagnostics path.
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
            Self::Opposed(stats) => ClipStats {
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
            Self::Off | Self::Clip(_) | Self::Opposed(_) => None,
        }
    }

    #[must_use]
    pub const fn opposed(self) -> Option<OpposedStats> {
        match self {
            Self::Opposed(stats) => Some(stats),
            Self::Off | Self::Clip(_) | Self::LocalRatios(_) => None,
        }
    }
}

/// Apply a pre-resolved RAW-stage highlight operation.
pub(crate) fn apply_cancellable(
    mosaic: MosaicImage<f32>,
    execution: HighlightExecution,
    cancellation: &CancellationToken,
) -> Result<HighlightOutput, PipelineError> {
    let span = tracing::info_span!(
        "cpu.highlight_processing",
        width = mosaic.width(),
        height = mosaic.height(),
        execution = ?execution,
    );
    let _guard = span.enter();

    match execution.apply_cancellable(mosaic, &|| cancellation.is_cancelled())? {
        HighlightExecutionOutput::Off(mosaic) => Ok(HighlightOutput {
            mosaic,
            diagnostics: HighlightDiagnostics::Off,
        }),
        HighlightExecutionOutput::Clip(output) => Ok(HighlightOutput {
            mosaic: output.mosaic,
            diagnostics: HighlightDiagnostics::Clip(output.stats),
        }),
        HighlightExecutionOutput::LocalRatios(output) => Ok(HighlightOutput {
            mosaic: output.mosaic,
            diagnostics: HighlightDiagnostics::LocalRatios(output.stats),
        }),
        HighlightExecutionOutput::Opposed(output) => Ok(HighlightOutput {
            mosaic: output.mosaic,
            diagnostics: HighlightDiagnostics::Opposed(output.stats),
        }),
    }
}
