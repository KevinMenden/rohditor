use eframe::egui;
use rohditor_core::{
    CorrectionComponents, DatabaseProvenance, LensProfileSummary, MetadataField, ProfileMatch,
};
use rohditor_edit::LensProfileSelection;

use super::theme::colors;
use super::widgets;

/// Presentation-only optics state supplied by the worker and current recipe.
/// The UI never owns a Lensfun database or calibration object.
#[derive(Debug, Clone)]
pub(crate) struct OpticsPanelModel {
    pub profile: LensProfileSelection,
    pub distortion: bool,
    pub vignetting: bool,
    pub chromatic_aberration: bool,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub focal_length_mm: Option<f32>,
    pub aperture_f_number: Option<f32>,
    pub focus_distance_m: Option<f32>,
    pub match_result: Option<ProfileMatch>,
    pub candidates: Vec<LensProfileSummary>,
    pub profile_filter: String,
    pub database: Option<DatabaseProvenance>,
    pub error: Option<String>,
    pub applied: Option<CorrectionComponents>,
    pub used_infinity_distance_fallback: bool,
    pub scale: Option<f32>,
}

#[derive(Debug, Clone)]
pub(crate) enum OpticsAction {
    SelectProfile(LensProfileSelection),
    SetDistortion(bool),
    SetVignetting(bool),
    SetChromaticAberration(bool),
    Reset,
}

