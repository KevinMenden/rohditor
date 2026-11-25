//! The Masks tool: what the panel and the pointer on the photo change.

use gpui::Context;
use rohditor_core::OutputGeometry;
use rohditor_engine::document::EditSession;
use rohditor_engine::tools::{MaskAction, MaskPointerEvents, MaskToolSession, MaskView, ToolKind};

use super::Session;

impl Session {
    /// The Masks tool, if it is active.
    pub const fn masks(&self) -> Option<&MaskToolSession> {
        self.tool.masks()
    }

    /// Carry out a request of the panel or a command.
    pub fn edit_masks(&mut self, action: MaskAction, cx: &mut Context<Self>) {
        self.change_masks(|masks, edits| masks.apply_action(edits, action), cx);
    }

    /// Forward what the pointer did on the photo, which is shown as `view`.
    pub fn drive_masks(
        &mut self,
        events: &MaskPointerEvents,
        view: &MaskView,
        cx: &mut Context<Self>,
    ) {
        self.change_masks(|masks, edits| masks.drive_pointer(edits, events, view), cx);
    }

    /// The pointer was released, as a click on the pin of `clicked` if there is one.
    pub fn release_masks(&mut self, clicked: Option<u32>, cx: &mut Context<Self>) {
        self.change_masks(
            |masks, edits| {
                let changed = masks.finish_pointer(edits);
                if let Some(mask) = clicked {
                    masks.select_layer(edits.recipe(), mask);
                }
                changed
            },
            cx,
        );
    }

    /// Stop drawing a new shape. Returns whether one was being drawn.
    pub fn cancel_mask_creation(&mut self, cx: &mut Context<Self>) -> bool {
        let cancelled = self
            .tool
            .masks_mut()
            .is_some_and(MaskToolSession::cancel_create);
        if cancelled {
            cx.notify();
        }
        cancelled
    }

    /// How the photo is shown to the Masks tool; `None` while it is not shown that way.
    pub fn mask_geometry(&self) -> Option<OutputGeometry> {
        let document = self.document.as_ref()?;
        // Coming from the Crop tool, the photo is shown without its crop for a moment.
        if self.tool.kind() != ToolKind::Masks || document.preview.full_frame_shown() {
            return None;
        }
        let policy = self.renderer.settings.render_options().raw_crop_policy;
        let info = &document.frame.as_ref()?.info;
        OutputGeometry::of_photo(info, policy, &document.edits.recipe().geometry).ok()
    }

    fn change_masks(
        &mut self,
        change: impl FnOnce(&mut MaskToolSession, &mut EditSession) -> bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(document) = self.document.as_mut()
            && let Some(masks) = self.tool.masks_mut()
            && change(masks, &mut document.edits)
        {
            document.notice = None;
            self.queue_preview();
        }
        self.tick(cx);
    }
}
