//! A folder of small JSON files, one per named item (presets, albums).
//!
//! Names that differ only in case or punctuation count as the same, because
//! the file name follows from the name.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;
use tracing::warn;

use crate::storage;

const EXTENSION: &str = "json";
const MAX_NAME_CHARS: usize = 64;
/// The slug becomes a file name (plus `.json`); file systems allow 255 bytes,
/// and non-ASCII letters take up to 4 bytes each.
const MAX_SLUG_BYTES: usize = 128;

/// Why an item could not be stored, changed or found.
#[derive(Debug, Error)]
pub enum NamedFileError {
    #[error("the name is not usable: {0}")]
    InvalidName(&'static str),

    #[error("the name \"{0}\" is already taken")]
    NameTaken(String),

    #[error("{0} already exists (perhaps a file that could not be loaded)")]
    FileExists(PathBuf),

    #[error("at most {0} can be stored")]
    TooMany(usize),

    #[error("there is nothing named \"{0}\"")]
    NotFound(String),

    #[error("the application has no configuration directory to store this in")]
    NoDirectory,

    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not remove {path}: {source}")]
    Remove {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not serialize: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Something stored under its name.
pub trait NamedFile: Sized {
    /// The folder inside the configuration directory; also what the items are
    /// called in messages.
    const DIRECTORY: &'static str;
    const MAX_FILES: usize;
    const MAX_FILE_BYTES: u64;

    fn name(&self) -> &str;

    /// The same item under another name, which was checked.
    #[must_use]
    fn renamed(&self, name: String) -> Self;

    fn to_json(&self) -> serde_json::Result<Vec<u8>>;

    /// Parse and validate a file. The error text is shown to the user.
    fn from_json(json: &[u8]) -> Result<Self, String>;
}

/// The items of one folder.
#[derive(Debug)]
pub struct NamedFiles<T> {
    /// `None` when the application has no configuration directory; the store
    /// is then empty and cannot save.
    directory: Option<PathBuf>,
    /// Sorted by name, ignoring case.
    items: Vec<T>,
    /// One line per file that was skipped on load.
    warnings: Vec<String>,
}

impl<T: NamedFile> NamedFiles<T> {
    /// Read every file. One that cannot be read, parsed or validated, or whose
    /// name does not match its file name, is skipped and reported in
    /// [`Self::warnings`]; it never fails the whole list.
    pub fn load(config_directory: Option<&Path>) -> Self {
        let mut store = Self {
            directory: config_directory.map(|directory| directory.join(T::DIRECTORY)),
            items: Vec::new(),
            warnings: Vec::new(),
        };
        let Some(directory) = store.directory.clone() else {
            return store;
        };
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return store,
            Err(error) => {
                store.warn(format!(
                    "The {} folder could not be read: {error}",
                    T::DIRECTORY
                ));
                return store;
            }
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == EXTENSION))
            .collect();
        paths.sort();
        if paths.len() > T::MAX_FILES {
            store.warn(format!(
                "Only the first {} {} were loaded",
                T::MAX_FILES,
                T::DIRECTORY
            ));
            paths.truncate(T::MAX_FILES);
        }
        for path in paths {
            match read_item(&path) {
                Ok(item) => store.items.push(item),
                Err(reason) => store.warn(format!("Skipped {}: {reason}", path.display())),
            }
        }
        store.sort();
        store
    }

    pub fn items(&self) -> &[T] {
        &self.items
    }

    pub fn get(&self, name: &str) -> Option<&T> {
        self.items.iter().find(|item| same_name(item.name(), name))
    }

    /// Why files were skipped on load.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Store a new item. Fails when a item with the same name (ignoring
    /// case and punctuation) exists; delete or rename that one first.
    pub fn save(&mut self, item: T) -> Result<(), NamedFileError> {
        if self.get(item.name()).is_some() {
            return Err(NamedFileError::NameTaken(item.name().to_owned()));
        }
        if self.items.len() >= T::MAX_FILES {
            return Err(NamedFileError::TooMany(T::MAX_FILES));
        }
        self.refuse_unknown_file(item.name())?;
        self.write(&item)?;
        self.items.push(item);
        self.sort();
        Ok(())
    }

    /// Replace the stored item of the same name with a changed one.
    pub fn update(&mut self, item: T) -> Result<(), NamedFileError> {
        let index = self.index_of(item.name())?;
        self.write(&item)?;
        self.items[index] = item;
        Ok(())
    }

    pub fn delete(&mut self, name: &str) -> Result<(), NamedFileError> {
        let index = self.index_of(name)?;
        let path = self.path_for(self.items[index].name())?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            // Already gone: the list should still drop it.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(NamedFileError::Remove { path, source }),
        }
        self.items.remove(index);
        Ok(())
    }

