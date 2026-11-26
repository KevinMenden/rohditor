//! The context menu entries of files and folders.

use std::path::Path;

use gpui::{Action, ClipboardItem};

use crate::actions::{AddToAlbum, RemoveFromAlbum};
use crate::ui::menu::MenuEntry;

pub fn path_entries(path: &Path) -> Vec<MenuEntry> {
    let (reveal, copy) = (path.to_path_buf(), path.display().to_string());
    vec![
        MenuEntry::new("Show in File Manager", move |_, cx| cx.reveal_path(&reveal)),
        MenuEntry::new("Copy Path", move |_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
        }),
    ]
}

/// Adds the selected photos to an album.
pub fn add_to_album() -> MenuEntry {
    entry("Add to Album…", AddToAlbum)
}

/// Removes the selected photos from the album shown.
pub fn remove_from_album() -> MenuEntry {
    entry("Remove from Album", RemoveFromAlbum)
}

fn entry(label: &'static str, action: impl Action) -> MenuEntry {
    MenuEntry::new(label, move |window, cx| {
        window.dispatch_action(action.boxed_clone(), cx);
    })
}
