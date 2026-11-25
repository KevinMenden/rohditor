//! The folder tree: several root folders with their subfolders and photos.
//!
//! Pure state. Folders are listed by the catalog worker: the frontend passes
//! on [`FolderTree::take_requests`] to
//! [`list_folder`](super::CatalogCoordinator::list_folder) and the resulting
//! [`FolderListed`](super::CatalogEvent::FolderListed) events to
//! [`FolderTree::folder_listed`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rohditor_catalog::FolderListing;

/// One visible line of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub path: PathBuf,
    pub name: String,
    /// 0 for a root.
    pub depth: usize,
    pub kind: TreeRowKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeRowKind {
    Folder {
        expanded: bool,
        /// Why the folder could not be read.
        failure: Option<String>,
    },
    Photo,
}

impl TreeRow {
    pub fn is_photo(&self) -> bool {
        self.kind == TreeRowKind::Photo
    }
}

/// Keyboard navigation steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeNavigation {
    Up,
    Down,
    /// Collapse the folder, or go to the parent.
    Left,
    /// Expand the folder, or go to its first child.
    Right,
    Home,
    End,
}

#[derive(Debug, Default)]
pub struct FolderTree {
    /// In the order they were added. None is inside another.
    roots: Vec<PathBuf>,
    expanded: HashSet<PathBuf>,
    /// The last listing of every folder that was listed.
    listings: HashMap<PathBuf, FolderListing>,
    failures: HashMap<PathBuf, String>,
    rows: Vec<TreeRow>,
    /// The row the keyboard is at.
    cursor: Option<PathBuf>,
    /// A photo to scroll to once its row exists.
    revealing: Option<PathBuf>,
    requests: Vec<PathBuf>,
}

impl FolderTree {
    /// A tree of remembered roots, collapsed.
    #[must_use]
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut tree = Self::default();
        for root in roots {
            tree.insert_root(root);
        }
        tree.rebuild();
        tree
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Add a folder, expanded, and put the cursor on it. A folder inside a
    /// root is shown there instead of becoming another root; roots inside the
    /// new one become part of it. `folder` must be absolute.
    pub fn add_root(&mut self, folder: PathBuf) {
        self.insert_root(folder.clone());
        self.expand_ancestors(&folder);
        self.expand_folder(&folder);
        self.revealing = Some(folder.clone());
        self.cursor = Some(folder);
        self.rebuild();
    }

    fn insert_root(&mut self, folder: PathBuf) {
        if self.root_of(&folder).is_none() {
            self.roots.retain(|root| !root.starts_with(&folder));
            self.roots.push(folder);
        }
    }

    /// Stop showing a root. Nothing happens to the folder itself.
    pub fn remove_root(&mut self, root: &Path) {
        self.roots.retain(|other| other != root);
        self.forget_outside_roots();
        self.rebuild();
    }

    fn root_of(&self, path: &Path) -> Option<&Path> {
        let root = self.roots.iter().find(|root| path.starts_with(root));
        root.map(PathBuf::as_path)
    }

    fn forget_outside_roots(&mut self) {
        let roots = &self.roots;
        let shown = |path: &Path| roots.iter().any(|root| path.starts_with(root));
        self.expanded.retain(|path| shown(path));
        self.listings.retain(|path, _| shown(path));
        self.failures.retain(|path, _| shown(path));
        self.requests.retain(|path| shown(path));
        self.revealing.take_if(|path| !shown(path));
    }

    pub fn is_expanded(&self, folder: &Path) -> bool {
        self.expanded.contains(folder)
    }

    /// Show what is in a folder. It is listed again even if it was before.
    pub fn expand(&mut self, folder: &Path) {
        self.expand_folder(folder);
        self.rebuild();
    }

    fn expand_folder(&mut self, folder: &Path) {
        self.expanded.insert(folder.to_path_buf());
        self.request(folder);
    }

    pub fn collapse(&mut self, folder: &Path) {
        self.expanded.remove(folder);
        self.rebuild();
    }

    pub fn toggle(&mut self, folder: &Path) {
        if self.is_expanded(folder) {
            self.collapse(folder);
        } else {
            self.expand(folder);
        }
    }

    /// A click on a folder, which also shows it. One that was not shown yet is
    /// only opened; one that was opens and closes.
    pub fn click_folder(&mut self, folder: &Path, was_shown: bool) {
        if was_shown {
            self.toggle(folder);
        } else if !self.is_expanded(folder) {
            self.expand(folder);
        }
        self.set_cursor(folder);
    }

    /// List every folder whose content is visible again.
    pub fn refresh(&mut self) {
        let open = self.rows.iter().filter(|row| {
            let expanded = matches!(row.kind, TreeRowKind::Folder { expanded: true, .. });
            expanded || row.depth == 0
        });
        for folder in open.map(|row| row.path.clone()).collect::<Vec<_>>() {
            self.request(&folder);
        }
    }

    /// Expand down to a photo, and scroll to it when its row is there. Returns
    /// whether a root contains it.
    pub fn reveal(&mut self, photo: &Path) -> bool {
        if self.root_of(photo).is_none() {
            return false;
        }
        self.expand_ancestors(photo);
        self.revealing = Some(photo.to_path_buf());
        self.rebuild();
        // It may be newer than its folder's listing.
        if let Some(parent) = photo.parent()
            && self.row_of(photo).is_none()
        {
            self.request(parent);
        }
        true
    }

