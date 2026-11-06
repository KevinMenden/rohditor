//! Tests for the local-adjustment terms of the adjustment stage and the mask overlay.

use std::sync::Arc;

use rayon::ThreadPoolBuilder;
use rohditor_edit::{
    EditRecipe, LinearGradient, LocalDetail, LocalSettings, LuminanceRange, MaskComponent,
    MaskLayer, MaskMode, MaskPoint, MaskShape, RenderingProfileSelection,
};
use rohditor_image::{LinearRgbImage, LinearRgbSpace, Orientation};
use rohditor_mask::{MaskFrame, PreparedMask, ReferenceRow};

use super::adjustments::apply_adjustments;
use super::local_stage::LocalStage;
use super::mask_overlay::{MaskOverlayPlan, render_mask_overlay_preview};
use crate::{CancellationToken, OutputGeometry};

pub(super) const WIDTH: usize = 23;
pub(super) const HEIGHT: usize = 11;

/// Neutral rendering keeps the global chain an identity, so a local edit is the only change.
pub(super) fn neutral_recipe() -> EditRecipe {
    let mut recipe = EditRecipe::default();
    recipe.rendering.profile = RenderingProfileSelection::NEUTRAL;
    recipe
}

pub(super) fn image_of(pixel: impl Fn(usize, usize) -> [f32; 3]) -> LinearRgbImage<f32> {
    let mut data = Vec::with_capacity(WIDTH * HEIGHT * 3);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            data.extend(pixel(x, y));
        }
    }
    LinearRgbImage::new(WIDTH, HEIGHT, WIDTH * 3, LinearRgbSpace::Rec2020D65, data)
        .expect("valid test image")
}

/// Different at every pixel and channel, so a mix-up of pixels or rows shows.
pub(super) fn varied_image() -> LinearRgbImage<f32> {
    image_of(|x, y| {
        let seed = (x * 7 + y * 13) % 17;
        let base = 0.05 + 0.6 * seed as f32 / 17.0;
        [base, base * 0.8 + 0.02 * x as f32 / 23.0, base * 0.6 + 0.01]
    })
}

pub(super) fn flat_image(value: f32) -> LinearRgbImage<f32> {
    image_of(|_, _| [value; 3])
}

pub(super) fn linear(start_x: f32, end_x: f32) -> MaskShape {
    MaskShape::Linear(LinearGradient {
        start: MaskPoint { x: start_x, y: 0.5 },
        end: MaskPoint { x: end_x, y: 0.5 },
    })
}

pub(super) fn layer(id: u32, shape: MaskShape, settings: LocalSettings) -> MaskLayer {
    MaskLayer {
        id,
        name: format!("layer {id}"),
        enabled: true,
        opacity: 1.0,
        components: vec![MaskComponent {
            mode: MaskMode::Add,
            invert: false,
            shape,
        }],
        settings,
    }
}

pub(super) fn adjusted(image: &LinearRgbImage<f32>, recipe: &EditRecipe) -> Vec<f32> {
    let mut image = image.clone();
    apply_adjustments(&mut image, recipe).expect("valid adjustments");
    image.data().to_vec()
}

/// Alpha of `layer` at every column of one row (a gradient along x only varies with x).
fn alpha_row(layer: &MaskLayer) -> Vec<f32> {
    let mask = PreparedMask::prepare(
        layer,
        MaskFrame {
            width: WIDTH,
            height: HEIGHT,
        },
    )
    .expect("valid mask");
    let mut row = vec![0.0; WIDTH];
    mask.evaluate_row(HEIGHT / 2, 0..WIDTH, ReferenceRow::NONE, &mut row);
    row
}

pub(super) fn pixel(data: &[f32], x: usize, y: usize) -> [f32; 3] {
    let start = (y * WIDTH + x) * 3;
    [data[start], data[start + 1], data[start + 2]]
}

