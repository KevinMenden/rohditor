//! Pipeline adapter for the independently testable demosaic algorithms.
//!
//! Classic algorithms run in `rohditor-demosaic`. [`DemosaicAlgorithm::Neural`]
//! is only described there; it runs here through the pipeline's loaded
//! `rohditor-neural` model, with a classic result blended in near saturation.

use rohditor_demosaic::{DemosaicAlgorithm, WhiteBalanceGains};
use rohditor_image::{LinearRgbImage, MosaicImage};
use rohditor_neural::{NeuralDemosaicer, blend_highlights, needs_highlight_fallback};

use crate::{CancellationToken, PipelineError};

/// Classic algorithm the neural demosaic falls back to around clipped
/// highlights. Interactive previews also use it while the neural pass runs,
/// so the switch only changes the non-highlight areas.
pub const NEURAL_FALLBACK_DEMOSAIC: DemosaicAlgorithm = DemosaicAlgorithm::Amaze;

pub(crate) fn demosaic_cancellable(
    neural: Option<&NeuralDemosaicer>,
    mosaic: &MosaicImage<f32>,
    balance: WhiteBalanceGains,
    algorithm: DemosaicAlgorithm,
    cancellation: &CancellationToken,
) -> Result<LinearRgbImage<f32>, PipelineError> {
    let span = tracing::info_span!(
        "cpu.demosaic",
        width = mosaic.width(),
        height = mosaic.height(),
        algorithm = ?algorithm
    );
    let _guard = span.enter();
    if algorithm == DemosaicAlgorithm::Neural {
        let neural = neural.ok_or_else(|| PipelineError::NeuralUnavailable {
            reason: "no neural demosaic model is loaded".to_owned(),
        })?;
        return neural_cancellable(neural, mosaic, balance, cancellation);
    }
    classic_cancellable(mosaic, balance, algorithm, cancellation)
}

fn classic_cancellable(
    mosaic: &MosaicImage<f32>,
    balance: WhiteBalanceGains,
    algorithm: DemosaicAlgorithm,
    cancellation: &CancellationToken,
) -> Result<LinearRgbImage<f32>, PipelineError> {
    rohditor_demosaic::demosaic_cancellable(mosaic, balance, algorithm, &|| {
        cancellation.is_cancelled()
    })
    .map_err(Into::into)
}

/// The network reconstructs under the as-shot `balance` (the contract
/// normalizes it to green = 1). Its peak working set is the mosaic plus two
/// full RGB images while the highlight fallback is blended in.
fn neural_cancellable(
    neural: &NeuralDemosaicer,
    mosaic: &MosaicImage<f32>,
    balance: WhiteBalanceGains,
    cancellation: &CancellationToken,
) -> Result<LinearRgbImage<f32>, PipelineError> {
    balance.validate()?;
    let mut image =
        neural.reconstruct(mosaic, [balance.red, balance.green, balance.blue], &|| {
            cancellation.is_cancelled()
        })?;
    if needs_highlight_fallback(mosaic) {
        let classic = classic_cancellable(mosaic, balance, NEURAL_FALLBACK_DEMOSAIC, cancellation)?;
        blend_highlights(&mut image, &classic, mosaic)?;
    }
    cancellation.checkpoint()?;
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rohditor_image::BayerPattern;
    use rohditor_neural::{ModelId, NeuralError, TileRunner, TileTensor, Tiling};

    #[test]
    fn maps_algorithm_cancellation_to_the_pipeline_error() {
        let mosaic =
            MosaicImage::new(8, 8, 8, BayerPattern::Rggb, vec![0.5; 64]).expect("valid mosaic");
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = demosaic_cancellable(
            None,
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::MalvarHeCutler,
            &cancellation,
        )
        .expect_err("cancelled reconstruction must stop");
        assert!(matches!(error, PipelineError::Cancelled));
    }

    #[test]
    fn neural_without_a_model_is_unavailable() {
        let mosaic =
            MosaicImage::new(8, 8, 8, BayerPattern::Rggb, vec![0.5; 64]).expect("valid mosaic");
        let error = demosaic_cancellable(
            None,
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::Neural,
            &CancellationToken::new(),
        )
        .expect_err("no model");
        assert!(matches!(error, PipelineError::NeuralUnavailable { .. }));
    }

    /// A "network" that outputs a constant, so the classic fallback is visible.
    struct Constant(f32);
    impl TileRunner for Constant {
        fn run_tile(&self, input: &TileTensor) -> Result<TileTensor, NeuralError> {
            let (h, w) = (2 * input.height(), 2 * input.width());
            TileTensor::new(3, h, w, vec![self.0; 3 * h * w])
        }
    }

    #[test]
    fn clipped_highlights_take_the_classic_result() {
        let (width, height) = (40, 24);
        let mut samples = vec![0.2; width * height];
        // A saturated 4x4 block at x 20..24, y 8..12.
        for y in 8..12 {
            for x in 20..24 {
                samples[y * width + x] = 1.0;
            }
        }
        let mosaic =
            MosaicImage::new(width, height, width, BayerPattern::Rggb, samples).expect("mosaic");
        let neural = NeuralDemosaicer::new(
            Box::new(Constant(0.05)),
            ModelId {
                contract_version: 1,
                sha256: [0; 32],
            },
        )
        .with_tiling(Tiling::new(16, 16).expect("tiling"));
        let balance = WhiteBalanceGains {
            red: 2.0,
            green: 1.0,
            blue: 1.5,
        };
        let image = demosaic_cancellable(
            Some(&neural),
            &mosaic,
            balance,
            DemosaicAlgorithm::Neural,
            &CancellationToken::new(),
        )
        .expect("neural");
        let green = |x: usize, y: usize| image.pixel(x, y).expect("pixel")[1];
        // Far from the highlight: the network, undone by the green gain of 1.
        assert_eq!(green(2, 2), 0.05);
        // Inside it: the classic reconstruction of the clipped block.
        assert!((green(21, 9) - 1.0).abs() < 1e-5, "{}", green(21, 9));
    }
}
