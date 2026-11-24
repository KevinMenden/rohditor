//! The Edit tool's panel body: view models, the section list, and the shared
//! slider wiring. Each group of controls lives in its own submodule.

mod color;
mod histogram;
mod tone;

pub(crate) use histogram::{histogram_canvas, histogram_panel};

use eframe::egui;
use rohditor_core::{HSL_CHANNEL_CENTERS, WhiteBalanceMode};
use rohditor_edit::{
    CameraProfileSelection, HighlightMethod, Parameter, RecipeSection, RenderingProfileSelection,
};
use rohditor_engine::presets::Preset;
use rohditor_engine::tools::Picker;

use self::color::{
    show_color_grading_controls, show_color_mixer_controls, show_profile_controls,
    show_saturation_controls, show_white_balance_controls,
};
use self::tone::{show_highlight_controls, show_tone_controls, show_tone_curve_controls};
use super::detail;
use super::info::{self, InfoPanelModel};
use super::optics::{self, OpticsAction, OpticsPanelModel};
use super::presence;
use super::presets::{self, PresetsAction, PresetsPanelState};
use super::theme::{self, colors};
use super::widgets::{self, AdjustmentSpec, CollapsibleSection};

pub(crate) const COLOR_MIXER_LABELS: [&str; 8] = [
    "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdjustmentTarget {
    Parameter(Parameter),
    WhiteBalanceRed,
    WhiteBalanceGreen,
    WhiteBalanceBlue,
}

impl From<Parameter> for AdjustmentTarget {
    fn from(parameter: Parameter) -> Self {
        Self::Parameter(parameter)
    }
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
    pub temperature: AdjustmentRange,
    pub tint: AdjustmentRange,
    pub exposure: AdjustmentRange,
    pub contrast: AdjustmentRange,
    pub highlights: AdjustmentRange,
    pub shadows: AdjustmentRange,
    pub whites: AdjustmentRange,
    pub blacks: AdjustmentRange,
    pub highlight_threshold: AdjustmentRange,
    pub tone_curve: AdjustmentRange,
    pub saturation: AdjustmentRange,
    pub vibrance: AdjustmentRange,
    pub hsl_hue: AdjustmentRange,
    pub hsl_saturation: AdjustmentRange,
    pub hsl_luminance: AdjustmentRange,
    pub grading: AdjustmentRange,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AdjustmentValues {
    pub white_balance_mode: WhiteBalanceMode,
    pub white_balance_red: f32,
    pub white_balance_green: f32,
    pub white_balance_blue: f32,
    pub white_balance_temperature: f32,
    pub white_balance_tint: f32,
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub highlight_method: HighlightMethod,
    pub highlight_threshold: f32,
    pub tone_curve_shadows: f32,
    pub tone_curve_darks: f32,
    pub tone_curve_lights: f32,
    pub tone_curve_highlights: f32,
    pub saturation: f32,
    pub vibrance: f32,
    pub hsl: [[f32; 3]; 8],
    pub grading: [[f32; 3]; 3],
}

#[derive(Debug, Clone)]
pub(crate) struct CameraProfileChoice {
    pub selection: CameraProfileSelection,
    pub label: String,
    pub detail: String,
    pub tooltip: String,
}

#[derive(Debug, Clone)]
pub(crate) struct DocumentPanelModel {
    pub file_name: String,
    pub camera: Option<String>,
    pub sensor_dimensions: Option<(usize, usize)>,
    /// Capture and file metadata; `None` until the RAW metadata is read.
    pub info: Option<InfoPanelModel>,
    pub revision: u64,
    pub dirty: bool,
    pub has_adjustments: bool,
    pub values: AdjustmentValues,
    pub camera_profile: CameraProfileSelection,
    pub camera_profile_choices: Vec<CameraProfileChoice>,
    pub rendering_profile: RenderingProfileSelection,
    pub ranges: AdjustmentRanges,
    pub auto_tone_available: bool,
    pub picker_mode: Option<Picker>,
    pub color_mixer_channel: usize,
    pub optics: OpticsPanelModel,
    pub false_color: rohditor_edit::FalseColorSuppression,
    pub noise_reduction: rohditor_edit::NoiseReduction,
    pub capture_sharpening: rohditor_edit::CaptureSharpening,
    pub defringe: rohditor_edit::Defringe,
    pub presence: rohditor_edit::PresenceAdjustments,
    pub noise_suggestion: NoiseSuggestion,
    /// Sections with edits: flagged in their header, and where saving a preset starts.
    pub changed_sections: Vec<RecipeSection>,
    pub suggested_preset_sections: Vec<RecipeSection>,
}

/// The saved presets and the Presets section's own state.
pub(crate) struct PresetsPanel<'a> {
    pub state: &'a mut PresetsPanelState,
    pub presets: &'a [Preset],
}

