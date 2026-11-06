//! Tests for luminance and colour range components in the adjustment stage.
//!
//! The recipes use neutral rendering unless a test is about the base rendering, so a local edit
//! is the only change to the pixels and "which pixels did the mask select" can be read off by
//! comparing a render with and without the layer.

use rohditor_color::rec2020_to_oklab;
use rohditor_edit::{
    CameraChroma, CameraTone, ColorRange, ColorSample, EditRecipe, LuminanceRange, MaskComponent,
    MaskLayer, MaskMode, MaskShape, RenderingProfileSelection,
};
use rohditor_image::{LinearRgbImage, Orientation};

use super::local_tests::{
    HEIGHT, WIDTH, adjusted, flat_image, image_of, layer, linear, neutral_recipe, pixel,
    varied_image,
};
use super::mask_overlay::{render_mask_overlay_from_image, render_mask_overlay_preview};
use super::stages::adjustments::ReferenceChain;
use rohditor_edit::LocalSettings;

/// One EV of local exposure: any pixel the layer selects changes visibly.
fn exposure() -> LocalSettings {
    LocalSettings {
        exposure_ev: 1.0,
        ..LocalSettings::default()
    }
}

fn bright_range() -> MaskShape {
    MaskShape::Luminance(LuminanceRange {
        low: 0.55,
        high: 1.0,
        feather_low: 0.0,
        feather_high: 0.0,
    })
}

fn with_components(
    id: u32,
    components: Vec<(MaskMode, MaskShape)>,
    settings: LocalSettings,
) -> MaskLayer {
    let mut layer = layer(id, components[0].1.clone(), settings);
    layer.components = components
        .into_iter()
        .map(|(mode, shape)| MaskComponent {
            mode,
            invert: false,
            shape,
        })
        .collect();
    layer
}

/// Indices of the pixels `with` changed compared to `without` (same globals, one layer fewer).
fn selected(image: &LinearRgbImage<f32>, with: &EditRecipe, without: &EditRecipe) -> Vec<usize> {
    let (with, without) = (adjusted(image, with), adjusted(image, without));
    (0..WIDTH * HEIGHT)
        .filter(|index| with[index * 3..index * 3 + 3] != without[index * 3..index * 3 + 3])
        .collect()
}

/// An edit to everything but the range layer, to check the selection does not move.
type GlobalEdit<'a> = &'a dyn Fn(&mut EditRecipe);

/// Dark left half, bright right half, constant down each column.
fn split_image() -> LinearRgbImage<f32> {
    image_of(|x, _| if x < WIDTH / 2 { [0.02; 3] } else { [0.5; 3] })
}

#[test]
fn a_luminance_range_edits_only_the_bright_pixels() {
    let image = split_image();
    let mut recipe = neutral_recipe();
    recipe.local.layers.push(with_components(
        0,
        vec![(MaskMode::Add, bright_range())],
        exposure(),
    ));
    let result = adjusted(&image, &recipe);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let expected = if x < WIDTH / 2 { 0.02 } else { 1.0 };
            let got = pixel(&result, x, y);
            for channel in got {
                assert!((channel - expected).abs() < 1e-5, "{x},{y}: {got:?}");
            }
        }
    }
}

#[test]
fn the_range_is_measured_after_the_global_exposure() {
    // 0.1 linear is about 0.35 encoded: outside [0.55, 1] until the global exposure lifts it.
    let image = flat_image(0.1);
    let layered = |global_ev: f32| {
        let mut recipe = neutral_recipe();
        recipe.light.exposure_ev = global_ev;
        let without = recipe.clone();
        recipe.local.layers.push(with_components(
            0,
            vec![(MaskMode::Add, bright_range())],
            exposure(),
        ));
        (recipe, without)
    };
    let (recipe, without) = layered(0.0);
    assert!(selected(&image, &recipe, &without).is_empty());
    let (recipe, without) = layered(2.0);
    assert_eq!(selected(&image, &recipe, &without).len(), WIDTH * HEIGHT);
}

