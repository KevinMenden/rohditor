//! Texture, clarity and dehaze: the spatial stage after the per-pixel adjustments.
//!
//! The filters live in `rohditor_detail`; this module maps the recipe onto them for the
//! Rec.2020 working space and translates their errors.

use rohditor_color::REC2020_LUMINANCE;
use rohditor_detail::{DetailError, Presence, apply_presence, presence_scratch_bytes};
use rohditor_edit::{EditRecipe, PresenceAdjustments};
use rohditor_image::LinearRgbImage;

use crate::{CancellationToken, PipelineError};

/// Run the presence sliders of `recipe` over the adjusted image. A no-op when all are neutral.
pub(super) fn apply_recipe_presence(
    image: &mut LinearRgbImage<f32>,
    recipe: &EditRecipe,
    cancellation: &CancellationToken,
) -> Result<(), PipelineError> {
    if !recipe.presence.is_active() {
        return Ok(());
    }
    let span = tracing::info_span!(
        "cpu.presence",
        texture = recipe.presence.texture,
        clarity = recipe.presence.clarity,
        dehaze = recipe.presence.dehaze
    );
    let _guard = span.enter();
    let PresenceAdjustments {
        texture,
        clarity,
        dehaze,
    } = recipe.presence;
    apply_presence(
        image,
        Presence {
            texture,
            clarity,
            dehaze,
        },
        REC2020_LUMINANCE,
        &|| cancellation.is_cancelled(),
    )
    .map_err(|error| match error {
        DetailError::Cancelled => PipelineError::Cancelled,
        other => PipelineError::InvalidRecipe {
            field: "presence",
            reason: other.to_string(),
        },
    })
}

/// Extra bytes the presence stage holds while it runs; 0 when it has nothing to do.
pub(crate) fn presence_stage_bytes(recipe: &EditRecipe, width: usize, height: usize) -> usize {
    if recipe.presence.is_active() {
        presence_scratch_bytes(width, height).unwrap_or(usize::MAX)
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use rohditor_image::{LinearRgbImage, LinearRgbSpace};

    use super::*;
    use crate::apply_adjustments;

    /// Fine two-tone checkerboard: texture has plenty to work on.
    fn checkerboard() -> LinearRgbImage<f32> {
        let (width, height) = (48_usize, 30_usize);
        let data = (0..width * height)
            .flat_map(|i| {
                let v = if (i % width + i / width).is_multiple_of(2) {
                    0.22
                } else {
                    0.12
                };
                [v, v, v]
            })
            .collect();
        LinearRgbImage::new(width, height, width * 3, LinearRgbSpace::Rec2020D65, data)
            .expect("checkerboard")
    }

    fn spread(image: &LinearRgbImage<f32>) -> f32 {
        let greens = image.data().iter().skip(1).step_by(3);
        let (low, high) = greens.fold((f32::MAX, f32::MIN), |(l, h), &v| (l.min(v), h.max(v)));
        high - low
    }

    #[test]
    fn adjustments_apply_presence_after_the_per_pixel_chain() {
        let mut neutral = checkerboard();
        let mut recipe = EditRecipe::default();
        apply_adjustments(&mut neutral, &recipe).expect("neutral");

        recipe.presence.texture = 1.0;
        let mut textured = checkerboard();
        apply_adjustments(&mut textured, &recipe).expect("textured");
        assert!(spread(&textured) > spread(&neutral) * 1.3);

        recipe.presence.texture = -1.0;
        let mut smoothed = checkerboard();
        apply_adjustments(&mut smoothed, &recipe).expect("smoothed");
        assert!(spread(&smoothed) < spread(&neutral) * 0.7);
    }

    #[test]
    fn a_cancelled_token_stops_the_stage() {
        let mut recipe = EditRecipe::default();
        recipe.presence.clarity = 0.5;
        let token = CancellationToken::new();
        token.cancel();
        let result = apply_recipe_presence(&mut checkerboard(), &recipe, &token);
        assert!(matches!(result, Err(PipelineError::Cancelled)));
    }

    #[test]
    fn scratch_is_only_budgeted_for_active_presence() {
        let mut recipe = EditRecipe::default();
        assert_eq!(presence_stage_bytes(&recipe, 4_000, 3_000), 0);
        recipe.presence.dehaze = 0.1;
        assert!(presence_stage_bytes(&recipe, 4_000, 3_000) >= 4_000 * 3_000 * 4);
    }
}
