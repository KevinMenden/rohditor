//! Which adjustments the panel lists: a search and "Edited only".

use gpui::{App, Context, Focusable as _, IntoElement, SharedString, Window, div, prelude::*, px};
use rohditor_edit::{EditRecipe, GradingTones, HighlightMethod, Parameter, RecipeSection};

use super::DevelopPanel;
use super::choices::Choice;
use super::mixer::channel_of;
use crate::actions::FocusPhoto;
use crate::assets::Icon;
use crate::panels::adjustments::{Adjustment, Group, Section};
use crate::panels::controls::FocusControls as _;
use crate::theme::{RADIUS, theme};
use crate::ui::button::{icon, icon_button};
use crate::ui::keys::{Cancel, Confirm, SelectNext};
use crate::ui::matching;
use crate::ui::test_id::TestId as _;
use crate::ui::toggle::toggle;

impl DevelopPanel {
    pub(super) fn query(&self, cx: &App) -> SharedString {
        self.search.read(cx).text().to_owned().into()
    }

    pub(super) fn searching(&self, cx: &App) -> bool {
        !self.query(cx).trim().is_empty()
    }

    /// Whether the search finds the control `label`, or a section as such when it is empty.
    pub(super) fn finds(
        &self,
        group: &Group,
        section: &Section,
        label: &str,
        cx: &Context<Self>,
    ) -> bool {
        let text = format!("{} {} {label}", group.title, section.section.label());
        matching::words(&text, &self.query(cx))
    }

    /// Whether the panel shows this adjustment.
    pub(super) fn shows(
        &self,
        group: &Group,
        section: &Section,
        adjustment: &Adjustment,
        recipe: &EditRecipe,
        defaults: &EditRecipe,
        cx: &Context<Self>,
    ) -> bool {
        let parameter = adjustment.parameter;
        if !applies(parameter, recipe) {
            return false;
        }
        // A slider must not vanish while it is changed.
        let active = [self.controls.dragged(), self.controls.typed()]
            .into_iter()
            .flatten()
            .any(|control| control.parameter() == Some(parameter));
        let passes = !self.edited_only || parameter.is_changed(recipe, defaults);
        // Searches and the filter list the sliders of all colours.
        let listed = self.edited_only
            || self.searching(cx)
            || channel_of(parameter).is_none_or(|channel| channel == self.mixer_channel);
        active || (listed && passes && self.finds(group, section, adjustment.label, cx))
    }

    pub(super) fn shows_choice(
        &self,
        choice: Choice,
        group: &Group,
        section: &Section,
        recipe: &EditRecipe,
        defaults: &EditRecipe,
        cx: &Context<Self>,
    ) -> bool {
        choice.section() == section.section
            && (!self.edited_only || choice.is_changed(recipe, defaults))
            && self.finds(group, section, choice.label(), cx)
    }

    pub(super) fn shows_wheels(
        &self,
        group: &Group,
        section: &Section,
        recipe: &EditRecipe,
        defaults: &EditRecipe,
        cx: &Context<Self>,
    ) -> bool {
        let grading = RecipeSection::ColorGrading;
        section.section == grading
            && (!self.edited_only || grading.is_changed(recipe, defaults))
            && GradingTones::ALL
                .iter()
                .any(|tones| self.finds(group, section, tones.label(), cx))
    }

    pub fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.focus_handle(cx).focus(window, cx);
    }

    pub(super) fn filter(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = theme();
        let search = div()
            .test_id("search")
            .relative()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_1()
            .px_1p5()
            .h(px(26.0))
            .rounded(RADIUS)
            .bg(theme.editor)
            .child(icon(Icon::Search, theme.faint))
            .child(div().flex_1().min_w_0().child(self.search.clone()))
            .when(self.searching(cx), |search| {
                search.child(icon_button(
                    "clear-search",
                    Icon::Close,
                    "Clear Search",
                    true,
                    cx.listener(|this, _, _, cx| {
                        this.search.update(cx, |search, cx| search.clear(cx));
                    }),
                ))
            })
            .on_action(cx.listener(|this, _: &Cancel, window, cx| {
                if this.search.read(cx).text().is_empty() {
                    window.dispatch_action(Box::new(FocusPhoto), cx);
                } else {
                    this.search.update(cx, |search, cx| search.clear(cx));
                }
            }))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| {
                this.focus_first(window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectNext, window, cx| {
                this.focus_first(window, cx);
            }));
        let panel = cx.entity();
        let edited_only = toggle(
            "edited-only",
            "Edited only",
            self.edited_only,
            move |_, cx| {
                panel.update(cx, |panel, cx| {
                    panel.edited_only = !panel.edited_only;
                    cx.notify();
                });
            },
        );
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .px_2()
            .child(search)
            .child(edited_only)
    }
}

/// Whether the parameter does something with the choices made in `recipe`.
fn applies(parameter: Parameter, recipe: &EditRecipe) -> bool {
    let method = recipe.raw.highlights.method;
    match parameter {
        Parameter::HighlightClipThreshold => method == HighlightMethod::Clip,
        Parameter::HighlightDetectionThreshold => method == HighlightMethod::Opposed,
        _ => true,
    }
}

/// Whether the parameter does something with the amount it depends on. Unlike one that does not
/// apply, it stays listed: turning up the amount must not move the rows below.
pub(super) fn has_effect(parameter: Parameter, recipe: &EditRecipe, defaults: &EditRecipe) -> bool {
    let amount = match parameter {
        Parameter::LuminanceDetail => Parameter::LuminanceNoiseReduction,
        Parameter::CaptureRadius | Parameter::CaptureNoise => Parameter::CaptureAmount,
        Parameter::DefringePurpleHueStart | Parameter::DefringePurpleHueEnd => {
            Parameter::DefringePurpleAmount
        }
        Parameter::DefringeGreenHueStart | Parameter::DefringeGreenHueEnd => {
            Parameter::DefringeGreenAmount
        }
        _ => return true,
    };
    amount.get(recipe, defaults) > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn details_need_their_amount() {
        let mut recipe = EditRecipe::default();
        let defaults = recipe.clone();
        let radius = Parameter::CaptureRadius;
        assert!(has_effect(Parameter::CaptureAmount, &recipe, &defaults));
        assert!(!has_effect(radius, &recipe, &defaults));
        recipe = Parameter::CaptureAmount.with_value(&recipe, &defaults, 0.3);
        assert!(has_effect(radius, &recipe, &defaults));
        let green = Parameter::DefringeGreenHueEnd;
        recipe.defringe.purple_amount = 0.3;
        assert!(!has_effect(green, &recipe, &defaults));
        recipe.defringe.green_amount = 0.3;
        assert!(has_effect(green, &recipe, &defaults));
    }

    #[test]
    fn only_the_threshold_of_the_chosen_method_applies() {
        let clip = Parameter::HighlightClipThreshold;
        let detection = Parameter::HighlightDetectionThreshold;
        for (method, applying) in [
            (HighlightMethod::Off, [false, false]),
            (HighlightMethod::Clip, [true, false]),
            (HighlightMethod::Opposed, [false, true]),
        ] {
            let mut recipe = EditRecipe::default();
            recipe.raw.highlights.method = method;
            assert_eq!([clip, detection].map(|p| applies(p, &recipe)), applying);
        }
    }
}
