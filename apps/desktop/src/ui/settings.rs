//! Presentation-only application settings dialog.

use eframe::egui;
use rohditor_demosaic::DemosaicAlgorithm;

use super::theme::colors;

#[derive(Debug, Clone)]
pub(crate) struct SettingsWindowModel<'a> {
    pub active_demosaic: DemosaicAlgorithm,
    pub draft_demosaic: DemosaicAlgorithm,
    pub warning: Option<&'a str>,
}

#[derive(Debug, Default)]
pub(crate) struct SettingsWindowOutput {
    pub selected_demosaic: Option<DemosaicAlgorithm>,
    pub cancel: bool,
    pub apply: bool,
}

pub(crate) fn show(
    context: &egui::Context,
    open: &mut bool,
    model: &SettingsWindowModel<'_>,
) -> SettingsWindowOutput {
    let mut output = SettingsWindowOutput::default();
    let was_open = *open;
    egui::Window::new("Settings")
        .open(open)
        .default_width(360.0)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(context, |ui| {
            ui.strong("Processing");
            ui.add_space(6.0);
            ui.label("Demosaic algorithm");

            let mut selected = model.draft_demosaic;
            ui.radio_value(
                &mut selected,
                DemosaicAlgorithm::MalvarHeCutler,
                "Malvar-He-Cutler (default)",
            );
            ui.radio_value(&mut selected, DemosaicAlgorithm::Rcd, "RCD");
            ui.radio_value(&mut selected, DemosaicAlgorithm::Amaze, "AMaZE");
            if model.draft_demosaic == DemosaicAlgorithm::Bilinear {
                ui.label(
                    egui::RichText::new("Bilinear is active for this command-line override.")
                        .color(colors::TEXT_MUTED),
                );
            }
            if selected != model.draft_demosaic {
                output.selected_demosaic = Some(selected);
            }

            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Used for preview, Source 1:1, and export.")
                    .color(colors::TEXT_MUTED),
            );
            ui.label(
                egui::RichText::new("Changing this rebuilds the current developed image.")
                    .color(colors::TEXT_MUTED),
            );
            if let Some(warning) = model.warning {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(warning).color(colors::WARNING));
            }

            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let dirty = model.draft_demosaic != model.active_demosaic;
                output.apply = ui.add_enabled(dirty, egui::Button::new("Apply")).clicked();
                output.cancel = ui.button("Cancel").clicked();
            });
        });

    if was_open && !*open {
        output.cancel = true;
    }
    if context.input(|input| input.key_pressed(egui::Key::Escape)) {
        output.cancel = true;
    }
    if model.draft_demosaic != model.active_demosaic
        && context.input(|input| input.key_pressed(egui::Key::Enter))
    {
        output.apply = true;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_normal_editor_algorithms_are_offered() {
        let choices = [
            DemosaicAlgorithm::MalvarHeCutler,
            DemosaicAlgorithm::Rcd,
            DemosaicAlgorithm::Amaze,
        ];
        assert!(!choices.contains(&DemosaicAlgorithm::Bilinear));
    }
}