    /// Give an item another name. Fails like [`Self::save`] when the new name
    /// is taken by a different item; changing only case or punctuation of
    /// its own name is fine.
    pub fn rename(&mut self, name: &str, new_name: &str) -> Result<(), NamedFileError> {
        let index = self.index_of(name)?;
        let renamed = self.items[index].renamed(checked_name(new_name)?);
        let taken = self
            .items
            .iter()
            .enumerate()
            .any(|(other, item)| other != index && same_name(item.name(), renamed.name()));
        if taken {
            return Err(NamedFileError::NameTaken(renamed.name().to_owned()));
        }
        let old_path = self.path_for(self.items[index].name())?;
        if self.path_for(renamed.name())? != old_path {
            self.refuse_unknown_file(renamed.name())?;
        }
        // Write the new file before removing the old one, so a failure never
        // loses the item.
        self.write(&renamed)?;
        let new_path = self.path_for(renamed.name())?;
        if new_path != old_path {
            match fs::remove_file(&old_path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    // Keep the old name as it was, rather than leave both
                    // files to load as two items next time.
                    let _ = fs::remove_file(&new_path);
                    return Err(NamedFileError::Remove {
                        path: old_path,
                        source,
                    });
                }
            }
        }
        self.items[index] = renamed;
        self.sort();
        Ok(())
    }

    fn index_of(&self, name: &str) -> Result<usize, NamedFileError> {
        self.items
            .iter()
            .position(|item| same_name(item.name(), name))
            .ok_or_else(|| NamedFileError::NotFound(name.to_owned()))
    }

    /// A file the store did not load (corrupt, newer format, over the size or
    /// count limit) can occupy the path a new item would be written to.
    /// Writing there would destroy it, so refuse instead.
    fn refuse_unknown_file(&self, name: &str) -> Result<(), NamedFileError> {
        let path = self.path_for(name)?;
        if path.exists() {
            return Err(NamedFileError::FileExists(path));
        }
        Ok(())
    }

    fn path_for(&self, name: &str) -> Result<PathBuf, NamedFileError> {
        let directory = self.directory.as_ref().ok_or(NamedFileError::NoDirectory)?;
        Ok(directory.join(format!("{}.{EXTENSION}", slug(name))))
    }

    fn write(&self, item: &T) -> Result<(), NamedFileError> {
        let path = self.path_for(item.name())?;
        let mut json = item.to_json()?;
        json.push(b'\n');
        storage::write_transactionally(&path, &json)
            .map_err(|source| NamedFileError::Write { path, source })
    }

    fn sort(&mut self) {
        self.items
            .sort_by_cached_key(|item| item.name().to_lowercase());
    }

    fn warn(&mut self, message: String) {
        warn!("{message}");
        self.warnings.push(message);
    }
}

fn same_name(a: &str, b: &str) -> bool {
    slug(a) == slug(b)
}

fn read_item<T: NamedFile>(path: &Path) -> Result<T, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() > T::MAX_FILE_BYTES {
        return Err(format!("file exceeds the {}-byte limit", T::MAX_FILE_BYTES));
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let item = T::from_json(&bytes)?;
    // The file name is how the store finds the file again, so it must follow
    // from the name.
    if path.file_stem().and_then(|stem| stem.to_str()) != Some(slug(item.name()).as_str()) {
        return Err("file name does not match the name inside".to_owned());
    }
    Ok(item)
}

/// The file stem a name is stored under, and the key for telling names apart:
/// lowercase letters and digits, with runs of anything else as one `-`.
/// Names that differ only in case or punctuation therefore count as the same.
fn slug(name: &str) -> String {
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

/// The trimmed name, if it can be stored under.
pub(crate) fn checked_name(name: &str) -> Result<String, NamedFileError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(NamedFileError::InvalidName("it is empty"));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(NamedFileError::InvalidName(
            "it is longer than 64 characters",
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(NamedFileError::InvalidName(
            "it contains a control character",
        ));
    }
    let slug = slug(name);
    if slug.is_empty() {
        return Err(NamedFileError::InvalidName("it has no letters or digits"));
    }
    if slug.len() > MAX_SLUG_BYTES {
        return Err(NamedFileError::InvalidName(
            "its file name would be too long",
        ));
    }
    Ok(name.to_owned())
}
