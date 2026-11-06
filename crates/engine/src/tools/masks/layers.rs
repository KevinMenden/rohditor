//! Layer-level operations of the Masks tool: what the layer list and the layer's controls do.
//!
//! These are the edits that are not pointer gestures: selecting, deleting, reordering, renaming,
//! the sliders of a layer, and how a component combines. Edits are pure over `EditRecipe`
//! ([`LayerEdit`]); the ones that also move the selection are methods of [`MaskToolSession`].
//! Like the gestures, every recipe returned passes `EditRecipe::validate`.

use rohditor_edit::{
    BLACKS_RANGE, CONTRAST_RANGE, EXPOSURE_EV_RANGE, EditRecipe, HIGHLIGHTS_RANGE,
    LOCAL_FALSE_COLOR_RANGE, LOCAL_NOISE_REDUCTION_RANGE, LOCAL_SATURATION_RANGE,
    LOCAL_SHARPENING_RANGE, LOCAL_TEMPERATURE_RANGE, LOCAL_TINT_RANGE, MASK_OPACITY_RANGE,
    MAX_MASK_NAME_BYTES, MaskLayer, MaskMode, ParameterRange, SHADOWS_RANGE, WHITES_RANGE,
};

use super::range::RangeSlider;
use super::{MaskEditMode, MaskToolSession};

/// A numeric control of a mask layer: its opacity or one of its adjustment sliders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalSlider {
    Opacity,
    Exposure,
    Temperature,
    Tint,
    Contrast,
    Highlights,
    Shadows,
    Whites,
    Blacks,
    Saturation,
    ChromaNoise,
    LuminanceNoise,
    FalseColor,
    Sharpening,
}

impl LocalSlider {
    /// In the order the panel shows them.
    pub const ALL: [Self; 14] = [
        Self::Opacity,
        Self::Exposure,
        Self::Temperature,
        Self::Tint,
        Self::Contrast,
        Self::Highlights,
        Self::Shadows,
        Self::Whites,
        Self::Blacks,
        Self::Saturation,
        Self::ChromaNoise,
        Self::LuminanceNoise,
        Self::FalseColor,
        Self::Sharpening,
    ];

    /// Whether this slider makes the pipeline develop another copy of the image, which a recipe
    /// has a limit of.
    #[must_use]
    pub const fn is_detail(self) -> bool {
        matches!(
            self,
            Self::ChromaNoise | Self::LuminanceNoise | Self::FalseColor | Self::Sharpening
        )
    }

    pub const fn range(self) -> ParameterRange {
        match self {
            Self::Opacity => MASK_OPACITY_RANGE,
            Self::Exposure => EXPOSURE_EV_RANGE,
            Self::Temperature => LOCAL_TEMPERATURE_RANGE,
            Self::Tint => LOCAL_TINT_RANGE,
            Self::Contrast => CONTRAST_RANGE,
            Self::Highlights => HIGHLIGHTS_RANGE,
            Self::Shadows => SHADOWS_RANGE,
            Self::Whites => WHITES_RANGE,
            Self::Blacks => BLACKS_RANGE,
            Self::Saturation => LOCAL_SATURATION_RANGE,
            Self::ChromaNoise | Self::LuminanceNoise => LOCAL_NOISE_REDUCTION_RANGE,
            Self::FalseColor => LOCAL_FALSE_COLOR_RANGE,
            Self::Sharpening => LOCAL_SHARPENING_RANGE,
        }
    }

    pub const fn get(self, layer: &MaskLayer) -> f32 {
        let settings = &layer.settings;
        match self {
            Self::Opacity => layer.opacity,
            Self::Exposure => settings.exposure_ev,
            Self::Temperature => settings.temperature,
            Self::Tint => settings.tint,
            Self::Contrast => settings.contrast,
            Self::Highlights => settings.highlights,
            Self::Shadows => settings.shadows,
            Self::Whites => settings.whites,
            Self::Blacks => settings.blacks,
            Self::Saturation => settings.saturation,
            Self::ChromaNoise => settings.detail.chroma_noise,
            Self::LuminanceNoise => settings.detail.luminance_noise,
            Self::FalseColor => settings.detail.false_color_steps as f32,
            Self::Sharpening => settings.detail.sharpening,
        }
    }

