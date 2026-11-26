//! What the library remembers between runs: the root folders and what it
//! showed last, in `library.json` in the configuration directory.
//!
//! Separate from [`crate::session`], whose file is rewritten whole.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::storage;

const FILE_NAME: &str = "library.json";
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryFile {
    pub roots: Vec<PathBuf>,
    pub shown: Option<Shown>,
}

/// What the library showed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shown {
    Folder(PathBuf),
    /// By name.
    Album(String),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    format_version: u32,
    roots: Vec<PathBuf>,
    shown: Option<Shown>,
}

/// What was remembered, or nothing if that cannot be read. Folders that are
/// not there at the moment are kept: their drive may come back.
pub fn load(config_directory: &Path) -> LibraryFile {
    let bytes = match fs::read(config_directory.join(FILE_NAME)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return LibraryFile::default(),
        Err(error) => {
            warn!(%error, "could not read the library file");
            return LibraryFile::default();
        }
    };
    match serde_json::from_slice::<Stored>(&bytes) {
        Ok(stored) if stored.format_version == FORMAT_VERSION => LibraryFile {
            roots: stored.roots,
            shown: stored.shown,
        },
        Ok(stored) => {
            let version = stored.format_version;
            warn!(version, "the library file has an unsupported version");
            LibraryFile::default()
        }
        Err(error) => {
            warn!(%error, "could not parse the library file");
            LibraryFile::default()
        }
    }
}

/// Fails for paths that are not valid UTF-8, which JSON cannot hold.
pub fn store(config_directory: &Path, library: &LibraryFile) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(&Stored {
        format_version: FORMAT_VERSION,
        roots: library.roots.clone(),
        shown: library.shown.clone(),
    })?;
    storage::write_transactionally(&config_directory.join(FILE_NAME), &json)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let name = format!("rohditor-library-file-{}-{id}", std::process::id());
            let path = std::env::temp_dir().join(name);
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn roots_and_the_shown_place_survive_a_reload() {
        let directory = TestDirectory::new();
        assert_eq!(load(&directory.0), LibraryFile::default());
        for shown in [
            Some(Shown::Folder("/photos/day".into())),
            Some(Shown::Album("Best".to_owned())),
            None,
        ] {
            // Not there, and still remembered.
            let library = LibraryFile {
                roots: vec!["/unplugged".into(), "/photos".into()],
                shown,
            };
            store(&directory.0, &library).expect("store");
            assert_eq!(load(&directory.0), library);
        }
    }

    #[test]
    fn unreadable_files_count_as_nothing() {
        let directory = TestDirectory::new();
        for contents in [
            "not json",
            r#"{"format_version": 2, "roots": ["/photos"], "shown": null}"#,
            r#"{"format_version": 1, "roots": 3, "shown": null}"#,
        ] {
            fs::write(directory.0.join(FILE_NAME), contents).expect("write");
            assert_eq!(load(&directory.0), LibraryFile::default(), "{contents}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_path_json_cannot_hold_is_an_error_and_keeps_the_file() {
        use std::os::unix::ffi::OsStringExt as _;

        let directory = TestDirectory::new();
        let good = LibraryFile {
            roots: vec!["/photos".into()],
            shown: None,
        };
        store(&directory.0, &good).expect("store");
        let odd = std::ffi::OsString::from_vec(b"/\xff".to_vec());
        let bad = LibraryFile {
            roots: vec![odd.into()],
            shown: None,
        };
        assert!(store(&directory.0, &bad).is_err());
        assert_eq!(load(&directory.0), good);
    }
}
