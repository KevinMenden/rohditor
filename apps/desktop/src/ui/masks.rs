//! The Masks tool's panel: the layer list, the selected layer's components and its sliders.
//!
//! Presentation only. It shows a [`MasksPanelModel`] and reports what the user asked for as
//! [`MasksAction`]s; `app/masks.rs` turns them into recipe edits through the engine's
//! `MaskToolSession` and `LayerEdit`.

use eframe::egui;
use rohditor_edit::{MaskLayer, MaskMode, MaskShape};
use rohditor_engine::tools::{LayerEdit, LocalSlider, MaskEditMode, MaskLimit};

use super::theme::colors;
use super::widgets::{self, AdjustmentSpec, ValueScale};

pub(crate) struct MasksPanelModel<'a> {
    pub layers: &'a [MaskLayer],
    pub selected_layer: Option<u32>,
    pub selected_component: Option<usize>,
    /// What the next drag on the image does; anything but `Select` is a creation in progress.
    pub mode: MaskEditMode,
    pub show_overlay: bool,
    /// The recipe limit that stopped the last edit, if any.
    pub limit: Option<MaskLimit>,
}

/// How a slider event relates to a drag, so the caller can make a whole drag one undo step.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SliderPhase {
    pub changed: bool,
    pub drag_started: bool,
    pub dragged: bool,
    pub drag_stopped: bool,
    pub reset: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum MasksAction {
    SelectLayer(u32),
    SelectComponent(usize),
    /// Start drawing a gradient into a new layer.
    NewLayer(MaskEditMode),
    /// Start drawing a gradient into the selected layer.
    NewComponent(MaskEditMode, MaskMode),
    CancelCreate,
    DeleteLayer(u32),
    DeleteComponent(usize),
    Edit(u32, LayerEdit),
    /// A slider event with the new value.
    Slider(u32, LocalSlider, f32, SliderPhase),
    SetOverlay(bool),
}

pub(crate) fn show(ui: &mut egui::Ui, model: &MasksPanelModel<'_>) -> Vec<MasksAction> {
    let mut actions = Vec::new();
    widgets::section_header(ui, "Masks");
    show_layer_list(ui, model, &mut actions);
    show_new_mask(ui, model, &mut actions);

    let selected = model
        .selected_layer
        .and_then(|id| model.layers.iter().find(|layer| layer.id == id));
    if let Some(layer) = selected {
        widgets::section_header(ui, "Components");
        show_components(ui, model, layer, &mut actions);
        widgets::section_header(ui, "Adjustments");
        show_sliders(ui, layer, &mut actions);
    }

    ui.add_space(6.0);
    let mut overlay = model.show_overlay;
    if ui
        .checkbox(&mut overlay, "Show overlay")
        .on_hover_text("Tint the selected mask red on the image (O)")
        .changed()
    {
        actions.push(MasksAction::SetOverlay(overlay));
    }
    if let Some(limit) = model.limit {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(limit.to_string()).color(colors::WARNING));
    }
    actions
}

fn show_layer_list(ui: &mut egui::Ui, model: &MasksPanelModel<'_>, actions: &mut Vec<MasksAction>) {
    if model.layers.is_empty() {
        ui.label(
            egui::RichText::new("No masks yet. Create one below, then drag on the image.")
                .small()
                .color(colors::TEXT_MUTED),
        );
        return;
    }
    let last = model.layers.len() - 1;
    for (index, layer) in model.layers.iter().enumerate() {
        ui.horizontal(|ui| {
            let mut enabled = layer.enabled;
            if ui
                .checkbox(&mut enabled, "")
                .on_hover_text("Enable or disable this mask")
                .changed()
            {
                actions.push(MasksAction::Edit(layer.id, LayerEdit::Enabled(enabled)));
            }
            show_layer_name(ui, model, layer, actions);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("×").on_hover_text("Delete mask").clicked() {
                    actions.push(MasksAction::DeleteLayer(layer.id));
                }
                let down = ui.add_enabled(index < last, egui::Button::new("↓").small());
                if down.on_hover_text("Apply later").clicked() {
                    actions.push(MasksAction::Edit(layer.id, LayerEdit::MoveDown));
                }
                let up = ui.add_enabled(index > 0, egui::Button::new("↑").small());
                if up.on_hover_text("Apply earlier").clicked() {
                    actions.push(MasksAction::Edit(layer.id, LayerEdit::MoveUp));
                }
            });
        });
    }
}

