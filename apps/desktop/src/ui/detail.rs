use eframe::egui;
use rohditor_edit::{
    CAPTURE_AMOUNT_RANGE, CAPTURE_NOISE_RANGE, CAPTURE_RADIUS_RANGE, CHROMA_NOISE_REDUCTION_RANGE,
    DEFAULT_GREEN_HUE, DEFAULT_PURPLE_HUE, DEFRINGE_AMOUNT_RANGE, DEFRINGE_HUE_RANGE_DEGREES,
    FALSE_COLOR_MAX_STEPS, LUMINANCE_DETAIL_RANGE, LUMINANCE_NOISE_REDUCTION_RANGE, ParameterRange,
};

use super::adjustment_panel::{
    AdjustmentPanelOutput, AdjustmentTarget, DocumentPanelModel, NoiseSuggestion, record_slider,
};
use super::widgets::{AdjustmentSpec, ValueScale};

/// The Detail section: source-resolution steps in pipeline order.
pub(super) fn show(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    false_color(ui, document, output);
    ui.separator();
    noise_reduction(ui, document, output);
    ui.separator();
    capture_sharpening(ui, document, output);
    ui.separator();
    defringe(ui, document, output);
}

/// Purple and green fringe removal, each with an amount and a hue range
/// (two sliders: the range ends).
fn defringe(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    ui.label("Defringe").on_hover_text(
        "Removes purple and green colour fringes along high-contrast edges (branches against \
         sky, bright highlights). Only pixels next to strong edges whose hue falls in the range \
         are touched; they lose colour but keep their brightness. Inspect at Source 1:1. Runs \
         on the CPU, so the preview and export use the CPU path while it is on.",
    );
    let settings = &mut document.defringe;
    let purple_active = settings.purple_active();
    let green_active = settings.green_active();
    for (name, amount_target, start_target, end_target, active, amount, hue, default_hue) in [
        (
            "Purple",
            AdjustmentTarget::DefringePurpleAmount,
            AdjustmentTarget::DefringePurpleHueStart,
            AdjustmentTarget::DefringePurpleHueEnd,
            purple_active,
            &mut settings.purple_amount,
            &mut settings.purple_hue,
            DEFAULT_PURPLE_HUE,
        ),
        (
            "Green",
            AdjustmentTarget::DefringeGreenAmount,
            AdjustmentTarget::DefringeGreenHueStart,
            AdjustmentTarget::DefringeGreenHueEnd,
            green_active,
            &mut settings.green_amount,
            &mut settings.green_hue,
            DEFAULT_GREEN_HUE,
        ),
    ] {
        record_slider(
            ui,
            &mut output.interactions,
            amount_target,
            amount,
            slider(name, DEFRINGE_AMOUNT_RANGE),
        );
        // The hue range has nothing to act on while the colour is off.
        let [start, end] = hue;
        ui.add_enabled_ui(active, |ui| {
            for (target, label, value, neutral) in [
                (start_target, "Hue from", start, default_hue[0]),
                (end_target, "Hue to", end, default_hue[1]),
            ] {
                record_slider(
                    ui,
                    &mut output.interactions,
                    target,
                    value,
                    AdjustmentSpec {
                        label,
                        minimum: DEFRINGE_HUE_RANGE_DEGREES.minimum,
                        maximum: DEFRINGE_HUE_RANGE_DEGREES.maximum,
                        neutral,
                        decimals: 0,
                        step: 1.0,
                        suffix: "°",
                        scale: ValueScale::Raw,
                    },
                );
            }
        });
    }
}

fn false_color(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    // A whole-step slider: the recipe stores a count, the widget a float.
    let mut steps = f32::from(document.false_color.steps);
    record_slider(
        ui,
        &mut output.interactions,
        AdjustmentTarget::FalseColorSteps,
        &mut steps,
        AdjustmentSpec {
            label: "Moiré / false colour",
            minimum: 0.0,
            maximum: f32::from(FALSE_COLOR_MAX_STEPS),
            neutral: 0.0,
            decimals: 0,
            step: 1.0,
            suffix: "",
            scale: ValueScale::Raw,
        },
    )
    .on_hover_text(
        "Removes rainbow-coloured bands that fine patterns (fabric, screens, distant buildings) \
         cause in demosaicing. Each step smooths colour further and can wash out tiny colour \
         details. Does not touch luminance. Inspect at Source 1:1. Runs on the CPU, so the \
         preview and export use the CPU path while it is on.",
    );
    document.false_color.steps = steps.round() as u8;
}

fn noise_reduction(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    ui.horizontal(|ui| {
        ui.label("Noise reduction").on_hover_text(
            "Removes colour noise, then luminance (grain) noise, at source resolution and \
             before sharpening. Inspect at Source 1:1. Runs on the CPU, so the preview and \
             export use the CPU path while it is on.",
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            auto_button(ui, document.noise_suggestion, output);
        });
    });
    let settings = &mut document.noise_reduction;
    record_slider(
        ui,
        &mut output.interactions,
        AdjustmentTarget::ChromaNoiseReduction,
        &mut settings.chroma,
        slider("Colour", CHROMA_NOISE_REDUCTION_RANGE),
    );
    record_slider(
        ui,
        &mut output.interactions,
        AdjustmentTarget::LuminanceNoiseReduction,
        &mut settings.luminance,
        slider("Luminance", LUMINANCE_NOISE_REDUCTION_RANGE),
    );
    // Texture preservation has nothing to act on while luminance is off.
    ui.add_enabled_ui(settings.luminance > 0.0, |ui| {
        record_slider(
            ui,
            &mut output.interactions,
            AdjustmentTarget::LuminanceDetail,
            &mut settings.luminance_detail,
            slider("Detail", LUMINANCE_DETAIL_RANGE),
        );
    })
    .response
    .on_hover_text("Higher keeps more fine texture (and some grain).");
}

/// Sets the colour strength from the photo's estimated noise; the first click
/// on a photo that has not been profiled yet runs the estimate.
fn auto_button(ui: &mut egui::Ui, suggestion: NoiseSuggestion, output: &mut AdjustmentPanelOutput) {
    let (enabled, hint) = match suggestion {
        NoiseSuggestion::Unknown => (
            true,
            "Estimates this photo's noise and sets the Colour strength to match.".to_owned(),
        ),
        NoiseSuggestion::Estimating => (false, "Estimating this photo's noise…".to_owned()),
        NoiseSuggestion::Unavailable => (
            false,
            "The noise of this photo could not be estimated.".to_owned(),
        ),
        NoiseSuggestion::Suggested { chroma } if chroma > 0.0 => (
            true,
            format!("Sets Colour to {chroma:.2}, from this photo's estimated noise."),
        ),
        NoiseSuggestion::Suggested { .. } => (
            true,
            "This photo is clean enough that no colour noise reduction is suggested.".to_owned(),
        ),
    };
    if ui
        .add_enabled(enabled, egui::Button::new("Auto").small())
        .on_hover_text(hint)
        .clicked()
    {
        output.auto_noise_reduction = true;
    }
}

/// A 0..1 slider with two decimals.
fn slider(label: &'static str, range: ParameterRange) -> AdjustmentSpec<'static> {
    AdjustmentSpec {
        label,
        minimum: range.minimum,
        maximum: range.maximum,
        neutral: range.neutral,
        decimals: 2,
        step: 0.01,
        suffix: "",
        scale: ValueScale::Raw,
    }
}

fn capture_sharpening(
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
