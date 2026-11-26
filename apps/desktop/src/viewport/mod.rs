//! The photo and the overlays on it. Drag pans, unless an overlay claims the drag; the wheel
//! zooms at the pointer, and a double click returns to fit.

mod clipping;
mod crop;
mod masks;
pub mod overlay;
mod picker;
mod welcome;

use std::sync::Arc;

use gpui::{
    App, Bounds, Context, Corners, CursorStyle, DispatchPhase, Entity, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, Role, ScrollWheelEvent,
    Window, canvas, div, point, prelude::*, px, size,
};
use rohditor_engine::view::{ImagePlacement, ViewState, fit_scale};

use crate::session::{Display, Session};
use crate::theme::theme;
use crate::ui::test_id::TestId as _;
use clipping::ClippingOverlay;
use crop::CropOverlay;
use masks::MaskOverlay;
use overlay::{Overlay, pair};
use picker::PickerOverlay;
use welcome::welcome;

/// Pixels per wheel line, for mice that report lines.
const LINE_HEIGHT: Pixels = px(20.0);
/// Space around a fitted photo.
const MARGIN: Pixels = px(20.0);

pub struct Viewport {
    session: Entity<Session>,
    bounds: Bounds<Pixels>,
    /// Bottom to top.
    overlays: Vec<Box<dyn Overlay>>,
    drag: Option<Drag>,
    cursor: Option<CursorStyle>,
}

enum Drag {
    /// Panning, with the pointer last seen here.
    Pan(Point<Pixels>),
    /// Claimed by an overlay.
    Overlay {
        index: usize,
        /// What it is not told yet, which happens once per frame: mice move more often than that.
        moves: Vec<MouseMoveEvent>,
    },
}

/// Sizes in logical pixels, as `ViewState` expects them. At 100% one image pixel covers one
/// device pixel.
#[derive(Clone, Copy)]
struct Geometry {
    viewport: [f32; 2],
    image: [f32; 2],
}

impl Geometry {
    fn fit(self) -> f32 {
        fit_scale(self.viewport, self.image)
    }
}

impl Viewport {
    pub fn new(session: Entity<Session>, cx: &mut Context<Self>) -> Self {
        cx.observe(&session, |_, _, cx| cx.notify()).detach();
        Self {
            overlays: vec![
                Box::new(ClippingOverlay(session.clone())),
                Box::new(CropOverlay(session.clone())),
                Box::new(MaskOverlay::new(session.clone())),
                Box::new(PickerOverlay(session.clone())),
            ],
            session,
            bounds: Bounds::default(),
            drag: None,
            cursor: None,
        }
    }

    fn geometry(&self, display: &Display, window: &Window) -> Geometry {
        let scale = window.scale_factor();
        let padded = self.bounds.size - size(MARGIN * 2.0, MARGIN * 2.0);
        Geometry {
            viewport: [
                padded.width.as_f32().max(1.0),
                padded.height.as_f32().max(1.0),
            ],
            image: [display.width as f32 / scale, display.height as f32 / scale],
        }
    }

