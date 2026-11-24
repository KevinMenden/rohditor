//! The Presets section of the Edit panel: apply a saved preset at some
//! strength, delete one, or save the current edits as a new one.
//!
//! The presets themselves live in `rohditor_engine::presets`; this module only
//! draws the list and the save form and reports what was asked for.

use eframe::egui;
use rohditor_edit::RecipeSection;
use rohditor_engine::presets::Preset;

use super::theme::colors;

/// What the user asked the Presets section to do this frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PresetsAction {
    /// Apply the named preset at `amount` (`0.0..=1.0`) as one undo step.
    Apply {
        name: String,
        amount: f32,
    },
    Delete(String),
    /// Save the open photo's chosen sections under `name`.
    Save {
        name: String,
        sections: Vec<RecipeSection>,
    },
}

/// What the section remembers between frames.
#[derive(Debug)]
pub(crate) struct PresetsPanelState {
    /// Blend strength for the next apply, `0.0..=1.0`.
    amount: f32,
    save_form: Option<SaveForm>,
    /// The last failed save, delete or apply, shown under the list.
    pub error: Option<String>,
}

impl Default for PresetsPanelState {
    fn default() -> Self {
        Self {
            amount: 1.0,
            save_form: None,
            error: None,
        }
    }
}

impl PresetsPanelState {
    /// Close the save form, for example after a successful save.
    pub(crate) fn close_save_form(&mut self) {
        self.save_form = None;
    }
}

#[derive(Debug)]
struct SaveForm {
    name: String,
    /// Every section with whether it goes into the preset.
    sections: Vec<(RecipeSection, bool)>,
}

impl SaveForm {
    fn new(suggested: &[RecipeSection]) -> Self {
        Self {
            name: String::new(),
            sections: RecipeSection::ALL
                .into_iter()
                .map(|section| (section, suggested.contains(&section)))
                .collect(),
        }
    }
}

/// Draws the section body. The save form starts from the `suggested` sections.
pub(crate) fn show(
    ui: &mut egui::Ui,
    state: &mut PresetsPanelState,
    presets: &[Preset],
    suggested: &[RecipeSection],
) -> Option<PresetsAction> {
    let mut action = None;

    if presets.is_empty() {
        ui.label(egui::RichText::new("No presets yet").color(colors::TEXT_MUTED));
    } else {
        ui.add(
            egui::Slider::new(&mut state.amount, 0.0..=1.0)
                .text("Amount")
                .custom_formatter(|value, _| format!("{:.0}%", value * 100.0)),
        )
        .on_hover_text("How strongly a preset is applied. 100% takes its values exactly.");
        for preset in presets {
            ui.horizontal(|ui| {
                let covers = preset
                    .sections()
                    .iter()
                    .map(|section| section.label())
                    .collect::<Vec<_>>()
                    .join(", ");
                let apply = ui
                    .add(
                        egui::Button::new(preset.name())
                            .min_size(egui::vec2(ui.available_width() - 28.0, 0.0)),
                    )
                    .on_hover_text(format!("Apply: {covers}"));
                if apply.clicked() {
                    action = Some(PresetsAction::Apply {
                        name: preset.name().to_owned(),
                        amount: state.amount,
                    });
                }
                if ui
                    .small_button("×")
                    .on_hover_text("Delete preset")
                    .clicked()
                {
                    action = Some(PresetsAction::Delete(preset.name().to_owned()));
                }
            });
        }
    }

    ui.add_space(4.0);
    match &mut state.save_form {
        None => {
            if ui.button("Save current edits as preset…").clicked() {
                state.save_form = Some(SaveForm::new(suggested));
                state.error = None;
            }
        }
        Some(form) => {
            let mut close = false;
            if let Some(save) = save_form(ui, form) {
                match save {
                    FormResult::Save => {
                        action = Some(PresetsAction::Save {
                            name: form.name.clone(),
                            sections: form
                                .sections
                                .iter()
                                .filter(|(_, included)| *included)
                                .map(|(section, _)| *section)
                                .collect(),
                        });
                    }
                    FormResult::Cancel => close = true,
                }
            }
            if close {
                state.save_form = None;
            }
        }
    }

    if let Some(error) = &state.error {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(error).small().color(colors::TEXT_MUTED));
    }
    action
}

enum FormResult {
    Save,
    Cancel,
}

fn save_form(ui: &mut egui::Ui, form: &mut SaveForm) -> Option<FormResult> {
    ui.add(
        egui::TextEdit::singleline(&mut form.name)
            .hint_text("Preset name")
            .desired_width(f32::INFINITY),
    );
    ui.label(
        egui::RichText::new("Include")
            .small()
            .color(colors::TEXT_MUTED),
    );
    for (section, included) in &mut form.sections {
        ui.checkbox(included, section.label());
    }
    let can_save =
        !form.name.trim().is_empty() && form.sections.iter().any(|(_, included)| *included);
    let mut result = None;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(can_save, egui::Button::new("Save"))
            .clicked()
        {
            result = Some(FormResult::Save);
        }
        if ui.button("Cancel").clicked() {
            result = Some(FormResult::Cancel);
        }
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the section in a headless harness (no GPU needed) and returns the
    /// harness plus the cell the last reported action lands in.
    fn harness(
        presets: Vec<Preset>,
    ) -> (egui_kittest::Harness<'static, (PresetsPanelState, Option<PresetsAction>)>,) {
        let harness = egui_kittest::Harness::new_ui_state(
            move |ui, (state, action): &mut (PresetsPanelState, Option<PresetsAction>)| {
                if let Some(next) = show(ui, state, &presets, &[RecipeSection::Tone]) {
                    *action = Some(next);
                }
            },
            (PresetsPanelState::default(), None),
        );
        (harness,)
    }

    fn preset(name: &str) -> Preset {
        let mut recipe = rohditor_edit::EditRecipe::default();
        recipe.light.exposure_ev = 1.0;
        Preset::new(name, &recipe, &[RecipeSection::Tone]).expect("valid preset")
    }

    #[test]
    fn clicking_a_preset_applies_it_at_the_chosen_amount() {
        use egui_kittest::kittest::Queryable as _;
        let (mut harness,) = harness(vec![preset("Punchy")]);
        harness.state_mut().0.amount = 0.5;
        harness.run();
        harness.get_by_label("Punchy").click();
        harness.run();
        assert_eq!(
            harness.state().1,
            Some(PresetsAction::Apply {
                name: "Punchy".to_owned(),
                amount: 0.5
            })
        );
    }

    #[test]
    fn saving_needs_a_name_and_reports_the_checked_sections() {
        use egui_kittest::kittest::Queryable as _;
        let (mut harness,) = harness(Vec::new());
        harness
            .get_by_label("Save current edits as preset…")
            .click();
        harness.run();
        // No name yet: Save is disabled and reports nothing.
        harness.get_by_label("Save").click();
        harness.run();
        assert_eq!(harness.state().1, None);

        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .focus();
        harness.run();
        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .type_text("Mine");
        harness.run();
        harness.get_by_label("Save").click();
        harness.run();
        assert_eq!(
            harness.state().1,
            Some(PresetsAction::Save {
                name: "Mine".to_owned(),
                sections: vec![RecipeSection::Tone],
            })
        );
    }
}
