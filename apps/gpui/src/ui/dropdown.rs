//! A control that shows the current choice and opens a menu of all of them.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Bounds, Entity, IntoElement, Pixels, Role, SharedString, Window, canvas, div, prelude::*,
    px,
};

use crate::assets::Icon;
use crate::theme::{RADIUS, theme};
use crate::ui::button::icon;
use crate::ui::menu::MenuEntry;
use crate::ui::overlays::Overlays;
use crate::ui::test_id::TestId as _;

pub fn dropdown(
    id: &'static str,
    label: &'static str,
    value: impl Into<SharedString>,
    overlays: Entity<Overlays>,
    entries: impl Fn(&mut Window, &mut App) -> Vec<MenuEntry> + 'static,
) -> impl IntoElement {
    let theme = theme();
    let value = value.into();
    let bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
    let measured = Rc::clone(&bounds);
    div()
        .id(id)
        .role(Role::ComboBox)
        .aria_label(label)
        .aria_value(value.clone())
        .test_id(format!("dropdown:{id}"))
        .tab_index(0)
        .relative()
        .flex()
        .items_center()
        .gap_1()
        .px_1p5()
        .rounded(RADIUS)
        .border_1()
        .border_color(gpui::transparent_black())
        .focus_visible(move |style| style.border_color(theme.accent))
        .cursor_pointer()
        .hover(move |style| style.bg(theme.hover))
        .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
            window.prevent_default();
        })
        .child(value)
        .child(icon(Icon::ChevronDown, theme.faint))
        .child(
            canvas(move |bounds, _, _| measured.set(bounds), |_, _, _, _| {})
                .absolute()
                .size_full(),
        )
        .on_click(move |_, window, cx| {
            let entries = entries(window, cx);
            let below = bounds.get().bottom_left() + gpui::point(px(0.0), px(2.0));
            overlays.update(cx, |overlays, cx| {
                overlays.show_menu(below, entries, window, cx);
            });
        })
}
