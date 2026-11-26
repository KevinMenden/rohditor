//! The developer diagnostics window. The engine gathers the snapshot
//! ([`rohditor_engine::diagnostics`]); this only lays it out.

use eframe::egui;
use rohditor_engine::diagnostics::{Diagnostics, GpuDiagnostics};

#[derive(Debug, Default)]
pub(crate) struct DiagnosticsOutput {
    pub export_requested: bool,
}

pub(crate) fn show(
    context: &egui::Context,
    open: &mut bool,
    model: &Diagnostics,
) -> DiagnosticsOutput {
    let mut output = DiagnosticsOutput::default();
    egui::Window::new("Developer diagnostics")
        .open(open)
        .default_width(390.0)
        .resizable(true)
        .show(context, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("Processor: {}", model.processor));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    output.export_requested = ui.button("Save report…").clicked();
                });
            });
            ui.weak(format!("{} UI", model.ui_renderer));
            if let Some(device) = &model.gpu_device {
                ui.weak(device);
            }
            for (label, message) in model.messages.labelled() {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 164, 74),
                    format!("{label}: {message}"),
                );
            }

            ui.add_space(6.0);
            ui.strong("Preview queue");
            grid(ui, "preview_queue_diagnostics", model.queue_rows());

            if let Some(catalog) = model.catalog {
                ui.add_space(6.0);
                ui.strong("Catalog");
                grid(ui, "catalog_diagnostics", catalog.rows());
            }

            let Some(preview) = &model.preview else {
                ui.add_space(6.0);
                ui.weak("No developed preview diagnostics yet.");
                return;
            };
            ui.add_space(8.0);
            ui.strong(format!("Last {} preview", preview.backend));
            for line in preview.summary() {
                ui.weak(line);
            }
            grid(ui, "preview_cache_diagnostics", preview.cache.rows());

            ui.add_space(8.0);
            ui.strong("CPU stage wall times");
            grid(ui, "preview_stage_diagnostics", preview.timings.rows());
            for line in preview.highlight.lines() {
                ui.label(line);
            }
            ui.label(preview.memory());

            if let Some(gpu) = preview.gpu {
                ui.add_space(8.0);
                ui.strong("GPU preview");
                grid(ui, "gpu_preview_diagnostics", gpu.rows());
                ui.weak(GpuDiagnostics::NOTE);
            }
        });
    output
}

fn grid(ui: &mut egui::Ui, id: &str, rows: Vec<(&'static str, String)>) {
    egui::Grid::new(id).num_columns(2).show(ui, |ui| {
        for (label, value) in rows {
            ui.label(label);
            ui.monospace(value);
            ui.end_row();
        }
    });
}
