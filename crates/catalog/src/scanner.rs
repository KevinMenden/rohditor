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

/// One requested file, which may not be there any more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScannedFile {
    Found(CatalogEntry),
    /// Gone, unreadable or not a regular file.
    Missing {
        path: PathBuf,
        file_name: String,
    },
}

impl ScannedFile {
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Found(entry) => entry.path(),
            Self::Missing { path, .. } => path,
        }
    }

    #[must_use]
    pub fn file_name(&self) -> &str {
        match self {
            Self::Found(entry) => entry.file_name(),
            Self::Missing { file_name, .. } => file_name,
        }
    }

    /// `None` for a missing file.
    #[must_use]
    pub const fn entry(&self) -> Option<&CatalogEntry> {
        match self {
            Self::Found(entry) => Some(entry),
            Self::Missing { .. } => None,
        }
    }
}

/// What one folder holds directly: its visible subfolders and supported files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderListing {
    pub folders: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
}

/// Scan one folder without descending into subfolders.
///
/// Individual entries that disappear, cannot be inspected, are hidden, or are
/// not regular supported files are skipped. The folder itself must be
/// inspectable and readable.
pub fn scan_folder(path: impl AsRef<Path>) -> Result<Vec<CatalogEntry>, ScanError> {
    let mut entries = Vec::new();
    for directory_entry in read_folder(path.as_ref())? {
        let Ok(directory_entry) = directory_entry else {
            continue;
        };
        let file_name = directory_entry.file_name();
        if !is_supported_file(&directory_entry) {
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
        display_order(
            (left.file_name(), left.path()),
            (right.file_name(), right.path()),
        )
    });
    Ok(entries)
}

/// Inspect a list of files from any folders, in the same order
/// [`scan_folder`] uses. Every path yields one result.
#[must_use]
pub fn scan_files(paths: &[PathBuf]) -> Vec<ScannedFile> {
    let mut files: Vec<ScannedFile> = paths.iter().map(|path| scan_file(path)).collect();
    files.sort_by(|left, right| {
        display_order(
            (left.file_name(), left.path()),
            (right.file_name(), right.path()),
        )
    });
    files
}

fn scan_file(path: &Path) -> ScannedFile {
    let path = path.to_path_buf();
    let file_name = file_name_of(&path);
    match fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => ScannedFile::Found(CatalogEntry {
            path,
            file_name,
            source_identity: source_identity(&metadata),
        }),
        _ => ScannedFile::Missing { path, file_name },
    }
}

/// List the subfolders and supported files of one folder, each in natural
/// order. Hidden entries and symbolic links are skipped, as in
/// [`scan_folder`]; files are not inspected.
pub fn list_folder(path: impl AsRef<Path>) -> Result<FolderListing, ScanError> {
    let mut listing = FolderListing::default();
    for directory_entry in read_folder(path.as_ref())? {
        let Ok(directory_entry) = directory_entry else {
            continue;
        };
        if is_supported_file(&directory_entry) {
            listing.files.push(directory_entry.path());
        } else if directory_entry
            .file_type()
            .is_ok_and(|file_type| file_type.is_dir())
            && !is_hidden(&directory_entry.file_name())
        {
            listing.folders.push(directory_entry.path());
        }
    }
    for paths in [&mut listing.folders, &mut listing.files] {
        paths.sort_by(|left, right| {
            display_order(
                (&file_name_of(left), left.as_path()),
                (&file_name_of(right), right.as_path()),
            )
        });
    }
    Ok(listing)
}

fn read_folder(path: &Path) -> Result<fs::ReadDir, ScanError> {
    let metadata = fs::metadata(path).map_err(|source| scan_inspect_error(path, source))?;
    if !metadata.is_dir() {
        return Err(ScanError::NotDirectory {
            path: path.to_path_buf(),
        });
    }
    fs::read_dir(path).map_err(|source| ScanError::ReadFolder {
        path: path.to_path_buf(),
        source,
    })
}

/// Whether the entry is a visible regular file of a supported type.
fn is_supported_file(entry: &fs::DirEntry) -> bool {
    let file_name = entry.file_name();
    entry.file_type().is_ok_and(|file_type| file_type.is_file())
        && !is_hidden(&file_name)
        && has_supported_extension(&file_name)
}

/// Natural order by name. The path decides between files of the same name.
fn display_order(left: (&str, &Path), right: (&str, &Path)) -> Ordering {
    natural_cmp(left.0, right.0)
        .then_with(|| left.0.cmp(right.0))
        .then_with(|| left.1.cmp(right.1))
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
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
    fn scanning_files_keeps_missing_ones_and_sorts_across_folders() {
        let first = TestDirectory::new();
        let second = TestDirectory::new();
        first.create_file("image10.arw");
        second.create_file("image2.arw");
        let paths = [
            first.path().join("image10.arw"),
            first.path().join("image3.arw"),
            second.path().join("image2.arw"),
            // A folder is not a photo.
            first.path().to_path_buf(),
        ];

        let files = scan_files(&paths);

        let photos: Vec<_> = files
            .iter()
            .filter(|file| file.file_name().starts_with("image"))
            .map(|file| (file.file_name(), file.entry().is_some()))
            .collect();
        assert_eq!(
            photos,
            [
                ("image2.arw", true),
                ("image3.arw", false),
                ("image10.arw", true)
            ]
        );
        assert_eq!(files.len(), 4);
        let folder = files
            .iter()
            .find(|file| file.path() == first.path())
            .expect("every path has a result");
        assert!(folder.entry().is_none());
    }

    #[test]
    fn scanning_a_file_gives_the_identity_of_the_folder_scan() {
        let directory = TestDirectory::new();
        directory.create_file("a.arw");
        let entries = scan_folder(directory.path()).expect("scan test directory");
        let files = scan_files(&[directory.path().join("a.arw")]);
        assert_eq!(files[0].entry(), Some(&entries[0]));
    }

    #[test]
    fn listing_shows_visible_folders_and_supported_files_in_natural_order() {
        let directory = TestDirectory::new();
        for name in ["day10", "day2", ".hidden", "odd.arw"] {
            fs::create_dir(directory.path().join(name)).expect("create subfolder");
        }
        for name in ["b10.ARW", "b2.arw", "notes.txt", ".c.arw"] {
            directory.create_file(name);
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(directory.path(), directory.path().join("loop"))
            .expect("create link");

        let listing = list_folder(directory.path()).expect("list test directory");

        let names = |paths: &[PathBuf]| paths.iter().map(|path| file_name_of(path)).collect();
        let folders: Vec<String> = names(&listing.folders);
        let files: Vec<String> = names(&listing.files);
        assert_eq!(folders, ["day2", "day10", "odd.arw"]);
        assert_eq!(files, ["b2.arw", "b10.ARW"]);
        assert!(matches!(
            list_folder(directory.path().join("missing")),
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