#[test]
fn the_reference_ignores_light_color_and_other_masks() {
    let image = varied_image();
    let range_layer = with_components(0, vec![(MaskMode::Add, bright_range())], exposure());
    let selection = |edit: GlobalEdit| {
        let mut without = neutral_recipe();
        edit(&mut without);
        let mut with = without.clone();
        with.local.layers.push(range_layer.clone());
        selected(&image, &with, &without)
    };
    let baseline = selection(&|_| {});
    // The test image straddles the range, so this is a real selection.
    assert!(baseline.len() > 10 && baseline.len() < WIDTH * HEIGHT - 10);

    let others: [(&str, GlobalEdit); 4] = [
        ("contrast", &|recipe| recipe.light.contrast = 0.6),
        ("shadows and highlights", &|recipe| {
            recipe.light.shadows = 0.7;
            recipe.light.highlights = -0.5;
        }),
        ("hsl and saturation", &|recipe| {
            recipe.color.hsl.channels[0].saturation = -0.8;
            recipe.color.hsl.channels[2].luminance = 0.5;
            recipe.color.saturation = 1.4;
        }),
        ("another mask", &|recipe| {
            recipe.local.layers.push(layer(
                9,
                linear(0.1, 0.9),
                LocalSettings {
                    exposure_ev: -1.5,
                    contrast: 0.5,
                    saturation: 0.6,
                    ..LocalSettings::default()
                },
            ));
        }),
    ];
    for (name, edit) in others {
        assert_eq!(selection(edit), baseline, "{name} moved the selection");
    }
}

#[test]
fn a_gradient_intersected_with_a_range_selects_only_where_both_do() {
    // Bright on the right, gradient covers the left 70 %: only the bright part of it remains.
    let image = split_image();
    let mut both = neutral_recipe();
    both.local.layers.push(with_components(
        0,
        vec![
            (MaskMode::Add, linear(0.6, 0.75)),
            (MaskMode::Intersect, bright_range()),
        ],
        exposure(),
    ));
    let selected_columns: std::collections::BTreeSet<usize> =
        selected(&image, &both, &neutral_recipe())
            .into_iter()
            .map(|index| index % WIDTH)
            .collect();
    // Bright columns are x >= 11; the gradient is full strength left of 0.6 * 23 = 13.8.
    assert!(selected_columns.iter().all(|x| *x >= WIDTH / 2));
    assert!(selected_columns.contains(&(WIDTH / 2)));
    assert!(!selected_columns.contains(&(WIDTH - 1)));
}

#[test]
fn a_color_range_selects_a_hue_and_leaves_the_others() {
    let red = [0.5, 0.02, 0.02];
    let blue = [0.02, 0.05, 0.5];
    let image = image_of(|x, _| if x < WIDTH / 2 { red } else { blue });
    let mut recipe = neutral_recipe();
    let [lightness, a, b] = rec2020_to_oklab(red);
    recipe.local.layers.push(with_components(
        0,
        vec![(
            MaskMode::Add,
            MaskShape::Color(ColorRange {
                samples: vec![ColorSample { lightness, a, b }],
                tolerance: 0.4,
            }),
        )],
        exposure(),
    ));
    let columns: std::collections::BTreeSet<usize> = selected(&image, &recipe, &neutral_recipe())
        .into_iter()
        .map(|index| index % WIDTH)
        .collect();
    assert_eq!(columns, (0..WIDTH / 2).collect());
}

/// Dark to far above white, and colourful, so the tone curve, its highlight roll-off and the
/// camera chroma all have something to do.
fn wide_range_image() -> LinearRgbImage<f32> {
    image_of(|x, y| {
        let level = 0.01 * 1.5f32.powi(((x * 7 + y * 13) % 17) as i32);
        [level, level * 0.7 + 0.01, level * 0.4 + 0.02]
    })
}

