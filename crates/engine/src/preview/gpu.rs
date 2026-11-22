//! GPU preview resources that are tied to the frontend's wgpu device.
//!
//! The frontend injects its adapter, device, queue and display format as a
//! [`WgpuContext`]; nothing here creates a device of its own. The resulting
//! display texture is exposed as a plain `wgpu::TextureView` that the
//! frontend registers with whatever toolkit it uses.

use std::time::Instant;

use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_edit::{EditRecipe, OpticsAdjustments};
use rohditor_gpu::{
    GpuCapabilities, GpuDisplayReadbackPending, GpuPreviewFrame, GpuPreviewProcessor,
    GpuPreviewSource, GpuSpatialFullSource,
};
use tracing::info;

use crate::coordinator::PreviewResolution;
use crate::document::PreviewTicket;

/// The wgpu objects a frontend shares with the engine. All four must come
/// from the same device the frontend displays with.
#[derive(Clone)]
pub struct WgpuContext {
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// Format of the surface the frontend draws into.
    pub target_format: wgpu::TextureFormat,
}

/// A validated GPU preview processor on the frontend's device.
pub struct GpuRuntime {
    context: WgpuContext,
    processor: GpuPreviewProcessor,
}

impl GpuRuntime {
    /// Create the processor, refusing software adapters: GPU processing is
    /// intentionally limited to real hardware.
    pub fn new(context: WgpuContext) -> Result<Self, String> {
        let capabilities =
            GpuCapabilities::detect(&context.adapter, &context.device, context.target_format);
        if !capabilities.is_hardware_adapter() {
            return Err(format!(
                "wgpu selected the {} CPU adapter; GPU processing is intentionally disabled",
                capabilities.adapter_name
            ));
        }
        let processor = GpuPreviewProcessor::new(
            &context.adapter,
            &context.device,
            &context.queue,
            context.target_format,
        )
        .map_err(|error| error.to_string())?;
        let capabilities = processor.capabilities();
        info!(
            adapter = capabilities.adapter_name,
            backend = capabilities.backend,
            device_type = capabilities.device_type,
            target_format = capabilities.target_format,
            timestamp_queries = capabilities.timestamp_queries,
            "GPU preview processor initialized from the frontend's shared wgpu state"
        );
        Ok(Self { context, processor })
    }

    pub const fn context(&self) -> &WgpuContext {
        &self.context
    }

    pub const fn processor(&self) -> &GpuPreviewProcessor {
        &self.processor
    }
}

/// The resident GPU source a document renders from.
pub enum GpuDocumentSource {
    Reduced(Box<GpuPreviewSource>),
    Full(Box<GpuSpatialFullSource>),
}

impl GpuDocumentSource {
    pub fn estimated_bytes(&self) -> usize {
        match self {
            Self::Reduced(source) => source.estimated_bytes(),
            Self::Full(source) => usize::try_from(source.estimated_bytes()).unwrap_or(usize::MAX),
        }
    }

    pub fn supports_white_balance_draft(&self, recipe: &EditRecipe) -> bool {
        match self {
            Self::Reduced(source) => source.supports_white_balance_draft(recipe),
            // Source 1:1 is an inspection mode. Rebuild exact clipped pixels
            // after WB edits instead of presenting a knowingly stale draft.
            Self::Full(_) => false,
        }
    }

    pub fn matches_recipe(&self, recipe: &EditRecipe) -> bool {
        match self {
            Self::Reduced(source) => source.matches_recipe(recipe),
            // Returning to fit view deliberately rebuilds the reduced source.
            Self::Full(_) => false,
        }
    }

    pub fn optics_matches_recipe(&self, adjustments: &OpticsAdjustments) -> bool {
        match self {
            Self::Reduced(source) => source.optics_matches_recipe(adjustments),
            Self::Full(source) => source.optics_matches_recipe(adjustments),
        }
    }
}

/// A resident source plus the frame last rendered from it.
pub struct GpuDocumentPreview {
    pub ticket: PreviewTicket,
    pub algorithm: DemosaicAlgorithm,
    pub resolution: PreviewResolution,
    pub source: GpuDocumentSource,
    pub frame: GpuPreviewFrame,
}

/// A display readback for the histogram that the GPU has not finished yet.
pub(super) struct PendingGpuHistogram {
    pub(super) ticket: PreviewTicket,
    pub(super) readback: GpuDisplayReadbackPending,
    pub(super) started: Instant,
}
