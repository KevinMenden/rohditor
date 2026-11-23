//! Named presets: partial recipes saved as files and applied to a photo.
//!
//! A preset covers some [`rohditor_edit::RecipeSection`]s. Applying one is a
//! pure function from the photo's recipe to a new recipe ([`Preset::apply`]);
//! the caller records it as one undo step. Presets live in one JSON file each
//! in a `presets` folder inside the configuration directory the application
//! passes in (see [`PresetStore`]).

mod preset;
mod store;
#[cfg(test)]
mod tests;

pub use preset::{ApplyOptions, Preset, PresetError};
pub use store::PresetStore;
