//! Draws the clipping tint over the photo.

use std::sync::Arc;

use gpui::{App, Corners, Entity, Window};
use rohditor_engine::view::ImagePlacement;

use super::bounds_of;
use super::overlay::Overlay;
use crate::session::Session;

pub struct ClippingOverlay(pub Entity<Session>);

impl Overlay for ClippingOverlay {
    fn paint(&self, photo: ImagePlacement, window: &mut Window, cx: &mut App) {
        let Some(image) = self.0.read(cx).clipping_image().map(Arc::clone) else {
            return;
        };
        let target = bounds_of(photo);
        if let Err(error) = window.paint_image(target, target, Corners::default(), image, 0, false)
        {
            log::error!("could not paint the clipping overlay: {error:#}");
        }
    }
}
