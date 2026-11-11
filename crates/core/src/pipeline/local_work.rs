//! Per-render preparation of the adjustment stage: local adjustments (mask layers) and the
//! scratch of the presence stage.
//!
//! Resolved before the working-set check, so the bytes of prepared masks, tone tables,
//! per-thread row scratch, local detail copies and presence scratch count towards `MemoryEstimate` and the CPU
//! limit.

use rohditor_edit::EditRecipe;

use super::{DemosaicedBase, local_detail};
use crate::PipelineError;
use crate::cpu::{LocalStage, presence_stage_bytes};

/// Mask layers of one render, prepared for the developed frame.
#[derive(Debug, Default)]
pub(super) struct LocalWork {
    pub(super) stage: Option<LocalStage>,
    detail_bytes: usize,
    presence_bytes: usize,
}

impl LocalWork {
    pub(super) fn prepare(
        base: &DemosaicedBase,
        recipe: &EditRecipe,
    ) -> Result<Self, PipelineError> {
        let images = &base.local_detail;
        Ok(Self {
            stage: LocalStage::prepare(
                recipe,
                base.image.width(),
                base.image.height(),
                &|layer| local_detail::image_for_layer(images, recipe, layer),
            )?,
            detail_bytes: local_detail::images_bytes(images),
            presence_bytes: presence_stage_bytes(recipe, base.image.width(), base.image.height()),
        })
    }

    /// Bytes held beyond the image buffers while this work renders.
    pub(super) fn bytes(&self) -> usize {
        self.stage
            .as_ref()
            .map_or(0, LocalStage::bytes)
            .saturating_add(self.detail_bytes)
            .saturating_add(self.presence_bytes)
    }
}
