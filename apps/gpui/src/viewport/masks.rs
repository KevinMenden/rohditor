//! The shapes of the masks: the handles of the selected one, and pins for the others.

use gpui::{
    App, BorderStyle, Bounds, Context, CursorStyle, Entity, Hsla, MouseDownEvent, MouseMoveEvent,
    PathBuilder, Pixels, Point, Window, black, point, px, quad, size, transparent_black, white,
};
use rohditor_engine::tools::{
    MaskEditMode, MaskHandle, MaskOutline, MaskPointerEvents, MaskToolSession, MaskView,
};
use rohditor_engine::view::ImagePlacement;

use super::Viewport;
use super::overlay::{Overlay, pair};
use crate::session::Session;
use crate::theme::theme;

const HANDLE_RADIUS: f32 = 5.0;
const PIN_RADIUS: f32 = 4.5;
/// How far the pointer may move in a click.
const CLICK_SLOP: f32 = 3.0;
/// Enough that an ellipse looks smooth at any zoom.
const ELLIPSE_SEGMENTS: u8 = 96;

pub struct MaskOverlay {
    session: Entity<Session>,
    /// The press on a pin, which selects its mask if it becomes a click.
    pressed_pin: Option<(u32, Point<Pixels>)>,
}

impl MaskOverlay {
    pub fn new(session: Entity<Session>) -> Self {
        Self {
            session,
            pressed_pin: None,
        }
    }

    /// Ask the tool something, if it is active and the photo is shown.
    fn ask<R>(
        &self,
        photo: ImagePlacement,
        cx: &App,
        question: impl FnOnce(&MaskToolSession, &rohditor_edit::EditRecipe, &MaskView) -> R,
    ) -> Option<R> {
        let session = self.session.read(cx);
        let view = view(session, photo)?;
        let recipe = session.document()?.edits.recipe();
        Some(question(session.masks()?, recipe, &view))
    }

    fn drive(&self, events: &MaskPointerEvents, photo: ImagePlacement, cx: &mut Context<Viewport>) {
        self.session.update(cx, |session, cx| {
            if let Some(view) = view(session, photo) {
                session.drive_masks(events, &view, cx);
            }
        });
    }
}

fn view(session: &Session, photo: ImagePlacement) -> Option<MaskView> {
    let geometry = session.mask_geometry()?;
    Some(MaskView::new(geometry, f64::from(photo.size[0])))
}

impl Overlay for MaskOverlay {
    fn paint(&self, photo: ImagePlacement, window: &mut Window, cx: &mut App) {
        let pointer = photo.to_image(pair(window.mouse_position()));
        let shown = self.ask(photo, cx, |masks, recipe, view| {
            (
                masks.pins(recipe, view),
                masks.pin_at(recipe, view, pointer),
                masks.outline(recipe, view),
            )
        });
        let Some((pins, hovered, outline)) = shown else {
            return;
        };
        let theme = theme();
        let screen = |position| screen(photo, position);
        for pin in pins {
            let fill = if hovered == Some(pin.layer) {
                theme.accent
            } else {
                black().opacity(0.6)
            };
            let edge = white().opacity(if pin.enabled { 1.0 } else { 0.45 });
            dot(screen(pin.position), PIN_RADIUS, fill, edge, 1.5, window);
        }
        match outline {
            Some(MaskOutline::Linear { start, center, end }) => {
                linear(screen(start), screen(center), screen(end), window);
            }
            Some(MaskOutline::Radial {
                center,
                x,
                y,
                feather,
                inner,
            }) => radial(
                screen(center),
                [screen(x), screen(y), screen(feather)],
                inner as f32,
                window,
            ),
            None => {}
        }
    }

    fn press(
        &mut self,
        event: &MouseDownEvent,
        photo: ImagePlacement,
        _: &mut Window,
        cx: &mut Context<Viewport>,
    ) -> bool {
        self.pressed_pin = None;
        let position = photo.to_image(pair(event.position));
        let found = self.ask(photo, cx, |masks, recipe, view| {
            let handle = masks.hit_test(recipe, view, position);
            // A double click inside an ellipse returns to fit, as everywhere.
            let moves = handle == Some(MaskHandle::RadialCenter) && event.click_count == 2;
            let grabs = masks.grabs(recipe, view, position) && !moves;
            (grabs, masks.pin_at(recipe, view, position))
        });
        let Some((grabs, pin)) = found else {
            return false;
        };
        self.pressed_pin = pin.map(|mask| (mask, event.position));
        if grabs {
            let events = MaskPointerEvents {
                press: Some(position),
                ..MaskPointerEvents::default()
            };
            self.drive(&events, photo, cx);
        }
        grabs || pin.is_some()
    }

    fn drag(
        &mut self,
        event: &MouseMoveEvent,
        photo: ImagePlacement,
        _: &mut Window,
        cx: &mut Context<Viewport>,
    ) {
        if let Some((_, pressed)) = self.pressed_pin {
            let moved = event.position - pressed;
            if moved.x.as_f32().hypot(moved.y.as_f32()) <= CLICK_SLOP {
                return;
            }
            self.pressed_pin = None;
        }
        let events = MaskPointerEvents {
            drag_to: vec![photo.to_image(pair(event.position))],
            ..MaskPointerEvents::default()
        };
        self.drive(&events, photo, cx);
    }

