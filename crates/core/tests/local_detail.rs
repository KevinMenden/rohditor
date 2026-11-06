//! Local detail settings through the whole CPU pipeline: inside a mask the image is the one the
//! global setting would give, outside it is untouched, in the preview and the export alike.

use std::error::Error;
use std::sync::Arc;

use rohditor_core::{
    CPU_WORKING_SET_LIMIT_BYTES, CpuPipeline, DitherMode, ExportImage, OutputBitDepth, OutputSize,
    PipelineError, PreviewOptions, RenderOptions, local_detail_variants,
};
use rohditor_edit::{
    EditRecipe, LinearGradient, LocalDetail, LocalSettings, MaskComponent, MaskLayer, MaskMode,
    MaskPoint, MaskShape,
};
use rohditor_image::{DisplayRgbImage, Orientation};
use rohditor_raw::{
    CameraColorMatrix, CameraMatrixOrigin, CaptureMetadata, CfaPattern, ImageRect, LevelPattern,
    PhotometricInterpretation, RawFileInfo, RawFrame,
};

const WIDTH: usize = 256;
const HEIGHT: usize = 192;

fn frame() -> RawFrame {
    frame_of(WIDTH, HEIGHT)
}

/// Fine structure and pseudo-random noise, so every detail filter changes something.
fn frame_of(width: usize, height: usize) -> RawFrame {
    let mut state = 0x2545_f491_u32;
    let samples: Vec<u16> = (0..width * height)
        .map(|index| {
            let (x, y) = (index % width, index / width);
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let noise = (state % 41) as f32 - 20.0;
            let pattern = if (x / 3 + y / 5) % 2 == 0 {
                150.0
            } else {
                -150.0
            };
            (500.0 + pattern + noise) as u16
        })
        .collect();
    RawFrame {
        info: RawFileInfo {
            format: "synthetic".to_owned(),
            make: "Rohditor".to_owned(),
            model: "Detail fixture".to_owned(),
            clean_make: "Rohditor".to_owned(),
            clean_model: "Detail fixture".to_owned(),
            source_size_bytes: 0,
            source_identity: None,
            width,
            height,
            components_per_pixel: 1,
            source_bits_per_sample: Some(16),
            decoded_bits_per_sample: 16,
            compression: None,
            active_area: Some(ImageRect {
                x: 0,
                y: 0,
                width,
                height,
            }),
            crop_area: None,
            photometric_interpretation: PhotometricInterpretation::Cfa {
                pattern: CfaPattern {
                    name: "RGGB".to_owned(),
                    width: 2,
                    height: 2,
                },
            },
            black_levels: LevelPattern {
                values: vec![0.0; 4],
                repeat_width: 2,
                repeat_height: 2,
                components_per_pixel: 1,
            },
            white_levels: vec![1000.0; 4],
            as_shot_white_balance: [Some(1.0), Some(1.0), Some(1.0), None],
            xyz_to_camera: [[0.0; 3]; 4],
            color_matrices: vec![CameraColorMatrix {
                illuminant: "D65".to_owned(),
                values: vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
                origin: CameraMatrixOrigin::DecoderDatabase,
            }],
            orientation: Orientation::Normal,
            capture: CaptureMetadata::default(),
            embedded_preview: None,
        },
        row_stride: width,
        mosaic: Arc::from(samples),
    }
}

/// Fully masks x < 0.4, not at all x > 0.6.
fn layer(id: u32, detail: LocalDetail) -> MaskLayer {
    MaskLayer {
        id,
        name: format!("Layer {id}"),
        enabled: true,
        opacity: 1.0,
        components: vec![MaskComponent {
            mode: MaskMode::Add,
            invert: false,
            shape: MaskShape::Linear(LinearGradient {
                start: MaskPoint { x: 0.4, y: 0.5 },
                end: MaskPoint { x: 0.6, y: 0.5 },
            }),
        }],
        settings: LocalSettings {
            detail,
            ..LocalSettings::default()
        },
    }
}

fn with_layer(mut recipe: EditRecipe, detail: LocalDetail) -> EditRecipe {
    recipe.local.layers.push(layer(0, detail));
    recipe
}

