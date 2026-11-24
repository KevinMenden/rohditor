//! Single-line text input with selection, clipboard and input methods.
//!
//! Enter, Escape and the vertical arrows are left to the view around it, as the actions of
//! [`crate::ui::keys`].

mod buffer;
mod element;

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels,
    Point, Render, Role, ShapedLine, SharedString, UTF16Selection, Window, actions, div, point,
    prelude::*, px,
};

use buffer::Buffer;
use element::TextElement;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        SelectHome,
        SelectEnd,
        Paste,
        Cut,
        Copy,
    ]
);

pub const CONTEXT: &str = "TextInput";

/// The text was edited.
pub struct Changed;

pub struct TextInput {
    focus: FocusHandle,
    label: SharedString,
    placeholder: SharedString,
    buffer: Buffer,
    /// The last painted line and where it was.
    layout: Option<(ShapedLine, Bounds<Pixels>)>,
    /// How far the line is moved left to show the cursor.
    scroll: Pixels,
    selecting: bool,
}

impl EventEmitter<Changed> for TextInput {}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TextInput {
    /// Starts with `text` selected. `label` names it to assistive technology.
    pub fn new(
        label: impl Into<SharedString>,
        placeholder: impl Into<SharedString>,
        text: String,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus: cx.focus_handle().tab_stop(true),
            label: label.into(),
            placeholder: placeholder.into(),
            buffer: Buffer::new(text),
            layout: None,
            scroll: px(0.0),
            selecting: false,
        }
    }

    pub fn text(&self) -> &str {
        self.buffer.text()
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.buffer = Buffer::new(String::new());
        cx.emit(Changed);
        cx.notify();
    }

    fn edit(&mut self, range: Option<Range<usize>>, text: &str, cx: &mut Context<Self>) {
        self.buffer.replace(range, text);
        cx.emit(Changed);
        cx.notify();
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.buffer.move_to(offset);
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.buffer.select_to(offset);
        cx.notify();
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.selection().is_empty() {
            self.buffer.select_to(self.buffer.previous());
        }
        self.edit(None, "", cx);
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.selection().is_empty() {
            self.buffer.select_to(self.buffer.next());
        }
        self.edit(None, "", cx);
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.edit(None, &text, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let selected = self.buffer.selected_text();
        if !selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(selected.to_owned()));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.buffer.selection().is_empty() {
            self.copy(&Copy, window, cx);
            self.edit(None, "", cx);
        }
    }

    fn offset_at(&self, position: Point<Pixels>) -> usize {
        match &self.layout {
            // An empty input shows its placeholder.
            Some((line, bounds)) if !self.text().is_empty() => {
                line.closest_index_for_x(position.x - bounds.left())
            }
            _ => 0,
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let offset = self.offset_at(event.position);
        if event.click_count > 1 {
            self.buffer.select(0..self.text().len());
            cx.notify();
        } else if event.modifiers.shift {
            self.select_to(offset, cx);
        } else {
            self.selecting = true;
            self.move_to(offset, cx);
        }
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting &= event.dragging();
        if self.selecting {
            self.select_to(self.offset_at(event.position), cx);
        }
    }

    fn mouse_up(&mut self) {
        self.selecting = false;
    }
}

/// What the platform's input methods use.
impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.buffer.range_from_utf16(&range);
        *actual = Some(self.buffer.range_to_utf16(&range));
        self.text().get(range).map(str::to_owned)
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.buffer.range_to_utf16(&self.buffer.selection()),
            reversed: self.buffer.reversed(),
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let marked = self.buffer.marked()?;
        Some(self.buffer.range_to_utf16(&marked))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.unmark();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range.map(|range| self.buffer.range_from_utf16(&range));
        self.edit(range, text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range.map(|range| self.buffer.range_from_utf16(&range));
        self.buffer.compose(range, text, selected);
        cx.emit(Changed);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (line, bounds) = self.layout.as_ref()?;
        let range = self.buffer.range_from_utf16(&range);
        let corner = |index, y| point(bounds.left() + line.x_for_index(index), y);
        Some(Bounds::from_corners(
            corner(range.start, bounds.top()),
            corner(range.end, bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.buffer.to_utf16(self.offset_at(position)))
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("text-input")
            .role(Role::TextInput)
            .aria_label(self.label.clone())
            .aria_placeholder(self.placeholder.clone())
            .aria_value(self.text().to_owned())
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(|this, _: &Left, _, cx| this.move_to(this.buffer.left(), cx)))
            .on_action(cx.listener(|this, _: &Right, _, cx| this.move_to(this.buffer.right(), cx)))
            .on_action(cx.listener(|this, _: &Home, _, cx| this.move_to(0, cx)))
            .on_action(cx.listener(|this, _: &End, _, cx| this.move_to(this.text().len(), cx)))
            .on_action(cx.listener(|this, _: &SelectLeft, _, cx| {
                this.select_to(this.buffer.previous(), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectRight, _, cx| {
                this.select_to(this.buffer.next(), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectHome, _, cx| this.select_to(0, cx)))
            .on_action(cx.listener(|this, _: &SelectEnd, _, cx| {
                this.select_to(this.text().len(), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.buffer.select(0..this.text().len());
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.mouse_up()),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.mouse_up()),
            )
            .child(TextElement { input: cx.entity() })
    }
}
