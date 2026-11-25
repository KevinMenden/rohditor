//! UI-thread catalog state.
//!
//! Pure event application with scan and source-identity guards, so results
//! from an earlier scan or a changed file never overwrite current state.
//! Deliberately free of egui, worker, and texture concerns for testability.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rohditor_catalog::{PlaceholderReason, ScannedFile, Thumbnail};
use rohditor_raw::SourceIdentity;

use super::{CatalogEvent, CatalogSource, ThumbnailResult};

#[derive(Debug, PartialEq)]
pub enum ThumbnailSlot {
    Pending,
    Ready(Thumbnail),
    Placeholder(PlaceholderReason),
    Failed(String),
    /// An album's photo that is not there any more.
    Missing,
}

#[derive(Debug, Default)]
pub struct CatalogState {
    source: Option<CatalogSource>,
    /// The scan the state belongs to.
    generation: u64,
    /// Changes whenever the list of photos does.
    revision: u64,
    photos: Vec<Photo>,
    index_by_path: HashMap<PathBuf, usize>,
    failure: Option<String>,
    scanning: bool,
}

#[derive(Debug)]
struct Photo {
    file: ScannedFile,
    slot: ThumbnailSlot,
    /// EXIF capture date, known once the thumbnail resolved.
    captured_at: Option<String>,
}

impl Photo {
    fn new(file: ScannedFile) -> Self {
        let slot = match file {
            ScannedFile::Found(_) => ThumbnailSlot::Pending,
            ScannedFile::Missing { .. } => ThumbnailSlot::Missing,
        };
        Self {
            file,
            slot,
            captured_at: None,
        }
    }

    fn identity(&self) -> Option<SourceIdentity> {
        self.file.entry().map(|entry| entry.source_identity())
    }
}

impl CatalogState {
    pub fn apply_event(&mut self, event: CatalogEvent) {
        match event {
            CatalogEvent::ScanStarted { generation, source } => {
                // Scanning the same place again replaces its photos when they
                // are known, so that nothing flickers.
                let same = self
                    .source
                    .as_ref()
                    .is_some_and(|current| current.is_same_place(&source));
                if !same {
                    self.set_photos(Vec::new());
                }
                self.source = Some(source);
                self.generation = generation;
                self.failure = None;
                self.scanning = true;
            }
            CatalogEvent::ScanEntries { generation, files } => {
                if generation == self.generation {
                    self.replace_photos(files);
                    self.scanning = false;
                }
            }
            CatalogEvent::ScanFailed { generation, reason } => {
                if generation == self.generation {
                    self.set_photos(Vec::new());
                    self.failure = Some(reason);
                    self.scanning = false;
                }
            }
            CatalogEvent::ThumbnailReady {
                generation,
                path,
                identity,
                outcome,
                captured_at,
            } => {
                if generation == self.generation {
                    self.apply_thumbnail(&path, identity, outcome, captured_at);
                }
            }
            // The folder tree's.
            CatalogEvent::FolderListed { .. } => {}
            CatalogEvent::WorkerStopped { message } => {
                self.failure = Some(message);
                self.scanning = false;
            }
        }
    }

    /// Show nothing until the next scan starts.
    pub fn clear(&mut self) {
        self.set_photos(Vec::new());
        self.source = None;
        // What the scan still sends belongs nowhere; scans count up from one.
        self.generation = 0;
        self.failure = None;
        self.scanning = false;
    }

    /// Take over a scan, keeping the thumbnails of photos that did not change.
    fn replace_photos(&mut self, files: Vec<ScannedFile>) {
        let mut known: HashMap<PathBuf, Photo> = std::mem::take(&mut self.photos)
            .into_iter()
            .map(|photo| (photo.file.path().to_path_buf(), photo))
            .collect();
        let photos = files.into_iter().map(|file| {
            let mut photo = Photo::new(file);
            if let Some(old) = known.remove(photo.file.path())
                && photo.identity().is_some()
                && old.identity() == photo.identity()
            {
                photo.slot = old.slot;
                photo.captured_at = old.captured_at;
            }
            photo
        });
        let photos = photos.collect();
        self.set_photos(photos);
    }

    fn set_photos(&mut self, photos: Vec<Photo>) {
        self.index_by_path = photos
            .iter()
            .enumerate()
            .map(|(index, photo)| (photo.file.path().to_path_buf(), index))
            .collect();
        self.photos = photos;
        self.revision += 1;
    }

