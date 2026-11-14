use eframe::egui;
use rohditor_core::WhiteBalanceMode;
use rohditor_edit::RenderingProfileSelection;
use rohditor_engine::tools::Picker;

use super::{
    AdjustmentPanelOutput, AdjustmentTarget, COLOR_MIXER_LABELS, DocumentPanelModel, record_slider,
};
use crate::ui::theme::colors;
use crate::ui::widgets::{self, AdjustmentSpec, ValueScale};

pub(super) fn show_profile_controls(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    let mut profile_index = document
        .camera_profile_choices
        .iter()
        .position(|choice| choice.selection == document.camera_profile)
        .unwrap_or(0);
    let selected_profile = document.camera_profile_choices.get(profile_index);
    widgets::dropdown(
        ui,
        "camera_profile",
        "Camera profile",
        selected_profile.map_or("Automatic (RAW / decoder matrix)", |choice| {
            choice.label.as_str()
        }),
        |ui| {
            for (index, choice) in document.camera_profile_choices.iter().enumerate() {
                ui.selectable_value(&mut profile_index, index, &choice.label)
                    .on_hover_text(&choice.tooltip);
            }
        },
    );
    if let Some(choice) = document.camera_profile_choices.get(profile_index) {
        ui.label(
            egui::RichText::new(&choice.detail)
                .small()
                .color(colors::TEXT_MUTED),
        );
        if profile_index
            != document
                .camera_profile_choices
                .iter()
                .position(|candidate| candidate.selection == document.camera_profile)
                .unwrap_or(0)
        {
            output.camera_profile = Some(choice.selection.clone());
            document.camera_profile = choice.selection.clone();
        }
    }
    if ui
        .small_button("Import DCP…")
        .on_hover_text("Validate and install a compatible DCP camera profile (matrices, hue/saturation map, and look table)")
        .clicked()
    {
        output.import_camera_profile = true;
    }
    ui.add_space(4.0);
    let original_rendering = document.rendering_profile;
    let mut rendering = original_rendering;
    widgets::dropdown(
        ui,
        "rendering_profile",
        "Rendering profile",
        rendering.display_name(),
        |ui| {
            ui.selectable_value(
                &mut rendering,
                RenderingProfileSelection::STANDARD,
                "Rohditor Standard",
            )
            .on_hover_text("Balanced tone and highlight rolloff for a finished starting point");
            ui.selectable_value(
                &mut rendering,
                RenderingProfileSelection::MATCHED,
                "Rohditor Matched",
            )
            .on_hover_text(
                "Tone and colour fitted to this photo's own camera JPEG; a typical camera look is used until the fit finishes",
            );
            ui.selectable_value(
                &mut rendering,
                RenderingProfileSelection::NEUTRAL,
                "Rohditor Neutral",
            )
            .on_hover_text(
                "No base tone or color rendering; useful for manual development and comparison",
            );
        },
    );
    ui.label(
        egui::RichText::new(match rendering {
            RenderingProfileSelection::RohditorStandard { .. } => {
                "Balanced tone and highlight rolloff for a finished starting point"
            }
            RenderingProfileSelection::RohditorNeutral => {
                "No base tone or color rendering; useful for manual development and comparison"
            }
            RenderingProfileSelection::RohditorMatched { .. } => {
                "Tone and colour fitted to this photo's camera JPEG preview"
            }
        })
        .small()
        .color(colors::TEXT_MUTED),
    );
    if rendering != original_rendering {
        output.rendering_profile = Some(rendering);
        document.rendering_profile = rendering;
    }
}

