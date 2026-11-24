//! Presets wiring: turns the Presets section's actions into engine calls and
//! an edit on the open photo.
//!
//! Storage and the apply math live in `rohditor_engine::presets`; one preset
//! is one undo step.

use eframe::egui;
use rohditor_engine::presets::Preset;

use super::RohditorApp;
use crate::ui::presets::PresetsAction;

impl RohditorApp {
    pub(super) fn handle_presets_action(&mut self, context: &egui::Context, action: PresetsAction) {
        match action {
            PresetsAction::Apply { name, amount } => self.apply_preset(context, &name, amount),
            PresetsAction::Delete(name) => {
                self.presets_ui.error = self
                    .presets
                    .delete(&name)
                    .err()
                    .map(|error| error.to_string());
            }
            PresetsAction::Save { name, sections } => self.save_preset(&name, &sections),
        }
    }

    fn apply_preset(&mut self, context: &egui::Context, name: &str, amount: f32) {
        let Some(preset) = self.presets.get(name) else {
            return;
        };
        let Some(document) = self.document.as_mut() else {
            return;
        };
        if document.core.apply_preset(preset, amount) {
            document.core.notice = Some(format!("Applied preset \"{}\"", preset.name()));
            let id = document.core.id;
            self.queue_preview(context, id);
        }
    }

    fn save_preset(&mut self, name: &str, sections: &[rohditor_edit::RecipeSection]) {
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let result = Preset::new(name, document.core.edits.recipe(), sections)
            .and_then(|preset| self.presets.save(preset));
        match result {
            Ok(()) => {
                self.presets_ui.error = None;
                self.presets_ui.close_save_form();
            }
            // The form stays open so the name can be fixed.
            Err(error) => self.presets_ui.error = Some(error.to_string()),
        }
    }
}
