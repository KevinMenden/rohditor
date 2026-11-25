//! What the library shows: a folder or an album.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogSource {
    /// The photos directly in a folder.
    Folder(PathBuf),
    /// Photos from any folder.
    Album { name: String, photos: Vec<PathBuf> },
}

impl CatalogSource {
    /// The folder's or album's name.
    #[must_use]
    pub fn title(&self) -> String {
        match self {
            Self::Folder(folder) => folder.file_name().map_or_else(
                || folder.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
            Self::Album { name, .. } => name.clone(),
        }
    }

    /// Whether both are the same folder or the same album, which may hold
    /// other photos by now.
    #[must_use]
    pub fn is_same_place(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Folder(one), Self::Folder(other)) => one == other,
            (Self::Album { name: one, .. }, Self::Album { name: other, .. }) => one == other,
            _ => false,
        }
    }

    /// Whether showing this source shows `photo`.
    #[must_use]
    pub fn contains(&self, photo: &Path) -> bool {
        match self {
            Self::Folder(folder) => photo.parent() == Some(folder),
            Self::Album { photos, .. } => photos.iter().any(|path| path == photo),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(name: &str, photos: &[&str]) -> CatalogSource {
        CatalogSource::Album {
            name: name.to_owned(),
            photos: photos.iter().map(PathBuf::from).collect(),
        }
    }

    #[test]
    fn a_folder_contains_only_its_direct_photos() {
        let folder = CatalogSource::Folder("/photos".into());
        assert!(folder.contains(Path::new("/photos/a.arw")));
        assert!(!folder.contains(Path::new("/photos/day/a.arw")));
        assert!(!folder.contains(Path::new("/a.arw")));
        assert_eq!(folder.title(), "photos");
        assert_eq!(CatalogSource::Folder("/".into()).title(), "/");
    }

    #[test]
    fn an_album_contains_its_photos_and_stays_the_same_when_they_change() {
        let before = album("Best", &["/x/a.arw"]);
        let after = album("Best", &["/x/a.arw", "/y/b.arw"]);
        assert!(after.contains(Path::new("/y/b.arw")));
        assert!(!before.contains(Path::new("/y/b.arw")));
        assert!(before.is_same_place(&after));
        assert!(!before.is_same_place(&album("Other", &["/x/a.arw"])));
        assert!(!before.is_same_place(&CatalogSource::Folder("Best".into())));
    }
}
