//! Worker job behind the colour mixer's picker.

use std::sync::Arc;

use rohditor_core::{CancellationToken, CpuPipeline, PreviewOptions, sample_mixer_channel};
use rohditor_edit::EditRecipe;
use rohditor_raw::RawFrame;

use super::range_jobs::with_base;
use crate::document::PreviewTicket;
use crate::preview_cache::PreviewCache;

#[derive(Debug)]
pub struct MixerSampleJob {
    pub(super) ticket: PreviewTicket,
    pub(super) frame: Arc<RawFrame>,
    pub(super) recipe: EditRecipe,
    /// Position `0..1` over the image as shown.
    pub(super) position: (f64, f64),
    pub(super) options: PreviewOptions,
}

/// The mixer channel at the job's position.
pub(super) fn mixer_channel(
    job: &MixerSampleJob,
    pipeline: &CpuPipeline,
    preview_cache: &mut PreviewCache,
) -> Result<usize, String> {
    with_base(
        job.ticket,
        &job.frame,
        &job.recipe,
        job.options,
        &CancellationToken::new(),
        pipeline,
        preview_cache,
        |base| {
            sample_mixer_channel(
                base.image(),
                base.source_orientation(),
                &job.recipe,
                job.position,
            )
            .ok_or_else(|| "That color is too neutral for the color mixer".to_owned())
        },
    )
}
