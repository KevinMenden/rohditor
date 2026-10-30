//! Frontend-agnostic recipe edits that need more than the recipe types alone:
//! histogram-driven auto tone, white-balance picking and mode switching, and
//! default recipe construction. Every function is a pure recipe transform; the
//! caller owns undo/revision handling.

mod auto_tone;
mod sampling;
mod white_balance;

pub use auto_tone::auto_tone;
pub use sampling::sample_rgb_patch;
pub use white_balance::{
    WhiteBalanceMode, WhiteBalanceModeMemory, white_balance_from_camera_sample,
};

use rohditor_edit::{EditRecipe, WhiteBalance};

use crate::WhiteBalanceCoordinates;

/// The recipe a document starts from, or is reset to. When the source's As Shot
/// coordinates are known the balance is expressed as Temperature/Tint so the
/// sliders start at the camera's own white point.
#[must_use]
pub fn default_recipe(as_shot: Option<WhiteBalanceCoordinates>) -> EditRecipe {
    let mut recipe = EditRecipe::default();
    if let Some(coordinates) = as_shot {
        recipe.color.white_balance = WhiteBalance::TemperatureTint {
            temperature: coordinates.temperature,
            tint: coordinates.tint,
        };
    }
    recipe
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_recipe_uses_as_shot_coordinates_when_known() {
        assert_eq!(default_recipe(None), EditRecipe::default());
        let recipe = default_recipe(Some(WhiteBalanceCoordinates {
            temperature: 5_200.0,
            tint: -0.2,
        }));
        assert_eq!(
            recipe.color.white_balance,
            WhiteBalance::TemperatureTint {
                temperature: 5_200.0,
                tint: -0.2,
            }
        );
    }
}
