//! The Edit tool's panel body: view models, the section list, and the shared
//! slider wiring. Each group of controls lives in its own submodule.

mod changes;
mod color;
mod export;
mod histogram;
mod tone;

pub(crate) use export::{ExportKind, ExportUiSettings, PngDepth, SizeMode};
pub(crate) use histogram::{histogram_canvas, histogram_panel};

use eframe::egui;
use rohditor_core::{HSL_CHANNEL_CENTERS, WhiteBalanceMode};
use rohditor_edit::{
    CameraProfileSelection, HighlightAdjustments, HighlightMethod, LensProfileSelection,
    OpticsAction, RenderingProfileSelection,
};
use rohditor_engine::tools::Picker;

use self::changes::SectionChanges;
use self::color::{
    show_color_grading_controls, show_color_mixer_controls, show_profile_controls,
    show_saturation_controls, show_white_balance_controls,
};
use self::export::show_export_controls;
use self::tone::{show_highlight_controls, show_tone_controls, show_tone_curve_controls};
use super::detail;
use super::optics::{self, OpticsPanelModel};
use super::theme::{self, colors};
use super::widgets::{self, AdjustmentSpec, CollapsibleSection};

pub(crate) const COLOR_MIXER_LABELS: [&str; 8] = [
    "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdjustmentTarget {
    WhiteBalanceRed,
    WhiteBalanceGreen,
    WhiteBalanceBlue,
    WhiteBalanceTemperature,
    WhiteBalanceTint,
    Exposure,
    Contrast,
    Highlights,
    Shadows,
    Whites,
    Blacks,
    HighlightThreshold,
    FalseColorSteps,
    ChromaNoiseReduction,
    LuminanceNoiseReduction,
    LuminanceDetail,
    CaptureAmount,
    CaptureRadius,
    CaptureNoise,
    DefringePurpleAmount,
    DefringePurpleHueStart,
    DefringePurpleHueEnd,
    DefringeGreenAmount,
    DefringeGreenHueStart,
    DefringeGreenHueEnd,
    ToneCurveShadows,
    ToneCurveDarks,
    ToneCurveLights,
    ToneCurveHighlights,
    Saturation,
    Vibrance,
    HslHue(usize),
    HslSaturation(usize),
    HslLuminance(usize),
    GradingShadows(usize),
    GradingMidtones(usize),
    GradingHighlights(usize),
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
    pub revision: u64,
    pub dirty: bool,
    pub has_adjustments: bool,
    /// White balance differs from this photo's as-shot default, which the
    /// panel cannot tell from the values alone.
    pub white_balance_changed: bool,
    pub values: AdjustmentValues,
    pub camera_profile: CameraProfileSelection,
    pub camera_profile_choices: Vec<CameraProfileChoice>,
    pub rendering_profile: RenderingProfileSelection,
    pub ranges: AdjustmentRanges,
    pub export_ready: bool,
    pub export_in_progress: bool,
    pub auto_tone_available: bool,
    pub picker_mode: Option<Picker>,
    pub color_mixer_channel: usize,
    pub optics: OpticsPanelModel,
    pub false_color: rohditor_edit::FalseColorSuppression,
    pub noise_reduction: rohditor_edit::NoiseReduction,
    /// What a reset restores; "changed" is judged against it.
    pub defaults: PanelDefaults,
    pub capture_sharpening: rohditor_edit::CaptureSharpening,
    pub defringe: rohditor_edit::Defringe,
    pub noise_suggestion: NoiseSuggestion,
}

/// The document's default values for the settings that are not simply the
/// neutral slider position. The photo's own noise estimate, for one, sets its
/// default noise reduction.
#[derive(Debug, Clone)]
pub(crate) struct PanelDefaults {
    pub camera_profile: CameraProfileSelection,
    pub rendering_profile: RenderingProfileSelection,
    pub highlights: HighlightAdjustments,
    pub noise_reduction: rohditor_edit::NoiseReduction,
}

/// The noise-reduction strength Auto would set, as far as it is known.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum NoiseSuggestion {
    /// Not estimated yet; clicking Auto estimates first.
    Unknown,
    Estimating,
    /// The frame is too small to profile, or the estimate failed.
    Unavailable,
    Suggested {
        chroma: f32,
    },
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
    pub reset_section: Option<PanelSection>,
    pub export: bool,
}

/// The collapsible sections of the Edit panel that can be reset on their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelSection {
    Tone,
    WhiteBalance,
    Saturation,
    ToneCurve,
    HighlightReconstruction,
    ColorProfile,
    ColorMixer,
    ColorGrading,
    NoiseReduction,
    CaptureSharpening,
    FalseColor,
    Defringe,
    Optics,
}

