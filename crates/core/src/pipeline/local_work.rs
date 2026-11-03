//! Per-render preparation of local adjustments (mask layers) and the optional mask overlay.
//!
//! Both are resolved before the working-set check, so the bytes of prepared masks, tone tables
//! and per-thread row scratch count towards `MemoryEstimate` and the CPU limit.

use rohditor_edit::EditRecipe;
use rohditor_image::LinearRgbImage;

use crate::cpu::{LocalStage, MaskAlphaImage, MaskOverlayPlan};
use crate::{CancellationToken, OutputGeometry, PipelineError};

/// Mask layers and overlay request of one render, prepared for the developed frame.
#[derive(Debug, Default)]
pub(super) struct LocalWork {
    pub(super) stage: Option<LocalStage>,
    overlay: Option<MaskOverlayPlan>,
}

impl LocalWork {
    /// `mask_overlay` is the layer id a preview wants drawn; exports pass `None`.
    pub(super) fn prepare(
        image: &LinearRgbImage<f32>,
        recipe: &EditRecipe,
        geometry: OutputGeometry,
        mask_overlay: Option<u32>,
    ) -> Result<Self, PipelineError> {
        let (width, height) = (image.width(), image.height());
        let stage = LocalStage::prepare(recipe, width, height)?;
        let overlay = match mask_overlay {
            Some(id) => MaskOverlayPlan::prepare(recipe, id, (width, height), geometry)?,
            None => None,
        };
        Ok(Self { stage, overlay })
    }

    /// Bytes held beyond the image buffers while this work renders.
    pub(super) fn bytes(&self) -> usize {
        self.stage
            .as_ref()
            .map_or(0, LocalStage::bytes)
            .saturating_add(self.overlay.as_ref().map_or(0, MaskOverlayPlan::bytes))
    }

    /// The overlay image, rendered in display geometry; `None` if none was requested or the
    /// requested layer does not exist.
    pub(super) fn render_overlay(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Option<MaskAlphaImage>, PipelineError> {
        self.overlay
            .as_ref()
            .map(|overlay| overlay.render(cancellation))
            .transpose()
    }
}
