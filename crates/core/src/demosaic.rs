//! Pipeline adapter for the independently testable demosaic algorithms.

use rohditor_demosaic::{DemosaicAlgorithm, WhiteBalanceGains};
use rohditor_image::{LinearRgbImage, MosaicImage};

use crate::{CancellationToken, PipelineError};

pub(crate) fn demosaic_cancellable(
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
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
    rohditor_demosaic::demosaic_cancellable(mosaic, gains, algorithm, &|| {
        cancellation.is_cancelled()
    })
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rohditor_image::BayerPattern;

    #[test]
    fn maps_algorithm_cancellation_to_the_pipeline_error() {
        let mosaic =
            MosaicImage::new(8, 8, 8, BayerPattern::Rggb, vec![0.5; 64]).expect("valid mosaic");
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = demosaic_cancellable(
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::MalvarHeCutler,
            &cancellation,
        )
        .expect_err("cancelled reconstruction must stop");
        assert!(matches!(error, PipelineError::Cancelled));
    }
}
