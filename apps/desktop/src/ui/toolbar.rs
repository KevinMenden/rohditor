use eframe::egui;

use super::icons::Icon;
use super::theme::{self, colors, metrics};
use super::widgets;

#[derive(Debug, Clone)]
pub(crate) struct ToolbarModel {
    pub document_name: Option<String>,
    pub can_undo: bool,
    pub can_redo: bool,
    pub fit_selected: bool,
    pub actual_size_selected: bool,
    pub zoom_label: String,
    pub diagnostics_open: bool,
    pub export_ready: bool,
}

#[derive(Debug, Default)]
pub(crate) struct ToolbarOutput {
    pub open: bool,
    pub close: bool,
    pub undo: bool,
    pub redo: bool,
    pub reset: bool,
    pub fit: bool,
    pub actual_size: bool,
    pub toggle_diagnostics: bool,
    pub export: bool,
}

impl ToolbarOutput {
    fn merge(&mut self, other: Self) {
        self.open |= other.open;
        self.close |= other.close;
        self.undo |= other.undo;
        self.redo |= other.redo;
        self.reset |= other.reset;
        self.fit |= other.fit;
        self.actual_size |= other.actual_size;
        self.toggle_diagnostics |= other.toggle_diagnostics;
        self.export |= other.export;
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FilePanelModel {
    pub file_name: Option<String>,
    pub camera: Option<String>,
    pub dimensions: Option<(usize, usize)>,
    pub source_state: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct FilePanelOutput {
    pub open: bool,
    pub close: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct StatusBarModel {
    pub processor: String,
    pub ui_renderer: String,
    pub activity: Option<String>,
    pub busy: bool,
    pub preview_dimensions: Option<(usize, usize)>,
    pub preview_milliseconds: Option<f64>,
    pub startup_error: Option<String>,
}

pub(crate) fn show_top(context: &egui::Context, model: &ToolbarModel) -> ToolbarOutput {
    let mut output = ToolbarOutput::default();
    let narrow = context.content_rect().width() < metrics::NARROW_TOOLBAR_BREAKPOINT;
    egui::TopBottomPanel::top("top_bar")
        .exact_height(metrics::TOOLBAR_HEIGHT)
        .frame(theme::toolbar_frame())
        .show(context, |ui| {
            let (menu_output, controls_output) = egui::Sides::new().shrink_left().truncate().show(
                ui,
                |ui| {
                    let mut output = ToolbarOutput::default();
                    show_app_menu(ui, model, &mut output);
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new("ROHDITOR")
                            .strong()
                            .size(14.0)
                            .color(colors::TEXT),
                    );
                    if let Some(name) = &model.document_name {
                        ui.add(egui::Separator::default().vertical().spacing(10.0));
                        ui.add(egui::Label::new(name).truncate());
                    }
                    output
                },
                |ui| {
                    let mut output = ToolbarOutput {
                        export: widgets::primary_button(ui, "Export", model.export_ready)
                            .on_hover_text("Export the developed full-resolution image")
                            .clicked(),
                        toggle_diagnostics: widgets::icon_button(
                            ui,
                            Icon::Diagnostics,
                            "Developer diagnostics",
                            model.diagnostics_open,
                            true,
                        )
                        .clicked(),
                        ..ToolbarOutput::default()
                    };

                    if model.document_name.is_some() {
                        let before_after = widgets::icon_button(
                            ui,
                            Icon::BeforeAfter,
                            "Before/after comparison is reserved for the comparison preview path",
                            false,
                            false,
                        );
                        let _ = before_after;
                        output.actual_size = widgets::toolbar_button(
                            ui,
                            if narrow { "1:1" } else { "Source 1:1" },
                            model.actual_size_selected,
                            true,
                        )
                        .on_hover_text("Develop and show one source pixel per screen point")
                        .clicked();
                        output.fit = widgets::toolbar_button(ui, "Fit", model.fit_selected, true)
                            .on_hover_text("Fit the whole photo in the viewport")
                            .clicked();
                        if !narrow && !model.fit_selected {
                            ui.label(
                                egui::RichText::new(&model.zoom_label)
                                    .monospace()
                                    .color(colors::TEXT_MUTED),
                            );
                        }
                        ui.add(egui::Separator::default().vertical().spacing(8.0));
                        output.redo = widgets::icon_button(
                            ui,
                            Icon::Redo,
                            "Redo the last edit",
                            false,
                            model.can_redo,
                        )
                        .clicked();
                        output.undo = widgets::icon_button(
                            ui,
                            Icon::Undo,
                            "Undo the last edit",
                            false,
                            model.can_undo,
                        )
                        .clicked();
                    }
                    output
                },
            );
            output.merge(menu_output);
            output.merge(controls_output);
        });
    output
}

fn show_app_menu(ui: &mut egui::Ui, model: &ToolbarModel, output: &mut ToolbarOutput) {
    let menu_button = widgets::icon_button(ui, Icon::Menu, "Application menu", false, true);
    let _ = egui::Popup::menu(&menu_button).show(|ui| {
        ui.set_min_width(190.0);
        if ui.button("Open RAW…").clicked() {
            output.open = true;
            ui.close();
        }
        if ui
            .add_enabled(
                model.document_name.is_some(),
                egui::Button::new("Close photo"),
            )
            .clicked()
        {
            output.close = true;
            ui.close();
        }
        ui.separator();
        if ui
            .add_enabled(model.can_undo, egui::Button::new("Undo"))
            .clicked()
        {
            output.undo = true;
            ui.close();
        }
        if ui
            .add_enabled(model.can_redo, egui::Button::new("Redo"))
            .clicked()
        {
            output.redo = true;
            ui.close();
        }
        if ui
            .add_enabled(
                model.document_name.is_some(),
                egui::Button::new("Reset adjustments"),
            )
            .clicked()
        {
            output.reset = true;
            ui.close();
        }
        ui.separator();
        if ui
            .selectable_label(model.diagnostics_open, "Developer diagnostics")
            .clicked()
        {
            output.toggle_diagnostics = true;
            ui.close();
        }
    });
}

pub(crate) fn show_file_panel(context: &egui::Context, model: &FilePanelModel) -> FilePanelOutput {
    let mut output = FilePanelOutput::default();
    if !file_panel_visible(context.content_rect().width()) {
        return output;
    }
    egui::SidePanel::left("file_navigation")
        .exact_width(metrics::FILE_PANEL_WIDTH)
        .resizable(false)
        .frame(theme::side_panel_frame())
        .show(context, |ui| {
            widgets::section_header(ui, "Files");
            output.open = widgets::toolbar_button(ui, "Open RAW…", false, true).clicked();
            ui.add_space(6.0);
            match &model.file_name {
                Some(file_name) => {
                    theme::card_frame().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let _ = widgets::icon_button(
                                ui,
                                Icon::File,
                                "Current RAW file",
                                true,
                                false,
                            );
                            ui.vertical(|ui| {
                                ui.add(
                                    egui::Label::new(egui::RichText::new(file_name).strong())
                                        .truncate(),
                                );
                                if let Some(state) = &model.source_state {
                                    ui.label(
                                        egui::RichText::new(state)
                                            .small()
                                            .color(colors::ACCENT_HOVER),
                                    );
                                }
                            });
                        });
                        if let Some(camera) = &model.camera {
                            ui.label(
                                egui::RichText::new(camera)
                                    .small()
                                    .color(colors::TEXT_MUTED),
                            );
                        }
                        if let Some((width, height)) = model.dimensions {
                            ui.label(
                                egui::RichText::new(format!("{width} × {height}"))
                                    .monospace()
                                    .small()
                                    .color(colors::TEXT_MUTED),
                            );
                        }
                    });
                    ui.add_space(4.0);
                    output.close = ui
                        .add(egui::Button::new("Close photo").frame(false))
                        .clicked();
                }
                None => {
                    ui.label(
                        egui::RichText::new("Your current photo appears here.")
                            .color(colors::TEXT_MUTED),
                    );
                }
            }
            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                ui.label(
                    egui::RichText::new("Single-document workspace")
                        .small()
                        .color(colors::TEXT_DISABLED),
                );
            });
        });
    output
}

