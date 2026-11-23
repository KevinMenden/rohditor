//! What the viewport draws: a CPU image, or the engine's GPU frame without a copy.

use std::sync::Arc;

use gpui::{App, RenderImage};
use gpui_wgpu::ExternalImage;
use rohditor_engine::preview::DocumentPreview;

use crate::atlas_image::{AtlasImage, RetiredImages};

enum Source {
    Cpu(AtlasImage),
    Gpu(ExternalImage),
}

/// The displayed frame of the open photo.
pub struct Display {
    source: Source,
    pub width: u32,
    pub height: u32,
}

impl Display {
    pub fn image(&self) -> &Arc<RenderImage> {
        match &self.source {
            Source::Cpu(image) => image.image(),
            Source::Gpu(image) => image.image(),
        }
    }

    /// Whether the frame is drawn straight from the engine's texture.
    pub const fn is_zero_copy(&self) -> bool {
        matches!(self.source, Source::Gpu(_))
    }
}

/// Holds the displayed frame and follows the engine's.
#[derive(Default)]
pub struct DisplaySlot {
    display: Option<Display>,
    synced_generation: u64,
    retired: RetiredImages,
}

impl DisplaySlot {
    pub fn get(&self) -> Option<&Display> {
        self.display.as_ref()
    }

    /// Forget the frame of a closed photo.
    pub fn clear(&mut self) {
        self.display = None;
        self.synced_generation = 0;
    }

    pub fn show_cpu_image(&mut self, width: usize, height: usize, rgb: &[u8]) {
        self.display =
            AtlasImage::from_rgb(width, height, rgb, &self.retired).map(|image| Display {
                width: width as u32,
                height: height as u32,
                source: Source::Cpu(image),
            });
    }

    /// Follow the engine's GPU frame. Cheap when nothing changed.
    pub fn sync(&mut self, preview: &DocumentPreview) {
        let generation = preview.display_generation();
        if self.synced_generation == generation {
            return;
        }
        self.synced_generation = generation;
        let Some(frame) = preview.gpu_display() else {
            if self.display.as_ref().is_some_and(Display::is_zero_copy) {
                self.display = None;
            }
            return;
        };
        let view = frame.view.clone();
        if let Some(Display {
            source: Source::Gpu(image),
            width,
            height,
        }) = self.display.as_mut()
        {
            image.update(view, frame.width, frame.height);
            (*width, *height) = (frame.width, frame.height);
        } else {
            self.display = Some(Display {
                source: Source::Gpu(ExternalImage::new(view, frame.width, frame.height)),
                width: frame.width,
                height: frame.height,
            });
        }
    }

    pub fn remove_retired(&self, cx: &mut App) {
        self.retired.remove_from_atlas(cx);
    }
}
