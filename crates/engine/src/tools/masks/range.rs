//! Range components of the Masks tool: luminance and colour ranges.
//!
//! Unlike gradients and brushes they have no handles to drag. A luminance range is added with
//! its default values and shaped with sliders; a colour range gets its samples from the picker,
//! where the frontend measures the colour under the pointer (see
//! `rohditor_core::sample_color_range`) and hands it to [`MaskToolSession::pick_color`].
//! Both are usually refinements of a gradient or brush, so in an existing layer they are added
//! as `Intersect`; in a new layer they are the only component and so `Add`.

use rohditor_edit::{
    COLOR_TOLERANCE_RANGE, ColorRange, ColorSample, EditRecipe, LuminanceRange, MASK_UNIT_RANGE,
    MAX_COLOR_SAMPLES, MaskMode, MaskShape, ParameterRange,
};

use super::edit::{self, Placement};
use super::{MaskEditMode, MaskLimit, MaskToolSession};

/// What a click of the colour picker does to the samples of a colour range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorPick {
    /// The clicked colour becomes the only sample (a plain click).
    Replace,
    /// The clicked colour joins the samples (Shift-click).
    Add,
    /// The sample nearest to the clicked colour is removed (Alt-click).
    RemoveNearest,
}

/// A numeric control of a range component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeSlider {
    LuminanceLow,
    LuminanceHigh,
    LuminanceFeatherLow,
    LuminanceFeatherHigh,
    ColorTolerance,
}

impl RangeSlider {
    pub const fn range(self) -> ParameterRange {
        match self {
            Self::ColorTolerance => COLOR_TOLERANCE_RANGE,
            _ => MASK_UNIT_RANGE,
        }
    }

    /// The slider's value in `shape`, or `None` if `shape` is not the kind this slider is for.
    pub const fn get(self, shape: &MaskShape) -> Option<f32> {
        match (self, shape) {
            (Self::LuminanceLow, MaskShape::Luminance(range)) => Some(range.low),
            (Self::LuminanceHigh, MaskShape::Luminance(range)) => Some(range.high),
            (Self::LuminanceFeatherLow, MaskShape::Luminance(range)) => Some(range.feather_low),
            (Self::LuminanceFeatherHigh, MaskShape::Luminance(range)) => Some(range.feather_high),
            (Self::ColorTolerance, MaskShape::Color(range)) => Some(range.tolerance),
            _ => None,
        }
    }

    /// Sets the value, false if `shape` is the wrong kind or `value` is outside the range.
    /// The two ends of a luminance range push each other along instead of crossing, so a
    /// slider drag never produces an inverted range.
    pub(super) fn set(self, shape: &mut MaskShape, value: f32) -> bool {
        if !self.range().contains(value) {
            return false;
        }
        match (self, shape) {
            (Self::LuminanceLow, MaskShape::Luminance(range)) => {
                range.low = value;
                range.high = range.high.max(value);
            }
            (Self::LuminanceHigh, MaskShape::Luminance(range)) => {
                range.high = value;
                range.low = range.low.min(value);
            }
            (Self::LuminanceFeatherLow, MaskShape::Luminance(range)) => range.feather_low = value,
            (Self::LuminanceFeatherHigh, MaskShape::Luminance(range)) => {
                range.feather_high = value;
            }
            (Self::ColorTolerance, MaskShape::Color(range)) => range.tolerance = value,
            _ => return false,
        }
        true
    }
}

impl MaskToolSession {
    /// Arms the colour picker: the frontend now samples the colour under a click and passes it
    /// to [`pick_color`](Self::pick_color), until the mode is left like any creation mode.
    pub fn start_color_pick(&mut self) {
        self.new_brush_component = false;
        self.mode = MaskEditMode::PickColor;
    }

    /// Whether clicks on the image should be sampled for [`pick_color`](Self::pick_color).
    pub const fn is_picking_color(&self) -> bool {
        matches!(self.mode, MaskEditMode::PickColor)
    }

    /// Adds a luminance range over the whole tonal range to the selected layer (as `Intersect`),
    /// or as a new layer, and selects it. `None` when a recipe limit stops it, see
    /// [`limit_reached`](Self::limit_reached).
    pub fn add_luminance_range(&mut self, recipe: &EditRecipe) -> Option<EditRecipe> {
        self.limit = None;
        self.add_range(recipe, MaskShape::Luminance(LuminanceRange::default()))
    }

