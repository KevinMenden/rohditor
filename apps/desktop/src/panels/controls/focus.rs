//! Keyboard focus within a panel. What gets it is scrolled into view.

use std::collections::HashMap;
use std::hash::Hash;

use gpui::{
    AnyElement, App, Bounds, Context, Div, FocusHandle, KeyBinding, Pixels, Role, ScrollHandle,
    SharedString, Stateful, Window, canvas, div, point, prelude::*, px,
};

use super::ControlPanel;
use crate::ui::keys::{SelectNext, SelectPrevious};

const CONTEXT: &str = "Adjustments";
/// Space kept around a control that is scrolled to.
const MARGIN: Pixels = px(8.0);

pub struct PanelFocus<C> {
    /// Before the first control, so that the one after it is the first.
    entry: FocusHandle,
    controls: HashMap<C, FocusHandle>,
    scroll: ScrollHandle,
    /// Scroll to this control once it is laid out.
    wanted: Option<C>,
    /// The control last scrolled to, so that scrolling away from it works.
    revealed: Option<FocusHandle>,
}

impl<C: Copy + Eq + Hash> PanelFocus<C> {
    pub fn new(controls: impl IntoIterator<Item = C>, cx: &mut App) -> Self {
        let mut focus = Self {
            entry: cx.focus_handle(),
            controls: HashMap::new(),
            scroll: ScrollHandle::new(),
            wanted: None,
            revealed: None,
        };
        focus.add(controls, cx);
        focus
    }

    /// `controls` can have the focus too. Those that already can keep it.
    pub fn add(&mut self, controls: impl IntoIterator<Item = C>, cx: &mut App) {
        for control in controls {
            self.controls
                .entry(control)
                .or_insert_with(|| cx.focus_handle().tab_stop(true));
        }
    }

    /// Move to the next or previous control, staying in the list at its ends.
    fn step(&self, forward: bool, window: &mut Window, cx: &mut App) {
        let start = window.focused(cx);
        if forward {
            window.focus_next(cx);
        } else {
            window.focus_prev(cx);
        }
        if let Some(start) = start.filter(|_| !self.entry.contains_focused(window, cx)) {
            start.focus(window, cx);
        }
    }

    pub fn control(&self, control: C) -> &FocusHandle {
        &self.controls[&control]
    }

    /// Scroll to the control again when the focus returns to it.
    pub fn forget_lost(&mut self, window: &Window) {
        let lost = |control: &FocusHandle| !control.is_focused(window);
        if self.revealed.as_ref().is_some_and(lost) {
            self.revealed = None;
        }
    }
}

/// The controls are a list, which the vertical arrows move in.
pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, Some(CONTEXT)),
        KeyBinding::new("up", SelectPrevious, Some(CONTEXT)),
    ]);
}

pub trait FocusControls: ControlPanel {
    /// The scrolling list of the controls.
    fn control_list(
        &self,
        id: &'static str,
        label: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let focus = &self.controls().focus;
        div()
            .id(id)
            .role(Role::Group)
            .aria_label(label)
            .key_context(CONTEXT)
            .track_focus(&focus.entry)
            .on_action(cx.listener(|this, _: &SelectNext, window, cx| {
                this.controls().focus.step(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, window, cx| {
                this.controls().focus.step(false, window, cx);
            }))
            .track_scroll(&focus.scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
    }

    /// Focus the first control. If the panel only opens now, the next Tab does.
    fn focus_first(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.controls().focus.entry.focus(window, cx);
        window.focus_next(cx);
    }

    /// Focus the control, and scroll to it also if it is only listed in the next frame.
    fn scroll_to(&mut self, control: Self::Control, window: &mut Window, cx: &mut Context<Self>) {
        let focus = &mut self.controls_mut().focus;
        focus.revealed = None;
        focus.wanted = Some(control);
        focus.control(control).focus(window, cx);
    }

    /// Covers the control and scrolls to it when it gets the focus by key.
    fn revealer(&self, control: Self::Control, cx: &mut Context<Self>) -> AnyElement {
        let focus = &self.controls().focus;
        let wanted = focus.wanted == Some(control);
        self.focus_revealer(focus.control(control).clone(), wanted, cx)
    }

    /// As [`Self::revealer`]; also scrolls to it now if it is `wanted`.
    fn focus_revealer(
        &self,
        control: FocusHandle,
        wanted: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = cx.entity();
        canvas(
            move |bounds, window, cx| {
                if wanted {
                    panel.update(cx, |panel, cx| {
                        panel.controls_mut().focus.wanted = None;
                        reveal(panel, &control, bounds, cx);
                    });
                } else if control.is_focused(window) && window.last_input_was_keyboard() {
                    panel.update(cx, |panel, cx| reveal(panel, &control, bounds, cx));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .into_any_element()
    }
}

impl<P: ControlPanel> FocusControls for P {}

fn reveal<P: ControlPanel>(
    panel: &mut P,
    control: &FocusHandle,
    bounds: Bounds<Pixels>,
    cx: &mut Context<P>,
) {
    let focus = &mut panel.controls_mut().focus;
    if focus.revealed.as_ref() == Some(control) {
        return;
    }
    focus.revealed = Some(control.clone());
    let visible = focus.scroll.bounds();
    let above = visible.top() + MARGIN - bounds.top();
    let below = bounds.bottom() + MARGIN - visible.bottom();
    let change = if above > px(0.0) {
        above
    } else if below > px(0.0) {
        -below
    } else {
        return;
    };
    let offset = focus.scroll.offset();
    let y = (offset.y + change).clamp(-focus.scroll.max_offset().y, px(0.0));
    focus.scroll.set_offset(point(offset.x, y));
    // This frame is already laid out.
    let panel = cx.entity();
    cx.defer(move |cx| panel.update(cx, |_, cx| cx.notify()));
}
