//! UI-thread catalog state.
//!
//! Pure event application with folder and source-identity guards, so results
//! from a previous folder or a changed file never overwrite current state.
//! Deliberately free of egui, worker, and texture concerns for testability.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rohditor_catalog::{CatalogEntry, PlaceholderReason, Thumbnail};
use rohditor_raw::SourceIdentity;

use super::{CatalogEvent, ThumbnailResult};

#[derive(Debug, PartialEq)]
pub(crate) enum ThumbnailSlot {
    Pending,
    Ready(Thumbnail),
    Placeholder(PlaceholderReason),
    Failed(String),
}

#[derive(Debug, Default)]
pub(crate) struct CatalogState {
    folder: Option<PathBuf>,
    entries: Vec<CatalogEntry>,
    slots: Vec<ThumbnailSlot>,
    /// EXIF capture date per entry, filled as thumbnails resolve.
    captured_at: Vec<Option<String>>,
    index_by_path: HashMap<PathBuf, usize>,
    failure: Option<String>,
    scanning: bool,
}

impl CatalogState {
    pub(crate) fn apply_event(&mut self, event: CatalogEvent) {
        match event {
            CatalogEvent::ScanStarted { folder } => {
                self.folder = Some(folder);
                self.entries.clear();
                self.slots.clear();
                self.captured_at.clear();
                self.index_by_path.clear();
                self.failure = None;
                self.scanning = true;
            }
            CatalogEvent::ScanEntries { folder, entries } => {
                if !self.is_current_folder(&folder) {
                    return;
                }
                self.index_by_path = entries
                    .iter()
                    .enumerate()
                    .map(|(index, entry)| (entry.path().to_path_buf(), index))
                    .collect();
                self.slots = entries.iter().map(|_| ThumbnailSlot::Pending).collect();
                self.captured_at = entries.iter().map(|_| None).collect();
                self.entries = entries;
                self.scanning = false;
            }
            CatalogEvent::ScanFailed { folder, reason } => {
                if self.is_current_folder(&folder) {
                    self.failure = Some(reason);
                    self.scanning = false;
                }
            }
            CatalogEvent::ThumbnailReady {
                folder,
                path,
                identity,
                outcome,
                captured_at,
            } => {
                self.apply_thumbnail(folder, path, identity, outcome, captured_at);
            }
            CatalogEvent::WorkerStopped { message } => {
                self.failure = Some(message);
                self.scanning = false;
            }
        }
    }

    fn apply_thumbnail(
        &mut self,
        folder: PathBuf,
        path: PathBuf,
        identity: SourceIdentity,
        outcome: ThumbnailResult,
        captured_at: Option<String>,
    ) {
        if !self.is_current_folder(&folder) {
            return;
        }
        let Some(&index) = self.index_by_path.get(&path) else {
            return;
        };
        // A file replaced since the scan keeps its stale thumbnail out of the
        // catalog; the next scan of the folder picks up the new identity.
        if self.entries[index].source_identity() != identity {
            return;
        }
        self.slots[index] = match outcome {
            ThumbnailResult::Ready(thumbnail) => ThumbnailSlot::Ready(thumbnail),
            ThumbnailResult::Placeholder(reason) => ThumbnailSlot::Placeholder(reason),
            ThumbnailResult::Failed(message) => ThumbnailSlot::Failed(message),
        };
        self.captured_at[index] = captured_at;
    }

    fn is_current_folder(&self, folder: &Path) -> bool {
        self.folder.as_deref() == Some(folder)
    }

    pub(crate) fn folder(&self) -> Option<&Path> {
        self.folder.as_deref()
    }

    pub(crate) fn scanning(&self) -> bool {
        self.scanning
    }

    pub(crate) fn entry_path(&self, index: usize) -> Option<&Path> {
        self.entries.get(index).map(CatalogEntry::path)
    }

    pub(crate) fn entry_name(&self, index: usize) -> Option<&str> {
        self.entries.get(index).map(CatalogEntry::file_name)
    }

    pub(crate) fn slot(&self, index: usize) -> Option<&ThumbnailSlot> {
        self.slots.get(index)
    }

    /// EXIF capture date for one entry, available once its thumbnail resolved.
    pub(crate) fn capture_date(&self, index: usize) -> Option<&str> {
        self.captured_at.get(index).and_then(|date| date.as_deref())
    }

    pub(crate) fn entry_index_for_path(&self, path: &Path) -> Option<usize> {
        self.index_by_path.get(path).copied()
    }

    pub(crate) fn folder_name(&self) -> Option<String> {
        self.folder.as_deref().and_then(|folder| {
            folder
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
    }

    pub(crate) fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Entries whose thumbnail outcome (ready, placeholder, or failure) is
    /// known.
    pub(crate) fn resolved_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| !matches!(slot, ThumbnailSlot::Pending))
            .count()
    }

