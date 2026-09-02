use std::collections::VecDeque;

use rohditor_edit::EditRecipe;

const HISTORY_LIMIT: usize = 100;

/// Identity attached to every asynchronous preview request and result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewTicket {
    pub document_id: u64,
    pub revision: u64,
}

impl PreviewTicket {
    pub(crate) const fn is_current(self, document_id: u64, revision: u64) -> bool {
        self.document_id == document_id && self.revision == revision
    }
}

#[derive(Debug, Clone)]
struct Gesture {
    before: EditRecipe,
    changed: bool,
}

/// Current recipe plus bounded, in-memory undo/redo history.
#[derive(Debug, Clone, Default)]
pub(crate) struct EditSession {
    recipe: EditRecipe,
    revision: u64,
    undo: VecDeque<EditRecipe>,
    redo: VecDeque<EditRecipe>,
    gesture: Option<Gesture>,
}

impl EditSession {
    pub(crate) const fn recipe(&self) -> &EditRecipe {
        &self.recipe
    }

    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) const fn gesture_active(&self) -> bool {
        self.gesture.is_some()
    }

    pub(crate) fn begin_gesture(&mut self) {
        if self.gesture.is_none() {
            self.gesture = Some(Gesture {
                before: self.recipe.clone(),
                changed: false,
            });
        }
    }

    /// Install an intermediate slider value. Every installed value receives a
    /// revision, while the whole drag becomes one undo command.
    pub(crate) fn set_during_gesture(&mut self, next: EditRecipe) -> bool {
        if next == self.recipe {
            return false;
        }
        if next.validate().is_err() {
            return false;
        }
        self.begin_gesture();
        if let Some(gesture) = self.gesture.as_mut()
            && !gesture.changed
        {
            self.redo.clear();
            gesture.changed = true;
        }
        self.recipe = next;
        self.advance_revision();
        true
    }

    pub(crate) fn finish_gesture(&mut self) {
        let Some(gesture) = self.gesture.take() else {
            return;
        };
        if gesture.changed && gesture.before != self.recipe {
            push_bounded(&mut self.undo, gesture.before);
        }
    }

    pub(crate) fn set_discrete(&mut self, next: EditRecipe) -> bool {
        self.finish_gesture();
        if next == self.recipe {
            return false;
        }
        if next.validate().is_err() {
            return false;
        }
        push_bounded(&mut self.undo, self.recipe.clone());
        self.redo.clear();
        self.recipe = next;
        self.advance_revision();
        true
    }

    pub(crate) fn reset(&mut self) -> bool {
        self.set_discrete(EditRecipe::default())
    }

    pub(crate) fn can_undo(&self) -> bool {
        !self.undo.is_empty() || self.gesture.is_some()
    }

    pub(crate) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub(crate) fn undo(&mut self) -> bool {
        self.finish_gesture();
        let Some(previous) = self.undo.pop_back() else {
            return false;
        };
        push_bounded(&mut self.redo, self.recipe.clone());
        self.recipe = previous;
        self.advance_revision();
        true
    }

    pub(crate) fn redo(&mut self) -> bool {
        self.finish_gesture();
        let Some(next) = self.redo.pop_back() else {
            return false;
        };
        push_bounded(&mut self.undo, self.recipe.clone());
        self.recipe = next;
        self.advance_revision();
        true
    }

    fn advance_revision(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }
}

fn push_bounded(stack: &mut VecDeque<EditRecipe>, recipe: EditRecipe) {
    if stack.len() == HISTORY_LIMIT {
        stack.pop_front();
    }
    stack.push_back(recipe);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exposed(exposure_ev: f32) -> EditRecipe {
        let mut recipe = EditRecipe::default();
        recipe.light.exposure_ev = exposure_ev;
        recipe
    }

    #[test]
    fn discrete_edits_reset_and_undo_redo_advance_revision() {
        let mut edits = EditSession::default();
        assert!(edits.set_discrete(exposed(1.0)));
        assert_eq!(edits.revision(), 1);
        assert!(edits.reset());
        assert_eq!(edits.revision(), 2);
        assert!(edits.undo());
        assert_eq!(edits.recipe().light.exposure_ev, 1.0);
        assert_eq!(edits.revision(), 3);
        assert!(edits.redo());
        assert_eq!(edits.recipe(), &EditRecipe::default());
        assert_eq!(edits.revision(), 4);
    }

    #[test]
    fn one_slider_gesture_has_many_revisions_but_one_undo_step() {
        let mut edits = EditSession::default();
        edits.begin_gesture();
        assert!(edits.set_during_gesture(exposed(0.25)));
        assert!(edits.set_during_gesture(exposed(0.5)));
        assert!(edits.set_during_gesture(exposed(0.75)));
        edits.finish_gesture();

        assert_eq!(edits.revision(), 3);
        assert_eq!(edits.recipe().light.exposure_ev, 0.75);
        assert!(edits.undo());
        assert_eq!(edits.recipe().light.exposure_ev, 0.0);
        assert!(!edits.undo());
    }

    #[test]
    fn a_new_edit_clears_redo_history() {
        let mut edits = EditSession::default();
        assert!(edits.set_discrete(exposed(1.0)));
        assert!(edits.undo());
        assert!(edits.can_redo());
        assert!(edits.set_discrete(exposed(-1.0)));
        assert!(!edits.can_redo());
    }

    #[test]
    fn invalid_recipes_never_enter_the_edit_session() {
        let mut edits = EditSession::default();
        let mut invalid = EditRecipe::default();
        invalid.light.exposure_ev = f32::NAN;

        assert!(!edits.set_discrete(invalid.clone()));
        assert_eq!(edits.revision(), 0);
        assert_eq!(edits.recipe(), &EditRecipe::default());

        edits.begin_gesture();
        assert!(!edits.set_during_gesture(invalid));
        edits.finish_gesture();
        assert_eq!(edits.revision(), 0);
        assert!(!edits.can_undo());
    }

    #[test]
    fn stale_or_foreign_preview_tickets_are_rejected() {
        let ticket = PreviewTicket {
            document_id: 7,
            revision: 3,
        };
        assert!(ticket.is_current(7, 3));
        assert!(!ticket.is_current(7, 4));
        assert!(!ticket.is_current(8, 3));
    }
}
