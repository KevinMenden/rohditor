//! One preset: a name, the sections it covers and the recipe values for them.

use rohditor_edit::{EditError, EditRecipe, RecipeSection, blend_sections, copy_sections};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::named_files::{NamedFile, NamedFileError, checked_name};

const PRESET_FORMAT_VERSION: u32 = 1;

/// Why a preset could not be created, stored or found.
#[derive(Debug, Error)]
pub enum PresetError {
    #[error("a preset must cover at least one section")]
    NoSections,

    #[error("preset recipe is invalid: {0}")]
    InvalidRecipe(#[from] EditError),

    #[error(transparent)]
    File(#[from] NamedFileError),
}

/// How a preset is applied to a photo's recipe.
#[derive(Debug, Clone, PartialEq)]
pub struct ApplyOptions {
    /// Which of the preset's sections to apply; `None` applies all of them.
    /// Sections the preset does not cover are ignored.
    pub sections: Option<Vec<RecipeSection>>,
    /// Blend strength in `0.0..=1.0`: 0 keeps the photo as it is, 1 takes the
    /// preset's values (see [`rohditor_edit::blend_sections`]).
    pub amount: f32,
    /// Sections the photo cannot take, for example white balance on an
    /// already demosaiced linear DNG. They are never applied.
    pub unavailable: Vec<RecipeSection>,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        Self {
            sections: None,
            amount: 1.0,
            unavailable: Vec::new(),
        }
    }
}

/// A named partial recipe. Create one with [`Preset::new`]; the values of
/// sections it does not cover are the defaults and are never applied.
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    name: String,
    /// In the Edit panel's order, without duplicates, never empty.
    sections: Vec<RecipeSection>,
    recipe: EditRecipe,
}

impl Preset {
    /// A preset holding `sections` of `recipe` under `name`. The recipe must be
    /// valid; the name is trimmed.
    pub fn new(
        name: &str,
        recipe: &EditRecipe,
        sections: &[RecipeSection],
    ) -> Result<Self, PresetError> {
        let name = checked_name(name)?;
        recipe.validate()?;
        let sections = canonical_sections(sections);
        if sections.is_empty() {
            return Err(PresetError::NoSections);
        }
        Ok(Self {
            name,
            recipe: copy_sections(&EditRecipe::default(), recipe, &sections),
            sections,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The sections this preset covers.
    pub fn sections(&self) -> &[RecipeSection] {
        &self.sections
    }

    /// `base` with the preset applied. Pure: the caller owns undo and
    /// revisions. At full amount the result does not depend on how often the
    /// preset was applied before.
    #[must_use]
    pub fn apply(&self, base: &EditRecipe, options: &ApplyOptions) -> EditRecipe {
        let requested = options.sections.as_deref();
        let sections: Vec<RecipeSection> = self
            .sections
            .iter()
            .copied()
            .filter(|section| requested.is_none_or(|requested| requested.contains(section)))
            .filter(|section| !options.unavailable.contains(section))
            .collect();
        blend_sections(base, &self.recipe, &sections, options.amount)
    }
}

impl NamedFile for Preset {
    const DIRECTORY: &'static str = "presets";
    const MAX_FILES: usize = 256;
    const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

    fn name(&self) -> &str {
        &self.name
    }

    fn renamed(&self, name: String) -> Self {
        Self {
            name,
            ..self.clone()
        }
    }

    fn to_json(&self) -> serde_json::Result<Vec<u8>> {
        serde_json::to_vec_pretty(&PresetFile {
            format_version: PRESET_FORMAT_VERSION,
            name: self.name.clone(),
            sections: self.sections.clone(),
            recipe: self.recipe.clone(),
        })
    }

    fn from_json(json: &[u8]) -> Result<Self, String> {
        let file: PresetFile = serde_json::from_slice(json).map_err(|error| error.to_string())?;
        if file.format_version != PRESET_FORMAT_VERSION {
            return Err(format!(
                "unsupported format version {}",
                file.format_version
            ));
        }
        Self::new(&file.name, &file.recipe, &file.sections).map_err(|error| error.to_string())
    }
}

/// The JSON layout of one preset file.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetFile {
    format_version: u32,
    name: String,
    sections: Vec<RecipeSection>,
    recipe: EditRecipe,
}

fn canonical_sections(sections: &[RecipeSection]) -> Vec<RecipeSection> {
    RecipeSection::ALL
        .into_iter()
        .filter(|section| sections.contains(section))
        .collect()
}
