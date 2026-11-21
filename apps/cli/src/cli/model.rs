//! `model`: install and check the neural demosaic model, and load it for
//! `--demosaic neural`.
//!
//! The model lives in `$XDG_DATA_HOME/rohditor/models` (default
//! `~/.local/share/rohditor/models`), shared with the desktop app. Any ONNX
//! export of the v1 layout can be installed; its sha256 identifies it.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use clap::{Args, Subcommand};
use rohditor_engine::models::{ModelStatus, ModelStore};
use rohditor_neural::{Backend, NeuralDemosaicer};

use super::commands::write_stdout;

#[derive(Debug, Subcommand)]
pub(super) enum ModelCommand {
    /// Show where the model is installed and its sha256.
    Status,
    /// Install a local `model.onnx`, replacing the installed one.
    Install {
        /// For example `ml/runs/shippable/export/model.onnx`.
        file: PathBuf,
    },
}

pub(super) fn model(command: &ModelCommand) -> Result<()> {
    let store = store()?;
    match command {
        ModelCommand::Status => write_stdout(&match store.status() {
            ModelStatus::Installed { path, id } => {
                format!("installed: {} ({id})", path.display())
            }
            ModelStatus::Missing(path) => format!(
                "not installed: {} (run `rohditor-cli model install <FILE>`)",
                path.display()
            ),
            ModelStatus::Unreadable { reason, .. } => format!("unreadable: {reason}"),
        }),
        ModelCommand::Install { file } => {
            let (path, id) = store
                .install_from_file(file)
                .map_err(|error| anyhow!(error))?;
            write_stdout(&format!(
                "Installed the neural demosaic model to {} ({id})",
                path.display()
            ))
        }
    }
}

/// Model options for `--demosaic neural`.
#[derive(Debug, Clone, Default, Args)]
pub(super) struct NeuralArgs {
    /// Model for `--demosaic neural` instead of the installed one.
    #[arg(long = "neural-model", value_name = "FILE")]
    model: Option<PathBuf>,

    /// Run the neural demosaic on the CPU instead of the GPU.
    #[arg(long = "neural-cpu")]
    cpu: bool,
}

impl NeuralArgs {
    /// The `--neural-model` file if given, otherwise the installed model.
    pub(super) fn load(&self) -> Result<Arc<NeuralDemosaicer>> {
        let backend = if self.cpu { Backend::Cpu } else { Backend::Gpu };
        match &self.model {
            Some(path) => NeuralDemosaicer::load(path, backend)
                .map(Arc::new)
                .with_context(|| format!("could not load the neural model {}", path.display())),
            None => {
                let store = store()?;
                store.load(backend).with_context(|| {
                    format!(
                        "could not load the neural model {}; install one with `rohditor-cli model install <FILE>` or pass --neural-model",
                        store.path().display()
                    )
                })
            }
        }
    }
}

fn store() -> Result<ModelStore> {
    let directory = data_directory_from(
        std::env::var_os("XDG_DATA_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
    .context("could not determine the Rohditor data directory (set XDG_DATA_HOME or HOME)")?;
    Ok(ModelStore::new(&directory))
}

fn data_directory_from(xdg_data_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    xdg_data_home
        .filter(|directory| !directory.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|directory| !directory.is_empty())
                .map(|directory| PathBuf::from(directory).join(".local/share"))
        })
        .map(|base| base.join("rohditor"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_data_directory_precedes_the_home_fallback() {
        assert_eq!(
            data_directory_from(Some(OsStr::new("/xdg")), Some(OsStr::new("/home/me"))),
            Some(PathBuf::from("/xdg/rohditor"))
        );
        assert_eq!(
            data_directory_from(None, Some(OsStr::new("/home/me"))),
            Some(PathBuf::from("/home/me/.local/share/rohditor"))
        );
        assert_eq!(data_directory_from(None, None), None);
    }
}
