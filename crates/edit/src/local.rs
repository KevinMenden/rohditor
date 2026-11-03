//! Local adjustments: ordered mask layers, each with its own small set of sliders.
//!
//! This module only holds the recipe model, validation and a few pure helpers. Mask
//! evaluation lives in a later crate, and the pipeline applies the result on top of the
//! global adjustments.
//!
//! # Coordinate contract
//!
//! * **Points** are normalized to the *developed frame*: the image as it enters the
//!   adjustment stage, i.e. after optics and before orientation override and crop. `x` and
//!   `y` each run `0..1` across that frame's width and height. Masks therefore stay attached
//!   to the content when the crop or orientation changes.
//! * **Lengths** (`radius`, `radius_x`, `radius_y`) are in *long-edge units*: a fraction of
//!   `max(width, height)` of the developed frame. A circle stays a circle and a fit preview
//!   matches the export.
//! * Points may lie outside the frame (gradients are often dragged from outside); they are
//!   accepted in [`MASK_POINT_RANGE`].
//! * Toggling lens distortion correction changes the developed frame slightly, so masks shift
//!   a little. That is accepted (Lightroom behaves the same).

use serde::{Deserialize, Serialize};

use crate::{
    BLACKS_RANGE, CONTRAST_RANGE, EXPOSURE_EV_RANGE, EditError, HIGHLIGHTS_RANGE, ParameterRange,
    SHADOWS_RANGE, WHITES_RANGE,
};

/// Most mask layers in one recipe.
pub const MAX_MASK_LAYERS: usize = 32;
/// Most components in one mask layer.
pub const MAX_MASK_COMPONENTS: usize = 16;
/// Most strokes in one brush component.
pub const MAX_BRUSH_STROKES: usize = 1024;
/// Most brush points summed over the whole recipe.
pub const MAX_BRUSH_POINTS: usize = 200_000;
/// Longest layer name, in bytes.
pub const MAX_MASK_NAME_BYTES: usize = 128;
/// Smallest distance between the two points of a linear gradient.
pub const MIN_LINEAR_GRADIENT_LENGTH: f32 = 1e-4;
/// Smallest radial gradient radius, in long-edge units.
pub const MIN_RADIAL_RADIUS: f32 = 1e-3;

/// Normalized point coordinates; outside the frame is allowed.
pub const MASK_POINT_RANGE: ParameterRange = ParameterRange {
    minimum: -1.0,
    maximum: 2.0,
    neutral: 0.0,
};
/// Layer opacity scaling the combined mask.
pub const MASK_OPACITY_RANGE: ParameterRange = ParameterRange {
    minimum: 0.0,
    maximum: 1.0,
    neutral: 1.0,
};
/// Radial gradient radius and brush radius, in long-edge units.
pub const MASK_RADIUS_RANGE: ParameterRange = ParameterRange {
    minimum: 0.0,
    maximum: 2.0,
    neutral: 0.1,
};
/// Radial gradient rotation.
pub const MASK_ANGLE_RANGE: ParameterRange = ParameterRange {
    minimum: -360.0,
    maximum: 360.0,
    neutral: 0.0,
};
/// Feather and flow, `0..=1`.
pub const MASK_UNIT_RANGE: ParameterRange = ParameterRange {
    minimum: 0.0,
    maximum: 1.0,
    neutral: 0.5,
};
/// Relative temperature shift inside a mask (not Kelvin).
pub const LOCAL_TEMPERATURE_RANGE: ParameterRange = ParameterRange {
    minimum: -1.0,
    maximum: 1.0,
    neutral: 0.0,
};
/// Relative tint shift inside a mask.
pub const LOCAL_TINT_RANGE: ParameterRange = ParameterRange {
    minimum: -1.0,
    maximum: 1.0,
    neutral: 0.0,
};
/// Saturation change inside a mask; unlike the global slider, 0 is neutral.
pub const LOCAL_SATURATION_RANGE: ParameterRange = ParameterRange {
    minimum: -1.0,
    maximum: 1.0,
    neutral: 0.0,
};

