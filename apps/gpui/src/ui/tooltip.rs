//! Tooltips: what a control does and its key.

use gpui::{
    AnyView, App, AppContext, Context, IntoElement, Render, SharedString, Window, div, prelude::*,
    px,
};

use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};

struct Tooltip {
    text: SharedString,
    key: Option<SharedString>,
}

/// For the `tooltip` of an element.
pub fn tooltip(
    text: impl Into<SharedString>,
    key: Option<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| {
        let (text, key) = (text.clone(), key.clone());
        cx.new(|_| Tooltip { text, key }).into()
    }
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = theme();
        // The margin keeps it clear of the pointer.
        div().pt(px(6.0)).pl(px(2.0)).child(
            div()
                .flex()
                .gap_2()
                .px_2()
                .py_1()
                .rounded(RADIUS)
                .border_1()
                .border_color(theme.border)
                .bg(theme.elevated)
                .shadow_md()
                .text_size(SMALL_FONT_SIZE)
                .text_color(theme.text)
                .child(self.text.clone())
                .children(
                    self.key
                        .clone()
                        .map(|key| div().text_color(theme.faint).child(key)),
                ),
        )
    }
}
