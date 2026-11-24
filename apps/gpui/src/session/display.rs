//! What the viewport draws: the engine's GPU frame without a copy, or an uploaded CPU image.

use std::sync::Arc;

use gpui::RenderImage;
use gpui_wgpu::ExternalImage;
use rohditor_engine::preview::DocumentPreview;

use super::clipping::Clipping;
use super::upload::upload_rgb;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// The camera's own preview.
    Embedded,
    Cpu,
    /// The engine's texture, drawn without a copy.
    Gpu,
}

/// The displayed frame of the open photo.
pub struct Display {
    kind: FrameKind,
    image: ExternalImage,
    view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

impl Display {
    pub fn image(&self) -> &Arc<RenderImage> {
        self.image.image()
    }

    pub(super) const fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn is_developed(&self) -> bool {
        self.kind != FrameKind::Embedded
    }

    /// Whether the frame is drawn straight from the engine's texture.
    pub fn is_zero_copy(&self) -> bool {
        self.kind == FrameKind::Gpu
    }
}

/// Holds the displayed frame and follows the engine's.
#[derive(Default)]
pub struct DisplaySlot {
    display: Option<Display>,
    synced_generation: u64,
    pub(super) clipping: Clipping,
}

impl DisplaySlot {
    pub fn get(&self) -> Option<&Display> {
        self.display.as_ref()
    }

    /// Forget the frame of a closed photo, and everything else that uses the GPU.
    pub fn clear(&mut self) {
        self.display = None;
        self.synced_generation = 0;
        self.clipping.release_gpu();
    }

    /// Show packed 8-bit RGB pixels.
    pub fn show_cpu_image(&mut self, kind: FrameKind, width: usize, height: usize, rgb: &[u8]) {
        match upload_rgb(width, height, rgb) {
            Some(view) => self.show(kind, view, width as u32, height as u32),
            None => {
                log::error!("could not show a CPU image of {width} × {height}");
                self.hide();
            }
        }
    }

    /// Follow the engine's GPU frame. Cheap when nothing changed.
    pub fn sync(&mut self, preview: &DocumentPreview) {
        let generation = preview.display_generation();
        if self.synced_generation == generation {
            return;
        }
        self.synced_generation = generation;
        if let Some(frame) = preview.gpu_display() {
            let view = frame.view.clone();
            self.show(FrameKind::Gpu, view, frame.width, frame.height);
        } else if self.display.as_ref().is_some_and(Display::is_zero_copy) {
            self.hide();
        }
    }

    pub fn toggle_clipping(&mut self) {
        self.clipping.toggle(self.display.as_ref());
    }

    fn show(&mut self, kind: FrameKind, view: wgpu::TextureView, width: u32, height: u32) {
        let image = match self.display.take() {
            Some(Display { image, .. }) => {
                image.update(view.clone(), width, height);
                image
            }
            None => ExternalImage::new(view.clone(), width, height),
        };
        self.display = Some(Display {
            kind,
            image,
            view,
            width,
            height,
        });
        self.clipping.follow(self.display.as_ref());
    }

    fn hide(&mut self) {
        self.display = None;
        self.clipping.follow(None);
    }
}
