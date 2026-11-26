//! One adjustment: a row that is its own slider.

use gpui::{
    AccessibleAction, App, Bounds, Context, CursorStyle, DispatchPhase, Div, Entity, IntoElement,
    KeyBinding, MouseButton, MouseDownEvent, Pixels, Point, Role, SharedString, Stateful, Window,
    actions, canvas, div, linear_color_stop, linear_gradient, prelude::*, px, relative, rgb,
};

use super::{ControlPanel, FocusControls as _, Gestures as _, Slider};
use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::keys::{Cancel, Confirm};
use crate::ui::menu::MenuEntry;
use crate::ui::slider::{Ramp, fraction};
use crate::ui::test_id::TestId as _;

actions!(
    slider,
    [Decrease, Increase, DecreaseMore, IncreaseMore, ResetValue]
);

pub const CONTEXT: &str = "Slider";
/// Steps of Page Up and Page Down.
const MANY_STEPS: i8 = 10;

pub(super) fn bind_keys(cx: &mut App) {
    let slider = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("left", Decrease, slider),
        KeyBinding::new("right", Increase, slider),
        KeyBinding::new("pagedown", DecreaseMore, slider),
        KeyBinding::new("pageup", IncreaseMore, slider),
        KeyBinding::new("delete", ResetValue, slider),
        KeyBinding::new("backspace", ResetValue, slider),
        KeyBinding::new("enter", Confirm, slider),
    ]);
}

/// What a row shows of its slider.
#[derive(Debug, Clone, Copy)]
pub struct RowState {
    pub value: f32,
    /// Where the filled part starts.
    pub origin: f32,
    pub edited: bool,
    /// Changing it does something.
    pub effective: bool,
}

pub trait SliderRows: ControlPanel {
    fn show_slider_menu(
        &self,
        control: Self::Control,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.entity();
        let typed = panel.clone();
        let entries = vec![
            MenuEntry::new("Type Value", move |window, cx| {
                typed.update(cx, |panel, cx| panel.start_typing(control, window, cx));
            })
            .key(Some("Enter".into())),
            MenuEntry::new("Reset", move |_, cx| {
                panel.update(cx, |panel, cx| panel.reset(control, cx));
            })
            .key(Some("Delete".into())),
        ];
        self.overlays().clone().update(cx, |overlays, cx| {
            overlays.show_menu(at, entries, window, cx);
        });
    }

