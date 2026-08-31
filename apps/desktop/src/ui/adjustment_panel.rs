use eframe::egui;

use super::theme::{self, colors, metrics};
use super::widgets::{self, AdjustmentSpec, ValueScale};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdjustmentTarget {
    WhiteBalanceRed,
    WhiteBalanceGreen,
    WhiteBalanceBlue,
    Exposure,
    Contrast,
    Saturation,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AdjustmentRange {
    pub minimum: f32,
    pub maximum: f32,
    pub neutral: f32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AdjustmentRanges {
    pub white_balance: AdjustmentRange,
    pub exposure: AdjustmentRange,
    pub contrast: AdjustmentRange,
    pub saturation: AdjustmentRange,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AdjustmentValues {
    pub manual_white_balance: bool,
    pub white_balance_red: f32,
    pub white_balance_green: f32,
    pub white_balance_blue: f32,
    pub exposure: f32,
    pub contrast: f32,
    pub saturation: f32,
}

#[derive(Debug, Clone)]
pub(crate) struct DocumentPanelModel {
    pub file_name: String,
    pub camera: Option<String>,
    pub sensor_dimensions: Option<(usize, usize)>,
    pub revision: u64,
    pub has_adjustments: bool,
    pub values: AdjustmentValues,
    pub ranges: AdjustmentRanges,
    pub export_ready: bool,
    pub export_in_progress: bool,
    pub error: Option<String>,
    pub warning: Option<String>,
    pub notice: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AdjustmentInteraction {
    pub target: AdjustmentTarget,
    pub value: f32,
    pub changed: bool,
    pub drag_started: bool,
    pub dragged: bool,
    pub drag_stopped: bool,
    pub reset: bool,
}

#[derive(Debug, Default)]
pub(crate) struct AdjustmentPanelOutput {
    pub manual_white_balance: Option<bool>,
    pub interactions: Vec<AdjustmentInteraction>,
    pub reset_all: bool,
    pub export: bool,
    pub dismiss_error: bool,
    pub dismiss_warning: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ExportKind {
    #[default]
    Jpeg,
    Png,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PngDepth {
    #[default]
    Eight,
    Sixteen,
}

#[derive(Debug, Clone)]
pub(crate) struct ExportUiSettings {
    pub kind: ExportKind,
    pub jpeg_quality: u8,
    pub png_depth: PngDepth,
    pub dither: bool,
    pub safe_metadata: bool,
    pub overwrite: bool,
}

impl ExportUiSettings {
    pub(crate) const fn with_jpeg_quality(jpeg_quality: u8) -> Self {
        Self {
            kind: ExportKind::Jpeg,
            jpeg_quality,
            png_depth: PngDepth::Eight,
            dither: false,
            safe_metadata: true,
            overwrite: false,
        }
    }
}

pub(crate) fn show(
    context: &egui::Context,
    document: Option<DocumentPanelModel>,
    export_settings: &mut ExportUiSettings,
) -> AdjustmentPanelOutput {
    let mut output = AdjustmentPanelOutput::default();
    egui::SidePanel::right("adjustments")
        .default_width(metrics::ADJUSTMENT_PANEL_WIDTH)
        .min_width(280.0)
        .max_width(350.0)
        .resizable(true)
        .frame(theme::side_panel_frame())
        .show(context, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.heading("Develop");
                    ui.label(
                        egui::RichText::new("NON-DESTRUCTIVE RAW ADJUSTMENTS")
                            .size(10.0)
                            .color(colors::TEXT_MUTED),
                    );
                    ui.add_space(7.0);

                    let Some(mut document) = document else {
                        empty_panel(ui);
                        return;
                    };

                    document_summary(ui, &document);
                    show_messages(ui, &document, &mut output);
                    histogram_shell(ui);
                    show_light_controls(ui, &mut document, &mut output);
                    show_color_controls(ui, &mut document, &mut output);

                    widgets::section_header(ui, "Export");
                    show_export_settings(ui, export_settings);
                    let export_label = if document.export_in_progress {
                        "Exporting…"
                    } else {
                        "Export image…"
                    };
                    output.export = widgets::primary_button(
                        ui,
                        export_label,
                        document.export_ready && !document.export_in_progress,
                    )
                    .on_hover_text("Export the full-resolution CPU-developed image")
                    .clicked();
                    if !document.export_ready && !document.export_in_progress {
                        ui.label(
                            egui::RichText::new("Available after RAW decoding completes")
                                .small()
                                .color(colors::TEXT_MUTED),
                        );
                    }
                });
        });
    output
}

fn empty_panel(ui: &mut egui::Ui) {
    theme::card_frame().show(ui, |ui| {
        ui.label(egui::RichText::new("No photo open").strong());
        ui.label(
            egui::RichText::new("Open a Sony ARW file to reveal the develop controls.")
                .color(colors::TEXT_MUTED),
        );
    });
    widgets::section_header(ui, "Histogram");
    histogram_canvas(ui, "WAITING FOR A PHOTO");
}

fn document_summary(ui: &mut egui::Ui, document: &DocumentPanelModel) {
    ui.add(
        egui::Label::new(egui::RichText::new(&document.file_name).strong().size(14.0)).truncate(),
    );
    if let Some(camera) = &document.camera {
        ui.label(
            egui::RichText::new(camera)
                .small()
                .color(colors::TEXT_MUTED),
        );
    }
    if let Some((width, height)) = document.sensor_dimensions {
        ui.label(
            egui::RichText::new(format!("{width} × {height}  ·  REV {}", document.revision))
                .monospace()
                .small()
                .color(colors::TEXT_MUTED),
        );
    }
}

fn histogram_shell(ui: &mut egui::Ui) {
    widgets::section_header(ui, "Histogram");
    histogram_canvas(ui, "ANALYSIS SHELL");
    ui.horizontal(|ui| {
        let shadows = widgets::toolbar_button(ui, "Shadows", false, false)
            .on_disabled_hover_text("Shadow clipping analysis is planned after MVP");
        let highlights = widgets::toolbar_button(ui, "Highlights", false, false)
            .on_disabled_hover_text("Highlight clipping analysis is planned after MVP");
        let _ = (shadows, highlights);
    });
}

fn histogram_canvas(ui: &mut egui::Ui, caption: &str) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 74.0), egui::Sense::hover());
    ui.painter().rect(
        rect,
        metrics::RADIUS,
        colors::FIELD,
        egui::Stroke::new(1.0, colors::BORDER),
        egui::StrokeKind::Inside,
    );
    for fraction in [0.25_f32, 0.5, 0.75] {
        let x = egui::lerp(rect.left()..=rect.right(), fraction);
        ui.painter().vline(
            x,
            rect.top() + 1.0..=rect.bottom() - 1.0,
            egui::Stroke::new(1.0, colors::BORDER.gamma_multiply(0.45)),
        );
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        caption,
        egui::FontId::monospace(9.5),
        colors::TEXT_DISABLED,
    );
}