pub(crate) fn show(
    ui: &mut egui::Ui,
    model: &mut OpticsPanelModel,
    action: &mut Option<OpticsAction>,
) -> Option<String> {
    let mut filter_changed = false;
    if model.candidates.len() > 1 {
        ui.horizontal(|ui| {
            ui.label("Manual profiles");
            filter_changed = ui
                .add(
                    egui::TextEdit::singleline(&mut model.profile_filter)
                        .hint_text("Filter camera, lens, or mount"),
                )
                .changed();
        });
    }

    let mut selected = model.profile.clone();
    let selected_label = profile_label(model);
    widgets::dropdown(ui, "lens_profile", "Profile", &selected_label, |ui| {
        ui.selectable_value(&mut selected, LensProfileSelection::Off, "Off");
        ui.selectable_value(&mut selected, LensProfileSelection::Automatic, "Automatic");
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
    if selected != model.profile {
        model.profile = selected.clone();
        *action = Some(OpticsAction::SelectProfile(selected));
    }

    show_match_status(ui, model);
    show_capture_metadata(ui, model);

    let enabled = !matches!(model.profile, LensProfileSelection::Off);
    let availability = available_components(model);
    ui.add_enabled_ui(enabled, |ui| {
        component_checkbox(
            ui,
            "Distortion",
            &mut model.distortion,
            availability.is_none_or(|value| value.distortion),
            OpticsAction::SetDistortion,
            action,
        );
        component_checkbox(
            ui,
            "Vignetting",
            &mut model.vignetting,
            availability.is_none_or(|value| value.vignetting),
            OpticsAction::SetVignetting,
            action,
        );
        component_checkbox(
            ui,
            "Chromatic aberration",
            &mut model.chromatic_aberration,
            availability.is_none_or(|value| value.chromatic_aberration),
            OpticsAction::SetChromaticAberration,
            action,
        );
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
    if let Some(database) = &model.database {
        ui.label(
            egui::RichText::new(format!(
                "Profile database: {} {}",
                database.source, database.snapshot
            ))
            .small()
            .color(colors::TEXT_MUTED),
        );
    }
    let changed = model.profile != LensProfileSelection::Off
        || !model.distortion
        || !model.vignetting
        || !model.chromatic_aberration;
    if ui
        .add_enabled(changed, egui::Button::new("Reset optics").frame(false))
        .clicked()
    {
        *action = Some(OpticsAction::Reset);
    }

    filter_changed.then(|| model.profile_filter.clone())
}

fn component_checkbox(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut bool,
    available: bool,
    make_action: fn(bool) -> OpticsAction,
    action: &mut Option<OpticsAction>,
) {
    let before = *value;
    let response = ui.add_enabled(available, egui::Checkbox::new(value, label));
    if response.changed() {
        let next = *value;
        *action = Some(make_action(next));
    }
    if !available {
        ui.label(
            egui::RichText::new("unavailable in this profile or capture")
                .small()
                .color(colors::TEXT_DISABLED),
        );
        *value = before;
    }
}

fn show_match_status(ui: &mut egui::Ui, model: &OpticsPanelModel) {
    match model.match_result.as_ref() {
        Some(ProfileMatch::Unique(summary)) => {
            ui.label(egui::RichText::new(format!("Detected: {}", summary.lens)).strong());
        }
        Some(ProfileMatch::Ambiguous(candidates)) => {
            ui.colored_label(
                colors::WARNING,
                format!(
                    "Ambiguous match: choose one of {} profiles",
                    candidates.len()
                ),
            );
        }
        Some(ProfileMatch::MissingMetadata { fields }) => {
            ui.colored_label(colors::WARNING, format_missing_fields(fields));
        }
        Some(ProfileMatch::CameraNotFound) => {
            ui.colored_label(colors::WARNING, "No compatible camera profile was found");
        }
        Some(ProfileMatch::LensNotFound) => {
            ui.colored_label(colors::WARNING, "No compatible lens profile was found");
        }
        None => {
            ui.label(
                egui::RichText::new("Matching lens profile…")
                    .small()
                    .color(colors::TEXT_MUTED),
            );
        }
    }
    if let Some(error) = &model.error {
        ui.colored_label(colors::ERROR, error);
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
    let focal = model.focal_length_mm.map_or_else(
        || "unknown focal length".to_owned(),
        |value| format!("{value:.1} mm"),
    );
    let aperture = model.aperture_f_number.map_or_else(
        || "aperture unknown".to_owned(),
        |value| format!("f/{value:.1}"),
    );
    let focus = model.focus_distance_m.map_or_else(
        || "focus distance unknown".to_owned(),
        |value| format!("focus {value:.1} m"),
    );
    ui.label(
        egui::RichText::new(format!("Captured: {lens} · {focal} · {aperture} · {focus}"))
            .small()
            .color(colors::TEXT_MUTED),
    );
}

fn profile_label(model: &OpticsPanelModel) -> String {
    match &model.profile {
        LensProfileSelection::Off => "Off".to_owned(),
        LensProfileSelection::Automatic => match model.match_result.as_ref() {
            Some(ProfileMatch::Unique(summary)) => format!("Automatic · {}", summary.lens),
            _ => "Automatic".to_owned(),
        },
        LensProfileSelection::Lensfun { profile_id } => model
            .all_candidate_profiles()
            .into_iter()
            .find(|profile| profile.id == *profile_id)
            .map_or_else(|| profile_id.clone(), |profile| profile.lens),
    }
}

fn candidate_profiles(model: &OpticsPanelModel) -> Vec<LensProfileSummary> {
    let mut profiles = model.all_candidate_profiles();
    let filter = model.profile_filter.trim().to_ascii_lowercase();
    if !filter.is_empty() {
        profiles.retain(|profile| {
            [
                profile.id.as_str(),
                profile.camera.as_str(),
                profile.lens.as_str(),
                profile.mount.as_str(),
            ]
            .into_iter()
            .any(|value| value.to_ascii_lowercase().contains(&filter))
        });
    }
    profiles
}

impl OpticsPanelModel {
    fn all_candidate_profiles(&self) -> Vec<LensProfileSummary> {
        if self.candidates.is_empty() {
            match self.match_result.as_ref() {
                Some(ProfileMatch::Unique(summary)) => vec![summary.clone()],
                Some(ProfileMatch::Ambiguous(candidates)) => candidates.clone(),
                _ => Vec::new(),
            }
        } else {
            self.candidates.clone()
        }
    }
}

fn available_components(model: &OpticsPanelModel) -> Option<CorrectionComponents> {
    match &model.profile {
        LensProfileSelection::Off => None,
        LensProfileSelection::Automatic => match model.match_result.as_ref() {
            Some(ProfileMatch::Unique(summary)) => Some(summary.available),
            _ => None,
        },
        LensProfileSelection::Lensfun { profile_id } => model
            .all_candidate_profiles()
            .into_iter()
            .find(|profile| profile.id == *profile_id)
            .map(|profile| profile.available),
    }
}

fn format_missing_fields(fields: &[MetadataField]) -> String {
    let values = fields.iter().map(ToString::to_string).collect::<Vec<_>>();
    format!("Missing metadata: {}", values.join(", "))
}

const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_profiles_preserve_worker_order() {
        let candidates = vec![summary("b"), summary("a")];
        let result = ProfileMatch::Ambiguous(candidates.clone());
        let model = model_with(result);
        assert_eq!(candidate_profiles(&model), candidates);
    }

    #[test]
    fn off_profile_is_not_reported_as_available() {
        let model = OpticsPanelModel {
            profile: LensProfileSelection::Off,
            distortion: true,
            vignetting: true,
            chromatic_aberration: true,
            camera: None,
            lens: None,
            focal_length_mm: None,
            aperture_f_number: None,
            focus_distance_m: None,
            match_result: Some(ProfileMatch::Unique(summary("lens"))),
            candidates: Vec::new(),
            profile_filter: String::new(),
            database: None,
            error: None,
            applied: None,
            used_infinity_distance_fallback: false,
            scale: None,
        };
        assert!(matches!(model.profile, LensProfileSelection::Off));
        assert!(available_components(&model).is_none());
    }

    fn model_with(match_result: ProfileMatch) -> OpticsPanelModel {
        OpticsPanelModel {
            profile: LensProfileSelection::Automatic,
            distortion: true,
            vignetting: true,
            chromatic_aberration: true,
            camera: None,
            lens: None,
            focal_length_mm: None,
            aperture_f_number: None,
            focus_distance_m: None,
            match_result: Some(match_result),
            candidates: Vec::new(),
            profile_filter: String::new(),
            database: None,
            error: None,
            applied: None,
            used_infinity_distance_fallback: false,
            scale: None,
        }
    }

    fn summary(id: &str) -> LensProfileSummary {
        LensProfileSummary {
            id: id.to_owned(),
            camera: "Camera".to_owned(),
            lens: "Lens".to_owned(),
            mount: "Mount".to_owned(),
            available: CorrectionComponents {
                distortion: true,
                vignetting: true,
                chromatic_aberration: true,
            },
        }
    }
}
