//! Lays out and paints a [`TextInput`]'s line, selection and cursor.

use gpui::{
    App, Bounds, Element, ElementId, ElementInputHandler, Entity, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, PaintQuad, Pixels, ShapedLine, Style, TextAlign,
    TextRun, UnderlineStyle, Window, fill, point, px, relative, size,
};

use super::TextInput;
use crate::theme::theme;

const CURSOR_WIDTH: Pixels = px(1.5);

pub struct TextElement {
    pub input: Entity<TextInput>,
}

pub struct Prepared {
    line: ShapedLine,
    /// Where the line starts, left of the bounds when it is scrolled.
    origin: gpui::Point<Pixels>,
    selection: PaintQuad,
    cursor: PaintQuad,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// The line's text runs: the composed text is underlined.
fn runs(run: TextRun, marked: Option<std::ops::Range<usize>>) -> Vec<TextRun> {
    let marked = marked.unwrap_or(0..0);
    let part = |len, underline| TextRun {
        len,
        underline,
        ..run.clone()
    };
    let underline = UnderlineStyle {
        color: Some(run.color),
        thickness: px(1.0),
        wavy: false,
    };
    [
        part(marked.start, None),
        part(marked.len(), Some(underline)),
        part(run.len - marked.end, None),
    ]
    .into_iter()
    .filter(|run| run.len > 0)
    .collect()
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepared;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepared {
        let theme = theme();
        let input = self.input.read(cx);
        let buffer = &input.buffer;
        let style = window.text_style();
        let empty = buffer.text().is_empty();
        let (text, color) = if empty {
            (input.placeholder.clone(), theme.faint)
        } else {
            (buffer.text().to_owned().into(), style.color)
        };
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window.text_system().shape_line(
            text,
            font_size,
            &runs(run, buffer.marked().filter(|_| !empty)),
            None,
        );
        // Scroll just far enough to show the cursor.
        let cursor = line.x_for_index(buffer.cursor());
        let room = bounds.size.width - CURSOR_WIDTH;
        let furthest = (line.width - room).max(px(0.0));
        let scroll = input
            .scroll
            .max(cursor - room)
            .min(cursor)
            .min(furthest)
            .max(px(0.0));
        let origin = point(bounds.left() - scroll, bounds.top());
        let selection = buffer.selection();
        let from = line.x_for_index(selection.start);
        let to = line.x_for_index(selection.end);
        let height = bounds.size.height;
        let quad = |x, width, color| {
            fill(
                Bounds::new(point(origin.x + x, origin.y), size(width, height)),
                color,
            )
        };
        self.input.update(cx, |input, _| input.scroll = scroll);
        Prepared {
            line,
            origin,
            selection: quad(from, to - from, theme.fill),
            cursor: quad(cursor, CURSOR_WIDTH, theme.accent),
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        prepared: &mut Prepared,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let focused = focus.is_focused(window);
        if focused {
            window.paint_quad(prepared.selection.clone());
        }
        let height = window.line_height();
        // Fails only when the glyph atlas cannot grow.
        let _ = prepared
            .line
            .paint(prepared.origin, height, TextAlign::Left, None, window, cx);
        if focused {
            window.paint_quad(prepared.cursor.clone());
        }
        let line = prepared.line.clone();
        let origin = prepared.origin;
        self.input.update(cx, |input, _| {
            input.layout = Some((line, Bounds::new(origin, bounds.size)));
        });
    }
}
