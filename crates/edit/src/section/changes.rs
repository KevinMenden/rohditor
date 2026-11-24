//! Which sections of a recipe are edited.

use super::copy_sections;
use crate::{EditRecipe, Parameter, RecipeSection};

/// The sections in which `recipe` differs from `defaults`, in the Edit panel's
/// order. Saving a preset from "what I changed" starts from this list.
#[must_use]
pub fn changed_sections(recipe: &EditRecipe, defaults: &EditRecipe) -> Vec<RecipeSection> {
    RecipeSection::ALL
        .into_iter()
        .filter(|section| section.is_changed(recipe, defaults))
        .collect()
}

impl RecipeSection {
    /// Whether `recipe` differs from `defaults` here. Agrees with
    /// [`Parameter::is_changed`] on the section's numbers.
    #[must_use]
    pub fn is_changed(self, recipe: &EditRecipe, defaults: &EditRecipe) -> bool {
        let mut section = copy_sections(defaults, recipe, &[self]);
        let parameters = Parameter::all().filter(|parameter| parameter.section() == self);
        for parameter in parameters {
            if !parameter.is_changed(recipe, defaults) {
                // Drop differences too small to count.
                let default = parameter.get(defaults, defaults);
                parameter.write(&mut section, defaults, default);
            }
        }
        section != *defaults
    }
}