/// Ordered mask layers, applied in order on top of the global adjustments.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalAdjustments {
    pub layers: Vec<MaskLayer>,
}

/// One mask (a combination of components) with its own sliders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaskLayer {
    /// Stable within one recipe (UI selection, overlay request).
    pub id: u32,
    pub name: String,
    pub enabled: bool,
    /// `0.0..=1.0`, scales the combined mask.
    pub opacity: f32,
    pub components: Vec<MaskComponent>,
    #[serde(default)]
    pub settings: LocalSettings,
}

/// How a component combines with the components before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaskMode {
    Add,
    Subtract,
    Intersect,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaskComponent {
    pub mode: MaskMode,
    pub invert: bool,
    pub shape: MaskShape,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MaskShape {
    Linear(LinearGradient),
    Radial(RadialGradient),
    Brush(BrushMask),
}

/// Normalized position in the developed frame; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MaskPoint {
    pub x: f32,
    pub y: f32,
}

/// Full effect at `start`, falling to none at `end`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LinearGradient {
    pub start: MaskPoint,
    pub end: MaskPoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RadialGradient {
    pub center: MaskPoint,
    /// Long-edge units, before rotation.
    pub radius_x: f32,
    pub radius_y: f32,
    pub angle_degrees: f32,
    /// `0..=1`; 0 is a hard edge.
    pub feather: f32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BrushMask {
    pub strokes: Vec<BrushStroke>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrushStroke {
    pub points: Vec<MaskPoint>,
    /// Long-edge units.
    pub radius: f32,
    /// `0..=1`.
    pub feather: f32,
    /// `0..=1`.
    pub flow: f32,
    pub erase: bool,
}

/// The sliders available inside a mask. All neutral at 0.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalSettings {
    pub exposure_ev: f32,
    /// Relative shift, not Kelvin.
    pub temperature: f32,
    /// Relative shift.
    pub tint: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub saturation: f32,
}

impl MaskLayer {
    /// True when this layer would change pixels: enabled, visible, shaped and non-neutral.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.enabled
            && self.opacity > 0.0
            && !self.components.is_empty()
            && !self.settings.is_neutral()
    }
}

impl LocalSettings {
    /// All sliders are exactly 0, so the layer changes nothing.
    #[must_use]
    pub fn is_neutral(&self) -> bool {
        *self == Self::default()
    }

    fn validate(&self) -> Result<(), EditError> {
        check(
            "local.layers.settings.exposure_ev",
            self.exposure_ev,
            EXPOSURE_EV_RANGE,
        )?;
        check(
            "local.layers.settings.temperature",
            self.temperature,
            LOCAL_TEMPERATURE_RANGE,
        )?;
        check("local.layers.settings.tint", self.tint, LOCAL_TINT_RANGE)?;
        check(
            "local.layers.settings.contrast",
            self.contrast,
            CONTRAST_RANGE,
        )?;
        check(
            "local.layers.settings.highlights",
            self.highlights,
            HIGHLIGHTS_RANGE,
        )?;
        check("local.layers.settings.shadows", self.shadows, SHADOWS_RANGE)?;
        check("local.layers.settings.whites", self.whites, WHITES_RANGE)?;
        check("local.layers.settings.blacks", self.blacks, BLACKS_RANGE)?;
        check(
            "local.layers.settings.saturation",
            self.saturation,
            LOCAL_SATURATION_RANGE,
        )
    }
}

