//! The Masks tool's panel: the layer list, the selected layer's components and its sliders.
//!
//! Presentation only. It shows a [`MasksPanelModel`] and reports what the user asked for as
//! [`MaskAction`]s; `app/masks.rs` hands them to the engine's `MaskToolSession`.

use eframe::egui;
use rohditor_edit::{
    MAX_LOCAL_DETAIL_VARIANTS, MaskLayer, MaskMode, MaskShape, local_detail_is_full,
};
use rohditor_engine::tools::{
    BrushSettings, BrushSlider, LayerEdit, LocalSlider, MaskAction, MaskEditMode, MaskLimit,
    RangeKind, SliderPhase,
};

use super::range_controls::{self, ComponentRef};
use super::theme::colors;
use super::widgets::{self, AdjustmentSpec, ValueScale};

pub(crate) struct MasksPanelModel<'a> {
    pub layers: &'a [MaskLayer],
    pub selected_layer: Option<u32>,
    pub selected_component: Option<usize>,
    /// What the next drag on the image does; anything but `Select` is a creation in progress
    /// (or, for `Paint`, painting).
    pub mode: MaskEditMode,
    /// What the next stroke is painted with.
    pub brush: BrushSettings,
    pub show_overlay: bool,
    /// The recipe limit that stopped the last edit, if any.
    pub limit: Option<MaskLimit>,
}

pub(crate) fn show(ui: &mut egui::Ui, model: &MasksPanelModel<'_>) -> Vec<MaskAction> {
    let mut actions = Vec::new();
    widgets::section_header(ui, "Masks");
    show_layer_list(ui, model, &mut actions);
    show_new_mask(ui, model, &mut actions);
    if painting(model.mode).is_some() {
        widgets::section_header(ui, "Brush");
        show_brush(ui, model, &mut actions);
    }

    let selected = model
        .selected_layer
        .and_then(|id| model.layers.iter().find(|layer| layer.id == id));
    if let Some(layer) = selected {
        widgets::section_header(ui, "Components");
        show_components(ui, model, layer, &mut actions);
        widgets::section_header(ui, "Adjustments");
        show_sliders(ui, model.layers, layer, &mut actions);
    }

    ui.add_space(6.0);
    // Painting always shows the overlay, whatever the setting says.
    let painting = painting(model.mode).is_some();
    let mut overlay = model.show_overlay || painting;
    if ui
        .add_enabled(!painting, egui::Checkbox::new(&mut overlay, "Show overlay"))
        .on_hover_text("Tint the selected mask red on the image (O). Always on while painting.")
        .changed()
    {
        actions.push(MaskAction::SetOverlay(overlay));
    }
    if let Some(limit) = model.limit {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(limit.to_string()).color(colors::WARNING));
    }
    actions
}

fn show_layer_list(ui: &mut egui::Ui, model: &MasksPanelModel<'_>, actions: &mut Vec<MaskAction>) {
    if model.layers.is_empty() {
        ui.label(
            egui::RichText::new("No masks yet. Create one below, then use it on the image.")
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
                actions.push(MaskAction::Edit(layer.id, LayerEdit::Enabled(enabled)));
            }
            show_layer_name(ui, model, layer, actions);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("×").on_hover_text("Delete mask").clicked() {
                    actions.push(MaskAction::DeleteLayer(layer.id));
                }
                let down = ui.add_enabled(index < last, egui::Button::new("↓").small());
                if down.on_hover_text("Apply later").clicked() {
                    actions.push(MaskAction::Edit(layer.id, LayerEdit::MoveDown));
                }
                let up = ui.add_enabled(index > 0, egui::Button::new("↑").small());
                if up.on_hover_text("Apply earlier").clicked() {
                    actions.push(MaskAction::Edit(layer.id, LayerEdit::MoveUp));
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
    actions: &mut Vec<MaskAction>,
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
            actions.push(MaskAction::SelectLayer(layer.id));
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
            actions.push(MaskAction::Edit(
                layer.id,
                LayerEdit::Rename(text.trim().to_owned()),
            ));
        }
    } else {
        ui.data_mut(|data| data.insert_temp(rename_id, (text, true)));
    }
}

