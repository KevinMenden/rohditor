//! The pickers: the next click on the photo samples it.

use gpui::Context;
use rohditor_engine::tools::Picker;

use super::{MixerColorPicked, Session};

impl Session {
    pub fn picker(&self) -> Option<Picker> {
        self.tool.picker()
    }

    /// Start this picker, or stop it.
    pub fn toggle_picker(&mut self, picker: Picker, cx: &mut Context<Self>) {
        let ready = self.document.as_ref().is_some_and(|d| d.frame.is_some());
        let next = (ready && self.picker() != Some(picker)).then_some(picker);
        self.tool.set_picker(next);
        cx.notify();
    }

    pub fn stop_picking(&mut self, cx: &mut Context<Self>) {
        if self.picker().is_some() {
            self.tool.set_picker(None);
            cx.notify();
        }
    }

    /// Sample at `position`, 0 to 1 over the photo.
    pub fn pick(&mut self, position: (f64, f64), cx: &mut Context<Self>) {
        let Some(picker) = self.picker() else {
            return;
        };
        self.tool.set_picker(None);
        if let Some(document) = self.document.as_mut() {
            let options = self.renderer.preview_options();
            document.pick(&self.renderer.coordinator, picker, position, options);
        }
        self.tick(cx);
    }

    /// Announce a picked colour that arrived from the worker.
    pub(super) fn announce_pick(&mut self, cx: &mut Context<Self>) {
        if let Some(channel) = self.picked_channel.take() {
            cx.emit(MixerColorPicked(channel));
        }
    }
}