impl LocalAdjustments {
    /// True when at least one layer would change pixels; the pipeline skips the stage otherwise.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.layers.iter().any(MaskLayer::is_active)
    }

    /// An id no current layer uses (one past the largest). Ids of deleted layers can only be
    /// reused when the highest id was the one deleted, so callers holding ids across a delete
    /// (selection, overlay request) must drop them on delete.
    #[must_use]
    pub fn next_layer_id(&self) -> u32 {
        self.layers
            .iter()
            .map(|layer| layer.id + 1)
            .max()
            .unwrap_or(0)
    }

    #[must_use]
    pub fn layer(&self, id: u32) -> Option<&MaskLayer> {
        self.layers.iter().find(|layer| layer.id == id)
    }

    pub fn layer_mut(&mut self, id: u32) -> Option<&mut MaskLayer> {
        self.layers.iter_mut().find(|layer| layer.id == id)
    }

    pub fn remove_layer(&mut self, id: u32) -> Option<MaskLayer> {
        let index = self.layers.iter().position(|layer| layer.id == id)?;
        Some(self.layers.remove(index))
    }

    /// Moves a layer to `to_index`, clamped to the last position. False if `id` is unknown.
    pub fn move_layer(&mut self, id: u32, to_index: usize) -> bool {
        let Some(from) = self.layers.iter().position(|layer| layer.id == id) else {
            return false;
        };
        let layer = self.layers.remove(from);
        self.layers.insert(to_index.min(self.layers.len()), layer);
        true
    }

    pub fn validate(&self) -> Result<(), EditError> {
        if self.layers.len() > MAX_MASK_LAYERS {
            return Err(error(
                "local.layers",
                format!("at most {MAX_MASK_LAYERS} layers"),
            ));
        }
        let mut brush_points = 0usize;
        for (index, layer) in self.layers.iter().enumerate() {
            if self.layers[..index]
                .iter()
                .any(|other| other.id == layer.id)
            {
                return Err(error(
                    "local.layers.id",
                    format!("duplicate layer id {}", layer.id),
                ));
            }
            layer.validate()?;
            brush_points += layer.brush_point_count();
            if brush_points > MAX_BRUSH_POINTS {
                return Err(error(
                    "local.layers.components.shape.strokes.points",
                    format!("at most {MAX_BRUSH_POINTS} brush points per recipe"),
                ));
            }
        }
        Ok(())
    }
}

impl MaskLayer {
    fn brush_point_count(&self) -> usize {
        self.components
            .iter()
            .map(|component| match &component.shape {
                MaskShape::Brush(brush) => brush.strokes.iter().map(|s| s.points.len()).sum(),
                _ => 0,
            })
            .sum()
    }

    fn validate(&self) -> Result<(), EditError> {
        if self.name.len() > MAX_MASK_NAME_BYTES {
            return Err(error(
                "local.layers.name",
                format!("limited to {MAX_MASK_NAME_BYTES} bytes"),
            ));
        }
        check("local.layers.opacity", self.opacity, MASK_OPACITY_RANGE)?;
        if self.components.len() > MAX_MASK_COMPONENTS {
            return Err(error(
                "local.layers.components",
                format!("at most {MAX_MASK_COMPONENTS} components per layer"),
            ));
        }
        // Later components combine with the result so far; the first has nothing to subtract
        // from or intersect with.
        if self
            .components
            .first()
            .is_some_and(|c| c.mode != MaskMode::Add)
        {
            return Err(error(
                "local.layers.components.mode",
                "the first component must be Add".to_owned(),
            ));
        }
        for component in &self.components {
            component.shape.validate()?;
        }
        self.settings.validate()
    }
}

