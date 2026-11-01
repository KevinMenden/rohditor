//! Regression test for the noise-derived default on a small synthetic fixture:
//! the default must stay off for a clean frame, switch chroma reduction on for
//! a noisy one, and that strength must actually remove colour noise without
//! touching luma or adding colour.

use rohditor_core::with_default_noise_reduction;
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_detail::{ChromaDenoise, NeverCancelled, denoise_chroma, estimate_luma_noise};
use rohditor_edit::EditRecipe;
use rohditor_image::LinearRgbImage;
use rohditor_reference::detail_scenes::{DetailScene, Scene, synthetic_set};
use rohditor_reference::metrics::detail_scores;
use rohditor_reference::noise::{NOISE_LEVELS, NoiseModel};

const WIDTH: usize = 384;
const HEIGHT: usize = 256;
const MARGIN: usize = 16;
const BALANCE: [f32; 3] = [1.0; 3];

fn scene() -> Scene {
    synthetic_set(WIDTH, HEIGHT)
        .expect("synthetic scenes")
        .into_iter()
        .find(|scene| scene.kind == Some(DetailScene::Gradient))
        .expect("gradient scene")
}

fn input(scene: &Scene, noise: Option<NoiseModel>) -> LinearRgbImage<f32> {
    scene
        .noisy_input(noise, 7, DemosaicAlgorithm::Amaze)
        .expect("stage input")
}

/// The chroma strength the default picks for `image`.
fn default_chroma(image: &LinearRgbImage<f32>) -> f32 {
    let profile = estimate_luma_noise(image, BALANCE, &NeverCancelled).expect("noise profile");
    with_default_noise_reduction(EditRecipe::default(), profile.as_ref())
        .noise_reduction
        .chroma
}

#[test]
fn default_is_off_when_clean_and_on_when_noisy() {
    let scene = scene();
    let (_, heavy) = NOISE_LEVELS[2];
    let moderate = NoiseModel {
        a: 4.0e-4,
        b: 1.0e-5,
    };
    let clean = default_chroma(&input(&scene, None));
    let mid = default_chroma(&input(&scene, Some(moderate)));
    let noisy = default_chroma(&input(&scene, Some(heavy)));
    assert_eq!(clean, 0.0, "clean frame stays off the CPU-only path");
    assert!(clean <= mid && mid <= noisy, "{clean} {mid} {noisy}");
    assert!(noisy > 0.0 && noisy <= 0.4, "noisy frame gets {noisy}");
}

#[test]
fn default_strength_removes_colour_noise_and_leaves_luma_alone() {
    let scene = scene();
    let (_, level) = NOISE_LEVELS[1];
    let noisy = input(&scene, Some(level));
    let strength = default_chroma(&noisy);
    assert!(strength > 0.0);
    let profile = estimate_luma_noise(&noisy, BALANCE, &NeverCancelled)
        .expect("noise profile")
        .expect("profile");
    let mut cleaned = noisy.clone();
    denoise_chroma(
        &mut cleaned,
        ChromaDenoise {
            strength,
            balance: BALANCE,
            ceilings: [f32::INFINITY; 3],
            luma_noise: profile,
        },
        &NeverCancelled,
    )
    .expect("chroma denoise");

    let before = detail_scores(&scene.truth, &noisy, MARGIN);
    let after = detail_scores(&scene.truth, &cleaned, MARGIN);
    assert!(
        after.chroma_rmse <= before.chroma_rmse * 0.8,
        "chroma RMSE {} -> {}",
        before.chroma_rmse,
        after.chroma_rmse
    );
    assert!(
        after.colour_added <= before.colour_added,
        "colour added {} -> {}",
        before.colour_added,
        after.colour_added
    );
    // The luma plane is rebuilt from RGB, so allow float round-off.
    assert!(
        (after.luma_psnr_db - before.luma_psnr_db).abs() < 0.01,
        "luma PSNR {} -> {}",
        before.luma_psnr_db,
        after.luma_psnr_db
    );
}
