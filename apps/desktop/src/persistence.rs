//! Desktop-local persistence for non-destructive RAW edits.
//!
//! Recipes live beside their source file and remain separate from the RAW
//! bytes. The core recipe owns schema migration and validation; this module
//! only owns the desktop sidecar format and atomic replacement.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rohditor_edit::{EditError, EditRecipe};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::storage;

const SIDECAR_SUFFIX: &str = ".rohditor.json";
const PROJECT_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectFile {
    format_version: u32,
    recipe: EditRecipe,
}

#[derive(Debug, Error)]
pub(crate) enum PersistenceError {
    #[error("could not derive a sidecar path for source {raw_file}")]
    InvalidSourcePath { raw_file: PathBuf },

    #[error("could not read recipe sidecar {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not parse recipe sidecar {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("recipe sidecar {path} uses unsupported format version {actual}; expected {expected}")]
    UnsupportedFormat {
        path: PathBuf,
        actual: u32,
        expected: u32,
    },

    #[error("recipe in sidecar {path} is invalid: {source}")]
    InvalidRecipe {
        path: PathBuf,
        #[source]
        source: EditError,
    },

    #[error("could not serialize recipe sidecar {path}: {source}")]
    Serialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("could not write recipe sidecar {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Return the recipe sidecar path for one RAW source.
pub(crate) fn sidecar_path(source: &Path) -> Result<PathBuf, PersistenceError> {
    let Some(file_name) = source.file_name() else {
        return Err(PersistenceError::InvalidSourcePath {
            raw_file: source.to_owned(),
        });
    };
    let mut sidecar_name = OsString::from(file_name);
    sidecar_name.push(SIDECAR_SUFFIX);
    Ok(source
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(sidecar_name))
}

/// Load the saved recipe for a source, if its sidecar exists.
pub(crate) fn load_recipe(source: &Path) -> Result<Option<EditRecipe>, PersistenceError> {
    let path = sidecar_path(source)?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(PersistenceError::Read { path, source }),
    };
    let project = serde_json::from_slice::<ProjectFile>(&bytes).map_err(|source| {
        PersistenceError::Parse {
            path: path.clone(),
            source,
        }
    })?;
    if project.format_version != PROJECT_FORMAT_VERSION {
        return Err(PersistenceError::UnsupportedFormat {
            path,
            actual: project.format_version,
            expected: PROJECT_FORMAT_VERSION,
        });
    }
    let recipe = project.recipe;
    recipe
        .validate()
        .map_err(|source| PersistenceError::InvalidRecipe { path, source })?;
    Ok(Some(recipe))
}

/// Persist the current recipe without touching the source RAW file.
pub(crate) fn save_recipe(source: &Path, recipe: &EditRecipe) -> Result<PathBuf, PersistenceError> {
    let path = sidecar_path(source)?;
    recipe
        .validate()
        .map_err(|source| PersistenceError::InvalidRecipe {
            path: path.clone(),
            source,
        })?;
    let project = ProjectFile {
        format_version: PROJECT_FORMAT_VERSION,
        recipe: recipe.clone(),
    };
    let bytes =
        serde_json::to_vec_pretty(&project).map_err(|source| PersistenceError::Serialize {
            path: path.clone(),
            source,
        })?;
    storage::write_transactionally(&path, &bytes).map_err(|source| PersistenceError::Write {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "rohditor-desktop-persistence-{}-{id}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create persistence test directory");
            Self(path)
        }

        fn source(&self) -> PathBuf {
            self.0.join("photo.ARW")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn sidecar_path_keeps_the_source_name_and_adds_a_known_suffix() {
        let path = sidecar_path(Path::new("/photos/holiday.ARW")).expect("sidecar path");
        assert_eq!(path, PathBuf::from("/photos/holiday.ARW.rohditor.json"));
        assert!(sidecar_path(Path::new("/")).is_err());
    }

    #[test]
    fn recipes_round_trip_with_an_application_format_version() {
        let directory = TestDirectory::new();
        let source = directory.source();
        let mut expected = EditRecipe::default();
        expected.light.exposure_ev = 1.25;
        expected.color.saturation = 1.2;

        let path = save_recipe(&source, &expected).expect("save recipe");
        let text = fs::read_to_string(&path).expect("read sidecar");
        assert!(text.contains("\"format_version\": 1"));
        assert_eq!(load_recipe(&source).expect("load recipe"), Some(expected));
    }

    #[test]
    fn absent_sidecars_are_distinct_from_load_errors() {
        let directory = TestDirectory::new();
        assert_eq!(
            load_recipe(&directory.source()).expect("missing sidecar"),
            None
        );
    }

    #[test]
    fn malformed_and_future_sidecars_are_reported() {
        let directory = TestDirectory::new();
        let source = directory.source();
        let path = sidecar_path(&source).expect("sidecar path");

        fs::write(&path, b"not json").expect("write malformed sidecar");
        assert!(matches!(
            load_recipe(&source),
            Err(PersistenceError::Parse { .. })
        ));

        fs::write(
            &path,
            br#"{"format_version":99,"recipe":{"schema_version":10,"light":{},"color":{},"geometry":{}}}"#,
        )
        .expect("write future sidecar");
        assert!(matches!(
            load_recipe(&source),
            Err(PersistenceError::UnsupportedFormat { .. })
        ));
    }
}
