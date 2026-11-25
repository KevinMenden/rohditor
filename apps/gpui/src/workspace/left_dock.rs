//! The left dock: the file tree and the albums, as tabs.

use gpui::{Action, Context, IntoElement, MouseButton, Role, StyleRefinement, div, prelude::*};

use super::Workspace;
use crate::actions::{NewAlbum, OpenFolder, ShowAlbums, ShowFiles};
use crate::assets::Icon;
use crate::theme::{FILES_WIDTH, RADIUS, TAB_HEIGHT, theme};
use crate::ui::button::{command_button, icon};
use crate::ui::test_id::TestId as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LeftTab {
    Files,
    Albums,
}

impl Workspace {
    pub(super) fn show_left(&mut self, tab: LeftTab, cx: &mut Context<Self>) {
        self.files_open = true;
        self.left_tab = tab;
        cx.notify();
    }

    pub(super) fn left_dock(&self) -> impl IntoElement + use<> {
        let theme = theme();
        let files = self.left_tab == LeftTab::Files;
        let style = StyleRefinement::default().flex_1().min_h_0().w_full();
        let panel = if files {
            self.files.clone().cached(style).into_any_element()
        } else {
            self.albums.clone().cached(style).into_any_element()
        };
        let add = if files {
            command_button("add-folder", Icon::Plus, OpenFolder, true)
        } else {
            command_button("add-album", Icon::Plus, NewAlbum, true)
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(FILES_WIDTH)
            .h_full()
            .bg(theme.panel)
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .id("left-tabs")
                    .role(Role::TabList)
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .h(TAB_HEIGHT)
                    .px_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(tab("Files", Icon::Files, files, &ShowFiles))
                    .child(tab("Albums", Icon::Album, !files, &ShowAlbums))
                    .child(div().flex_1())
                    .child(add),
            )
            .child(panel)
    }
}

fn tab(
    label: &'static str,
    glyph: Icon,
    selected: bool,
    action: &dyn Action,
) -> impl IntoElement + use<> {
    let theme = theme();
    let action = action.boxed_clone();
    div()
        .id(label)
        .role(Role::Tab)
        .aria_label(label)
        .aria_selected(selected)
        .test_id(format!("tab:{label}"))
        .tab_index(0)
        .relative()
        .flex()
        .items_center()
        .gap_1()
        .px_2()
        .py_0p5()
        .rounded(RADIUS)
        .border_1()
        .border_color(gpui::transparent_black())
        .focus_visible(move |style| style.border_color(theme.accent))
        .cursor_pointer()
        .text_color(if selected { theme.text } else { theme.muted })
        .when(selected, |tab| tab.bg(theme.selected))
        .when(!selected, |tab| {
            tab.hover(move |style| style.bg(theme.hover))
        })
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .child(icon(glyph, if selected { theme.text } else { theme.muted }))
        .child(label)
        .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
}
