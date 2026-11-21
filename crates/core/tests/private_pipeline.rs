use std::error::Error;
use std::path::{Path, PathBuf};

use rohditor_core::{
    CancellationToken, CpuPipeline, Histogram, OutputPolicy, PreviewOptions, RenderOptions,
    auto_tone, decode_camera_preview, estimate_noise_profile, match_camera_look,
    with_default_noise_reduction,
};
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_edit::{CameraProfileSelection, EditRecipe, RenderingProfileSelection};
use rohditor_image::Orientation;
use rohditor_look::{Proxy, compare_proxies};
use rohditor_raw::{RawDecoder, RawlerDecoder};

const SAMPLES: [&str; 6] = [
    "DSC00851.ARW",
    "DSC01166.ARW",
    "DSC02382.ARW",
    "DSC03270.ARW",
    "DSC03687.ARW",
    "DSC03821.ARW",
];
const PHASE_9_PREVIEW_PEAK_LIMIT_BYTES: usize = 600 * 1_024 * 1_024;

#[test]
#[ignore = "requires the ignored Sony ARW corpus in testdata/private"]
fn neutral_recipe_develops_every_private_sample_deterministically() -> Result<(), Box<dyn Error>> {
    let decoder = RawlerDecoder::default();
    let pipeline = CpuPipeline::default();
    let mut recipe = EditRecipe::default();
    recipe.rendering.profile = RenderingProfileSelection::NEUTRAL;
    let algorithms = [
        ("bilinear", DemosaicAlgorithm::Bilinear),
        ("mhc", DemosaicAlgorithm::MalvarHeCutler),
    ];

    for (algorithm_name, algorithm) in algorithms {
        let options = RenderOptions {
            demosaic: algorithm,
            ..RenderOptions::default()
        };
        let preview_options = PreviewOptions {
            render: options,
            ..PreviewOptions::default()
        };
        let mut first_hash = None;

        for (index, name) in SAMPLES.iter().enumerate() {
            let path = private_corpus_directory().join(name);
            let frame = decoder.decode(&path)?;
            let orientation = frame.info.orientation;
            let result = pipeline.render(&frame, &recipe, options)?;
            let expected_dimensions = if orientation == Orientation::Rotate270 {
                (4_000, 6_000)
            } else {
                (6_000, 4_000)
            };
            assert_eq!(
                (result.image.width(), result.image.height()),
                expected_dimensions,
                "{name} {algorithm_name}"
            );
            assert_eq!(
                result.image.data().len(),
                expected_dimensions.0 * expected_dimensions.1 * 3,
                "{name} {algorithm_name}"
            );
            let preview = pipeline.render_preview(&frame, &recipe, preview_options)?;
            assert!(
                preview.memory.estimated_peak_bytes < PHASE_9_PREVIEW_PEAK_LIMIT_BYTES,
                "{name} {algorithm_name} preview exceeded the Phase 9 transient-memory budget"
            );
            let expected_preview_dimensions = if orientation == Orientation::Rotate270 {
                (1_707, 2_560)
            } else {
                (2_560, 1_707)
            };
            assert_eq!(
                (preview.image.width(), preview.image.height()),
                expected_preview_dimensions,
                "{name} {algorithm_name} preview"
            );

            let statistics = statistics(result.image.data());
            assert!(
                statistics.maximum - statistics.minimum >= 128,
                "{name} {algorithm_name}"
            );
            assert!(
                statistics.mean > 5.0 && statistics.mean < 250.0,
                "{name} {algorithm_name}"
            );
            assert!(statistics.distinct_codes >= 128, "{name} {algorithm_name}");
            let hash = fnv1a64(result.image.data());
            if index == 0 {
                first_hash = Some(hash);
            }
            println!(
                "{name} {algorithm_name}: {}x{}, codes {}..{} ({} distinct), mean {:.2}, hash {hash:016x}, render {:.1} ms, 2560-edge preview {:.1} ms, full peak {} MiB, preview peak {} MiB",
                result.image.width(),
                result.image.height(),
                statistics.minimum,
                statistics.maximum,
                statistics.distinct_codes,
                statistics.mean,
                result.timings.total.as_secs_f64() * 1_000.0,
                preview.timings.total.as_secs_f64() * 1_000.0,
                result.memory.estimated_peak_bytes.div_ceil(1024 * 1024),
                preview.memory.estimated_peak_bytes.div_ceil(1024 * 1024),
            );
        }

        let first_path = private_corpus_directory().join(SAMPLES[0]);
        let repeated = pipeline.render(&decoder.decode(&first_path)?, &recipe, options)?;
        assert_eq!(first_hash, Some(fnv1a64(repeated.image.data())));
    }
    Ok(())
}

