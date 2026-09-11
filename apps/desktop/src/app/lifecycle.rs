//! Document open/save/close transitions and background recipe persistence.

use std::path::PathBuf;

use eframe::egui;
use rohditor_edit::EditRecipe;

use super::{Document, RohditorApp};
use crate::persistence::{self, SaveJob, SaveResult};

impl RohditorApp {
    pub(super) fn update_autosave(&mut self, context: &egui::Context) {
        let now = std::time::Instant::now();
        let Some(document) = self.document.as_mut() else {
            return;
        };
        document.autosave.observe(
            document.edits.revision(),
            document.edits.gesture_active(),
            now,
        );
        let due = document.autosave.take_due(now);
        if let Some(delay) = document.autosave.delay(now) {
            context.request_repaint_after(delay);
        }
        if due {
            let _ = self.schedule_recipe_save(true);
        }
    }

    pub(super) fn open_path(&mut self, context: &egui::Context, path: PathBuf) {
        let _ = self.schedule_recipe_save(true);
        self.open_path_now(context, path);
    }

    fn open_path_now(&mut self, context: &egui::Context, path: PathBuf) {
        self.close_document_now(context);
        let (recipe, warning, initialize_white_balance_from_metadata) = match self
            .queued_recipe_snapshots
            .get(&path)
            .map(|(_, recipe)| recipe.clone())
            .map(|recipe| Ok(Some(recipe)))
            .unwrap_or_else(|| persistence::load_recipe(&path))
        {
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
        let _ = self.schedule_recipe_save(true);
        self.close_document_now(context);
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

    /// Request an immediate background write for the current recipe.
    pub(super) fn save_document(&mut self) -> Result<(), String> {
        self.schedule_recipe_save(true)
    }

    /// Called after the UI debounce, or immediately for navigation and Save.
    pub(super) fn schedule_recipe_save(&mut self, immediate: bool) -> Result<(), String> {
        let Some(document) = self.document.as_ref().filter(|document| {
            document.is_dirty()
                || self
                    .queued_recipe_snapshots
                    .get(&document.path)
                    .is_some_and(|(_, recipe)| recipe != document.edits.recipe())
        }) else {
            return Ok(());
        };
        let document_id = document.id;
        let revision = document.edits.revision();
        let source = document.path.clone();
        let recipe = document.edits.recipe().clone();
        let job = SaveJob {
            document_id,
            revision,
            source: source.clone(),
            recipe: recipe.clone(),
        };
        self.recipe_save_status = Some(if immediate {
            "Saving edits…".to_owned()
        } else {
            "Edits pending save…".to_owned()
        });
        self.recipe_save_error = None;
        if let Err(error) = self.recipe_saves.enqueue(job, immediate) {
            self.recipe_save_error = Some(error.clone());
            if let Some(document) = self.document.as_mut() {
                document.error = Some(format!("Could not queue edits for saving: {error}"));
            }
            return Err(error);
        }
        self.queued_recipe_snapshots
            .insert(source, (revision, recipe));
        Ok(())
    }

    pub(super) fn process_recipe_save_events(&mut self) {
        let results = self.recipe_saves.try_results().collect::<Vec<_>>();
        for result in results {
            self.process_recipe_save_result(result);
        }
    }

    fn process_recipe_save_result(&mut self, result: SaveResult) {
        let SaveResult {
            document_id,
            revision,
            source,
            recipe,
            result,
        } = result;
        match result {
            Ok(path) => {
                if self.queued_recipe_snapshots.get(&source).is_some_and(
                    |(queued_revision, queued_recipe)| {
                        *queued_revision == revision && *queued_recipe == recipe
                    },
                ) {
                    self.queued_recipe_snapshots.remove(&source);
                }
                let newer_pending = self.queued_recipe_snapshots.contains_key(&source);
                let same_document = self
                    .document
                    .as_ref()
                    .is_some_and(|document| document.id == document_id);
                let current = self
                    .document
                    .as_mut()
                    .filter(|document| document.id == document_id)
                    .is_some_and(|document| {
                        document.edits.mark_saved_if_current(revision, &recipe)
                    });
                self.recipe_save_status = Some(if !newer_pending && (!same_document || current) {
                    format!("Saved edits to {}.", path.display())
                } else {
                    "Edits pending save…".to_owned()
                });
                if current
                    && let Some(document) = self
                        .document
                        .as_mut()
                        .filter(|document| document.id == document_id)
                {
                    document.error = None;
                }
                self.recipe_save_error = None;
            }
            Err(error) => {
                let newer_pending = self.queued_recipe_snapshots.contains_key(&source);
                self.recipe_save_status = Some(if newer_pending {
                    "Edits pending save…".to_owned()
                } else {
                    "Could not save edits".to_owned()
                });
                self.recipe_save_error = Some(error.clone());
                if let Some(document) = self
                    .document
                    .as_mut()
                    .filter(|document| document.id == document_id)
                {
                    document.error = Some(format!("Could not save edits: {error}"));
                }
            }
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

impl Drop for RohditorApp {
    fn drop(&mut self) {
        // The worker drains all queued snapshots before joining, so closing
        // the native window cannot discard the current recipe.
        let _ = self.schedule_recipe_save(true);
        self.recipe_saves.shutdown();
    }
}
