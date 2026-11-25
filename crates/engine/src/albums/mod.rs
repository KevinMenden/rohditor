//! Albums: named lists of photos from any folder.
//!
//! An album only refers to photos: changing or deleting it never touches them
//! or the edits beside them. Albums live in one JSON file each in an `albums`
//! folder inside the configuration directory (see [`AlbumStore`]).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::catalog::CatalogSource;
use crate::named_files::{NamedFile, NamedFileError, NamedFiles, checked_name};

#[cfg(test)]
mod tests;

const ALBUM_FORMAT_VERSION: u32 = 1;

/// Why an album could not be created, stored or found.
#[derive(Debug, Error)]
pub enum AlbumError {
    #[error("{0} is not an absolute path")]
    NotAbsolute(PathBuf),

    #[error("{0} cannot be in an album: its path is not valid UTF-8")]
    NotUtf8(PathBuf),

    #[error(transparent)]
    File(#[from] NamedFileError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Album {
    name: String,
    /// Absolute, without duplicates, in the order they were added.
    photos: Vec<PathBuf>,
}

impl Album {
    /// The name is trimmed; photos given more than once are taken once.
    pub fn new(name: &str, photos: &[PathBuf]) -> Result<Self, AlbumError> {
        let mut album = Self {
            name: checked_name(name)?,
            photos: Vec::new(),
        };
        album.add(photos)?;
        Ok(album)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn photos(&self) -> &[PathBuf] {
        &self.photos
    }

    /// For showing the album in the library.
    #[must_use]
    pub fn source(&self) -> CatalogSource {
        CatalogSource::Album {
            name: self.name.clone(),
            photos: self.photos.clone(),
        }
    }

    /// Add the photos not in the album yet, and return how many those were.
    fn add(&mut self, photos: &[PathBuf]) -> Result<usize, AlbumError> {
        photos.iter().try_for_each(|photo| check_path(photo))?;
        let before = self.photos.len();
        let mut known: HashSet<&Path> = self.photos.iter().map(PathBuf::as_path).collect();
        let new = photos.iter().filter(|photo| known.insert(photo));
        let new: Vec<PathBuf> = new.cloned().collect();
        self.photos.extend(new);
        Ok(self.photos.len() - before)
    }

    /// Return how many of the photos were in the album.
    fn remove(&mut self, photos: &[PathBuf]) -> usize {
        let before = self.photos.len();
        let unwanted: HashSet<&PathBuf> = photos.iter().collect();
        self.photos.retain(|photo| !unwanted.contains(photo));
        before - self.photos.len()
    }
}

/// Only absolute paths mean the same next time, and JSON only holds UTF-8.
fn check_path(photo: &Path) -> Result<(), AlbumError> {
    if !photo.is_absolute() {
        return Err(AlbumError::NotAbsolute(photo.to_path_buf()));
    }
    if photo.to_str().is_none() {
        return Err(AlbumError::NotUtf8(photo.to_path_buf()));
    }
    Ok(())
}

/// The JSON layout of one album file.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AlbumFile {
    format_version: u32,
    name: String,
    photos: Vec<PathBuf>,
}

impl NamedFile for Album {
    const DIRECTORY: &'static str = "albums";
    const MAX_FILES: usize = 1024;
    /// About a hundred thousand photos.
    const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

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
        serde_json::to_vec_pretty(&AlbumFile {
            format_version: ALBUM_FORMAT_VERSION,
            name: self.name.clone(),
            photos: self.photos.clone(),
        })
    }

    fn from_json(json: &[u8]) -> Result<Self, String> {
        let file: AlbumFile = serde_json::from_slice(json).map_err(|error| error.to_string())?;
        if file.format_version != ALBUM_FORMAT_VERSION {
            return Err(format!(
                "unsupported format version {}",
                file.format_version
            ));
        }
        Self::new(&file.name, &file.photos).map_err(|error| error.to_string())
    }
}

/// The albums, loaded from `<config directory>/albums`.
pub type AlbumStore = NamedFiles<Album>;

impl AlbumStore {
    pub fn albums(&self) -> &[Album] {
        self.items()
    }

    /// Add photos to an album and return how many were not in it yet.
    pub fn add_photos(&mut self, album: &str, photos: &[PathBuf]) -> Result<usize, AlbumError> {
        let mut changed = self.album(album)?.clone();
        let added = changed.add(photos)?;
        if added > 0 {
            self.update(changed)?;
        }
        Ok(added)
    }

    /// Remove photos from an album and return how many were in it.
    pub fn remove_photos(&mut self, album: &str, photos: &[PathBuf]) -> Result<usize, AlbumError> {
        let mut changed = self.album(album)?.clone();
        let removed = changed.remove(photos);
        if removed > 0 {
            self.update(changed)?;
        }
        Ok(removed)
    }

    fn album(&self, name: &str) -> Result<&Album, NamedFileError> {
        self.get(name)
            .ok_or_else(|| NamedFileError::NotFound(name.to_owned()))
    }
}