fn export(frame: &RawFrame, recipe: &EditRecipe) -> Result<DisplayRgbImage<u16>, PipelineError> {
    let result = CpuPipeline::default().render_export(
        frame,
        recipe,
        RenderOptions::default(),
        OutputBitDepth::Sixteen,
        DitherMode::None,
        OutputSize::Original,
        None,
    )?;
    match result.image {
        ExportImage::Rgb16(image) => Ok(image),
        ExportImage::Rgb8(_) => unreachable!("sixteen bits were requested"),
    }
}

/// Asserts the columns `x` of the two images are bit-identical.
fn assert_same_columns<T: PartialEq + Copy + std::fmt::Debug>(
    a: &[T],
    b: &[T],
    width: usize,
    columns: std::ops::Range<usize>,
) {
    assert_eq!(a.len(), b.len());
    for (row_a, row_b) in a.chunks_exact(width * 3).zip(b.chunks_exact(width * 3)) {
        assert_eq!(
            &row_a[columns.start * 3..columns.end * 3],
            &row_b[columns.start * 3..columns.end * 3]
        );
    }
}

fn columns_differ<T: PartialEq>(a: &[T], b: &[T], width: usize, x: usize) -> bool {
    a.chunks_exact(width * 3)
        .zip(b.chunks_exact(width * 3))
        .any(|(a, b)| a[x * 3..x * 3 + 3] != b[x * 3..x * 3 + 3])
}

/// One local setting and the global recipe that should look the same inside the mask.
fn cases() -> Vec<(&'static str, LocalDetail, EditRecipe)> {
    let mut chroma = EditRecipe::default();
    chroma.noise_reduction.chroma = 0.8;
    let mut luminance = EditRecipe::default();
    luminance.noise_reduction.luminance = 0.8;
    let mut false_color = EditRecipe::default();
    false_color.false_color.steps = 3;
    let mut sharpening = EditRecipe::default();
    sharpening.capture_sharpening.enabled = true;
    sharpening.capture_sharpening.amount = 0.8;
    vec![
        (
            "colour noise",
            LocalDetail {
                chroma_noise: 0.8,
                ..LocalDetail::default()
            },
            chroma,
        ),
        (
            "luminance noise",
            LocalDetail {
                luminance_noise: 0.8,
                ..LocalDetail::default()
            },
            luminance,
        ),
        (
            "moiré",
            LocalDetail {
                false_color_steps: 3,
                ..LocalDetail::default()
            },
            false_color,
        ),
        (
            "sharpening",
            LocalDetail {
                sharpening: 0.8,
                ..LocalDetail::default()
            },
            sharpening,
        ),
    ]
}

#[test]
fn export_matches_the_global_setting_inside_the_mask_and_no_setting_outside()
-> Result<(), Box<dyn Error>> {
    let frame = frame();
    let plain = export(&frame, &EditRecipe::default())?;
    for (name, detail, global) in cases() {
        let masked = export(&frame, &with_layer(EditRecipe::default(), detail))?;
        let everywhere = export(&frame, &global)?;
        assert!(
            columns_differ(plain.data(), everywhere.data(), WIDTH, 10),
            "{name}: the fixture must react to this filter"
        );
        // Fully inside the mask: exactly the global result.
        assert_same_columns(masked.data(), everywhere.data(), WIDTH, 0..WIDTH * 2 / 5);
        // Fully outside: untouched.
        assert_same_columns(masked.data(), plain.data(), WIDTH, WIDTH * 3 / 5 + 1..WIDTH);
    }
    Ok(())
}

#[test]
fn the_fit_preview_matches_the_global_setting_inside_the_mask_too() -> Result<(), Box<dyn Error>> {
    let frame = frame();
    let pipeline = CpuPipeline::default();
    let options = PreviewOptions {
        max_long_edge: WIDTH / 2,
        ..PreviewOptions::default()
    };
    let plain = pipeline.render_preview(&frame, &EditRecipe::default(), options)?;
    let width = plain.image.width();
    for (name, detail, global) in cases() {
        let masked =
            pipeline.render_preview(&frame, &with_layer(EditRecipe::default(), detail), options)?;
        let everywhere = pipeline.render_preview(&frame, &global, options)?;
        assert_same_columns(
            masked.image.data(),
            everywhere.image.data(),
            width,
            0..width * 2 / 5 - 1,
        );
        assert_same_columns(
            masked.image.data(),
            plain.image.data(),
            width,
            width * 3 / 5 + 2..width,
        );
        assert!(
            masked.timings.local_detail > std::time::Duration::ZERO,
            "{name}"
        );
    }
    Ok(())
}

