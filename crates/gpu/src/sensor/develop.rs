//! Shared RAW-to-resident-camera entry point for preview and export.
use rohditor_core::{
    CancellationToken, CpuPipeline, HighlightDiagnostics, PreviewOptions, SensorCameraMetadata,
    SpatialCompletionDescription, StageTimings,
};
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_edit::EditRecipe;
use rohditor_raw::RawFrame;

use super::{GpuSensorProcessor, normalize::invalid};
use crate::{GpuCapturedSource, GpuPreviewError, SpatialMetrics};

pub struct GpuDevelopedSource {
    captured: GpuCapturedSource,
    metadata: SensorCameraMetadata,
    diagnostics: HighlightDiagnostics,
}

impl GpuDevelopedSource {
    pub fn captured(&self) -> &GpuCapturedSource {
        &self.captured
    }

    /// Reuse camera pixels only for the same immutable RAW and sensor/capture
    /// contracts. Optics, reduction, output geometry, and color may change.
    pub fn describe(
        &self,
        cpu: &CpuPipeline,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: PreviewOptions,
        cancellation: &CancellationToken,
    ) -> Result<SpatialCompletionDescription, GpuPreviewError> {
        let metadata = self
            .metadata
            .with_recipe(frame, recipe, options)
            .map_err(|error| GpuPreviewError::BaseMismatch {
                reason: error.to_string(),
            })?;
        cpu.describe_sensor_completion(
            &metadata,
            self.diagnostics,
            StageTimings::default(),
            cancellation,
        )
        .map_err(pipeline_error)
    }

    pub fn initial_description(&self) -> &SpatialCompletionDescription {
        &self.captured.description
    }
}

impl GpuSensorProcessor {
    pub fn supports(description: &rohditor_core::SensorDevelopmentDescription) -> bool {
        matches!(
            description.highlight_execution(),
            rohditor_core::HighlightExecution::Off | rohditor_core::HighlightExecution::Clip(_)
        ) && matches!(
            description.demosaic().algorithm(),
            DemosaicAlgorithm::Bilinear
                | DemosaicAlgorithm::MalvarHeCutler
                | DemosaicAlgorithm::Rcd
        )
    }

    pub fn develop(
        &self,
        cpu: &CpuPipeline,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: PreviewOptions,
        cancellation: &CancellationToken,
    ) -> Result<(GpuDevelopedSource, SpatialMetrics), GpuPreviewError> {
        super::normalize::check_cancel(cancellation)?;
        let metadata = SensorCameraMetadata::new(frame, recipe, options).map_err(pipeline_error)?;
        if !Self::supports(metadata.sensor()) {
            return Err(GpuPreviewError::UnsupportedEdits {
                reason: format!(
                    "GPU sensor development does not support {} highlights with {} demosaic",
                    metadata.sensor().highlight_execution().stable_name(),
                    metadata.sensor().demosaic().stable_name()
                ),
            });
        }
        let contract = metadata.capture_contract().map_err(pipeline_error)?;
        let (normalized, normalization) =
            self.normalize(frame, metadata.sensor().normalization(), cancellation)?;
        let (highlighted, diagnostics, highlight) =
            self.apply_highlight(normalized, metadata.sensor(), cancellation)?;
        let (camera, demosaic) =
            self.demosaic(highlighted, metadata.sensor(), &contract, cancellation)?;
        // Generated capture tiles include demosaic dispatch in their timer.
        // Separate the overlapping durations before reporting stage totals.
        let mut capture = demosaic.capture;
        capture.total = capture
            .total
            .saturating_sub(demosaic.capture_input_demosaic);
        capture.compute_and_wait = capture
            .compute_and_wait
            .saturating_sub(demosaic.capture_input_demosaic);
        let demosaic_time = demosaic.total.saturating_sub(capture.total);
        let timings = StageTimings {
            normalization: normalization.total,
            highlight_processing: highlight.total,
            highlight_clipping: highlight.total,
            demosaic: demosaic_time,
            total: normalization.total + highlight.total + demosaic_time,
            ..Default::default()
        };
        let description = cpu
            .describe_sensor_completion(&metadata, diagnostics, timings, cancellation)
            .map_err(pipeline_error)?;
        let metrics = SpatialMetrics {
            sensor_gpu: true,
            capture,
            upload: normalization.upload,
            uploaded_bytes: normalization.uploaded_bytes,
            readback_bytes: highlight.diagnostic_readback_bytes
                + demosaic.diagnostic_readback_bytes,
            estimated_gpu_bytes: normalization
                .estimated_gpu_bytes
                .max(highlight.estimated_gpu_bytes)
                .max(demosaic.estimated_gpu_bytes),
            submissions: normalization.submissions + highlight.submissions + demosaic.submissions,
            ..Default::default()
        };
        let captured = GpuCapturedSource {
            planes: std::sync::Arc::new(camera.planes),
            description,
            capture_contract: contract,
            uploaded_bytes: normalization.uploaded_bytes,
        };
        Ok((
            GpuDevelopedSource {
                captured,
                metadata,
                diagnostics,
            },
            metrics,
        ))
    }
}

fn pipeline_error(error: rohditor_core::PipelineError) -> GpuPreviewError {
    if matches!(error, rohditor_core::PipelineError::Cancelled) {
        GpuPreviewError::Cancelled
    } else {
        invalid(&error.to_string())
    }
}
