//! The folder tree and the selection, as the panels use them.

use std::path::{Path, PathBuf};

use gpui::{Context, Modifiers};
use rohditor_engine::catalog::folder_tree::FolderTree;

use super::Library;

impl Library {
    pub fn tree(&self) -> &FolderTree {
        &self.tree
    }

    /// Change the tree, and have the folders listed that it needs.
    pub fn change_tree<R>(
        &mut self,
        change: impl FnOnce(&mut FolderTree) -> R,
        cx: &mut Context<Self>,
    ) -> R {
        let result = change(&mut self.tree);
        for folder in self.tree.take_requests() {
            if let Err(error) = self.coordinator.list_folder(folder) {
                self.error = Some(error);
            }
        }
        cx.notify();
        result
    }

    /// The row to scroll to now.
    pub fn take_revealed(&mut self) -> Option<usize> {
        self.tree.take_revealed()
    }

    /// Open or close a folder in the tree, and show it.
    pub fn click_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        let was_shown = self.folder() == Some(&folder);
        self.change_tree(|tree| tree.click_folder(&folder, was_shown), cx);
        if !was_shown {
            self.show_folder(folder, cx);
        }
    }

    pub fn remove_root(&mut self, root: &Path, cx: &mut Context<Self>) {
        self.change_tree(|tree| tree.remove_root(root), cx);
        self.save();
    }

    /// List the open folders and the one shown again.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.change_tree(FolderTree::refresh, cx);
        if let Some(source) = self.catalog.source().cloned() {
            self.error = self.coordinator.show(source).err();
        }
    }

    pub fn is_selected(&self, photo: &Path) -> bool {
        self.state.selection.contains(photo)
    }

    /// A click on a photo in the tree. Returns whether to open it.
    pub fn click_photo(
        &mut self,
        photo: &Path,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) -> bool {
        let selection = &mut self.state.selection;
        self.tree.set_cursor(photo);
        cx.notify();
        if modifiers.shift {
            selection.select_range(photo, self.tree.photos());
        } else if modifiers.secondary() {
            selection.toggle(photo);
        } else {
            selection.select_only(photo);
            return true;
        }
        false
    }
}

#[cfg(test)]
impl Library {
    /// How many photos the shown folder has.
    pub fn len(&self) -> usize {
        self.state.order().len()
    }

    pub fn selected(&self) -> Vec<PathBuf> {
        self.state.selection.in_order(self.tree.photos())
    }
}
