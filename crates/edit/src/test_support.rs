//! Recipes and comparisons shared by the tests.

use std::collections::{BTreeMap, BTreeSet};

use rohditor_camera_profile::{CalibrationIlluminant, MatrixCalibration, MatrixCameraProfile};
use rohditor_image::Orientation;
use serde_json::Value;

use crate::{
    CameraChroma, CameraProfileSelection, CameraTone, EditRecipe, HighlightMethod,
    HslChannelAdjustments, LensProfileSelection, LocalSettings, MaskLayer, NormalizedCropRect,
    RenderingProfileSelection, ToneCurve, WhiteBalance,
};

/// A valid recipe whose every serialized value differs from the defaults, with
/// different values in every field so that a mix-up between fields shows.
pub(crate) fn fully_edited() -> EditRecipe {
    let mut recipe = EditRecipe::default();

    let light = &mut recipe.light;
    light.exposure_ev = 0.7;
    light.contrast = 0.1;
    light.highlights = -0.2;
    light.shadows = 0.3;
    light.whites = 0.15;
    light.blacks = -0.1;
    light.tone_curve = ToneCurve::through(&[[0.2, 0.28], [0.45, 0.4], [0.8, 0.85]]);

    let color = &mut recipe.color;
    color.camera_profile = CameraProfileSelection::Matrix(MatrixCameraProfile {
        format_version: 1,
        source_sha256: "ab".repeat(32),
        name: "Test".to_owned(),
        camera_model: "Camera".to_owned(),
        copyright: None,
        calibrations: vec![MatrixCalibration {
            illuminant: CalibrationIlluminant::D65,
            xyz_to_camera: [[1.1, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            forward_camera_to_xyz_d50: None,
            hue_sat_map: None,
        }],
        look_table: None,
        tone_curve: None,
    });
    color.white_balance = WhiteBalance::AsShot;
    color.saturation = 1.4;
    color.vibrance = 0.2;
    for (index, channel) in color.hsl.channels.iter_mut().enumerate() {
        let step = (index + 1) as f32 * 0.01;
        *channel = HslChannelAdjustments {
            hue: 0.1 + step,
            saturation: -0.1 - step,
            luminance: 0.05 + step,
        };
    }
    color.grading.shadows = [0.1, 0.2, 0.3];
    color.grading.midtones = [-0.1, -0.2, -0.3];
    color.grading.highlights = [0.15, -0.25, 0.35];

    recipe.rendering.profile = RenderingProfileSelection::MATCHED;
    recipe.rendering.camera_tone = Some(CameraTone::static_fallback());
    recipe.rendering.camera_chroma = Some(CameraChroma::STATIC_FALLBACK);

    recipe.raw.highlights.method = HighlightMethod::Opposed;
    recipe.raw.highlights.clip.threshold = 0.91;
    recipe.raw.highlights.opposed.detection_threshold = 0.93;

    recipe.presence.texture = 0.2;
    recipe.presence.clarity = 0.3;
    recipe.presence.dehaze = -0.4;

    recipe.noise_reduction.chroma = 0.5;
    recipe.noise_reduction.luminance = 0.4;
    recipe.noise_reduction.luminance_detail = 0.7;

    recipe.capture_sharpening.enabled = true;
    recipe.capture_sharpening.amount = 0.6;
    recipe.capture_sharpening.radius = 1.1;
    recipe.capture_sharpening.noise_protection = 0.8;

    recipe.false_color.steps = 2;

    recipe.defringe.purple_amount = 0.5;
    recipe.defringe.purple_hue = [251.0, 291.0];
    recipe.defringe.green_amount = 0.4;
    recipe.defringe.green_hue = [81.0, 141.0];

    recipe.optics.profile = LensProfileSelection::Automatic;
    recipe.optics.distortion = false;
    recipe.optics.vignetting = false;
    recipe.optics.chromatic_aberration = false;

    recipe.geometry.orientation_override = Some(Orientation::Rotate90);
    recipe.geometry.crop = Some(NormalizedCropRect {
        left: 0.1,
        top: 0.2,
        right: 0.8,
        bottom: 0.9,
    });

    recipe.local.layers.push(MaskLayer {
        id: 3,
        name: "Sky".to_owned(),
        enabled: true,
        opacity: 0.6,
        components: Vec::new(),
        settings: LocalSettings {
            exposure_ev: -0.5,
            ..LocalSettings::default()
        },
    });

    recipe
}

/// Every serialized value of a recipe by path, e.g. `recipe.light.shadows` or
/// `recipe.color.hsl.channels[2].hue`. Empty containers count as values.
pub(crate) fn leaves(recipe: &EditRecipe) -> BTreeMap<String, Value> {
    fn walk(value: &Value, path: String, leaves: &mut BTreeMap<String, Value>) {
        match value {
            Value::Object(fields) if !fields.is_empty() => {
                for (name, field) in fields {
                    walk(field, format!("{path}.{name}"), leaves);
                }
            }
            Value::Array(items) if !items.is_empty() => {
                for (index, item) in items.iter().enumerate() {
                    walk(item, format!("{path}[{index}]"), leaves);
                }
            }
            leaf => {
                leaves.insert(path, leaf.clone());
            }
        }
    }
    let mut leaves = BTreeMap::new();
    walk(
        &serde_json::to_value(recipe).expect("serialize"),
        "recipe".to_owned(),
        &mut leaves,
    );
    leaves
}

/// Paths whose value differs between two recipes, including values only one
/// of them has.
pub(crate) fn changed_paths(a: &EditRecipe, b: &EditRecipe) -> BTreeSet<String> {
    let (a, b) = (leaves(a), leaves(b));
    a.keys()
        .chain(b.keys())
        .filter(|path| a.get(*path) != b.get(*path))
        .cloned()
        .collect()
}