#[test]
fn neutral_disabled_zero_opacity_and_empty_layers_are_bit_identical_to_no_layers() {
    let image = varied_image();
    let mut recipe = EditRecipe::default();
    recipe.light.exposure_ev = 0.3;
    recipe.light.contrast = 0.2;
    recipe.color.saturation = 1.1;
    let reference = adjusted(&image, &recipe);

    let active = LocalSettings {
        exposure_ev: 1.0,
        shadows: 0.5,
        ..LocalSettings::default()
    };
    let mut neutral = layer(0, linear(0.2, 0.8), LocalSettings::default());
    let mut disabled = layer(1, linear(0.2, 0.8), active);
    disabled.enabled = false;
    let mut transparent = layer(2, linear(0.2, 0.8), active);
    transparent.opacity = 0.0;
    let mut empty = layer(3, linear(0.2, 0.8), active);
    empty.components.clear();
    neutral.name.clear();
    recipe.local.layers = vec![neutral, disabled, transparent, empty];

    assert!(!recipe.local.is_active());
    assert!(
        LocalStage::prepare(&recipe, WIDTH, HEIGHT, &|_| None)
            .expect("prepares")
            .is_none()
    );
    assert_eq!(adjusted(&image, &recipe), reference);
}

#[test]
fn exposure_follows_the_gradient_and_matches_a_global_edit_where_alpha_is_one() {
    let image = flat_image(0.2);
    let local_layer = layer(
        0,
        linear(0.3, 0.7),
        LocalSettings {
            exposure_ev: 1.0,
            ..LocalSettings::default()
        },
    );
    let alpha = alpha_row(&local_layer);
    assert_eq!(alpha[0], 1.0);
    assert_eq!(alpha[WIDTH - 1], 0.0);

    // A global exposure underneath, so alpha 1 must equal a global edit with the summed stops.
    let mut base = neutral_recipe();
    base.light.exposure_ev = 0.5;
    let mut masked = base.clone();
    masked.local.layers.push(local_layer);
    let mut summed = neutral_recipe();
    summed.light.exposure_ev = 1.5;

    let output = adjusted(&image, &masked);
    let without = adjusted(&image, &base);
    let with_global_sum = adjusted(&image, &summed);
    let y = HEIGHT / 2;
    for (x, &a) in alpha.iter().enumerate() {
        if a == 1.0 {
            assert_eq!(pixel(&output, x, y), pixel(&with_global_sum, x, y), "x={x}");
        } else if a == 0.0 {
            assert_eq!(pixel(&output, x, y), pixel(&without, x, y), "x={x}");
        }
    }
    // In between the result rises monotonically towards the left, where the alpha grows.
    let lumas: Vec<f32> = (0..WIDTH).map(|x| pixel(&output, x, y)[1]).collect();
    assert!(lumas.windows(2).all(|pair| pair[0] >= pair[1]), "{lumas:?}");
    assert!(lumas[0] > lumas[WIDTH - 1]);
    // Every row is identical because the gradient only varies along x.
    for row in 0..HEIGHT {
        assert_eq!(pixel(&output, 4, row), pixel(&output, 4, y));
    }
}

#[test]
fn local_exposure_rolls_off_through_the_base_curve_instead_of_clipping() {
    let image = flat_image(0.4);
    let mut recipe = EditRecipe::default();
    recipe.local.layers.push(layer(
        0,
        linear(0.9, 1.0),
        LocalSettings {
            exposure_ev: 2.0,
            ..LocalSettings::default()
        },
    ));
    let output = adjusted(&image, &recipe);
    // 0.4 * 4 = 1.6 scene-linear; the Standard base curve must keep it below clipping and
    // above the unmasked pixel.
    let masked = pixel(&output, 0, 0)[1];
    let plain = pixel(&output, WIDTH - 1, 0)[1];
    assert!(masked > plain);
    assert!(masked.is_finite());
}

#[test]
fn temperature_shifts_colour_only_inside_the_mask() {
    let image = flat_image(0.3);
    let mut recipe = neutral_recipe();
    recipe.local.layers.push(layer(
        0,
        linear(0.3, 0.7),
        LocalSettings {
            temperature: 1.0,
            ..LocalSettings::default()
        },
    ));
    let output = adjusted(&image, &recipe);
    let y = 0;
    let inside = pixel(&output, 0, y);
    let outside = pixel(&output, WIDTH - 1, y);
    assert_eq!(outside, [0.3; 3]);
    assert!(inside[0] > inside[2], "warm shift raises red over blue");
    let luminance = 0.2627 * inside[0] + 0.678 * inside[1] + 0.0593 * inside[2];
    assert!(
        (luminance - 0.3).abs() < 1e-5,
        "luminance preserved: {luminance}"
    );
}

