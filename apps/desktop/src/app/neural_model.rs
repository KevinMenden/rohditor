//! Loading the neural demosaic model at startup.
//!
//! The model is optional: without it the "Neural" demosaic choice is disabled
//! and explains how to install it. Inference runs on the GPU when ONNX
//! Runtime's WebGPU provider works, otherwise on the CPU.

use std::path::Path;
use std::sync::Arc;

use rohditor_engine::models::{ModelStatus, ModelStore};
use rohditor_neural::{Backend, NeuralDemosaicer};
use tracing::{info, warn};

/// The loaded model, or why the neural demosaic is unavailable.
pub(super) fn load(data_dir: Option<&Path>) -> Result<Arc<NeuralDemosaicer>, String> {
    let directory = data_dir.ok_or_else(|| "the Rohditor data directory is unknown".to_owned())?;
    let store = ModelStore::new(directory);
    if let ModelStatus::Missing(path) = store.status() {
        info!(path = %path.display(), "neural demosaic model is not installed");
        return Err("no model is installed (run `rohditor-cli model install <FILE>`)".to_owned());
    }
    store.load(Backend::Gpu).map_err(|error| {
        warn!(%error, "neural model failed to load");
        error.to_string()
    })
}
