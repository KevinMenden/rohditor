//! The empty viewport.

use gpui::{IntoElement, MouseButton, Role, div, prelude::*, px};

use crate::actions::{commands, key_label};
use crate::theme::{RADIUS, theme};

/// Shown while no photo is open: the commands to start with.
pub fn welcome() -> impl IntoElement {
    let theme = theme();
    let starters = ["file: open", "file: open folder", "view: command palette"];
    let rows = commands()
        .into_iter()
        .filter(move |command| starters.contains(&command.name.as_str()))
        .map(move |command| {
            let action = command.action;
            let title = match command.name.as_str() {
                "file: open" => "Open File",
                "file: open folder" => "Open Folder",
                _ => "Command Palette",
            };
            div()
                .id(command.name.clone())
                .role(Role::Button)
                .aria_label(title)
                .tab_index(0)
                .border_1()
                .border_color(gpui::transparent_black())
                .focus_visible(move |style| style.border_color(theme.accent))
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                .flex()
                .justify_between()
                .w(px(260.0))
                .px_3()
                .py_1()
                .rounded(RADIUS)
                .cursor_pointer()
                .hover(move |style| style.bg(theme.hover))
                .child(title)
                .children(
                    command
                        .key
                        .map(|key| div().text_color(theme.faint).child(key_label(key))),
                )
                .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
        });
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_1()
        .text_color(theme.muted)
        .children(rows)
}