impl AdjustmentPanelOutput {
    /// Remembers a section's reset click; the topmost click of a frame wins.
    fn request_section_reset(&mut self, section: PanelSection, clicked: bool) {
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
    export_settings: &mut ExportUiSettings,
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
    show_sections(ui, &mut document, export_settings, &mut output);
    output
}

/// The panel's flat list of sections. The everyday controls start open; the
/// rest stay out of the way, and flag their header while they hold an edit.
fn show_sections(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    export_settings: &mut ExportUiSettings,
    output: &mut AdjustmentPanelOutput,
) {
    let changed = SectionChanges::of(document);
    use PanelSection as P;
    section(
        ui,
        output,
        P::Tone,
        CollapsibleSection::new("Tone")
            .default_open()
            .changed(changed.tone),
        |ui, output| show_tone_controls(ui, document, output),
    );
    section(
        ui,
        output,
        P::WhiteBalance,
        CollapsibleSection::new("White balance")
            .default_open()
            .changed(changed.white_balance),
        |ui, output| show_white_balance_controls(ui, document, output),
    );
    section(
        ui,
        output,
        P::Saturation,
        CollapsibleSection::new("Saturation & vibrance")
            .default_open()
            .changed(changed.saturation),
        |ui, output| show_saturation_controls(ui, document, output),
    );
    section(
        ui,
        output,
        P::ToneCurve,
        CollapsibleSection::new("Tone curve").changed(changed.tone_curve),
        |ui, output| show_tone_curve_controls(ui, document, output),
    );
    section(
        ui,
        output,
        P::HighlightReconstruction,
        CollapsibleSection::new("Highlight reconstruction").changed(changed.highlights),
        |ui, output| show_highlight_controls(ui, document, output),
    );
    section(
        ui,
        output,
        P::ColorProfile,
        CollapsibleSection::new("Color profile").changed(changed.profile),
        |ui, output| show_profile_controls(ui, document, output),
    );
    section(
        ui,
        output,
        P::ColorMixer,
        CollapsibleSection::new("Color mixer").changed(changed.color_mixer),
        |ui, output| show_color_mixer_controls(ui, document, output),
    );
    section(
        ui,
        output,
        P::ColorGrading,
        CollapsibleSection::new("Three-way RGB tint").changed(changed.grading),
        |ui, output| show_color_grading_controls(ui, document, output),
    );
    section(
        ui,
        output,
        P::NoiseReduction,
        CollapsibleSection::new("Noise reduction")
            .changed(document.noise_reduction != document.defaults.noise_reduction)
            .tooltip(detail::NOISE_REDUCTION_HINT),
        |ui, output| detail::noise_reduction(ui, document, output),
    );
    section(
        ui,
        output,
        P::CaptureSharpening,
        CollapsibleSection::new("Capture sharpening").changed(document.capture_sharpening.enabled),
        |ui, output| detail::capture_sharpening(ui, document, output),
    );
    section(
        ui,
        output,
        P::FalseColor,
        CollapsibleSection::new("Moiré / false colour").changed(document.false_color.is_active()),
        |ui, output| detail::false_color(ui, document, output),
    );
    section(
        ui,
        output,
        P::Defringe,
        CollapsibleSection::new("Defringe")
            .changed(document.defringe.is_active())
            .tooltip(detail::DEFRINGE_HINT),
        |ui, output| detail::defringe(ui, document, output),
    );
    let optics_changed = !matches!(document.optics.profile, LensProfileSelection::Off);
    output.optics_filter = section(
        ui,
        output,
        P::Optics,
        CollapsibleSection::new("Optics").changed(optics_changed),
        |ui, output| optics::show(ui, &mut document.optics, &mut output.optics_action),
    )
    .flatten();
    CollapsibleSection::new("Export").show(ui, |ui| {
        show_export_controls(ui, document, export_settings, output);
    });
}

/// Shows one section with a reset button, and records a click on it.
fn section<R>(
    ui: &mut egui::Ui,
    output: &mut AdjustmentPanelOutput,
    which: PanelSection,
    section: CollapsibleSection<'_>,
    add_contents: impl FnOnce(&mut egui::Ui, &mut AdjustmentPanelOutput) -> R,
) -> Option<R> {
    let shown = section
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
    target: AdjustmentTarget,
    value: &mut f32,
    spec: AdjustmentSpec<'_>,
) -> egui::Response {
    let neutral = spec.neutral;
    let response = match target {
        AdjustmentTarget::WhiteBalanceTemperature => {
            widgets::temperature_adjustment_slider(ui, value, spec)
        }
        AdjustmentTarget::WhiteBalanceTint => widgets::tint_adjustment_slider(ui, value, spec),
        AdjustmentTarget::HslHue(channel) => {
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
