use eframe::egui;
use rohditor_core::{OutputMedium, OutputSharpening, OutputSharpeningStrength};

use super::{AdjustmentPanelOutput, DocumentPanelModel};
use crate::ui::theme::colors;
use crate::ui::widgets;

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

/// Which size control the export dialog shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum SizeMode {
    #[default]
    Original,
    LongEdge,
    Fit,
}

#[derive(Debug, Clone)]
pub(crate) struct ExportUiSettings {
    pub kind: ExportKind,
    pub jpeg_quality: u8,
    pub png_depth: PngDepth,
    pub size_mode: SizeMode,
    pub long_edge: u32,
    pub fit_width: u32,
    pub fit_height: u32,
    /// Export-only; `None` is off.
    pub sharpening: Option<OutputSharpening>,
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
            size_mode: SizeMode::Original,
            long_edge: 2048,
            fit_width: 2048,
            fit_height: 2048,
            sharpening: None,
            dither: false,
            safe_metadata: true,
            overwrite: false,
        }
    }
}

pub(super) fn show_export_controls(
    ui: &mut egui::Ui,
    document: &DocumentPanelModel,
    export_settings: &mut ExportUiSettings,
    output: &mut AdjustmentPanelOutput,
) {
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
    show_size_controls(ui, settings);
    show_sharpening_controls(ui, &mut settings.sharpening);
    ui.checkbox(&mut settings.dither, "Ordered output dithering");
    ui.checkbox(&mut settings.safe_metadata, "Include safe EXIF metadata");
    ui.checkbox(&mut settings.overwrite, "Allow replacing an existing file");
    ui.add_space(4.0);
}

fn pixel_field(value: &mut u32) -> egui::DragValue<'_> {
    egui::DragValue::new(value).range(1..=65_535).suffix(" px")
}

/// Output size: original, long edge, or bounding box. Never upscales.
fn show_size_controls(ui: &mut egui::Ui, settings: &mut ExportUiSettings) {
    let mut mode = settings.size_mode;
    widgets::dropdown(
        ui,
        "export_size",
        "Size",
        match mode {
            SizeMode::Original => "Original",
            SizeMode::LongEdge => "Long edge",
            SizeMode::Fit => "Fit in box",
        },
        |ui| {
            ui.selectable_value(&mut mode, SizeMode::Original, "Original");
            ui.selectable_value(&mut mode, SizeMode::LongEdge, "Long edge");
            ui.selectable_value(&mut mode, SizeMode::Fit, "Fit in box");
        },
    );
    settings.size_mode = mode;
    match mode {
        SizeMode::Original => {}
        SizeMode::LongEdge => {
            ui.add(pixel_field(&mut settings.long_edge));
        }
        SizeMode::Fit => {
            ui.horizontal(|ui| {
                ui.add(pixel_field(&mut settings.fit_width));
                ui.label("×");
                ui.add(pixel_field(&mut settings.fit_height));
            });
        }
    }
}

/// Output sharpening medium and strength. It is applied on export only, so
/// the editor preview does not show it.
fn show_sharpening_controls(ui: &mut egui::Ui, sharpening: &mut Option<OutputSharpening>) {
    let medium_label = |medium: Option<OutputMedium>| match medium {
        None => "Off",
        Some(OutputMedium::Screen) => "Screen",
        Some(OutputMedium::Matte) => "Matte",
        Some(OutputMedium::Glossy) => "Glossy",
    };
    let mut medium = sharpening.map(|value| value.medium);
    widgets::dropdown(
        ui,
        "export_sharpening",
        "Output sharpening",
        medium_label(medium),
        |ui| {
            for choice in [
                None,
                Some(OutputMedium::Screen),
                Some(OutputMedium::Matte),
                Some(OutputMedium::Glossy),
            ] {
                ui.selectable_value(&mut medium, choice, medium_label(choice));
            }
        },
    );
    let Some(medium) = medium else {
        *sharpening = None;
        return;
    };
    let mut strength =
        sharpening.map_or(OutputSharpeningStrength::Standard, |value| value.strength);
    widgets::dropdown(
        ui,
        "export_sharpening_strength",
        "Strength",
        match strength {
            OutputSharpeningStrength::Low => "Low",
            OutputSharpeningStrength::Standard => "Standard",
            OutputSharpeningStrength::High => "High",
        },
        |ui| {
            ui.selectable_value(&mut strength, OutputSharpeningStrength::Low, "Low");
            ui.selectable_value(
                &mut strength,
                OutputSharpeningStrength::Standard,
                "Standard",
            );
            ui.selectable_value(&mut strength, OutputSharpeningStrength::High, "High");
        },
    );
    *sharpening = Some(OutputSharpening { medium, strength });
    ui.weak("Applied on export only; not shown in the preview.");
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
