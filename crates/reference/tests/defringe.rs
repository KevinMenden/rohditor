//! Fringe removal on the synthetic fringed-edge scene (issue #74).
//!
//! The scene has purple fringes before and green fringes after every
//! dark-to-bright edge. "Fringe chroma" is the mean colour-difference
//! magnitude over exactly the fringe pixels, so it is not diluted by the rest
//! of the image.

use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_detail::{
    FringeColour, FringeRemoval, NeverCancelled, estimate_luma_noise, remove_fringes,
};
use rohditor_image::LinearRgbImage;
use rohditor_reference::detail_scenes::{DetailScene, EDGE_PERIOD, FRINGE_WIDTH, synthetic_set};

const WIDTH: usize = 768;
const HEIGHT: usize = 512;
const MARGIN: usize = 16;

fn removal(amount: f32, image: &LinearRgbImage<f32>) -> FringeRemoval {
    let colour = |hue| FringeColour { amount, hue };
    FringeRemoval {
        purple: colour([270.0, 330.0]),
        green: colour([75.0, 150.0]),
        balance: [1.0; 3],
        to_rec2020: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        ceilings: [f32::INFINITY; 3],
        noise: estimate_luma_noise(image, [1.0; 3], &NeverCancelled).expect("noise"),
    }
}

/// Whether column `x` lies within the fringe of an edge.
fn is_fringe(x: usize) -> bool {
    let half = EDGE_PERIOD / 2;
    let to_edge = (x % half).min(half - 1 - x % half);
    to_edge < FRINGE_WIDTH
}

/// Mean of `max(|r - g|, |b - g|)` over the fringe pixels.
fn fringe_chroma(image: &LinearRgbImage<f32>) -> f64 {
    let (mut sum, mut count) = (0.0, 0_u32);
    for y in MARGIN..HEIGHT - MARGIN {
        for x in (MARGIN..WIDTH - MARGIN).filter(|&x| is_fringe(x)) {
            let px = image.pixel(x, y).expect("pixel");
            sum += f64::from((px[0] - px[1]).abs().max((px[2] - px[1]).abs()));
            count += 1;
        }
    }
    sum / f64::from(count)
}

fn fringed_input(algorithm: Option<DemosaicAlgorithm>) -> LinearRgbImage<f32> {
    match algorithm {
        // The scene as described: fringes exactly three pixels wide.
        None => DetailScene::FringedEdges
            .input(WIDTH, HEIGHT)
            .expect("scene"),
        // Through a mosaic and a demosaic, which blurs the fringes and adds
        // colour artefacts of its own at the edges.
        Some(algorithm) => synthetic_set(WIDTH, HEIGHT)
            .expect("synthetic scenes")
            .into_iter()
            .find(|scene| scene.kind == Some(DetailScene::FringedEdges))
            .expect("fringed scene")
            .noisy_input(None, 1, algorithm)
            .expect("demosaiced scene"),
    }
}

fn drop_at(amount: f32, algorithm: Option<DemosaicAlgorithm>) -> f64 {
    let input = fringed_input(algorithm);
    let mut output = input.clone();
    let params = removal(amount, &input);
    remove_fringes(&mut output, params, &NeverCancelled).expect("defringe");
    1.0 - fringe_chroma(&output) / fringe_chroma(&input)
}

/// Goal of issue #74: at least 80% less fringe chroma at amount 1.
#[test]
fn full_amount_removes_the_modelled_fringes() {
    let drop = drop_at(1.0, None);
    assert!(drop >= 0.8, "fringe chroma dropped by {:.0}%", drop * 100.0);
}

#[test]
fn zero_amount_changes_nothing_and_half_removes_about_half() {
    assert_eq!(drop_at(0.0, None), 0.0);
    let half = drop_at(0.5, None);
    assert!((0.35..=0.65).contains(&half), "{half}");
}

/// Regression bounds, not goals. After a mosaic and demosaic the edge pixels
/// carry demosaic colour artefacts (zippering) of every hue next to the
/// fringes, and defringe only touches purple and green. Measured at amount 1:
/// bilinear -20%, RCD -18%, AMaZE -40%.
#[test]
fn demosaiced_fringes_are_reduced_for_every_algorithm() {
    for (algorithm, minimum_drop) in [
        (DemosaicAlgorithm::Bilinear, 0.12),
        (DemosaicAlgorithm::Rcd, 0.12),
        (DemosaicAlgorithm::Amaze, 0.30),
    ] {
        let drop = drop_at(1.0, Some(algorithm));
        assert!(
            drop >= minimum_drop,
            "{algorithm:?}: fringe chroma dropped by {:.0}%",
            drop * 100.0
        );
    }
}