/// The layer's name: a selectable label, or a text field after a double click.
fn show_layer_name(
    ui: &mut egui::Ui,
    model: &MasksPanelModel<'_>,
    layer: &MaskLayer,
    actions: &mut Vec<MasksAction>,
) {
    // Rename state lives in egui's temporary memory: (text, whether the field has been focused).
    let rename_id = ui.id().with(("mask_rename", layer.id));
    let editing: Option<(String, bool)> = ui.data(|data| data.get_temp(rename_id));
    let Some((mut text, focused)) = editing else {
        let label = ui
            .selectable_label(model.selected_layer == Some(layer.id), &layer.name)
            .on_hover_text("Double-click to rename");
        if label.double_clicked() {
            ui.data_mut(|data| data.insert_temp(rename_id, (layer.name.clone(), false)));
        } else if label.clicked() {
            actions.push(MasksAction::SelectLayer(layer.id));
        }
        return;
    };
    let field = ui.add(egui::TextEdit::singleline(&mut text).desired_width(110.0));
    if !focused {
        field.request_focus();
    }
    if field.lost_focus() {
        ui.data_mut(|data| data.remove_temp::<(String, bool)>(rename_id));
        let cancelled = ui.input(|input| input.key_pressed(egui::Key::Escape));
        if !cancelled && !text.trim().is_empty() {
            actions.push(MasksAction::Edit(
                layer.id,
                LayerEdit::Rename(text.trim().to_owned()),
            ));
        }
    } else {
        ui.data_mut(|data| data.insert_temp(rename_id, (text, true)));
    }
}

fn show_new_mask(ui: &mut egui::Ui, model: &MasksPanelModel<'_>, actions: &mut Vec<MasksAction>) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.menu_button("New mask", |ui| {
            for (label, shape) in SHAPES {
                if ui.button(label).clicked() {
                    actions.push(MasksAction::NewLayer(shape));
                    ui.close();
                }
            }
        });
        if model.mode != MaskEditMode::Select && ui.button("Cancel").clicked() {
            actions.push(MasksAction::CancelCreate);
        }
    });
    if model.mode != MaskEditMode::Select {
        ui.label(
            egui::RichText::new("Drag on the image to draw it. Esc cancels.")
                .small()
                .color(colors::ACCENT_HOVER),
        );
    }
}

/// The shapes a gradient can be created as. Brush joins them with its own tool.
const SHAPES: [(&str, MaskEditMode); 2] = [
    ("Linear gradient", MaskEditMode::CreateLinear),
    ("Radial gradient", MaskEditMode::CreateRadial),
];

const fn shape_name(shape: &MaskShape) -> &'static str {
    match shape {
        MaskShape::Linear(_) => "Linear gradient",
        MaskShape::Radial(_) => "Radial gradient",
        MaskShape::Brush(_) => "Brush",
    }
}

const fn mode_name(mode: MaskMode) -> &'static str {
    match mode {
        MaskMode::Add => "Add",
        MaskMode::Subtract => "Subtract",
        MaskMode::Intersect => "Intersect",
    }
}