fn show_light_controls(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    widgets::section_header(ui, "Light");
    record_slider(
        ui,
        &mut output.interactions,
        AdjustmentTarget::Exposure,
        &mut document.values.exposure,
        AdjustmentSpec {
            label: "Exposure",
            minimum: document.ranges.exposure.minimum,
            maximum: document.ranges.exposure.maximum,
            neutral: document.ranges.exposure.neutral,
            decimals: 2,
            step: 0.01,
            suffix: " EV",
            scale: ValueScale::Raw,
        },
    );
    record_slider(
        ui,
        &mut output.interactions,
        AdjustmentTarget::Contrast,
        &mut document.values.contrast,
        AdjustmentSpec {
            label: "Contrast",
            minimum: document.ranges.contrast.minimum,
            maximum: document.ranges.contrast.maximum,
            neutral: document.ranges.contrast.neutral,
            decimals: 0,
            step: 0.01,
            suffix: "%",
            scale: ValueScale::OffsetPercent,
        },
    );
}

fn show_color_controls(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    widgets::section_header(ui, "Color");
    let mut manual = document.values.manual_white_balance;
    widgets::dropdown(
        ui,
        "white_balance_mode",
        "White balance",
        if manual {
            "Manual multipliers"
        } else {
            "As shot"
        },
        |ui| {
            ui.selectable_value(&mut manual, false, "As shot");
            ui.selectable_value(&mut manual, true, "Manual multipliers");
        },
    );
    if manual != document.values.manual_white_balance {
        output.manual_white_balance = Some(manual);
        document.values.manual_white_balance = manual;
    }
    ui.label(
        egui::RichText::new("Manual values are relative to the camera multipliers")
            .small()
            .color(colors::TEXT_MUTED),
    );
    ui.add_space(4.0);

    if document.values.manual_white_balance {
        for (label, target, value) in [
            (
                "Red multiplier",
                AdjustmentTarget::WhiteBalanceRed,
                &mut document.values.white_balance_red,
            ),
            (
                "Green multiplier",
                AdjustmentTarget::WhiteBalanceGreen,
                &mut document.values.white_balance_green,
            ),
            (
                "Blue multiplier",
                AdjustmentTarget::WhiteBalanceBlue,
                &mut document.values.white_balance_blue,
            ),
        ] {
            record_slider(
                ui,
                &mut output.interactions,
                target,
                value,
                AdjustmentSpec {
                    label,
                    minimum: document.ranges.white_balance.minimum,
                    maximum: document.ranges.white_balance.maximum,
                    neutral: document.ranges.white_balance.neutral,
                    decimals: 2,
                    step: 0.01,
                    suffix: "×",
                    scale: ValueScale::RawUnsigned,
                },
            );
        }
    }

    record_slider(
        ui,
        &mut output.interactions,
        AdjustmentTarget::Saturation,
        &mut document.values.saturation,
        AdjustmentSpec {
            label: "Saturation",
            minimum: document.ranges.saturation.minimum,
            maximum: document.ranges.saturation.maximum,
            neutral: document.ranges.saturation.neutral,
            decimals: 0,
            step: 0.01,
            suffix: "%",
            scale: ValueScale::OffsetPercent,
        },
    );

    output.reset_all = ui
        .add_enabled(
            document.has_adjustments,
            egui::Button::new("Reset all adjustments").frame(false),
        )
        .on_hover_text("Restore the neutral recipe")
        .clicked();
}

