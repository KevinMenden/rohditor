//! Installing the neural demosaic model from the settings dialog.
//!
//! The model goes to the data directory shared with the CLI. The render worker took its model at
//! startup, so a newly installed one is used after a restart.

use std::path::PathBuf;

use gpui::{AppContext as _, Context, PathPromptOptions};
use rohditor_engine::models::ModelStore;

use super::Session;
use super::settings::ModelInstall;

impl Session {
    /// Ask for an ONNX file and install it as the neural model.
    pub fn prompt_neural_model(&mut self, cx: &mut Context<Self>) {
        if self.model_install() == Some(ModelInstall::Installing) {
            return;
        }
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Install neural model".into()),
        });
        cx.spawn(async move |this, cx| {
            let chosen = chosen.await;
            // Gone only while the application quits.
            let _ = this.update(cx, |session, cx| match chosen {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        session.install_neural_model(path, cx);
                    }
                }
                Ok(Err(error)) => {
                    session.model_install_failed(format!("The file dialog failed: {error:#}"), cx);
                }
                Ok(Ok(None)) | Err(_) => {}
            });
        })
        .detach();
    }

    /// Install `path` as the neural model in the background: checking that it loads into ONNX
    /// Runtime and hashing it can take a moment.
    pub fn install_neural_model(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(data) = self.settings_file.data.clone() else {
            self.model_install_failed(
                "The model cannot be installed: the Rohditor data directory is unknown.".into(),
                cx,
            );
            return;
        };
        // A failed install leaves the installed file, and so this state, as it was.
        let previous = self
            .settings_file
            .model_install
            .replace(ModelInstall::Installing);
        self.settings_file.warning = None;
        cx.notify();
        let installed =
            cx.background_spawn(async move { ModelStore::new(&data).install_from_file(&path) });
        cx.spawn(async move |this, cx| {
            let installed = installed.await;
            let _ = this.update(cx, |session, cx| match installed {
                Ok((_, id)) => {
                    session.settings_file.model_install = Some(ModelInstall::Installed(id));
                    cx.notify();
                }
                Err(error) => {
                    session.settings_file.model_install = previous;
                    session.model_install_failed(error, cx);
                }
            });
        })
        .detach();
    }

    fn model_install_failed(&mut self, message: String, cx: &mut Context<Self>) {
        log::warn!("the neural model was not installed: {message}");
        self.settings_file.warning = Some(message);
        cx.notify();
    }
}