#[test]
#[ignore = "requires the ignored Sony ARW corpus in testdata/private"]
fn source_scale_inspection_meets_phase_9_latency_and_memory_budgets() -> Result<(), Box<dyn Error>>
{
    let frame = RawlerDecoder::default().decode(&private_corpus_directory().join(SAMPLES[0]))?;
    let mut recipe = EditRecipe::default();
    recipe.rendering.profile = RenderingProfileSelection::NEUTRAL;
    let result = CpuPipeline::default().render_source_scale_preview_cancellable(
        &frame,
        &recipe,
        RenderOptions::default(),
        &CancellationToken::new(),
    )?;
    assert_eq!(
        (result.image.width(), result.image.height()),
        (6_000, 4_000)
    );
    assert!(
        result.timings.total.as_secs_f64() < 2.0,
        "source-scale development took {:.3} s",
        result.timings.total.as_secs_f64()
    );
    assert!(
        result.memory.estimated_peak_bytes < PHASE_9_PREVIEW_PEAK_LIMIT_BYTES,
        "source-scale peak was {} MiB",
        result.memory.estimated_peak_bytes.div_ceil(1024 * 1024)
    );
    println!(
        "source-scale MHC: {:.1} ms, {} MiB peak",
        result.timings.total.as_secs_f64() * 1_000.0,
        result.memory.estimated_peak_bytes.div_ceil(1024 * 1024)
    );
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct PixelStatistics {
    minimum: u8,
    maximum: u8,
    mean: f64,
    distinct_codes: usize,
}

fn statistics(samples: &[u8]) -> PixelStatistics {
    let mut minimum = u8::MAX;
    let mut maximum = u8::MIN;
    let mut sum = 0_u64;
    let mut codes = [false; 256];
    for &sample in samples {
        minimum = minimum.min(sample);
        maximum = maximum.max(sample);
        sum += u64::from(sample);
        codes[usize::from(sample)] = true;
    }
    PixelStatistics {
        minimum,
        maximum,
        mean: sum as f64 / samples.len() as f64,
        distinct_codes: codes.into_iter().filter(|present| *present).count(),
    }
}

fn fnv1a64(samples: &[u8]) -> u64 {
    samples.iter().fold(0xcbf2_9ce4_8422_2325, |hash, sample| {
        (hash ^ u64::from(*sample)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn private_corpus_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/private")
}

/// The fitted camera tone must bring the render's brightness to the camera
/// JPEG's, where Standard sits clearly darker on this corpus.
#[test]
#[ignore = "requires the ignored Sony ARW corpus in testdata/private"]
fn fitted_camera_tone_matches_the_embedded_jpeg_brightness() -> Result<(), Box<dyn Error>> {
    let decoder = RawlerDecoder::default();
    let pipeline = CpuPipeline::default();
    let options = PreviewOptions {
        max_long_edge: 768,
        ..PreviewOptions::default()
    };

    for name in SAMPLES {
        let path = private_corpus_directory().join(name);
        let frame = decoder.decode(&path)?;
        let preview = decoder
            .embedded_preview(&path)?
            .ok_or_else(|| format!("{name} has no embedded preview"))?;
        let camera = decode_camera_preview(&preview, frame.info.orientation)?;
        let look = match_camera_look(
            &pipeline,
            &frame,
            &preview,
            &CameraProfileSelection::Automatic,
        )?;

        let mut bias = [0.0_f32; 2];
        for (slot, profile) in [
            RenderingProfileSelection::STANDARD,
            RenderingProfileSelection::MATCHED,
        ]
        .into_iter()
        .enumerate()
        {
            let mut recipe = EditRecipe::default();
            recipe.rendering.profile = profile;
            look.store_in(&mut recipe.rendering);
            let rendered = pipeline.render_preview(&frame, &recipe, options)?;
            let (proxy, camera_proxy) = Proxy::pair(&rendered.image, &camera)?;
            bias[slot] = compare_proxies(&proxy, &camera_proxy)
                .ok_or("proxy grids differ")?
                .luminance_bias;
        }
        assert!(
            bias[1].abs() < 0.02 && bias[1].abs() < bias[0].abs() / 3.0,
            "{name}: Standard bias {:+.3}, Matched bias {:+.3}",
            bias[0],
            bias[1]
        );
    }
    Ok(())
}

/// Auto tone refines the camera rendering: on real renders the median moves
/// towards the typical value without passing it, highlights do not clip more,
/// and a second press leaves the sliders alone.
#[test]
#[ignore = "requires the ignored Sony ARW corpus in testdata/private"]
fn auto_tone_refines_the_camera_rendering_of_every_private_sample() -> Result<(), Box<dyn Error>> {
    const TYPICAL_MEDIAN: i32 = 105;
    const SECOND_PRESS_TOLERANCE: f32 = 0.05;
    let clipped_share = |histogram: &Histogram| {
        let total = histogram.luminance.iter().sum::<u64>() as f64;
        histogram.highlight_clipped.into_iter().max().unwrap_or(0) as f64 / total
    };
    let decoder = RawlerDecoder::default();
    let pipeline = CpuPipeline::default();
    let options = PreviewOptions {
        max_long_edge: 1_024,
        ..PreviewOptions::default()
    };

    for name in SAMPLES {
        let path = private_corpus_directory().join(name);
        let frame = decoder.decode(&path)?;
        let preview = decoder
            .embedded_preview(&path)?
            .ok_or_else(|| format!("{name} has no embedded preview"))?;
        let look = match_camera_look(
            &pipeline,
            &frame,
            &preview,
            &CameraProfileSelection::Automatic,
        )?;
        let mut recipe = EditRecipe::default();
        recipe.rendering.profile = RenderingProfileSelection::MATCHED;
        look.store_in(&mut recipe.rendering);

        let base = pipeline.prepare_preview_base(&frame, &recipe, options)?;
        let histogram = |recipe: &EditRecipe| {
            pipeline
                .render_preview_from_base(&base, recipe, OutputPolicy::ClipToSrgb)
                .map(|result| result.histogram)
        };
        let start = histogram(&recipe)?;
        let first = auto_tone(&recipe, &start).ok_or("no usable midtone")?;
        let end = histogram(&first)?;
        let second = auto_tone(&first, &end).ok_or("no usable midtone")?;

        let start_median = i32::from(start.luminance_percentile(0.5));
        let end_median = i32::from(end.luminance_percentile(0.5));
        assert!(
            (end_median - TYPICAL_MEDIAN).abs() <= (start_median - TYPICAL_MEDIAN).abs()
                && (end_median - TYPICAL_MEDIAN).signum()
                    * (start_median - TYPICAL_MEDIAN).signum()
                    >= 0,
            "{name}: median moved from {start_median} to {end_median}"
        );
        assert!(
            clipped_share(&end) <= clipped_share(&start) + 0.005,
            "{name}: clipped share grew from {} to {}",
            clipped_share(&start),
            clipped_share(&end)
        );
        for (control, before, after) in [
            (
                "exposure",
                first.light.exposure_ev,
                second.light.exposure_ev,
            ),
            ("shadows", first.light.shadows, second.light.shadows),
            (
                "highlights",
                first.light.highlights,
                second.light.highlights,
            ),
            ("blacks", first.light.blacks, second.light.blacks),
        ] {
            assert!(
                (before - after).abs() < SECOND_PRESS_TOLERANCE,
                "{name}: second press moved {control} from {before} to {after}"
            );
        }
    }
    Ok(())
}

/// Chroma strength the derived default picks per corpus file, as recorded by
/// `rohditor-cli detail-corpus`. Base-ISO files must stay off so they keep the
/// GPU path.
#[test]
#[ignore = "requires the ignored Sony ARW corpus in testdata/private"]
fn default_noise_reduction_follows_each_private_files_noise() -> Result<(), Box<dyn Error>> {
    let decoder = RawlerDecoder::default();
    let pipeline = CpuPipeline::default();
    let mut recipe = EditRecipe::default();
    recipe.rendering.profile = RenderingProfileSelection::NEUTRAL;
    let options = RenderOptions {
        demosaic: DemosaicAlgorithm::Amaze,
        ..RenderOptions::default()
    };
    // (file, minimum, maximum) of the derived chroma strength.
    let expected = [
        ("DSC00851.ARW", 0.0, 0.0),
        ("DSC01166.ARW", 0.3, 0.4),
        ("DSC02382.ARW", 0.0, 0.0),
        ("DSC03270.ARW", 0.1, 0.25),
        ("DSC03687.ARW", 0.3, 0.4),
        ("DSC03821.ARW", 0.1, 0.25),
        ("DSC00554.ARW", 0.1, 0.25),
        ("DSC00850.ARW", 0.0, 0.0),
        ("DSC01109.ARW", 0.3, 0.4),
        ("DSC02616.ARW", 0.25, 0.4),
        ("DSC01787.ARW", 0.3, 0.4),
    ];
    for (name, minimum, maximum) in expected {
        let frame = decoder.decode(&private_corpus_directory().join(name))?;
        let profile = estimate_noise_profile(&pipeline, &frame, &recipe, options)?;
        let derived = with_default_noise_reduction(recipe.clone(), profile.as_ref());
        let chroma = derived.noise_reduction.chroma;
        assert!(
            (minimum..=maximum).contains(&chroma),
            "{name}: derived chroma {chroma}, expected {minimum}..={maximum}"
        );
        assert!(
            !derived.noise_reduction.luminance_active(),
            "{name}: luminance reduction must stay off"
        );
        // The derived recipe renders, and the chroma stage runs exactly when on.
        let rendered = pipeline.render(&frame, &derived, options)?;
        assert_eq!(
            !rendered.timings.chroma_noise_reduction.is_zero(),
            chroma > 0.0,
            "{name}"
        );
    }
    Ok(())
}

/// The neural model (`ROHDITOR_NEURAL_MODEL`, else the local
/// `ml/runs/shippable` export) on the GPU if available, or `None` to skip.
fn neural_model() -> Option<std::sync::Arc<rohditor_neural::NeuralDemosaicer>> {
    let path = std::env::var_os("ROHDITOR_NEURAL_MODEL").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ml/runs/shippable/export/model.onnx"),
        PathBuf::from,
    );
    if !path.exists() {
        eprintln!("no neural model at {}; skipping", path.display());
        return None;
    }
    let model = rohditor_neural::NeuralDemosaicer::load(&path, rohditor_neural::Backend::Gpu)
        .expect("the model loads");
    Some(std::sync::Arc::new(model))
}

/// Neural development of real photos is deterministic and keeps AMaZE's
/// overall brightness. Per-pixel differences are only reported: at high ISO
/// they are mostly the noise the network removes.
#[test]
#[ignore = "requires the ignored Sony ARW corpus in testdata/private and the neural model"]
fn neural_demosaic_develops_private_samples_deterministically() -> Result<(), Box<dyn Error>> {
    let Some(model) = neural_model() else {
        return Ok(());
    };
    let decoder = RawlerDecoder::default();
    let pipeline = CpuPipeline::default().with_neural(model);
    let mut recipe = EditRecipe::default();
    recipe.rendering.profile = RenderingProfileSelection::NEUTRAL;
    let neural = RenderOptions {
        demosaic: DemosaicAlgorithm::Neural,
        ..RenderOptions::default()
    };
    let amaze = RenderOptions {
        demosaic: DemosaicAlgorithm::Amaze,
        ..RenderOptions::default()
    };
    for name in &SAMPLES[..2] {
        let frame = decoder.decode(&private_corpus_directory().join(name))?;
        let first = pipeline.render(&frame, &recipe, neural)?;
        let second = pipeline.render(&frame, &recipe, neural)?;
        assert_eq!(
            first.image.data(),
            second.image.data(),
            "{name} is deterministic"
        );
        let classic = pipeline.render(&frame, &recipe, amaze)?;
        let mean =
            |data: &[u8]| data.iter().map(|&v| f64::from(v)).sum::<f64>() / data.len() as f64;
        let mean_difference = first
            .image
            .data()
            .iter()
            .zip(classic.image.data())
            .map(|(&a, &b)| (f64::from(a) - f64::from(b)).abs())
            .sum::<f64>()
            / first.image.data().len() as f64;
        eprintln!(
            "{name}: neural mean {:.2}, AMaZE mean {:.2}, mean |difference| {mean_difference:.2} (8-bit), demosaic {:?}",
            mean(first.image.data()),
            mean(classic.image.data()),
            first.timings.demosaic
        );
        assert!(
            (mean(first.image.data()) - mean(classic.image.data())).abs() < 1.0,
            "{name}"
        );
    }
    Ok(())
}