#[test]
fn display_terms_blend_by_alpha_and_equal_the_global_edit_at_full_alpha() {
    let image = varied_image();
    let settings = LocalSettings {
        contrast: 0.6,
        shadows: 0.4,
        saturation: 0.5,
        ..LocalSettings::default()
    };
    let local_layer = layer(0, linear(0.3, 0.7), settings);
    let alpha = alpha_row(&local_layer);
    let mut masked = EditRecipe::default();
    masked.local.layers.push(local_layer);

    // The same sliders applied globally after the same base rendering. Global saturation is
    // a multiplier (`1 + local`) and runs after the tone terms, as the local one does.
    let mut global = EditRecipe::default();
    global.light.contrast = settings.contrast;
    global.light.shadows = settings.shadows;
    global.color.saturation = 1.0 + settings.saturation;

    let output = adjusted(&image, &masked);
    let untouched = adjusted(&image, &EditRecipe::default());
    let global = adjusted(&image, &global);
    let y = HEIGHT / 2;
    for (x, &a) in alpha.iter().enumerate() {
        let (got, base, full) = (
            pixel(&output, x, y),
            pixel(&untouched, x, y),
            pixel(&global, x, y),
        );
        if a == 0.0 {
            assert_eq!(got, base, "x={x}");
        } else if a == 1.0 {
            for c in 0..3 {
                assert!(
                    (got[c] - full[c]).abs() < 1e-5,
                    "x={x} c={c}: {got:?} {full:?}"
                );
            }
        } else {
            for c in 0..3 {
                let expected = base[c] + a * (full[c] - base[c]);
                assert!((got[c] - expected).abs() < 1e-4, "x={x} c={c}");
            }
        }
    }
}

#[test]
fn display_layers_apply_in_order_and_scene_terms_do_not() {
    let image = flat_image(0.3);
    let blacks = LocalSettings {
        blacks: 0.9,
        ..LocalSettings::default()
    };
    let contrast = LocalSettings {
        contrast: 0.9,
        ..LocalSettings::default()
    };
    let build = |order: [(u32, LocalSettings); 2]| {
        let mut recipe = neutral_recipe();
        for (id, settings) in order {
            let mut layer = layer(id, linear(0.9, 1.0), settings);
            layer.opacity = 0.7;
            recipe.local.layers.push(layer);
        }
        adjusted(&image, &recipe)
    };
    let forward = build([(0, blacks), (1, contrast)]);
    let swapped = build([(1, contrast), (0, blacks)]);
    assert_ne!(pixel(&forward, 0, 0), pixel(&swapped, 0, 0));

    let exposure_a = LocalSettings {
        exposure_ev: 0.7,
        ..LocalSettings::default()
    };
    let exposure_b = LocalSettings {
        exposure_ev: -0.2,
        ..LocalSettings::default()
    };
    let build_scene = |order: [(u32, LocalSettings); 2]| {
        let mut recipe = neutral_recipe();
        for (id, settings) in order {
            let mut layer = layer(id, linear(0.9, 1.0), settings);
            layer.opacity = 0.7;
            recipe.local.layers.push(layer);
        }
        adjusted(&image, &recipe)
    };
    assert_eq!(
        build_scene([(0, exposure_a), (1, exposure_b)]),
        build_scene([(1, exposure_b), (0, exposure_a)])
    );
}

#[test]
fn hdr_negative_and_non_finite_pixels_are_preserved() {
    let image = image_of(|x, _| match x % 4 {
        0 => [f32::NAN, 0.2, 0.2],
        1 => [4.0, 2.0, 1.5],
        2 => [-0.05, 0.1, 0.1],
        _ => [0.2, 0.3, 0.1],
    });
    let mut recipe = neutral_recipe();
    recipe.local.layers.push(layer(
        0,
        linear(0.9, 1.0),
        LocalSettings {
            exposure_ev: 0.5,
            contrast: 0.5,
            saturation: -0.3,
            ..LocalSettings::default()
        },
    ));
    let output = adjusted(&image, &recipe);
    assert!(pixel(&output, 0, 0)[0].is_nan());
    assert!(
        pixel(&output, 1, 0)
            .iter()
            .all(|v| v.is_finite() && *v > 1.0)
    );
    assert!(pixel(&output, 2, 0)[0].is_finite());
}

