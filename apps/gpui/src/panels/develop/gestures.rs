//! Changing a value: dragging, arrow keys, typing and resetting.

use std::time::Duration;

use gpui::{
    Context, DispatchPhase, Entity, FocusHandle, Focusable, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Size, Subscription, Task, Window,
    canvas, prelude::*, px,
};

use rohditor_core::ToneCurveHandle;
use rohditor_edit::{GradingTones, Parameter};

use super::DevelopPanel;
use crate::actions::FocusPhoto;
use crate::panels::adjustments::{Adjustment, adjustment};
use crate::session::EditPhase;
use crate::ui::slider::dragged;
use crate::ui::text_input::TextInput;

const FINE_DRAG: f32 = 0.1;
/// A press this close to a row's right end is on its value.
const VALUE_WIDTH: Pixels = px(72.0);
/// Arrow key presses closer together than this are one undo step.
const NUDGE_PAUSE: Duration = Duration::from_millis(600);

/// What a drag holds.
#[derive(Clone, Copy)]
pub(super) enum Grip {
    Row(Adjustment),
    Curve(ToneCurveHandle),
    Wheel(GradingTones),
    PresetAmount,
}

impl Grip {
    /// What it changes, or the first of them.
    fn parameter(self) -> Option<Parameter> {
        match self {
            Self::Row(adjustment) => Some(adjustment.parameter),
            Self::Curve(handle) => Some(handle.parameter()),
            Self::Wheel(tones) => Some(tones.parameter()),
            Self::PresetAmount => None,
        }
    }
}

/// What an arrow key changes.
#[derive(Clone, Copy)]
pub(super) enum NudgeBy {
    Steps(Adjustment, i8),
    /// By parts of the radius.
    Wheel(GradingTones, f32, f32),
    PresetAmount(f32),
}

#[derive(Clone, Copy)]
pub(super) struct Drag {
    /// `None` for the amount of a preset.
    pub parameter: Option<Parameter>,
    grip: Grip,
    last: Point<Pixels>,
    value: f32,
    /// Of the row or the graph.
    size: Size<Pixels>,
    moved: bool,
    on_value: bool,
}

/// Arrow key changes that are not an undo step yet.
pub(super) struct Nudge {
    parameter: Option<Parameter>,
    _pause: Task<()>,
}

/// A value being typed.
pub(super) struct Typing {
    pub adjustment: Adjustment,
    pub input: Entity<TextInput>,
    return_to: Option<FocusHandle>,
    /// The last press was the click that opened it, so that one more makes a double click.
    just_clicked: bool,
    _blurred: Subscription,
}

impl DevelopPanel {
    fn value(&self, parameter: Parameter, cx: &Context<Self>) -> Option<f32> {
        let session = self.session.read(cx);
        let document = session.document().filter(|open| open.frame.is_some())?;
        let defaults = document.default_recipe();
        Some(parameter.get(document.edits.recipe(), &defaults))
    }

    /// The value of the parameter, or the amount of the preset without one.
    fn gripped(&self, parameter: Option<Parameter>, cx: &Context<Self>) -> Option<f32> {
        match parameter {
            Some(parameter) => self.value(parameter, cx),
            None => self.model.applied_preset.as_ref().map(|applied| applied.1),
        }
    }

