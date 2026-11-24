//! The graph of the tone curve. A press anywhere on it grabs the closest handle and drags it up
//! and down, Shift drags finer, and a double click resets it. The rows below have its values.

use gpui::{
    Bounds, Context, CursorStyle, DispatchPhase, Entity, Hsla, IntoElement, MouseButton,
    MouseDownEvent, PathBuilder, Pixels, Point, Role, Window, canvas, div, point, prelude::*, px,
    relative,
};
use rohditor_core::{ToneCurveHandle, display_tone_curve};
use rohditor_edit::ToneCurve;

use super::DevelopPanel;
use super::focus::Control;
use super::gestures::Grip;
use crate::theme::{RADIUS, theme};
use crate::ui::test_id::TestId as _;

const HEIGHT: Pixels = px(160.0);
/// The curve bends between its points on these axes.
const SEGMENTS: u8 = 96;
const HANDLE: Pixels = px(9.0);

impl DevelopPanel {
    pub(super) fn tone_curve(
        &self,
        curve: &ToneCurve,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = theme();
        let grabbed = self.drag.as_ref().map(|drag| drag.parameter);
        let handles = ToneCurveHandle::ALL.map(|handle| {
            let active = grabbed == Some(handle.parameter());
            div()
                .test_id(format!("handle:{:?}", handle.parameter()))
                .absolute()
                .left(relative(handle.x()))
                .top(relative(1.0 - handle.y(curve)))
                .ml(-HANDLE / 2.0)
                .mt(-HANDLE / 2.0)
                .size(HANDLE)
                .rounded_full()
                .border_1()
                .border_color(theme.editor)
                .bg(if active { theme.accent } else { theme.text })
        });
        div()
            .id("tone-curve")
            .role(Role::Image)
            .aria_label("Tone curve")
            .test_id("tone-curve")
            .relative()
            .h(HEIGHT)
            .rounded(RADIUS)
            .bg(theme.editor)
            .cursor(CursorStyle::ResizeUpDown)
            .child(graph(curve.clone()))
            .children(handles)
            .child(press_area(cx.entity()))
    }

    fn press_curve(
        &mut self,
        event: &MouseDownEvent,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let handle =
            ToneCurveHandle::nearest((event.position.x - bounds.left()) / bounds.size.width);
        // For the arrow keys. Not on the panel, which would get it now.
        window.prevent_default();
        let row = Control::Parameter(handle.parameter());
        self.focus.control(row).focus(window, cx);
        self.press(Grip::Curve(handle), event, bounds, cx);
    }
}

fn graph(curve: ToneCurve) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let theme = theme();
            for part in [0.25, 0.5, 0.75] {
                stroke(
                    bounds,
                    &[(part, 0.0), (part, 1.0)],
                    1.0,
                    theme.border,
                    window,
                );
                stroke(
                    bounds,
                    &[(0.0, part), (1.0, part)],
                    1.0,
                    theme.border,
                    window,
                );
            }
            stroke(bounds, &[(0.0, 0.0), (1.0, 1.0)], 1.0, theme.faint, window);
            let points: Vec<_> = (0..=SEGMENTS)
                .map(|step| f32::from(step) / f32::from(SEGMENTS))
                .map(|x| (x, display_tone_curve(&curve, x)))
                .collect();
            stroke(bounds, &points, 1.5, theme.accent, window);
        },
    )
    .absolute()
    .size_full()
}

/// Draw a line through `points`, which are 0 to 1 from the bottom left.
fn stroke(
    bounds: Bounds<Pixels>,
    points: &[(f32, f32)],
    width: f32,
    color: Hsla,
    window: &mut Window,
) {
    let at = |&(x, y): &(f32, f32)| -> Point<Pixels> {
        bounds.bottom_left() + point(bounds.size.width * x, -bounds.size.height * y)
    };
    let mut path = PathBuilder::stroke(px(width));
    path.move_to(at(&points[0]));
    for point in &points[1..] {
        path.line_to(at(point));
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// Covers the graph and reports presses together with its bounds.
fn press_area(panel: Entity<DevelopPanel>) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
        move |bounds, hitbox, window, _| {
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble
                    && event.button == MouseButton::Left
                    && hitbox.is_hovered(window)
                {
                    panel.update(cx, |panel, cx| panel.press_curve(event, bounds, window, cx));
                }
            });
        },
    )
    .absolute()
    .size_full()
}
