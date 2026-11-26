//! Flat buttons. A press does not move the focus; Tab does.

use gpui::{
    Action, App, ClickEvent, Hsla, IntoElement, MouseButton, Pixels, Role, SharedString, Stateful,
    Window, div, prelude::*, px, svg,
};

use crate::actions::Command;
use crate::assets::Icon;
use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::test_id::TestId as _;
use crate::ui::tooltip::tooltip;

pub fn icon(icon: Icon, color: Hsla) -> impl IntoElement {
    svg()
        .path(icon.path())
        .size(px(15.0))
        .flex_none()
        .text_color(color)
}

pub fn icon_button(
    id: impl Into<SharedString>,
    glyph: Icon,
    label: impl Into<SharedString>,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<gpui::Div> {
    keyed_icon_button(id.into(), glyph, label.into(), None, enabled, on_click)
}

/// An icon button that runs a command, and says which and its key.
pub fn command_button(
    id: &'static str,
    glyph: Icon,
    action: impl Action,
    enabled: bool,
) -> Stateful<gpui::Div> {
    let command = Command::of(&action);
    let action = action.boxed_clone();
    keyed_icon_button(
        id.into(),
        glyph,
        command.title().into(),
        command.key_label().map(SharedString::from),
        enabled,
        move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx),
    )
}

fn keyed_icon_button(
    id: SharedString,
    glyph: Icon,
    label: SharedString,
    key: Option<SharedString>,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<gpui::Div> {
    let theme = theme();
    let size: Pixels = px(24.0);
    let color = if enabled { theme.muted } else { theme.faint };
    let button = div()
        .id(id.clone())
        .role(Role::Button)
        .aria_label(label.clone())
        .size(size)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(RADIUS)
        .border_1()
        .border_color(gpui::transparent_black())
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.prevent_default();
            // Keep presses on a button from moving the window when it sits in the title bar.
            cx.stop_propagation();
        })
        .tooltip(tooltip(label, key.clone()))
        .when_some(key, |button, key| button.aria_keyshortcuts(key))
        .child(icon(glyph, color))
        .test_id(format!("button:{id}"));
    if enabled {
        button
            .tab_index(0)
            .focus_visible(move |style| style.border_color(theme.accent))
            .cursor_pointer()
            .hover(move |style| style.bg(theme.hover))
            .on_click(on_click)
    } else {
        button
    }
}

/// A button with a text, for dialogs.
pub fn button(
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let theme = theme();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .test_id(format!("button:{id}"))
        .tab_index(0)
        .px_3()
        .py_1()
        .rounded(RADIUS)
        .border_1()
        .border_color(theme.border)
        .bg(theme.element)
        .focus_visible(move |style| style.border_color(theme.accent))
        .cursor_pointer()
        .hover(move |style| style.bg(theme.hover))
        .child(label)
        .on_click(on_click)
}

/// A small text that runs a command, and says which and its key.
pub fn link(
    id: &'static str,
    label: &'static str,
    action: impl Action,
    enabled: bool,
) -> Stateful<gpui::Div> {
    let theme = theme();
    let command = Command::of(&action);
    let key = command.key_label().map(SharedString::from);
    let link = div()
        .id(id)
        .role(Role::Button)
        .aria_label(command.title())
        .test_id(format!("button:{id}"))
        .border_1()
        .border_color(gpui::transparent_black())
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .tooltip(tooltip(command.title(), key))
        .px_1p5()
        .rounded(RADIUS)
        .text_size(SMALL_FONT_SIZE)
        .child(label);
    if enabled {
        link.tab_index(0)
            .focus_visible(move |style| style.border_color(theme.accent))
            .text_color(theme.muted)
            .cursor_pointer()
            .hover(move |style| style.bg(theme.hover).text_color(theme.text))
            .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
    } else {
        link.text_color(theme.faint)
    }
}
