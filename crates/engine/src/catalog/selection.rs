//! The selected photos, shared by every panel that shows photos.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// A set of photos, by path, beside the open photo.
///
/// Panels show photos in different orders, so whatever depends on an order
/// takes it from the caller. The open photo need not be selected.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhotoSelection {
    selected: HashSet<PathBuf>,
    /// Where a range starts.
    anchor: Option<PathBuf>,
    open: Option<PathBuf>,
}

impl PhotoSelection {
    /// A photo was opened: it becomes the whole selection.
    pub fn set_open(&mut self, photo: &Path) {
        self.open = Some(photo.to_path_buf());
        self.select_only(photo);
    }

    /// The photo was closed. The selection stays.
    pub fn clear_open(&mut self) {
        self.open = None;
    }

    pub fn open(&self) -> Option<&Path> {
        self.open.as_deref()
    }

    pub fn contains(&self, photo: &Path) -> bool {
        self.selected.contains(photo)
    }

    pub fn len(&self) -> usize {
        self.selected.len()
    }

    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    /// A click.
    pub fn select_only(&mut self, photo: &Path) {
        self.selected.clear();
        self.selected.insert(photo.to_path_buf());
        self.anchor = Some(photo.to_path_buf());
    }

    /// A click with Ctrl.
    pub fn toggle(&mut self, photo: &Path) {
        if !self.selected.remove(photo) {
            self.selected.insert(photo.to_path_buf());
        }
        self.anchor = Some(photo.to_path_buf());
    }

    /// A click with Shift: everything in `order` between the last photo
    /// clicked without Shift and this one.
    pub fn select_range<'a>(&mut self, photo: &Path, order: impl IntoIterator<Item = &'a Path>) {
        let order: Vec<&Path> = order.into_iter().collect();
        let position = |wanted: &Path| order.iter().position(|path| *path == wanted);
        let ends = self
            .anchor
            .as_deref()
            .and_then(position)
            .zip(position(photo));
        let Some((start, end)) = ends else {
            self.select_only(photo);
            return;
        };
        self.selected = order[start.min(end)..=start.max(end)]
            .iter()
            .map(|path| path.to_path_buf())
            .collect();
    }

    pub fn select_all<'a>(&mut self, photos: impl IntoIterator<Item = &'a Path>) {
        self.selected = photos.into_iter().map(Path::to_path_buf).collect();
    }

    /// Take a photo out, for example because it is gone.
    pub fn remove(&mut self, photo: &Path) {
        self.selected.remove(photo);
        if self.anchor.as_deref() == Some(photo) {
            self.anchor = None;
        }
    }

    /// Select just the open photo, or nothing.
    pub fn reset(&mut self) {
        self.selected = self.open.iter().cloned().collect();
        self.anchor = self.open.clone();
    }

    /// The selected photos as `order` has them, then the ones it does not have
    /// by path.
    pub fn in_order<'a>(&self, order: impl IntoIterator<Item = &'a Path>) -> Vec<PathBuf> {
        let mut ordered: Vec<PathBuf> = order
            .into_iter()
            .filter(|path| self.contains(path))
            .map(Path::to_path_buf)
            .collect();
        let known: HashSet<PathBuf> = ordered.iter().cloned().collect();
        let mut rest: Vec<PathBuf> = self.selected.difference(&known).cloned().collect();
        rest.sort();
        ordered.extend(rest);
        ordered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORDER: [&str; 5] = ["e", "b", "d", "a", "c"];

    fn order() -> impl Iterator<Item = &'static Path> {
        ORDER.into_iter().map(Path::new)
    }

    fn selected(selection: &PhotoSelection) -> Vec<String> {
        let paths = selection.in_order(order());
        paths
            .iter()
            .map(|path| path.display().to_string())
            .collect()
    }

    #[test]
    fn a_click_selects_one_and_ctrl_toggles() {
        let mut selection = PhotoSelection::default();
        assert!(selection.is_empty());
        selection.select_only(Path::new("a"));
        selection.select_only(Path::new("b"));
        assert_eq!(selected(&selection), ["b"]);
        selection.toggle(Path::new("a"));
        assert_eq!(selected(&selection), ["b", "a"]);
        selection.toggle(Path::new("b"));
        assert_eq!(selected(&selection), ["a"]);
        assert_eq!(selection.len(), 1);
    }

    #[test]
    fn a_range_follows_the_given_order_from_the_anchor_in_both_directions() {
        let mut selection = PhotoSelection::default();
        selection.select_only(Path::new("d"));
        selection.select_range(Path::new("c"), order());
        assert_eq!(selected(&selection), ["d", "a", "c"]);
        // The anchor stays, so the next range replaces this one.
        selection.select_range(Path::new("e"), order());
        assert_eq!(selected(&selection), ["e", "b", "d"]);
        // Ctrl moves the anchor.
        selection.toggle(Path::new("a"));
        selection.select_range(Path::new("c"), order());
        assert_eq!(selected(&selection), ["a", "c"]);
    }

    #[test]
    fn a_range_without_a_usable_anchor_selects_one() {
        let mut selection = PhotoSelection::default();
        selection.select_range(Path::new("b"), order());
        assert_eq!(selected(&selection), ["b"]);
        selection.select_only(Path::new("elsewhere"));
        selection.select_range(Path::new("a"), order());
        assert_eq!(selected(&selection), ["a"]);
    }

    #[test]
    fn opening_selects_the_photo_and_resetting_returns_to_it() {
        let mut selection = PhotoSelection::default();
        selection.set_open(Path::new("a"));
        selection.select_all(order());
        assert_eq!(selection.len(), 5);
        assert_eq!(selection.open(), Some(Path::new("a")));
        selection.reset();
        assert_eq!(selected(&selection), ["a"]);
        selection.select_range(Path::new("c"), order());
        assert_eq!(selected(&selection), ["a", "c"]);
        selection.clear_open();
        assert_eq!(selected(&selection), ["a", "c"]);
        selection.reset();
        assert!(selection.is_empty());
    }

    #[test]
    fn photos_outside_the_order_come_last() {
        let mut selection = PhotoSelection::default();
        for path in ["z", "c", "y", "e"] {
            selection.toggle(Path::new(path));
        }
        assert_eq!(selected(&selection), ["e", "c", "y", "z"]);
        selection.remove(Path::new("e"));
        assert_eq!(selected(&selection), ["c", "y", "z"]);
        selection.select_range(Path::new("a"), order());
        assert_eq!(selected(&selection), ["a"], "the anchor was removed");
    }
}
