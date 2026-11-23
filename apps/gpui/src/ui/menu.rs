//! A list of things to do, shown by [`super::overlays`] as a dropdown or context menu.

use std::rc::Rc;

use gpui::{
    App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, IntoElement, Render, Role,
    SharedString, Window, div, prelude::*, px,
};

use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::keys::{Confirm, SelectNext, SelectPrevious};
use crate::ui::test_id::TestId as _;

pub const CONTEXT: &str = "Menu";

type Run = Rc<dyn Fn(&mut Window, &mut App)>;

pub struct MenuEntry {
    label: SharedString,
    key: Option<SharedString>,
    /// The current choice of a dropdown.
    checked: bool,
    run: Run,
}

impl MenuEntry {
    pub fn new(
        label: impl Into<SharedString>,
        run: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            key: None,
            checked: false,
            run: Rc::new(run),
        }
    }

    pub fn key(mut self, key: Option<SharedString>) -> Self {
        self.key = key;
        self
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }
}

pub struct Menu {
    focus: FocusHandle,
    entries: Vec<MenuEntry>,
    selected: Option<usize>,
}

impl EventEmitter<DismissEvent> for Menu {}

impl Focusable for Menu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Menu {
    pub fn new(entries: Vec<MenuEntry>, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            selected: entries.iter().position(|entry| entry.checked),
            entries,
        }
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = Some(index);
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let next = self.selected.map_or(0, |index| index + 1);
        self.select(next % self.entries.len(), cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.entries.len();
        self.select((self.selected.unwrap_or(0) + count - 1) % count, cx);
    }

    /// Close first, so that the entry runs with the focus back where it was.
    fn run(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let run = Rc::clone(&self.entries[index].run);
        cx.emit(DismissEvent);
        window.defer(cx, move |window, cx| run(window, cx));
    }

    fn entry(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = theme();
        let entry = &self.entries[index];
        let selected = self.selected == Some(index);
        div()
            .id(index)
            .role(Role::MenuItem)
            .aria_label(entry.label.clone())
            .when(selected, |row| row.aria_active_descendant())
            .test_id(format!("menu:{}", entry.label))
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .h(px(26.0))
            .px_2()
            .rounded(RADIUS)
            .cursor_pointer()
            .when(selected, |row| row.bg(theme.selected))
            .when(entry.checked, |row| row.text_color(theme.accent))
            .child(entry.label.clone())
            .children(entry.key.clone().map(|key| {
                div()
                    .text_size(SMALL_FONT_SIZE)
                    .text_color(theme.faint)
                    .child(key)
            }))
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if this.selected != Some(index) {
                    this.select(index, cx);
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| this.run(index, window, cx)))
    }
}

impl Render for Menu {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme();
        let entries: Vec<_> = (0..self.entries.len())
            .map(|index| self.entry(index, cx))
            .collect();
        div()
            .id("menu")
            .role(Role::Menu)
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| {
                if let Some(index) = this.selected {
                    this.run(index, window, cx);
                }
            }))
            .occlude()
            .min_w(px(160.0))
            .p_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.elevated)
            .shadow_lg()
            .children(entries)
    }
}
