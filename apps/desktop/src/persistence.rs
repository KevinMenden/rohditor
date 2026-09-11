//! Desktop-local persistence for non-destructive RAW edits.
//!
//! Recipes live beside their source file and remain separate from the RAW
//! bytes. The core recipe owns schema migration and validation; this module
//! only owns the desktop sidecar format and atomic replacement.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rohditor_edit::{EditError, EditRecipe};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::storage;

const SIDECAR_SUFFIX: &str = ".rohdit";
const PROJECT_FORMAT_VERSION: u32 = 1;
const AUTOSAVE_DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectFile {
    format_version: u32,
    recipe: EditRecipe,
}

#[derive(Debug, Error)]
pub(crate) enum PersistenceError {
    #[error("could not derive a sidecar path for source {raw_file}")]
    InvalidSourcePath { raw_file: PathBuf },

    #[error("could not read recipe sidecar {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not parse recipe sidecar {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("recipe sidecar {path} uses unsupported format version {actual}; expected {expected}")]
    UnsupportedFormat {
        path: PathBuf,
        actual: u32,
        expected: u32,
    },

    #[error("recipe in sidecar {path} is invalid: {source}")]
    InvalidRecipe {
        path: PathBuf,
        #[source]
        source: EditError,
    },

    #[error("could not serialize recipe sidecar {path}: {source}")]
    Serialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("could not write recipe sidecar {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Return the recipe sidecar path for one RAW source.
pub(crate) fn sidecar_path(source: &Path) -> Result<PathBuf, PersistenceError> {
    let Some(file_name) = source.file_name() else {
        return Err(PersistenceError::InvalidSourcePath {
            raw_file: source.to_owned(),
        });
    };
    let mut sidecar_name = OsString::from(file_name);
    sidecar_name.push(SIDECAR_SUFFIX);
    Ok(source
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(sidecar_name))
}

/// Load the saved recipe for a source, if its sidecar exists.
pub(crate) fn load_recipe(source: &Path) -> Result<Option<EditRecipe>, PersistenceError> {
    let path = sidecar_path(source)?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(PersistenceError::Read { path, source }),
    };
    let project = serde_json::from_slice::<ProjectFile>(&bytes).map_err(|source| {
        PersistenceError::Parse {
            path: path.clone(),
            source,
        }
    })?;
    if project.format_version != PROJECT_FORMAT_VERSION {
        return Err(PersistenceError::UnsupportedFormat {
            path,
            actual: project.format_version,
            expected: PROJECT_FORMAT_VERSION,
        });
    }
    let recipe = project.recipe;
    recipe
        .validate()
        .map_err(|source| PersistenceError::InvalidRecipe { path, source })?;
    Ok(Some(recipe))
}

/// Persist the current recipe without touching the source RAW file.
pub(crate) fn save_recipe(source: &Path, recipe: &EditRecipe) -> Result<PathBuf, PersistenceError> {
    let path = sidecar_path(source)?;
    recipe
        .validate()
        .map_err(|source| PersistenceError::InvalidRecipe {
            path: path.clone(),
            source,
        })?;
    let project = ProjectFile {
        format_version: PROJECT_FORMAT_VERSION,
        recipe: recipe.clone(),
    };
    let bytes =
        serde_json::to_vec_pretty(&project).map_err(|source| PersistenceError::Serialize {
            path: path.clone(),
            source,
        })?;
    storage::write_transactionally(&path, &bytes).map_err(|source| PersistenceError::Write {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

/// A complete, immutable recipe snapshot handed to the persistence thread.
#[derive(Debug, Clone)]
pub(crate) struct SaveJob {
    pub(crate) document_id: u64,
    pub(crate) revision: u64,
    pub(crate) source: PathBuf,
    pub(crate) recipe: EditRecipe,
}

#[derive(Debug)]
pub(crate) struct SaveResult {
    pub(crate) document_id: u64,
    pub(crate) revision: u64,
    pub(crate) source: PathBuf,
    pub(crate) recipe: EditRecipe,
    pub(crate) result: Result<PathBuf, String>,
}

enum SaveCommand {
    Save { job: Box<SaveJob>, immediate: bool },
    Shutdown,
}

struct PendingSave {
    job: SaveJob,
    due: Instant,
}

/// Debounced, newest-wins sidecar writer. It deliberately has no UI or image
/// processing dependencies; the application polls completed writes each frame.
pub(crate) struct SaveWorker {
    requests: Option<Sender<SaveCommand>>,
    results: Receiver<SaveResult>,
    worker: Option<JoinHandle<()>>,
}

impl SaveWorker {
    pub(crate) fn new() -> io::Result<Self> {
        let (request_sender, request_receiver) = mpsc::channel();
        let (result_sender, result_receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("rohditor-recipe-writer".to_owned())
            .spawn(move || save_worker_loop(request_receiver, result_sender))?;
        Ok(Self {
            requests: Some(request_sender),
            results: result_receiver,
            worker: Some(worker),
        })
    }

    pub(crate) fn enqueue(&self, job: SaveJob, immediate: bool) -> Result<(), String> {
        self.requests
            .as_ref()
            .ok_or_else(|| "recipe save worker is stopped".to_owned())?
            .send(SaveCommand::Save {
                job: Box::new(job),
                immediate,
            })
            .map_err(|_| "recipe save worker stopped unexpectedly".to_owned())
    }

    pub(crate) fn try_results(&self) -> impl Iterator<Item = SaveResult> + '_ {
        self.results.try_iter()
    }

    pub(crate) fn shutdown(&mut self) {
        if let Some(requests) = self.requests.take() {
            drop(requests.send(SaveCommand::Shutdown));
        }
        if let Some(worker) = self.worker.take() {
            drop(worker.join());
        }
    }
}

impl Drop for SaveWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn save_worker_loop(receiver: Receiver<SaveCommand>, results: Sender<SaveResult>) {
    let mut pending = HashMap::<PathBuf, PendingSave>::new();
    loop {
        if pending.is_empty() {
            match receiver.recv() {
                Ok(SaveCommand::Save { job, immediate }) => {
                    queue_save(&mut pending, *job, immediate);
                }
                Ok(SaveCommand::Shutdown) | Err(_) => break,
            }
            continue;
        }

        let now = Instant::now();
        let next_due = pending.values().map(|save| save.due).min().unwrap_or(now);
        let timeout = next_due.saturating_duration_since(now);
        match receiver.recv_timeout(timeout) {
            Ok(SaveCommand::Save { job, immediate }) => {
                queue_save(&mut pending, *job, immediate);
            }
            Ok(SaveCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                flush_pending(&mut pending, &results);
                break;
            }
            Err(RecvTimeoutError::Timeout) => flush_due(&mut pending, &results),
        }
    }
}

fn queue_save(pending: &mut HashMap<PathBuf, PendingSave>, job: SaveJob, immediate: bool) {
    let due = if immediate {
        Instant::now()
    } else {
        Instant::now() + AUTOSAVE_DEBOUNCE
    };
    pending.insert(job.source.clone(), PendingSave { job, due });
}

fn flush_due(pending: &mut HashMap<PathBuf, PendingSave>, results: &Sender<SaveResult>) {
    let now = Instant::now();
    let due_paths = pending
        .iter()
        .filter(|(_, save)| save.due <= now)
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    for path in due_paths {
        if let Some(save) = pending.remove(&path) {
            write_save(save.job, results);
        }
    }
}

fn flush_pending(pending: &mut HashMap<PathBuf, PendingSave>, results: &Sender<SaveResult>) {
    let saves = pending
        .drain()
        .map(|(_, save)| save.job)
        .collect::<Vec<_>>();
    for job in saves {
        write_save(job, results);
    }
}

fn write_save(job: SaveJob, results: &Sender<SaveResult>) {
    let result = save_recipe(&job.source, &job.recipe).map_err(|error| error.to_string());
    drop(results.send(SaveResult {
        document_id: job.document_id,
        revision: job.revision,
        source: job.source,
        recipe: job.recipe,
        result,
    }));
}

#[cfg(test)]
mod tests {
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
                "rohditor-desktop-persistence-{}-{id}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create persistence test directory");
            Self(path)
        }

        fn source(&self) -> PathBuf {
            self.0.join("photo.ARW")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn sidecar_path_keeps_the_source_name_and_adds_a_known_suffix() {
        let path = sidecar_path(Path::new("/photos/holiday.ARW")).expect("sidecar path");
        assert_eq!(path, PathBuf::from("/photos/holiday.ARW.rohdit"));
        assert!(sidecar_path(Path::new("/")).is_err());
    }

    #[test]
    fn recipes_round_trip_with_an_application_format_version() {
        let directory = TestDirectory::new();
        let source = directory.source();
        let mut expected = EditRecipe::default();
        expected.light.exposure_ev = 1.25;
        expected.color.saturation = 1.2;
        expected.capture_sharpening = rohditor_edit::CaptureSharpening {
            enabled: true,
            amount: 0.7,
            radius: 0.8,
            noise_protection: 0.3,
        };

        let path = save_recipe(&source, &expected).expect("save recipe");
        let text = fs::read_to_string(&path).expect("read sidecar");
        assert!(text.contains("\"format_version\": 1"));
        assert_eq!(load_recipe(&source).expect("load recipe"), Some(expected));
    }

    #[test]
    fn absent_sidecars_are_distinct_from_load_errors() {
        let directory = TestDirectory::new();
        assert_eq!(
            load_recipe(&directory.source()).expect("missing sidecar"),
            None
        );
    }

    #[test]
    fn malformed_and_future_sidecars_are_reported() {
        let directory = TestDirectory::new();
        let source = directory.source();
        let path = sidecar_path(&source).expect("sidecar path");

        fs::write(&path, b"not json").expect("write malformed sidecar");
        assert!(matches!(
            load_recipe(&source),
            Err(PersistenceError::Parse { .. })
        ));

        fs::write(
            &path,
            br#"{"format_version":99,"recipe":{"schema_version":10,"light":{},"color":{},"geometry":{}}}"#,
        )
        .expect("write future sidecar");
        assert!(matches!(
            load_recipe(&source),
            Err(PersistenceError::UnsupportedFormat { .. })
        ));
    }

    #[test]
    fn background_writer_flushes_the_newest_snapshot_on_shutdown() {
        let directory = TestDirectory::new();
        let source = directory.source();
        let mut first = EditRecipe::default();
        first.light.exposure_ev = 0.5;
        let mut second = first.clone();
        second.light.exposure_ev = 1.0;
        let mut worker = SaveWorker::new().expect("start save worker");
        worker
            .enqueue(
                SaveJob {
                    document_id: 1,
                    revision: 1,
                    source: source.clone(),
                    recipe: first,
                },
                true,
            )
            .expect("queue first save");
        worker
            .enqueue(
                SaveJob {
                    document_id: 1,
                    revision: 2,
                    source: source.clone(),
                    recipe: second.clone(),
                },
                true,
            )
            .expect("queue second save");
        worker.shutdown();
        assert_eq!(
            load_recipe(&source).expect("load saved recipe"),
            Some(second)
        );
    }
}
