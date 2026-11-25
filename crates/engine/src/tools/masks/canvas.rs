//! What a frontend shows of the masks on the image, and what a press on it would do.
//!
//! Everything is in display positions (`0..1` over the image on screen); a frontend maps them to
//! its screen and draws.

use rohditor_edit::{EditRecipe, MaskLayer, MaskPoint, MaskShape};

use super::{HandleMarker, MaskEditMode, MaskHandle, MaskToolSession, MaskView};

/// How close, in screen pixels, a click must be to a mask's pin to select it.
const PIN_HIT_RADIUS_PX: f64 = 10.0;

/// Where a mask that is not selected is, for a click to select it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MaskPin {
    pub layer: u32,
    /// Whether the mask is applied.
    pub enabled: bool,
    pub position: (f64, f64),
}

/// The gradient whose handles are shown.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MaskOutline {
    Linear {
        start: (f64, f64),
        center: (f64, f64),
        end: (f64, f64),
    },
    /// An ellipse around `center` through the ends of its half axes.
    Radial {
        center: (f64, f64),
        x: (f64, f64),
        y: (f64, f64),
        /// The handle of the feather.
        feather: (f64, f64),
        /// Scale of the ellipse inside which the mask has its full strength.
        inner: f64,
    },
}

impl MaskToolSession {
    /// The outline of the selected component; `None` without one, and unless the mode is
    /// `Select`, as for [`Self::handle_markers`].
    pub fn outline(&self, recipe: &EditRecipe, view: &MaskView) -> Option<MaskOutline> {
        let markers = self.handle_markers(recipe, view);
        let at = |wanted: MaskHandle| {
            markers
                .iter()
                .find(|HandleMarker { handle, .. }| *handle == wanted)
                .map(|marker| marker.position)
        };
        match self.selected_shape(recipe)?.1 {
            MaskShape::Linear(_) => Some(MaskOutline::Linear {
                start: at(MaskHandle::LinearStart)?,
                center: at(MaskHandle::LinearCenter)?,
                end: at(MaskHandle::LinearEnd)?,
            }),
            MaskShape::Radial(radial) => Some(MaskOutline::Radial {
                center: at(MaskHandle::RadialCenter)?,
                x: at(MaskHandle::RadialRadiusX)?,
                y: at(MaskHandle::RadialRadiusY)?,
                feather: at(MaskHandle::RadialFeather)?,
                inner: 1.0 - f64::from(radial.feather),
            }),
            MaskShape::Brush(_) | MaskShape::Luminance(_) | MaskShape::Color(_) => None,
        }
    }

    /// The pins of the masks that are not selected.
    pub fn pins(&self, recipe: &EditRecipe, view: &MaskView) -> Vec<MaskPin> {
        recipe
            .local
            .layers
            .iter()
            .filter(|layer| self.selected_layer != Some(layer.id))
            .filter_map(|layer| {
                let anchor = layer_anchor(layer)?;
                Some(MaskPin {
                    layer: layer.id,
                    enabled: layer.enabled,
                    position: view.space_to_display(view.space().point_to_space(anchor)),
                })
            })
            .collect()
    }

    /// The mask whose pin a click at `position` selects. None while painting or picking a color,
    /// which take every click.
    pub fn pin_at(
        &self,
        recipe: &EditRecipe,
        view: &MaskView,
        position: (f64, f64),
    ) -> Option<u32> {
        if self.is_painting() || self.is_picking_color() {
            return None;
        }
        self.pins(recipe, view)
            .into_iter()
            .find(|pin| view.screen_distance(pin.position, position) <= PIN_HIT_RADIUS_PX)
            .map(|pin| pin.layer)
    }

    /// Whether a press at `position` would start a gesture, so that the image must not pan with
    /// it. Drawing a gradient and painting take any press, otherwise only a handle of the
    /// selection does. Picking a color is a click, not a drag: the image still pans.
    pub fn grabs(&self, recipe: &EditRecipe, view: &MaskView, position: (f64, f64)) -> bool {
        match self.mode {
            MaskEditMode::Select => self.hit_test(recipe, view, position).is_some(),
            MaskEditMode::CreateLinear
            | MaskEditMode::CreateRadial
            | MaskEditMode::Paint { .. } => true,
            MaskEditMode::PickColor => false,
        }
    }
}

