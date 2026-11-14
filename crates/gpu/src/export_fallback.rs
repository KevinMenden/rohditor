//! Full-resolution export with an explicit GPU-or-CPU backend policy.
//!
//! Applications share this instead of each re-implementing device creation,
//! failure recovery, and the CPU reference fallback.

use rohditor_core::{
    CancellationToken, CpuPipeline, DitherMode, ExportRenderResult, OutputBitDepth,
    OutputSharpening, OutputSize, PipelineError, RenderOptions,
};
use rohditor_edit::EditRecipe;
use rohditor_raw::RawFrame;
use thiserror::Error;

use crate::{GpuCapabilities, GpuExportProcessor, GpuExportResult};

/// Which processor an export may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportBackend {
    Cpu,
    /// Try the GPU, then fall back to the CPU reference path.
    PreferGpu,
    /// Fail instead of falling back.
    RequireGpu,
}

/// The completed export and the backend that produced it.
// The GPU result is large, but a render is rare and short-lived.
#[allow(clippy::large_enum_variant)]
pub enum ExportRender {
    Gpu {
        result: GpuExportResult,
        capabilities: GpuCapabilities,
    },
    Cpu {
        result: ExportRenderResult,
        /// Why the GPU was skipped, when it was attempted.
        gpu_error: Option<String>,
    },
}

impl ExportRender {
    pub fn into_image(self) -> rohditor_core::ExportImage {
        match self {
            Self::Gpu { result, .. } => result.image,
            Self::Cpu { result, .. } => result.image,
        }
    }
}

#[derive(Debug, Error)]
pub enum ExportRenderError {
    #[error("GPU export was required: {0}")]
    GpuRequired(String),
    #[error(transparent)]
    Cpu(#[from] PipelineError),
}

/// Reusable exporter that owns a lazily created headless GPU device.
///
/// A failed render drops the device so the next export creates a fresh one.
#[derive(Default)]
pub struct FallbackExporter {
    gpu: Option<Result<GpuExportProcessor, String>>,
}

impl FallbackExporter {
    /// Drop any cached device, e.g. after a panic during rendering.
    pub fn reset(&mut self) {
        self.gpu = None;
    }

    /// Render under `backend`. `on_fallback` runs with the GPU failure reason
    /// just before the CPU path starts, so callers can report progress.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        backend: ExportBackend,
        pipeline: &CpuPipeline,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: RenderOptions,
        bit_depth: OutputBitDepth,
        dithering: DitherMode,
        size: OutputSize,
        sharpening: Option<OutputSharpening>,
        on_fallback: impl FnOnce(&str),
    ) -> Result<ExportRender, ExportRenderError> {
        let mut gpu_error = None;
        // A CPU-only stage is a routing choice, not a GPU failure: skip the
        // GPU without touching (or discarding) its device.
        if let Some(reason) = rohditor_core::cpu_only_stage_reason(recipe)
            && backend != ExportBackend::Cpu
        {
            if backend == ExportBackend::RequireGpu {
                return Err(ExportRenderError::GpuRequired(reason.to_owned()));
            }
            on_fallback(reason);
            gpu_error = Some(reason.to_owned());
        } else if backend != ExportBackend::Cpu {
            match self.render_gpu(
                pipeline, frame, recipe, options, bit_depth, dithering, size, sharpening,
            ) {
                Ok(rendered) => return Ok(rendered),
                Err(error) if backend == ExportBackend::RequireGpu => {
                    return Err(ExportRenderError::GpuRequired(error));
                }
                Err(error) => {
                    on_fallback(&error);
                    gpu_error = Some(error);
                }
            }
        }
        let result = pipeline.render_export(
            frame, recipe, options, bit_depth, dithering, size, sharpening,
        )?;
        Ok(ExportRender::Cpu { result, gpu_error })
    }

    #[allow(clippy::too_many_arguments)]
    fn render_gpu(
        &mut self,
        pipeline: &CpuPipeline,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: RenderOptions,
        bit_depth: OutputBitDepth,
        dithering: DitherMode,
        size: OutputSize,
        sharpening: Option<OutputSharpening>,
    ) -> Result<ExportRender, String> {
        let gpu = self
            .gpu
            .get_or_insert_with(|| GpuExportProcessor::headless().map_err(|e| e.to_string()))
            .as_mut()
            .map_err(|error| error.clone())?;
        let capabilities = gpu.capabilities().clone();
        let rendered = gpu.render(
            pipeline,
            frame,
            recipe,
            options,
            bit_depth,
            dithering,
            size,
            sharpening,
            &CancellationToken::new(),
        );
        match rendered {
            Ok(result) => Ok(ExportRender::Gpu {
                result,
                capabilities,
            }),
            Err(error) => {
                // A failed device must not poison later exports.
                self.gpu = None;
                Err(error.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rohditor_image::Orientation;

    use super::*;
    use crate::preview::processor::tests::synthetic_frame;

    /// A CPU-only stage (here presence) must route to the CPU without touching a GPU device,
    /// and must fail a GPU-required export instead of silently rendering it on the CPU.
    #[test]
    fn a_cpu_only_stage_never_reaches_the_gpu() {
        let frame = synthetic_frame(Orientation::Normal);
        let mut recipe = EditRecipe::default();
        recipe.presence.clarity = 0.5;
        let render = |backend| {
            let mut fallback = None;
            let result = FallbackExporter::default().render(
                backend,
                &CpuPipeline::default(),
                &frame,
                &recipe,
                RenderOptions::default(),
                OutputBitDepth::Eight,
                DitherMode::None,
                OutputSize::Original,
                None,
                |reason| fallback = Some(reason.to_owned()),
            );
            (result, fallback)
        };

        let (result, reported) = render(ExportBackend::PreferGpu);
        assert!(matches!(
            result,
            Ok(ExportRender::Cpu {
                gpu_error: Some(_),
                ..
            })
        ));
        assert!(reported.is_some_and(|reason| reason.contains("clarity")));

        let (result, _) = render(ExportBackend::RequireGpu);
        assert!(matches!(result, Err(ExportRenderError::GpuRequired(_))));
    }
}