pub(crate) use rohditor_engine::open_document::NoiseSuggestion;

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
    pub camera_profile: Option<CameraProfileSelection>,
    pub rendering_profile: Option<RenderingProfileSelection>,
    pub import_camera_profile: bool,
    pub white_balance_mode: Option<WhiteBalanceMode>,
    pub highlight_method: Option<HighlightMethod>,
    pub capture_enabled: Option<bool>,
    pub picker_mode: Option<Option<Picker>>,
    pub color_mixer_channel: Option<usize>,
    pub optics_action: Option<OpticsAction>,
    pub optics_filter: Option<String>,
    pub interactions: Vec<AdjustmentInteraction>,
    pub auto_tone: bool,
    /// The Auto button of the noise-reduction section.
    pub auto_noise_reduction: bool,
    pub reset_all: bool,
    /// The reset button of one section's header.
    pub reset_section: Option<RecipeSection>,
    pub presets: Option<PresetsAction>,
}

impl AdjustmentPanelOutput {
    /// Remembers a section's reset click; the topmost click of a frame wins.
    fn request_section_reset(&mut self, section: RecipeSection, clicked: bool) {
        if clicked {
            self.reset_section.get_or_insert(section);
        }
    }
}

/// The Edit tool's panel body. The surrounding panel, heading and histogram
/// are owned by `tool_panel`.
pub(crate) fn show_body(
    ui: &mut egui::Ui,
    document: Option<DocumentPanelModel>,
    presets: PresetsPanel<'_>,
) -> AdjustmentPanelOutput {
    let mut output = AdjustmentPanelOutput::default();
    let Some(mut document) = document else {
        empty_panel(ui);
        return output;
    };

    document_summary(ui, &document);
    output.reset_all = ui
        .add_enabled(
            document.has_adjustments,
            egui::Button::new("Reset all adjustments").frame(false),
        )
        .on_hover_text("Restore Rohditor Standard defaults")
        .clicked();
    ui.add_space(4.0);
    if let Some(model) = &document.info {
        // Read-only, so it bypasses `section` and has no reset button.
        CollapsibleSection::new("Info").show_resettable(ui, |ui| info::show(ui, model));
    }
    output.presets = CollapsibleSection::new("Presets")
        .show_resettable(ui, |ui| {
            presets::show(
                ui,
                presets.state,
                presets.presets,
                &document.suggested_preset_sections,
            )
        })
        .body
        .flatten();
    show_sections(ui, &mut document, &mut output);
    output
}