#[test]
fn result_does_not_depend_on_the_thread_count() {
    let image = varied_image();
    let mut recipe = EditRecipe::default();
    for (id, x) in [(0, 0.3), (1, 0.6)] {
        recipe.local.layers.push(layer(
            id,
            linear(x, x + 0.2),
            LocalSettings {
                exposure_ev: 0.4,
                temperature: 0.3,
                shadows: 0.3,
                saturation: 0.2,
                ..LocalSettings::default()
            },
        ));
    }
    let run = |threads| {
        ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .expect("pool")
            .install(|| adjusted(&image, &recipe))
    };
    assert_eq!(run(1), run(4));
}

#[test]
fn stage_bytes_grow_with_layers_and_frame_width() {
    let mut recipe = EditRecipe::default();
    let settings = LocalSettings {
        exposure_ev: 0.5,
        contrast: 0.5,
        ..LocalSettings::default()
    };
    recipe
        .local
        .layers
        .push(layer(0, linear(0.2, 0.8), settings));
    let one = LocalStage::prepare(&recipe, 100, 50, &|_| None)
        .expect("valid test fixture")
        .expect("valid test fixture")
        .bytes();
    recipe
        .local
        .layers
        .push(layer(1, linear(0.2, 0.8), settings));
    let two = LocalStage::prepare(&recipe, 100, 50, &|_| None)
        .expect("valid test fixture")
        .expect("valid test fixture")
        .bytes();
    let wide = LocalStage::prepare(&recipe, 1000, 50, &|_| None)
        .expect("valid test fixture")
        .expect("valid test fixture")
        .bytes();
    assert!(two > one);
    assert!(wide > two);
}

#[test]
fn overlay_has_display_geometry_and_follows_orientation_and_crop() {
    let mut recipe = EditRecipe::default();
    // Not active (neutral settings): the overlay still shows the shape.
    let mut shown = layer(7, linear(0.2, 0.8), LocalSettings::default());
    shown.opacity = 0.0;
    shown.enabled = false;
    recipe.local.layers.push(shown.clone());

    let crop = rohditor_edit::NormalizedCropRect {
        left: 0.1,
        top: 0.2,
        right: 0.9,
        bottom: 0.85,
    };
    shown.opacity = 1.0;
    let mask = PreparedMask::prepare(
        &shown,
        MaskFrame {
            width: WIDTH,
            height: HEIGHT,
        },
    )
    .expect("valid test fixture");
    for orientation in [
        Orientation::Normal,
        Orientation::Rotate90,
        Orientation::Rotate180,
        Orientation::Transpose,
    ] {
        let geometry = OutputGeometry::new(WIDTH, HEIGHT, orientation, Some(crop))
            .expect("valid test fixture");
        let plan = MaskOverlayPlan::prepare(&recipe, 7, (WIDTH, HEIGHT), geometry)
            .expect("valid test fixture")
            .expect("layer exists");
        let overlay = plan
            .render(None, &CancellationToken::new())
            .expect("valid test fixture");
        let (width, height) = geometry.output_dimensions();
        assert_eq!((overlay.width(), overlay.height()), (width, height));
        assert_eq!(overlay.data().len(), width * height);
        let mut row = vec![0.0; WIDTH];
        let mut distinct = std::collections::BTreeSet::new();
        for y in 0..height {
            for x in 0..width {
                let (sx, sy) = geometry.source_coordinate_in_bounds(x, y);
                mask.evaluate_row(sy, 0..WIDTH, ReferenceRow::NONE, &mut row);
                let expected = (row[sx].clamp(0.0, 1.0) * 255.0).round() as u8;
                assert_eq!(
                    overlay.data()[y * width + x],
                    expected,
                    "{orientation:?} {x},{y}"
                );
                distinct.insert(expected);
            }
        }
        // The crop keeps part of the soft edge, so the overlay is not flat.
        assert!(distinct.len() > 2, "{orientation:?}: {distinct:?}");
    }
    assert!(
        MaskOverlayPlan::prepare(
            &recipe,
            99,
            (WIDTH, HEIGHT),
            OutputGeometry::new(WIDTH, HEIGHT, Orientation::Normal, None)
                .expect("valid test fixture")
        )
        .expect("valid test fixture")
        .is_none()
    );
}