    fn set(self, layer: &mut MaskLayer, value: f32) {
        let settings = &mut layer.settings;
        match self {
            Self::Opacity => layer.opacity = value,
            Self::Exposure => settings.exposure_ev = value,
            Self::Temperature => settings.temperature = value,
            Self::Tint => settings.tint = value,
            Self::Contrast => settings.contrast = value,
            Self::Highlights => settings.highlights = value,
            Self::Shadows => settings.shadows = value,
            Self::Whites => settings.whites = value,
            Self::Blacks => settings.blacks = value,
            Self::Saturation => settings.saturation = value,
            Self::ChromaNoise => settings.detail.chroma_noise = value,
            Self::LuminanceNoise => settings.detail.luminance_noise = value,
            // Whole passes; `range` has checked that this fits.
            Self::FalseColor => settings.detail.false_color_steps = value.round() as i8,
            Self::Sharpening => settings.detail.sharpening = value,
        }
    }
}

/// A change to one layer that does not touch the selection.
#[derive(Debug, Clone, PartialEq)]
pub enum LayerEdit {
    /// Cut to the length the recipe accepts.
    Rename(String),
    Enabled(bool),
    Slider(LocalSlider, f32),
    /// How a component combines with the ones before it. The first component stays `Add`.
    ComponentMode {
        component: usize,
        mode: MaskMode,
    },
    ComponentInvert {
        component: usize,
        invert: bool,
    },
    /// A slider of a luminance or colour range component; ignored for other shapes.
    RangeSlider {
        component: usize,
        slider: RangeSlider,
        value: f32,
    },
    /// One step earlier in the order the layers are applied.
    MoveUp,
    /// One step later in the order the layers are applied.
    MoveDown,
}

impl LayerEdit {
    /// `recipe` with this edit applied to layer `layer`, or `None` when that changes nothing
    /// (an unknown layer or component, a value the recipe would not accept, the first or last
    /// layer moved past the end).
    #[must_use]
    pub fn apply(&self, recipe: &EditRecipe, layer: u32) -> Option<EditRecipe> {
        let mut next = recipe.clone();
        let index = next.local.layers.iter().position(|l| l.id == layer)?;
        match self {
            Self::Rename(name) => next.local.layers[index].name = truncated(name),
            Self::Enabled(enabled) => next.local.layers[index].enabled = *enabled,
            Self::Slider(slider, value) => {
                if !slider.range().contains(*value) {
                    return None;
                }
                slider.set(&mut next.local.layers[index], *value);
                if slider.is_detail() && next.local.validate().is_err() {
                    return None;
                }
            }
            Self::ComponentMode { component, mode } => {
                if *component == 0 && *mode != MaskMode::Add {
                    return None;
                }
                next.local.layers[index]
                    .components
                    .get_mut(*component)?
                    .mode = *mode;
            }
            Self::ComponentInvert { component, invert } => {
                next.local.layers[index]
                    .components
                    .get_mut(*component)?
                    .invert = *invert;
            }
            Self::RangeSlider {
                component,
                slider,
                value,
            } => {
                let shape = &mut next.local.layers[index]
                    .components
                    .get_mut(*component)?
                    .shape;
                if !slider.set(shape, *value) {
                    return None;
                }
            }
            Self::MoveUp => {
                let to = index.checked_sub(1)?;
                next.local.move_layer(layer, to);
            }
            Self::MoveDown => {
                if index + 1 >= next.local.layers.len() {
                    return None;
                }
                next.local.move_layer(layer, index + 1);
            }
        }
        (next != *recipe).then_some(next)
    }
}

