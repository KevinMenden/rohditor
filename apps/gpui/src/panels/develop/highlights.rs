//! The method of the highlight reconstruction.

use gpui::{Context, IntoElement, div, prelude::*, px};
use rohditor_edit::{EditRecipe, HighlightMethod};

use super::DevelopPanel;
use super::focus::Control;
use crate::panels::adjustments::{HIGHLIGHT_METHODS, method_label};
use crate::theme::theme;
use crate::ui::dropdown::dropdown;
use crate::ui::menu::MenuEntry;

impl DevelopPanel {
    pub fn set_highlight_method(&mut self, method: HighlightMethod, cx: &mut Context<Self>) {
        self.finish_nudge(cx);
        self.session.update(cx, |session, cx| {
            session.set_highlight_method(method, cx);
        });
    }

    pub(super) fn highlight_method(
        &self,
        recipe: &EditRecipe,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let current = recipe.raw.highlights.method;
        let panel = cx.entity();
        let choices = dropdown(
            "highlight-method",
            "Highlight reconstruction method",
            method_label(current),
            self.overlays.clone(),
            move |_, _| {
                let entries = HIGHLIGHT_METHODS.into_iter().map(|method| {
                    let panel = panel.clone();
                    MenuEntry::new(method_label(method), move |_, cx| {
                        panel.update(cx, |panel, cx| panel.set_highlight_method(method, cx));
                    })
                    .checked(method == current)
                });
                entries.collect()
            },
        );
        let control = Control::HighlightMethod;
        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(24.0))
            .pl_2()
            .text_color(theme().muted)
            .child("Method")
            .child(
                choices
                    .track_focus(self.focus.control(control))
                    .child(self.revealer(control, cx)),
            )
    }
}
