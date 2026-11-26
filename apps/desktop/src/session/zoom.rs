//! Fit and source-resolution 1:1: the two ways the open photo is rendered for the viewport.

use gpui::Context;

use super::Session;

impl Session {
    /// Show the whole photo, rendered at the fitted preview size.
    pub fn show_fit(&mut self, cx: &mut Context<Self>) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        if document.preview.request_fit() {
            self.queue_preview();
            self.tick(cx);
        } else {
            cx.notify();
        }
    }

    /// Show one photo pixel per device pixel. The photo is rendered again at source resolution,
    /// and the view jumps to 100% when that frame arrives.
    pub fn show_actual_size(&mut self, cx: &mut Context<Self>) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        if document.preview.request_actual_size() {
            self.queue_preview();
            self.tick(cx);
        } else {
            cx.notify();
        }
    }
}
