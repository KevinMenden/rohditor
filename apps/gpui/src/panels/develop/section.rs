//! A section of the panel: its header and its rows.

use gpui::{App, Context, IntoElement, KeyBinding, MouseButton, Role, Window, div, prelude::*, px};
use rohditor_edit::{EditRecipe, RecipeSection};

use super::DevelopPanel;
use super::focus::Control;
use super::row::ResetValue;
use crate::assets::Icon;
use crate::panels::adjustments::Section;
use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::button::{icon, icon_button};
use crate::ui::test_id::TestId as _;

const CONTEXT: &str = "SectionHeader";

pub(super) fn bind_keys(cx: &mut App) {
    let header = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("delete", ResetValue, header),
        KeyBinding::new("backspace", ResetValue, header),
    ]);
}

impl DevelopPanel {
    /// Restore the photo's defaults in this section.
    pub fn reset_section(
        &mut self,
        section: RecipeSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish_nudge(cx);
        // What is being typed must not be applied afterwards.
        if self
            .typing
            .as_ref()
            .is_some_and(|typing| typing.adjustment.parameter.section() == section)
        {
            self.stop_typing(false, window, cx);
        }
        self.session
            .update(cx, |session, cx| session.reset_section(section, cx));
    }

    fn toggle_section(&mut self, section: RecipeSection, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&section) {
            self.collapsed.insert(section);
        }
        cx.notify();
    }

    /// The header and the visible rows, or `None` when the filter hides all of them.
    pub(super) fn section(
        &self,
        section: &Section,
        recipe: &EditRecipe,
        defaults: &EditRecipe,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let changed = section.section.is_changed(recipe, defaults);
        let shown: Vec<_> = section
            .adjustments
            .iter()
            .filter(|adjustment| self.shows(adjustment, recipe, defaults))
            .collect();
        if shown.is_empty() && !changed {
            return None;
        }
        let rows: Vec<_> = if self.collapsed.contains(&section.section) {
            Vec::new()
        } else {
            shown
                .into_iter()
                .map(|adjustment| self.row(*adjustment, recipe, defaults, cx))
                .collect()
        };
        let header = self.section_header(section.section, changed, cx);
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .px_2()
                .pb_2()
                .child(header)
                .children(rows),
        )
    }

    fn section_header(
        &self,
        section: RecipeSection,
        changed: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(self.section_title(section, changed, cx))
            .when(changed, |header| {
                let label = format!("Reset {}", section.label());
                let reset = icon_button(
                    format!("reset-{section:?}"),
                    Icon::Reset,
                    label,
                    true,
                    cx.listener(move |this, _, window, cx| this.reset_section(section, window, cx)),
                );
                // By keyboard, Delete on the header does the same.
                header.child(reset.tab_stop(false))
            })
    }

    /// The title, which collapses the section.
    fn section_title(
        &self,
        section: RecipeSection,
        changed: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = theme();
        let collapsed = self.collapsed.contains(&section);
        let chevron = if collapsed {
            Icon::ChevronRight
        } else {
            Icon::ChevronDown
        };
        let title = section.label();
        let described = if changed {
            format!("{title}, edited")
        } else {
            title.to_owned()
        };
        let control = Control::Section(section);
        div()
            .id(title)
            .role(Role::Button)
            .aria_label(described)
            .aria_expanded(!collapsed)
            .key_context(CONTEXT)
            .track_focus(self.focus.control(control))
            .relative()
            .child(self.revealer(control, cx))
            .rounded(RADIUS)
            .border_1()
            .border_color(gpui::transparent_black())
            .focus_visible(move |style| style.border_color(theme.accent))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .test_id(format!("section:{section:?}"))
            .flex()
            .flex_1()
            .items_center()
            .gap_1()
            .h(px(28.0))
            .cursor_pointer()
            .text_size(SMALL_FONT_SIZE)
            .text_color(if changed { theme.text } else { theme.muted })
            .child(icon(chevron, theme.faint))
            .child(title)
            .when(changed, |header| {
                header.child(
                    div()
                        .test_id(format!("changed:{section:?}"))
                        .relative()
                        .ml_1()
                        .size(px(5.0))
                        .rounded_full()
                        .bg(theme.accent),
                )
            })
            .on_action(cx.listener(move |this, _: &ResetValue, window, cx| {
                this.reset_section(section, window, cx);
            }))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_section(section, cx)))
    }
}