fn show_new_mask(ui: &mut egui::Ui, model: &MasksPanelModel<'_>, actions: &mut Vec<MaskAction>) {
    ui.add_space(4.0);
    let (label, hint) = match model.mode {
        MaskEditMode::Paint { .. } => ("Done", "Paint on the image. Esc stops."),
        MaskEditMode::PickColor => (
            "Done",
            "Click the image to pick a color. Shift adds, Alt removes. Esc stops.",
        ),
        _ => ("Cancel", "Drag on the image to draw it. Esc cancels."),
    };
    ui.horizontal(|ui| {
        ui.menu_button("New mask", |ui| {
            for (label, shape) in SHAPES {
                if ui.button(label).clicked() {
                    actions.push(MaskAction::NewLayer(shape));
                    ui.close();
                }
            }
            ui.separator();
            for kind in RangeKind::ALL {
                if ui.button(kind.label()).clicked() {
                    actions.push(MaskAction::NewRange {
                        kind,
                        new_layer: true,
                    });
                    ui.close();
                }
            }
        });
        if model.mode != MaskEditMode::Select && ui.button(label).clicked() {
            actions.push(MaskAction::CancelCreate);
        }
    });
    if model.mode != MaskEditMode::Select {
        ui.label(
            egui::RichText::new(hint)
                .small()
                .color(colors::ACCENT_HOVER),
        );
    }
}

/// The shapes a mask or component can be created as.
const SHAPES: [(&str, MaskEditMode); 3] = [
    ("Linear gradient", MaskEditMode::CreateLinear),
    ("Radial gradient", MaskEditMode::CreateRadial),
    ("Brush", MaskEditMode::Paint { erase: false }),
];

/// Whether the brush is active, and if so whether it erases.
const fn painting(mode: MaskEditMode) -> Option<bool> {
    match mode {
        MaskEditMode::Paint { erase } => Some(erase),
        _ => None,
    }
}

/// Size, feather and flow of the brush, and whether it erases. These are authoring state: the
/// next stroke copies them, strokes already painted keep theirs.
fn show_brush(ui: &mut egui::Ui, model: &MasksPanelModel<'_>, actions: &mut Vec<MaskAction>) {
    let mut erase = painting(model.mode).unwrap_or(false);
    if ui
        .checkbox(&mut erase, "Erase")
        .on_hover_text("Remove from the mask instead of adding to it (E). Hold Alt to flip it.")
        .changed()
    {
        actions.push(MaskAction::SetErase(erase));
    }

    // Size is the radius, as a percentage of the image's long edge. The slider snaps its value
    // to the step on every frame, and the `[` and `]` keys change it by a factor: with a coarser
    // step than this, a factor-1.15 increase at the smallest sizes would snap back and the
    // brush could not grow.
    let mut brush = model.brush;
    for slider in BrushSlider::ALL {
        let (decimals, step) = match slider {
            BrushSlider::Size => (1, 0.0005),
            BrushSlider::Feather | BrushSlider::Flow => (0, 0.01),
        };
        let range = slider.range();
        let spec = AdjustmentSpec {
            label: slider.label(),
            minimum: range.minimum,
            maximum: range.maximum,
            neutral: range.neutral,
            decimals,
            step,
            suffix: "%",
            scale: ValueScale::Percent,
        };
        let mut value = slider.get(brush);
        if brush_slider(ui, &mut value, spec) {
            brush = slider.with(brush, value);
            actions.push(MaskAction::SetBrush(brush));
        }
    }
    ui.label(
        egui::RichText::new("[ ] size · Shift+[ ] feather · E erase · hold Alt to flip erase")
            .small()
            .color(colors::TEXT_MUTED),
    );
}

/// A brush slider with the same widget and reset button as the others. True when the value
/// changed.
fn brush_slider(ui: &mut egui::Ui, value: &mut f32, spec: AdjustmentSpec<'_>) -> bool {
    let response = widgets::adjustment_slider(ui, value, spec);
    if response.reset_clicked {
        *value = spec.neutral;
    }
    response.reset_clicked || response.response.changed()
}