    fn slider_row(
        &self,
        control: Self::Control,
        id: &str,
        slider: Slider,
        state: RowState,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = theme();
        let Slider {
            label,
            format,
            range,
            ramp,
        } = slider;
        let RowState {
            value,
            origin,
            edited,
            effective,
        } = state;
        let at = |value| fraction(format, range, value);
        let (from, to) = (at(origin).min(at(value)), at(origin).max(at(value)));
        let active = self.controls().dragged() == Some(control);
        let typing = self
            .controls()
            .typing
            .as_ref()
            .filter(|typing| typing.control == control)
            .map(|typing| typing.input.clone());
        // The input gets the presses and keys while typing.
        let idle = typing.is_none();
        let nudge = |steps: i8| {
            let panel = cx.entity();
            move |_: &mut Window, cx: &mut App| {
                panel.update(cx, |panel, cx| panel.nudge(control, steps, cx));
            }
        };
        let (increase, decrease) = (nudge(1), nudge(-1));
        let (increase_more, decrease_more) = (nudge(MANY_STEPS), nudge(-MANY_STEPS));
        let (increment, decrement) = (nudge(1), nudge(-1));
        div()
            .id(SharedString::from(id.to_owned()))
            .role(Role::Slider)
            .aria_label(label.clone())
            .aria_value(format.label(value, range))
            .aria_numeric_value(f64::from(value))
            .aria_min_numeric_value(f64::from(range.minimum))
            .aria_max_numeric_value(f64::from(range.maximum))
            .aria_numeric_value_step(format.step())
            .on_a11y_action(AccessibleAction::Increment, move |_, window, cx| {
                increment(window, cx);
            })
            .on_a11y_action(AccessibleAction::Decrement, move |_, window, cx| {
                decrement(window, cx);
            })
            .when(idle, |row| row.key_context(CONTEXT))
            .track_focus(self.controls().focus.control(control))
            .child(self.revealer(control, cx))
            .border_1()
            .border_color(gpui::transparent_black())
            // Also after a press, as it shows what the arrow keys change.
            .focus(move |style| style.border_color(theme.accent))
            .on_action(move |_: &Decrease, window, cx| decrease(window, cx))
            .on_action(move |_: &Increase, window, cx| increase(window, cx))
            .on_action(move |_: &DecreaseMore, window, cx| decrease_more(window, cx))
            .on_action(move |_: &IncreaseMore, window, cx| increase_more(window, cx))
            .on_action(cx.listener(move |this, _: &ResetValue, _, cx| this.reset(control, cx)))
            // Enter on the slider starts typing, Enter in its input ends it.
            .on_action(cx.listener(move |this, _: &Confirm, window, cx| {
                if this.controls().typed().is_some() {
                    this.stop_typing(true, window, cx);
                } else {
                    this.start_typing(control, window, cx);
                }
            }))
            .when(!idle, |row| {
                row.on_action(cx.listener(|this, _: &Cancel, window, cx| {
                    this.stop_typing(false, window, cx);
                }))
                .capture_any_mouse_down(cx.listener(|this, event, window, cx| {
                    this.press_while_typing(event, window, cx);
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        window.prevent_default();
                        this.stop_typing(true, window, cx);
                    }),
                )
            })
            .test_id(format!("adjustment:{id}"))
            .relative()
            .flex()
            .items_center()
            .justify_between()
            .h(px(24.0))
            .px_2()
            .rounded(RADIUS)
            .overflow_hidden()
            .when(!effective, |row| row.opacity(0.5))
            .bg(if active { theme.hover } else { theme.element })
            .hover(move |style| style.bg(theme.hover))
            .cursor(CursorStyle::ResizeLeftRight)
            .children(ramp.map(|ramp| track(ramp, at(range.neutral))))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(from))
                    .w(relative(to - from))
                    .bg(theme.fill),
            )
            // Marks the value, so that a slider at its default still shows its position.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(at(value).min(0.993)))
                    .w(px(2.0))
                    .bg(if edited || active {
                        theme.accent
                    } else {
                        theme.selected
                    }),
            )
            .child(
                div()
                    .relative()
                    .min_w_0()
                    .truncate()
                    .text_color(if edited { theme.text } else { theme.muted })
                    .child(label),
            )
            .child(
                div()
                    .relative()
                    .flex_none()
                    .text_size(SMALL_FONT_SIZE)
                    .text_color(if edited { theme.text } else { theme.faint })
                    .map(|slot| match typing {
                        Some(input) => slot
                            .w(px(72.0))
                            .px_1()
                            .rounded(px(2.0))
                            .bg(theme.editor)
                            .text_color(theme.text)
                            .child(input),
                        None => slot.child(format.label(value, range)),
                    }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.show_slider_menu(control, event.position, window, cx);
                }),
            )
            .when(idle, |row| row.child(press_area(cx.entity(), control)))
    }
}

impl<P: ControlPanel> SliderRows for P {}

/// The colours a value leads to, along the bottom of its row.
fn track(ramp: Ramp, neutral: f32) -> impl IntoElement {
    let stops = ramp.stops(neutral);
    let parts: Vec<_> = stops
        .windows(2)
        .map(|ends| {
            let [(from, start), (to, end)] = [ends[0], ends[1]];
            div().h_full().w(relative(to - from)).bg(linear_gradient(
                90.0,
                linear_color_stop(rgb(start), 0.0),
                linear_color_stop(rgb(end), 1.0),
            ))
        })
        .collect();
    div()
        .absolute()
        .bottom_0()
        .left_0()
        .w_full()
        .h(px(2.0))
        .flex()
        .children(parts)
}

/// Covers a control and reports presses together with its bounds.
pub fn press_area<P: ControlPanel>(panel: Entity<P>, control: P::Control) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
        move |bounds: Bounds<Pixels>, hitbox, window, _| {
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble
                    && event.button == MouseButton::Left
                    && hitbox.is_hovered(window)
                {
                    panel.update(cx, |panel, cx| panel.press(control, event, bounds, cx));
                }
            });
        },
    )
    .absolute()
    .size_full()
    .top_0()
    .left_0()
}
