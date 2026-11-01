//! Source-resolution defringe, the first step of the spatial stage: it runs
//! after optics (lateral CA is already corrected there, leaving axial
//! fringes) and before area reduction (fringes are 1-3 source pixels wide and
//! reduction would smear them).
//!
//! Unlike the detail stage it is not cached with the captured source: it sits
//! behind optics, so a change only re-runs this step and what follows.
use std::time::{Duration, Instant};

use rohditor_demosaic::WhiteBalanceGains;
use rohditor_detail::{
    DetailError, FringeColour, FringeRemoval, defringe_scratch_bytes, estimate_luma_noise,
    remove_fringes,
};
use rohditor_edit::{Defringe, EditRecipe, WhiteBalance};
use rohditor_image::LinearRgbImage;

use crate::color::{CameraCalibration, resolve_camera_colour};
use crate::{CancellationToken, PipelineError};

/// Bump when the defringe output changes for the same settings.
pub const DEFRINGE_ALGORITHM_VERSION: u16 = 1;

/// Settings and implementation identity of the defringe step that ran.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DefringeProvenance {
    pub settings: Defringe,
    pub version: u16,
}

impl DefringeProvenance {
    /// `None` when the recipe has no active defringe.
    #[must_use]
    pub fn for_recipe(recipe: &EditRecipe) -> Option<Self> {
        recipe.defringe.is_active().then_some(Self {
            settings: recipe.defringe,
            version: DEFRINGE_ALGORITHM_VERSION,
        })
    }
}

/// Scratch bytes defringe needs for a `width` x `height` source (0 when off).
pub(crate) fn scratch_bytes(
    recipe: &EditRecipe,
    width: usize,
    height: usize,
) -> Result<usize, PipelineError> {
    if !recipe.defringe.is_active() {
        return Ok(0);
    }
    defringe_scratch_bytes(width, height)
        .ok_or_else(|| super::orchestration::dimension_overflow(width, height))
}

/// Defringe `image` in place; returns the time spent (zero when off).
///
/// `balance` is the as-shot demosaic balance and `gains` the user's white
/// balance (it only shapes the highlight ceilings). Hue is judged in the
/// as-shot rendering, which keeps the result independent of white-balance
/// edits; a large white-balance change therefore shifts which colours the
/// final image shows as purple or green relative to the configured ranges.
pub(super) fn apply(
    image: &mut LinearRgbImage<f32>,
    recipe: &EditRecipe,
    calibration: &CameraCalibration,
    balance: WhiteBalanceGains,
    gains: WhiteBalanceGains,
    cancellation: &CancellationToken,
) -> Result<Duration, PipelineError> {
    cancellation.checkpoint()?;
    let settings = recipe.defringe;
    if !settings.is_active() {
        return Ok(Duration::ZERO);
    }
    let started = Instant::now();
    let as_shot = resolve_camera_colour(
        calibration,
        &recipe.color.camera_profile,
        WhiteBalance::AsShot,
    )?;
    let balance = [balance.red, balance.green, balance.blue];
    let is_cancelled = || cancellation.is_cancelled();
    // Edges weaker than the grain are not edges. Too small to profile means
    // too small to have meaningful fringes.
    let noise = estimate_luma_noise(image, balance, &is_cancelled).map_err(defringe_error)?;
    let colour = |amount: f32, hue: [f32; 2]| FringeColour { amount, hue };
    remove_fringes(
        image,
        FringeRemoval {
            purple: colour(settings.purple_amount, settings.purple_hue),
            green: colour(settings.green_amount, settings.green_hue),
            balance,
            to_rec2020: as_shot.camera_to_linear_rec2020.values(),
            ceilings: super::detail::ceilings(recipe, gains),
            noise,
        },
        &is_cancelled,
    )
    .map_err(defringe_error)?;
    Ok(started.elapsed())
}

