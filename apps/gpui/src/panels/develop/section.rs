//! A section of the panel: its header and its rows.

use gpui::{
    App, Context, IntoElement, KeyBinding, MouseButton, Role, SharedString, Window, div,
    prelude::*, px,
};
use rohditor_edit::{EditRecipe, LensCorrection, RecipeSection};
use rohditor_engine::open_document::NoiseSuggestion;
use rohditor_engine::tools::Picker;

use super::DevelopPanel;
use super::choices::Choice;
use super::focus::Control;
use super::row::ResetValue;
use crate::actions::{AutoNoiseReduction, PickMixerColor, PickWhiteBalance};
use crate::assets::Icon;
use crate::panels::adjustments::{Group, Section, section_hint};
use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::button::{command_button, icon, icon_button, link};
use crate::ui::test_id::TestId as _;
use crate::ui::tooltip::tooltip;

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

    /// Whether the rows of the section are shown. Always while searching.
    fn is_open(&self, section: RecipeSection, cx: &Context<Self>) -> bool {
        self.searching(cx) || !self.collapsed.contains(&section)
    }

    /// The group's title and sections, or `None` when the filter hides all of them.
    pub(super) fn group(
        &self,
        group: &Group,
        recipe: &EditRecipe,
        defaults: &EditRecipe,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let sections: Vec<_> = group
            .sections
            .iter()
            .filter_map(|section| self.section(group, section, recipe, defaults, cx))
            .collect();
        if sections.is_empty() {
            return None;
        }
        let theme = theme();
        let title = div()
            .test_id(format!("group:{}", group.title))
            .relative()
            .mx_2()
            .mb_1()
            .px_1()
            .pb_1()
            .border_b_1()
            .border_color(theme.border)
            .text_color(theme.text)
            .child(group.title);
        Some(
            div()
                .id(group.title)
                .role(Role::Group)
                .aria_label(group.title)
                .pt_2()
                .child(title)
                .children(sections),
        )
    }

    /// The header and the visible rows, or `None` when the filter hides all of them.
    pub(super) fn section(
        &self,
        group: &Group,
        section: &Section,
        recipe: &EditRecipe,
        defaults: &EditRecipe,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let changed = section.section.is_changed(recipe, defaults);
        let shown: Vec<_> = section
            .adjustments
            .iter()
            .filter(|adjustment| self.shows(group, section, adjustment, recipe, defaults, cx))
            .collect();
        let choices: Vec<_> = Choice::ALL
            .into_iter()
            .filter(|choice| self.shows_choice(*choice, group, section, recipe, defaults, cx))
            .collect();
        let wheels = self.shows_wheels(group, section, recipe, defaults, cx);
        let corrections: Vec<_> = LensCorrection::ALL
            .into_iter()
            .filter(|one| self.shows_correction(*one, group, section, recipe, defaults, cx))
            .collect();
        // Also without a row for what is changed.
        let found = changed && self.finds(group, section, "", cx);
        if shown.is_empty() && choices.is_empty() && corrections.is_empty() && !wheels && !found {
            return None;
        }
        let open = self.is_open(section.section, cx);
        let curve = (open && section.section == RecipeSection::ToneCurve && !shown.is_empty())
            .then(|| self.tone_curve(&recipe.light.tone_curve, cx));
        let mixer = open
            && section.section == RecipeSection::ColorMixer
            && !self.edited_only
            && !self.searching(cx);
        let mixer = mixer.then(|| self.mixer_channels(recipe, defaults, cx));
        let wheels = (open && wheels).then(|| self.wheels(recipe, cx));
        let choices: Vec<_> = choices
            .into_iter()
            .filter(|_| open)
            .map(|choice| self.choice(choice, recipe, defaults, cx))
            .collect();
        let rows: Vec<_> = if !open {
            Vec::new()
        } else {
            shown
                .into_iter()
                .map(|adjustment| self.row(*adjustment, recipe, defaults, cx))
                .collect()
        };
        let notes =
            (open && section.section == RecipeSection::Optics).then(|| self.lens_notes(recipe));
        let corrections: Vec<_> = corrections
            .into_iter()
            .filter(|_| open)
            .map(|correction| self.correction(correction, recipe, cx))
            .collect();
        let header = self.section_header(section.section, changed, cx);
        Some(
            div()
                .id(SharedString::from(format!("{:?}", section.section)))
                .role(Role::Group)
                .aria_label(section.section.label())
                .flex()
                .flex_col()
                .gap(px(3.0))
                .px_2()
                .pb_2()
                .child(header)
                .children(curve)
                .children(choices)
                .children(notes)
                .children(corrections)
                .children(mixer)
                .children(wheels)
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
            .children(self.picker_button(section))
            .children(self.auto_button(section))
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

    /// Starts the section's picker, if it has one.
    fn picker_button(&self, section: RecipeSection) -> Option<impl IntoElement + use<>> {
        let (picker, button) = match section {
            RecipeSection::WhiteBalance => (
                Picker::WhiteBalance,
                command_button("pick-white-balance", Icon::Pipette, PickWhiteBalance, true),
            ),
            RecipeSection::ColorMixer => (
                Picker::ColorMixer,
                command_button("pick-mixer-color", Icon::Pipette, PickMixerColor, true),
            ),
            _ => return None,
        };
        let active = self.model.picker == Some(picker);
        Some(button.when(active, |button| button.bg(theme().selected)))
    }

    /// Sets the color noise reduction from the photo's noise.
    fn auto_button(&self, section: RecipeSection) -> Option<impl IntoElement + use<>> {
        if section != RecipeSection::NoiseReduction {
            return None;
        }
        let suggestion = self.model.noise?;
        let label = match suggestion {
            NoiseSuggestion::Estimating => "Estimating…",
            _ => "Auto",
        };
        let enabled = !matches!(
            suggestion,
            NoiseSuggestion::Estimating | NoiseSuggestion::Unavailable
        );
        Some(link("auto-noise", label, AutoNoiseReduction, enabled))
    }

    /// The title, which collapses the section.
    fn section_title(
        &self,
        section: RecipeSection,
        changed: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = theme();
        let collapsed = !self.is_open(section, cx);
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
            .when_some(section_hint(section), |header, hint| {
                header.tooltip(tooltip(hint, None))
            })
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