pub(crate) fn show_status(context: &egui::Context, model: &StatusBarModel) {
    egui::TopBottomPanel::bottom("status_bar")
        .exact_height(metrics::STATUS_HEIGHT)
        .frame(theme::status_frame())
        .show(context, |ui| {
            egui::Sides::new()
                .height(17.0)
                .shrink_left()
                .truncate()
                .show(
                    ui,
                    |ui| {
                        let dot_color = if model.startup_error.is_some() {
                            colors::ERROR
                        } else if model.busy {
                            colors::ACCENT
                        } else {
                            colors::SUCCESS
                        };
                        let (dot_rect, _) =
                            ui.allocate_exact_size(egui::vec2(9.0, 17.0), egui::Sense::hover());
                        ui.painter()
                            .circle_filled(dot_rect.center(), 3.0, dot_color);
                        ui.label(
                            egui::RichText::new(&model.processor)
                                .small()
                                .color(colors::TEXT_MUTED),
                        );
                        if let Some(activity) = &model.activity {
                            ui.separator();
                            if model.busy {
                                ui.spinner();
                            }
                            ui.add(egui::Label::new(activity).truncate());
                        }
                        if let Some(error) = &model.startup_error {
                            ui.separator();
                            ui.colored_label(colors::ERROR, error);
                        }
                    },
                    |ui| {
                        if let Some((width, height)) = model.preview_dimensions {
                            ui.label(
                                egui::RichText::new(format!("{width} × {height}"))
                                    .monospace()
                                    .small()
                                    .color(colors::TEXT_MUTED),
                            );
                        }
                        if let Some(milliseconds) = model.preview_milliseconds {
                            ui.label(
                                egui::RichText::new(format!("{milliseconds:.1} ms preview"))
                                    .monospace()
                                    .small()
                                    .color(colors::TEXT_MUTED),
                            );
                        }
                        ui.label(
                            egui::RichText::new(&model.ui_renderer)
                                .monospace()
                                .small()
                                .color(colors::TEXT_DISABLED),
                        );
                    },
                );
        });
}

const fn file_panel_visible(window_width: f32) -> bool {
    window_width >= metrics::FILE_PANEL_BREAKPOINT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_panel_yields_viewport_space_on_narrow_windows() {
        assert!(!file_panel_visible(900.0));
        assert!(!file_panel_visible(metrics::FILE_PANEL_BREAKPOINT - 1.0));
        assert!(file_panel_visible(metrics::FILE_PANEL_BREAKPOINT));
        assert!(file_panel_visible(1_440.0));
    }
}