    /// Applies one click of the colour picker, with `sample` the colour under the pointer.
    ///
    /// If the selected component is a colour range, the click edits its samples. Otherwise it
    /// adds a new colour range with that one sample (a removal has nothing to remove then).
    /// `None` when nothing changes or a recipe limit stops it.
    pub fn pick_color(
        &mut self,
        recipe: &EditRecipe,
        sample: ColorSample,
        pick: ColorPick,
    ) -> Option<EditRecipe> {
        self.limit = None;
        let selected_color = self
            .selected_shape(recipe)
            .and_then(|(at, shape)| matches!(shape, MaskShape::Color(_)).then_some(at));
        let Some(at) = selected_color else {
            if pick == ColorPick::RemoveNearest {
                return None;
            }
            let range = ColorRange {
                samples: vec![sample],
                ..ColorRange::default()
            };
            return self.add_range(recipe, MaskShape::Color(range));
        };

        let mut next = recipe.clone();
        let MaskShape::Color(range) = edit::shape_mut(&mut next, at)? else {
            return None;
        };
        match pick {
            ColorPick::Replace => range.samples = vec![sample],
            ColorPick::Add => {
                if range.samples.len() >= MAX_COLOR_SAMPLES {
                    self.limit = Some(MaskLimit::ColorSamples);
                    return None;
                }
                range.samples.push(sample);
            }
            ColorPick::RemoveNearest => {
                let nearest = range
                    .samples
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| {
                        distance_squared(a, &sample).total_cmp(&distance_squared(b, &sample))
                    })
                    .map(|(index, _)| index)?;
                range.samples.remove(nearest);
            }
        }
        (next != *recipe).then_some(next)
    }

    /// Appends `shape` as a range component of the selected layer, or of a new layer, and
    /// selects it.
    fn add_range(&mut self, recipe: &EditRecipe, shape: MaskShape) -> Option<EditRecipe> {
        match edit::add_component(recipe, self.selected_layer, shape) {
            Ok((mut next, placed)) => {
                self.set_intersect_in_existing_layer(&mut next, placed);
                self.select(placed);
                Some(next)
            }
            Err(limit) => {
                self.limit = Some(limit);
                None
            }
        }
    }

    /// A range refines what is already selected, so in an existing layer it intersects.
    fn set_intersect_in_existing_layer(&self, recipe: &mut EditRecipe, placed: Placement) {
        if placed.component > 0 {
            edit::set_component_mode(recipe, placed, MaskMode::Intersect);
        }
    }
}

fn distance_squared(a: &ColorSample, b: &ColorSample) -> f32 {
    let (lightness, da, db) = (a.lightness - b.lightness, a.a - b.a, a.b - b.b);
    lightness * lightness + da * da + db * db
}

#[cfg(test)]
mod tests {
    use rohditor_edit::{LinearGradient, MaskPoint};

    use super::*;

    fn gradient() -> MaskShape {
        MaskShape::Linear(LinearGradient {
            start: MaskPoint { x: 0.1, y: 0.1 },
            end: MaskPoint { x: 0.9, y: 0.9 },
        })
    }

    fn sample(lightness: f32, a: f32, b: f32) -> ColorSample {
        ColorSample { lightness, a, b }
    }

    /// A session with one gradient layer selected.
    fn with_gradient() -> (MaskToolSession, EditRecipe) {
        let (recipe, at) = edit::add_component(&EditRecipe::default(), None, gradient())
            .expect("valid test fixture");
        let mut session = MaskToolSession::default();
        session.select(at);
        (session, recipe)
    }

    fn colors(recipe: &EditRecipe) -> &ColorRange {
        match &recipe.local.layers[0]
            .components
            .last()
            .expect("valid test fixture")
            .shape
        {
            MaskShape::Color(range) => range,
            other => panic!("not a colour range: {other:?}"),
        }
    }

    #[test]
    fn a_luminance_range_intersects_in_an_existing_layer_and_adds_in_a_new_one() {
        let (mut session, recipe) = with_gradient();
        let next = session.add_luminance_range(&recipe).expect("room");
        let layer = &next.local.layers[0];
        assert_eq!(layer.components.len(), 2);
        assert_eq!(layer.components[1].mode, MaskMode::Intersect);
        assert!(matches!(layer.components[1].shape, MaskShape::Luminance(_)));
        assert_eq!(session.selected_component, Some(1));
        assert!(next.validate().is_ok());

        // With nothing selected it starts a layer of its own, as `Add`.
        let mut empty = MaskToolSession::default();
        let fresh = empty
            .add_luminance_range(&EditRecipe::default())
            .expect("valid test fixture");
        assert_eq!(fresh.local.layers[0].components[0].mode, MaskMode::Add);
        assert!(fresh.validate().is_ok());
    }

