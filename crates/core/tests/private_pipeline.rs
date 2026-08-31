use std::error::Error;
use std::path::{Path, PathBuf};

use rohditor_core::{CpuPipeline, DemosaicAlgorithm, EditRecipe, PreviewOptions, RenderOptions};
use rohditor_raw::{RawDecoder, RawOrientation, RawlerDecoder};

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
    let pipeline = CpuPipeline;
    let recipe = EditRecipe::default();
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
            let expected_dimensions = if orientation == RawOrientation::Rotate270 {
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
            let expected_preview_dimensions = if orientation == RawOrientation::Rotate270 {
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
