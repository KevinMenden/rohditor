//! Document open/save/close transitions and their user-facing decisions.

use std::path::PathBuf;

use eframe::egui;
use rohditor_edit::EditRecipe;

use super::{Document, RohditorApp};
use crate::persistence;

#[derive(Debug)]
pub(super) enum PendingDocumentAction {
    Close,
    Open(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnsavedChangesDecision {
    Save,
    Discard,
    Cancel,
}

impl RohditorApp {
    pub(super) fn open_path(&mut self, context: &egui::Context, path: PathBuf) {
        if self.document.as_ref().is_some_and(Document::is_dirty) {
            self.pending_document_action = Some(PendingDocumentAction::Open(path));
            return;
        }
        self.open_path_now(context, path);
    }

    fn open_path_now(&mut self, context: &egui::Context, path: PathBuf) {
        self.close_document_now(context);
        let (recipe, warning, initialize_white_balance_from_metadata) =
            match persistence::load_recipe(&path) {
                Ok(Some(recipe)) => (recipe, None, false),
                Ok(None) => (EditRecipe::default(), None, true),
                Err(error) => (
                    EditRecipe::default(),
                    Some(format!(
                        "Could not load saved edits for {}: {error}; using the default recipe.",
                        path.display()
                    )),
                    true,
                ),
            };
        let document_id = self.next_document_id;
        self.next_document_id = self.next_document_id.saturating_add(1);
        let mut document = Document::opening(document_id, path.clone(), recipe, warning);
        document.initialize_white_balance_from_metadata = initialize_white_balance_from_metadata;
        self.document = Some(document);
        if self.gpu_required_but_unavailable() {
            if let Some(document) = self.document.as_mut() {
                document.open_status = None;
                document.error = self.startup_error.clone();
            }
            self.update_window_title(context);
            return;
        }
        if let Err(error) = self.coordinator.open(document_id, path)
            && let Some(document) = self.document.as_mut()
        {
            document.open_status = None;
            document.error = Some(error);
        }
        self.update_window_title(context);
    }

    pub(super) fn request_close_document(&mut self, context: &egui::Context) {
        if self.document.as_ref().is_some_and(Document::is_dirty) {
            self.pending_document_action = Some(PendingDocumentAction::Close);
        } else {
            self.close_document_now(context);
        }
    }

    fn close_document_now(&mut self, context: &egui::Context) {
        self.picker_mode = None;
        self.color_mixer_channel = 0;
        self.pending_white_balance_pick = None;
        self.white_balance_memory = super::WhiteBalanceModeMemory::default();
        self.crop_tool = None;
        if let Some(mut document) = self.document.take() {
            self.release_gpu_preview(&mut document);
            self.coordinator.abandon(document.id);
        }
        self.update_window_title(context);
    }

    pub(super) fn save_document(&mut self) -> Result<(), String> {
        let Some((document_id, source, recipe)) = self.document.as_ref().and_then(|document| {
            document.is_dirty().then(|| {
                (
                    document.id,
                    document.path.clone(),
                    document.edits.recipe().clone(),
                )
            })
        }) else {
            return Ok(());
        };
        let path = persistence::save_recipe(&source, &recipe).map_err(|error| error.to_string());
        match path {
            Ok(path) => {
                if let Some(document) = self
                    .document
                    .as_mut()
                    .filter(|document| document.id == document_id)
                {
                    document.edits.mark_saved();
                    document.notice = Some(format!("Saved edits to {}.", path.display()));
                    document.error = None;
                }
                Ok(())
            }
            Err(error) => {
                if let Some(document) = self
                    .document
                    .as_mut()
                    .filter(|document| document.id == document_id)
                {
                    document.error = Some(format!("Could not save edits: {error}"));
                }
                Err(error)
            }
        }
    }

    fn finish_pending_document_action(&mut self, context: &egui::Context) {
        let Some(action) = self.pending_document_action.take() else {
            return;
        };
        match action {
            PendingDocumentAction::Close => self.close_document_now(context),
            PendingDocumentAction::Open(path) => self.open_path_now(context, path),
        }
    }

    pub(super) fn show_unsaved_changes_dialog(&mut self, context: &egui::Context) {
        if self.pending_document_action.is_none() {
            return;
        }
        let mut open = true;
        let mut decision = None;
        egui::Window::new("Unsaved edits")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(context, |ui| {
                ui.label("This photo has unsaved edits.");
                ui.label("Save them before continuing?");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        decision = Some(UnsavedChangesDecision::Save);
                    }
                    if ui.button("Discard").clicked() {
                        decision = Some(UnsavedChangesDecision::Discard);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(UnsavedChangesDecision::Cancel);
                    }
                });
            });
        if !open {
            self.pending_document_action = None;
            return;
        }
        match decision {
            Some(UnsavedChangesDecision::Save) => {
                if self.save_document().is_ok() {
                    self.finish_pending_document_action(context);
                }
            }
            Some(UnsavedChangesDecision::Discard) => {
                self.finish_pending_document_action(context);
            }
            Some(UnsavedChangesDecision::Cancel) | None => {}
        }
    }

    pub(super) fn update_window_title(&self, context: &egui::Context) {
        let title = self.document.as_ref().map_or_else(
            || "Rohditor".to_owned(),
            |document| {
                let marker = if document.is_dirty() { "* " } else { "" };
                format!("{marker}{} — Rohditor", document.file_name())
            },
        );
        context.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }
}
