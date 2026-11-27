//! The tone curve editor, on sRGB-encoded axes with the histogram behind it.
//!
//! A press on a point selects and drags it, a press elsewhere adds a point on the curve there and
//! drags that, Shift drags finer, and a double click on a point removes it. With the focus, the
//! horizontal arrows move the selected point sideways, Page Up and Page Down move it up and down,
//! and Delete removes it. The two rows below have its input and output, to type or nudge.

use gpui::{
    App, Bounds, Context, CursorStyle, DispatchPhase, Entity, Hsla, IntoElement, MouseButton,
    MouseDownEvent, PathBuilder, Pixels, Point, Role, Window, canvas, div, point, prelude::*, px,
    relative,
};
use rohditor_core::Histogram;
use rohditor_edit::{ParameterRange, ToneCurve};

use super::DevelopPanel;
use super::control::Control;
use crate::panels::controls::{
    ControlPanel as _, Decrease, DecreaseMore, FocusControls as _, Gestures as _, Increase,
    IncreaseMore, ResetValue, RowState, SLIDER_CONTEXT, Slider, SliderRows as _,
};
use crate::session::EditPhase;
use crate::theme::{RADIUS, theme};
use crate::ui::histogram::histogram_backdrop;
use crate::ui::slider::Format;
use crate::ui::test_id::TestId as _;

const HEIGHT: Pixels = px(180.0);
/// The curve is drawn as this many straight pieces.
const SEGMENTS: u16 = 128;
const HANDLE: Pixels = px(9.0);
/// A press this close to a point grabs it instead of adding one.
const GRAB: Pixels = px(8.0);
/// What an arrow key moves the selected point.
const STEP: f32 = 0.01;
/// The input and output of a point.
const POINT_RANGE: ParameterRange = ParameterRange {
    minimum: 0.0,
    maximum: 1.0,
    neutral: 0.0,
};

impl DevelopPanel {
    pub(super) fn tone_curve(
        &self,
        curve: &ToneCurve,
        histogram: Option<&Histogram>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = theme();
        let control = Control::Curve;
        let selected = self.selected_curve_point(curve);
        let grabbed = self.controls.dragged();
        let handles: Vec<_> = (0..curve.points.len())
            .filter(|&index| curve.is_movable(index))
            .map(|index| {
                let [x, y] = curve.points[index];
                let active = selected == Some(index) || grabbed == Some(Control::CurvePoint(index));
                div()
                    .test_id(format!("curve-point:{index}"))
                    .absolute()
                    .left(relative(x))
                    .top(relative(1.0 - y))
                    .ml(-HANDLE / 2.0)
                    .mt(-HANDLE / 2.0)
                    .size(HANDLE)
                    .rounded_full()
                    .border_1()
                    .border_color(theme.editor)
                    .bg(if active { theme.accent } else { theme.text })
            })
            .collect();
        let readout = selected.map_or_else(
            || format!("{} points", curve.points.len()),
            |index| {
                let [x, y] = curve.points[index];
                format!("point at {:.0}, {:.0}", x * 100.0, y * 100.0)
            },
        );
        div()
            .id("tone-curve")
            .role(Role::Group)
            .aria_label("Tone curve")
            .aria_value(readout)
            .test_id("tone-curve")
            .key_context(SLIDER_CONTEXT)
            .track_focus(self.controls.focus.control(control))
            .child(self.revealer(control, cx))
            .relative()
            .h(HEIGHT)
            .rounded(RADIUS)
            .border_1()
            .border_color(gpui::transparent_black())
            .focus(move |style| style.border_color(theme.accent))
            .bg(theme.editor)
            .cursor(CursorStyle::Crosshair)
            .on_action(cx.listener(|this, _: &Decrease, _, cx| this.nudge_curve(-STEP, 0.0, cx)))
            .on_action(cx.listener(|this, _: &Increase, _, cx| this.nudge_curve(STEP, 0.0, cx)))
            .on_action(cx.listener(|this, _: &DecreaseMore, _, cx| {
                this.nudge_curve(0.0, -STEP, cx);
            }))
            .on_action(cx.listener(|this, _: &IncreaseMore, _, cx| {
                this.nudge_curve(0.0, STEP, cx);
            }))
            .on_action(cx.listener(move |this, _: &ResetValue, _, cx| this.reset(control, cx)))
            .child(histogram_backdrop(histogram))
            .child(graph(curve.clone()))
            .children(handles)
            .child(press_area(cx.entity()))
    }

