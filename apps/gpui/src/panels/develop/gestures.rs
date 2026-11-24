//! Changing a value: dragging, arrow keys, typing and resetting.

use std::time::Duration;

use gpui::{
    Context, DispatchPhase, Entity, FocusHandle, Focusable, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Subscription, Task, Window, canvas,
    prelude::*, px,
};

use super::DevelopPanel;
use crate::actions::FocusPhoto;
use crate::panels::adjustments::Adjustment;
use crate::session::EditPhase;
use crate::ui::slider::dragged;
use crate::ui::text_input::TextInput;

const FINE_DRAG: f32 = 0.1;
/// A press this close to a row's right end is on its value.
const VALUE_WIDTH: Pixels = px(72.0);
/// Arrow key presses closer together than this are one undo step.
const NUDGE_PAUSE: Duration = Duration::from_millis(600);

#[derive(Clone, Copy)]
pub(super) struct Drag {
    pub adjustment: Adjustment,
    last_x: Pixels,
    value: f32,
    width: Pixels,
    moved: bool,
    on_value: bool,
}

/// Arrow key changes that are not an undo step yet.
pub(super) struct Nudge {
    adjustment: Adjustment,
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
    fn value(&self, adjustment: Adjustment, cx: &Context<Self>) -> Option<f32> {
        let session = self.session.read(cx);
        let document = session.document().filter(|open| open.frame.is_some())?;
        Some(adjustment.get(document.edits.recipe(), session.as_shot()))
    }

    fn set(&self, adjustment: Adjustment, value: f32, phase: EditPhase, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| {
            let as_shot = session.as_shot();
            session.edit(
                |recipe| adjustment.set(recipe, as_shot, value),
                phase,
                adjustment.is_white_balance(),
                cx,
            );
        });
    }

    pub(super) fn reset(&mut self, adjustment: Adjustment, cx: &mut Context<Self>) {
        self.finish_nudge(cx);
        let value = adjustment.default_value(self.session.read(cx).as_shot());
        self.set(adjustment, value, EditPhase::Finished, cx);
    }

    pub(super) fn press(
        &mut self,
        adjustment: Adjustment,
        event: &MouseDownEvent,
        row: gpui::Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.finish_nudge(cx);
        if event.click_count == 2 {
            self.drag = None;
            return self.reset(adjustment, cx);
        }
        let Some(value) = self.value(adjustment, cx) else {
            return;
        };
        self.session
            .update(cx, |session, _| session.begin_gesture());
        self.drag = Some(Drag {
            adjustment,
            last_x: event.position.x,
            value,
            width: row.size.width,
            moved: false,
            on_value: row.right() - event.position.x < VALUE_WIDTH,
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
        if event.position.x == drag.last_x {
            return;
        }
        let speed = if event.modifiers.shift {
            FINE_DRAG
        } else {
            1.0
        };
        let delta = (event.position.x - drag.last_x) / drag.width * speed;
        drag.last_x = event.position.x;
        drag.moved = true;
        let adjustment = drag.adjustment;
        drag.value = dragged(adjustment.format(), adjustment.range(), drag.value, delta);
        let value = drag.value;
        self.set(adjustment, value, EditPhase::Dragging, cx);
    }

    fn release(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        self.session.update(cx, |session, cx| {
            session.finish_gesture(drag.adjustment.is_white_balance(), cx);
        });
        if drag.on_value && !drag.moved {
            self.start_typing(drag.adjustment, window, cx);
            if let Some(typing) = self.typing.as_mut() {
                typing.just_clicked = true;
            }
        }
        cx.notify();
    }

    /// Change the value by `steps` arrow key steps.
    pub(super) fn nudge(&mut self, adjustment: Adjustment, steps: i8, cx: &mut Context<Self>) {
        let same = |nudge: &Nudge| nudge.adjustment == adjustment;
        if !self.nudge.as_ref().is_some_and(same) {
            self.finish_nudge(cx);
        }
        let Some(value) = self.value(adjustment, cx) else {
            return;
        };
        let next = adjustment
            .format()
            .stepped(adjustment.range(), value, steps);
        self.set(adjustment, next, EditPhase::Dragging, cx);
        let pause = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NUDGE_PAUSE).await;
            // Gone only while the application quits.
            let _ = this.update(cx, Self::finish_nudge);
        });
        self.nudge = Some(Nudge {
            adjustment,
            _pause: pause,
        });
    }

    fn finish_nudge(&mut self, cx: &mut Context<Self>) {
        if let Some(nudge) = self.nudge.take() {
            self.session.update(cx, |session, cx| {
                session.finish_gesture(nudge.adjustment.is_white_balance(), cx);
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
        let Some(value) = self.value(adjustment, cx) else {
            return;
        };
        let text = adjustment.format().label(value, adjustment.range());
        let input = cx.new(|cx| TextInput::new(adjustment.label(), "", text, cx));
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
        let (format, range) = (adjustment.format(), adjustment.range());
        let text = input.read(cx).text();
        let Some(value) = format.parse(text, range) else {
            return;
        };
        // The label is rounded: confirming it must not change the value.
        let shown = self
            .value(adjustment, cx)
            .map(|value| format.label(value, range));
        if shown != Some(format.label(value, range)) {
            self.set(adjustment, value, EditPhase::Finished, cx);
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
