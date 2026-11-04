//! Per-render preparation of local adjustments (mask layers).
//!
//! Resolved before the working-set check, so the bytes of prepared masks, tone tables and
//! per-thread row scratch count towards `MemoryEstimate` and the CPU limit.

use rohditor_edit::EditRecipe;
use rohditor_image::LinearRgbImage;

use crate::PipelineError;
use crate::cpu::LocalStage;

/// Mask layers of one render, prepared for the developed frame.
#[derive(Debug, Default)]
pub(super) struct LocalWork {
    pub(super) stage: Option<LocalStage>,
}

impl LocalWork {
    pub(super) fn prepare(
        image: &LinearRgbImage<f32>,
        recipe: &EditRecipe,
    ) -> Result<Self, PipelineError> {
        Ok(Self {
            stage: LocalStage::prepare(recipe, image.width(), image.height())?,
        })
    }

    /// Bytes held beyond the image buffers while this work renders.
    pub(super) fn bytes(&self) -> usize {
        self.stage.as_ref().map_or(0, LocalStage::bytes)
    }
}