/// One recipe per kind of base rendering: none, the standard curve, and a matched camera look
/// with its own tone and chroma.
fn recipes_of_every_rendering_profile() -> Vec<(&'static str, EditRecipe)> {
    let mut matched = EditRecipe::default();
    matched.rendering.profile = RenderingProfileSelection::MATCHED;
    matched.rendering.camera_tone = Some(CameraTone {
        knots: vec![[0.0, 0.0], [0.2, 0.3], [0.6, 0.75], [1.0, 1.0]],
    });
    matched.rendering.camera_chroma = Some(CameraChroma {
        gains: [2.0, 1.8, 1.6, 1.4, 1.2, 1.0, 0.8, 0.6, 0.5],
    });
    vec![
        ("neutral", neutral_recipe()),
        ("standard", EditRecipe::default()),
        ("matched with chroma", matched),
    ]
}

/// The range reference duplicates only the exposure of the render loop; the base rendering is
/// shared code. This pins the two together: with Light and Color neutral and no local layers,
/// the stage's output is exactly the reference pixel, whatever the profile and exposure.
#[test]
fn the_reference_chain_matches_what_the_stage_does_without_light_color_or_local_edits() {
    let image = wide_range_image();
    for (name, base) in recipes_of_every_rendering_profile() {
        for exposure_ev in [-1.5, 0.0, 0.7, 2.0] {
            let mut recipe = base.clone();
            recipe.light.exposure_ev = exposure_ev;
            let chain = ReferenceChain::new(&recipe);
            let result = adjusted(&image, &recipe);
            let mut rendering_changed_something = false;
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    let source = pixel(image.data(), x, y);
                    let mut expected = source;
                    chain.apply(&mut expected);
                    assert_eq!(
                        pixel(&result, x, y),
                        expected,
                        "{name} at {exposure_ev} EV, pixel {x},{y}"
                    );
                    let exposure_only = source.map(|v| v * exposure_ev.exp2());
                    rendering_changed_something |= exposure_only != expected;
                }
            }
            // The comparison is not vacuous: a profile with rendering moves pixels beyond the
            // exposure gain.
            assert_eq!(rendering_changed_something, name != "neutral", "{name}");
        }
    }
}

#[test]
fn the_overlay_of_a_range_needs_the_image_and_shows_the_selection() {
    let image = split_image();
    let mut recipe = neutral_recipe();
    recipe.local.layers.push(with_components(
        4,
        vec![(MaskMode::Add, bright_range())],
        exposure(),
    ));

    // Without an image there is nothing to measure.
    assert!(
        render_mask_overlay_preview(&recipe, 4, (WIDTH, HEIGHT), Orientation::Normal, 100).is_err()
    );

    let overlay = render_mask_overlay_from_image(&image, Orientation::Normal, &recipe, 4)
        .expect("renders")
        .expect("layer exists");
    assert_eq!((overlay.width(), overlay.height()), (WIDTH, HEIGHT));
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let expected = if x < WIDTH / 2 { 0 } else { 255 };
            assert_eq!(overlay.data()[y * WIDTH + x], expected, "{x},{y}");
        }
    }

    // Other layers of a recipe still render from an image, and a missing layer is `None`.
    assert!(
        render_mask_overlay_from_image(&image, Orientation::Normal, &recipe, 99)
            .expect("renders")
            .is_none()
    );
}

#[test]
fn a_range_component_adds_reference_scratch_to_the_working_set_estimate() {
    let mut recipe = neutral_recipe();
    recipe
        .local
        .layers
        .push(layer(0, linear(0.2, 0.8), exposure()));
    let plain = super::local_stage::LocalStage::prepare(&recipe, 1000, 50, &|_| None)
        .expect("prepares")
        .expect("active")
        .bytes();
    recipe.local.layers[0].components.push(MaskComponent {
        mode: MaskMode::Intersect,
        invert: false,
        shape: MaskShape::Color(ColorRange::default()),
    });
    let ranged = super::local_stage::LocalStage::prepare(&recipe, 1000, 50, &|_| None)
        .expect("prepares")
        .expect("active")
        .bytes();
    assert!(ranged > plain);
}
