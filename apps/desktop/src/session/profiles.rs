//! Camera profiles: which there are, and importing one.

use std::path::PathBuf;

use gpui::{Context, PathPromptOptions};
use rohditor_edit::CameraProfileSelection;

use super::Session;

impl Session {
    /// The profiles to offer for the open photo.
    pub fn camera_profiles(&self) -> Vec<rohditor_camera_profile::MatrixCameraProfile> {
        self.document.as_ref().map_or_else(Vec::new, |document| {
            let selected = &document.edits.recipe().color.camera_profile;
            self.profiles.choices(document.info.as_ref(), selected)
        })
    }

    pub fn set_camera_profile(&mut self, profile: CameraProfileSelection, cx: &mut Context<Self>) {
        if let Some(document) = self.document.as_mut()
            && document.select_camera_profile(profile)
        {
            self.queue_preview();
            self.tick(cx);
        }
    }

    /// Ask for a DCP file, install it and use it for the open photo.
    pub fn prompt_camera_profile(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.document.as_ref().map(|document| document.id) else {
            return;
        };
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import DCP camera profile".into()),
        });
        cx.spawn(async move |this, cx| {
            let chosen = chosen.await;
            // Gone only while the application quits.
            let _ = this.update(cx, |session, cx| match chosen {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        session.import_camera_profile(id, path, cx);
                    }
                }
                Ok(Err(error)) => session.fail(format!("The file dialog failed: {error:#}"), cx),
                Ok(Ok(None)) | Err(_) => {}
            });
        })
        .detach();
    }

    pub fn import_camera_profile(
        &mut self,
        document_id: u64,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        // The photo may have changed while the dialog was open.
        let Some(document) = self.document.as_mut().filter(|d| d.id == document_id) else {
            return;
        };
        let Some(info) = document.info.clone() else {
            return;
        };
        match self.profiles.import(&path, &info) {
            Ok(profile) => {
                document.error = None;
                document.notice = Some(format!("Imported {}", profile.name));
                if let Some(warning) = self.profiles.warning() {
                    document.warning = Some(warning.to_owned());
                }
                self.set_camera_profile(CameraProfileSelection::Matrix(profile), cx);
                cx.notify();
            }
            Err(error) => self.fail(error, cx),
        }
    }

    fn fail(&mut self, message: String, cx: &mut Context<Self>) {
        if let Some(document) = self.document.as_mut() {
            document.error = Some(message);
            cx.notify();
        }
    }
}