    pub(crate) fn ready_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| matches!(slot, ThumbnailSlot::Ready(_)))
            .count()
    }

    pub(crate) fn placeholder_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| matches!(slot, ThumbnailSlot::Placeholder(_)))
            .count()
    }

    pub(crate) fn failure_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| matches!(slot, ThumbnailSlot::Failed(_)))
            .count()
    }

    pub(crate) fn pending_count(&self) -> usize {
        self.entry_count() - self.resolved_count()
    }

    /// Encoded bytes retained by ready thumbnails.
    pub(crate) fn resident_thumbnail_bytes(&self) -> usize {
        self.slots
            .iter()
            .filter_map(|slot| match slot {
                ThumbnailSlot::Ready(thumbnail) => Some(thumbnail.bytes().len()),
                _ => None,
            })
            .sum()
    }

    pub(crate) fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    use rohditor_catalog::scan_folder;

    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, AtomicOrdering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "rohditor-desktop-catalog-state-{}-{id}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create catalog state test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn create_arw(&self, name: &str) {
            File::create(self.path().join(name)).expect("create catalog state test file");
        }

        fn scan(&self) -> Vec<CatalogEntry> {
            scan_folder(self.path()).expect("scan catalog state test folder")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn apply_scan(state: &mut CatalogState, folder: &Path, entries: Vec<CatalogEntry>) {
        state.apply_event(CatalogEvent::ScanStarted {
            folder: folder.to_path_buf(),
        });
        state.apply_event(CatalogEvent::ScanEntries {
            folder: folder.to_path_buf(),
            entries,
        });
    }

    #[test]
    fn entry_accessors_mirror_scan_results() {
        let directory = TestDirectory::new();
        directory.create_arw("a.arw");
        let entries = directory.scan();

        let mut state = CatalogState::default();
        apply_scan(&mut state, directory.path(), entries);

        assert_eq!(state.entry_name(0), Some("a.arw"));
        assert!(state.entry_path(0).expect("entry path").ends_with("a.arw"));
        assert_eq!(state.folder(), Some(directory.path()));
        assert!(matches!(state.slot(0), Some(ThumbnailSlot::Pending)));
        assert_eq!(state.entry_name(1), None);
        assert_eq!(state.entry_path(1), None);
        assert_eq!(state.slot(1), None);
    }

    #[test]
    fn scan_events_populate_entries_and_reset_previous_state() {
        let directory = TestDirectory::new();
        directory.create_arw("a.arw");
        let entries = directory.scan();

        let mut state = CatalogState::default();
        apply_scan(&mut state, directory.path(), entries);
        let expected_folder_name = directory
            .path()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .expect("test folder name");
        assert_eq!(
            state.folder_name().as_deref(),
            Some(expected_folder_name.as_str())
        );
        assert_eq!(state.entry_count(), 1);
        assert_eq!(state.resolved_count(), 0);
        assert_eq!(state.pending_count(), 1);
        assert!(!state.scanning());
        assert_eq!(state.failure(), None);
    }

    #[test]
    fn thumbnail_results_apply_only_to_current_folder_and_identity() {
        let directory = TestDirectory::new();
        directory.create_arw("a.arw");
        directory.create_arw("b.arw");
        let entries = directory.scan();
        let first = entries[0].clone();
        let second = entries[1].clone();

        let mut state = CatalogState::default();
        apply_scan(&mut state, directory.path(), entries);

        // A result for another folder never applies.
        state.apply_event(CatalogEvent::ThumbnailReady {
            folder: directory
                .path()
                .parent()
                .expect("parent folder")
                .to_path_buf(),
            path: first.path().to_path_buf(),
            identity: first.source_identity(),
            outcome: ThumbnailResult::Failed("other folder".to_owned()),
            captured_at: None,
        });
        assert_eq!(state.resolved_count(), 0);

        // A result whose source changed since the scan is rejected.
        let mut replaced = first.source_identity();
        replaced.size_bytes = first.source_identity().size_bytes + 17;
        state.apply_event(CatalogEvent::ThumbnailReady {
            folder: directory.path().to_path_buf(),
            path: first.path().to_path_buf(),
            identity: replaced,
            outcome: ThumbnailResult::Failed("changed file".to_owned()),
            captured_at: None,
        });
        assert_eq!(state.resolved_count(), 0);

        // Matching results apply to their own entries only.
        state.apply_event(CatalogEvent::ThumbnailReady {
            folder: directory.path().to_path_buf(),
            path: second.path().to_path_buf(),
            identity: second.source_identity(),
            outcome: ThumbnailResult::Failed("unreadable".to_owned()),
            captured_at: Some("2024:05:06 07:08:09".to_owned()),
        });
        assert_eq!(state.resolved_count(), 1);
        assert!(matches!(
            state.slots[1],
            ThumbnailSlot::Failed(ref reason) if reason == "unreadable"
        ));
        assert!(matches!(state.slots[0], ThumbnailSlot::Pending));
        assert_eq!(state.capture_date(1), Some("2024:05:06 07:08:09"));
        assert_eq!(state.capture_date(0), None);
    }

    #[test]
    fn scan_failures_are_reported_and_a_new_scan_clears_them() {
        let directory = TestDirectory::new();
        let entries = directory.scan();

        let mut state = CatalogState::default();
        state.apply_event(CatalogEvent::ScanStarted {
            folder: directory.path().to_path_buf(),
        });
        assert!(state.scanning());
        state.apply_event(CatalogEvent::ScanFailed {
            folder: directory.path().to_path_buf(),
            reason: "permission denied".to_owned(),
        });
        assert_eq!(state.failure(), Some("permission denied"));
        assert_eq!(state.entry_count(), 0);
        assert_eq!(state.pending_count(), 0);
        assert!(!state.scanning());

        apply_scan(&mut state, directory.path(), entries);
        assert_eq!(state.failure(), None);

        state.apply_event(CatalogEvent::WorkerStopped {
            message: "worker stopped".to_owned(),
        });
        assert_eq!(state.failure(), Some("worker stopped"));
    }
}
