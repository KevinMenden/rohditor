//! The red tint over the selected mask, as an image to draw.

use std::sync::Arc;

use gpui::RenderImage;
use gpui_wgpu::ExternalImage;
use rohditor_core::MaskAlphaImage;
use rohditor_engine::document::PreviewTicket;
use rohditor_engine::open_document::TintUpdate;
use rohditor_engine::tools::tint_pixels;

use super::Session;
use super::upload::upload_rgba;

#[derive(Default)]
pub(super) struct MaskTint(Option<ExternalImage>);

impl MaskTint {
    fn update(&mut self, update: TintUpdate) {
        match update {
            TintUpdate::Keep => {}
            TintUpdate::Clear => self.clear(),
            TintUpdate::Show(alpha) => self.show(&alpha),
        }
    }

    fn show(&mut self, alpha: &MaskAlphaImage) {
        let (width, height) = (alpha.width(), alpha.height());
        let Some(view) = upload_rgba(width, height, &tint_pixels(alpha.data())) else {
            log::error!("could not show a mask of {width} × {height}");
            return self.clear();
        };
        let (width, height) = (width as u32, height as u32);
        match &self.0 {
            Some(image) => image.update(view, width, height),
            None => self.0 = Some(ExternalImage::new(view, width, height)),
        }
    }

    pub fn clear(&mut self) {
        self.0 = None;
    }
}

impl Session {
    /// The tint to draw over the photo.
    pub fn mask_tint(&self) -> Option<&Arc<RenderImage>> {
        // Not while the photo is shown without its crop, which the tint has.
        self.mask_geometry()?;
        let shown = self.document()?.shows_mask_tint(self.masks()?);
        let image = self.mask_tint.0.as_ref().filter(|_| shown)?;
        Some(image.image())
    }

    /// Follow the recipe and the selected mask. Cheap when nothing changed.
    pub(super) fn refresh_mask_tint(&mut self) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let Some(masks) = self.tool.masks() else {
            self.mask_tint.clear();
            document.forget_mask_tint();
            return;
        };
        let options = self.renderer.preview_options();
        let update = document.refresh_mask_tint(&self.renderer.coordinator, masks, options);
        self.mask_tint.update(update);
    }

    /// The worker measured a mask, or failed to.
    pub(super) fn mask_measured(
        &mut self,
        ticket: PreviewTicket,
        layer: u32,
        overlay: Option<MaskAlphaImage>,
    ) {
        let policy = self.renderer.settings.render_options().raw_crop_policy;
        let Some(document) = self.current(ticket.document_id) else {
            return;
        };
        let update = match overlay {
            Some(overlay) => document.mask_overlay_ready(ticket, layer, overlay, policy),
            None => document.mask_overlay_failed(ticket),
        };
        self.mask_tint.update(update);
    }
}
