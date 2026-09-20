//! Shared-eframe GPU resource lifecycle and direct egui texture registration.
//!
//! The application owns the document/revision policy. This module owns only
//! resources that must stay tied to eframe's adapter, device, and renderer.

use eframe::egui;
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_gpu::{
    GpuCapabilities, GpuDisplayReadbackPending, GpuPreviewFrame, GpuPreviewProcessor,
    GpuPreviewSource, GpuSpatialFullSource,
};
use std::time::Instant;
use tracing::info;

use crate::ProcessorPreference;
use crate::coordinator::PreviewResolution;
use crate::document::PreviewTicket;

pub(super) enum GpuDocumentSource {
    Reduced(Box<GpuPreviewSource>),
    Full(Box<GpuSpatialFullSource>),
}

impl GpuDocumentSource {
    pub(super) fn estimated_bytes(&self) -> usize {
        match self {
            Self::Reduced(source) => source.estimated_bytes(),
            Self::Full(source) => usize::try_from(source.estimated_bytes()).unwrap_or(usize::MAX),
        }
    }

    pub(super) fn supports_white_balance_draft(&self, recipe: &rohditor_edit::EditRecipe) -> bool {
        match self {
            Self::Reduced(source) => source.supports_white_balance_draft(recipe),
            // Source 1:1 is an inspection mode. Rebuild exact clipped pixels
            // after WB edits instead of presenting a knowingly stale draft.
            Self::Full(_) => false,
        }
    }

    pub(super) fn matches_recipe(&self, recipe: &rohditor_edit::EditRecipe) -> bool {
        match self {
            Self::Reduced(source) => source.matches_recipe(recipe),
            // Returning to fit view deliberately rebuilds the reduced source.
            Self::Full(_) => false,
        }
    }

    pub(super) fn optics_matches_recipe(
        &self,
        adjustments: &rohditor_edit::OpticsAdjustments,
    ) -> bool {
        match self {
            Self::Reduced(source) => source.optics_matches_recipe(adjustments),
            Self::Full(source) => source.optics_matches_recipe(adjustments),
        }
    }
}

pub(super) struct GpuDocumentPreview {
    pub(super) ticket: PreviewTicket,
    pub(super) algorithm: DemosaicAlgorithm,
    pub(super) resolution: PreviewResolution,
    pub(super) source: GpuDocumentSource,
    pub(super) frame: GpuPreviewFrame,
    pub(super) texture_id: egui::TextureId,
}

pub(super) struct PendingGpuHistogram {
    pub(super) ticket: PreviewTicket,
    pub(super) readback: GpuDisplayReadbackPending,
    pub(super) started: Instant,
}

pub(super) struct GpuRuntime {
    pub(super) render_state: eframe::egui_wgpu::RenderState,
    pub(super) processor: GpuPreviewProcessor,
}

pub(super) fn initialize_gpu_runtime(
    context: &eframe::CreationContext<'_>,
    preference: ProcessorPreference,
) -> std::io::Result<(Option<GpuRuntime>, Option<String>)> {
    match preference {
        ProcessorPreference::Cpu => Ok((None, None)),
        ProcessorPreference::Auto => match create_gpu_runtime(context) {
            Ok(runtime) => Ok((Some(runtime), None)),
            Err(error) => Ok((
                None,
                Some(format!("GPU unavailable; using CPU preview ({error})")),
            )),
        },
        ProcessorPreference::Gpu => create_gpu_runtime(context)
            .map(|runtime| (Some(runtime), None))
            .map_err(std::io::Error::other),
    }
}

fn create_gpu_runtime(context: &eframe::CreationContext<'_>) -> Result<GpuRuntime, String> {
    let render_state = context.wgpu_render_state.clone().ok_or_else(|| {
        "the selected UI renderer does not expose a shared wgpu device".to_owned()
    })?;
    let capabilities = GpuCapabilities::detect(
        &render_state.adapter,
        &render_state.device,
        render_state.target_format,
    );
    if !capabilities.is_hardware_adapter() {
        return Err(format!(
            "wgpu selected the {} CPU adapter; GPU processing is intentionally disabled",
            capabilities.adapter_name
        ));
    }
    let processor = GpuPreviewProcessor::new(
        &render_state.adapter,
        &render_state.device,
        &render_state.queue,
        render_state.target_format,
    )
    .map_err(|error| error.to_string())?;
    let capabilities = processor.capabilities();
    info!(
        adapter = capabilities.adapter_name,
        backend = capabilities.backend,
        device_type = capabilities.device_type,
        target_format = capabilities.target_format,
        timestamp_queries = capabilities.timestamp_queries,
        "GPU preview processor initialized from eframe's shared wgpu state"
    );
    Ok(GpuRuntime {
        render_state,
        processor,
    })
}

pub(super) fn register_or_update_gpu_texture(
    runtime: &GpuRuntime,
    existing: Option<egui::TextureId>,
    frame: &GpuPreviewFrame,
) -> egui::TextureId {
    let mut renderer = runtime.render_state.renderer.write();
    if let Some(texture_id) = existing {
        renderer.update_egui_texture_from_wgpu_texture(
            &runtime.render_state.device,
            frame.display_view(),
            wgpu::FilterMode::Linear,
            texture_id,
        );
        texture_id
    } else {
        renderer.register_native_texture(
            &runtime.render_state.device,
            frame.display_view(),
            wgpu::FilterMode::Linear,
        )
    }
}

pub(super) fn gpu_output_size((width, height): (u32, u32)) -> egui::Vec2 {
    egui::vec2(width as f32, height as f32)
}