/// The panel's flat list of sections. The everyday controls start open; the
/// rest stay out of the way, and flag their header while they hold an edit.
fn show_sections(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    let changed = document.changed_sections.clone();
    use RecipeSection as P;
    section(
        ui,
        output,
        P::Tone,
        &changed,
        CollapsibleSection::new("Tone").default_open(),
        |ui, output| show_tone_controls(ui, document, output),
    );
    section(
        ui,
        output,
        P::Presence,
        &changed,
        CollapsibleSection::new("Presence").tooltip(presence::PRESENCE_HINT),
        |ui, output| presence::presence(ui, document, output),
    );
    section(
        ui,
        output,
        P::WhiteBalance,
        &changed,
        CollapsibleSection::new("White balance").default_open(),
        |ui, output| show_white_balance_controls(ui, document, output),
    );
    section(
        ui,
        output,
        P::Saturation,
        &changed,
        CollapsibleSection::new("Saturation & vibrance").default_open(),
        |ui, output| show_saturation_controls(ui, document, output),
    );
    section(
        ui,
        output,
        P::ToneCurve,
        &changed,
        CollapsibleSection::new("Tone curve"),
        |ui, output| show_tone_curve_controls(ui, document, output),
    );
    section(
        ui,
        output,
        P::HighlightReconstruction,
        &changed,
        CollapsibleSection::new("Highlight reconstruction"),
        |ui, output| show_highlight_controls(ui, document, output),
    );
    section(
        ui,
        output,
        P::ColorProfile,
        &changed,
        CollapsibleSection::new("Color profile"),
        |ui, output| show_profile_controls(ui, document, output),
    );
    section(
        ui,
        output,
        P::ColorMixer,
        &changed,
        CollapsibleSection::new("Color mixer"),
        |ui, output| show_color_mixer_controls(ui, document, output),
    );
    section(
        ui,
        output,
        P::ColorGrading,
        &changed,
        CollapsibleSection::new("Three-way RGB tint"),
        |ui, output| show_color_grading_controls(ui, document, output),
    );
    section(
        ui,
        output,
        P::NoiseReduction,
        &changed,
        CollapsibleSection::new("Noise reduction").tooltip(detail::NOISE_REDUCTION_HINT),
        |ui, output| detail::noise_reduction(ui, document, output),
    );
    section(
        ui,
        output,
        P::CaptureSharpening,
        &changed,
        CollapsibleSection::new("Capture sharpening"),
        |ui, output| detail::capture_sharpening(ui, document, output),
    );
    section(
        ui,
        output,
        P::FalseColor,
        &changed,
        CollapsibleSection::new("Moiré & false color"),
        |ui, output| detail::false_color(ui, document, output),
    );
    section(
        ui,
        output,
        P::Defringe,
        &changed,
        CollapsibleSection::new("Defringe").tooltip(detail::DEFRINGE_HINT),
        |ui, output| detail::defringe(ui, document, output),
    );
    output.optics_filter = section(
        ui,
        output,
        P::Optics,
        &changed,
        CollapsibleSection::new("Optics"),
        |ui, output| optics::show(ui, &mut document.optics, &mut output.optics_action),
    )
    .flatten();
}

/// Shows one section with a reset button, and records a click on it.
fn section<R>(
    ui: &mut egui::Ui,
    output: &mut AdjustmentPanelOutput,
    which: RecipeSection,
    changed: &[RecipeSection],
    section: CollapsibleSection<'_>,
    add_contents: impl FnOnce(&mut egui::Ui, &mut AdjustmentPanelOutput) -> R,
) -> Option<R> {
    let shown = section
        .changed(changed.contains(&which))
        .resettable()
        .show_resettable(ui, |ui| add_contents(ui, output));
    output.request_section_reset(which, shown.reset_clicked);
    shown.body
}

fn empty_panel(ui: &mut egui::Ui) {
    theme::card_frame().show(ui, |ui| {
        ui.label(egui::RichText::new("No photo open").strong());
        ui.label(
            egui::RichText::new("Open a Sony ARW file to reveal the develop controls.")
                .color(colors::TEXT_MUTED),
        );
    });
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
        let save_state = if document.dirty { "UNSAVED" } else { "SAVED" };
        ui.label(
            egui::RichText::new(format!(
                "{width} × {height}  ·  REV {}  ·  {save_state}",
                document.revision
            ))
            .monospace()
            .small()
            .color(colors::TEXT_MUTED),
        );
    }
}

pub(super) fn record_slider(
    ui: &mut egui::Ui,
    interactions: &mut Vec<AdjustmentInteraction>,
    target: impl Into<AdjustmentTarget>,
    value: &mut f32,
    spec: AdjustmentSpec<'_>,
) -> egui::Response {
    let target = target.into();
    let neutral = spec.neutral;
    let response = match target {
        AdjustmentTarget::Parameter(Parameter::Temperature) => {
            widgets::temperature_adjustment_slider(ui, value, spec)
        }
        AdjustmentTarget::Parameter(Parameter::Tint) => {
            widgets::tint_adjustment_slider(ui, value, spec)
        }
        AdjustmentTarget::Parameter(Parameter::HslHue(channel)) => {
            widgets::hue_adjustment_slider(ui, value, spec, HSL_CHANNEL_CENTERS[channel])
        }
        _ => widgets::adjustment_slider(ui, value, spec),
    };
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
    response.response
}
