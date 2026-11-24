//! Changing tools, and what the tools do with the edit history.
//!
//! Every tool has an arm in `enter` and one in [`ActiveTool::leave`]; nothing else here knows
//! the tools. The methods return whether the caller must queue a preview.

use rohditor_core::{OutputGeometry, RawCropPolicy};
use rohditor_edit::EditRecipe;

use super::{ActiveTool, CropToolSession, MaskToolSession, ToolKind};
use crate::document::EditSession;
use crate::open_document::OpenDocument;

impl ActiveTool {
    /// Change to `target`, applying what the current tool drafted. Nothing happens if the photo
    /// is not ready for it. `now` is the frontend clock of the view's feedback.
    #[must_use = "the caller must queue a preview"]
    pub fn switch_to(
        &mut self,
        target: ToolKind,
        document: &mut OpenDocument,
        policy: RawCropPolicy,
        now: f64,
    ) -> bool {
        if target == self.kind() {
            return false;
        }
        let Some(next) = Self::enter(target, document, policy) else {
            return false;
        };
        // The panel that a slider is dragged in goes away.
        document.edits.finish_gesture();
        document.cancel_pick();
        let left = self.close(document, true, now);
        *self = next;
        // The crop that was just applied is the one to start from.
        self.sync(document.edits.recipe());
        let full_frame = target == ToolKind::Crop;
        if full_frame {
            document.preview.request_full_frame(true, now);
        }
        left || full_frame
    }

    /// Return to Edit, applying what the tool drafted.
    #[must_use = "the caller must queue a preview"]
    pub fn apply(&mut self, document: &mut OpenDocument, now: f64) -> bool {
        self.close(document, true, now)
    }

    /// Return to Edit, discarding what the tool drafted.
    #[must_use = "the caller must queue a preview"]
    pub fn cancel(&mut self, document: &mut OpenDocument, now: f64) -> bool {
        self.close(document, false, now)
    }

    /// The tool `target` at its start, if the photo is ready for it.
    fn enter(target: ToolKind, document: &OpenDocument, policy: RawCropPolicy) -> Option<Self> {
        let recipe = document.edits.recipe();
        let mut tool = Self::default();
        match target {
            ToolKind::Edit => {}
            ToolKind::Crop => {
                let info = &document.frame.as_ref()?.info;
                let photo = OutputGeometry::of_photo(info, policy, &recipe.geometry).ok()?;
                let (width, height) = photo.full_dimensions();
                tool.enter_crop(CropToolSession::new(recipe.geometry.crop, width, height));
            }
            ToolKind::Masks => {
                let mut session = MaskToolSession::default();
                // So that there is something to show.
                if let Some(first) = recipe.local.layers.first() {
                    session.select_layer(recipe, first.id);
                }
                tool.enter_masks(session);
            }
        }
        Some(tool)
    }

    fn close(&mut self, document: &mut OpenDocument, commit: bool, now: f64) -> bool {
        let full_frame = self.crop().is_some();
        if let Some(next) = std::mem::take(self).leave(document.edits.recipe(), commit)
            && document.edits.set_discrete(next)
        {
            document.notice = None;
        }
        if full_frame {
            document.preview.request_full_frame(false, now);
        }
        full_frame
    }

    /// Whether the tool drafted something that is not in the recipe.
    pub fn is_modified(&self) -> bool {
        self.crop().is_some_and(CropToolSession::is_modified)
    }

    /// Undo. What the tool drafted is the latest change, so it goes first.
    #[must_use = "the caller must queue a preview"]
    pub fn undo(&mut self, edits: &mut EditSession) -> bool {
        if self.discard_changes() {
            return false;
        }
        let changed = edits.undo();
        self.sync(edits.recipe());
        changed
    }

    /// Redo. A draft is newer than what could be redone, so there is nothing to redo then.
    #[must_use = "the caller must queue a preview"]
    pub fn redo(&mut self, edits: &mut EditSession) -> bool {
        if self.is_modified() {
            return false;
        }
        let changed = edits.redo();
        self.sync(edits.recipe());
        changed
    }

    /// Reset all edits, drafted ones included.
    #[must_use = "the caller must queue a preview"]
    pub fn reset(&mut self, edits: &mut EditSession, defaults: EditRecipe) -> bool {
        self.discard_changes();
        let changed = edits.reset_to(defaults);
        self.sync(edits.recipe());
        changed
    }