    fn apply_thumbnail(
        &mut self,
        path: &Path,
        identity: SourceIdentity,
        outcome: ThumbnailResult,
        captured_at: Option<String>,
    ) {
        let Some(&index) = self.index_by_path.get(path) else {
            return;
        };
        let photo = &mut self.photos[index];
        // A file replaced since the scan keeps its stale thumbnail out of the
        // catalog; the next scan of the folder picks up the new identity.
        if photo.identity() != Some(identity) {
            return;
        }
        photo.slot = match outcome {
            ThumbnailResult::Ready(thumbnail) => ThumbnailSlot::Ready(thumbnail),
            ThumbnailResult::Placeholder(reason) => ThumbnailSlot::Placeholder(reason),
            ThumbnailResult::Failed(message) => ThumbnailSlot::Failed(message),
        };
        photo.captured_at = captured_at;
    }

    /// The folder or album shown.
    pub fn source(&self) -> Option<&CatalogSource> {
        self.source.as_ref()
    }

    /// The folder shown, unless it is an album.
    pub fn folder(&self) -> Option<&Path> {
        match &self.source {
            Some(CatalogSource::Folder(folder)) => Some(folder),
            _ => None,
        }
    }

    /// Changes whenever the list of photos does.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn scanning(&self) -> bool {
        self.scanning
    }

    /// The paths of all photos, missing ones included.
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.photos.iter().map(|photo| photo.file.path())
    }

    pub fn entry_path(&self, index: usize) -> Option<&Path> {
        self.photos.get(index).map(|photo| photo.file.path())
    }

    pub fn entry_name(&self, index: usize) -> Option<&str> {
        self.photos.get(index).map(|photo| photo.file.file_name())
    }

    /// What tells versions of the file apart; `None` for a missing file.
    pub fn identity(&self, index: usize) -> Option<SourceIdentity> {
        self.photos.get(index).and_then(Photo::identity)
    }

    pub fn is_missing(&self, index: usize) -> bool {
        matches!(self.slot(index), Some(ThumbnailSlot::Missing))
    }

    pub fn slot(&self, index: usize) -> Option<&ThumbnailSlot> {
        self.photos.get(index).map(|photo| &photo.slot)
    }

    /// EXIF capture date for one entry, available once its thumbnail resolved.
    pub fn capture_date(&self, index: usize) -> Option<&str> {
        self.photos.get(index)?.captured_at.as_deref()
    }

    pub fn entry_index_for_path(&self, path: &Path) -> Option<usize> {
        self.index_by_path.get(path).copied()
    }

    /// The name of the folder or album shown.
    pub fn title(&self) -> Option<String> {
        self.source.as_ref().map(CatalogSource::title)
    }

    pub fn entry_count(&self) -> usize {
        self.photos.len()
    }

    /// Entries whose thumbnail outcome (ready, placeholder, failure, or missing)
    /// is known.
    pub fn resolved_count(&self) -> usize {
        self.slots()
            .filter(|slot| !matches!(slot, ThumbnailSlot::Pending))
            .count()
    }

    pub fn ready_count(&self) -> usize {
        self.slots()
            .filter(|slot| matches!(slot, ThumbnailSlot::Ready(_)))
            .count()
    }

    pub fn placeholder_count(&self) -> usize {
        self.slots()
            .filter(|slot| matches!(slot, ThumbnailSlot::Placeholder(_)))
            .count()
    }

    pub fn failure_count(&self) -> usize {
        self.slots()
            .filter(|slot| matches!(slot, ThumbnailSlot::Failed(_)))
            .count()
    }

    pub fn pending_count(&self) -> usize {
        self.entry_count() - self.resolved_count()
    }

    /// Encoded bytes retained by ready thumbnails.
    pub fn resident_thumbnail_bytes(&self) -> usize {
        self.slots()
            .filter_map(|slot| match slot {
                ThumbnailSlot::Ready(thumbnail) => Some(thumbnail.bytes().len()),
                _ => None,
            })
            .sum()
    }

    fn slots(&self) -> impl Iterator<Item = &ThumbnailSlot> {
        self.photos.iter().map(|photo| &photo.slot)
    }

    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use rohditor_catalog::{CatalogEntry, scan_files, scan_folder};

    use super::*;
    use crate::catalog::test_directory::TestDirectory;

    fn scan(directory: &TestDirectory) -> Vec<CatalogEntry> {
        scan_folder(directory.path()).expect("scan catalog state test folder")
    }

    #[test]
    fn a_cleared_catalog_ignores_the_rest_of_its_scan() {
        let directory = TestDirectory::new();
        let mut state = CatalogState::default();
        let source = CatalogSource::Folder(directory.path().to_path_buf());
        start(&mut state, 1, source);
        state.clear();
        state.apply_event(CatalogEvent::ScanEntries {
            generation: 1,
            files: scan_files(&[directory.create_arw("a.arw")]),
        });
        assert_eq!((state.entry_count(), state.source()), (0, None));
        assert!(!state.scanning());
    }

    fn start(state: &mut CatalogState, generation: u64, source: CatalogSource) {
        state.apply_event(CatalogEvent::ScanStarted { generation, source });
    }

    fn entries(state: &mut CatalogState, generation: u64, files: Vec<ScannedFile>) {
        state.apply_event(CatalogEvent::ScanEntries { generation, files });
    }

    fn apply_scan(
        state: &mut CatalogState,
        generation: u64,
        folder: &Path,
        found: Vec<CatalogEntry>,
    ) {
        start(
            state,
            generation,
            CatalogSource::Folder(folder.to_path_buf()),
        );
        let files = found.into_iter().map(ScannedFile::Found).collect();
        entries(state, generation, files);
    }

    fn apply_album(state: &mut CatalogState, generation: u64, photos: Vec<PathBuf>) {
        let files = scan_files(&photos);
        let name = "Best".to_owned();
        start(state, generation, CatalogSource::Album { name, photos });
        entries(state, generation, files);
    }

    /// Resolve the thumbnail of `path` with a message to recognise it by.
    fn resolve(state: &mut CatalogState, generation: u64, path: &Path, message: &str) {
        let identity = scan_files(&[path.to_path_buf()])[0]
            .entry()
            .expect("the file exists")
            .source_identity();
        state.apply_event(CatalogEvent::ThumbnailReady {
            generation,
            path: path.to_path_buf(),
            identity,
            outcome: ThumbnailResult::Failed(message.to_owned()),
            captured_at: Some("2024:05:06 07:08:09".to_owned()),
        });
    }

    fn is_resolved(state: &CatalogState, index: usize, message: &str) -> bool {
        matches!(state.slot(index), Some(ThumbnailSlot::Failed(reason)) if reason == message)
    }

    #[test]
    fn entry_accessors_mirror_scan_results() {
        let directory = TestDirectory::new();
        directory.create_arw("a.arw");

        let mut state = CatalogState::default();
        apply_scan(&mut state, 1, directory.path(), scan(&directory));

        assert_eq!(state.entry_name(0), Some("a.arw"));
        assert!(state.entry_path(0).expect("entry path").ends_with("a.arw"));
        assert_eq!(state.folder(), Some(directory.path()));
        assert!(matches!(state.slot(0), Some(ThumbnailSlot::Pending)));
        assert!(state.identity(0).is_some());
        assert_eq!(state.entry_name(1), None);
        assert_eq!(state.entry_path(1), None);
        assert_eq!(state.slot(1), None);
        let name = directory.path().file_name().expect("test folder name");
        assert_eq!(state.title(), Some(name.to_string_lossy().into_owned()));
        assert_eq!(state.entry_count(), 1);
        assert_eq!(state.resolved_count(), 0);
        assert_eq!(state.pending_count(), 1);
        assert!(!state.scanning());
        assert_eq!(state.failure(), None);
    }

    #[test]
    fn results_of_an_earlier_scan_or_version_of_a_file_never_apply() {
        let directory = TestDirectory::new();
        let first = directory.create_arw("a.arw");
        let second = directory.create_arw("b.arw");
        let found = scan(&directory);
        let identity = found[0].source_identity();

        let mut state = CatalogState::default();
        apply_scan(&mut state, 1, directory.path(), found.clone());
        apply_scan(&mut state, 2, directory.path(), found);

        resolve(&mut state, 1, &first, "old scan");
        assert_eq!(state.resolved_count(), 0);
        entries(&mut state, 1, Vec::new());
        assert_eq!(state.entry_count(), 2);
        state.apply_event(CatalogEvent::ScanFailed {
            generation: 1,
            reason: "old scan".to_owned(),
        });
        assert_eq!(state.failure(), None);

        // A result whose source changed since the scan is rejected.
        let mut replaced = identity;
        replaced.size_bytes += 17;
        state.apply_event(CatalogEvent::ThumbnailReady {
            generation: 2,
            path: first,
            identity: replaced,
            outcome: ThumbnailResult::Failed("changed file".to_owned()),
            captured_at: None,
        });
        assert_eq!(state.resolved_count(), 0);

        // Matching results apply to their own entries only.
        resolve(&mut state, 2, &second, "unreadable");
        assert_eq!(state.resolved_count(), 1);
        assert!(is_resolved(&state, 1, "unreadable"));
        assert!(matches!(state.slot(0), Some(ThumbnailSlot::Pending)));
        assert_eq!(state.capture_date(1), Some("2024:05:06 07:08:09"));
        assert_eq!(state.capture_date(0), None);
    }

    #[test]
    fn scanning_again_keeps_the_thumbnails_of_unchanged_photos() {
        let directory = TestDirectory::new();
        let kept = directory.create_arw("b.arw");
        let changed = directory.create_arw("c.arw");
        let mut state = CatalogState::default();
        apply_scan(&mut state, 1, directory.path(), scan(&directory));
        resolve(&mut state, 1, &kept, "kept");
        resolve(&mut state, 1, &changed, "changed");
        let revision = state.revision();

        // A new photo shifts the others.
        directory.create_arw("a.arw");
        std::fs::write(&changed, b"longer now").expect("change a photo");
        start(
            &mut state,
            2,
            CatalogSource::Folder(directory.path().to_path_buf()),
        );
        assert!(state.scanning());
        assert_eq!(state.entry_count(), 2, "shown until the scan is done");
        assert_eq!(state.revision(), revision);

        let files = scan(&directory).into_iter().map(ScannedFile::Found);
        let files = files.collect();
        entries(&mut state, 2, files);
        assert_ne!(state.revision(), revision);
        assert!(matches!(state.slot(0), Some(ThumbnailSlot::Pending)));
        assert!(is_resolved(&state, 1, "kept"));
        assert!(state.capture_date(1).is_some());
        assert!(matches!(state.slot(2), Some(ThumbnailSlot::Pending)));
        assert_eq!(state.capture_date(2), None);
    }

    #[test]
    fn an_album_marks_missing_photos_and_takes_them_back() {
        let directory = TestDirectory::new();
        let here = directory.create_arw("a.arw");
        let gone = directory.path().join("gone.arw");
        let mut state = CatalogState::default();
        apply_album(&mut state, 1, vec![gone.clone(), here.clone()]);

        assert_eq!(state.title().as_deref(), Some("Best"));
        assert_eq!(state.folder(), None);
        assert_eq!(state.paths().collect::<Vec<_>>(), [&here, &gone]);
        assert!(!state.is_missing(0));
        assert!(state.is_missing(1));
        assert_eq!(state.entry_name(1), Some("gone.arw"));
        assert_eq!(state.identity(1), None);
        assert_eq!(state.pending_count(), 1, "nothing to wait for");

        directory.create_arw("gone.arw");
        apply_album(&mut state, 2, vec![gone, here]);
        assert!(matches!(state.slot(1), Some(ThumbnailSlot::Pending)));
    }

    #[test]
    fn another_source_starts_empty() {
        let directory = TestDirectory::new();
        directory.create_arw("a.arw");
        let mut state = CatalogState::default();
        apply_scan(&mut state, 1, directory.path(), scan(&directory));

        start(&mut state, 2, CatalogSource::Folder("/elsewhere".into()));
        assert_eq!(state.entry_count(), 0);
        assert_eq!(
            state.entry_index_for_path(&directory.path().join("a.arw")),
            None
        );
    }

    #[test]
    fn scan_failures_are_reported_and_a_new_scan_clears_them() {
        let directory = TestDirectory::new();
        directory.create_arw("a.arw");
        let mut state = CatalogState::default();
        apply_scan(&mut state, 1, directory.path(), scan(&directory));

        let folder = CatalogSource::Folder(directory.path().to_path_buf());
        start(&mut state, 2, folder);
        assert!(state.scanning());
        state.apply_event(CatalogEvent::ScanFailed {
            generation: 2,
            reason: "permission denied".to_owned(),
        });
        assert_eq!(state.failure(), Some("permission denied"));
        assert_eq!(state.entry_count(), 0);
        assert_eq!(state.pending_count(), 0);
        assert!(!state.scanning());

        apply_scan(&mut state, 3, directory.path(), scan(&directory));
        assert_eq!(state.failure(), None);

        state.apply_event(CatalogEvent::WorkerStopped {
            message: "worker stopped".to_owned(),
        });
        assert_eq!(state.failure(), Some("worker stopped"));
    }
}
