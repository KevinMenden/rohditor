//! ROHDITOR: draws a texture the application rendered on GPUI's device without copying it.
//!
//! An [`ExternalImage`] hands out an ordinary [`RenderImage`] (a 1x1 placeholder). When GPUI
//! asks the atlas for that image's tile, the atlas answers with a tile covering the whole
//! external texture instead of uploading the placeholder, so `Window::paint_image` and the
//! sprite pipeline work unchanged.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use gpui::{
    AtlasKey, AtlasTextureId, AtlasTextureKind, AtlasTile, Bounds, DevicePixels, ImageId, Point,
    RenderImage, Size, TileId,
};
use parking_lot::Mutex;

use crate::wgpu_atlas::WgpuTextureInfo;

/// Texture indices from here on are external; the atlas allocates its own from zero.
const FIRST_INDEX: u32 = 1 << 31;

static NEXT_INDEX: AtomicU32 = AtomicU32::new(FIRST_INDEX);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(0);
static IMAGES: Mutex<Vec<(ImageId, Entry)>> = Mutex::new(Vec::new());

struct Entry {
    index: u32,
    generation: u64,
    view: wgpu::TextureView,
    size: Size<DevicePixels>,
}

/// A texture on GPUI's device that can be painted like an image. Unregistered on drop.
pub struct ExternalImage {
    image: Arc<RenderImage>,
}

impl ExternalImage {
    /// `view` must be a filterable 2D view of a texture on the device GPUI renders with.
    pub fn new(view: wgpu::TextureView, width: u32, height: u32) -> Self {
        let placeholder = image::Frame::new(image::RgbaImage::new(1, 1));
        let image = Arc::new(RenderImage::new(vec![placeholder]));
        IMAGES.lock().push((
            image.id,
            Entry {
                index: NEXT_INDEX.fetch_add(1, Ordering::Relaxed),
                generation: next_generation(),
                view,
                size: device_size(width, height),
            },
        ));
        Self { image }
    }

    /// Show another texture from now on, for example after the source was resized.
    pub fn update(&self, view: wgpu::TextureView, width: u32, height: u32) {
        if let Some((_, entry)) = IMAGES
            .lock()
            .iter_mut()
            .find(|(id, _)| *id == self.image.id)
        {
            entry.generation = next_generation();
            entry.view = view;
            entry.size = device_size(width, height);
        }
    }

    /// What to pass to `Window::paint_image`.
    pub fn image(&self) -> &Arc<RenderImage> {
        &self.image
    }
}

impl Drop for ExternalImage {
    fn drop(&mut self) {
        IMAGES.lock().retain(|(id, _)| *id != self.image.id);
    }
}

pub(crate) fn tile(key: &AtlasKey) -> Option<AtlasTile> {
    let AtlasKey::Image(params) = key else {
        return None;
    };
    let images = IMAGES.lock();
    let (_, entry) = images.iter().find(|(id, _)| *id == params.image_id)?;
    Some(AtlasTile {
        texture_id: AtlasTextureId {
            index: entry.index,
            kind: AtlasTextureKind::Polychrome,
        },
        tile_id: TileId(0),
        padding: 0,
        bounds: Bounds {
            origin: Point::default(),
            size: entry.size,
        },
    })
}

pub(crate) fn texture_info(id: AtlasTextureId) -> Option<WgpuTextureInfo> {
    if id.index < FIRST_INDEX {
        return None;
    }
    let images = IMAGES.lock();
    let (_, entry) = images.iter().find(|(_, entry)| entry.index == id.index)?;
    Some(WgpuTextureInfo {
        view: entry.view.clone(),
        generation: entry.generation,
    })
}

fn next_generation() -> u64 {
    NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
}

fn device_size(width: u32, height: u32) -> Size<DevicePixels> {
    Size {
        width: DevicePixels(width as i32),
        height: DevicePixels(height as i32),
    }
}