    /// Follow a recipe that something else than the tool changed.
    pub fn sync(&mut self, recipe: &EditRecipe) {
        if let Some(session) = self.crop_mut() {
            session.sync(recipe.geometry.crop);
        }
    }

    fn discard_changes(&mut self) -> bool {
        self.crop_mut()
            .is_some_and(CropToolSession::discard_changes)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rohditor_core::default_recipe;

    use super::super::{CropAspect, Picker};
    use super::*;

    fn document() -> OpenDocument {
        OpenDocument::opening(1, PathBuf::from("a.arw"), default_recipe(None), None)
    }

    /// Crop as it is open after `switch_to`, which needs a RAW frame.
    fn cropping(document: &mut OpenDocument) -> ActiveTool {
        let mut tool = ActiveTool::default();
        let crop = document.edits.recipe().geometry.crop;
        tool.enter_crop(CropToolSession::new(crop, 6000, 4000));
        document.preview.request_full_frame(true, 0.0);
        tool
    }

    fn draft(tool: &mut ActiveTool, aspect: CropAspect) {
        tool.crop_mut().expect("Crop is open").set_aspect(aspect);
    }

    fn exposed(document: &mut OpenDocument, ev: f32) {
        let mut next = document.edits.recipe().clone();
        next.light.exposure_ev = ev;
        assert!(document.edits.set_discrete(next));
    }

    #[test]
    fn crop_waits_for_the_photo_and_keeps_the_tool_until_then() {
        let mut document = document();
        let mut tool = ActiveTool::Edit {
            picker: Some(Picker::WhiteBalance),
        };
        let policy = RawCropPolicy::default();
        assert!(!tool.switch_to(ToolKind::Crop, &mut document, policy, 0.0));
        assert_eq!(tool.picker(), Some(Picker::WhiteBalance));
    }

    #[test]
    fn changing_tools_applies_the_crop_as_one_undo_step() {
        let mut document = document();
        let mut tool = cropping(&mut document);
        draft(&mut tool, CropAspect::Square);
        draft(&mut tool, CropAspect::FourThree);
        let policy = RawCropPolicy::default();
        assert!(tool.switch_to(ToolKind::Masks, &mut document, policy, 0.0));
        assert_eq!(tool.kind(), ToolKind::Masks);
        assert!(document.edits.recipe().geometry.crop.is_some());
        assert!(tool.undo(&mut document.edits));
        assert_eq!(document.edits.recipe().geometry.crop, None);
        assert!(!document.edits.can_undo());
    }

    #[test]
    fn cancelling_discards_the_crop_and_shows_the_cropped_photo_again() {
        let mut document = document();
        let mut tool = cropping(&mut document);
        draft(&mut tool, CropAspect::Square);
        assert!(tool.cancel(&mut document, 0.0));
        assert_eq!(tool.kind(), ToolKind::Edit);
        assert!(!document.edits.can_undo());
        assert!(document.preview.request_actual_size(0.0), "1:1 works again");
    }

    #[test]
    fn undo_takes_the_draft_back_before_earlier_edits() {
        let mut document = document();
        exposed(&mut document, 1.0);
        let mut tool = cropping(&mut document);
        draft(&mut tool, CropAspect::Square);
        assert!(!tool.undo(&mut document.edits));
        assert!(!tool.is_modified());
        assert_eq!(document.edits.recipe().light.exposure_ev, 1.0);
        assert!(tool.undo(&mut document.edits));
        assert_eq!(document.edits.recipe().light.exposure_ev, 0.0);
        assert_eq!(tool.kind(), ToolKind::Crop);

        draft(&mut tool, CropAspect::Square);
        assert!(!tool.redo(&mut document.edits), "the draft is newer");
        assert!(tool.is_modified());
    }

    #[test]
    fn an_edit_made_meanwhile_stays_and_a_reset_takes_the_draft_too() {
        let mut document = document();
        let mut tool = cropping(&mut document);
        draft(&mut tool, CropAspect::Square);
        exposed(&mut document, 1.0);
        tool.sync(document.edits.recipe());
        assert!(tool.is_modified());
        assert!(tool.apply(&mut document, 0.0));
        assert_eq!(document.edits.recipe().light.exposure_ev, 1.0);

        let mut tool = cropping(&mut document);
        draft(&mut tool, CropAspect::SixteenNine);
        assert!(tool.reset(&mut document.edits, default_recipe(None)));
        let session = tool.crop().expect("still cropping");
        assert_eq!(session.committed_crop(), None);
        assert!(!tool.is_modified());
    }
}
