use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::export::ExportError;

static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Commit already encoded bytes through the same sibling-file transaction used
/// by developed image exports.
///
/// The destination changes only after the complete payload has been written,
/// flushed, and synchronized. When `overwrite` is false, an existing path is
/// never replaced.
///
/// # Errors
///
/// Returns [`ExportError`] when a destination exists without overwrite
/// permission or when any temporary-write or commit operation fails.
pub fn write_output_bytes(
    destination: &Path,
    bytes: &[u8],
    overwrite: bool,
) -> Result<u64, ExportError> {
    write_transactionally(destination, overwrite, |writer| {
        writer
            .write_all(bytes)
            .map_err(|source| ExportError::DestinationIo {
                operation: "write temporary",
                path: destination.to_owned(),
                source,
            })
    })
}

/// Determine whether two existing path names resolve to the same file.
///
/// Symbolic links are followed. A missing `right` path is considered distinct,
/// while errors inspecting either existing path are reported to the caller.
///
/// # Errors
///
/// Returns an I/O error when the left path or an existing right path cannot be
/// inspected.
pub fn paths_refer_to_same_file(left: &Path, right: &Path) -> io::Result<bool> {
    if left == right {
        fs::metadata(left)?;
        return Ok(true);
    }
    let left_metadata = fs::metadata(left)?;
    let right_metadata = match fs::metadata(right) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Ok(left_metadata.dev() == right_metadata.dev()
            && left_metadata.ino() == right_metadata.ino())
    }
    #[cfg(not(unix))]
    {
        drop((left_metadata, right_metadata));
        Ok(fs::canonicalize(left)? == fs::canonicalize(right)?)
    }
}

pub(crate) fn write_transactionally<F>(
    destination: &Path,
    overwrite: bool,
    write: F,
) -> Result<u64, ExportError>
where
    F: FnOnce(&mut dyn Write) -> Result<(), ExportError>,
{
    if !overwrite && path_entry_exists(destination)? {
        return Err(ExportError::AlreadyExists {
            path: destination.to_owned(),
        });
    }

    let mut temporary = TemporaryOutput::create(destination)?;
    let temporary_path = temporary.path.clone();
    {
        let mut writer = BufWriter::new(temporary.file_mut());
        write(&mut writer)?;
        writer
            .flush()
            .map_err(|source| ExportError::DestinationIo {
                operation: "flush temporary",
                path: temporary_path,
                source,
            })?;
    }
    temporary
        .file_mut()
        .sync_all()
        .map_err(|source| ExportError::DestinationIo {
            operation: "synchronize temporary",
            path: temporary.path.clone(),
            source,
        })?;
    let bytes_written = temporary
        .file_mut()
        .metadata()
        .map_err(|source| ExportError::DestinationIo {
            operation: "inspect temporary",
            path: temporary.path.clone(),
            source,
        })?
        .len();
    temporary.commit(destination, overwrite)?;
    Ok(bytes_written)
}

fn path_entry_exists(path: &Path) -> Result<bool, ExportError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(ExportError::DestinationIo {
            operation: "inspect destination",
            path: path.to_owned(),
            source,
        }),
    }
}

#[derive(Debug)]
struct TemporaryOutput {
    path: PathBuf,
    file: Option<File>,
}

impl TemporaryOutput {
    fn create(destination: &Path) -> Result<Self, ExportError> {
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        let name = destination
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("export");
        for _ in 0..1_024 {
            let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(
                ".{name}.rohditor-{}-{sequence}.tmp",
                std::process::id()
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(source) => {
                    return Err(ExportError::DestinationIo {
                        operation: "create temporary",
                        path,
                        source,
                    });
                }
            }
        }
        Err(ExportError::DestinationIo {
            operation: "create unique temporary",
            path: destination.to_owned(),
            source: io::Error::new(
                io::ErrorKind::AlreadyExists,
                "temporary filename attempts were exhausted",
            ),
        })
    }

    fn file_mut(&mut self) -> &mut File {
        self.file
            .as_mut()
            .expect("temporary output owns its file before commit")
    }

    fn commit(mut self, destination: &Path, overwrite: bool) -> Result<(), ExportError> {
        drop(self.file.take());
        if overwrite {
            fs::rename(&self.path, destination).map_err(|source| ExportError::DestinationIo {
                operation: "atomically replace destination",
                path: destination.to_owned(),
                source,
            })?;
        } else {
            match fs::hard_link(&self.path, destination) {
                Ok(()) => {
                    if fs::remove_file(&self.path).is_ok() {
                        self.path.clear();
                    }
                }
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                    return Err(ExportError::AlreadyExists {
                        path: destination.to_owned(),
                    });
                }
                Err(source) => {
                    return Err(ExportError::DestinationIo {
                        operation: "atomically install destination",
                        path: destination.to_owned(),
                        source,
                    });
                }
            }
        }
        if overwrite {
            self.path.clear();
        }
        Ok(())
    }
}

impl Drop for TemporaryOutput {
    fn drop(&mut self) {
        drop(self.file.take());
        if !self.path.as_os_str().is_empty() {
            let _cleanup_result = fs::remove_file(&self.path);
        }
    }
}