pub(super) fn show_white_balance_controls(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    let mut mode = document.values.white_balance_mode;
    widgets::dropdown(
        ui,
        "white_balance_mode",
        "Mode",
        match mode {
            WhiteBalanceMode::AsShot => "As shot",
            WhiteBalanceMode::TemperatureTint => "Temperature / tint",
            WhiteBalanceMode::ManualMultipliers => "Manual multipliers",
        },
        |ui| {
            ui.selectable_value(&mut mode, WhiteBalanceMode::AsShot, "As shot");
            ui.selectable_value(
                &mut mode,
                WhiteBalanceMode::TemperatureTint,
                "Temperature / tint",
            );
            ui.selectable_value(
                &mut mode,
                WhiteBalanceMode::ManualMultipliers,
                "Manual multipliers",
            );
        },
    );
    if mode != document.values.white_balance_mode {
        output.white_balance_mode = Some(mode);
        document.values.white_balance_mode = mode;
    }
    let white_balance_hint = match mode {
        WhiteBalanceMode::AsShot => "Using the camera's as-shot white balance",
        WhiteBalanceMode::TemperatureTint => {
            "Camera-calibrated temperature (Kelvin) and green/magenta tint"
        }
        WhiteBalanceMode::ManualMultipliers => {
            "Manual values are relative to the camera multipliers"
        }
    };
    ui.label(
        egui::RichText::new(white_balance_hint)
            .small()
            .color(colors::TEXT_MUTED),
    );
    let picker_active = document.picker_mode == Some(Picker::WhiteBalance);
    let picker_label = if picker_active {
        "Cancel picker"
    } else {
        "Pick neutral"
    };
    if ui
        .small_button(picker_label)
        .on_hover_text("Click a neutral or gray area in the image")
        .clicked()
    {
        output.picker_mode = Some(if picker_active {
            None
        } else {
            Some(Picker::WhiteBalance)
        });
    }
    ui.add_space(4.0);

    if document.values.white_balance_mode != WhiteBalanceMode::ManualMultipliers {
        record_slider(
            ui,
            &mut output.interactions,
            AdjustmentTarget::WhiteBalanceTemperature,
            &mut document.values.white_balance_temperature,
            AdjustmentSpec {
                label: "Temperature",
                minimum: document.ranges.temperature.minimum,
                maximum: document.ranges.temperature.maximum,
                neutral: document.ranges.temperature.neutral,
                decimals: 0,
                step: 10.0,
                suffix: " K",
                scale: ValueScale::Raw,
            },
        );
        record_slider(
            ui,
            &mut output.interactions,
            AdjustmentTarget::WhiteBalanceTint,
            &mut document.values.white_balance_tint,
            AdjustmentSpec {
                label: "Tint",
                minimum: document.ranges.tint.minimum,
                maximum: document.ranges.tint.maximum,
                neutral: document.ranges.tint.neutral,
                decimals: 0,
                step: 0.01,
                suffix: "%",
                scale: ValueScale::OffsetPercent,
            },
        );
    } else {
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
}

pub(super) fn show_saturation_controls(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
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
    record_slider(
        ui,
        &mut output.interactions,
        AdjustmentTarget::Vibrance,
        &mut document.values.vibrance,
        AdjustmentSpec {
            label: "Vibrance",
            minimum: document.ranges.vibrance.minimum,
            maximum: document.ranges.vibrance.maximum,
            neutral: document.ranges.vibrance.neutral,
            decimals: 0,
            step: 0.01,
            suffix: "%",
            scale: ValueScale::OffsetPercent,
        },
    );
}

pub(super) fn show_color_mixer_controls(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    const CHANNEL_COLORS: [egui::Color32; 8] = [
        egui::Color32::from_rgb(220, 66, 67),
        egui::Color32::from_rgb(232, 132, 48),
        egui::Color32::from_rgb(220, 190, 50),
        egui::Color32::from_rgb(72, 174, 91),
        egui::Color32::from_rgb(52, 181, 178),
        egui::Color32::from_rgb(65, 116, 220),
        egui::Color32::from_rgb(139, 87, 213),
        egui::Color32::from_rgb(205, 75, 164),
    ];
    let selected = document
        .color_mixer_channel
        .min(COLOR_MIXER_LABELS.len() - 1);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (index, (label, color)) in COLOR_MIXER_LABELS
            .into_iter()
            .zip(CHANNEL_COLORS)
            .enumerate()
        {
            if color_swatch(ui, label, color, index == selected).clicked() {
                output.color_mixer_channel = Some(index);
            }
        }
        ui.add_space(2.0);
        let picker_active = document.picker_mode == Some(Picker::ColorMixer);
        if color_picker_swatch(ui, picker_active, CHANNEL_COLORS).clicked() {
            output.picker_mode = Some(if picker_active {
                None
            } else {
                Some(Picker::ColorMixer)
            });
        }
    });
    ui.add_space(3.0);
    ui.label(
        egui::RichText::new(COLOR_MIXER_LABELS[selected])
            .strong()
            .color(colors::TEXT),
    );
    ui.add_space(3.0);

    let ranges = [
        document.ranges.hsl_hue,
        document.ranges.hsl_saturation,
        document.ranges.hsl_luminance,
    ];
    let labels = ["Hue", "Saturation", "Luminance"];
    for component in 0..3 {
        let target = match component {
            0 => AdjustmentTarget::HslHue(selected),
            1 => AdjustmentTarget::HslSaturation(selected),
            _ => AdjustmentTarget::HslLuminance(selected),
        };
        let range = ranges[component];
        record_slider(
            ui,
            &mut output.interactions,
            target,
            &mut document.values.hsl[selected][component],
            AdjustmentSpec {
                label: labels[component],
                minimum: range.minimum,
                maximum: range.maximum,
                neutral: range.neutral,
                decimals: 0,
                step: 0.01,
                suffix: "%",
                scale: ValueScale::OffsetPercent,
            },
        );
    }
}