#[test]
fn the_small_overlay_matches_the_full_size_one_under_orientation_and_crop() {
    const FULL: (usize, usize) = (400, 240);
    let mut recipe = EditRecipe::default();
    recipe
        .local
        .layers
        .push(layer(3, linear(0.2, 0.8), LocalSettings::default()));
    recipe.geometry.crop = Some(rohditor_edit::NormalizedCropRect {
        left: 0.1,
        top: 0.2,
        right: 0.9,
        bottom: 0.85,
    });
    for orientation in [Orientation::Normal, Orientation::Rotate90] {
        recipe.geometry.orientation_override = Some(orientation);
        let geometry = OutputGeometry::new(FULL.0, FULL.1, orientation, recipe.geometry.crop)
            .expect("valid test fixture");
        let full = MaskOverlayPlan::prepare(&recipe, 3, FULL, geometry)
            .expect("valid test fixture")
            .expect("layer exists")
            .render(None, &CancellationToken::new())
            .expect("valid test fixture");
        let small = render_mask_overlay_preview(&recipe, 3, FULL, Orientation::Normal, 100)
            .expect("valid test fixture")
            .expect("layer exists");

        // Smaller, with the same aspect up to rounding to whole pixels.
        assert!(small.width().max(small.height()) < full.width().max(full.height()) / 3);
        let aspect = |width: usize, height: usize| width as f64 / height as f64;
        let (a, b) = (
            aspect(small.width(), small.height()),
            aspect(full.width(), full.height()),
        );
        assert!((a - b).abs() / b < 0.05, "{orientation:?}: {a} vs {b}");

        // Every small pixel agrees with the full-size overlay where it lands, within the ramp
        // across the pixels in between.
        let mut ramp_pixels = 0;
        for y in 0..small.height() {
            for x in 0..small.width() {
                let fx = (x as f64 + 0.5) / small.width() as f64 * full.width() as f64;
                let fy = (y as f64 + 0.5) / small.height() as f64 * full.height() as f64;
                let expected = full.data()[fy as usize * full.width() + fx as usize];
                let actual = small.data()[y * small.width() + x];
                assert!(
                    actual.abs_diff(expected) <= 24,
                    "{orientation:?} {x},{y}: {actual} vs {expected}"
                );
                ramp_pixels += usize::from(actual > 0 && actual < 255);
            }
        }
        assert!(ramp_pixels > 0, "{orientation:?}: the soft edge is visible");
    }
    assert!(
        render_mask_overlay_preview(&recipe, 99, FULL, Orientation::Normal, 100)
            .expect("valid test fixture")
            .is_none()
    );
}

/// The stage with `copy` as the local detail image of every layer that has detail settings.
fn adjusted_with_copy(
    image: &LinearRgbImage<f32>,
    recipe: &EditRecipe,
    copies: &[(u32, LinearRgbImage<f32>)],
) -> LinearRgbImage<f32> {
    let copies: Vec<(u32, Arc<LinearRgbImage<f32>>)> = copies
        .iter()
        .map(|(id, copy)| (*id, Arc::new(copy.clone())))
        .collect();
    let stage = LocalStage::prepare(recipe, image.width(), image.height(), &|layer| {
        copies
            .iter()
            .find(|(id, _)| *id == layer.id)
            .map(|(_, copy)| Arc::clone(copy))
    })
    .expect("prepares")
    .expect("active");
    let mut working = image.clone();
    super::adjustments::apply_adjustments_with_local(
        &mut working,
        recipe,
        Some(&stage),
        &CancellationToken::new(),
    )
    .expect("adjusts");
    working
}

fn detail_settings() -> LocalSettings {
    LocalSettings {
        detail: LocalDetail {
            luminance_noise: 0.5,
            ..LocalDetail::default()
        },
        ..LocalSettings::default()
    }
}

