//! Changing a value: dragging, arrow keys, typing and resetting.

use std::time::Duration;

use gpui::{
    Bounds, Context, DispatchPhase, Entity, FocusHandle, Focusable, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Size, Subscription, Task, Window,
    canvas, prelude::*, px,
};

use super::ControlPanel;
use crate::actions::FocusPhoto;
use crate::session::EditPhase;
use crate::ui::slider::dragged;
use crate::ui::text_input::TextInput;

const FINE_DRAG: f32 = 0.1;
/// A press this close to a slider's right end is on its value.
const VALUE_WIDTH: Pixels = px(72.0);
/// Arrow key presses closer together than this are one undo step.
const NUDGE_PAUSE: Duration = Duration::from_millis(600);

pub(super) struct Drag<C> {
    pub control: C,
    last: Point<Pixels>,
    value: f32,
    /// Of the control.
    size: Size<Pixels>,
    moved: bool,
    on_value: bool,
}

/// Arrow key changes that are not an undo step yet.
pub(super) struct Nudge<C> {
    control: C,
    _pause: Task<()>,
}

/// A value being typed.
pub(super) struct Typing<C> {
    pub control: C,
    pub input: Entity<TextInput>,
    return_to: Option<FocusHandle>,
    /// The last press was the click that opened it, so that one more makes a double click.
    just_clicked: bool,
    _blurred: Subscription,
}

pub trait Gestures: ControlPanel {
    /// The left button went down on the control, which covers `area`.
    fn press(
        &mut self,
        control: Self::Control,
        event: &MouseDownEvent,
        area: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.finish_nudge(cx);
        if event.click_count == 2 {
            self.controls_mut().drag = None;
            return self.reset(control, cx);
        }
        let Some(value) = self.value(control, cx) else {
            return;
        };
        self.session()
            .clone()
            .update(cx, |session, _| session.begin_gesture());
        let on_value =
            self.slider(control).is_some() && area.right() - event.position.x < VALUE_WIDTH;
        self.controls_mut().drag = Some(Drag {
            control,
            last: event.position,
            value,
            size: area.size,
            moved: false,
            on_value,
        });
        cx.notify();
    }

    fn drag_to(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.controls_mut().drag.as_mut() else {
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
        let speed = if event.modifiers.shift {
            FINE_DRAG
        } else {
            1.0
        };
        drag.last = event.position;
        let control = drag.control;
        let slider = self.slider(control);
        if right == 0.0 && (up == 0.0 || slider.is_some()) {
            return;
        }
        let Some(drag) = self.controls_mut().drag.as_mut() else {
            return;
        };
        drag.moved = true;
        match slider {
            Some(slider) => {
                drag.value = dragged(slider.format, slider.range, drag.value, right * speed);
                let value = drag.value;
                self.set_value(control, value, EditPhase::Dragging, cx);
            }
            None => self.move_by(control, right * speed, up * speed, cx),
        }
    }

    fn release(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.controls_mut().drag.take() else {
            return;
        };
        self.finish_gesture(drag.control, cx);
        if drag.on_value && !drag.moved {
            self.start_typing(drag.control, window, cx);
            if let Some(typing) = self.controls_mut().typing.as_mut() {
                typing.just_clicked = true;
            }
        }
        cx.notify();
    }

    /// Change a slider by steps, as the arrow keys do.
    fn nudge(&mut self, control: Self::Control, steps: i8, cx: &mut Context<Self>) {
        self.hold_nudge(control, cx);
        if let Some(slider) = self.slider(control)
            && let Some(value) = self.value(control, cx)
        {
            let next = slider.format.stepped(slider.range, value, steps);
            self.set_value(control, next, EditPhase::Dragging, cx);
        }
    }

    /// The changes to the control until a pause are one undo step.
    fn hold_nudge(&mut self, control: Self::Control, cx: &mut Context<Self>) {
        let nudge = &self.controls().nudge;
        if nudge.as_ref().is_none_or(|nudge| nudge.control != control) {
            self.finish_nudge(cx);
        }
        let pause = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NUDGE_PAUSE).await;
            // Gone only while the application quits.
            let _ = this.update(cx, Self::finish_nudge);
        });
        self.controls_mut().nudge = Some(Nudge {
            control,
            _pause: pause,
        });
    }

    fn finish_nudge(&mut self, cx: &mut Context<Self>) {
        if let Some(nudge) = self.controls_mut().nudge.take() {
            self.finish_gesture(nudge.control, cx);
        }
    }

    fn start_typing(
        &mut self,
        control: Self::Control,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish_nudge(cx);
        let Some(slider) = self.slider(control) else {
            return;
        };
        let Some(value) = self.value(control, cx) else {
            return;
        };
        let text = slider.format.label(value, slider.range);
        let input = cx.new(|cx| TextInput::new(slider.label, "", text, cx));
        let focus = input.read(cx).focus_handle(cx);
        let return_to = window.focused(cx);
        focus.focus(window, cx);
        let blurred = cx.on_blur(&focus, window, |panel, _, cx| panel.commit_typing(cx));
        self.controls_mut().typing = Some(Typing {
            control,
            input,
            return_to,
            just_clicked: false,
            _blurred: blurred,
        });
        cx.notify();
    }

    /// A press on the slider of the value being typed, before its input gets it. The second click
    /// of a double click on the value resets it, as anywhere on the slider.
    fn press_while_typing(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(typing) = self.controls_mut().typing.as_mut() else {
            return;
        };
        let just_clicked = std::mem::take(&mut typing.just_clicked);
        if just_clicked && event.button == MouseButton::Left && event.click_count == 2 {
            let control = typing.control;
            window.prevent_default();
            cx.stop_propagation();
            self.stop_typing(false, window, cx);
            self.reset(control, cx);
        }
    }

    /// Stop typing and give the focus back.
    fn stop_typing(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let return_to = self
            .controls_mut()
            .typing
            .as_mut()
            .and_then(|typing| typing.return_to.take());
        if commit {
            self.commit_typing(cx);
        }
        self.controls_mut().typing = None;
        match return_to {
            Some(focus) => focus.focus(window, cx),
            None => window.dispatch_action(Box::new(FocusPhoto), cx),
        }
        cx.notify();
    }

    /// Forget what is being changed, as it is no longer there.
    fn abandon_gestures(&mut self, cx: &mut Context<Self>) {
        self.finish_nudge(cx);
        let controls = self.controls_mut();
        controls.typing = None;
        if let Some(drag) = controls.drag.take() {
            self.finish_gesture(drag.control, cx);
        }
    }

    /// Apply what was typed, unless it is unchanged or not a value.
    fn commit_typing(&mut self, cx: &mut Context<Self>) {
        let Some(typing) = self.controls_mut().typing.take() else {
            return;
        };
        cx.notify();
        let Some(slider) = self.slider(typing.control) else {
            return;
        };
        let (format, range) = (slider.format, slider.range);
        let Some(value) = format.parse(typing.input.read(cx).text(), range) else {
            return;
        };
        // The label is rounded: confirming it must not change the value.
        let shown = self
            .value(typing.control, cx)
            .map(|value| format.label(value, range));
        if shown != Some(format.label(value, range)) {
            self.set_value(typing.control, value, EditPhase::Finished, cx);
        }
    }
}

impl<P: ControlPanel> Gestures for P {}

/// Follows a drag wherever the pointer goes.
pub fn drag_tracker<P: ControlPanel>(panel: Entity<P>) -> impl IntoElement {
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
