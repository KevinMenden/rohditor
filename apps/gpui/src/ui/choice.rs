//! One choice out of a few, with every option in view.
//!
//! A dropdown is not an option inside a dialog: [`super::overlays`] shows one menu or dialog at a
//! time, so opening a menu would close the dialog.

use std::rc::Rc;

use gpui::{App, Div, Role, SharedString, Stateful, Window, div, prelude::*};

use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::test_id::TestId as _;

/// A row of buttons of which `current` is pressed. `id` names the group and its options.
pub fn choice<T: Copy + PartialEq + 'static>(
    id: &'static str,
    label: &'static str,
    options: &[(T, &'static str)],
    current: T,
    on_select: impl Fn(T, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let theme = theme();
    let on_select = Rc::new(on_select);
    let segments = options.iter().map(|&(value, name)| {
        let selected = value == current;
        let on_select = Rc::clone(&on_select);
        div()
            .id(SharedString::from(format!("{id}:{name}")))
            .role(Role::RadioButton)
            .aria_label(name)
            .aria_selected(selected)
            .test_id(format!("choice:{id}:{name}"))
            .tab_index(0)
            .px_2()
            .py_0p5()
            .rounded(RADIUS)
            .border_1()
            .border_color(gpui::transparent_black())
            .focus_visible(move |style| style.border_color(theme.accent))
            .cursor_pointer()
            .text_size(SMALL_FONT_SIZE)
            .when(selected, |segment| {
                segment.bg(theme.selected).text_color(theme.text)
            })
            .when(!selected, |segment| {
                segment
                    .text_color(theme.muted)
                    .hover(move |style| style.bg(theme.hover))
            })
            .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                window.prevent_default();
            })
            .on_click(move |_, window, cx| on_select(value, window, cx))
            .child(name)
    });
    div()
        .id(id)
        .role(Role::RadioGroup)
        .aria_label(label)
        .flex()
        .flex_wrap()
        .gap_1()
        .children(segments.collect::<Vec<_>>())
}