impl MaskShape {
    fn validate(&self) -> Result<(), EditError> {
        match self {
            Self::Linear(gradient) => {
                check_point("local.layers.components.shape.start", gradient.start)?;
                check_point("local.layers.components.shape.end", gradient.end)?;
                let length =
                    (gradient.end.x - gradient.start.x).hypot(gradient.end.y - gradient.start.y);
                if length < MIN_LINEAR_GRADIENT_LENGTH {
                    return Err(error(
                        "local.layers.components.shape.end",
                        format!("must be at least {MIN_LINEAR_GRADIENT_LENGTH} from start"),
                    ));
                }
            }
            Self::Radial(radial) => {
                check_point("local.layers.components.shape.center", radial.center)?;
                for (field, radius) in [
                    ("local.layers.components.shape.radius_x", radial.radius_x),
                    ("local.layers.components.shape.radius_y", radial.radius_y),
                ] {
                    check(field, radius, MASK_RADIUS_RANGE)?;
                    if radius < MIN_RADIAL_RADIUS {
                        return Err(error(
                            field,
                            format!("must be at least {MIN_RADIAL_RADIUS}"),
                        ));
                    }
                }
                check(
                    "local.layers.components.shape.angle_degrees",
                    radial.angle_degrees,
                    MASK_ANGLE_RANGE,
                )?;
                check(
                    "local.layers.components.shape.feather",
                    radial.feather,
                    MASK_UNIT_RANGE,
                )?;
            }
            Self::Brush(brush) => {
                if brush.strokes.len() > MAX_BRUSH_STROKES {
                    return Err(error(
                        "local.layers.components.shape.strokes",
                        format!("at most {MAX_BRUSH_STROKES} strokes per brush"),
                    ));
                }
                for stroke in &brush.strokes {
                    stroke.validate()?;
                }
            }
        }
        Ok(())
    }
}

impl BrushStroke {
    fn validate(&self) -> Result<(), EditError> {
        const POINTS: &str = "local.layers.components.shape.strokes.points";
        if self.points.is_empty() {
            return Err(error(
                POINTS,
                "a stroke needs at least one point".to_owned(),
            ));
        }
        // Checked before the per-recipe total so one huge stroke is rejected cheaply too.
        if self.points.len() > MAX_BRUSH_POINTS {
            return Err(error(
                POINTS,
                format!("at most {MAX_BRUSH_POINTS} brush points"),
            ));
        }
        for point in &self.points {
            check_point(POINTS, *point)?;
        }
        check(
            "local.layers.components.shape.strokes.radius",
            self.radius,
            MASK_RADIUS_RANGE,
        )?;
        if self.radius <= 0.0 {
            return Err(error(
                "local.layers.components.shape.strokes.radius",
                "must be greater than 0".to_owned(),
            ));
        }
        check(
            "local.layers.components.shape.strokes.feather",
            self.feather,
            MASK_UNIT_RANGE,
        )?;
        check(
            "local.layers.components.shape.strokes.flow",
            self.flow,
            MASK_UNIT_RANGE,
        )
    }
}

fn error(field: &'static str, reason: String) -> EditError {
    EditError { field, reason }
}

fn check(field: &'static str, value: f32, range: ParameterRange) -> Result<(), EditError> {
    if range.contains(value) {
        Ok(())
    } else {
        Err(error(
            field,
            format!(
                "{value} is outside the inclusive range {}..={}",
                range.minimum, range.maximum
            ),
        ))
    }
}

