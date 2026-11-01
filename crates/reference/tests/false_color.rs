//! Regression bounds for false-colour suppression on the synthetic scenes.
//!
//! The filter is the plain RawTherapee-style colour-difference median, so
//! the bounds are set from what it achieves (with some margin), not from the
//! original 60% / 5% goals of issue #73; the gap to those goals is tracked
//! in a follow-up issue. They stop the filter, or the median it is built on,
//! from silently getting worse.

use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_detail::{FalseColor, NeverCancelled, suppress_false_color};
use rohditor_reference::detail_scenes::{DetailScene, Scene, synthetic_set};
use rohditor_reference::metrics::detail_scores;

/// Same geometry as `rohditor-cli detail-compare`.
const WIDTH: usize = 768;
const HEIGHT: usize = 512;
const MARGIN: usize = 16;
const STEPS: u8 = 2;

/// Chroma RMSE of the clean scene before and after suppression.
fn chroma_rmse(scene: &Scene, algorithm: DemosaicAlgorithm) -> (f64, f64) {
    let input = scene
        .noisy_input(None, 1, algorithm)
        .expect("demosaiced scene");
    let mut output = input.clone();
    suppress_false_color(
        &mut output,
        FalseColor {
            steps: STEPS,
            balance: [1.0; 3],
            ceilings: [f32::INFINITY; 3],
        },
        &NeverCancelled,
    )
    .expect("suppression");
    (
        detail_scores(&scene.truth, &input, MARGIN).chroma_rmse,
        detail_scores(&scene.truth, &output, MARGIN).chroma_rmse,
    )
}

fn scene(kind: DetailScene) -> Scene {
    synthetic_set(WIDTH, HEIGHT)
        .expect("synthetic scenes")
        .into_iter()
        .find(|scene| scene.kind == Some(kind))
        .expect("scene in the synthetic set")
}

/// Measured at 2 steps: AMaZE -33%, RCD -51% (goal was -60%).
#[test]
fn zone_plate_false_colour_drops_at_two_steps() {
    let zone_plate = scene(DetailScene::ZonePlate);
    for (algorithm, minimum_drop) in [
        (DemosaicAlgorithm::Amaze, 0.25),
        (DemosaicAlgorithm::Rcd, 0.40),
    ] {
        let (before, after) = chroma_rmse(&zone_plate, algorithm);
        let drop = 1.0 - after / before;
        assert!(
            drop >= minimum_drop,
            "{algorithm:?}: chroma RMSE {before:.5} -> {after:.5} ({:.0}% drop, need {:.0}%)",
            drop * 100.0,
            minimum_drop * 100.0
        );
    }
}

/// Measured at 2 steps: AMaZE +6%, RCD +16% (goal was at most +5%). The
/// scene is deliberately harsh: its finest colour cells are 2 px wide.
#[test]
fn colour_detail_loss_stays_bounded_at_two_steps() {
    let colour_detail = scene(DetailScene::ColourDetail);
    for (algorithm, maximum_loss) in [
        (DemosaicAlgorithm::Amaze, 0.10),
        (DemosaicAlgorithm::Rcd, 0.20),
    ] {
        let (before, after) = chroma_rmse(&colour_detail, algorithm);
        let loss = after / before - 1.0;
        assert!(
            loss <= maximum_loss,
            "{algorithm:?}: chroma RMSE {before:.5} -> {after:.5} ({:.0}% worse, allowed {:.0}%)",
            loss * 100.0,
            maximum_loss * 100.0
        );
    }
}
