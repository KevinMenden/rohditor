//! Tiny session persistence: the last browsed catalog folder.
//!
//! Kept deliberately minimal — one JSON file in the XDG config directory.
//! Failures are non-fatal: the catalog simply starts empty.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::storage;

#[derive(Debug, Serialize, Deserialize, Default)]
struct SessionFile {
    last_folder: Option<PathBuf>,
}

/// Restore the last browsed folder, if one was recorded and still exists.
pub(crate) fn load_last_folder() -> Option<PathBuf> {
    let directory = storage::config_directory()?;
    let folder = load_folder_from(&directory)?;
    folder.is_dir().then_some(folder)
}

/// Record the last browsed folder for the next session.
pub(crate) fn store_last_folder(folder: &Path) {
    let Some(directory) = storage::config_directory() else {
        return;
    };
    store_folder_to(&directory, folder);
}

fn load_folder_from(directory: &Path) -> Option<PathBuf> {
    let bytes = match fs::read(directory.join("session.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            warn!(%error, "could not read the Rohditor session file");
            return None;
        }
    };
    match serde_json::from_slice::<SessionFile>(&bytes) {
        Ok(file) => file.last_folder,
        Err(error) => {
            warn!(%error, "could not parse the Rohditor session file");
            None
        }
    }
}

fn store_folder_to(directory: &Path, folder: &Path) {
    let file = SessionFile {
        last_folder: Some(folder.to_path_buf()),
    };
    let Ok(bytes) = serde_json::to_vec(&file) else {
        return;
    };
    if let Err(error) = fs::create_dir_all(directory)
        .and_then(|()| fs::write(directory.join("session.json"), bytes))
    {
        warn!(%error, "could not persist the last catalog folder");
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, AtomicOrdering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "rohditor-desktop-session-{}-{id}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create session test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn last_folder_round_trips_and_survives_restart_semantics() {
        let directory = TestDirectory::new();
        let folder = directory.path().join("Holiday");
        fs::create_dir(&folder).expect("create browsed folder");

        assert_eq!(load_folder_from(directory.path()), None);
        store_folder_to(directory.path(), &folder);
        assert_eq!(load_folder_from(directory.path()), Some(folder));
    }

    #[test]
    fn malformed_or_missing_session_files_are_tolerated() {
        let directory = TestDirectory::new();
        fs::write(directory.path().join("session.json"), b"not json")
            .expect("write malformed session file");
        assert_eq!(load_folder_from(directory.path()), None);
    }

    #[test]
    fn restored_folders_must_still_exist() {
        let directory = TestDirectory::new();
        let folder = directory.path().join("Gone");
        fs::create_dir(&folder).expect("create soon-deleted folder");
        store_folder_to(directory.path(), &folder);
        fs::remove_dir_all(&folder).expect("delete the folder");
        assert_eq!(load_last_folder_for_test(directory.path()), None);
    }

    /// Same existence check as `load_last_folder`, with an injectable
    /// directory so the test never touches the user's real configuration.
    fn load_last_folder_for_test(directory: &Path) -> Option<PathBuf> {
        load_folder_from(directory).filter(|folder| folder.is_dir())
    }
}
