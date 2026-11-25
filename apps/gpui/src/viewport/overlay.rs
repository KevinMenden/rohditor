//! What is drawn over the photo and reacts to the pointer there: the clipping tint, and the
//! place for the tools (pickers, crop, masks) to plug into.

use gpui::{App, Context, CursorStyle, MouseDownEvent, MouseMoveEvent, Pixels, Point, Window};
use rohditor_engine::view::ImagePlacement;

use super::Viewport;

/// A layer over the photo. `photo` says where the photo is at that moment, in the window's
/// logical pixels; its methods take positions from [`pair`].
///
/// Layers must not read or update the [`Viewport`] entity, which is busy calling them.
pub trait Overlay {
    /// Draw. Clipped to the viewport.
    fn paint(&self, photo: ImagePlacement, window: &mut Window, cx: &mut App);

    /// The left button went down in the viewport, possibly beside the photo. Returning `true`
    /// claims the drag: the photo does not pan, and [`Self::drag`] and [`Self::release`] follow.
    fn press(
        &mut self,
        _event: &MouseDownEvent,
        _photo: ImagePlacement,
        _window: &mut Window,
        _cx: &mut Context<Viewport>,
    ) -> bool {
        false
    }

    /// The pointer moved during a claimed drag, possibly outside the viewport. `moves` are those
    /// since the last frame, oldest first, so that a fast drag is followed but rendered once.
    fn drag(
        &mut self,
        _moves: &[MouseMoveEvent],
        _photo: ImagePlacement,
        _window: &mut Window,
        _cx: &mut Context<Viewport>,
    ) {
    }

    /// A claimed drag is over.
    fn release(&mut self, _window: &mut Window, _cx: &mut Context<Viewport>) {}

    /// Whether it is drawn at the pointer.
    fn follows_pointer(&self, _cx: &App) -> bool {
        false
    }

    /// The cursor while the pointer is at `position`.
    fn cursor(
        &self,
        _position: Point<Pixels>,
        _photo: ImagePlacement,
        _cx: &App,
    ) -> Option<CursorStyle> {
        None
    }
}

/// A position as the engine's view math takes it.
pub fn pair(position: Point<Pixels>) -> [f32; 2] {
    [position.x.as_f32(), position.y.as_f32()]
}
