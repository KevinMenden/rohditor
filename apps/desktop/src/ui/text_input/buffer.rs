//! The text and selection of a single-line input. Offsets are UTF-8 bytes on grapheme boundaries;
//! the platform's input methods count in UTF-16 instead.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation as _;

#[derive(Debug, Default)]
pub struct Buffer {
    text: String,
    selection: Range<usize>,
    /// The cursor is at the start of the selection.
    reversed: bool,
    /// Text an input method is still composing.
    marked: Option<Range<usize>>,
}

impl Buffer {
    /// Holds `text`, all of it selected.
    pub fn new(text: String) -> Self {
        Self {
            selection: 0..text.len(),
            text,
            ..Self::default()
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.selection.clone()]
    }

    pub const fn reversed(&self) -> bool {
        self.reversed
    }

    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub fn unmark(&mut self) {
        self.marked = None;
    }

    pub const fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    pub fn previous(&self) -> usize {
        let before = &self.text[..self.cursor()];
        before
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    pub fn next(&self) -> usize {
        let cursor = self.cursor();
        let step = self.text[cursor..].graphemes(true).next();
        cursor + step.map_or(0, str::len)
    }

    /// Where the left arrow goes: to the start of a selection, or one step back.
    pub fn left(&self) -> usize {
        if self.selection.is_empty() {
            self.previous()
        } else {
            self.selection.start
        }
    }

    pub fn right(&self) -> usize {
        if self.selection.is_empty() {
            self.next()
        } else {
            self.selection.end
        }
    }

    pub fn move_to(&mut self, offset: usize) {
        self.selection = offset..offset;
        self.reversed = false;
    }

    pub fn select(&mut self, range: Range<usize>) {
        self.selection = range;
        self.reversed = false;
    }

    /// Move the cursor, keeping the other end of the selection.
    pub fn select_to(&mut self, offset: usize) {
        let anchor = if self.reversed {
            self.selection.end
        } else {
            self.selection.start
        };
        self.reversed = offset < anchor;
        self.selection = offset.min(anchor)..offset.max(anchor);
    }

    /// Replace `range`, or else the composed text, or else the selection.
    pub fn replace(&mut self, range: Option<Range<usize>>, text: &str) -> Range<usize> {
        let range = range
            .or_else(|| self.marked.take())
            .unwrap_or_else(|| self.selection.clone());
        // A single line.
        let text = text.replace(['\n', '\r'], " ");
        self.text.replace_range(range.clone(), &text);
        let end = range.start + text.len();
        self.marked = None;
        self.move_to(end);
        range.start..end
    }

    /// As [`Self::replace`], with text still being composed. `selected` is relative to it, in
    /// UTF-16.
    pub fn compose(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
    ) {
        let inserted = self.replace(range, text);
        if let Some(selected) = selected {
            let start = self.to_utf16(inserted.start);
            self.select(self.range_from_utf16(&(start + selected.start..start + selected.end)));
        }
        self.marked = (!inserted.is_empty()).then_some(inserted);
    }

    fn offset_from_utf16(&self, wanted: usize) -> usize {
        let mut units = 0;
        self.text
            .char_indices()
            .find(|(_, letter)| {
                let reached = units >= wanted;
                units += letter.len_utf16();
                reached
            })
            .map_or(self.text.len(), |(index, _)| index)
    }

    pub fn to_utf16(&self, offset: usize) -> usize {
        self.text[..offset].encode_utf16().count()
    }

    pub fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    pub fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.to_utf16(range.start)..self.to_utf16(range.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_replaces_the_selection() {
        let mut buffer = Buffer::new("+0.50 EV".to_owned());
        buffer.replace(None, "1");
        buffer.replace(None, ".5\n");
        assert_eq!(buffer.text(), "1.5 ");
        assert_eq!(buffer.selection(), 4..4);
    }

    #[test]
    fn the_cursor_moves_over_whole_graphemes() {
        // An `e` with a combining accent, and an emoji of two UTF-16 units.
        let mut buffer = Buffer::new("e\u{301}😀".to_owned());
        assert_eq!((buffer.left(), buffer.right()), (0, 7));
        buffer.move_to(7);
        assert_eq!(buffer.previous(), 3);
        buffer.move_to(3);
        assert_eq!((buffer.previous(), buffer.next()), (0, 7));
        assert_eq!(buffer.range_to_utf16(&(3..7)), 2..4);
        assert_eq!(buffer.range_from_utf16(&(2..4)), 3..7);
    }

    #[test]
    fn a_selection_can_cross_its_anchor() {
        let mut buffer = Buffer::new("abcd".to_owned());
        buffer.move_to(2);
        buffer.select_to(4);
        assert_eq!(buffer.selected_text(), "cd");
        buffer.select_to(1);
        assert_eq!((buffer.selected_text(), buffer.cursor()), ("b", 1));
    }

    #[test]
    fn ranges_of_an_input_method_are_cut_to_the_text() {
        let mut buffer = Buffer::new("a😀".to_owned());
        // Past the end, and inside the emoji's two UTF-16 units.
        assert_eq!(buffer.range_from_utf16(&(1..9)), 1..5);
        assert_eq!(buffer.range_from_utf16(&(2..2)), 5..5);
        let range = buffer.range_from_utf16(&(8..9));
        buffer.replace(Some(range), "b");
        assert_eq!(buffer.text(), "a😀b");
        buffer.compose(Some(0..0), "😀", Some(1..7));
        assert_eq!(buffer.text(), "😀a😀b");
        assert!(buffer.text().get(buffer.selection()).is_some());
    }

    #[test]
    fn composed_text_is_replaced_by_its_result() {
        let mut buffer = Buffer::new("a".to_owned());
        buffer.move_to(1);
        buffer.compose(None, "´", Some(1..1));
        assert_eq!(buffer.marked(), Some(1..3));
        buffer.replace(None, "é");
        assert_eq!((buffer.text(), buffer.marked()), ("aé", None));
    }
}
