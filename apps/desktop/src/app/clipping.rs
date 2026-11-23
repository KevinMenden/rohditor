//! The viewport's highlight/shadow clipping overlay.
//!
//! The clipping rule and the per-pixel mask live in `rohditor_core`
//! (`ClippingMask`), next to the histogram that counts the same pixels. This
//! module only owns the toggle, keeps one tint texture per document up to
//! date, and hands it to the viewport to draw.
//!
//! The mask is derived from the final 8-bit display buffer:
//! - a CPU preview already has it in `preview_pixels`, so the mask follows
//!   the frame immediately;
//! - a GPU preview keeps its display texture on the GPU, so the mask is built
//!   from the debounced display readback that also feeds the histogram. It
//!   therefore trails a slider drag by that debounce. While the overlay is on
//!   the readback is also requested for 1:1 frames, which the histogram alone
//!   does not need.
//!
//! The overlay is a transient view aid: it is not persisted, never reaches
//! an export, and never touches the recipe.

use eframe::egui;
use rohditor_core::ClippingMask;

use super::{Document, RohditorApp};
use crate::ui::viewport::PreviewTexture;

/// Lightroom's convention: blown highlights red, crushed shadows blue.
const HIGHLIGHT_TINT: egui::Color32 = egui::Color32::from_rgba_premultiplied(190, 45, 45, 190);
const SHADOW_TINT: egui::Color32 = egui::Color32::from_rgba_premultiplied(45, 85, 190, 190);

/// Per-document overlay state.
#[derive(Default)]
pub(super) struct ClippingState {
    /// Tinted mask of the last analysed display frame.
    texture: Option<egui::TextureHandle>,
    /// The CPU display pixels changed since the mask was built.
    stale: bool,
}

impl ClippingState {
    /// The CPU preview pixels were replaced; rebuild the mask if it is shown.
    pub(super) fn mark_stale(&mut self) {
        self.stale = true;
    }

    /// The texture to draw over a preview of `preview_size` pixels.
    ///
    /// A mask of another size (a GPU frame whose readback has not arrived
    /// after a resolution or crop change) would be misaligned, so it is held
    /// back until a matching one exists.
    pub(super) fn texture_for(
        &self,
        preview: Option<&PreviewTexture>,
    ) -> Option<&egui::TextureHandle> {
        let texture = self.texture.as_ref()?;
        let (width, height) = preview?.dimensions();
        (texture.size() == [width, height]).then_some(texture)
    }

    fn install(&mut self, context: &egui::Context, document_id: u64, mask: &ClippingMask) {
        let image = tint_image(mask);
        match self.texture.as_mut() {
            Some(texture) => texture.set(image, tint_texture_options()),
            None => {
                self.texture = Some(context.load_texture(
                    format!("document-{document_id}-clipping"),
                    image,
                    tint_texture_options(),
                ));
            }
        }
        self.stale = false;
    }
}

/// Crisp pixels when zoomed in, smooth when the image is shown smaller.
fn tint_texture_options() -> egui::TextureOptions {
    egui::TextureOptions {
        magnification: egui::TextureFilter::Nearest,
        minification: egui::TextureFilter::Linear,
        ..egui::TextureOptions::LINEAR
    }
}

/// Colour the mask: clipped highlights win over clipped shadows when a pixel
/// is both (for example pure red).
fn tint_image(mask: &ClippingMask) -> egui::ColorImage {
    let pixels = mask
        .flags()
        .iter()
        .map(|flags| {
            if flags.highlight() {
                HIGHLIGHT_TINT
            } else if flags.shadow() {
                SHADOW_TINT
            } else {
                egui::Color32::TRANSPARENT
            }
        })
        .collect();
    egui::ColorImage {
        size: [mask.width(), mask.height()],
        source_size: egui::vec2(mask.width() as f32, mask.height() as f32),
        pixels,
    }
}

impl RohditorApp {
    /// `J` toggles the overlay, unless a text field has the keyboard.
    pub(super) fn handle_clipping_shortcut(&mut self, context: &egui::Context) {
        if self.document.is_none() || context.egui_wants_keyboard_input() {
            return;
        }
        let pressed =
            context.input(|input| input.modifiers.is_none() && input.key_pressed(egui::Key::J));
        if pressed {
            self.toggle_clipping_overlay();
        }
    }

    pub(super) fn toggle_clipping_overlay(&mut self) {
        self.show_clipping = !self.show_clipping;
        let Some(document) = self.document.as_mut() else {
            return;
        };
        if self.show_clipping {
            // Rebuild from the current frame: from the CPU pixels on the next
            // `refresh_clipping_overlay`, or from a fresh GPU readback.
            document.clipping.mark_stale();
            document.core.preview.request_histogram_now();
        } else {
            document.clipping = ClippingState::default();
        }
    }

    /// Bring the CPU-path mask up to date. Called once per frame before the
    /// viewport is drawn; a no-op while the overlay is off or current.
    pub(super) fn refresh_clipping_overlay(&mut self, context: &egui::Context) {
        if !self.show_clipping {
            return;
        }
        let Some(document) = self.document.as_mut() else {
            return;
        };
        if !document.clipping.stale || !matches!(document.texture, Some(PreviewTexture::Cpu(_))) {
            return;
        }
        // The embedded preview is the camera's JPEG, not the render being
        // judged, so it gets no overlay.
        if !document
            .core
            .preview
            .source
            .is_some_and(rohditor_engine::preview::PreviewSource::is_developed)
        {
            return;
        }
        let Some(pixels) = document.preview_pixels.as_ref() else {
            return;
        };
        let Some(mask) = ClippingMask::from_rgb8(pixels.width, pixels.height, &pixels.rgb) else {
            return;
        };
        let id = document.core.id;
        document.clipping.install(context, id, &mask);
    }
}

/// Install the mask of a GPU display readback, if the overlay is on.
pub(super) fn install_gpu_readback(
    context: &egui::Context,
    document: &mut Document,
    width: usize,
    height: usize,
    rgba: &[u8],
) {
    if let Some(mask) = ClippingMask::from_rgba8(width, height, rgba) {
        let id = document.core.id;
        document.clipping.install(context, id, &mask);
    }
}

#[cfg(test)]
mod tests {
    use rohditor_core::ClippingMask;

    use super::{HIGHLIGHT_TINT, SHADOW_TINT, tint_image};

    #[test]
    fn tint_marks_highlights_and_shadows_and_prefers_highlights() {
        // none, shadow only, highlight only, both ends at once.
        let rgb = [10, 20, 30, 0, 9, 9, 255, 9, 9, 255, 0, 9];
        let mask = ClippingMask::from_rgb8(4, 1, &rgb).expect("mask");
        let image = tint_image(&mask);
        assert_eq!(image.size, [4, 1]);
        assert_eq!(
            image.pixels,
            [
                eframe::egui::Color32::TRANSPARENT,
                SHADOW_TINT,
                HIGHLIGHT_TINT,
                HIGHLIGHT_TINT,
            ]
        );
    }
}
