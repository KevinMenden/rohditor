//! Pure decisions about which backend and base can serve a preview request.

use rohditor_core::NEURAL_FALLBACK_DEMOSAIC;
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_edit::EditRecipe;

use crate::coordinator::PreviewResolution;
use crate::document::PreviewIntent;

/// Whether the GPU pipeline implements every stage of `recipe`.
pub fn gpu_supports_recipe(recipe: &EditRecipe) -> bool {
    rohditor_gpu::GpuPreviewProcessor::supports_recipe(recipe)
}

/// Whether a resident GPU base can serve `requested`. While the background
/// neural pass runs, its classic stand-in base keeps serving neural requests;
/// once it is ready, the next request goes to the worker for the neural source.
pub fn gpu_base_algorithm_matches(
    resident: DemosaicAlgorithm,
    requested: DemosaicAlgorithm,
    neural_ready: bool,
) -> bool {
    resident == requested
        || (requested == DemosaicAlgorithm::Neural
            && resident == NEURAL_FALLBACK_DEMOSAIC
            && !neural_ready)
}

/// Whether a result at `resolution` answers the viewport's current `intent`.
pub const fn preview_intent_matches_resolution(
    intent: PreviewIntent,
    resolution: PreviewResolution,
) -> bool {
    matches!(
        (intent, resolution),
        (PreviewIntent::CommittedFit, PreviewResolution::Fit)
            | (PreviewIntent::SourceScale, PreviewResolution::SourceScale)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resident_gpu_bases_are_algorithm_specific() {
        assert!(gpu_base_algorithm_matches(
            DemosaicAlgorithm::Rcd,
            DemosaicAlgorithm::Rcd,
            false
        ));
        assert!(!gpu_base_algorithm_matches(
            DemosaicAlgorithm::Rcd,
            DemosaicAlgorithm::Amaze,
            false
        ));
        // The stand-in serves neural requests only until the neural pass is ready.
        assert!(gpu_base_algorithm_matches(
            NEURAL_FALLBACK_DEMOSAIC,
            DemosaicAlgorithm::Neural,
            false
        ));
        assert!(!gpu_base_algorithm_matches(
            NEURAL_FALLBACK_DEMOSAIC,
            DemosaicAlgorithm::Neural,
            true
        ));
    }

    #[test]
    fn fit_results_never_answer_a_source_scale_intent() {
        assert!(preview_intent_matches_resolution(
            PreviewIntent::CommittedFit,
            PreviewResolution::Fit
        ));
        assert!(!preview_intent_matches_resolution(
            PreviewIntent::CommittedFit,
            PreviewResolution::SourceScale
        ));
    }
}
