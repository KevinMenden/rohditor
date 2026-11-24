use eframe::egui;
use std::sync::Arc;

use rohditor_core::{CorrectionComponents, LensProfileSummary};
use rohditor_edit::{LensCorrection, LensProfileSelection, OpticsAdjustments};
use rohditor_engine::open_document::LensProfiles;

use super::theme::colors;
use super::widgets;

/// A change to the optics of the recipe.
#[derive(Debug, Clone)]
pub(crate) enum OpticsAction {
    SelectProfile(LensProfileSelection),
    Set(LensCorrection, bool),
}

/// Presentation-only optics state supplied by the worker and current recipe.
/// The UI never owns a Lensfun database or calibration object.
#[derive(Debug, Clone)]
pub(crate) struct OpticsPanelModel {
    pub optics: OpticsAdjustments,
    pub camera: Option<String>,
    pub lens: Option<String>,
    /// Display labels, e.g. `35 mm`, `f/2.8`, `1.2 m`.
    pub focal_length: Option<String>,
    pub aperture: Option<String>,
    pub focus_distance: Option<String>,
    pub profiles: Arc<LensProfiles>,
    pub profile_filter: String,
    pub applied: Option<CorrectionComponents>,
    pub used_infinity_distance_fallback: bool,
    pub scale: Option<f32>,
}

pub(crate) fn show(
    ui: &mut egui::Ui,
    model: &mut OpticsPanelModel,
    action: &mut Option<OpticsAction>,
) -> Option<String> {
    let mut filter_changed = false;
    if model.profiles.choices.len() > 1 {
        ui.horizontal(|ui| {
            ui.label("Manual profiles");
            filter_changed = ui
                .add(
                    egui::TextEdit::singleline(&mut model.profile_filter)
                        .hint_text("Filter lens or mount"),
                )
                .changed();
        });
    }

    let mut selected = model.optics.profile.clone();
    let selected_label = profile_label(model);
    widgets::dropdown(ui, "lens_profile", "Profile", &selected_label, |ui| {
        ui.selectable_value(&mut selected, LensProfileSelection::Off, "Off");
        // Without a detected lens it could not be rendered.
        if model.profiles.detected().is_some() {
            ui.selectable_value(&mut selected, LensProfileSelection::Automatic, "Automatic");
        }
        for summary in candidate_profiles(model) {
            ui.selectable_value(
                &mut selected,
                LensProfileSelection::Lensfun {
                    profile_id: summary.id.clone(),
                },
                format!("Manual · {}", summary.lens),
            );
        }
    });
    if selected != model.optics.profile {
        *action = Some(OpticsAction::SelectProfile(selected));
    }

    show_match_status(ui, model);
    show_capture_metadata(ui, model);

    let enabled = !matches!(model.optics.profile, LensProfileSelection::Off);
    ui.add_enabled_ui(enabled, |ui| {
        for correction in LensCorrection::ALL {
            correction_checkbox(ui, model, correction, action);
        }
    });
    if !enabled {
        ui.label(
            egui::RichText::new("Correction is disabled; component choices are retained")
                .small()
                .color(colors::TEXT_MUTED),
        );
    }

    if let Some(applied) = model.applied {
        ui.label(
            egui::RichText::new(format!(
                "Applied: distortion {}, vignetting {}, TCA {}",
                yes_no(applied.distortion),
                yes_no(applied.vignetting),
                yes_no(applied.chromatic_aberration),
            ))
            .small()
            .color(colors::TEXT_MUTED),
        );
    }
    if model.used_infinity_distance_fallback {
        ui.label(
            egui::RichText::new("Vignetting distance: using 1000 m infinity fallback")
                .small()
                .color(colors::WARNING),
        );
    }
    if let Some(scale) = model.scale {
        ui.label(
            egui::RichText::new(format!("Correction scale: {scale:.4}"))
                .small()
                .color(colors::TEXT_MUTED),
        );
    }
    if let Some(database) = &model.profiles.database {
        ui.label(
            egui::RichText::new(format!(
                "Profile database: {} {}",
                database.source, database.snapshot
            ))
            .small()
            .color(colors::TEXT_MUTED),
        );
    }
    filter_changed.then(|| model.profile_filter.clone())
}

fn correction_checkbox(
    ui: &mut egui::Ui,
    model: &OpticsPanelModel,
    correction: LensCorrection,
    action: &mut Option<OpticsAction>,
) {
    let mut on = correction.is_on(&model.optics);
    let available = model.profiles.offers(&model.optics.profile, correction) != Some(false);
    let checkbox = egui::Checkbox::new(&mut on, correction.label());
    if ui.add_enabled(available, checkbox).changed() {
        *action = Some(OpticsAction::Set(correction, on));
    }
    if !available {
        ui.label(
            egui::RichText::new("unavailable in this profile")
                .small()
                .color(colors::TEXT_DISABLED),
        );
    }
}

fn show_match_status(ui: &mut egui::Ui, model: &OpticsPanelModel) {
    if let Some(detected) = model.profiles.detected() {
        ui.label(egui::RichText::new(format!("Detected: {}", detected.lens)).strong());
    } else if let Some(problem) = model.profiles.problem() {
        ui.colored_label(colors::WARNING, problem);
    } else {
        ui.label(
            egui::RichText::new("Matching lens profile…")
                .small()
                .color(colors::TEXT_MUTED),
        );
    }
}

fn show_capture_metadata(ui: &mut egui::Ui, model: &OpticsPanelModel) {
    if let Some(camera) = &model.camera {
        ui.label(
            egui::RichText::new(format!("Camera: {camera}"))
                .small()
                .color(colors::TEXT_MUTED),
        );
    }
    let lens = model.lens.as_deref().unwrap_or("unknown lens");
    let focal = model
        .focal_length
        .as_deref()
        .unwrap_or("unknown focal length");
    let aperture = model.aperture.as_deref().unwrap_or("aperture unknown");
    let focus = model.focus_distance.as_ref().map_or_else(
        || "focus distance unknown".to_owned(),
        |value| format!("focus {value}"),
    );
    ui.label(
        egui::RichText::new(format!("Captured: {lens} · {focal} · {aperture} · {focus}"))
            .small()
            .color(colors::TEXT_MUTED),
    );
}

fn profile_label(model: &OpticsPanelModel) -> String {
    let profile = model.profiles.profile(&model.optics.profile);
    match (&model.optics.profile, profile) {
        (LensProfileSelection::Off, _) => "Off".to_owned(),
        (LensProfileSelection::Automatic, None) => "Automatic".to_owned(),
        (LensProfileSelection::Automatic, Some(profile)) => format!("Automatic · {}", profile.lens),
        (LensProfileSelection::Lensfun { .. }, Some(profile)) => profile.lens.clone(),
        (LensProfileSelection::Lensfun { profile_id }, None) => profile_id.clone(),
    }
}

fn candidate_profiles(model: &OpticsPanelModel) -> Vec<&LensProfileSummary> {
    let filter = model.profile_filter.trim().to_ascii_lowercase();
    let matches = |profile: &&LensProfileSummary| {
        [&profile.lens, &profile.mount]
            .into_iter()
            .any(|value| value.to_ascii_lowercase().contains(&filter))
    };
    model.profiles.choices.iter().filter(matches).collect()
}

const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
