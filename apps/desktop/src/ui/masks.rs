//! The Masks tool's panel: the layer list, the selected layer's components and its sliders.
//!
//! Presentation only. It shows a [`MasksPanelModel`] and reports what the user asked for as
//! [`MasksAction`]s; `app/masks.rs` turns them into recipe edits through the engine's
//! `MaskToolSession` and `LayerEdit`.

use eframe::egui;
use rohditor_edit::{
    MASK_UNIT_RANGE, MAX_LOCAL_DETAIL_VARIANTS, ParameterRange, local_detail_count,
};
use rohditor_edit::{MaskLayer, MaskMode, MaskShape};
use rohditor_engine::tools::{
    BRUSH_RADIUS_RANGE, BrushSettings, LayerEdit, LocalSlider, MaskEditMode, MaskLimit, RangeSlider,
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

/// How a slider event relates to a drag, so the caller can make a whole drag one undo step.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SliderPhase {
    pub changed: bool,
    pub drag_started: bool,
    pub dragged: bool,
    pub drag_stopped: bool,
    pub reset: bool,
}

/// The two ways to select by what the image looks like instead of where things are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RangeKind {
    Luminance,
    Color,
}

impl RangeKind {
    const ALL: [Self; 2] = [Self::Luminance, Self::Color];

    const fn label(self) -> &'static str {
        match self {
            Self::Luminance => "Luminance range",
            Self::Color => "Color range",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum MasksAction {
    SelectLayer(u32),
    SelectComponent(usize),
    /// Start drawing a gradient (or painting a brush) into a new layer.
    NewLayer(MaskEditMode),
    /// Start drawing a gradient (or painting a brush) into the selected layer.
    NewComponent(MaskEditMode, MaskMode),
    /// Paint more strokes into the selected brush component.
    PaintSelected,
    /// Add a luminance or color range, as a new mask or as a component of the selected one.
    /// A color range starts by picking its colors on the image.
    NewRange {
        kind: RangeKind,
        new_layer: bool,
    },
    /// Pick the colors of the selected color range on the image.
    PickColor,
    /// A slider event of a range component (the luminance bar or the color tolerance).
    RangeSlider {
        layer: u32,
        component: usize,
        slider: RangeSlider,
        value: f32,
        phase: SliderPhase,
    },
    CancelCreate,
    /// New brush settings, from the brush sliders.
    SetBrush(BrushSettings),
    /// Paint (`false`) or erase (`true`) with the brush.
    SetErase(bool),
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
                    actions.push(MasksAction::NewLayer(shape));
                    ui.close();
                }
            }
            ui.separator();
            for kind in RangeKind::ALL {
                if ui.button(kind.label()).clicked() {
                    actions.push(MasksAction::NewRange {
                        kind,
                        new_layer: true,
                    });
                    ui.close();
                }
            }
        });
        if model.mode != MaskEditMode::Select && ui.button(label).clicked() {
            actions.push(MasksAction::CancelCreate);
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
fn show_brush(ui: &mut egui::Ui, model: &MasksPanelModel<'_>, actions: &mut Vec<MasksAction>) {
    let mut erase = painting(model.mode).unwrap_or(false);
    if ui
        .checkbox(&mut erase, "Erase")
        .on_hover_text("Remove from the mask instead of adding to it (E). Hold Alt to flip it.")
        .changed()
    {
        actions.push(MasksAction::SetErase(erase));
    }

    let mut brush = model.brush;
    let spec = |label, range: ParameterRange, decimals, step| AdjustmentSpec {
        label,
        minimum: range.minimum,
        maximum: range.maximum,
        neutral: range.neutral,
        decimals,
        step,
        suffix: "%",
        scale: ValueScale::Percent,
    };
    // Size is the radius, as a percentage of the image's long edge. The slider snaps its value
    // to the step on every frame, and the `[` and `]` keys change it by a factor: with a coarser
    // step than this, a factor-1.15 increase at the smallest sizes would snap back and the
    // brush could not grow.
    let mut changed = brush_slider(
        ui,
        &mut brush.radius,
        spec("Size", BRUSH_RADIUS_RANGE, 1, 0.0005),
    );
    changed |= brush_slider(
        ui,
        &mut brush.feather,
        spec("Feather", MASK_UNIT_RANGE, 0, 0.01),
    );
    let flow = ParameterRange {
        neutral: 1.0,
        ..MASK_UNIT_RANGE
    };
    changed |= brush_slider(ui, &mut brush.flow, spec("Flow", flow, 0, 0.01));
    if changed {
        actions.push(MasksAction::SetBrush(brush));
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

const fn shape_name(shape: &MaskShape) -> &'static str {
    match shape {
        MaskShape::Linear(_) => "Linear gradient",
        MaskShape::Radial(_) => "Radial gradient",
        MaskShape::Brush(_) => "Brush",
        MaskShape::Luminance(_) => "Luminance range",
        MaskShape::Color(_) => "Color range",
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
    show_range_controls(ui, model, layer, actions);
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
        // Ranges refine what the other components select, so they intersect by default.
        ui.menu_button("Refine", |ui| {
            for kind in RangeKind::ALL {
                if ui.button(kind.label()).clicked() {
                    actions.push(MasksAction::NewRange {
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
            actions.push(MasksAction::PaintSelected);
        }
    });
}

/// The controls of the selected component when it is a luminance or color range.
fn show_range_controls(
    ui: &mut egui::Ui,
    model: &MasksPanelModel<'_>,
    layer: &MaskLayer,
    actions: &mut Vec<MasksAction>,
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
        LocalSlider::ChromaNoise => ("Colour noise", 0, "%", ValueScale::OffsetPercent),
        LocalSlider::LuminanceNoise => ("Luminance noise", 0, "%", ValueScale::OffsetPercent),
        LocalSlider::FalseColor => ("Moiré", 0, "", ValueScale::Raw),
        LocalSlider::Sharpening => ("Sharpening", 0, "%", ValueScale::OffsetPercent),
    };
    AdjustmentSpec {
        label,
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
    actions: &mut Vec<MasksAction>,
) {
    // A layer that already has detail settings may keep changing them; another one would need
    // a further copy of the image.
    let detail_full = layer.settings.detail.is_neutral()
        && local_detail_count(layers) >= MAX_LOCAL_DETAIL_VARIANTS;
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