    /// The input and output rows of the selected point.
    pub(super) fn tone_curve_rows(
        &self,
        curve: &ToneCurve,
        cx: &mut Context<Self>,
    ) -> Vec<impl IntoElement + use<>> {
        let Some(index) = self.selected_curve_point(curve) else {
            return Vec::new();
        };
        let [x, y] = curve.points[index];
        // Filled from the diagonal, where the point changes nothing.
        let state = |value| RowState {
            value,
            origin: x,
            edited: x != y,
            effective: true,
        };
        [
            (Control::CurveInput, "CurveInput", state(x)),
            (Control::CurveOutput, "CurveOutput", state(y)),
        ]
        .into_iter()
        .filter_map(|(control, id, state)| {
            let slider = curve_point_slider(control)?;
            Some(self.slider_row(control, id, slider, state, cx))
        })
        .collect()
    }

    /// The selected point, if it is still one that can be moved.
    fn selected_curve_point(&self, curve: &ToneCurve) -> Option<usize> {
        self.curve_point.filter(|&index| curve.is_movable(index))
    }

    fn current_curve(&self, cx: &App) -> Option<ToneCurve> {
        let document = self.session.read(cx).document()?;
        Some(document.edits.recipe().light.tone_curve.clone())
    }

    fn set_curve(&self, curve: ToneCurve, phase: EditPhase, cx: &mut Context<Self>) {
        self.session
            .update(cx, |session, cx| session.set_tone_curve(curve, phase, cx));
    }

    fn press_curve(
        &mut self,
        event: &MouseDownEvent,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(curve) = self.current_curve(cx) else {
            return;
        };
        // For the arrow keys. Not on the panel, which would get it now.
        window.prevent_default();
        self.controls
            .focus
            .control(Control::Curve)
            .focus(window, cx);
        let index = match point_at(&curve, event.position, bounds) {
            Some(index) => index,
            // The second press of a double click removes a point, and adds none.
            None if event.click_count > 1 => return,
            None => {
                let x = (event.position.x - bounds.left()) / bounds.size.width;
                let Some((added, index)) = curve.with_point_added(x) else {
                    return;
                };
                // Part of the drag that follows, so adding and moving are one undo step.
                self.set_curve(added, EditPhase::Dragging, cx);
                index
            }
        };
        self.curve_point = Some(index);
        self.press(Control::CurvePoint(index), event, bounds, cx);
    }

    /// Move a point by parts of the graph's width and height.
    pub(super) fn move_curve_point(
        &mut self,
        index: usize,
        right: f32,
        up: f32,
        phase: EditPhase,
        cx: &mut Context<Self>,
    ) {
        let Some(curve) = self.current_curve(cx) else {
            return;
        };
        let Some(&[x, y]) = curve.points.get(index) else {
            return;
        };
        self.set_curve(
            curve.with_point_moved(index, [x + right, y + up]),
            phase,
            cx,
        );
    }

    pub(super) fn remove_curve_point(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(curve) = self.current_curve(cx) else {
            return;
        };
        if curve.is_movable(index) {
            self.curve_point = None;
            self.set_curve(curve.with_point_removed(index), EditPhase::Finished, cx);
        }
    }

    /// The selected point: removed by Delete on the graph.
    pub(super) fn selected_point_now(&self, cx: &App) -> Option<usize> {
        self.selected_curve_point(&self.current_curve(cx)?)
    }

