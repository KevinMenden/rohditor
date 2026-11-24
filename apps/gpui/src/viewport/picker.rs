//! Passes clicks on the photo to the active picker.

use gpui::{App, Context, CursorStyle, Entity, MouseDownEvent, Pixels, Point, Window};
use rohditor_engine::view::ImagePlacement;

use super::Viewport;
use super::overlay::{Overlay, pair};
use crate::session::Session;

pub struct PickerOverlay(pub Entity<Session>);

impl PickerOverlay {
    fn reacts(&self, position: Point<Pixels>, photo: ImagePlacement, cx: &App) -> bool {
        self.0.read(cx).picker().is_some() && photo.contains(pair(position))
    }
}

impl Overlay for PickerOverlay {
    fn paint(&self, _: ImagePlacement, _: &mut Window, _: &mut App) {}

    fn press(
        &mut self,
        event: &MouseDownEvent,
        photo: ImagePlacement,
        _: &mut Window,
        cx: &mut Context<Viewport>,
    ) -> bool {
        let reacts = self.reacts(event.position, photo, cx);
        if reacts {
            let position = photo.to_image_clamped(pair(event.position));
            self.0.update(cx, |session, cx| session.pick(position, cx));
        }
        reacts
    }

    fn cursor(
        &self,
        position: Point<Pixels>,
        photo: ImagePlacement,
        cx: &App,
    ) -> Option<CursorStyle> {
        self.reacts(position, photo, cx)
            .then_some(CursorStyle::Crosshair)
    }
}
