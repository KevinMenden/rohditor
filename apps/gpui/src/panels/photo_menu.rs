//! The context menu entries every file and folder has.

use std::path::Path;

use gpui::ClipboardItem;

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
