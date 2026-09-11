use eframe::egui;
use rohditor_edit::{CAPTURE_AMOUNT_RANGE, CAPTURE_NOISE_RANGE, CAPTURE_RADIUS_RANGE};

use super::adjustment_panel::{
    AdjustmentPanelOutput, AdjustmentTarget, DocumentPanelModel, record_slider,
};
use super::widgets::{AdjustmentSpec, ValueScale};

pub(super) fn show(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    let settings = &mut document.capture_sharpening;
    if ui.checkbox(&mut settings.enabled, "Capture sharpening").on_hover_text(
        "Recover fine detail before resizing. Inspect at Source 1:1; sharpening can amplify noise."
    ).changed() {
        output.capture_enabled = Some(settings.enabled);
    }
    ui.add_enabled_ui(settings.enabled, |ui| {
        for (target, label, value, range, suffix) in [
            (
                AdjustmentTarget::CaptureAmount,
                "Amount",
                &mut settings.amount,
                CAPTURE_AMOUNT_RANGE,
                "",
            ),
            (
                AdjustmentTarget::CaptureRadius,
                "Radius",
                &mut settings.radius,
                CAPTURE_RADIUS_RANGE,
                " px",
            ),
            (
                AdjustmentTarget::CaptureNoise,
                "Noise protection",
                &mut settings.noise_protection,
                CAPTURE_NOISE_RANGE,
                "",
            ),
        ] {
            record_slider(
                ui,
                &mut output.interactions,
                target,
                value,
                AdjustmentSpec {
                    label,
                    minimum: range.minimum,
                    maximum: range.maximum,
                    neutral: range.neutral,
                    decimals: 2,
                    step: 0.01,
                    suffix,
                    scale: ValueScale::Raw,
                },
            );
        }
    });
}