fn defringe_error(error: DetailError) -> PipelineError {
    match error {
        DetailError::Cancelled => PipelineError::Cancelled,
        other => PipelineError::InvalidRecipe {
            field: "defringe",
            reason: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_exists_only_while_active_and_tracks_settings() {
        let mut recipe = EditRecipe::default();
        assert_eq!(DefringeProvenance::for_recipe(&recipe), None);
        recipe.defringe.purple_amount = 0.5;
        let purple = DefringeProvenance::for_recipe(&recipe).expect("active");
        assert_eq!(purple.settings, recipe.defringe);
        assert_eq!(purple.version, DEFRINGE_ALGORITHM_VERSION);
        recipe.defringe.green_amount = 0.5;
        assert_ne!(DefringeProvenance::for_recipe(&recipe), Some(purple));
        assert_eq!(
            scratch_bytes(&EditRecipe::default(), 100, 100).expect("scratch"),
            0
        );
        assert!(scratch_bytes(&recipe, 100, 100).expect("scratch") > 0);
        assert!(scratch_bytes(&recipe, usize::MAX, 2).is_err());
    }

    /// Sony-like calibration: as-shot gains (2.5, 1, 1.85) and a D65 matrix.
    fn calibration() -> (CameraCalibration, WhiteBalanceGains) {
        use rohditor_raw::{CameraColorMatrix, CameraMatrixOrigin};
        let calibration = CameraCalibration {
            make: "SONY".to_owned(),
            model: "ILCE-6400".to_owned(),
            clean_make: "Sony".to_owned(),
            clean_model: "ILCE-6400".to_owned(),
            as_shot_white_balance: [Some(2.5), Some(1.0), Some(1.85), None],
            xyz_to_camera: [[0.0; 3]; 4],
            color_matrices: vec![CameraColorMatrix {
                illuminant: "D65".to_owned(),
                values: vec![
                    0.7657, -0.2847, -0.0607, -0.4083, 1.1966, 0.2389, -0.0684, 0.1418, 0.5844,
                ],
                origin: CameraMatrixOrigin::DecoderDatabase,
            }],
        };
        let balance = WhiteBalanceGains {
            red: 2.5,
            green: 1.0,
            blue: 1.85,
        };
        (calibration, balance)
    }

    /// Dark/bright vertical edge with a 3 px purple fringe before it and a
    /// 3 px green one after it; neutral values are as-shot balanced.
    fn fringed_edge(balance: WhiteBalanceGains) -> LinearRgbImage<f32> {
        use rohditor_image::LinearRgbSpace;
        let (width, height) = (40, 8);
        let gains = [balance.red, balance.green, balance.blue];
        let mut data = Vec::new();
        for _ in 0..height {
            for x in 0..width {
                let balanced = match x {
                    17..=19 => [0.28, 0.01, 0.28],
                    20..=22 => [0.78, 0.9, 0.78],
                    _ if x < 20 => [0.03; 3],
                    _ => [0.9; 3],
                };
                data.extend((0..3).map(|c| balanced[c] / gains[c]));
            }
        }
        LinearRgbImage::new(width, height, width * 3, LinearRgbSpace::CameraNative, data)
            .expect("image")
    }

    fn chroma(image: &LinearRgbImage<f32>, x: usize, balance: WhiteBalanceGains) -> f32 {
        let px = image.pixel(x, 4).expect("pixel");
        let (r, g, b) = (
            px[0] * balance.red,
            px[1] * balance.green,
            px[2] * balance.blue,
        );
        (r - g).abs().max((b - g).abs())
    }

    #[test]
    fn removes_fringes_through_the_camera_matrix_and_is_off_at_zero() {
        let (calibration, balance) = calibration();
        let token = CancellationToken::new();
        let input = fringed_edge(balance);
        let run = |recipe: &EditRecipe| {
            let mut image = input.clone();
            let elapsed =
                apply(&mut image, recipe, &calibration, balance, balance, &token).expect("apply");
            (image, elapsed)
        };

        let (untouched, elapsed) = run(&EditRecipe::default());
        assert_eq!(untouched, input);
        assert!(elapsed.is_zero());

        let mut recipe = EditRecipe::default();
        recipe.defringe.purple_amount = 1.0;
        recipe.defringe.green_amount = 1.0;
        // Through this camera's matrix the synthetic purple lands at about
        // 280 degrees, near the edge of the default range: widen it.
        recipe.defringe.purple_hue = [250.0, 320.0];
        let (fixed, elapsed) = run(&recipe);
        assert!(!elapsed.is_zero());
        for x in [17, 18, 19, 20, 21, 22] {
            assert!(
                chroma(&fixed, x, balance) <= 0.2 * chroma(&input, x, balance),
                "x={x}: {} of {}",
                chroma(&fixed, x, balance),
                chroma(&input, x, balance)
            );
        }
        // Far from the edge nothing changes.
        assert_eq!(fixed.pixel(5, 4), input.pixel(5, 4));
        assert_eq!(fixed.pixel(35, 4), input.pixel(35, 4));
    }

    #[test]
    fn cancellation_stops_an_active_defringe() {
        let (calibration, balance) = calibration();
        let token = CancellationToken::new();
        token.cancel();
        let mut recipe = EditRecipe::default();
        recipe.defringe.purple_amount = 1.0;
        let mut image = fringed_edge(balance);
        assert!(matches!(
            apply(&mut image, &recipe, &calibration, balance, balance, &token),
            Err(PipelineError::Cancelled)
        ));
    }
}
