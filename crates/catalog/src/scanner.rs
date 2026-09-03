use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rohditor_raw::SourceIdentity;
use thiserror::Error;

/// File extensions included by the first catalog implementation.
pub const SUPPORTED_EXTENSIONS: &[&str] = &["arw"];

/// A supported image discovered in a catalog folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    path: PathBuf,
    file_name: String,
    source_identity: SourceIdentity,
}

impl CatalogEntry {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    #[must_use]
    pub const fn source_identity(&self) -> SourceIdentity {
        self.source_identity
    }
}

/// Errors encountered before a folder can be scanned.
#[derive(Debug, Error)]
pub enum ScanError {
    #[error("catalog path does not exist: {path}")]
    NotFound { path: PathBuf },

    #[error("catalog path is not a directory: {path}")]
    NotDirectory { path: PathBuf },

    #[error("could not inspect catalog folder {path}: {source}")]
    Inspect {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not read catalog folder {path}: {source}")]
    ReadFolder {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Scan one folder without descending into subfolders.
///
/// Individual entries that disappear, cannot be inspected, are hidden, or are
/// not regular supported files are skipped. The folder itself must be
/// inspectable and readable.
pub fn scan_folder(path: impl AsRef<Path>) -> Result<Vec<CatalogEntry>, ScanError> {
    let path = path.as_ref();
    let metadata = fs::metadata(path).map_err(|source| scan_inspect_error(path, source))?;
    if !metadata.is_dir() {
        return Err(ScanError::NotDirectory {
            path: path.to_path_buf(),
        });
    }

    let directory = fs::read_dir(path).map_err(|source| ScanError::ReadFolder {
        path: path.to_path_buf(),
        source,
    })?;
    let mut entries = Vec::new();

    for directory_entry in directory {
        let Ok(directory_entry) = directory_entry else {
            continue;
        };
        let Ok(file_type) = directory_entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }

        let file_name = directory_entry.file_name();
        if is_hidden(&file_name) || !has_supported_extension(&file_name) {
            continue;
        }
        let Ok(metadata) = directory_entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }

        entries.push(CatalogEntry {
            path: directory_entry.path(),
            file_name: file_name.to_string_lossy().into_owned(),
            source_identity: source_identity(&metadata),
        });
    }

    entries.sort_by(|left, right| {
        natural_cmp(left.file_name(), right.file_name())
            .then_with(|| left.file_name().cmp(right.file_name()))
            .then_with(|| left.path().cmp(right.path()))
    });
    Ok(entries)
}

fn scan_inspect_error(path: &Path, source: io::Error) -> ScanError {
    if source.kind() == io::ErrorKind::NotFound {
        ScanError::NotFound {
            path: path.to_path_buf(),
        }
    } else {
        ScanError::Inspect {
            path: path.to_path_buf(),
            source,
        }
    }
}

fn is_hidden(file_name: &std::ffi::OsStr) -> bool {
    file_name.to_string_lossy().starts_with('.')
}

fn has_supported_extension(file_name: &std::ffi::OsStr) -> bool {
    Path::new(file_name).extension().is_some_and(|extension| {
        let extension = extension.to_string_lossy();
        SUPPORTED_EXTENSIONS
            .iter()
            .any(|supported| extension.eq_ignore_ascii_case(supported))
    })
}

fn source_identity(metadata: &fs::Metadata) -> SourceIdentity {
    SourceIdentity {
        size_bytes: metadata.len(),
        modified_unix_nanos: metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos()),
        filesystem_device: filesystem_device(metadata),
        filesystem_inode: filesystem_inode(metadata),
    }
}

#[cfg(unix)]
fn filesystem_device(metadata: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;

    Some(metadata.dev())
}

#[cfg(not(unix))]
const fn filesystem_device(_metadata: &fs::Metadata) -> Option<u64> {
    None
}

#[cfg(unix)]
fn filesystem_inode(metadata: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;

    Some(metadata.ino())
}

#[cfg(not(unix))]
const fn filesystem_inode(_metadata: &fs::Metadata) -> Option<u64> {
    None
}

fn natural_cmp(left: &str, right: &str) -> Ordering {
    let left = left.to_ascii_lowercase();
    let right = right.to_ascii_lowercase();
    let left = left.as_bytes();
    let right = right.as_bytes();
    let mut left_index = 0;
    let mut right_index = 0;

    while left_index < left.len() && right_index < right.len() {
        let left_byte = left[left_index];
        let right_byte = right[right_index];
        if left_byte.is_ascii_digit() && right_byte.is_ascii_digit() {
            let left_end = digit_run_end(left, left_index);
            let right_end = digit_run_end(right, right_index);
            let left_significant = trim_leading_zeroes(&left[left_index..left_end]);
            let right_significant = trim_leading_zeroes(&right[right_index..right_end]);

            if left_significant.len() != right_significant.len() {
                return left_significant.len().cmp(&right_significant.len());
            }
            if left_significant != right_significant {
                return left_significant.cmp(right_significant);
            }
            if left_end - left_index != right_end - right_index {
                return (left_end - left_index).cmp(&(right_end - right_index));
            }
            left_index = left_end;
            right_index = right_end;
        } else {
            match left_byte.cmp(&right_byte) {
                Ordering::Equal => {
                    left_index += 1;
                    right_index += 1;
                }
                order => return order,
            }
        }
    }

    left.len().cmp(&right.len())
}

fn digit_run_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    end
}

fn trim_leading_zeroes(digits: &[u8]) -> &[u8] {
    let first_non_zero = digits
        .iter()
        .position(|digit| *digit != b'0')
        .unwrap_or(digits.len().saturating_sub(1));
    &digits[first_non_zero..]
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, AtomicOrdering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "rohditor-catalog-scanner-{}-{id}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create scanner test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn create_file(&self, name: &str) {
            File::create(self.path().join(name)).expect("create scanner test file");
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn scans_visible_arw_files_in_natural_order() {
        let directory = TestDirectory::new();
        directory.create_file("image10.ARW");
        directory.create_file("image2.arw");
        directory.create_file("image1.arw");
        directory.create_file("notes.txt");
        directory.create_file(".hidden.arw");
        fs::create_dir(directory.path().join("nested.arw")).expect("create nested directory");

        let entries = scan_folder(directory.path()).expect("scan test directory");

        assert_eq!(
            entries
                .iter()
                .map(CatalogEntry::file_name)
                .collect::<Vec<_>>(),
            ["image1.arw", "image2.arw", "image10.ARW"]
        );
        assert_eq!(entries[0].source_identity().size_bytes, 0);
    }

    #[test]
    fn reports_invalid_scan_roots() {
        let directory = TestDirectory::new();
        let file = directory.path().join("image.arw");
        File::create(&file).expect("create file");
        assert!(matches!(
            scan_folder(&file),
            Err(ScanError::NotDirectory { .. })
        ));
        assert!(matches!(
            scan_folder(directory.path().join("missing")),
            Err(ScanError::NotFound { .. })
        ));
    }

    #[test]
    fn natural_sort_compares_numeric_runs() {
        assert_eq!(
            natural_cmp("DSC2.ARW", "DSC10.ARW"),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            natural_cmp("DSC02.ARW", "DSC2.ARW"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(natural_cmp("a", "A"), std::cmp::Ordering::Equal);
    }
}
