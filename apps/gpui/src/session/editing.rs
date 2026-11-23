//! Changes to the open photo's recipe.

use gpui::Context;
use rohditor_core::auto_tone;
use rohditor_edit::EditRecipe;
use rohditor_engine::document::EditSession;
use rohditor_engine::open_document::OpenDocument;

use super::Session;

/// Whether an edit comes from a drag still in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditPhase {
    Dragging,
    Finished,
}

impl Session {
    pub fn begin_gesture(&mut self) {
        if let Some(document) = self.document.as_mut() {
            document.edits.begin_gesture();
        }
    }

    /// Replace the recipe as part of a gesture, or as one step of its own. `white_balance` edits
    /// are drafted on the resident GPU frame while dragging and rendered exactly at the end.
    pub fn edit(
        &mut self,
        change: impl FnOnce(&mut EditRecipe),
        phase: EditPhase,
        white_balance: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let mut next = document.edits.recipe().clone();
        change(&mut next);
        let changed = match phase {
            EditPhase::Dragging => document.edits.set_during_gesture(next),
            EditPhase::Finished => document.edits.set_discrete(next),
        };
        if !changed {
            return;
        }
        let drafted = white_balance
            && phase == EditPhase::Dragging
            && self.renderer.run(
                document,
                &mut self.display,
                |runtime, coordinator, preview, request| {
                    runtime.render_white_balance_draft(coordinator, preview, request)
                },
            );
        if !drafted {
            self.queue_preview();
        }
        self.tick(cx);
    }

    pub fn finish_gesture(&mut self, white_balance: bool, cx: &mut Context<Self>) {
        if let Some(document) = self.document.as_mut() {
            document.edits.finish_gesture();
            if white_balance {
                self.queue_preview();
            }
        }
        self.tick(cx);
    }

    fn change_history(
        &mut self,
        change: impl FnOnce(&mut EditSession) -> bool,
        cx: &mut Context<Self>,
    ) {
        if self.document.as_mut().is_some_and(|d| change(&mut d.edits)) {
            self.queue_preview();
            self.tick(cx);
        }
    }

    pub fn undo(&mut self, cx: &mut Context<Self>) {
        self.change_history(EditSession::undo, cx);
    }

    pub fn redo(&mut self, cx: &mut Context<Self>) {
        self.change_history(EditSession::redo, cx);
    }

    pub fn reset(&mut self, cx: &mut Context<Self>) {
        let Some(defaults) = self.document.as_ref().map(OpenDocument::default_recipe) else {
            return;
        };
        self.change_history(|edits| edits.reset_to(defaults), cx);
    }

    /// Auto Tone needs the histogram of exactly the recipe it starts from.
    pub fn auto_tone(&mut self, cx: &mut Context<Self>) {
        let next = self.document.as_ref().and_then(|document| {
            let preview = &document.preview;
            if preview.histogram_revision != Some(document.edits.revision()) {
                return None;
            }
            auto_tone(document.edits.recipe(), preview.histogram.as_ref()?)
        });
        if let Some(next) = next {
            self.change_history(|edits| edits.set_discrete(next), cx);
        }
    }
}