#[test]
fn a_mask_can_take_the_global_filter_away() -> Result<(), Box<dyn Error>> {
    let frame = frame();
    let mut global = EditRecipe::default();
    global.noise_reduction.luminance = 0.8;
    let masked = export(
        &frame,
        &with_layer(
            global.clone(),
            LocalDetail {
                luminance_noise: -1.0,
                ..LocalDetail::default()
            },
        ),
    )?;
    assert_same_columns(
        masked.data(),
        export(&frame, &EditRecipe::default())?.data(),
        WIDTH,
        0..WIDTH * 2 / 5,
    );
    assert_same_columns(
        masked.data(),
        export(&frame, &global)?.data(),
        WIDTH,
        WIDTH * 3 / 5 + 1..WIDTH,
    );
    Ok(())
}

#[test]
fn only_settings_that_change_the_work_need_a_copy() {
    let detail = LocalDetail {
        luminance_noise: 0.5,
        ..LocalDetail::default()
    };
    let mut recipe = EditRecipe::default();
    assert!(local_detail_variants(&recipe).is_empty());

    recipe.local.layers.push(layer(0, detail));
    assert_eq!(local_detail_variants(&recipe).len(), 1);

    // The same setting in another layer shares the copy.
    recipe.local.layers.push(layer(1, detail));
    assert_eq!(local_detail_variants(&recipe).len(), 1);

    recipe.local.layers.push(layer(
        2,
        LocalDetail {
            luminance_noise: 0.25,
            ..LocalDetail::default()
        },
    ));
    assert_eq!(local_detail_variants(&recipe).len(), 2);

    // Disabled layers and offsets that clamp to the global setting need none.
    recipe.local.layers[2].enabled = false;
    assert_eq!(local_detail_variants(&recipe).len(), 1);
    recipe.noise_reduction.luminance = 1.0;
    assert!(local_detail_variants(&recipe).is_empty());
}

#[test]
fn a_base_without_the_copies_is_refused_rather_than_rendered_without_the_edit()
-> Result<(), Box<dyn Error>> {
    let frame = frame();
    let pipeline = CpuPipeline::default();
    let recipe = with_layer(
        EditRecipe::default(),
        LocalDetail {
            luminance_noise: 0.5,
            ..LocalDetail::default()
        },
    );
    let options = PreviewOptions::default();
    let reconstructed = pipeline.prepare_preview_reconstruction(&frame, &recipe, options)?;
    let base = pipeline.prepare_preview_base_from_reconstruction(&reconstructed, &recipe)?;
    assert!(matches!(
        pipeline.render_preview_from_base(&base, &recipe, options.render.output_policy),
        Err(PipelineError::InvalidRecipe { .. })
    ));
    Ok(())
}

#[test]
fn working_set_counts_the_copies() -> Result<(), Box<dyn Error>> {
    let frame = frame();
    let pipeline = CpuPipeline::default();
    let options = PreviewOptions::default();
    let bytes = |recipe: &EditRecipe| -> Result<usize, PipelineError> {
        let result = pipeline.render_preview(&frame, recipe, options)?;
        assert!(result.memory.estimated_peak_bytes < CPU_WORKING_SET_LIMIT_BYTES);
        Ok(result.memory.local_adjustment_bytes)
    };
    let shape_only = bytes(&with_layer(
        EditRecipe::default(),
        LocalDetail {
            sharpening: -1.0,
            ..LocalDetail::default()
        },
    ))?;
    let one_copy = bytes(&with_layer(
        EditRecipe::default(),
        LocalDetail {
            sharpening: 0.5,
            ..LocalDetail::default()
        },
    ))?;
    assert_eq!(one_copy - shape_only, WIDTH * HEIGHT * 3 * size_of::<f32>());
    Ok(())
}

#[test]
fn an_export_that_cannot_hold_its_copies_fails_before_allocating_them() {
    // 48 MP with two copies: 2.3 GiB at the peak, over the 2 GiB limit.
    let frame = frame_of(8_000, 6_000);
    let mut recipe = with_layer(
        EditRecipe::default(),
        LocalDetail {
            luminance_noise: 0.5,
            ..LocalDetail::default()
        },
    );
    recipe.local.layers.push(layer(
        1,
        LocalDetail {
            luminance_noise: 0.25,
            ..LocalDetail::default()
        },
    ));
    assert!(matches!(
        export(&frame, &recipe),
        Err(PipelineError::WorkingSetLimit { .. })
    ));
}