    /// Expand the folders around `path`, which must be inside a root. Folders
    /// already listed are not listed again.
    fn expand_ancestors(&mut self, path: &Path) {
        let Some(root) = self.root_of(path).map(Path::to_path_buf) else {
            return;
        };
        let ancestors = path.ancestors().skip(1);
        for folder in ancestors.take_while(|folder| folder.starts_with(&root)) {
            self.expanded.insert(folder.to_path_buf());
            if !self.listings.contains_key(folder) {
                self.request(folder);
            }
        }
    }

    fn request(&mut self, folder: &Path) {
        if !self.requests.iter().any(|path| path == folder) {
            self.requests.push(folder.to_path_buf());
        }
    }

    /// The folders to list now.
    pub fn take_requests(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.requests)
    }

    /// Take over the listing of a folder.
    pub fn folder_listed(&mut self, folder: PathBuf, result: Result<FolderListing, String>) {
        // The root may have been removed meanwhile.
        if self.root_of(&folder).is_none() {
            return;
        }
        let failed = result.is_err();
        match result {
            Ok(listing) => {
                self.failures.remove(&folder);
                self.listings.insert(folder.clone(), listing);
            }
            Err(reason) => {
                self.listings.remove(&folder);
                self.failures.insert(folder.clone(), reason);
            }
        }
        self.rebuild();
        // What should be revealed turned out not to be there. The folders above
        // may still be to come, so its row says nothing.
        let absent = self.revealing.as_ref().is_some_and(|path| {
            if path.parent() != Some(&folder) {
                return failed && path.starts_with(&folder);
            }
            let listing = self.listings.get(&folder);
            !listing.is_some_and(|listing| {
                listing.files.contains(path) || listing.folders.contains(path)
            })
        });
        if absent {
            self.revealing = None;
        }
    }

    /// The visible lines, top to bottom.
    pub fn rows(&self) -> &[TreeRow] {
        &self.rows
    }

    pub fn row_of(&self, path: &Path) -> Option<usize> {
        self.rows.iter().position(|row| row.path == path)
    }

    /// The visible photos, top to bottom: the order a range selection follows.
    pub fn photos(&self) -> impl Iterator<Item = &Path> {
        let photos = self.rows.iter().filter(|row| row.is_photo());
        photos.map(|row| row.path.as_path())
    }

    /// The row to scroll to now, once for every reveal.
    pub fn take_revealed(&mut self) -> Option<usize> {
        let row = self.row_of(self.revealing.as_deref()?)?;
        self.revealing = None;
        Some(row)
    }

    pub fn cursor(&self) -> Option<&Path> {
        self.cursor.as_deref()
    }

    pub fn set_cursor(&mut self, path: &Path) {
        self.cursor = Some(path.to_path_buf());
    }

    /// Move the cursor and return its row. Without a cursor, every step goes to
    /// the first row.
    pub fn navigate(&mut self, step: TreeNavigation) -> Option<usize> {
        let last = self.rows.len().checked_sub(1)?;
        let Some(at) = self.cursor.as_deref().and_then(|path| self.row_of(path)) else {
            self.cursor = Some(self.rows[0].path.clone());
            return Some(0);
        };
        let row = &self.rows[at];
        let expanded = match row.kind {
            TreeRowKind::Folder { expanded, .. } => Some(expanded),
            TreeRowKind::Photo => None,
        };
        let target = match step {
            TreeNavigation::Up => at.saturating_sub(1),
            TreeNavigation::Down => (at + 1).min(last),
            TreeNavigation::Home => 0,
            TreeNavigation::End => last,
            TreeNavigation::Left if expanded == Some(true) => {
                let folder = row.path.clone();
                self.collapse(&folder);
                at
            }
            TreeNavigation::Left => {
                let parent = self.rows[..at]
                    .iter()
                    .rposition(|above| above.depth < row.depth);
                parent.unwrap_or(at)
            }
            TreeNavigation::Right if expanded == Some(false) => {
                let folder = row.path.clone();
                self.expand(&folder);
                at
            }
            TreeNavigation::Right => {
                let child = self.rows.get(at + 1);
                let is_child = child.is_some_and(|child| child.depth > row.depth);
                if is_child { at + 1 } else { at }
            }
        };
        self.cursor = Some(self.rows[target].path.clone());
        Some(target)
    }

    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        for root in &self.roots {
            self.add_folder(root, 0, &mut rows);
        }
        self.rows = rows;
        // The cursor's folder was collapsed or the cursor's row is gone: go to
        // the closest folder that is still visible. While revealing, the row
        // is still to come.
        if let Some(cursor) = self.cursor.take() {
            self.cursor = if self.revealing.as_ref() == Some(&cursor) {
                Some(cursor)
            } else {
                cursor
                    .ancestors()
                    .find(|path| self.row_of(path).is_some())
                    .map(Path::to_path_buf)
            };
        }
    }

    fn add_folder(&self, folder: &Path, depth: usize, rows: &mut Vec<TreeRow>) {
        let expanded = self.is_expanded(folder);
        rows.push(TreeRow {
            name: name_of(folder),
            path: folder.to_path_buf(),
            depth,
            kind: TreeRowKind::Folder {
                expanded,
                failure: self.failures.get(folder).cloned(),
            },
        });
        let Some(listing) = self.listings.get(folder).filter(|_| expanded) else {
            return;
        };
        for subfolder in &listing.folders {
            self.add_folder(subfolder, depth + 1, rows);
        }
        rows.extend(listing.files.iter().map(|photo| TreeRow {
            name: name_of(photo),
            path: photo.clone(),
            depth: depth + 1,
            kind: TreeRowKind::Photo,
        }));
    }
}

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
#[path = "folder_tree_tests.rs"]
mod tests;
