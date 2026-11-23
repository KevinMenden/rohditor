//! What the viewport draws: a CPU image, or the engine's GPU frame without a copy.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, RenderImage};
use gpui_wgpu::ExternalImage;

use super::Session;

/// Images no longer shown, waiting to be removed from GPUI's sprite atlas.
pub(super) type RetiredImages = Rc<RefCell<Vec<Arc<RenderImage>>>>;

/// A CPU image that leaves the sprite atlas when it is dropped. GPUI keeps images it has painted
/// until told otherwise.
pub struct AtlasImage {
    image: Arc<RenderImage>,
    retired: RetiredImages,
}

impl AtlasImage {
    pub(super) fn from_rgb(
        width: usize,
        height: usize,
        rgb: &[u8],
        retired: &RetiredImages,
    ) -> Option<Self> {
        // GPUI images are BGRA.
        let bgra = rgb
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [b, g, r, 255])
            .collect();
        let buffer = image::RgbaImage::from_raw(
            u32::try_from(width).ok()?,
            u32::try_from(height).ok()?,
            bgra,
        )?;
        Some(Self {
            image: Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])),
            retired: Rc::clone(retired),
        })
    }

    pub fn image(&self) -> &Arc<RenderImage> {
        &self.image
    }
}

impl Drop for AtlasImage {
    fn drop(&mut self) {
        self.retired.borrow_mut().push(Arc::clone(&self.image));
    }
}

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

impl Session {
    pub(super) fn show_cpu_image(&mut self, width: usize, height: usize, rgb: &[u8]) {
        self.display =
            AtlasImage::from_rgb(width, height, rgb, &self.retired).map(|image| Display {
                width: width as u32,
                height: height as u32,
                source: Source::Cpu(image),
            });
    }

    /// Follow the engine's GPU frame. Cheap when nothing changed.
    pub(super) fn sync_display(&mut self) {
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let generation = document.preview.display_generation();
        if self.synced_generation == generation {
            return;
        }
        self.synced_generation = generation;
        let Some(frame) = document.preview.gpu_display() else {
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

    pub(super) fn drop_retired(&self, cx: &mut App) {
        for image in self.retired.take() {
            cx.drop_image(image, None);
        }
    }
}