    fn change_view(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut ViewState, Geometry, f64),
    ) {
        let Some(geometry) = self
            .session
            .read(cx)
            .display()
            .map(|display| self.geometry(display, window))
        else {
            return;
        };
        self.session.update(cx, |session, cx| {
            let now = session.now();
            if let Some(document) = session.document_mut() {
                change(&mut document.preview.view, geometry, now);
                cx.notify();
            }
        });
    }

    pub fn zoom_to_fit(&mut self, cx: &mut Context<Self>) {
        self.session.update(cx, Session::show_fit);
    }

    /// Source resolution: the photo is rendered again at its full size.
    pub fn actual_pixels(&mut self, cx: &mut Context<Self>) {
        self.session.update(cx, Session::show_actual_size);
    }

    /// Where the photo is, if one is shown.
    fn placement(&self, window: &Window, cx: &App) -> Option<ImagePlacement> {
        let session = self.session.read(cx);
        let geometry = self.geometry(session.display()?, window);
        let view = &session.document()?.preview.view;
        Some(view.placement(geometry.fit(), pair(self.bounds.center()), geometry.image))
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.end_drag(window, cx);
        let Some(photo) = self.placement(window, cx) else {
            return;
        };
        // The topmost overlay comes first.
        let claimed = (0..self.overlays.len())
            .rev()
            .find(|&index| self.overlays[index].press(event, photo, window, cx));
        if let Some(index) = claimed {
            self.drag = Some(Drag::Overlay {
                index,
                moves: Vec::new(),
            });
            return;
        }
        self.drag = Some(Drag::Pan(event.position));
        if event.click_count == 2 {
            self.zoom_to_fit(cx);
        }
    }

    /// The pointer moved anywhere in the window.
    fn drag_to(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !event.dragging() {
            // Also a release the window never saw.
            self.end_drag(window, cx);
            return;
        }
        match &mut self.drag {
            Some(Drag::Pan(from)) => {
                let delta = pair(event.position - *from);
                *from = event.position;
                self.change_view(window, cx, |view, geometry, _| {
                    view.pan_by(geometry.fit(), geometry.viewport, geometry.image, delta);
                });
            }
            Some(Drag::Overlay { moves, .. }) => {
                moves.push(event.clone());
                if moves.len() == 1 {
                    cx.on_next_frame(window, Self::continue_drag);
                }
            }
            None => {}
        }
    }

    /// Tell the overlay that claimed the drag how the pointer moved.
    fn continue_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Drag::Overlay { index, moves }) = &mut self.drag else {
            return;
        };
        let (index, moves) = (*index, std::mem::take(moves));
        if !moves.is_empty()
            && let Some(photo) = self.placement(window, cx)
        {
            self.overlays[index].drag(&moves, photo, window, cx);
        }
    }

    fn end_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.continue_drag(window, cx);
        if let Some(Drag::Overlay { index, .. }) = self.drag.take() {
            self.overlays[index].release(window, cx);
        }
    }

    /// Follow the pointer, with the cursor of the overlay under it.
    fn hover(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.drag.is_some() {
            return;
        }
        let cursor = self.placement(window, cx).and_then(|photo| {
            let mut topmost = self.overlays.iter().rev();
            topmost.find_map(|overlay| overlay.cursor(event.position, photo, cx))
        });
        let follows = self.overlays.iter().any(|one| one.follows_pointer(cx));
        if self.cursor != cursor || follows {
            self.cursor = cursor;
            cx.notify();
        }
    }

    fn scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The positions so far are on the photo as it is still shown.
        self.continue_drag(window, cx);
        let delta = event.delta.pixel_delta(LINE_HEIGHT).y;
        if let Some(photo) = self.placement(window, cx) {
            let mut topmost = self.overlays.iter_mut().rev();
            if topmost.any(|one| one.scroll(delta / LINE_HEIGHT, event.modifiers, photo, cx)) {
                return;
            }
        }
        let pointer = pair(event.position - self.bounds.center());
        let amount = delta.as_f32();
        self.change_view(window, cx, |view, geometry, now| {
            view.zoom_by(
                geometry.fit(),
                geometry.viewport,
                geometry.image,
                Some(pointer),
                amount,
                now,
            );
        });
    }

    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.bounds = bounds;
        let Some(photo) = self.placement(window, cx) else {
            return;
        };
        let Some(image) = self
            .session
            .read(cx)
            .display()
            .map(|d| Arc::clone(d.image()))
        else {
            return;
        };
        let target = bounds_of(photo);
        if let Err(error) = window.paint_image(target, target, Corners::default(), image, 0, false)
        {
            log::error!("could not paint the preview: {error:#}");
        }
        for overlay in &self.overlays {
            overlay.paint(photo, window, cx);
        }
    }

    /// How the open photo is shown.
    pub fn view<'a>(&self, cx: &'a App) -> Option<&'a ViewState> {
        Some(&self.session.read(cx).document()?.preview.view)
    }

    /// The zoom as a share of the displayed image's pixels.
    pub fn zoom_label(&self, window: &Window, cx: &App) -> Option<String> {
        let session = self.session.read(cx);
        let geometry = self.geometry(session.display()?, window);
        let view = &session.document()?.preview.view;
        Some(if view.is_fit() {
            format!("Fit {:.0}%", geometry.fit() * 100.0)
        } else {
            format!("{:.0}%", view.scale(geometry.fit()) * 100.0)
        })
    }
}

/// Where the photo is, in the window.
fn bounds_of(photo: ImagePlacement) -> Bounds<Pixels> {
    let [x, y] = photo.origin.map(px);
    let [width, height] = photo.size.map(px);
    Bounds::new(point(x, y), size(width, height))
}

/// Follow the pointer wherever it goes while a drag lasts.
fn follow_drag(viewport: Entity<Viewport>, window: &mut Window) {
    let moved = viewport.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if phase == DispatchPhase::Capture {
            moved.update(cx, |viewport, cx| viewport.drag_to(event, window, cx));
        }
    });
    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
        if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
            viewport.update(cx, |viewport, cx| viewport.end_drag(window, cx));
        }
    });
}

impl Render for Viewport {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme();
        let viewport = cx.entity();
        let session = self.session.read(cx);
        let empty = session.document().is_none();
        let opening = session.display().is_none() && !empty;
        div()
            .id("viewport")
            .role(Role::Image)
            .aria_label("Photo")
            .relative()
            .flex_1()
            .min_h_0()
            .bg(if empty { theme.editor } else { theme.viewport })
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::hover))
            // What is drawn at the pointer leaves with it.
            .on_hover(cx.listener(|_, _, _, cx| cx.notify()))
            .when_some(self.cursor, |view, cursor| view.cursor(cursor))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .test_id("viewport")
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, cx| {
                        viewport.update(cx, |viewport, cx| viewport.paint(bounds, window, cx));
                        follow_drag(viewport, window);
                    },
                )
                .size_full(),
            )
            .when(empty, |view| view.child(welcome()))
            .when(opening, |view| {
                view.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme.faint)
                        .child("Opening…"),
                )
            })
    }
}

#[cfg(test)]
impl Viewport {
    pub fn push_overlay(&mut self, overlay: Box<dyn Overlay>, cx: &mut Context<Self>) {
        self.overlays.push(overlay);
        cx.notify();
    }
}
