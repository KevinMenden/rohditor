//! End-to-end preview reconstruction at full A6400 sensor size.
//!
//! The interactive preview develops the whole sensor before reducing it to
//! 2560 px, so these numbers (not the preview-sized stage benches) show what
//! opening a RAW costs. Each variant prints its per-stage breakdown once before
//! Criterion measures the total.

mod common;

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use common::{must, raw_frame};
use criterion::{Criterion, criterion_group, criterion_main};
use rohditor_core::{CpuPipeline, PreviewOptions, RenderOptions, StageTimings};
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_edit::{CaptureSharpening, EditRecipe, LensProfileSelection};
use rohditor_optics::OpticsService;
use rohditor_raw::{RationalValue, RawFrame};

const SENSOR_WIDTH: usize = 6_048;
const SENSOR_HEIGHT: usize = 4_024;

fn benchmark_full_preview(criterion: &mut Criterion) {
    let frame = lensed_frame();
    let pipeline = CpuPipeline::new(Arc::new(must(OpticsService::load_bundled())));
    let mut group = criterion.benchmark_group("full_sensor_preview");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(60));

    for demosaic in [DemosaicAlgorithm::MalvarHeCutler, DemosaicAlgorithm::Amaze] {
        let options = PreviewOptions {
            render: RenderOptions {
                demosaic,
                ..RenderOptions::default()
            },
            ..PreviewOptions::default()
        };
        for (variant, optics, sharpening) in [
            ("baseline", false, false),
            ("optics", true, false),
            ("sharpening", false, true),
            ("optics_sharpening", true, true),
        ] {
            let recipe = recipe(optics, sharpening);
            let name = format!("{demosaic:?}/{variant}");
            let timings =
                must(pipeline.prepare_preview_reconstruction(&frame, &recipe, options)).timings();
            eprintln!("{name}: {}", describe(timings));
            group.bench_function(&name, |bencher| {
                bencher.iter(|| {
                    must(pipeline.prepare_preview_reconstruction(
                        black_box(&frame),
                        black_box(&recipe),
                        options,
                    ))
                });
            });
        }
    }
    group.finish();
}

/// Sensor frame whose EXIF matches a bundled Lensfun profile.
fn lensed_frame() -> RawFrame {
    let mut frame = raw_frame(SENSOR_WIDTH, SENSOR_HEIGHT);
    let info = &mut frame.info;
    "Sony".clone_into(&mut info.make);
    "Sony".clone_into(&mut info.clean_make);
    "ILCE-6400".clone_into(&mut info.model);
    "ILCE-6400".clone_into(&mut info.clean_model);
    let rational = |numerator| {
        Some(RationalValue {
            numerator,
            denominator: 1,
        })
    };
    info.capture.lens_make = Some("Tamron".to_owned());
    info.capture.lens_model = Some("Tamron 17-70mm F/2.8 Di III-A VC RXD".to_owned());
    info.capture.focal_length = rational(35);
    info.capture.aperture = rational(4);
    info.capture.focus_distance = rational(10);
    frame
}

fn recipe(optics: bool, sharpening: bool) -> EditRecipe {
    let mut recipe = EditRecipe::default();
    if optics {
        recipe.optics.profile = LensProfileSelection::Automatic;
    }
    recipe.capture_sharpening = CaptureSharpening {
        enabled: sharpening,
        ..CaptureSharpening::default()
    };
    recipe
}

fn describe(timings: StageTimings) -> String {
    let ms = |duration: Duration| duration.as_secs_f64() * 1_000.0;
    format!(
        "normalize {:.0} ms, highlight {:.0} ms, demosaic {:.0} ms, sharpening {:.0} ms, \
         optics {:.0} ms, resample {:.0} ms, total {:.0} ms",
        ms(timings.normalization),
        ms(timings.highlight_processing),
        ms(timings.demosaic),
        ms(timings.capture_sharpening),
        ms(timings.optics),
        ms(timings.resampling),
        ms(timings.total),
    )
}

criterion_group!(benches, benchmark_full_preview);
criterion_main!(benches);