fn show_components(
    ui: &mut egui::Ui,
    model: &MasksPanelModel<'_>,
    layer: &MaskLayer,
    actions: &mut Vec<MasksAction>,
) {
    for (index, component) in layer.components.iter().enumerate() {
        ui.horizontal(|ui| {
            let selected = model.selected_component == Some(index);
            let label = format!("{}. {}", index + 1, shape_name(&component.shape));
            if ui.selectable_label(selected, label).clicked() {
                actions.push(MasksAction::SelectComponent(index));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("×")
                    .on_hover_text("Delete component")
                    .clicked()
                {
                    actions.push(MasksAction::DeleteComponent(index));
                }
                let mut invert = component.invert;
                if ui
                    .checkbox(&mut invert, "Invert")
                    .on_hover_text("Select everything outside this shape")
                    .changed()
                {
                    actions.push(MasksAction::Edit(
                        layer.id,
                        LayerEdit::ComponentInvert {
                            component: index,
                            invert,
                        },
                    ));
                }
                // The first component has nothing to combine with.
                if index == 0 {
                    ui.label(egui::RichText::new("Base").color(colors::TEXT_MUTED));
                } else {
                    let mut mode = component.mode;
                    egui::ComboBox::from_id_salt(("mask_component_mode", layer.id, index))
                        .selected_text(mode_name(mode))
                        .width(84.0)
                        .show_ui(ui, |ui| {
                            for choice in [MaskMode::Add, MaskMode::Subtract, MaskMode::Intersect] {
                                ui.selectable_value(&mut mode, choice, mode_name(choice));
                            }
                        });
                    if mode != component.mode {
                        actions.push(MasksAction::Edit(
                            layer.id,
                            LayerEdit::ComponentMode {
                                component: index,
                                mode,
                            },
                        ));
                    }
                }
            });
        });
    }
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        for mode in [MaskMode::Add, MaskMode::Subtract] {
            ui.menu_button(mode_name(mode), |ui| {
                for (label, shape) in SHAPES {
                    if ui.button(label).clicked() {
                        actions.push(MasksAction::NewComponent(shape, mode));
                        ui.close();
                    }
                }
            });
        }
    });
}

/// Label, unit and precision of a slider, matching the global sliders of the same name.
fn slider_spec(slider: LocalSlider) -> AdjustmentSpec<'static> {
    let range = slider.range();
    let (label, decimals, suffix, scale) = match slider {
        LocalSlider::Opacity => ("Opacity", 0, "%", ValueScale::Percent),
        LocalSlider::Exposure => ("Exposure", 2, " EV", ValueScale::Raw),
        LocalSlider::Temperature => ("Temperature", 0, "%", ValueScale::OffsetPercent),
        LocalSlider::Tint => ("Tint", 0, "%", ValueScale::OffsetPercent),
        LocalSlider::Contrast => ("Contrast", 0, "%", ValueScale::OffsetPercent),
        LocalSlider::Highlights => ("Highlights", 0, "%", ValueScale::OffsetPercent),
        LocalSlider::Shadows => ("Shadows", 0, "%", ValueScale::OffsetPercent),
        LocalSlider::Whites => ("Whites", 0, "%", ValueScale::OffsetPercent),
        LocalSlider::Blacks => ("Blacks", 0, "%", ValueScale::OffsetPercent),
        LocalSlider::Saturation => ("Saturation", 0, "%", ValueScale::OffsetPercent),
    };
    AdjustmentSpec {
        label,
        minimum: range.minimum,
        maximum: range.maximum,
        neutral: range.neutral,
        decimals,
        step: 0.01,
        suffix,
        scale,
    }
}

fn show_sliders(ui: &mut egui::Ui, layer: &MaskLayer, actions: &mut Vec<MasksAction>) {
    for slider in LocalSlider::ALL {
        let spec = slider_spec(slider);
        let mut value = slider.get(layer);
        // The same widget, reset button and drag events as the global sliders.
        let response = widgets::adjustment_slider(ui, &mut value, spec);
        if response.reset_clicked {
            value = spec.neutral;
        }
        let widget = &response.response;
        if widget.changed()
            || widget.drag_started()
            || widget.drag_stopped()
            || response.reset_clicked
        {
            actions.push(MasksAction::Slider(
                layer.id,
                slider,
                value,
                SliderPhase {
                    changed: widget.changed(),
                    drag_started: widget.drag_started(),
                    dragged: widget.dragged(),
                    drag_stopped: widget.drag_stopped(),
                    reset: response.reset_clicked,
                },
            ));
        }
    }
}