    fn set(&self, parameter: Parameter, value: f32, phase: EditPhase, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| {
            session.set_parameter(parameter, value, phase, cx);
        });
    }

    pub(super) fn reset(&mut self, adjustment: Adjustment, cx: &mut Context<Self>) {
        self.finish_nudge(cx);
        let defaults = self
            .session
            .read(cx)
            .document()
            .map(|document| document.default_recipe());
        if let Some(defaults) = defaults {
            let value = adjustment.parameter.get(&defaults, &defaults);
            self.set(adjustment.parameter, value, EditPhase::Finished, cx);
        }
    }

    pub(super) fn press(
        &mut self,
        grip: Grip,
        event: &MouseDownEvent,
        area: gpui::Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.finish_nudge(cx);
        if event.click_count == 2 {
            self.drag = None;
            return match grip {
                Grip::Row(adjustment) => self.reset(adjustment, cx),
                Grip::Curve(handle) => self.reset(adjustment(handle.parameter()), cx),
                Grip::Wheel(tones) => self.reset_wheel(tones, cx),
                Grip::PresetAmount => self.reset_preset_amount(cx),
            };
        }
        let parameter = grip.parameter();
        let Some(value) = self.gripped(parameter, cx) else {
            return;
        };
        self.session
            .update(cx, |session, _| session.begin_gesture());
        self.drag = Some(Drag {
            parameter,
            grip,
            last: event.position,
            value,
            size: area.size,
            moved: false,
            on_value: matches!(grip, Grip::Row(_)) && area.right() - event.position.x < VALUE_WIDTH,
        });
        cx.notify();
    }

    fn drag_to(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        if !event.dragging() {
            // The release happened outside the window.
            drag.on_value = false;
            return self.release(window, cx);
        }
        let moved = event.position - drag.last;
        // In widths and heights, upwards.
        let (right, up) = (moved.x / drag.size.width, -moved.y / drag.size.height);
        let still = match drag.grip {
            Grip::Row(_) | Grip::PresetAmount => right == 0.0,
            Grip::Curve(_) => up == 0.0,
            Grip::Wheel(_) => right == 0.0 && up == 0.0,
        };
        if still {
            return;
        }
        let speed = if event.modifiers.shift {
            FINE_DRAG
        } else {
            1.0
        };
        drag.last = event.position;
        drag.moved = true;
        let value = match drag.grip {
            Grip::Row(adjustment) => {
                let value = dragged(
                    adjustment.format,
                    adjustment.range(),
                    drag.value,
                    right * speed,
                );
                drag.value = value;
                value
            }
            Grip::PresetAmount => {
                drag.value = (drag.value + right * speed).clamp(0.0, 1.0);
                let amount = drag.value;
                return self.set_preset_amount(amount, cx);
            }
            // The radius is half the size.
            Grip::Wheel(tones) => {
                return self.move_wheel(tones, right * speed * 2.0, up * speed * 2.0, cx);
            }
            Grip::Curve(handle) => {
                let Some(document) = self.session.read(cx).document() else {
                    return;
                };
                handle.dragged(&document.edits.recipe().light.tone_curve, up * speed)
            }
        };
        let Some(parameter) = drag.parameter else {
            return;
        };
        self.set(parameter, value, EditPhase::Dragging, cx);
    }

    fn release(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        self.session.update(cx, |session, cx| {
            session.finish_gesture(drag.parameter, cx);
        });
        if let Grip::Row(adjustment) = drag.grip
            && drag.on_value
            && !drag.moved
        {
            self.start_typing(adjustment, window, cx);
            if let Some(typing) = self.typing.as_mut() {
                typing.just_clicked = true;
            }
        }
        cx.notify();
    }

    pub(super) fn nudge(&mut self, by: NudgeBy, cx: &mut Context<Self>) {
        let parameter = match by {
            NudgeBy::Steps(adjustment, _) => Some(adjustment.parameter),
            NudgeBy::Wheel(tones, ..) => Some(tones.parameter()),
            NudgeBy::PresetAmount(_) => None,
        };
        let same = |nudge: &Nudge| nudge.parameter == parameter;
        if !self.nudge.as_ref().is_some_and(same) {
            self.finish_nudge(cx);
        }
        let Some(value) = self.gripped(parameter, cx) else {
            return;
        };
        match by {
            NudgeBy::Steps(adjustment, steps) => {
                let next = adjustment.format.stepped(adjustment.range(), value, steps);
                self.set(adjustment.parameter, next, EditPhase::Dragging, cx);
            }
            NudgeBy::PresetAmount(change) => self.set_preset_amount(value + change, cx),
            NudgeBy::Wheel(tones, x, y) => self.move_wheel(tones, x, y, cx),
        }
        let pause = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NUDGE_PAUSE).await;
            // Gone only while the application quits.
            let _ = this.update(cx, Self::finish_nudge);
        });
        self.nudge = Some(Nudge {
            parameter,
            _pause: pause,
        });
    }

    pub(super) fn finish_nudge(&mut self, cx: &mut Context<Self>) {
        if let Some(nudge) = self.nudge.take() {
            self.session.update(cx, |session, cx| {
                session.finish_gesture(nudge.parameter, cx);
            });
        }
    }

    pub(super) fn start_typing(
        &mut self,
        adjustment: Adjustment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish_nudge(cx);
        let Some(value) = self.value(adjustment.parameter, cx) else {
            return;
        };
        let text = adjustment.format.label(value, adjustment.range());
        let input = cx.new(|cx| TextInput::new(adjustment.label, "", text, cx));
        let focus = input.read(cx).focus_handle(cx);
        let return_to = window.focused(cx);
        focus.focus(window, cx);
        let blurred = cx.on_blur(&focus, window, |panel, _, cx| panel.commit_typing(cx));
        self.typing = Some(Typing {
            adjustment,
            input,
            return_to,
            just_clicked: false,
            _blurred: blurred,
        });
        cx.notify();
    }

    /// A press on the row of the value being typed, before its input gets it. The second click of
    /// a double click on the value resets it, as anywhere on the row.
    pub(super) fn press_while_typing(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(typing) = self.typing.as_mut() else {
            return;
        };
        let just_clicked = std::mem::take(&mut typing.just_clicked);
        if just_clicked && event.button == MouseButton::Left && event.click_count == 2 {
            let adjustment = typing.adjustment;
            window.prevent_default();
            cx.stop_propagation();
            self.stop_typing(false, window, cx);
            self.reset(adjustment, cx);
        }
    }

    /// Stop typing and give the focus back.
    pub(super) fn stop_typing(
        &mut self,
        commit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let return_to = self
            .typing
            .as_mut()
            .and_then(|typing| typing.return_to.take());
        if commit {
            self.commit_typing(cx);
        }
        self.typing = None;
        match return_to {
            Some(focus) => focus.focus(window, cx),
            None => window.dispatch_action(Box::new(FocusPhoto), cx),
        }
        cx.notify();
    }

    /// Apply what was typed, unless it is unchanged or not a value.
    fn commit_typing(&mut self, cx: &mut Context<Self>) {
        let Some(typing) = self.typing.take() else {
            return;
        };
        cx.notify();
        let Typing {
            adjustment, input, ..
        } = typing;
        let (format, range) = (adjustment.format, adjustment.range());
        let text = input.read(cx).text();
        let Some(value) = format.parse(text, range) else {
            return;
        };
        // The label is rounded: confirming it must not change the value.
        let shown = self
            .value(adjustment.parameter, cx)
            .map(|value| format.label(value, range));
        if shown != Some(format.label(value, range)) {
            self.set(adjustment.parameter, value, EditPhase::Finished, cx);
        }
    }
}

/// Follows a drag wherever the pointer goes.
pub(super) fn drag_tracker(panel: Entity<DevelopPanel>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |_, (), window, _| {
            let moved = panel.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture {
                    moved.update(cx, |panel, cx| panel.drag_to(event, window, cx));
                }
            });
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                    panel.update(cx, |panel, cx| panel.release(window, cx));
                }
            });
        },
    )
    .absolute()
    .size_0()
}