    fn release(&mut self, _: &mut Window, cx: &mut Context<Viewport>) {
        let clicked = self.pressed_pin.take().map(|(mask, _)| mask);
        self.session
            .update(cx, |session, cx| session.release_masks(clicked, cx));
    }

    fn cursor(
        &self,
        position: Point<Pixels>,
        photo: ImagePlacement,
        cx: &App,
    ) -> Option<CursorStyle> {
        let position = photo.to_image(pair(position));
        self.ask(photo, cx, |masks, recipe, view| {
            if masks.pin_at(recipe, view, position).is_some() {
                return Some(CursorStyle::PointingHand);
            }
            match masks.mode {
                MaskEditMode::Select => masks.hit_test(recipe, view, position).map(cursor_for),
                _ => Some(CursorStyle::Crosshair),
            }
        })?
    }
}

const fn cursor_for(handle: MaskHandle) -> CursorStyle {
    match handle {
        // GPUI has no cursor for turning.
        MaskHandle::LinearRotate | MaskHandle::RadialRotate => CursorStyle::PointingHand,
        MaskHandle::LinearStart
        | MaskHandle::LinearEnd
        | MaskHandle::LinearCenter
        | MaskHandle::RadialCenter
        | MaskHandle::RadialRadiusX
        | MaskHandle::RadialRadiusY
        | MaskHandle::RadialFeather => CursorStyle::OpenHand,
    }
}

fn screen(photo: ImagePlacement, position: (f64, f64)) -> Point<Pixels> {
    let [x, y] = photo.to_screen(position).map(px);
    point(x, y)
}

/// Lines across the photo at the start, center and end, and the axis between them with a ring on
/// it that stands for turning.
fn linear(start: Point<Pixels>, center: Point<Pixels>, end: Point<Pixels>, window: &mut Window) {
    let axis = end - start;
    let length = axis.x.as_f32().hypot(axis.y.as_f32());
    if length <= f32::EPSILON {
        return;
    }
    let visible = window.content_mask().bounds;
    // Long enough to cross what is visible from any point near it.
    let reach = visible.size.width.as_f32() + visible.size.height.as_f32() + length;
    let across = point(-axis.y, axis.x) * (reach / length);
    for (anchor, width) in [(start, 1.0), (center, 1.5), (end, 1.0)] {
        halo(&[anchor - across, anchor + across], width, window);
    }
    halo(&[start, end], 1.0, window);
    let ring = start + (center - start) * 0.5;
    dot(
        ring,
        HANDLE_RADIUS + 1.0,
        transparent_black(),
        black().opacity(0.6),
        3.0,
        window,
    );
    dot(
        ring,
        HANDLE_RADIUS,
        transparent_black(),
        white(),
        1.5,
        window,
    );
    handle(start, HANDLE_RADIUS - 1.0, window);
    handle(end, HANDLE_RADIUS - 1.0, window);
    handle(center, HANDLE_RADIUS, window);
}

/// The ellipse through `x` and `y`, and the one inside which the mask has its full strength.
fn radial(
    center: Point<Pixels>,
    [x, y, feather]: [Point<Pixels>; 3],
    inner: f32,
    window: &mut Window,
) {
    let (right, down) = (x - center, y - center);
    let ellipse = |scale: f32| -> Vec<Point<Pixels>> {
        (0..=ELLIPSE_SEGMENTS)
            .map(|step| {
                let angle = f32::from(step) / f32::from(ELLIPSE_SEGMENTS) * std::f32::consts::TAU;
                let (sin, cos) = angle.sin_cos();
                center + right * (cos * scale) + down * (sin * scale)
            })
            .collect()
    };
    halo(&ellipse(1.0), 1.5, window);
    if inner > 0.0 {
        halo(&ellipse(inner), 1.0, window);
    }
    for marker in [x, y, feather] {
        handle(marker, HANDLE_RADIUS - 1.0, window);
    }
    handle(center, HANDLE_RADIUS, window);
}

/// A line that shows on any photo.
fn halo(points: &[Point<Pixels>], width: f32, window: &mut Window) {
    for (width, color) in [(width + 2.0, black().opacity(0.6)), (width, white())] {
        let mut path = PathBuilder::stroke(px(width));
        path.move_to(points[0]);
        for point in &points[1..] {
            path.line_to(*point);
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    }
}

fn handle(center: Point<Pixels>, radius: f32, window: &mut Window) {
    dot(center, radius, theme().accent, black(), 1.0, window);
}

fn dot(
    center: Point<Pixels>,
    radius: f32,
    fill: Hsla,
    edge: Hsla,
    edge_width: f32,
    window: &mut Window,
) {
    let diameter = px(radius * 2.0);
    window.paint_quad(quad(
        Bounds::centered_at(center, size(diameter, diameter)),
        px(radius),
        fill,
        px(edge_width),
        edge,
        BorderStyle::Solid,
    ));
}
