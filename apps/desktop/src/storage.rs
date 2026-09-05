//! Desktop configuration paths and transactional file replacement.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

/// Resolve Rohditor's application configuration directory without coupling
/// callers to process-global environment access.
pub(crate) fn config_directory_from(
    xdg_config_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    xdg_config_home
        .filter(|directory| !directory.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|directory| !directory.is_empty())
                .map(|directory| PathBuf::from(directory).join(".config"))
        })
        .map(|base| base.join("rohditor"))
}

pub(crate) fn config_directory() -> Option<PathBuf> {
    config_directory_from(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// Replace one complete configuration file with another. The uniquely named
/// sibling prevents concurrent processes from sharing a partial temporary
/// file; rename is atomic on the supported Linux filesystem boundary.
pub(crate) fn write_transactionally(path: &Path, contents: &[u8]) -> io::Result<()> {
    write_transactionally_with(path, contents, |from, to| fs::rename(from, to))
}

fn write_transactionally_with(
    path: &Path,
    contents: &[u8],
    rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let directory = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "configuration file has no parent directory",
        )
    })?;
    fs::create_dir_all(directory)?;

    let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .unwrap_or_else(|| OsStr::new("settings.json"))
        .to_string_lossy();
    let temporary = directory.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));

    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(contents)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
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
                "rohditor-desktop-storage-{}-{id}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create storage test directory");
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
    fn xdg_directory_precedes_the_home_fallback() {
        assert_eq!(
            config_directory_from(Some(OsStr::new("/xdg")), Some(OsStr::new("/home/me"))),
            Some(PathBuf::from("/xdg/rohditor"))
        );
        assert_eq!(
            config_directory_from(None, Some(OsStr::new("/home/me"))),
            Some(PathBuf::from("/home/me/.config/rohditor"))
        );
        assert_eq!(config_directory_from(None, None), None);
    }

    #[test]
    fn failed_rename_preserves_the_previous_complete_file() {
        let directory = TestDirectory::new();
        let path = directory.path().join("settings.json");
        fs::write(&path, b"old complete contents").expect("write old settings");

        let error = write_transactionally_with(&path, b"new contents", |_, _| {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "simulated"))
        })
        .expect_err("simulated rename should fail");

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            fs::read(&path).expect("old settings remain"),
            b"old complete contents"
        );
        assert_eq!(
            fs::read_dir(directory.path())
                .expect("read test directory")
                .count(),
            1
        );
    }
}
