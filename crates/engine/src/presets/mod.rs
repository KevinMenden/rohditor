//! Named presets: partial recipes saved as files and applied to a photo.
//!
//! A preset covers some [`rohditor_edit::RecipeSection`]s. Applying one is a
//! pure function from the photo's recipe to a new recipe ([`Preset::apply`]);
//! the caller records it as one undo step. Presets live in one JSON file each
//! in a `presets` folder inside the configuration directory the application
//! passes in (see [`PresetStore`]).

mod preset;
#[cfg(test)]
mod tests;

pub use preset::{ApplyOptions, Preset, PresetError};

use crate::named_files::NamedFiles;

/// The saved presets, loaded from `<config directory>/presets`.
pub type PresetStore = NamedFiles<Preset>;

impl PresetStore {
    pub fn presets(&self) -> &[Preset] {
        self.items()
    }
}
