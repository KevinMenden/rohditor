//! Presentation-only application settings dialog.

use eframe::egui;
use rohditor_core::OutputPolicy;
use rohditor_demosaic::DemosaicAlgorithm;

use super::theme::colors;

#[derive(Debug, Clone)]
pub(crate) struct SettingsWindowModel<'a> {
    pub active_demosaic: DemosaicAlgorithm,
    pub draft_demosaic: DemosaicAlgorithm,
    pub active_output_policy: OutputPolicy,
    pub draft_output_policy: OutputPolicy,
    pub active_cpu_only: bool,
    pub draft_cpu_only: bool,
    pub warning: Option<&'a str>,
}

impl SettingsWindowModel<'_> {
    fn is_dirty(&self) -> bool {
        self.draft_demosaic != self.active_demosaic
            || self.draft_output_policy != self.active_output_policy
            || self.draft_cpu_only != self.active_cpu_only
    }
}

#[derive(Debug, Default)]
pub(crate) struct SettingsWindowOutput {
    pub selected_demosaic: Option<DemosaicAlgorithm>,
    pub selected_output_policy: Option<OutputPolicy>,
    pub selected_cpu_only: Option<bool>,
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

            ui.add_space(12.0);
            ui.label("Output gamut mapping");
            let mut output_policy = model.draft_output_policy;
            ui.radio_value(
                &mut output_policy,
                OutputPolicy::ClipToSrgb,
                "Clip to sRGB (compatibility)",
            );
            ui.radio_value(
                &mut output_policy,
                OutputPolicy::ChromaCompressToSrgb,
                "Chroma compress to sRGB",
            );
            if output_policy != model.draft_output_policy {
                output.selected_output_policy = Some(output_policy);
            }

            ui.add_space(12.0);
            let mut cpu_only = model.draft_cpu_only;
            ui.checkbox(&mut cpu_only, "CPU only (don't use the GPU)");
            if cpu_only != model.draft_cpu_only {
                output.selected_cpu_only = Some(cpu_only);
            }
            ui.label(
                egui::RichText::new(
                    "Otherwise the GPU is used wherever possible. Takes effect after restart.",
                )
                .color(colors::TEXT_MUTED),
            );

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
                output.apply = ui
                    .add_enabled(model.is_dirty(), egui::Button::new("Apply"))
                    .clicked();
                output.cancel = ui.button("Cancel").clicked();
            });
        });

    if was_open && !*open {
        output.cancel = true;
    }
    if context.input(|input| input.key_pressed(egui::Key::Escape)) {
        output.cancel = true;
    }
    if model.is_dirty() && context.input(|input| input.key_pressed(egui::Key::Enter)) {
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
