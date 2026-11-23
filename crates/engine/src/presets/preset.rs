//! One preset: a name, the sections it covers and the recipe values for them.

use rohditor_edit::{EditError, EditRecipe, RecipeSection, blend_sections, copy_sections};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_NAME_CHARS: usize = 64;

/// Why a preset could not be created, stored or found.
#[derive(Debug, Error)]
pub enum PresetError {
    #[error("preset name is not usable: {0}")]
    InvalidName(&'static str),

    #[error("a preset must cover at least one section")]
    NoSections,

    #[error("preset recipe is invalid: {0}")]
    InvalidRecipe(#[from] EditError),

    #[error("a preset named \"{0}\" already exists")]
    NameTaken(String),

    #[error("there is no preset named \"{0}\"")]
    NotFound(String),

    #[error("the application has no configuration directory to store presets in")]
    NoDirectory,

    #[error("could not write preset {path}: {source}")]
    Write {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not serialize preset: {0}")]
    Serialize(#[from] serde_json::Error),
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

    /// The same preset under another name.
    pub(super) fn renamed(&self, name: &str) -> Result<Self, PresetError> {
        Ok(Self {
            name: checked_name(name)?,
            ..self.clone()
        })
    }

    pub(super) fn to_file(&self) -> PresetFile {
        PresetFile {
            format_version: PRESET_FORMAT_VERSION,
            name: self.name.clone(),
            sections: self.sections.clone(),
            recipe: self.recipe.clone(),
        }
    }

    pub(super) fn from_file(file: PresetFile) -> Result<Self, FileError> {
        if file.format_version != PRESET_FORMAT_VERSION {
            return Err(FileError::Version(file.format_version));
        }
        Self::new(&file.name, &file.recipe, &file.sections).map_err(FileError::Invalid)
    }
}

pub(super) const PRESET_FORMAT_VERSION: u32 = 1;

/// The JSON layout of one preset file.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PresetFile {
    format_version: u32,
    name: String,
    sections: Vec<RecipeSection>,
    recipe: EditRecipe,
}

/// Why a preset file was rejected on load.
#[derive(Debug, Error)]
pub(super) enum FileError {
    #[error("unsupported format version {0}")]
    Version(u32),
    #[error("{0}")]
    Invalid(PresetError),
}

/// The file stem a name is stored under, and the key for telling names apart:
/// lowercase letters and digits, with runs of anything else as one `-`.
/// Names that differ only in case or punctuation therefore count as the same.
pub(super) fn slug(name: &str) -> String {
    let mut slug = String::new();
    for character in name.chars() {
        if character.is_alphanumeric() {
            slug.extend(character.to_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.truncate(slug.trim_end_matches('-').len());
    slug
}

fn checked_name(name: &str) -> Result<String, PresetError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(PresetError::InvalidName("it is empty"));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(PresetError::InvalidName("it is longer than 64 characters"));
    }
    if name.chars().any(char::is_control) {
        return Err(PresetError::InvalidName("it contains a control character"));
    }
    if slug(name).is_empty() {
        return Err(PresetError::InvalidName("it has no letters or digits"));
    }
    Ok(name.to_owned())
}

fn canonical_sections(sections: &[RecipeSection]) -> Vec<RecipeSection> {
    RecipeSection::ALL
        .into_iter()
        .filter(|section| sections.contains(section))
        .collect()
}
