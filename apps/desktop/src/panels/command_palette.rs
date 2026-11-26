//! Command palette: every command, by name.

use gpui::{Context, SharedString};

use crate::actions::commands;
use crate::ui::matching;
use crate::ui::search_list::{SearchEntry, SearchList};

pub fn command_palette(cx: &mut Context<SearchList>) -> SearchList {
    let entries = commands().into_iter().map(|command| {
        let key = command.key_label().map(SharedString::from);
        let action = command.action;
        SearchEntry::new(command.name, key, move |window, cx| {
            window.dispatch_action(action.boxed_clone(), cx);
        })
    });
    SearchList::new(
        "Command palette",
        "Run a command…",
        entries.collect(),
        matching::letters,
        cx,
    )
}
