//! Texture, clarity and dehaze sliders.

use eframe::egui;
use rohditor_edit::PRESENCE_RANGE;

use super::adjustment_panel::{
    AdjustmentPanelOutput, AdjustmentTarget, DocumentPanelModel, record_slider,
};
use super::widgets::{AdjustmentSpec, ValueScale};

/// Shown on the Presence section's header.
pub(super) const PRESENCE_HINT: &str = "Texture and clarity add (or, below zero, remove) local contrast at a fine and a broad \
     scale; dehaze cuts through atmospheric haze, or adds it below zero. They work on the \
     whole developed image after the other adjustments. Run on the CPU, so the preview and \
     export use the CPU path while one is set.";

pub(super) fn presence(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    let settings = &mut document.presence;
    for (label, target, value, hint) in [
        (
            "Texture",
            AdjustmentTarget::PresenceTexture,
            &mut settings.texture,
            "Fine-scale contrast: skin, bark, fabric. Below zero it smooths without softening edges.",
        ),
        (
            "Clarity",
            AdjustmentTarget::PresenceClarity,
            &mut settings.clarity,
            "Broad local contrast, strongest in the mid-tones. Below zero it gives a soft glow.",
        ),
        (
            "Dehaze",
            AdjustmentTarget::PresenceDehaze,
            &mut settings.dehaze,
            "Removes haze and fog from distant scenes. Below zero it adds a hazy veil.",
        ),
    ] {
        record_slider(
            ui,
            &mut output.interactions,
            target,
            value,
            AdjustmentSpec {
                label,
                minimum: PRESENCE_RANGE.minimum,
                maximum: PRESENCE_RANGE.maximum,
                neutral: PRESENCE_RANGE.neutral,
                decimals: 0,
                step: 0.01,
                suffix: "%",
                scale: ValueScale::OffsetPercent,
            },
        )
        .on_hover_text(hint);
    }
}
