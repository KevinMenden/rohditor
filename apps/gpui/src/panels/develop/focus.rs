//! Keyboard focus within the panel. What gets it is scrolled into view.

use std::collections::HashMap;

use gpui::{
    App, Bounds, Context, Entity, FocusHandle, IntoElement, KeyBinding, Pixels, ScrollHandle,
    Window, canvas, point, prelude::*, px,
};

use rohditor_edit::{GradingTones, Parameter, RecipeSection};

use super::DevelopPanel;
use super::choices::Choice;
use crate::panels::adjustments::sections;
use crate::ui::keys::{SelectNext, SelectPrevious};

pub(super) const CONTEXT: &str = "Adjustments";
/// Space kept around a control that is scrolled to.
const MARGIN: Pixels = px(8.0);

/// What in the panel can have the focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Control {
    /// The header of a section.
    Section(RecipeSection),
    Parameter(Parameter),
    Choice(Choice),
    MixerChannels,
    Wheel(GradingTones),
}

pub(super) struct PanelFocus {
    /// Before the first control, so that the one after it is the first.
    pub entry: FocusHandle,
    controls: HashMap<Control, FocusHandle>,
    pub scroll: ScrollHandle,
    /// Scroll to this control once it is laid out.
    wanted: Option<Control>,
    /// The control last scrolled to, so that scrolling away from it works.
    revealed: Option<FocusHandle>,
}

impl PanelFocus {
    pub fn new(cx: &mut Context<DevelopPanel>) -> Self {
        let controls = sections().flat_map(|section| {
            let rows = section.adjustments.iter();
            let rows = rows.map(|row| Control::Parameter(row.parameter));
            std::iter::once(Control::Section(section.section)).chain(rows)
        });
        Self {
            entry: cx.focus_handle(),
            controls: controls
                .chain(Choice::ALL.map(Control::Choice))
                .chain([Control::MixerChannels])
                .chain(GradingTones::ALL.map(Control::Wheel))
                .map(|control| (control, cx.focus_handle().tab_stop(true)))
                .collect(),
            scroll: ScrollHandle::new(),
            wanted: None,
            revealed: None,
        }
    }

    /// Move to the next or previous control, staying in the list at its ends.
    pub fn step(&self, forward: bool, window: &mut Window, cx: &mut App) {
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

    pub fn control(&self, control: Control) -> &FocusHandle {
        &self.controls[&control]
    }
}

/// The controls are a list, which the vertical arrows move in.
pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, Some(CONTEXT)),
        KeyBinding::new("up", SelectPrevious, Some(CONTEXT)),
    ]);
}

impl DevelopPanel {
    /// Focus the first control of the adjustments. If the panel only opens now, the next Tab does.
    pub fn focus_adjustments(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.entry.focus(window, cx);
        window.focus_next(cx);
    }

    /// Focus the control, and scroll to it also if it is only listed in the next frame.
    pub(super) fn scroll_to(
        &mut self,
        control: Control,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus.revealed = None;
        self.focus.wanted = Some(control);
        self.focus.control(control).focus(window, cx);
    }

    /// Covers the control and scrolls to it when it gets the focus by key.
    pub(super) fn revealer(
        &self,
        control: Control,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let panel: Entity<Self> = cx.entity();
        let wanted = self.focus.wanted == Some(control);
        let control = self.focus.control(control).clone();
        canvas(
            move |bounds, window, cx| {
                if wanted {
                    panel.update(cx, |panel, cx| {
                        panel.focus.wanted = None;
                        panel.reveal(&control, bounds, cx);
                    });
                } else if control.is_focused(window) && window.last_input_was_keyboard() {
                    panel.update(cx, |panel, cx| panel.reveal(&control, bounds, cx));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    fn reveal(&mut self, control: &FocusHandle, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        if self.focus.revealed.as_ref() == Some(control) {
            return;
        }
        self.focus.revealed = Some(control.clone());
        let visible = self.focus.scroll.bounds();
        let above = visible.top() + MARGIN - bounds.top();
        let below = bounds.bottom() + MARGIN - visible.bottom();
        let change = if above > px(0.0) {
            above
        } else if below > px(0.0) {
            -below
        } else {
            return;
        };
        let offset = self.focus.scroll.offset();
        let y = (offset.y + change).clamp(-self.focus.scroll.max_offset().y, px(0.0));
        self.focus.scroll.set_offset(point(offset.x, y));
        // This frame is already laid out.
        let panel = cx.entity();
        cx.defer(move |cx| panel.update(cx, |_, cx| cx.notify()));
    }

    /// Scroll to the control again when the focus returns to it.
    pub(super) fn forget_lost_focus(&mut self, window: &Window) {
        let lost = |control: &FocusHandle| !control.is_focused(window);
        if self.focus.revealed.as_ref().is_some_and(lost) {
            self.focus.revealed = None;
        }
    }
}
