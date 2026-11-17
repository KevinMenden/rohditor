//! The mask recipes the CPU/GPU parity tests run, shared by preview and export.

use rohditor_image::Orientation;
use rohditor_raw::RawFrame;

use rohditor_edit::{
    BrushMask, BrushStroke, ColorRange, ColorSample, EditRecipe, LinearGradient, LocalSettings,
    LuminanceRange, MaskComponent, MaskLayer, MaskMode, MaskPoint, MaskShape, RadialGradient,
};

/// A mosaic with colour: each CFA site follows its own ramp, so hue changes across the frame and
/// the range masks have something to select.
pub(crate) fn colored_frame(width: usize, height: usize, orientation: Orientation) -> RawFrame {
    let mut frame = crate::preview::processor::tests::synthetic_frame(orientation);
    frame.info.width = width;
    frame.info.height = height;
    frame.row_stride = width;
    frame.mosaic = (0..width * height)
        .map(|index| {
            let (x, y) = (index % width, index / width);
            let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
            let level = 4_000.0 + 40_000.0 * v * v;
            let tint = match (y % 2, x % 2) {
                (0, 0) => 0.3 + 1.4 * u,
                (1, 1) => 1.7 - 1.4 * u,
                _ => 1.0,
            };
            let grain = ((index as u32).wrapping_mul(2_654_435_761) >> 22) as f32 - 512.0;
            (level * tint + grain).clamp(0.0, 65_535.0) as u16
        })
        .collect::<Vec<_>>()
        .into();
    frame
}

fn point(x: f32, y: f32) -> MaskPoint {
    MaskPoint { x, y }
}

pub(crate) fn linear() -> MaskShape {
    MaskShape::Linear(LinearGradient {
        start: point(0.2, 0.15),
        end: point(0.75, 0.6),
    })
}

pub(crate) fn radial() -> MaskShape {
    MaskShape::Radial(RadialGradient {
        center: point(0.35, 0.6),
        radius_x: 0.4,
        radius_y: 0.15,
        angle_degrees: 25.0,
        feather: 0.6,
    })
}

/// Two painted strokes and an erase across them, off centre so a flipped plane cannot pass.
pub(crate) fn brush(offset: f32) -> MaskShape {
    let stroke = |points: Vec<MaskPoint>, erase| BrushStroke {
        points,
        radius: 0.08,
        feather: 0.5,
        flow: 0.7,
        erase,
    };
    MaskShape::Brush(BrushMask {
        strokes: vec![
            stroke(
                vec![
                    point(0.1 + offset, 0.2),
                    point(0.5 + offset, 0.3),
                    point(0.7, 0.75),
                ],
                false,
            ),
            stroke(vec![point(0.15, 0.8), point(0.4, 0.6)], false),
            stroke(vec![point(0.3 + offset, 0.1), point(0.35, 0.45)], true),
        ],
    })
}

pub(crate) fn luminance() -> MaskShape {
    MaskShape::Luminance(LuminanceRange {
        low: 0.35,
        high: 0.6,
        feather_low: 0.15,
        feather_high: 0.05,
    })
}

pub(crate) fn color(sample: ColorSample) -> MaskShape {
    MaskShape::Color(ColorRange {
        samples: vec![
            ColorSample {
                lightness: 0.9,
                a: -0.2,
                b: 0.2,
            },
            sample,
        ],
        tolerance: 0.45,
    })
}

pub(crate) fn component(mode: MaskMode, invert: bool, shape: MaskShape) -> MaskComponent {
    MaskComponent {
        mode,
        invert,
        shape,
    }
}

pub(crate) fn layer(components: Vec<MaskComponent>, settings: LocalSettings) -> MaskLayer {
    MaskLayer {
        id: 0,
        name: String::new(),
        enabled: true,
        opacity: 1.0,
        components,
        settings,
    }
}

/// Recipes reject duplicate layer ids.
pub(crate) fn with_unique_ids(layers: Vec<MaskLayer>) -> Vec<MaskLayer> {
    layers
        .into_iter()
        .enumerate()
        .map(|(index, layer)| MaskLayer {
            id: index as u32,
            ..layer
        })
        .collect()
}

fn add(shape: MaskShape) -> Vec<MaskComponent> {
    vec![component(MaskMode::Add, false, shape)]
}

fn every_slider() -> LocalSettings {
    LocalSettings {
        exposure_ev: 0.8,
        temperature: 0.4,
        tint: -0.3,
        contrast: 0.5,
        highlights: -0.4,
        shadows: 0.3,
        whites: 0.2,
        blacks: -0.2,
        saturation: 0.6,
        detail: rohditor_edit::LocalDetail::default(),
    }
}

/// Every mask kind the issue names, on top of `base`. `sample` is a colour the fixture has.
pub(crate) fn mask_recipes(
    base: &EditRecipe,
    sample: ColorSample,
) -> Vec<(&'static str, EditRecipe)> {
    let exposure = LocalSettings {
        exposure_ev: 1.0,
        ..LocalSettings::default()
    };
    let display = LocalSettings {
        contrast: 0.7,
        saturation: -0.5,
        ..LocalSettings::default()
    };
    let cases = vec![
        ("linear", vec![layer(add(linear()), every_slider())]),
        ("radial", vec![layer(add(radial()), every_slider())]),
        ("brush", vec![layer(add(brush(0.0)), every_slider())]),
        ("luminance", vec![layer(add(luminance()), every_slider())]),
        ("color", vec![layer(add(color(sample)), every_slider())]),
        (
            "inverted",
            vec![layer(
                vec![component(MaskMode::Add, true, radial())],
                exposure,
            )],
        ),
        (
            "inverted brush and range",
            vec![layer(
                vec![
                    component(MaskMode::Add, true, brush(0.0)),
                    component(MaskMode::Intersect, true, luminance()),
                ],
                exposure,
            )],
        ),
        (
            "subtracted",
            vec![layer(
                vec![
                    component(MaskMode::Add, false, radial()),
                    component(MaskMode::Subtract, false, linear()),
                    component(MaskMode::Subtract, false, brush(0.1)),
                ],
                every_slider(),
            )],
        ),
        (
            "intersected ranges",
            vec![layer(
                vec![
                    component(MaskMode::Add, false, linear()),
                    component(MaskMode::Intersect, false, luminance()),
                    component(MaskMode::Intersect, true, color(sample)),
                ],
                exposure,
            )],
        ),
        (
            // Display terms depend on the layer order; a brush plane per layer.
            "multi-layer",
            vec![
                layer(add(linear()), exposure),
                layer(add(brush(0.0)), display),
                MaskLayer {
                    opacity: 0.5,
                    ..layer(add(radial()), every_slider())
                },
                layer(
                    add(brush(0.1)),
                    LocalSettings {
                        temperature: -0.6,
                        saturation: 0.9,
                        ..LocalSettings::default()
                    },
                ),
                layer(add(luminance()), display),
            ],
        ),
    ];
    cases
        .into_iter()
        .map(|(name, layers)| {
            let mut recipe = base.clone();
            recipe.local.layers = with_unique_ids(layers);
            (name, recipe)
        })
        .collect()
}
