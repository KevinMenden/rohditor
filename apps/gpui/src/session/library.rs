//! The open folder and its thumbnails.

use super::{AtlasImage, Session};

impl Session {
    pub(super) fn process_catalog_events(&mut self) {
        for event in self.catalog_coordinator.try_events() {
            self.catalog.apply_event(event);
        }
        // Thumbnails wait while the open photo needs the CPU.
        let busy = self.document.as_ref().is_some_and(|document| {
            document.open_status.is_some()
                || document.preview.status.is_some()
                || document.export_status.is_some()
        });
        self.catalog_coordinator.set_paused(busy);
        self.library.begin_frame();
        self.library.sync(&self.catalog);
        let visible = self.library.visible;
        let retired = &self.retired;
        self.library
            .fill_textures(&self.catalog, visible, |_, thumbnail| {
                AtlasImage::from_rgb(thumbnail.width, thumbnail.height, &thumbnail.rgb, retired)
                    .expect("a decoded thumbnail's pixels match its dimensions")
            });
    }

    /// The list shows these rows now; have their thumbnails ready.
    pub fn set_visible_files(&mut self, start: usize, end: usize, cx: &mut gpui::Context<Self>) {
        if self.library.visible != (start, end) {
            self.library.visible = (start, end);
            self.tick(cx);
        }
    }
}
