//! The installed neural demosaic model in the user's data directory.
//!
//! Models are not shipped in git or in the binaries; the user installs their
//! own export. The frontend chooses the data directory (XDG for the desktop and
//! CLI) and passes it in. Any model that ONNX Runtime accepts with one input and
//! one output is installed; its sha256 identifies it in caches.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rohditor_neural::{Backend, ModelFile, ModelId, NeuralDemosaicer, NeuralError, OrtRunner};
use tracing::{info, warn};

use crate::storage;

/// File name of the installed model inside `<data directory>/models`.
const MODEL_FILE_NAME: &str = "neural-demosaic.onnx";

/// Whether a model is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStatus {
    Installed { path: PathBuf, id: ModelId },
    Missing(PathBuf),
    Unreadable { path: PathBuf, reason: String },
}

/// The neural demosaic model inside `<data directory>/models`.
#[derive(Debug, Clone)]
pub struct ModelStore {
    directory: PathBuf,
}

impl ModelStore {
    #[must_use]
    pub fn new(data_directory: &Path) -> Self {
        Self {
            directory: data_directory.join("models"),
        }
    }

    /// Where the model is (or would be) installed.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.directory.join(MODEL_FILE_NAME)
    }

    /// Reads and hashes the installed file.
    #[must_use]
    pub fn status(&self) -> ModelStatus {
        let path = self.path();
        match ModelFile::read(&path) {
            Ok(model) => ModelStatus::Installed {
                path,
                id: model.id(),
            },
            Err(NeuralError::ModelMissing { .. }) => ModelStatus::Missing(path),
            Err(error) => ModelStatus::Unreadable {
                path,
                reason: error.to_string(),
            },
        }
    }

    /// Load the installed model, on the GPU when possible.
    pub fn load(&self, backend: Backend) -> Result<Arc<NeuralDemosaicer>, NeuralError> {
        NeuralDemosaicer::load(&self.path(), backend).map(Arc::new)
    }

    /// Install `source` (for example `ml/runs/shippable/export/model.onnx`),
    /// replacing any installed model. The file must load into ONNX Runtime
    /// first, and is written atomically, so a bad file never replaces a
    /// working model.
    pub fn install_from_file(&self, source: &Path) -> Result<(PathBuf, ModelId), String> {
        let model = ModelFile::read(source).map_err(|error| error.to_string())?;
        OrtRunner::new(&model, Backend::Cpu)
            .map_err(|error| format!("{} is not a usable model: {error}", source.display()))?;
        std::fs::create_dir_all(&self.directory)
            .map_err(|error| format!("could not create {}: {error}", self.directory.display()))?;
        let path = self.path();
        storage::write_transactionally(&path, model.bytes())
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
        Ok((path, model.id()))
    }
}

/// The installed model for a frontend's startup, or why the neural demosaic
/// is unavailable, said so that a settings UI can show it (each frontend adds
/// how to install one). `data_directory`
/// is `None` when the frontend could not determine one. Inference runs on the
/// GPU when ONNX Runtime's WebGPU provider works, otherwise (or with
/// `cpu_only`) on the CPU.
pub fn load_installed(
    data_directory: Option<&Path>,
    cpu_only: bool,
) -> Result<Arc<NeuralDemosaicer>, String> {
    let directory =
        data_directory.ok_or_else(|| "the Rohditor data directory is unknown".to_owned())?;
    let store = ModelStore::new(directory);
    if let ModelStatus::Missing(path) = store.status() {
        info!(path = %path.display(), "neural demosaic model is not installed");
        return Err("no model is installed".to_owned());
    }
    let backend = if cpu_only { Backend::Cpu } else { Backend::Gpu };
    store.load(backend).map_err(|error| {
        warn!(%error, "neural model failed to load");
        error.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("rohditor-models-{name}-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&path));
        path
    }

    #[test]
    fn installs_a_model_and_rejects_files_that_are_not_models() {
        let directory = scratch("install");
        let store = ModelStore::new(&directory);
        assert_eq!(store.status(), ModelStatus::Missing(store.path()));

        std::fs::create_dir_all(&directory).expect("scratch");
        let garbage = directory.join("garbage.onnx");
        std::fs::write(&garbage, b"not a model").expect("write");
        let error = store.install_from_file(&garbage).expect_err("not ONNX");
        assert!(error.contains("not a usable model"), "{error}");
        assert_eq!(store.status(), ModelStatus::Missing(store.path()));

        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../neural/tests/data/tiny.onnx");
        let (path, id) = store.install_from_file(&fixture).expect("install");
        assert_eq!(path, store.path());
        assert_eq!(id, ModelFile::read(&fixture).expect("fixture").id());
        assert_eq!(store.status(), ModelStatus::Installed { path, id });
        drop(std::fs::remove_dir_all(&directory));
    }

    #[test]
    fn a_missing_model_or_directory_says_why() {
        let missing = load_installed(None, true).expect_err("no directory");
        assert!(missing.contains("data directory"), "{missing}");
        let directory = scratch("startup");
        let missing = load_installed(Some(&directory), true).expect_err("no model");
        assert_eq!(missing, "no model is installed");
    }
}
