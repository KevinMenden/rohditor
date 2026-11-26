//! Lens profiles and their corrections.

use gpui::Context;
use rohditor_edit::{LensCorrection, LensProfileSelection};
use rohditor_engine::open_document::OpenDocument;

use super::Session;

impl Session {
    pub fn set_lens_profile(&mut self, profile: LensProfileSelection, cx: &mut Context<Self>) {
        self.change_optics(|document| document.select_lens_profile(profile), cx);
    }

    pub fn set_lens_correction(
        &mut self,
        correction: LensCorrection,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        self.change_optics(|document| document.set_lens_correction(correction, on), cx);
    }

    fn change_optics(
        &mut self,
        change: impl FnOnce(&mut OpenDocument) -> bool,
        cx: &mut Context<Self>,
    ) {
        if self.document.as_mut().is_some_and(change) {
            self.queue_preview();
        }
        self.tick(cx);
    }
}