    #[test]
    fn the_picker_creates_a_range_then_replaces_adds_and_removes_samples() {
        let (mut session, recipe) = with_gradient();
        session.start_color_pick();
        assert!(session.is_picking_color());

        // A removal with no colour range selected has nothing to act on.
        assert!(
            session
                .pick_color(&recipe, sample(0.5, 0.0, 0.0), ColorPick::RemoveNearest)
                .is_none()
        );

        let red = sample(0.63, 0.22, 0.13);
        let blue = sample(0.45, -0.03, -0.31);
        let green = sample(0.87, -0.23, 0.18);
        let created = session
            .pick_color(&recipe, red, ColorPick::Replace)
            .expect("valid test fixture");
        assert_eq!(
            created.local.layers[0].components[1].mode,
            MaskMode::Intersect
        );
        assert_eq!(colors(&created).samples, [red]);
        assert!(session.is_picking_color(), "the picker stays armed");

        let both = session
            .pick_color(&created, blue, ColorPick::Add)
            .expect("valid test fixture");
        assert_eq!(colors(&both).samples, [red, blue]);
        let three = session
            .pick_color(&both, green, ColorPick::Add)
            .expect("valid test fixture");
        // Alt-click near blue removes blue, not the others.
        let near_blue = sample(0.46, -0.02, -0.3);
        let removed = session
            .pick_color(&three, near_blue, ColorPick::RemoveNearest)
            .expect("valid test fixture");
        assert_eq!(colors(&removed).samples, [red, green]);
        // A plain click starts over with one sample and keeps the tolerance.
        let mut tuned = removed.clone();
        if let MaskShape::Color(range) = &mut tuned.local.layers[0].components[1].shape {
            range.tolerance = 0.7;
        }
        let replaced = session
            .pick_color(&tuned, blue, ColorPick::Replace)
            .expect("valid test fixture");
        assert_eq!(colors(&replaced).samples, [blue]);
        assert_eq!(colors(&replaced).tolerance, 0.7);
        assert!(replaced.validate().is_ok());

        // Clicking the sample that is the only one again changes nothing.
        assert!(
            session
                .pick_color(&replaced, blue, ColorPick::Replace)
                .is_none()
        );
    }

    #[test]
    fn the_eighth_sample_is_the_last_and_the_limit_is_reported() {
        let (mut session, mut recipe) = with_gradient();
        recipe = session
            .pick_color(&recipe, sample(0.5, 0.1, 0.1), ColorPick::Replace)
            .expect("valid test fixture");
        for i in 1..MAX_COLOR_SAMPLES {
            recipe = session
                .pick_color(&recipe, sample(0.5, 0.1, i as f32 * 0.01), ColorPick::Add)
                .expect("valid test fixture");
        }
        assert_eq!(colors(&recipe).samples.len(), MAX_COLOR_SAMPLES);
        assert!(
            session
                .pick_color(&recipe, sample(0.2, 0.2, 0.2), ColorPick::Add)
                .is_none()
        );
        assert_eq!(session.limit_reached(), Some(MaskLimit::ColorSamples));
        assert!(recipe.validate().is_ok());
    }

    #[test]
    fn luminance_ends_push_each_other_and_wrong_shapes_are_refused() {
        let mut shape = MaskShape::Luminance(LuminanceRange {
            low: 0.3,
            high: 0.6,
            feather_low: 0.1,
            feather_high: 0.1,
        });
        assert!(RangeSlider::LuminanceLow.set(&mut shape, 0.8));
        assert_eq!(RangeSlider::LuminanceLow.get(&shape), Some(0.8));
        assert_eq!(RangeSlider::LuminanceHigh.get(&shape), Some(0.8));
        assert!(RangeSlider::LuminanceHigh.set(&mut shape, 0.2));
        assert_eq!(RangeSlider::LuminanceLow.get(&shape), Some(0.2));
        assert!(!RangeSlider::LuminanceLow.set(&mut shape, 1.5));
        assert!(!RangeSlider::ColorTolerance.set(&mut shape, 0.5));
        assert_eq!(RangeSlider::ColorTolerance.get(&shape), None);
        assert!(RangeSlider::LuminanceFeatherHigh.set(&mut shape, 0.0));

        let mut colour = MaskShape::Color(ColorRange::default());
        assert!(RangeSlider::ColorTolerance.set(&mut colour, 0.9));
        assert!(!RangeSlider::LuminanceLow.set(&mut colour, 0.5));
    }
}
