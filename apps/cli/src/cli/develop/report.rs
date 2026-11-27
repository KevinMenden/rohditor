//! Text formatting for the `develop` summary.

use std::time::Duration;

use rohditor_core::{ExportFormat, StageTimings};
use rohditor_edit::{LensProfileSelection, OpticsAdjustments};

pub(super) fn format_optics_report(
    adjustments: &OpticsAdjustments,
    provenance: Option<&rohditor_core::OpticsProvenance>,
) -> String {
    let Some(provenance) = provenance else {
        return "Lens correction: off".to_owned();
    };
    format!(
        "Lens profile: {} [{}] ({})\nOptics components: requested {} applied {}\nOptics database: {} {} (fingerprint {:016x})\nOptics fallback distance: {}\nOptics correction scale: {:.6}",
        provenance.profile.lens,
        provenance.profile.id,
        match adjustments.profile {
            LensProfileSelection::Automatic => "automatic",
            LensProfileSelection::Lensfun { .. } => "explicit",
            LensProfileSelection::Off => "off",
        },
        format_components(provenance.requested),
        format_components(provenance.applied),
        provenance.database.source,
        provenance.database.snapshot,
        provenance.database.fingerprint,
        if provenance.used_infinity_distance_fallback {
            "1000 m (infinity fallback)"
        } else {
            "capture distance"
        },
        provenance.scale,
    )
}

fn format_components(components: rohditor_core::CorrectionComponents) -> String {
    format!(
        "distortion={}, vignetting={}, tca={}",
        components.distortion, components.vignetting, components.chromatic_aberration
    )
}

pub(super) fn format_quality(format: ExportFormat) -> String {
    match format {
        ExportFormat::Jpeg { quality } => format!(" (quality {quality})"),
        ExportFormat::Png { .. } => String::new(),
    }
}

pub(super) fn format_stage_timings(
    decode: Duration,
    timings: StageTimings,
    encode: Duration,
) -> String {
    format!(
        "CPU stages: decode {:.1} ms, metadata {:.1} ms, normalize {:.1} ms, highlight processing {:.1} ms, demosaic {:.1} ms, false colour {:.1} ms, chroma NR {:.1} ms, luminance NR {:.1} ms, capture sharpening {:.1} ms, optics {:.1} ms, defringe {:.1} ms, area resize {:.1} ms, color {:.1} ms, adjustments {:.1} ms, output {:.1} ms, pipeline total {:.1} ms, export encode/commit {:.1} ms",
        decode.as_secs_f64() * 1_000.0,
        timings.metadata.as_secs_f64() * 1_000.0,
        timings.normalization.as_secs_f64() * 1_000.0,
        timings.highlight_processing.as_secs_f64() * 1_000.0,
        timings.demosaic.as_secs_f64() * 1_000.0,
        timings.false_color.as_secs_f64() * 1_000.0,
        timings.chroma_noise_reduction.as_secs_f64() * 1_000.0,
        timings.luminance_noise_reduction.as_secs_f64() * 1_000.0,
        timings.capture_sharpening.as_secs_f64() * 1_000.0,
        timings.optics.as_secs_f64() * 1_000.0,
        timings.defringe.as_secs_f64() * 1_000.0,
        timings.resampling.as_secs_f64() * 1_000.0,
        timings.color_conversion.as_secs_f64() * 1_000.0,
        timings.adjustments.as_secs_f64() * 1_000.0,
        timings.output_conversion.as_secs_f64() * 1_000.0,
        timings.total.as_secs_f64() * 1_000.0,
        encode.as_secs_f64() * 1_000.0,
    )
}

pub(super) fn bytes_to_mib(bytes: usize) -> usize {
    bytes.div_ceil(1024 * 1024)
}