/// `name` cut to the recipe's byte limit at a character boundary.
fn truncated(name: &str) -> String {
    let mut end = name.len().min(MAX_MASK_NAME_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_owned()
}

impl MaskToolSession {
    /// Selects a layer and its first component (the handles shown). A press now selects, so
    /// a pending creation mode ends.
    pub fn select_layer(&mut self, recipe: &EditRecipe, layer: u32) {
        let Some(found) = recipe.local.layer(layer) else {
            return;
        };
        self.selected_layer = Some(layer);
        self.selected_component = (!found.components.is_empty()).then_some(0);
        self.return_to_select();
    }

    /// Selects a component of the selected layer, so its handles are shown.
    pub fn select_component(&mut self, recipe: &EditRecipe, component: usize) {
        let in_range = self
            .selected_layer
            .and_then(|id| recipe.local.layer(id))
            .is_some_and(|layer| component < layer.components.len());
        if in_range {
            self.selected_component = Some(component);
            self.return_to_select();
        }
    }

    /// The next press-drag-release draws a gradient into a new layer (`shape` is
    /// `CreateLinear` or `CreateRadial`), or the next stroke starts a brush there (`Paint`).
    pub fn create_layer(&mut self, shape: MaskEditMode) {
        self.selected_layer = None;
        self.selected_component = None;
        self.component_mode = MaskMode::Add;
        self.new_brush_component = false;
        self.mode = shape;
    }

    /// The next press-drag-release draws a gradient into the selected layer, combined as
    /// `component_mode`; with `Paint`, the next stroke starts a new brush component there, and
    /// the strokes after it join that one. Without a selected layer it starts a new one.
    pub fn create_component(&mut self, shape: MaskEditMode, component_mode: MaskMode) {
        self.component_mode = component_mode;
        self.new_brush_component = matches!(shape, MaskEditMode::Paint { .. });
        self.mode = shape;
    }

    /// Leaves a creation mode. False when there was none, or while a gesture is running: the
    /// gesture owns the mode until its release.
    pub fn cancel_create(&mut self) -> bool {
        let creating = self.mode != MaskEditMode::Select && !self.is_dragging();
        if creating {
            self.return_to_select();
        }
        creating
    }

    /// Deletes layer `layer` and drops the selection if it pointed there.
    #[must_use]
    pub fn delete_layer(&mut self, recipe: &EditRecipe, layer: u32) -> Option<EditRecipe> {
        let mut next = recipe.clone();
        next.local.remove_layer(layer)?;
        if self.selected_layer == Some(layer) {
            self.selected_layer = None;
            self.selected_component = None;
        }
        Some(next)
    }

    /// Deletes a component; the layer's last one takes the layer with it. The component that
    /// becomes first is made `Add`, because the recipe requires it.
    #[must_use]
    pub fn delete_component(
        &mut self,
        recipe: &EditRecipe,
        layer: u32,
        component: usize,
    ) -> Option<EditRecipe> {
        let count = recipe.local.layer(layer)?.components.len();
        if component >= count {
            return None;
        }
        if count == 1 {
            return self.delete_layer(recipe, layer);
        }
        let mut next = recipe.clone();
        let components = &mut next.local.layer_mut(layer)?.components;
        components.remove(component);
        components[0].mode = MaskMode::Add;
        if self.selected_layer == Some(layer) {
            let last = components.len() - 1;
            self.selected_component = self.selected_component.map(|selected| match selected {
                selected if selected > component => selected - 1,
                // The deleted component was selected: its neighbour takes over.
                selected if selected == component => component.min(last),
                selected => selected,
            });
        }
        Some(next)
    }

    /// The Delete key: the selected component, or the selected layer when no component is
    /// selected or it is the last one. Nothing while a gesture is running.
    #[must_use]
    pub fn delete_selected(&mut self, recipe: &EditRecipe) -> Option<EditRecipe> {
        // Not under a running gesture: its release would write to what was deleted.
        if self.is_dragging() {
            return None;
        }
        let layer = self.selected_layer?;
        match self.selected_component {
            Some(component) => self.delete_component(recipe, layer, component),
            None => self.delete_layer(recipe, layer),
        }
    }
}

#[cfg(test)]
mod tests {
    use rohditor_edit::{LinearGradient, MaskPoint, MaskShape};

    use super::super::edit::add_component;
    use super::*;

    fn gradient() -> MaskShape {
        MaskShape::Linear(LinearGradient {
            start: MaskPoint { x: 0.1, y: 0.1 },
            end: MaskPoint { x: 0.9, y: 0.9 },
        })
    }

    fn layer(recipe: &EditRecipe) -> &MaskLayer {
        recipe.local.layer(0).expect("layer 0 exists")
    }

    /// Layer 0 with `components` gradients, then layer 1 with one.
    fn recipe(components: usize) -> EditRecipe {
        let mut recipe = EditRecipe::default();
        for _ in 0..components {
            recipe = add_component(
                &recipe,
                recipe.local.layers.first().map(|l| l.id),
                gradient(),
            )
            .expect("room")
            .0;
        }
        add_component(&recipe, None, gradient()).expect("room").0
    }

    #[test]
    fn range_sliders_edit_the_addressed_range_and_refuse_anything_else() {
        let mut base = recipe(1);
        base.local.layers[0]
            .components
            .push(rohditor_edit::MaskComponent {
                mode: MaskMode::Intersect,
                invert: false,
                shape: MaskShape::Luminance(rohditor_edit::LuminanceRange::default()),
            });
        let edit = |component, slider, value| LayerEdit::RangeSlider {
            component,
            slider,
            value,
        };

        let moved = edit(1, RangeSlider::LuminanceHigh, 0.7)
            .apply(&base, 0)
            .expect("changes the range");
        assert_eq!(
            RangeSlider::LuminanceHigh.get(&layer(&moved).components[1].shape),
            Some(0.7)
        );
        assert!(moved.validate().is_ok());
        // Same value, a value outside the slider, the gradient, a missing component and a
        // missing layer: nothing to install.
        assert!(
            edit(1, RangeSlider::LuminanceHigh, 1.0)
                .apply(&base, 0)
                .is_none()
        );
        assert!(
            edit(1, RangeSlider::LuminanceHigh, 1.5)
                .apply(&base, 0)
                .is_none()
        );
        assert!(
            edit(0, RangeSlider::LuminanceHigh, 0.5)
                .apply(&base, 0)
                .is_none()
        );
        assert!(
            edit(1, RangeSlider::ColorTolerance, 0.5)
                .apply(&base, 0)
                .is_none()
        );
        assert!(
            edit(7, RangeSlider::LuminanceHigh, 0.5)
                .apply(&base, 0)
                .is_none()
        );
        assert!(
            edit(1, RangeSlider::LuminanceHigh, 0.5)
                .apply(&base, 9)
                .is_none()
        );
    }

    #[test]
    fn detail_sliders_stop_at_the_recipes_limit_of_different_settings() {
        use rohditor_edit::MAX_LOCAL_DETAIL_VARIANTS;

        let mut next = EditRecipe::default();
        for _ in 0..=MAX_LOCAL_DETAIL_VARIANTS {
            next = add_component(&next, None, gradient()).expect("room").0;
        }
        let set = |recipe: &EditRecipe, layer, value| {
            LayerEdit::Slider(LocalSlider::LuminanceNoise, value).apply(recipe, layer)
        };
        for layer in 0..MAX_LOCAL_DETAIL_VARIANTS {
            next = set(&next, layer as u32, 0.1 + layer as f32 * 0.1).expect("under the limit");
        }
        let over = MAX_LOCAL_DETAIL_VARIANTS as u32;
        assert!(set(&next, over, 0.9).is_none());
        // A value another layer already has needs no further copy.
        let shared = set(&next, over, 0.1).expect("shares a copy");
        assert!(shared.validate().is_ok());
        // Neither does changing a layer that already has its own copy.
        assert!(set(&next, 0, 0.9).is_some());
        // Other sliders of the layer over the limit keep working.
        assert!(
            LayerEdit::Slider(LocalSlider::Contrast, 0.5)
                .apply(&next, over)
                .is_some()
        );
    }

    #[test]
    fn the_moire_slider_stores_whole_median_passes() {
        let next = LayerEdit::Slider(LocalSlider::FalseColor, -2.4)
            .apply(&recipe(1), 0)
            .expect("in range");
        assert_eq!(layer(&next).settings.detail.false_color_steps, -2);
        assert_eq!(LocalSlider::FalseColor.get(layer(&next)), -2.0);
    }

    #[test]
    fn sliders_set_validated_values_and_refuse_out_of_range_ones() {
        let base = recipe(1);
        for slider in LocalSlider::ALL {
            let range = slider.range();
            let target = if range.neutral < range.maximum {
                range.maximum
            } else {
                range.minimum
            };
            let next = LayerEdit::Slider(slider, target)
                .apply(&base, 0)
                .expect("a value in range");
            assert!(next.validate().is_ok());
            assert_eq!(slider.get(layer(&next)), target);
            assert!(
                LayerEdit::Slider(slider, range.maximum + 1.0)
                    .apply(&base, 0)
                    .is_none()
            );
        }
        // Setting the value a layer already has changes nothing.
        assert!(
            LayerEdit::Slider(LocalSlider::Opacity, 1.0)
                .apply(&base, 0)
                .is_none()
        );
    }

    #[test]
    fn names_are_cut_at_a_character_boundary_within_the_limit() {
        let long = "é".repeat(MAX_MASK_NAME_BYTES);
        let next = LayerEdit::Rename(long)
            .apply(&recipe(1), 0)
            .expect("renamed");
        assert!(next.validate().is_ok());
        assert_eq!(layer(&next).name.len(), MAX_MASK_NAME_BYTES);
    }

    #[test]
    fn the_first_component_cannot_leave_add() {
        let base = recipe(2);
        let subtract = |component| LayerEdit::ComponentMode {
            component,
            mode: MaskMode::Subtract,
        };
        assert!(subtract(0).apply(&base, 0).is_none());
        let next = subtract(1).apply(&base, 0).expect("second component");
        assert!(next.validate().is_ok());
        assert_eq!(layer(&next).components[1].mode, MaskMode::Subtract);
        let inverted = LayerEdit::ComponentInvert {
            component: 0,
            invert: true,
        }
        .apply(&base, 0)
        .expect("invert");
        assert!(layer(&inverted).components[0].invert);
    }

    #[test]
    fn layers_move_one_step_and_stop_at_the_ends() {
        let base = recipe(1);
        assert!(LayerEdit::MoveUp.apply(&base, 0).is_none());
        assert!(LayerEdit::MoveDown.apply(&base, 1).is_none());
        let moved = LayerEdit::MoveDown.apply(&base, 0).expect("down");
        assert_eq!(
            moved.local.layers.iter().map(|l| l.id).collect::<Vec<_>>(),
            [1, 0]
        );
        let back = LayerEdit::MoveUp.apply(&moved, 0).expect("up");
        assert_eq!(back, base);
    }

    #[test]
    fn deleting_the_last_component_deletes_the_layer_and_the_selection() {
        let base = recipe(1);
        let mut session = MaskToolSession::default();
        session.select_layer(&base, 1);
        assert_eq!(session.selected_component, Some(0));
        let next = session.delete_selected(&base).expect("deleted");
        assert_eq!(next.local.layers.len(), 1);
        assert_eq!(session.selected_layer, None);
        assert_eq!(session.selected_component, None);
        assert!(next.validate().is_ok());
    }

    #[test]
    fn deleting_a_component_keeps_the_layer_valid_and_selects_a_neighbour() {
        let mut base = recipe(3);
        base = LayerEdit::ComponentMode {
            component: 1,
            mode: MaskMode::Subtract,
        }
        .apply(&base, 0)
        .expect("subtract");
        let mut session = MaskToolSession::default();
        session.select_layer(&base, 0);

        // Deleting the first component makes the next one first, and that one must be Add.
        let next = session.delete_component(&base, 0, 0).expect("deleted");
        assert!(next.validate().is_ok());
        assert_eq!(layer(&next).components.len(), 2);
        assert_eq!(layer(&next).components[0].mode, MaskMode::Add);
        assert_eq!(session.selected_component, Some(0));

        // Deleting the selected last component moves the selection up.
        session.select_component(&next, 1);
        let next = session.delete_component(&next, 0, 1).expect("deleted");
        assert_eq!(session.selected_component, Some(0));
        assert_eq!(layer(&next).components.len(), 1);
    }

    #[test]
    fn deleting_a_component_before_the_selected_one_keeps_it_selected() {
        let base = recipe(3);
        let mut session = MaskToolSession::default();
        session.select_layer(&base, 0);
        session.select_component(&base, 2);
        let next = session.delete_component(&base, 0, 0).expect("deleted");
        assert_eq!(session.selected_component, Some(1));
        assert_eq!(layer(&next).components.len(), 2);
    }

    #[test]
    fn creation_modes_set_up_the_next_gradient_and_selecting_ends_them() {
        let base = recipe(1);
        let mut session = MaskToolSession::default();
        session.select_layer(&base, 0);
        session.create_component(MaskEditMode::CreateRadial, MaskMode::Subtract);
        assert_eq!(session.selected_layer, Some(0));
        assert_eq!(session.component_mode, MaskMode::Subtract);
        session.create_layer(MaskEditMode::CreateLinear);
        assert_eq!(session.selected_layer, None);
        assert_eq!(session.component_mode, MaskMode::Add);
        assert!(session.cancel_create());
        assert!(!session.cancel_create());
        session.create_layer(MaskEditMode::CreateLinear);
        session.select_layer(&base, 1);
        assert_eq!(session.mode, MaskEditMode::Select);
    }
}