#[test]
fn the_detail_copy_is_blended_in_by_alpha_at_the_right_pixel_and_row() {
    let image = varied_image();
    // A different value in every row, column and channel, so a mix-up shows.
    let copy = image_of(|x, y| {
        [
            0.1 + 0.03 * x as f32,
            0.2 + 0.04 * y as f32,
            0.3 + 0.001 * (x * y) as f32,
        ]
    });
    let mut recipe = neutral_recipe();
    recipe
        .local
        .layers
        .push(layer(3, linear(0.25, 0.75), detail_settings()));
    let result = adjusted_with_copy(&image, &recipe, &[(3, copy.clone())]);

    let mask = PreparedMask::prepare(
        &recipe.local.layers[0],
        MaskFrame {
            width: WIDTH,
            height: HEIGHT,
        },
    )
    .expect("prepares");
    let mut alphas = vec![0.0; WIDTH];
    let mut partial = 0;
    for y in 0..HEIGHT {
        mask.evaluate_row(y, 0..WIDTH, ReferenceRow::NONE, &mut alphas);
        for (x, &alpha) in alphas.iter().enumerate() {
            partial += usize::from(alpha > 0.0 && alpha < 1.0);
            for channel in 0..3 {
                let at = y * WIDTH * 3 + x * 3 + channel;
                let (from, to) = (image.data()[at], copy.data()[at]);
                let expected = from + alpha * (to - from);
                assert!(
                    (result.data()[at] - expected).abs() < 1e-5,
                    "({x}, {y}) channel {channel}: {} vs {expected}",
                    result.data()[at]
                );
            }
        }
    }
    assert!(partial > 0, "the gradient must also test partial alpha");
}

#[test]
fn later_detail_layers_blend_over_earlier_ones() {
    let image = flat_image(0.2);
    let mut recipe = neutral_recipe();
    // Both cover the left half fully; the second one wins there.
    for id in [0, 1] {
        recipe
            .local
            .layers
            .push(layer(id, linear(0.5, 0.5001), detail_settings()));
    }
    let result = adjusted_with_copy(
        &image,
        &recipe,
        &[(0, flat_image(0.4)), (1, flat_image(0.6))],
    );
    assert_eq!(result.data()[0], 0.6);
    assert_eq!(result.data()[(WIDTH - 1) * 3], 0.2);
}

#[test]
fn a_detail_copy_does_not_change_what_range_masks_select() {
    // Dark pixels on the left, bright ones on the right. The copy swaps them, so a range mask
    // reading blended pixels would select the opposite side.
    let image = image_of(|x, _| [if x < WIDTH / 2 { 0.01 } else { 0.9 }; 3]);
    let copy = image_of(|x, _| [if x < WIDTH / 2 { 0.9 } else { 0.01 }; 3]);
    let everywhere = linear(1.9, 2.0);

    let mut recipe = neutral_recipe();
    recipe
        .local
        .layers
        .push(layer(0, everywhere.clone(), detail_settings()));
    let mut ranged = layer(
        1,
        everywhere,
        LocalSettings {
            exposure_ev: 1.0,
            ..LocalSettings::default()
        },
    );
    ranged.components.push(MaskComponent {
        mode: MaskMode::Intersect,
        invert: false,
        shape: MaskShape::Luminance(LuminanceRange {
            low: 0.0,
            high: 0.3,
            feather_low: 0.0,
            feather_high: 0.0,
        }),
    });
    recipe.local.layers.push(ranged);

    let result = adjusted_with_copy(&image, &recipe, &[(0, copy)]);
    // Left was dark when it entered the stage, so it is selected: 0.9, then one stop up.
    assert!(
        (result.data()[0] - 1.8).abs() < 1e-4,
        "{}",
        result.data()[0]
    );
    // Right was bright, so it isn't, although it is dark after the blend.
    let right = result.data()[(WIDTH - 1) * 3];
    assert!((right - 0.01).abs() < 1e-4, "{right}");
}

#[test]
fn non_finite_copy_pixels_leave_the_base_pixel_alone() {
    let image = flat_image(0.2);
    let copy = image_of(|x, _| [if x == 0 { f32::NAN } else { 0.4 }; 3]);
    let mut recipe = neutral_recipe();
    recipe
        .local
        .layers
        .push(layer(0, linear(1.9, 2.0), detail_settings()));
    let result = adjusted_with_copy(&image, &recipe, &[(0, copy)]);
    assert_eq!(result.data()[0], 0.2);
    assert_eq!(result.data()[3], 0.4);
}

#[test]
fn a_copy_of_another_size_is_refused() {
    let wrong = Arc::new(
        LinearRgbImage::new(1, 1, 3, LinearRgbSpace::Rec2020D65, vec![0.0; 3])
            .expect("valid test image"),
    );
    let mut recipe = neutral_recipe();
    recipe
        .local
        .layers
        .push(layer(0, linear(0.2, 0.8), detail_settings()));
    assert!(LocalStage::prepare(&recipe, WIDTH, HEIGHT, &|_| Some(Arc::clone(&wrong))).is_err());
}
