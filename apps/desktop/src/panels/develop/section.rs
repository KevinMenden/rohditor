//! A section of the panel: its header and its rows.

use gpui::{Context, IntoElement, Role, SharedString, Window, div, prelude::*, px};
use rohditor_edit::{EditRecipe, LensCorrection, RecipeSection};
use rohditor_engine::open_document::NoiseSuggestion;
use rohditor_engine::tools::Picker;

use super::DevelopPanel;
use super::choices::Choice;
use super::control::Control;
use super::filter::has_effect;
use crate::actions::{AutoNoiseReduction, PickMixerColor, PickWhiteBalance};
use crate::assets::Icon;
use crate::panels::adjustments::{Adjustment, Group, Section, section_hint};
use crate::panels::controls::{
    ControlPanel as _, Gestures as _, Header, ResetValue, RowState, SectionHeaders as _,
    SliderRows as _,
};
use crate::theme::theme;
use crate::ui::button::{command_button, link};
use crate::ui::test_id::TestId as _;
use crate::ui::tooltip::tooltip;

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
        let typed = self.controls.typed().and_then(Control::section);
        if typed == Some(section) {
            self.stop_typing(false, window, cx);
        }
        self.session
            .update(cx, |session, cx| session.reset_section(section, cx));
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
        let curve = self.shows_curve(group, section, recipe, defaults, cx);
        let corrections: Vec<_> = LensCorrection::ALL
            .into_iter()
            .filter(|one| self.shows_correction(*one, group, section, recipe, defaults, cx))
            .collect();
        // Also without a row for what is changed.
        let found = changed && self.finds(group, section, "", cx);
        let nothing = shown.is_empty() && choices.is_empty() && corrections.is_empty();
        if nothing && !wheels && !curve && !found {
            return None;
        }
        let open = self.is_open(Control::Section(section.section), cx);
        let curve = open && curve;
        let curve_rows = if curve {
            self.tone_curve_rows(&recipe.light.tone_curve, cx)
        } else {
            Vec::new()
        };
        let histogram = self.model.histogram.as_ref();
        let curve = curve.then(|| self.tone_curve(&recipe.light.tone_curve, histogram, cx));
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
                .children(curve_rows)
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
        let header = Header {
            control: Control::Section(section),
            title: section.label(),
            id: format!("{section:?}").into(),
            changed,
        };
        let reset = self.reset_button(
            &header,
            move |this, window, cx| this.reset_section(section, window, cx),
            cx,
        );
        let title = self
            .title(header, cx)
            .when_some(section_hint(section), |title, hint| {
                title.tooltip(tooltip(hint, None))
            })
            .on_action(cx.listener(move |this, _: &ResetValue, window, cx| {
                this.reset_section(section, window, cx);
            }));
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(title)
            .children(self.picker_button(section))
            .children(self.auto_button(section))
            .children(reset)
    }

    fn row(
        &self,
        adjustment: Adjustment,
        recipe: &EditRecipe,
        defaults: &EditRecipe,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let parameter = adjustment.parameter;
        let state = RowState {
            value: parameter.get(recipe, defaults),
            origin: parameter.get(defaults, defaults),
            edited: parameter.is_changed(recipe, defaults),
            effective: has_effect(parameter, recipe, defaults),
        };
        let control = Control::Parameter(parameter);
        let id = format!("{parameter:?}");
        self.slider_row(control, &id, adjustment.slider(), state, cx)
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
}
