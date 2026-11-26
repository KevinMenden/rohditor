//! The clipping overlay: a tint over the displayed frame's clipped pixels.
//!
//! A view aid only: it is not saved and never reaches the recipe or an export. The setting stays
//! for the next photo.

use std::sync::Arc;

use gpui::{Context, RenderImage};
use gpui_wgpu::ExternalImage;
use rohditor_gpu::{ClippingTints, GpuClippingOverlay};

use super::Session;
use super::display::Display;
use crate::device;

/// Blown highlights red, crushed shadows blue.
const TINTS: ClippingTints = ClippingTints {
    highlight: [1.0, 0.24, 0.24, 0.75],
    shadow: [0.24, 0.45, 1.0, 0.75],
};

#[derive(Default)]
pub struct Clipping {
    shown: bool,
    overlay: Option<GpuClippingOverlay>,
    image: Option<ExternalImage>,
}

impl Clipping {
    pub fn image(&self) -> Option<&Arc<RenderImage>> {
        self.image.as_ref().map(ExternalImage::image)
    }

    /// Follow a new frame, or the disappearance of the last one.
    pub fn follow(&mut self, display: Option<&Display>) {
        // The embedded preview is the camera's picture, not the one being judged.
        let Some(display) = display.filter(|display| self.shown && display.is_developed()) else {
            self.image = None;
            return;
        };
        if self.overlay.is_none() {
            self.overlay = device::shared_context()
                .map(|gpu| GpuClippingOverlay::new(&gpu.device, &gpu.queue, TINTS));
        }
        let Some(overlay) = self.overlay.as_mut() else {
            return;
        };
        let (width, height) = (display.width, display.height);
        let view = overlay.render(display.view(), width, height);
        match &self.image {
            Some(image) => image.update(view, width, height),
            None => self.image = Some(ExternalImage::new(view, width, height)),
        }
    }

    pub fn toggle(&mut self, display: Option<&Display>) {
        self.shown = !self.shown;
        if !self.shown {
            self.release_gpu();
        }
        self.follow(display);
    }

    /// Let go of the GPU.
    pub fn release_gpu(&mut self) {
        self.overlay = None;
        self.image = None;
    }
}

impl Session {
    pub fn clipping_shown(&self) -> bool {
        self.display.clipping.shown
    }

    /// The tint to draw over the displayed frame.
    pub fn clipping_image(&self) -> Option<&Arc<RenderImage>> {
        self.display.clipping.image()
    }

    pub fn toggle_clipping(&mut self, cx: &mut Context<Self>) {
        self.display.toggle_clipping();
        cx.notify();
    }
}
