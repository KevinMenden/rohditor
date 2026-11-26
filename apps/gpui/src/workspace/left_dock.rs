//! The left dock: the file tree, the albums and, when asked for, the diagnostics, as tabs.

use gpui::{Action, Context, IntoElement, MouseButton, Role, StyleRefinement, div, prelude::*};

use super::Workspace;
use crate::actions::{NewAlbum, OpenFolder, ShowAlbums, ShowFiles, ToggleDiagnostics};
use crate::assets::Icon;
use crate::theme::{FILES_WIDTH, RADIUS, TAB_HEIGHT, theme};
use crate::ui::button::{command_button, icon};
use crate::ui::test_id::TestId as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LeftTab {
    Files,
    Albums,
    Diagnostics,
}

impl Workspace {
    pub(super) fn show_left(&mut self, tab: LeftTab, cx: &mut Context<Self>) {
        self.files_open = true;
        self.left_tab = tab;
        cx.notify();
    }

    /// Remove the diagnostics tab while it is in view; otherwise add it and show it.
    pub(super) fn toggle_diagnostics(&mut self, cx: &mut Context<Self>) {
        let in_view = self.files_open && self.left_tab == LeftTab::Diagnostics;
        self.diagnostics_open = !in_view;
        self.show_left(
            if in_view {
                LeftTab::Files
            } else {
                LeftTab::Diagnostics
            },
            cx,
        );
    }

    pub(super) fn left_dock(&self) -> impl IntoElement + use<> {
        let theme = theme();
        let current = self.left_tab;
        let style = StyleRefinement::default().flex_1().min_h_0().w_full();
        let panel = match current {
            LeftTab::Files => self.files.clone().cached(style).into_any_element(),
            LeftTab::Albums => self.albums.clone().cached(style).into_any_element(),
            LeftTab::Diagnostics => self.diagnostics.clone().cached(style).into_any_element(),
        };
        let add = match current {
            LeftTab::Files => Some(command_button("add-folder", Icon::Plus, OpenFolder, true)),
            LeftTab::Albums => Some(command_button("add-album", Icon::Plus, NewAlbum, true)),
            LeftTab::Diagnostics => None,
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
                    .child(tab(
                        "Files",
                        Icon::Files,
                        current == LeftTab::Files,
                        &ShowFiles,
                    ))
                    .child(tab(
                        "Albums",
                        Icon::Album,
                        current == LeftTab::Albums,
                        &ShowAlbums,
                    ))
                    .when(self.diagnostics_open, |tabs| {
                        let selected = current == LeftTab::Diagnostics;
                        tabs.child(diagnostics_tab(selected))
                    })
                    .child(div().flex_1())
                    .children(add),
            )
            .child(panel)
    }
}

/// The diagnostics tab is an icon only, for room. Pressing it while it is shown removes it.
fn diagnostics_tab(selected: bool) -> impl IntoElement + use<> {
    command_button("tab-diagnostics", Icon::Activity, ToggleDiagnostics, true)
        .role(Role::Tab)
        .aria_selected(selected)
        .when(selected, |tab| tab.bg(theme().selected))
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
