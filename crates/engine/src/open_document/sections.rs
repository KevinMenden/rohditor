//! Edits of one section that concern the open photo's defaults.

use rohditor_core::default_recipe;
use rohditor_edit::{CameraProfileSelection, EditRecipe, RecipeSection, copy_sections};

use super::OpenDocument;

impl OpenDocument {
    /// Restore the photo's defaults in one section. Returns whether the recipe
    /// changed.
    pub fn reset_section(&mut self, section: RecipeSection) -> bool {
        let next = copy_sections(self.edits.recipe(), &self.default_recipe(), &[section]);
        self.set_with_camera_profile(next)
    }

    /// Choose the camera profile. Returns whether the recipe changed.
    pub fn select_camera_profile(&mut self, selection: CameraProfileSelection) -> bool {
        let mut next = self.edits.recipe().clone();
        next.color.camera_profile = selection;
        self.set_with_camera_profile(next)
    }

    fn set_with_camera_profile(&mut self, next: EditRecipe) -> bool {
        let next = self.follow_camera_profile(self.edits.recipe(), next);
        self.edits.set_discrete(next)
    }

    /// `next`, made from `base`. The coordinates of the as-shot white balance
    /// depend on the camera profile, so a balance left as shot follows a change of
    /// profile.
    pub(super) fn follow_camera_profile(
        &self,
        base: &EditRecipe,
        mut next: EditRecipe,
    ) -> EditRecipe {
        let as_shot = |recipe: &EditRecipe| {
            let coordinates = self.as_shot_coordinates_for(&recipe.color.camera_profile);
            default_recipe(coordinates).color.white_balance
        };
        if next.color.camera_profile != base.color.camera_profile
            && next.color.white_balance == as_shot(base)
        {
            next.color.white_balance = as_shot(&next);
        }
        next
    }
}
