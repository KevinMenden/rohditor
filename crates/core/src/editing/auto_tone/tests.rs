use rohditor_edit::{BLACKS_RANGE, HIGHLIGHTS_RANGE, RenderingProfileSelection, SHADOWS_RANGE};
use rohditor_image::{DisplayColorSpace, DisplayRgbImage};

use super::*;

/// Scene luminances at zero exposure: mostly `median`, with 2% each of `dark`
/// and `bright` so the 1st, 99th, and 99.5th percentiles land on them.
fn scene(dark: f32, median: f32, bright: f32) -> Vec<f32> {
    let mut samples = vec![dark; 10];
    samples.extend(vec![median; 480]);
    samples.extend(vec![bright; 10]);
    samples
}

/// Histogram of a scene rendered by the recipe's modelled tone stages.
fn rendered_histogram(recipe: &EditRecipe, scene: &[f32]) -> Histogram {
    let base_lut = base_rendering_lut(&recipe.rendering);
    let stages = ToneStages::new(base_lut.as_deref(), recipe);
    let gain = recipe.light.exposure_ev.exp2();
    let mut data = Vec::with_capacity(scene.len() * 3);
    for value in scene {
        let code = stages.display_code(value * gain).round() as u8;
        data.extend([code; 3]);
    }
    let image = DisplayRgbImage::new(
        scene.len(),
        1,
        scene.len() * 3,
        DisplayColorSpace::SRGB,
        data,
    )
    .expect("valid histogram fixture");
    Histogram::from_display_rgb8(&image)
}

fn neutral_recipe() -> EditRecipe {
    let mut recipe = EditRecipe::default();
    recipe.rendering.profile = RenderingProfileSelection::NEUTRAL;
    recipe
}

fn standard_recipe() -> EditRecipe {
    EditRecipe::default()
}

fn median_code(recipe: &EditRecipe, scene: &[f32]) -> u8 {
    rendered_histogram(recipe, scene).luminance_percentile(0.5)
}

#[test]
fn black_image_has_no_usable_midtone() {
    let recipe = neutral_recipe();
    assert!(auto_tone(&recipe, &rendered_histogram(&recipe, &[0.0; 16])).is_none());
}

#[test]
fn dark_scene_rises_only_part_of_the_way_to_the_target() {
    let recipe = neutral_recipe();
    let scene = scene(0.01, 0.04, 0.3);
    let start = median_code(&recipe, &scene);
    let next = auto_tone(&recipe, &rendered_histogram(&recipe, &scene)).expect("midtone");
    let end = median_code(&next, &scene);
    assert!(next.light.exposure_ev > 0.0);
    assert!(
        f32::from(start) < f32::from(end) && f32::from(end) < TARGET_MEDIAN_CODE,
        "median moved from {start} to {end}"
    );
}

#[test]
fn bright_scene_falls_only_part_of_the_way_to_the_target() {
    let recipe = neutral_recipe();
    let scene = scene(0.05, 0.5, 0.8);
    let start = median_code(&recipe, &scene);
    let next = auto_tone(&recipe, &rendered_histogram(&recipe, &scene)).expect("midtone");
    let end = median_code(&next, &scene);
    assert!(next.light.exposure_ev < 0.0);
    assert!(
        TARGET_MEDIAN_CODE < f32::from(end) && end < start,
        "median moved from {start} to {end}"
    );
}

/// With a steep base curve a correction measured on the display would overshoot.
/// Half the stops to the target, solved through the curve, lands exactly.
#[test]
fn exposure_is_solved_through_the_base_rendering() {
    let recipe = standard_recipe();
    let scene = scene(0.005, 0.04, 0.2);
    let next = auto_tone(&recipe, &rendered_histogram(&recipe, &scene)).expect("midtone");

    let base_lut = base_rendering_lut(&recipe.rendering);
    let stages = ToneStages::new(base_lut.as_deref(), &recipe);
    let start_code = f32::from(median_code(&recipe, &scene));
    let measured_scene = stages.scene_luminance(start_code);
    let expected = MEDIAN_CORRECTION_STRENGTH
        * stops_between(measured_scene, stages.scene_luminance(TARGET_MEDIAN_CODE));
    assert!((next.light.exposure_ev - expected).abs() < 0.01);
}

