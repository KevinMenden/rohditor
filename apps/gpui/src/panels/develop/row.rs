//! One adjustment: a row that is its own slider.

use gpui::{
    AccessibleAction, App, Bounds, Context, CursorStyle, DispatchPhase, Entity, IntoElement,
    KeyBinding, MouseButton, MouseDownEvent, Pixels, Point, Role, SharedString, Window, actions,
    canvas, div, linear_color_stop, linear_gradient, prelude::*, px, relative, rgb,
};
use rohditor_edit::EditRecipe;

use super::DevelopPanel;
use super::focus::Control;
use crate::panels::adjustments::Adjustment;
use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::keys::{Cancel, Confirm};
use crate::ui::menu::MenuEntry;
use crate::ui::slider::{Ramp, fraction};
use crate::ui::test_id::TestId as _;

actions!(
    slider,
    [Decrease, Increase, DecreaseMore, IncreaseMore, ResetValue]
);

const CONTEXT: &str = "Slider";
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

impl DevelopPanel {
    fn show_menu(
        &self,
        adjustment: Adjustment,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.entity();
        let typed = panel.clone();
        let entries = vec![
            MenuEntry::new("Type Value", move |window, cx| {
                typed.update(cx, |panel, cx| panel.start_typing(adjustment, window, cx));
            })
            .key(Some("Enter".into())),
            MenuEntry::new("Reset", move |_, cx| {
                panel.update(cx, |panel, cx| panel.reset(adjustment, cx));
            })
            .key(Some("Delete".into())),
        ];
        self.overlays.update(cx, |overlays, cx| {
            overlays.show_menu(at, entries, window, cx);
        });
    }

    pub(super) fn row(
        &self,
        adjustment: Adjustment,
        recipe: &EditRecipe,
        defaults: &EditRecipe,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = theme();
        let Adjustment {
            parameter,
            label,
            format,
            ramp,
        } = adjustment;
        let range = adjustment.range();
        let control = Control::Parameter(parameter);
        let value = parameter.get(recipe, defaults);
        let default = parameter.get(defaults, defaults);
        let at = |value| fraction(format, range, value);
        let (from, to) = (at(default).min(at(value)), at(default).max(at(value)));
        let edited = parameter.is_changed(recipe, defaults);
        let active = self
            .drag
            .is_some_and(|drag| drag.adjustment.parameter == parameter);
        let panel = cx.entity();
        let typing = self
            .typing
            .as_ref()
            .filter(|typing| typing.adjustment.parameter == parameter)
            .map(|typing| typing.input.clone());
        // The input gets the presses and keys while typing.
        let idle = typing.is_none();
        div()
            .id(SharedString::from(format!("{parameter:?}")))
            .role(Role::Slider)
            .aria_label(label)
            .aria_value(format.label(value, range))
            .aria_numeric_value(f64::from(value))
            .aria_min_numeric_value(f64::from(range.minimum))
            .aria_max_numeric_value(f64::from(range.maximum))
            .aria_numeric_value_step(format.step())
            .on_a11y_action(AccessibleAction::Increment, {
                let panel = cx.entity();
                move |_, _, cx| panel.update(cx, |panel, cx| panel.nudge(adjustment, 1, cx))
            })
            .on_a11y_action(AccessibleAction::Decrement, {
                let panel = cx.entity();
                move |_, _, cx| panel.update(cx, |panel, cx| panel.nudge(adjustment, -1, cx))
            })
            .when(idle, |row| row.key_context(CONTEXT))
            .track_focus(self.focus.control(control))
            .child(self.revealer(control, cx))
            .border_1()
            .border_color(gpui::transparent_black())
            // Also after a press, as it shows what the arrow keys change.
            .focus(move |style| style.border_color(theme.accent))
            .on_action(cx.listener(move |this, _: &Decrease, _, cx| this.nudge(adjustment, -1, cx)))
            .on_action(cx.listener(move |this, _: &Increase, _, cx| this.nudge(adjustment, 1, cx)))
            .on_action(cx.listener(move |this, _: &DecreaseMore, _, cx| {
                this.nudge(adjustment, -MANY_STEPS, cx);
            }))
            .on_action(cx.listener(move |this, _: &IncreaseMore, _, cx| {
                this.nudge(adjustment, MANY_STEPS, cx);
            }))
            .on_action(cx.listener(move |this, _: &ResetValue, _, cx| this.reset(adjustment, cx)))
            // Enter on the slider starts typing, Enter in its input ends it.
            .on_action(cx.listener(move |this, _: &Confirm, window, cx| {
                if this.typing.is_some() {
                    this.stop_typing(true, window, cx);
                } else {
                    this.start_typing(adjustment, window, cx);
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
            .test_id(format!("adjustment:{parameter:?}"))
            .relative()
            .flex()
            .items_center()
            .justify_between()
            .h(px(24.0))
            .px_2()
            .rounded(RADIUS)
            .overflow_hidden()
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
                    .text_color(if edited { theme.text } else { theme.muted })
                    .child(label),
            )
            .child(
                div()
                    .relative()
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
                    this.show_menu(adjustment, event.position, window, cx);
                }),
            )
            .when(idle, |row| row.child(press_area(panel, adjustment)))
    }
}

/// The colours a value leads to, along the bottom of its row.
fn track(ramp: Ramp, neutral: f32) -> impl IntoElement {
    let [start, middle, end] = ramp.0.map(rgb);
    let half = |from: gpui::Rgba, to: gpui::Rgba, width| {
        div().h_full().w(relative(width)).bg(linear_gradient(
            90.0,
            linear_color_stop(from, 0.0),
            linear_color_stop(to, 1.0),
        ))
    };
    div()
        .absolute()
        .bottom_0()
        .left_0()
        .w_full()
        .h(px(2.0))
        .flex()
        .child(half(start, middle, neutral))
        .child(half(middle, end, 1.0 - neutral))
}

/// Covers a row and reports presses together with the row's width.
fn press_area(panel: Entity<DevelopPanel>, adjustment: Adjustment) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
        move |bounds: Bounds<Pixels>, hitbox, window, _| {
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble
                    && event.button == MouseButton::Left
                    && hitbox.is_hovered(window)
                {
                    panel.update(cx, |panel, cx| {
                        panel.press(adjustment, event, bounds, cx);
                    });
                }
            });
        },
    )
    .absolute()
    .size_full()
    .top_0()
    .left_0()
}
