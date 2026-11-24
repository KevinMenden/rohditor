//! The lens profile's corrections, and searching for a lens.

use gpui::{Context, IntoElement, SharedString, Window, div, prelude::*, px};
use rohditor_edit::{EditRecipe, LensCorrection, LensProfileSelection, RecipeSection};

use super::DevelopPanel;
use super::focus::Control;
use crate::actions::ChooseLens;
use crate::panels::adjustments::{Group, Section};
use crate::theme::{SMALL_FONT_SIZE, theme};
use crate::ui::matching;
use crate::ui::menu::MenuEntry;
use crate::ui::overlays::ToastKind;
use crate::ui::search_list::{SearchEntry, SearchList};
use crate::ui::toggle::toggle;

const fn toggle_id(correction: LensCorrection) -> &'static str {
    match correction {
        LensCorrection::Distortion => "lens-distortion",
        LensCorrection::Vignetting => "lens-vignetting",
        LensCorrection::ChromaticAberration => "lens-chromatic-aberration",
    }
}

fn by_hand(id: &str) -> LensProfileSelection {
    LensProfileSelection::Lensfun {
        profile_id: id.to_owned(),
    }
}

impl DevelopPanel {
    pub(super) fn lens_profile_label(&self, recipe: &EditRecipe) -> SharedString {
        let selection = &recipe.optics.profile;
        let profile = self.model.lens_profiles.profile(selection);
        match (selection, profile) {
            (LensProfileSelection::Off, _) => "Off".into(),
            (LensProfileSelection::Automatic, _) => "Automatic".into(),
            (LensProfileSelection::Lensfun { .. }, Some(profile)) => profile.lens.clone().into(),
            (LensProfileSelection::Lensfun { .. }, None) => "Unknown lens".into(),
        }
    }

    /// The entries of the profile dropdown. All other profiles are behind a search.
    pub(super) fn lens_entries(
        &self,
        recipe: &EditRecipe,
        cx: &mut Context<Self>,
    ) -> Vec<MenuEntry> {
        let profiles = self.model.lens_profiles.clone();
        let selected = &recipe.optics.profile;
        let mut listed = vec![(LensProfileSelection::Off, SharedString::from("Off"))];
        if let Some(detected) = profiles.detected() {
            let label = format!("Automatic ({})", detected.lens);
            listed.push((LensProfileSelection::Automatic, label.into()));
        }
        let candidates = profiles.ambiguous().iter();
        listed
            .extend(candidates.map(|profile| (by_hand(&profile.id), profile.lens.clone().into())));
        if !listed.iter().any(|(selection, _)| selection == selected) {
            listed.push((selected.clone(), self.lens_profile_label(recipe)));
        }
        let mut entries: Vec<_> = listed
            .into_iter()
            .map(|(selection, label)| {
                let current = selection == *selected;
                self.entry(label, current, cx, move |session, cx| {
                    session.set_lens_profile(selection.clone(), cx);
                })
            })
            .collect();
        if !profiles.choices.is_empty() {
            entries.push(MenuEntry::new("Choose Lens…", |window, cx| {
                window.dispatch_action(Box::new(ChooseLens), cx);
            }));
        }
        entries
    }

    /// Search all lenses with a profile for the photo.
    pub fn choose_lens(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let profiles = &self.model.lens_profiles;
        if profiles.choices.is_empty() {
            let text = profiles
                .problem()
                .unwrap_or_else(|| "There are no lens profiles for the photo".to_owned());
            return self.overlays.update(cx, |overlays, cx| {
                overlays.toast(ToastKind::Error, text, cx);
            });
        }
        let selection = self
            .model
            .recipes
            .as_ref()
            .map(|(recipe, _)| &recipe.optics.profile);
        let current = selection
            .and_then(|selection| profiles.profile(selection))
            .or_else(|| profiles.detected())
            .and_then(|current| profiles.choices.iter().position(|one| one.id == current.id));
        let panel = cx.entity();
        let entries = profiles.choices.iter().map(|profile| {
            let panel = panel.clone();
            let id = profile.id.clone();
            // A lens is listed once for each mount it can be adapted from.
            let mount = profile.mount.clone();
            SearchEntry::new(profile.lens.clone(), Some(mount.into()), move |_, cx| {
                panel.update(cx, |panel, cx| {
                    panel.choose(|session, cx| session.set_lens_profile(by_hand(&id), cx), cx);
                });
            })
        });
        let entries = entries.collect();
        let list = cx.new(|cx| {
            SearchList::new(
                "Choose lens",
                "Search lenses…",
                entries,
                matching::words,
                cx,
            )
            .at(current.unwrap_or(0))
        });
        self.overlays.update(cx, |overlays, cx| {
            overlays.show_dialog(list, window, cx);
        });
    }

    pub(super) fn shows_correction(
        &self,
        correction: LensCorrection,
        group: &Group,
        section: &Section,
        recipe: &EditRecipe,
        defaults: &EditRecipe,
        cx: &Context<Self>,
    ) -> bool {
        let changed = correction.is_on(&recipe.optics) != correction.is_on(&defaults.optics);
        section.section == RecipeSection::Optics
            && (!self.edited_only || changed)
            && self.finds(group, section, correction.label(), cx)
    }

    /// Switches a correction of the profile. Without one, or where the profile lacks it, the
    /// choice is kept but does nothing.
    pub(super) fn correction(
        &self,
        correction: LensCorrection,
        recipe: &EditRecipe,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let on = correction.is_on(&recipe.optics);
        let offered = self
            .model
            .lens_profiles
            .offers(&recipe.optics.profile, correction);
        let label = if offered == Some(false) {
            format!("{} (not in this profile)", correction.label())
        } else {
            correction.label().to_owned()
        };
        let control = Control::Correction(correction);
        let panel = cx.entity();
        toggle(toggle_id(correction), label, on, move |_, cx| {
            if offered != Some(false) {
                panel.update(cx, |panel, cx| {
                    panel.choose(|s, cx| s.set_lens_correction(correction, !on, cx), cx);
                });
            }
        })
        .when(offered != Some(true), |toggle| toggle.opacity(0.5))
        .track_focus(self.focus.control(control))
        .relative()
        .pl_2()
        .child(self.revealer(control, cx))
    }

    /// The captured lens and what keeps it from being corrected.
    pub(super) fn lens_notes(&self, recipe: &EditRecipe) -> impl IntoElement + use<> {
        let theme = theme();
        let profiles = &self.model.lens_profiles;
        let muted = |text: String| (text, theme.faint);
        let profile = profiles.profile(&recipe.optics.profile);
        let lens = match (&recipe.optics.profile, profile, &self.model.lens) {
            // By name of the profile that was found, which can differ from the captured one.
            (LensProfileSelection::Automatic, Some(profile), _) => Some(profile.lens.clone()),
            (_, _, Some(captured)) => Some(format!("Taken with {captured}")),
            _ => None,
        };
        let chosen = matches!(recipe.optics.profile, LensProfileSelection::Lensfun { .. });
        let problem = profiles
            .problem()
            .filter(|_| !chosen)
            .map(|problem| (problem, theme.warning));
        let unfocused = self.model.infinity_vignetting.then(|| {
            muted("No focus distance: vignetting is corrected as if at infinity".to_owned())
        });
        let notes = [lens.map(muted), problem, unfocused];
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .pl_2()
            .text_size(SMALL_FONT_SIZE)
            .children(
                notes
                    .into_iter()
                    .flatten()
                    .map(|(text, color)| div().text_color(color).child(text)),
            )
    }
}