/// Quality and cost on a real photo. Prints the numbers a PR should report.
#[test]
#[ignore = "requires the ignored Sony ARW corpus in testdata/private"]
fn private_local_noise_reduction_costs_and_matches() -> Result<(), Box<dyn Error>> {
    use std::time::Instant;

    use rohditor_raw::{RawDecoder, RawlerDecoder};

    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/private/DSC00851.ARW");
    if !source.is_file() {
        eprintln!("skipping: {} is absent", source.display());
        return Ok(());
    }
    let frame = RawlerDecoder::default().open(&source)?.decode()?;
    println!("{}x{}", frame.info.width, frame.info.height);
    let pipeline = CpuPipeline::default();
    let options = PreviewOptions::default();
    let detail = LocalDetail {
        luminance_noise: 0.6,
        chroma_noise: 0.5,
        ..LocalDetail::default()
    };
    let recipe = with_layer(EditRecipe::default(), detail);
    let mut global = EditRecipe::default();
    global.noise_reduction.luminance = 0.6;
    global.noise_reduction.chroma = 0.5;

    // The fit preview: developing with a copy, then what an edit of the mask's shape costs.
    let started = Instant::now();
    let plain_base = pipeline.prepare_preview_base(&frame, &EditRecipe::default(), options)?;
    println!("fit base, no copy: {:?}", started.elapsed());
    let started = Instant::now();
    let base = pipeline.prepare_preview_base(&frame, &recipe, options)?;
    println!(
        "fit base, one copy: {:?} (copy {:?})",
        started.elapsed(),
        base.timings().local_detail
    );
    let started = Instant::now();
    let unmasked = pipeline.render_preview_from_base(
        &plain_base,
        &EditRecipe::default(),
        options.render.output_policy,
    )?;
    println!("fit render, no mask: {:?}", started.elapsed());
    let mut moved = recipe.clone();
    if let MaskShape::Linear(gradient) = &mut moved.local.layers[0].components[0].shape {
        gradient.end.x = 0.7;
    }
    let started = Instant::now();
    let masked = pipeline.render_preview_from_base(&base, &moved, options.render.output_policy)?;
    println!(
        "fit render, shape edit with blend: {:?}, {} bytes of copies and masks",
        started.elapsed(),
        masked.memory.local_adjustment_bytes,
    );
    assert_ne!(unmasked.image.data(), masked.image.data());

    // Full resolution: exact against the global setting inside the mask, and against none outside.
    let started = Instant::now();
    let export_masked = export(&frame, &recipe)?;
    println!("export with one copy: {:?}", started.elapsed());
    let started = Instant::now();
    let export_global = export(&frame, &global)?;
    println!("export, global setting: {:?}", started.elapsed());
    let export_plain = export(&frame, &EditRecipe::default())?;
    let width = export_masked.width();
    assert_same_columns(
        export_masked.data(),
        export_global.data(),
        width,
        0..width * 2 / 5 - 1,
    );
    assert_same_columns(
        export_masked.data(),
        export_plain.data(),
        width,
        width * 3 / 5 + 1..width,
    );
    assert!(columns_differ(
        export_plain.data(),
        export_global.data(),
        width,
        10
    ));

    // Three copies, the most a recipe can have.
    let mut three = with_layer(EditRecipe::default(), detail);
    for (id, luminance) in [(1, 0.3), (2, 0.9)] {
        three.local.layers.push(layer(
            id,
            LocalDetail {
                luminance_noise: luminance,
                ..LocalDetail::default()
            },
        ));
    }
    let started = Instant::now();
    let result = pipeline.render_export(
        &frame,
        &three,
        RenderOptions::default(),
        OutputBitDepth::Sixteen,
        DitherMode::None,
        OutputSize::Original,
        None,
    )?;
    println!(
        "export with three copies: {:?}, estimated peak {} MiB",
        started.elapsed(),
        result.memory.estimated_peak_bytes / (1024 * 1024)
    );
    Ok(())
}
