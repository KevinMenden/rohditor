//! An on/off switch with its label.

use gpui::{App, Div, Role, SharedString, Stateful, Toggled, Window, div, prelude::*, px};

use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::test_id::TestId as _;

/// A switch followed by its label.
pub fn toggle(
    id: &'static str,
    label: impl Into<SharedString>,
    on: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let label = label.into();
    switch(id, label.clone(), on, on_click)
        .gap_2()
        .px_1()
        .child(label)
}

/// An on/off switch alone, for where its label is shown elsewhere. `label` is what screen
/// readers say.
pub fn switch(
    id: &'static str,
    label: impl Into<SharedString>,
    on: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let theme = theme();
    let track = if on { theme.accent } else { theme.selected };
    let knob = if on { px(11.0) } else { px(1.0) };
    div()
        .id(id)
        .role(Role::Switch)
        .aria_label(label.into())
        .aria_toggled(if on { Toggled::True } else { Toggled::False })
        .test_id(format!("toggle:{id}"))
        .tab_index(0)
        .flex()
        .items_center()
        .h(px(24.0))
        .rounded(RADIUS)
        .border_1()
        .border_color(gpui::transparent_black())
        .focus_visible(move |style| style.border_color(theme.accent))
        .cursor_pointer()
        .text_size(SMALL_FONT_SIZE)
        .text_color(if on { theme.text } else { theme.muted })
        .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
            window.prevent_default();
        })
        .child(
            div()
                .relative()
                .flex_none()
                .w(px(22.0))
                .h(px(12.0))
                .rounded_full()
                .bg(track)
                .child(
                    div()
                        .absolute()
                        .top(px(1.0))
                        .left(knob)
                        .size(px(10.0))
                        .rounded_full()
                        .bg(theme.text),
                ),
        )
        .on_click(move |_, window, cx| on_click(window, cx))
}
