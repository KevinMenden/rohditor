//! Flat icon buttons.

use gpui::{
    App, ClickEvent, Hsla, IntoElement, Pixels, Stateful, Window, div, prelude::*, px, svg,
};

use crate::assets::Icon;
use crate::theme::{RADIUS, theme};
use crate::ui::test_id::TestId as _;

pub fn icon(icon: Icon, color: Hsla) -> impl IntoElement {
    svg()
        .path(icon.path())
        .size(px(15.0))
        .flex_none()
        .text_color(color)
}

pub fn icon_button(
    id: &'static str,
    glyph: Icon,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<gpui::Div> {
    let theme = theme();
    let size: Pixels = px(24.0);
    let color = if enabled { theme.muted } else { theme.faint };
    let button = div()
        .id(id)
        .size(size)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(RADIUS)
        // Keep presses on a button from moving the window when it sits in the title bar.
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(icon(glyph, color))
        .test_id(format!("button:{id}"));
    if enabled {
        button
            .cursor_pointer()
            .hover(move |style| style.bg(theme.hover))
            .on_click(on_click)
    } else {
        button
    }
}
