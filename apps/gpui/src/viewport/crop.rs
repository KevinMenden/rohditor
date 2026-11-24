//! The rectangle of the Crop tool, with its handles.

use gpui::{
    App, BorderStyle, Bounds, Context, CursorStyle, Entity, MouseDownEvent, MouseMoveEvent, Pixels,
    Point, Window, black, fill, outline, point, px, quad, size, white,
};
use rohditor_edit::NormalizedCropRect;
use rohditor_engine::tools::CropHandle;
use rohditor_engine::view::ImagePlacement;

use super::overlay::{Overlay, pair};
use super::{Viewport, bounds_of};
use crate::session::Session;
use crate::theme::theme;

const HANDLE_RADIUS: Pixels = px(5.0);
const LINE: Pixels = px(1.0);

pub struct CropOverlay(pub Entity<Session>);

impl CropOverlay {
    fn handle_at(
        &self,
        position: Point<Pixels>,
        photo: ImagePlacement,
        cx: &App,
    ) -> Option<CropHandle> {
        let draft = self.0.read(cx).crop_draft()?.draft();
        CropHandle::at(photo, draft, pair(position))
    }
}

impl Overlay for CropOverlay {
    fn paint(&self, photo: ImagePlacement, window: &mut Window, cx: &mut App) {
        let Some(draft) = self.0.read(cx).crop_draft().map(|crop| crop.draft()) else {
            return;
        };
        let crop = crop_bounds(photo, draft);
        dim_outside(bounds_of(photo), crop, window);
        thirds(crop, window);
        let accent = theme().accent;
        window.paint_quad(outline(crop, accent, BorderStyle::Solid));
        for handle in CropHandle::OUTLINE {
            let Some(position) = handle.position(draft) else {
                continue;
            };
            let [x, y] = photo.to_screen(position).map(px);
            let dot =
                Bounds::centered_at(point(x, y), size(HANDLE_RADIUS * 2.0, HANDLE_RADIUS * 2.0));
            window.paint_quad(quad(
                dot,
                HANDLE_RADIUS,
                accent,
                LINE,
                black(),
                BorderStyle::Solid,
            ));
        }
    }

    fn press(
        &mut self,
        event: &MouseDownEvent,
        photo: ImagePlacement,
        _: &mut Window,
        cx: &mut Context<Viewport>,
    ) -> bool {
        let Some(handle) = self.handle_at(event.position, photo, cx) else {
            return false;
        };
        // A double click inside the crop returns to fit, as everywhere.
        if handle == CropHandle::Move && event.click_count == 2 {
            return false;
        }
        let position = photo.to_image(pair(event.position));
        self.0.update(cx, |session, cx| {
            session.edit_crop(|crop| crop.begin_drag(handle, position), cx);
        });
        true
    }

    fn drag(
        &mut self,
        event: &MouseMoveEvent,
        photo: ImagePlacement,
        _: &mut Window,
        cx: &mut Context<Viewport>,
    ) {
        let position = photo.to_image(pair(event.position));
        self.0.update(cx, |session, cx| {
            session.edit_crop(|crop| crop.drag_to(position), cx);
        });
    }

    fn release(&mut self, _: &mut Window, cx: &mut Context<Viewport>) {
        self.0.update(cx, |session, cx| {
            session.edit_crop(|crop| crop.finish_drag(), cx);
        });
    }

    fn cursor(
        &self,
        position: Point<Pixels>,
        photo: ImagePlacement,
        cx: &App,
    ) -> Option<CursorStyle> {
        self.handle_at(position, photo, cx).map(cursor_for)
    }
}

const fn cursor_for(handle: CropHandle) -> CursorStyle {
    match handle {
        CropHandle::NorthWest | CropHandle::SouthEast => CursorStyle::ResizeUpLeftDownRight,
        CropHandle::NorthEast | CropHandle::SouthWest => CursorStyle::ResizeUpRightDownLeft,
        CropHandle::North | CropHandle::South => CursorStyle::ResizeUpDown,
        CropHandle::East | CropHandle::West => CursorStyle::ResizeLeftRight,
        CropHandle::Move => CursorStyle::OpenHand,
    }
}

fn crop_bounds(photo: ImagePlacement, crop: NormalizedCropRect) -> Bounds<Pixels> {
    let corner = |x, y| {
        let [x, y] = photo.to_screen((x, y)).map(px);
        point(x, y)
    };
    Bounds::from_corners(corner(crop.left, crop.top), corner(crop.right, crop.bottom))
}

/// Darken what the crop cuts off.
fn dim_outside(photo: Bounds<Pixels>, crop: Bounds<Pixels>, window: &mut Window) {
    let sides = [
        (photo.origin, point(photo.right(), crop.top())),
        (point(photo.left(), crop.bottom()), photo.bottom_right()),
        (
            point(photo.left(), crop.top()),
            point(crop.left(), crop.bottom()),
        ),
        (
            point(crop.right(), crop.top()),
            point(photo.right(), crop.bottom()),
        ),
    ];
    for (from, to) in sides {
        window.paint_quad(fill(Bounds::from_corners(from, to), black().opacity(0.6)));
    }
}

fn thirds(crop: Bounds<Pixels>, window: &mut Window) {
    for third in [1.0 / 3.0, 2.0 / 3.0] {
        let column = point(crop.left() + crop.size.width * third, crop.top());
        let row = point(crop.left(), crop.top() + crop.size.height * third);
        for line in [
            Bounds::new(column, size(LINE, crop.size.height)),
            Bounds::new(row, size(crop.size.width, LINE)),
        ] {
            window.paint_quad(fill(line, white().opacity(0.35)));
        }
    }
}