#[test]
fn large_corrections_are_capped() {
    let recipe = neutral_recipe();
    let scene = scene(0.0005, 0.001, 0.002);
    let next = auto_tone(&recipe, &rendered_histogram(&recipe, &scene)).expect("midtone");
    assert_eq!(next.light.exposure_ev, MAX_EXPOSURE_CHANGE_EV);
}

#[test]
fn clipped_highlights_block_exposure_and_shadows_take_over() {
    let recipe = neutral_recipe();
    let scene = scene(0.01, 0.05, 1.0);
    let next = auto_tone(&recipe, &rendered_histogram(&recipe, &scene)).expect("midtone");
    assert_eq!(next.light.exposure_ev, 0.0);
    assert!(next.light.shadows > 0.0);
    assert!(next.light.highlights < 0.0);
    assert!(median_code(&next, &scene) > median_code(&recipe, &scene));
}

#[test]
fn exposure_stops_where_the_highlights_reach_the_ceiling() {
    let recipe = neutral_recipe();
    let scene = scene(0.01, 0.04, 0.7);
    let next = auto_tone(&recipe, &rendered_histogram(&recipe, &scene)).expect("midtone");
    let bright = linear_code(0.7 * next.light.exposure_ev.exp2());
    assert!(next.light.exposure_ev > 0.0);
    assert!(
        bright <= HIGHLIGHT_CEILING_CODE + 0.5,
        "bright code {bright}"
    );
}

fn linear_code(linear: f32) -> f32 {
    rohditor_color::linear_srgb_to_srgb(linear) * 255.0
}

#[test]
fn never_raises_highlights_or_lifts_shadows_on_a_bright_scene() {
    let recipe = neutral_recipe();
    for bright in [0.2, 0.3, 0.45] {
        let scene = scene(0.1, 0.18, bright);
        let next = auto_tone(&recipe, &rendered_histogram(&recipe, &scene)).expect("midtone");
        assert_eq!(next.light.highlights, 0.0);
        assert_eq!(next.light.shadows, 0.0);
    }
}

#[test]
fn black_point_rule_uses_the_black_level_after_exposure() {
    let recipe = neutral_recipe();
    // Below the raised-black range as shot, inside it once exposure lifts it.
    let scene = scene(0.008, 0.02, 0.2);
    let start = rendered_histogram(&recipe, &scene);
    assert!(f32::from(start.luminance_percentile(0.01)) < BLACKS_PULL_CODES.0);
    let next = auto_tone(&recipe, &start).expect("midtone");
    assert!(next.light.exposure_ev > 0.0);
    assert!(next.light.blacks < 0.0);
}

#[test]
fn crushed_blacks_are_never_lifted() {
    let recipe = neutral_recipe();
    let scene = scene(0.0, 0.1, 0.4);
    let next = auto_tone(&recipe, &rendered_histogram(&recipe, &scene)).expect("midtone");
    assert_eq!(next.light.blacks, 0.0);
}

/// A saturated colour close to clipping in one channel leaves luminance low.
#[test]
fn a_single_clipped_channel_blocks_exposure() {
    let mut data = Vec::new();
    for _ in 0..480 {
        data.extend([70, 70, 70]);
    }
    for _ in 0..20 {
        data.extend([255, 30, 30]);
    }
    let image = DisplayRgbImage::new(500, 1, 1500, DisplayColorSpace::SRGB, data)
        .expect("valid histogram fixture");
    let histogram = Histogram::from_display_rgb8(&image);
    assert!(histogram.luminance_percentile(0.99) < 200);
    let next = auto_tone(&neutral_recipe(), &histogram).expect("midtone");
    assert_eq!(next.light.exposure_ev, 0.0);
    assert!(next.light.shadows > 0.0);
}

