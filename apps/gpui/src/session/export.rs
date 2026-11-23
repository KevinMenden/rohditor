//! Full-resolution JPEG export of the open photo.

use std::path::PathBuf;

use gpui::Context;
use rohditor_core::paths_refer_to_same_file;
use rohditor_engine::export_options::ExportOptions;
use rohditor_engine::open_document::ExportActivity;
use rohditor_engine::preview::ProcessorPreference;

use super::Session;

const JPEG_QUALITY: u8 = 92;

pub fn options() -> ExportOptions {
    ExportOptions::with_jpeg_quality(JPEG_QUALITY)
}

impl Session {
    pub fn export_ready(&self) -> bool {
        self.document
            .as_ref()
            .is_some_and(|open| open.frame.is_some() && open.export_status.is_none())
    }

    /// Ask where to export to, then export in the background.
    pub fn prompt_export(&mut self, cx: &mut Context<Self>) {
        if !self.export_ready() {
            return;
        }
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let id = document.id;
        let directory = document
            .path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_default();
        let name = options().suggested_file_name(&document.path);
        let chosen = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn(async move |this, cx| {
            let chosen = chosen.await;
            // Gone only while the application quits.
            let _ = this.update(cx, |session, cx| {
                match chosen {
                    Ok(Ok(Some(path))) => session.export_to(id, path),
                    Ok(Err(error)) => session.fail_export(format!("{error:#}")),
                    Ok(Ok(None)) | Err(_) => {}
                }
                session.tick(cx);
            });
        })
        .detach();
    }

    fn fail_export(&mut self, message: String) {
        if let Some(document) = self.document.as_mut() {
            document.error = Some(message);
        }
    }

    fn export_to(&mut self, document_id: u64, mut destination: PathBuf) {
        // The photo may have changed while the dialog was open.
        if !self.export_ready() || self.document.as_ref().is_none_or(|d| d.id != document_id) {
            return;
        }
        if destination.extension().is_none() {
            destination.set_extension(options().extension());
        }
        let settings = options().core();
        if let Err(error) = settings.validate_destination(&destination) {
            return self.fail_export(error.to_string());
        }
        let Some(document) = self.document.as_mut() else {
            return;
        };
        match paths_refer_to_same_file(&document.path, &destination) {
            Ok(false) => {}
            Ok(true) => return self.fail_export("Refusing to replace the source RAW.".to_owned()),
            Err(error) => return self.fail_export(format!("Could not check destination: {error}")),
        }
        let Some(frame) = document.frame.clone() else {
            return;
        };
        let export_id = self.next_export_id;
        self.next_export_id += 1;
        let recipe_revision = document.edits.revision();
        document.export_status = Some(ExportActivity {
            id: export_id,
            recipe_revision,
            detail: "Exporting".to_owned(),
        });
        document.error = None;
        document.notice = None;
        if let Err(error) = self.coordinator.export(
            document_id,
            export_id,
            recipe_revision,
            destination,
            frame,
            document.edits.recipe().clone(),
            settings,
            self.settings.render_options(),
            self.runtime.preference() != ProcessorPreference::Cpu,
        ) {
            document.export_status = None;
            document.error = Some(error);
        }
    }
}