fn color_swatch(
    ui: &mut egui::Ui,
    label: &str,
    color: egui::Color32,
    selected: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(27.0), egui::Sense::click());
    let visuals = ui.style().interact_selectable(&response, selected);
    if selected {
        ui.painter().circle_stroke(
            rect.center(),
            11.5,
            egui::Stroke::new(2.0_f32, colors::ACCENT_HOVER),
        );
    }
    ui.painter().circle_filled(rect.center(), 8.0, color);
    ui.painter()
        .circle_stroke(rect.center(), 8.0, visuals.bg_stroke);
    response.on_hover_text(label)
}

fn color_picker_swatch(
    ui: &mut egui::Ui,
    selected: bool,
    channel_colors: [egui::Color32; 8],
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(27.0), egui::Sense::click());
    let visuals = ui.style().interact_selectable(&response, selected);
    if selected {
        ui.painter().circle_stroke(
            rect.center(),
            11.5,
            egui::Stroke::new(2.0_f32, colors::ACCENT_HOVER),
        );
    }
    for (index, color) in channel_colors.into_iter().enumerate() {
        let angle = index as f32 * std::f32::consts::TAU / channel_colors.len() as f32;
        let offset = egui::vec2(angle.cos(), angle.sin()) * 5.0;
        ui.painter()
            .circle_filled(rect.center() + offset, 2.8, color);
    }
    ui.painter()
        .circle_stroke(rect.center(), 9.0, visuals.bg_stroke);
    response.on_hover_text(if selected {
        "Cancel Color Mixer picker"
    } else {
        "Pick a Color Mixer band from the image"
    })
}

pub(super) fn show_color_grading_controls(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    ui.label(
        egui::RichText::new("Luminance-preserving tint; values are not lift/gamma/gain")
            .small()
            .color(colors::TEXT_MUTED),
    );
    for (group, label, target) in [
        (0, "Shadows", AdjustmentTarget::GradingShadows(0)),
        (1, "Midtones", AdjustmentTarget::GradingMidtones(0)),
        (2, "Highlights", AdjustmentTarget::GradingHighlights(0)),
    ] {
        ui.label(egui::RichText::new(label).small().color(colors::TEXT_MUTED));
        for channel in 0..3 {
            let target = match target {
                AdjustmentTarget::GradingShadows(_) => AdjustmentTarget::GradingShadows(channel),
                AdjustmentTarget::GradingMidtones(_) => AdjustmentTarget::GradingMidtones(channel),
                AdjustmentTarget::GradingHighlights(_) => {
                    AdjustmentTarget::GradingHighlights(channel)
                }
                _ => unreachable!("color grading group is fixed"),
            };
            let label = ["Red", "Green", "Blue"][channel];
            record_slider(
                ui,
                &mut output.interactions,
                target,
                &mut document.values.grading[group][channel],
                AdjustmentSpec {
                    label,
                    minimum: document.ranges.grading.minimum,
                    maximum: document.ranges.grading.maximum,
                    neutral: document.ranges.grading.neutral,
                    decimals: 0,
                    step: 0.01,
                    suffix: "%",
                    scale: ValueScale::OffsetPercent,
                },
            );
        }
    }
}