    fn nudge_curve(&mut self, right: f32, up: f32, cx: &mut Context<Self>) {
        let Some(index) = self.selected_point_now(cx) else {
            return;
        };
        self.hold_nudge(Control::Curve, cx);
        self.move_curve_point(index, right, up, EditPhase::Dragging, cx);
    }

    /// The input or output of the selected point.
    pub(super) fn curve_point_value(&self, control: Control, cx: &App) -> Option<f32> {
        let curve = self.current_curve(cx)?;
        let selected = || {
            self.selected_curve_point(&curve)
                .map(|index| curve.points[index])
        };
        match control {
            Control::CurveInput => selected().map(|point| point[0]),
            Control::CurveOutput => selected().map(|point| point[1]),
            Control::CurvePoint(index) => curve.points.get(index).map(|point| point[1]),
            _ => None,
        }
    }

    pub(super) fn set_curve_point_value(
        &mut self,
        control: Control,
        value: f32,
        phase: EditPhase,
        cx: &mut Context<Self>,
    ) {
        let Some(curve) = self.current_curve(cx) else {
            return;
        };
        let Some(index) = self.selected_curve_point(&curve) else {
            return;
        };
        let [x, y] = curve.points[index];
        let to = match control {
            Control::CurveInput => [value, y],
            Control::CurveOutput => [x, value],
            _ => return,
        };
        self.set_curve(curve.with_point_moved(index, to), phase, cx);
    }
}

/// The rows of the selected point are sliders.
pub(super) fn curve_point_slider(control: Control) -> Option<Slider> {
    let label = match control {
        Control::CurveInput => "Input",
        Control::CurveOutput => "Output",
        _ => return None,
    };
    Some(Slider {
        label: label.into(),
        format: Format::Amount,
        range: POINT_RANGE,
        ramp: None,
    })
}

/// The movable point within [`GRAB`] of `position`, the closest if there are several.
fn point_at(curve: &ToneCurve, position: Point<Pixels>, bounds: Bounds<Pixels>) -> Option<usize> {
    let distance = |index: &usize| {
        let at = to_screen(bounds, curve.points[*index]);
        let (dx, dy) = (f32::from(at.x - position.x), f32::from(at.y - position.y));
        dx.hypot(dy)
    };
    (0..curve.points.len())
        .filter(|&index| curve.is_movable(index))
        .filter(|index| distance(index) <= f32::from(GRAB))
        .min_by(|a, b| distance(a).total_cmp(&distance(b)))
}

/// Where a point of the curve's axes, 0 to 1 from the bottom left, is on the screen.
fn to_screen(bounds: Bounds<Pixels>, [x, y]: [f32; 2]) -> Point<Pixels> {
    bounds.bottom_left() + point(bounds.size.width * x, -bounds.size.height * y)
}

/// The grid, the diagonal of the identity and the curve, which uses the pipeline's evaluation.
fn graph(curve: ToneCurve) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let theme = theme();
            for part in [0.25, 0.5, 0.75] {
                let lines = [[[part, 0.0], [part, 1.0]], [[0.0, part], [1.0, part]]];
                for line in lines {
                    stroke(bounds, &line, 1.0, theme.border, window);
                }
            }
            stroke(bounds, &[[0.0, 0.0], [1.0, 1.0]], 1.0, theme.faint, window);
            let points: Vec<_> = (0..=SEGMENTS)
                .map(|step| f32::from(step) / f32::from(SEGMENTS))
                .map(|x| [x, curve.evaluate(x)])
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
    points: &[[f32; 2]],
    width: f32,
    color: Hsla,
    window: &mut Window,
) {
    let mut path = PathBuilder::stroke(px(width));
    path.move_to(to_screen(bounds, points[0]));
    for &point in &points[1..] {
        path.line_to(to_screen(bounds, point));
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
