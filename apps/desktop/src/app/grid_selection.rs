//! The library grid's single selection and its keyboard navigation.

use std::path::{Path, PathBuf};

/// Keyboard navigation steps in the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridNavigation {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

#[derive(Debug)]
pub struct GridSelection {
    folder: Option<PathBuf>,
    pub selection: Option<usize>,
    /// Column count of the laid-out grid.
    pub columns: usize,
    /// Scroll the selection into view once, after keyboard navigation.
    pub scroll_to_selection: bool,
}

impl Default for GridSelection {
    fn default() -> Self {
        Self {
            folder: None,
            selection: None,
            columns: 1,
            scroll_to_selection: false,
        }
    }
}

impl GridSelection {
    /// Reset on a folder change and drop a selection past the end.
    pub fn sync(&mut self, folder: Option<&Path>, count: usize) {
        if folder != self.folder.as_deref() {
            self.folder = folder.map(Path::to_path_buf);
            self.selection = None;
        }
        self.selection = self.selection.filter(|index| *index < count);
    }

    /// Move the selection by a keyboard step and scroll it into view.
    pub fn navigate(&mut self, entry_count: usize, key: GridNavigation) {
        if entry_count == 0 {
            return;
        }
        self.selection = Some(moved_selection(
            self.selection,
            self.columns,
            entry_count,
            key,
        ));
        self.scroll_to_selection = true;
    }
}

/// Move the selection one grid step; `count` must be at least one entry.
/// Without a selection, any key selects the first entry.
fn moved_selection(
    current: Option<usize>,
    columns: usize,
    count: usize,
    key: GridNavigation,
) -> usize {
    let columns = columns.max(1);
    let last = count - 1;
    let Some(current) = current.map(|current| current.min(last)) else {
        return 0;
    };
    match key {
        GridNavigation::Left => current.saturating_sub(1),
        GridNavigation::Right => (current + 1).min(last),
        GridNavigation::Up => current.saturating_sub(columns),
        GridNavigation::Down => (current + columns).min(last),
        GridNavigation::Home => 0,
        GridNavigation::End => last,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_moves_by_grid_steps_and_clamps() {
        use GridNavigation::{Down, End, Home, Left, Right, Up};

        assert_eq!(moved_selection(None, 3, 10, Right), 0);
        assert_eq!(moved_selection(None, 3, 10, Left), 0);
        assert_eq!(moved_selection(Some(4), 3, 10, Down), 7);
        assert_eq!(moved_selection(Some(4), 3, 10, Up), 1);
        assert_eq!(moved_selection(Some(4), 3, 10, Left), 3);
        assert_eq!(moved_selection(Some(9), 3, 10, Right), 9);
        assert_eq!(moved_selection(Some(9), 3, 10, Down), 9);
        assert_eq!(moved_selection(Some(5), 3, 10, Home), 0);
        assert_eq!(moved_selection(Some(5), 3, 10, End), 9);
        assert_eq!(moved_selection(Some(5), 1, 1, End), 0);
    }

    #[test]
    fn navigating_requests_a_scroll_and_ignores_an_empty_catalog() {
        let mut grid = GridSelection {
            columns: 4,
            ..GridSelection::default()
        };
        grid.navigate(0, GridNavigation::Right);
        assert_eq!(grid.selection, None);
        assert!(!grid.scroll_to_selection);

        grid.navigate(10, GridNavigation::Right);
        assert_eq!(grid.selection, Some(0));
        assert!(grid.scroll_to_selection);
        grid.navigate(10, GridNavigation::Down);
        assert_eq!(grid.selection, Some(4));
    }
}