#[test]
fn repeating_auto_changes_nothing() {
    let scenes = [
        scene(0.01, 0.04, 0.3),
        scene(0.05, 0.5, 0.8),
        scene(0.01, 0.05, 1.0),
        scene(0.03, 0.14, 0.6),
    ];
    for recipe in [neutral_recipe(), standard_recipe()] {
        for scene in &scenes {
            let first = auto_tone(&recipe, &rendered_histogram(&recipe, scene)).expect("midtone");
            let second = auto_tone(&first, &rendered_histogram(&first, scene)).expect("midtone");
            assert_eq!(first, second);
        }
    }
}

/// Exposure that just reaches clipping must survive a second press, even though
/// the first press saw no clipping and the second one does.
#[test]
fn exposure_that_reached_clipping_is_kept_on_the_next_press() {
    let image = |bright_red: u8| {
        let mut data = Vec::new();
        for _ in 0..480 {
            data.extend([60, 60, 60]);
        }
        for _ in 0..20 {
            data.extend([bright_red, 30, 30]);
        }
        let image = DisplayRgbImage::new(500, 1, 1500, DisplayColorSpace::SRGB, data)
            .expect("valid histogram fixture");
        Histogram::from_display_rgb8(&image)
    };
    let recipe = neutral_recipe();
    let first = auto_tone(&recipe, &image(120)).expect("midtone");
    assert!(first.light.exposure_ev > 0.0);
    let second = auto_tone(&first, &image(255)).expect("midtone");
    assert!(second.light.exposure_ev > 0.0);
    assert!(second.light.exposure_ev <= first.light.exposure_ev);
}

#[test]
fn result_does_not_depend_on_the_sliders_auto_sets() {
    let scene = scene(0.01, 0.04, 0.3);
    let plain = neutral_recipe();
    let expected = auto_tone(&plain, &rendered_histogram(&plain, &scene)).expect("midtone");

    let mut edited = neutral_recipe();
    edited.light.exposure_ev = 1.0;
    edited.light.shadows = 0.2;
    edited.light.highlights = 0.4;
    edited.light.blacks = -0.2;
    let next = auto_tone(&edited, &rendered_histogram(&edited, &scene)).expect("midtone");

    assert!((next.light.exposure_ev - expected.light.exposure_ev).abs() < 0.05);
    assert!((next.light.shadows - expected.light.shadows).abs() < 0.02);
    assert!((next.light.highlights - expected.light.highlights).abs() < 0.02);
    assert!((next.light.blacks - expected.light.blacks).abs() < 0.02);
}

#[test]
fn keeps_other_controls_and_accounts_for_them() {
    let scene = scene(0.01, 0.06, 0.3);
    let plain = neutral_recipe();
    let mut contrasted = neutral_recipe();
    contrasted.light.contrast = 0.5;
    let next = auto_tone(&contrasted, &rendered_histogram(&contrasted, &scene)).expect("midtone");
    assert_eq!(next.light.contrast, 0.5);
    // Contrast darkens this median, so the same scene needs more exposure.
    let plain_next = auto_tone(&plain, &rendered_histogram(&plain, &scene)).expect("midtone");
    assert!(next.light.exposure_ev > plain_next.light.exposure_ev);
}

#[test]
fn stays_finite_and_in_range_for_extreme_histograms() {
    for code in [1_u8, 8, 32, 240, 254, 255] {
        for recipe in [neutral_recipe(), standard_recipe()] {
            let image = DisplayRgbImage::new(8, 1, 24, DisplayColorSpace::SRGB, vec![code; 24])
                .expect("valid histogram fixture");
            let next = auto_tone(&recipe, &Histogram::from_display_rgb8(&image)).expect("midtone");
            assert!(EXPOSURE_EV_RANGE.contains(next.light.exposure_ev));
            assert!(SHADOWS_RANGE.contains(next.light.shadows));
            assert!(HIGHLIGHTS_RANGE.contains(next.light.highlights));
            assert!(BLACKS_RANGE.contains(next.light.blacks));
        }
    }
}
