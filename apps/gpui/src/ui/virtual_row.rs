//! A horizontally scrolled row of many equally wide cells, which lays out only those on screen.
//!
//! GPUI's `uniform_list` is vertical only. The wheel scrolls the row too.

use std::ops::Range;

use gpui::{
    AnyElement, Bounds, Div, ElementId, Pixels, ScrollHandle, Stateful, Window, div, point,
    prelude::*, px,
};

/// Where the row is scrolled to. Kept by the view that shows the row.
#[derive(Default)]
pub struct VirtualRowScroll(ScrollHandle);

impl VirtualRowScroll {
    /// Scroll as little as needed to show a cell and its neighbours. Returns whether it could:
    /// not before the row was laid out once.
    pub fn reveal(&self, index: usize, count: usize, cell: Pixels) -> bool {
        if self.0.bounds() == Bounds::default() {
            return false;
        }
        let scrolled = -self.0.offset().x.as_f32();
        let width = self.0.bounds().size.width.as_f32();
        let scrolled = scroll_to_show(index, count, cell.as_f32(), scrolled, width);
        self.scroll_to(px(scrolled));
        true
    }

    pub fn to_start(&self) {
        self.scroll_to(px(0.0));
    }

    fn scroll_to(&self, scrolled: Pixels) {
        self.0.set_offset(point(-scrolled, px(0.0)));
    }

    /// The cells that may be on screen. The row is at most as wide as the window, which holds
    /// also when its own width is about to change.
    pub fn visible(&self, count: usize, cell: Pixels, window: &Window) -> Range<usize> {
        let scrolled = -self.0.offset().x.as_f32();
        let width = window.viewport_size().width.as_f32();
        visible_cells(count, cell.as_f32(), scrolled, width)
    }
}

/// The row, as high as its parent, with the cells that [`VirtualRowScroll::visible`] asked for.
pub fn virtual_row(
    id: impl Into<ElementId>,
    scroll: &VirtualRowScroll,
    count: usize,
    cell: Pixels,
    cells: impl IntoIterator<Item = (usize, AnyElement)>,
) -> Stateful<Div> {
    let place = |(index, content): (usize, AnyElement)| {
        div()
            .absolute()
            .left(cell * index as f32)
            .top_0()
            .w(cell)
            .h_full()
            .child(content)
    };
    div()
        .id(id)
        .size_full()
        .overflow_x_scroll()
        .track_scroll(&scroll.0)
        .child(
            div()
                .relative()
                .h_full()
                .w(cell * count as f32)
                .children(cells.into_iter().map(place)),
        )
}

fn visible_cells(count: usize, cell: f32, scrolled: f32, width: f32) -> Range<usize> {
    // The content may have become shorter than what is scrolled to.
    let scrolled = scrolled.min(count as f32 * cell - width).max(0.0);
    let start = (scrolled / cell).floor() as usize;
    let end = ((scrolled + width) / cell).ceil() as usize;
    start.min(count)..end.min(count)
}

fn scroll_to_show(index: usize, count: usize, cell: f32, scrolled: f32, width: f32) -> f32 {
    let left = index.saturating_sub(1) as f32 * cell;
    let right = (index + 2).min(count) as f32 * cell;
    let longest = (count as f32 * cell - width).max(0.0);
    // The left neighbour wins where there is no room for all.
    scrolled.max(right - width).min(left).clamp(0.0, longest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partly_visible_cells_are_laid_out() {
        assert_eq!(visible_cells(1000, 10.0, 0.0, 95.0), 0..10);
        assert_eq!(visible_cells(1000, 10.0, 25.0, 100.0), 2..13);
        assert_eq!(visible_cells(5, 10.0, 0.0, 100.0), 0..5);
        assert_eq!(visible_cells(0, 10.0, 0.0, 100.0), 0..0);
    }

    #[test]
    fn scrolling_beyond_the_end_shows_the_last_cells() {
        assert_eq!(visible_cells(20, 10.0, 5000.0, 100.0), 10..20);
        assert_eq!(visible_cells(20, 10.0, -30.0, 100.0), 0..10);
    }

    #[test]
    fn a_cell_is_revealed_with_its_neighbours_and_without_moving_if_they_show() {
        let show = |index, scrolled| scroll_to_show(index, 100, 10.0, scrolled, 100.0);
        assert_eq!(show(25, 200.0), 200.0);
        assert_eq!(show(20, 200.0), 190.0);
        assert_eq!(show(29, 200.0), 210.0);
        assert_eq!(show(80, 0.0), 720.0);
        assert_eq!(show(0, 500.0), 0.0);
        assert_eq!(show(99, 0.0), 900.0);
        assert_eq!(scroll_to_show(2, 3, 10.0, 0.0, 100.0), 0.0);
    }
}
