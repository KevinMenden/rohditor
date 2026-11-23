//! The photo: drag pans, the wheel zooms at the pointer, a double click returns to fit.

use std::sync::Arc;

use gpui::{
    Bounds, Context, Corners, Entity, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    Pixels, Point, Render, ScrollWheelEvent, Window, canvas, div, point, prelude::*, px, size,
};
use rohditor_engine::view::{ViewState, fit_scale};

use crate::actions::{commands, key_label};
use crate::session::{Display, Session};
use crate::theme::{RADIUS, theme};
use crate::ui::test_id::TestId as _;

/// Pixels per wheel line, for mice that report lines.
const LINE_HEIGHT: Pixels = px(20.0);
/// Space around a fitted photo.
const MARGIN: Pixels = px(20.0);

pub struct Viewport {
    session: Entity<Session>,
    bounds: Bounds<Pixels>,
    drag_from: Option<Point<Pixels>>,
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
            session,
            bounds: Bounds::default(),
            drag_from: None,
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

    pub fn zoom_to_fit(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.change_view(window, cx, |view, _, now| view.fit(now));
    }

    pub fn actual_pixels(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.change_view(window, cx, |view, _, now| view.actual_size(now));
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.drag_from = Some(event.position);
        if event.click_count == 2 {
            self.zoom_to_fit(window, cx);
        }
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !event.dragging() {
            self.drag_from = None;
            return;
        }
        let Some(from) = self.drag_from.replace(event.position) else {
            return;
        };
        let delta = event.position - from;
        self.change_view(window, cx, |view, geometry, _| {
            view.pan_by(
                geometry.fit(),
                geometry.viewport,
                geometry.image,
                [delta.x.as_f32(), delta.y.as_f32()],
            );
        });
    }

    fn scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let pointer = event.position - self.bounds.center();
        let amount = event.delta.pixel_delta(LINE_HEIGHT).y.as_f32();
        self.change_view(window, cx, |view, geometry, now| {
            view.zoom_by(
                geometry.fit(),
                geometry.viewport,
                geometry.image,
                Some([pointer.x.as_f32(), pointer.y.as_f32()]),
                amount,
                now,
            );
        });
    }

    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.bounds = bounds;
        let session = self.session.read(cx);
        let (Some(display), Some(document)) = (session.display(), session.document()) else {
            return;
        };
        let geometry = self.geometry(display, window);
        let view = &document.preview.view;
        let scale = view.scale(geometry.fit());
        let pan = view.pan();
        let target = Bounds::centered_at(
            bounds.center() + point(px(pan[0]), px(pan[1])),
            size(px(geometry.image[0] * scale), px(geometry.image[1] * scale)),
        );
        let image = Arc::clone(display.image());
        if let Err(error) = window.paint_image(target, target, Corners::default(), image, 0, false)
        {
            log::error!("could not paint the preview: {error:#}");
        }
    }

    /// How the open photo is shown.
    pub fn view<'a>(&self, cx: &'a gpui::App) -> Option<&'a ViewState> {
        Some(&self.session.read(cx).document()?.preview.view)
    }

    /// The zoom as a share of the displayed image's pixels.
    pub fn zoom_label(&self, window: &Window, cx: &gpui::App) -> Option<String> {
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

/// Shown while no photo is open: the commands to start with.
fn welcome() -> impl IntoElement {
    let theme = theme();
    let starters = ["file: open", "file: open folder", "view: command palette"];
    let rows = commands()
        .into_iter()
        .filter(move |command| starters.contains(&command.name))
        .map(move |command| {
            let action = command.action;
            let title = match command.name {
                "file: open" => "Open File",
                "file: open folder" => "Open Folder",
                _ => "Command Palette",
            };
            div()
                .id(command.name)
                .flex()
                .justify_between()
                .w(px(260.0))
                .px_3()
                .py_1()
                .rounded(RADIUS)
                .cursor_pointer()
                .hover(move |style| style.bg(theme.hover))
                .child(title)
                .children(
                    command
                        .key
                        .map(|key| div().text_color(theme.faint).child(key_label(key))),
                )
                .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
        });
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_1()
        .text_color(theme.muted)
        .children(rows)
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
            .relative()
            .flex_1()
            .min_h_0()
            .bg(if empty { theme.editor } else { theme.viewport })
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .test_id("viewport")
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, cx| {
                        viewport.update(cx, |viewport, cx| viewport.paint(bounds, window, cx));
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
