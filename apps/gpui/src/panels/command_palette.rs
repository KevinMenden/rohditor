//! Command palette: type to filter, Enter to run.

use gpui::{
    App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, IntoElement, KeyDownEvent,
    Render, Window, div, prelude::*, px,
};

use crate::actions::{Command, commands, key_label};
use crate::assets::Icon;
use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::button::icon;

pub struct CommandPalette {
    focus: FocusHandle,
    commands: Vec<Command>,
    query: String,
    selected: usize,
}

impl EventEmitter<DismissEvent> for CommandPalette {}

impl Focusable for CommandPalette {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
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
        Self {
            focus: cx.focus_handle(),
            commands: commands(),
            query: String::new(),
            selected: 0,
        }
    }

    fn visible(&self) -> Vec<usize> {
        (0..self.commands.len())
            .filter(|&index| matches(self.commands[index].name, &self.query))
            .collect()
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let visible = self.visible();
        let keystroke = &event.keystroke;
        match keystroke.key.as_str() {
            "escape" => cx.emit(DismissEvent),
            "enter" => {
                if let Some(&index) = visible.get(self.selected) {
                    self.run(index, window, cx);
                }
            }
            "down" => self.selected = (self.selected + 1) % visible.len().max(1),
            "up" => {
                let count = visible.len().max(1);
                self.selected = (self.selected + count - 1) % count;
            }
            "backspace" => {
                self.query.pop();
                self.selected = 0;
            }
            _ => {
                let typed = keystroke.key_char.as_ref();
                let shortcut = keystroke.modifiers.control || keystroke.modifiers.alt;
                if let Some(typed) = typed.filter(|_| !shortcut) {
                    self.query.push_str(typed);
                    self.selected = 0;
                }
            }
        }
        cx.stop_propagation();
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
            .flex()
            .items_center()
            .justify_between()
            .h(px(28.0))
            .px_2()
            .rounded(RADIUS)
            .cursor_pointer()
            .child(command.name)
            .children(command.key.map(|key| {
                div()
                    .text_size(SMALL_FONT_SIZE)
                    .text_color(theme.faint)
                    .child(key_label(key))
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
        let visible = self.visible();
        let entries: Vec<_> = visible
            .iter()
            .enumerate()
            .map(|(row, &index)| self.entry(index, row == self.selected, cx))
            .collect();
        let empty = entries.is_empty();
        let query = if self.query.is_empty() {
            div().text_color(theme.faint).child("Run a command…")
        } else {
            div().child(self.query.clone())
        };
        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .occlude()
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
                    .child(query),
            )
            .child(
                div()
                    .id("commands")
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