fn show_components(
    ui: &mut egui::Ui,
    model: &MasksPanelModel<'_>,
    layer: &MaskLayer,
    actions: &mut Vec<MaskAction>,
) {
    for (index, component) in layer.components.iter().enumerate() {
        ui.horizontal(|ui| {
            let selected = model.selected_component == Some(index);
            let label = format!("{}. {}", index + 1, component.shape.label());
            if ui.selectable_label(selected, label).clicked() {
                actions.push(MaskAction::SelectComponent(index));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("×")
                    .on_hover_text("Delete component")
                    .clicked()
                {
                    actions.push(MaskAction::DeleteComponent(index));
                }
                let mut invert = component.invert;
                if ui
                    .checkbox(&mut invert, "Invert")
                    .on_hover_text("Select everything outside this shape")
                    .changed()
                {
                    actions.push(MaskAction::Edit(
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
                        .selected_text(mode.label())
                        .width(84.0)
                        .show_ui(ui, |ui| {
                            for choice in MaskMode::ALL {
                                ui.selectable_value(&mut mode, choice, choice.label());
                            }
                        });
                    if mode != component.mode {
                        actions.push(MaskAction::Edit(
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
    show_range_controls(ui, model, layer, actions);
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        for mode in [MaskMode::Add, MaskMode::Subtract] {
            ui.menu_button(mode.label(), |ui| {
                for (label, shape) in SHAPES {
                    if ui.button(label).clicked() {
                        actions.push(MaskAction::NewComponent(shape, mode));
                        ui.close();
                    }
                }
            });
        }
        // Ranges refine what the other components select, so they intersect by default.
        ui.menu_button("Refine", |ui| {
            for kind in RangeKind::ALL {
                if ui.button(kind.label()).clicked() {
                    actions.push(MaskAction::NewRange {
                        kind,
                        new_layer: false,
                    });
                    ui.close();
                }
            }
        });
        // Back to painting on a brush that is selected (after Done, say).
        let on_brush = model
            .selected_component
            .and_then(|index| layer.components.get(index))
            .is_some_and(|component| matches!(component.shape, MaskShape::Brush(_)));
        if on_brush
            && painting(model.mode).is_none()
            && ui
                .button("Paint")
                .on_hover_text("Paint more strokes into the selected brush")
                .clicked()
        {
            actions.push(MaskAction::PaintSelected);
        }
    });
}

/// The controls of the selected component when it is a luminance or color range.
fn show_range_controls(
    ui: &mut egui::Ui,
    model: &MasksPanelModel<'_>,
    layer: &MaskLayer,
    actions: &mut Vec<MaskAction>,
) {
    let Some(index) = model.selected_component else {
        return;
    };
    let Some(component) = layer.components.get(index) else {
        return;
    };
    let at = ComponentRef {
        layer: layer.id,
        component: index,
    };
    ui.add_space(4.0);
    match &component.shape {
        MaskShape::Luminance(range) => range_controls::show_luminance(ui, at, range, actions),
        MaskShape::Color(range) => range_controls::show_color(
            ui,
            at,
            range,
            model.mode == MaskEditMode::PickColor,
            actions,
        ),
        _ => {}
    }
}

/// Label, unit and precision of a slider, matching the global sliders of the same name.
fn slider_spec(slider: LocalSlider) -> AdjustmentSpec<'static> {
    let range = slider.range();
    let (decimals, suffix, scale) = match slider {
        LocalSlider::Opacity => (0, "%", ValueScale::Percent),
        LocalSlider::Exposure => (2, " EV", ValueScale::Raw),
        LocalSlider::FalseColor => (0, "", ValueScale::Raw),
        _ => (0, "%", ValueScale::OffsetPercent),
    };
    AdjustmentSpec {
        label: slider.label(),
        minimum: range.minimum,
        maximum: range.maximum,
        neutral: range.neutral,
        decimals,
        step: if slider == LocalSlider::FalseColor {
            1.0
        } else {
            0.01
        },
        suffix,
        scale,
    }
}

fn show_sliders(
    ui: &mut egui::Ui,
    layers: &[MaskLayer],
    layer: &MaskLayer,
    actions: &mut Vec<MaskAction>,
) {
    let detail_full = local_detail_is_full(layers, layer);
    for slider in LocalSlider::ALL {
        if slider == LocalSlider::ChromaNoise {
            ui.add_space(4.0);
            ui.label("Detail").on_hover_text(format!(
                "Noise reduction, moiré suppression and capture sharpening inside this mask, as \
                 an offset on the global Detail settings. They use the CPU path and develop the \
                 image again for each different setting, up to \
                 {MAX_LOCAL_DETAIL_VARIANTS} per photo."
            ));
            if detail_full {
                ui.weak(format!(
                    "This photo has {MAX_LOCAL_DETAIL_VARIANTS} different settings already. \
                     Masks with equal settings don't count twice."
                ));
            }
        }
        let spec = slider_spec(slider);
        let mut value = slider.get(layer);
        // The same widget, reset button and drag events as the global sliders.
        let enabled = !(detail_full && slider.is_detail());
        let response = ui
            .add_enabled_ui(enabled, |ui| {
                widgets::adjustment_slider(ui, &mut value, spec)
            })
            .inner;
        if response.reset_clicked {
            value = spec.neutral;
        }
        let widget = &response.response;
        if widget.changed()
            || widget.drag_started()
            || widget.drag_stopped()
            || response.reset_clicked
        {
            actions.push(MaskAction::Slider(
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