fn record_slider(
    ui: &mut egui::Ui,
    interactions: &mut Vec<AdjustmentInteraction>,
    target: AdjustmentTarget,
    value: &mut f32,
    spec: AdjustmentSpec<'_>,
) {
    let neutral = spec.neutral;
    let response = widgets::adjustment_slider(ui, value, spec);
    if response.reset_clicked {
        *value = neutral;
    }
    if response.response.changed()
        || response.response.drag_started()
        || response.response.drag_stopped()
        || response.reset_clicked
    {
        interactions.push(AdjustmentInteraction {
            target,
            value: *value,
            changed: response.response.changed(),
            drag_started: response.response.drag_started(),
            dragged: response.response.dragged(),
            drag_stopped: response.response.drag_stopped(),
            reset: response.reset_clicked,
        });
    }
}

fn show_export_settings(ui: &mut egui::Ui, settings: &mut ExportUiSettings) {
    let mut kind = settings.kind;
    widgets::dropdown(
        ui,
        "export_format",
        "Format",
        match kind {
            ExportKind::Jpeg => "JPEG",
            ExportKind::Png => "PNG",
        },
        |ui| {
            ui.selectable_value(&mut kind, ExportKind::Jpeg, "JPEG");
            ui.selectable_value(&mut kind, ExportKind::Png, "PNG");
        },
    );
    settings.kind = kind;

    match settings.kind {
        ExportKind::Jpeg => {
            ui.label(
                egui::RichText::new("JPEG quality")
                    .small()
                    .color(colors::TEXT_MUTED),
            );
            let available = ui.available_width();
            ui.scope(|ui| {
                ui.spacing_mut().slider_width = available;
                ui.add(
                    egui::Slider::new(&mut settings.jpeg_quality, 1..=100)
                        .show_value(true)
                        .trailing_fill(true),
                );
            });
        }
        ExportKind::Png => {
            let mut depth = settings.png_depth;
            widgets::dropdown(
                ui,
                "png_depth",
                "Bit depth",
                match depth {
                    PngDepth::Eight => "8-bit",
                    PngDepth::Sixteen => "16-bit",
                },
                |ui| {
                    ui.selectable_value(&mut depth, PngDepth::Eight, "8-bit");
                    ui.selectable_value(&mut depth, PngDepth::Sixteen, "16-bit");
                },
            );
            settings.png_depth = depth;
        }
    }
    ui.checkbox(&mut settings.dither, "Ordered output dithering");
    ui.checkbox(&mut settings.safe_metadata, "Include safe EXIF metadata");
    ui.checkbox(&mut settings.overwrite, "Allow replacing an existing file");
    ui.add_space(4.0);
}

fn show_messages(
    ui: &mut egui::Ui,
    document: &DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    if document.error.is_some() || document.warning.is_some() || document.notice.is_some() {
        widgets::section_header(ui, "Messages");
    }
    if let Some(message) = &document.error {
        output.dismiss_error =
            widgets::message_card(ui, message, colors::ERROR, Some("Dismiss error"));
    }
    if let Some(message) = &document.warning {
        output.dismiss_warning =
            widgets::message_card(ui, message, colors::WARNING, Some("Dismiss warning"));
    }
    if let Some(message) = &document.notice {
        let _ = widgets::message_card(ui, message, colors::SUCCESS, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_ui_defaults_are_stable_and_non_destructive() {
        let settings = ExportUiSettings::with_jpeg_quality(87);
        assert_eq!(settings.kind, ExportKind::Jpeg);
        assert_eq!(settings.jpeg_quality, 87);
        assert_eq!(settings.png_depth, PngDepth::Eight);
        assert!(settings.safe_metadata);
        assert!(!settings.overwrite);
    }
}