fn check_point(field: &'static str, point: MaskPoint) -> Result<(), EditError> {
    check(field, point.x, MASK_POINT_RANGE)?;
    check(field, point.y, MASK_POINT_RANGE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EDIT_RECIPE_SCHEMA_VERSION, EditRecipe};

    fn point(x: f32, y: f32) -> MaskPoint {
        MaskPoint { x, y }
    }

    fn component(shape: MaskShape) -> MaskComponent {
        MaskComponent {
            mode: MaskMode::Add,
            invert: false,
            shape,
        }
    }

    fn linear() -> MaskShape {
        MaskShape::Linear(LinearGradient {
            start: point(0.1, -0.2),
            end: point(0.7, 0.4),
        })
    }

    fn radial() -> MaskShape {
        MaskShape::Radial(RadialGradient {
            center: point(0.3, 0.6),
            radius_x: 0.4,
            radius_y: 0.2,
            angle_degrees: 30.0,
            feather: 0.25,
        })
    }

    fn brush(points: usize) -> MaskShape {
        MaskShape::Brush(BrushMask {
            strokes: vec![BrushStroke {
                points: vec![point(0.5, 0.5); points],
                radius: 0.05,
                feather: 0.5,
                flow: 0.8,
                erase: false,
            }],
        })
    }

    fn layer(id: u32, shapes: Vec<MaskShape>) -> MaskLayer {
        MaskLayer {
            id,
            name: format!("Mask {id}"),
            enabled: true,
            opacity: 0.75,
            components: shapes.into_iter().map(component).collect(),
            settings: LocalSettings {
                exposure_ev: 0.5,
                ..LocalSettings::default()
            },
        }
    }

    fn recipe_with(layers: Vec<MaskLayer>) -> EditRecipe {
        EditRecipe {
            local: LocalAdjustments { layers },
            ..EditRecipe::default()
        }
    }

    fn rejected_field(layers: Vec<MaskLayer>) -> &'static str {
        recipe_with(layers)
            .validate()
            .expect_err("recipe should be rejected")
            .field
    }

    #[test]
    fn round_trips_one_layer_of_each_shape() {
        let mut second = layer(7, vec![linear(), radial(), brush(3)]);
        second.components[1].mode = MaskMode::Subtract;
        second.components[2].invert = true;
        let recipe = recipe_with(vec![layer(1, vec![linear()]), second]);
        let json = serde_json::to_string(&recipe).expect("serialize");
        assert!(json.contains(r#""kind":"linear""#) && json.contains(r#""kind":"brush""#));
        assert_eq!(
            serde_json::from_str::<EditRecipe>(&json).expect("deserialize"),
            recipe
        );
    }

    #[test]
    fn schema_15_migrates_to_empty_local_adjustments() {
        let mut previous = serde_json::to_value(EditRecipe::default()).expect("serialize");
        previous["schema_version"] = 15.into();
        previous.as_object_mut().expect("object").remove("local");
        let migrated: EditRecipe = serde_json::from_value(previous).expect("migrate");
        assert_eq!(migrated.schema_version, EDIT_RECIPE_SCHEMA_VERSION);
        assert_eq!(migrated.local, LocalAdjustments::default());
    }

    #[test]
    fn rejects_invalid_values_with_the_offending_field() {
        let mut bad_point = layer(1, vec![linear()]);
        bad_point.components[0].shape = MaskShape::Linear(LinearGradient {
            start: point(f32::NAN, 0.0),
            end: point(1.0, 1.0),
        });
        assert_eq!(
            rejected_field(vec![bad_point]),
            "local.layers.components.shape.start"
        );

        let mut far_point = layer(1, vec![linear()]);
        far_point.components[0].shape = MaskShape::Linear(LinearGradient {
            start: point(0.0, 0.0),
            end: point(2.5, 0.0),
        });
        assert_eq!(
            rejected_field(vec![far_point]),
            "local.layers.components.shape.end"
        );

        let mut degenerate = layer(1, vec![linear()]);
        degenerate.components[0].shape = MaskShape::Linear(LinearGradient {
            start: point(0.5, 0.5),
            end: point(0.5, 0.50001),
        });
        assert_eq!(
            rejected_field(vec![degenerate]),
            "local.layers.components.shape.end"
        );

        let mut thin = layer(1, vec![radial()]);
        if let MaskShape::Radial(radial) = &mut thin.components[0].shape {
            radial.radius_y = 1e-4;
        }
        assert_eq!(
            rejected_field(vec![thin]),
            "local.layers.components.shape.radius_y"
        );

        let mut zero_radius = layer(1, vec![brush(1)]);
        if let MaskShape::Brush(brush) = &mut zero_radius.components[0].shape {
            brush.strokes[0].radius = 0.0;
        }
        assert_eq!(
            rejected_field(vec![zero_radius]),
            "local.layers.components.shape.strokes.radius"
        );

        let mut subtract_first = layer(1, vec![linear()]);
        subtract_first.components[0].mode = MaskMode::Subtract;
        assert_eq!(
            rejected_field(vec![subtract_first]),
            "local.layers.components.mode"
        );

        assert_eq!(
            rejected_field(vec![layer(3, vec![linear()]), layer(3, vec![radial()])]),
            "local.layers.id"
        );

        let mut loud = layer(1, vec![linear()]);
        loud.settings.exposure_ev = 9.0;
        assert_eq!(
            rejected_field(vec![loud]),
            "local.layers.settings.exposure_ev"
        );

        let mut opaque = layer(1, vec![linear()]);
        opaque.opacity = 1.5;
        assert_eq!(rejected_field(vec![opaque]), "local.layers.opacity");
    }

    #[test]
    fn rejects_recipes_over_the_size_limits() {
        let layers = (0..=MAX_MASK_LAYERS as u32)
            .map(|id| layer(id, vec![linear()]))
            .collect();
        assert_eq!(rejected_field(layers), "local.layers");

        let too_many_components = layer(1, vec![linear(); MAX_MASK_COMPONENTS + 1]);
        assert_eq!(
            rejected_field(vec![too_many_components]),
            "local.layers.components"
        );

        // 150k points per layer is fine alone but 300k over two layers is not.
        let heavy = |id| layer(id, vec![brush(150_000)]);
        assert!(recipe_with(vec![heavy(1)]).validate().is_ok());
        assert_eq!(
            rejected_field(vec![heavy(1), heavy(2)]),
            "local.layers.components.shape.strokes.points"
        );

        let many_strokes = BrushMask {
            strokes: vec![
                BrushStroke {
                    points: vec![point(0.5, 0.5)],
                    radius: 0.1,
                    feather: 0.0,
                    flow: 1.0,
                    erase: false
                };
                MAX_BRUSH_STROKES + 1
            ],
        };
        assert_eq!(
            rejected_field(vec![layer(1, vec![MaskShape::Brush(many_strokes)])]),
            "local.layers.components.shape.strokes"
        );
    }

    #[test]
    fn is_active_needs_an_enabled_visible_non_neutral_layer() {
        let active = layer(1, vec![linear()]);
        assert!(
            LocalAdjustments {
                layers: vec![active.clone()]
            }
            .is_active()
        );
        assert!(!LocalAdjustments::default().is_active());

        let variants: [fn(&mut MaskLayer); 4] = [
            |l| l.enabled = false,
            |l| l.opacity = 0.0,
            |l| l.components.clear(),
            |l| l.settings = LocalSettings::default(),
        ];
        for edit in variants {
            let mut inactive = active.clone();
            edit(&mut inactive);
            assert!(
                !LocalAdjustments {
                    layers: vec![inactive]
                }
                .is_active()
            );
        }
        // One inactive layer does not hide an active one.
        let mut off = active.clone();
        off.enabled = false;
        off.id = 0;
        assert!(
            LocalAdjustments {
                layers: vec![off, active]
            }
            .is_active()
        );
    }

    #[test]
    fn layer_helpers_find_remove_and_reorder_by_id() {
        let mut local = LocalAdjustments {
            layers: vec![
                layer(4, vec![linear()]),
                layer(9, vec![radial()]),
                layer(2, vec![linear()]),
            ],
        };
        assert_eq!(local.next_layer_id(), 10);
        assert_eq!(LocalAdjustments::default().next_layer_id(), 0);
        assert_eq!(local.layer(9).map(|l| l.id), Some(9));
        local.layer_mut(9).expect("layer").opacity = 0.1;
        assert_eq!(local.layer(9).expect("layer").opacity, 0.1);

        assert!(local.move_layer(2, 0));
        assert_eq!(
            local.layers.iter().map(|l| l.id).collect::<Vec<_>>(),
            [2, 4, 9]
        );
        assert!(local.move_layer(2, 99));
        assert_eq!(
            local.layers.iter().map(|l| l.id).collect::<Vec<_>>(),
            [4, 9, 2]
        );
        assert!(!local.move_layer(5, 0));

        assert_eq!(local.remove_layer(9).map(|l| l.id), Some(9));
        assert!(local.remove_layer(9).is_none());
        assert_eq!(local.layers.len(), 2);
    }
}
