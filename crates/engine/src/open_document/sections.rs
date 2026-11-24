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

    /// The coordinates of the as-shot white balance depend on the camera profile,
    /// so a balance left as shot follows a change of profile.
    fn set_with_camera_profile(&mut self, mut next: EditRecipe) -> bool {
        let current = self.edits.recipe();
        let as_shot = self.default_recipe().color.white_balance;
        if next.color.camera_profile != current.color.camera_profile
            && next.color.white_balance == as_shot
        {
            let coordinates = self.as_shot_coordinates_for(&next.color.camera_profile);
            next.color.white_balance = default_recipe(coordinates).color.white_balance;
        }
        self.edits.set_discrete(next)
    }
}
