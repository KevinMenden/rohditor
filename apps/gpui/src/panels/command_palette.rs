//! Command palette: type to filter, Enter to run.

use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render,
    Role, Window, div, prelude::*, px,
};

use crate::actions::{Command, commands};
use crate::assets::Icon;
use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::button::icon;
use crate::ui::keys::{Confirm, SelectNext, SelectPrevious};
use crate::ui::text_input::{Changed, TextInput};

pub struct CommandPalette {
    commands: Vec<Command>,
    query: Entity<TextInput>,
    selected: usize,
}

impl EventEmitter<DismissEvent> for CommandPalette {}

impl Focusable for CommandPalette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

/// Whether the letters of `query` appear in `name` in order, ignoring case and spaces.
fn matches(name: &str, query: &str) -> bool {
    let mut letters = name.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .filter(|letter| !letter.is_whitespace())
        .flat_map(char::to_lowercase)
        .all(|wanted| letters.any(|letter| letter == wanted))
}

impl CommandPalette {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| TextInput::new("Command", "Run a command…", String::new(), cx));
        cx.subscribe(&query, |palette, _, _: &Changed, cx| {
            palette.selected = 0;
            cx.notify();
        })
        .detach();
        Self {
            commands: commands(),
            query,
            selected: 0,
        }
    }

    fn visible(&self, cx: &App) -> Vec<usize> {
        let query = self.query.read(cx).text();
        (0..self.commands.len())
            .filter(|&index| matches(&self.commands[index].name, query))
            .collect()
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(&index) = self.visible(cx).get(self.selected) {
            self.run(index, window, cx);
        }
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = (self.selected + 1) % self.visible(cx).len().max(1);
        cx.notify();
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.visible(cx).len().max(1);
        self.selected = (self.selected + count - 1) % count;
        cx.notify();
    }

    /// Close first, so that the action reaches the workspace the focus returns to.
    fn run(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let action = self.commands[index].action.boxed_clone();
        cx.emit(DismissEvent);
        window.defer(cx, move |window, cx| window.dispatch_action(action, cx));
    }

    fn entry(
        &self,
        index: usize,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = theme();
        let command = &self.commands[index];
        let entry = div()
            .id(index)
            .role(Role::ListBoxOption)
            .aria_label(command.name.clone())
            .aria_selected(selected)
            .flex()
            .items_center()
            .justify_between()
            .h(px(28.0))
            .px_2()
            .rounded(RADIUS)
            .cursor_pointer()
            .child(command.name.clone())
            .children(command.key_label().map(|key| {
                div()
                    .text_size(SMALL_FONT_SIZE)
                    .text_color(theme.faint)
                    .child(key)
            }))
            .on_click(cx.listener(move |this, _, window, cx| this.run(index, window, cx)));
        if selected {
            entry.bg(theme.selected)
        } else {
            entry.hover(move |style| style.bg(theme.hover))
        }
    }
}

impl Render for CommandPalette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme();
        let visible = self.visible(cx);
        let entries: Vec<_> = visible
            .iter()
            .enumerate()
            .map(|(row, &index)| self.entry(index, row == self.selected, cx))
            .collect();
        let empty = entries.is_empty();
        div()
            .id("command-palette")
            .role(Role::Dialog)
            .aria_label("Command palette")
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .w(px(520.0))
            .flex()
            .flex_col()
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.elevated)
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(40.0))
                    .px_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(icon(Icon::Search, theme.muted))
                    .child(self.query.clone()),
            )
            .child(
                div()
                    .id("commands")
                    .role(Role::ListBox)
                    .p_1()
                    .max_h(px(400.0))
                    .overflow_y_scroll()
                    .children(entries)
                    .when(empty, |list| {
                        list.child(div().p_2().text_color(theme.faint).child("No matches"))
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn query_letters_match_in_order() {
        assert!(matches("view: zoom to fit", "zfit"));
        assert!(matches("view: zoom to fit", "Zoom Fit"));
        assert!(!matches("view: zoom to fit", "fitzoom"));
        assert!(matches("edit: undo", ""));
    }
}
