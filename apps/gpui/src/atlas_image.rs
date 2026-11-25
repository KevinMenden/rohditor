//! Images the application uploads to GPUI's sprite atlas.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, RenderImage};

/// Images no longer shown, waiting to be removed from the atlas.
#[derive(Clone, Default)]
pub struct RetiredImages(Rc<RefCell<Vec<Arc<RenderImage>>>>);

impl RetiredImages {
    pub fn remove_from_atlas(&self, cx: &mut App) {
        for image in self.0.take() {
            cx.drop_image(image, None);
        }
    }
}

/// A CPU image that leaves the sprite atlas when it is dropped. GPUI keeps images it has painted
/// until told otherwise.
pub struct AtlasImage {
    image: Arc<RenderImage>,
    retired: RetiredImages,
}

impl AtlasImage {
    pub fn from_rgb(
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
            retired: retired.clone(),
        })
    }

    #[expect(dead_code, reason = "for the filmstrip (#233, L.3)")]
    pub fn image(&self) -> &Arc<RenderImage> {
        &self.image
    }
}

impl Drop for AtlasImage {
    fn drop(&mut self) {
        self.retired.0.borrow_mut().push(Arc::clone(&self.image));
    }
}