fn layer_anchor(layer: &MaskLayer) -> Option<MaskPoint> {
    match &layer.components.first()?.shape {
        MaskShape::Linear(line) => Some(MaskPoint {
            x: (line.start.x + line.end.x) / 2.0,
            y: (line.start.y + line.end.y) / 2.0,
        }),
        MaskShape::Radial(radial) => Some(radial.center),
        MaskShape::Brush(brush) => brush
            .strokes
            .iter()
            .find_map(|stroke| stroke.points.first())
            .copied(),
        // A range has no position to pin.
        MaskShape::Luminance(_) | MaskShape::Color(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use rohditor_core::OutputGeometry;
    use rohditor_edit::{LinearGradient, LuminanceRange, RadialGradient};
    use rohditor_image::Orientation;

    use super::super::edit::add_component;
    use super::*;

    /// A 600x400 photo turned by 90 degrees, shown at half size.
    fn view() -> MaskView {
        let geometry = OutputGeometry::new(600, 400, Orientation::Rotate90, None).expect("valid");
        MaskView::new(geometry, 200.0)
    }

    fn point(x: f32, y: f32) -> MaskPoint {
        MaskPoint { x, y }
    }

    /// Layer 0 is a linear gradient, layer 1 a radial one and layer 2 a range.
    fn recipe() -> EditRecipe {
        let shapes = [
            MaskShape::Linear(LinearGradient {
                start: point(0.1, 0.2),
                end: point(0.5, 0.2),
            }),
            MaskShape::Radial(RadialGradient {
                center: point(0.75, 0.5),
                radius_x: 0.2,
                radius_y: 0.1,
                angle_degrees: 0.0,
                feather: 0.25,
            }),
            MaskShape::Luminance(LuminanceRange::default()),
        ];
        shapes
            .into_iter()
            .fold(EditRecipe::default(), |recipe, shape| {
                add_component(&recipe, None, shape).expect("room").0
            })
    }

    fn selecting(layer: u32) -> MaskToolSession {
        let mut session = MaskToolSession::default();
        session.select_layer(&recipe(), layer);
        session
    }

    #[track_caller]
    fn assert_at(position: (f64, f64), expected: (f64, f64)) {
        let distance = (position.0 - expected.0).hypot(position.1 - expected.1);
        assert!(distance < 1e-6, "{position:?} is not {expected:?}");
    }

    #[test]
    fn the_screen_scale_is_the_shown_size_per_output_pixel() {
        assert_eq!(view().screen_scale, 0.5);
    }

    #[test]
    fn other_masks_have_pins_and_a_click_near_one_selects_it() {
        let (recipe, view) = (recipe(), view());
        let pins = selecting(0).pins(&recipe, &view);
        assert_eq!(pins.len(), 1, "not the selected mask and not the range");
        assert_eq!(pins[0].layer, 1);
        // Developed (0.75, 0.5) after turning clockwise.
        assert_at(pins[0].position, (0.5, 0.75));

        let session = selecting(0);
        // The image is 200x300 on screen: 0.04 are 8 and 12 pixels.
        assert_eq!(session.pin_at(&recipe, &view, (0.54, 0.75)), Some(1));
        assert_eq!(session.pin_at(&recipe, &view, (0.5, 0.79)), None);
        let mut painting = session.clone();
        painting.mode = MaskEditMode::Paint { erase: false };
        assert_eq!(painting.pin_at(&recipe, &view, (0.5, 0.75)), None);
        // Nothing selected: every mask that has a place.
        assert_eq!(MaskToolSession::default().pins(&recipe, &view).len(), 2);
    }

    #[test]
    fn the_outline_follows_the_selected_shape() {
        let (recipe, view) = (recipe(), view());
        let Some(MaskOutline::Linear { start, center, end }) = selecting(0).outline(&recipe, &view)
        else {
            panic!("a line");
        };
        assert_at(start, (0.8, 0.1));
        assert_at(center, (0.8, 0.3));
        assert_at(end, (0.8, 0.5));

        let Some(MaskOutline::Radial {
            center,
            x,
            y,
            inner,
            ..
        }) = selecting(1).outline(&recipe, &view)
        else {
            panic!("an ellipse");
        };
        assert_at(center, (0.5, 0.75));
        // Long-edge units of 600 pixels; the photo is 400x600 as shown.
        assert_at(x, (0.5, 0.95));
        assert_at(y, (0.35, 0.75));
        assert_eq!(inner, 0.75);

        assert_eq!(selecting(2).outline(&recipe, &view), None);
        let mut creating = selecting(0);
        creating.mode = MaskEditMode::CreateLinear;
        assert_eq!(creating.outline(&recipe, &view), None);
    }

    #[test]
    fn creating_and_painting_take_every_press_and_selecting_only_handles() {
        let (recipe, view) = (recipe(), view());
        let mut session = selecting(0);
        assert!(session.grabs(&recipe, &view, (0.8, 0.1)));
        assert!(!session.grabs(&recipe, &view, (0.1, 0.9)));
        for (mode, grabs) in [
            (MaskEditMode::CreateLinear, true),
            (MaskEditMode::CreateRadial, true),
            (MaskEditMode::Paint { erase: true }, true),
            (MaskEditMode::PickColor, false),
        ] {
            session.mode = mode;
            assert_eq!(session.grabs(&recipe, &view, (0.1, 0.9)), grabs);
        }
    }
}
