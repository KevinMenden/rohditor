//! `highlight-compare`: render every RAW highlight method on identical crops.
//!
//! This produces the evidence required before the highlight default can change:
//! per-method reconstruction counts, scratch bytes, wall time, and 100% crops
//! with their difference from Clip, which stays the reference default.

use std::fs;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use rohditor_core::{
    CpuPipeline, DitherMode, ExportFormat, ExportImage, ExportMetadataPolicy, ExportSettings,
    HighlightDiagnostics, OutputBitDepth, OutputPolicy, OutputSize, PngBitDepth, RawCropPolicy,
    RenderOptions, export_image, write_output_bytes,
};
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_edit::{EditRecipe, HighlightMethod, RenderingProfileSelection};
use rohditor_image::{DisplayRgbImage, Orientation};
use rohditor_raw::{RawDecoder, RawlerDecoder};
use serde::Serialize;

use super::commands::{
    QualityCropSpec, crop_display_image, load_quality_manifest, milliseconds,
    source_stem_for_artifact, write_stdout,
};

/// Methods in report order; Clip is the comparison reference.
const METHODS: [HighlightMethod; 4] = [
    HighlightMethod::Off,
    HighlightMethod::Clip,
    HighlightMethod::LocalRatios,
    HighlightMethod::Opposed,
];

/// One method's render, reduced to its report fields and the manifest crops.
struct RenderedMethod {
    method: &'static str,
    counts: SiteCounts,
    highlight_ms: f64,
    pipeline_total_ms: f64,
    highlight_scratch_bytes: usize,
    estimated_peak_bytes: usize,
    full_export: Option<String>,
    cropped: Vec<DisplayRgbImage<u8>>,
}

const fn method_name(method: HighlightMethod) -> &'static str {
    match method {
        HighlightMethod::Off => "off",
        HighlightMethod::Clip => "clip",
        HighlightMethod::LocalRatios => "local-ratios",
        HighlightMethod::Opposed => "opposed",
    }
}

/// Mean and maximum absolute 8-bit channel difference between equal-size crops.
fn difference(a: &DisplayRgbImage<u8>, b: &DisplayRgbImage<u8>) -> (f64, u8) {
    let row_bytes = a.width() * 3;
    let mut sum = 0_u64;
    let mut max = 0_u8;
    for y in 0..a.height() {
        let row_a = &a.data()[y * a.row_stride()..][..row_bytes];
        let row_b = &b.data()[y * b.row_stride()..][..row_bytes];
        for (&left, &right) in row_a.iter().zip(row_b) {
            let delta = left.abs_diff(right);
            sum += u64::from(delta);
            max = max.max(delta);
        }
    }
    (sum as f64 / (row_bytes * a.height()) as f64, max)
}

#[derive(Debug, Serialize)]
struct HighlightCompareReport {
    schema_version: u32,
    pipeline_version: &'static str,
    demosaic: &'static str,
    manifest: String,
    sources: Vec<SourceReport>,
}

#[derive(Debug, Serialize)]
struct SourceReport {
    source: String,
    decoded_dimensions: [usize; 2],
    methods: Vec<MethodReport>,
}

#[derive(Debug, Serialize)]
struct MethodReport {
    method: &'static str,
    counts: SiteCounts,
    highlight_ms: f64,
    pipeline_total_ms: f64,
    highlight_scratch_bytes: usize,
    estimated_peak_bytes: usize,
    full_export: Option<String>,
    crops: Vec<CropReport>,
}

/// Whole-frame site counts. Fields a method does not produce are `None`.
#[derive(Debug, Default, Serialize)]
struct SiteCounts {
    suspected_or_affected: Option<usize>,
    reconstructed: Option<usize>,
    changed: Option<usize>,
    fallback: Option<usize>,
    fully_unsupported: Option<usize>,
}

#[derive(Debug, Serialize)]
struct CropReport {
    name: String,
    category: String,
    coordinates: [usize; 4],
    output_100_percent: String,
    /// Mean/max absolute 8-bit channel difference from the Clip crop.
    mean_abs_diff_vs_clip: f64,
    max_abs_diff_vs_clip: u8,
}

impl From<HighlightDiagnostics> for SiteCounts {
    fn from(value: HighlightDiagnostics) -> Self {
        match value {
            HighlightDiagnostics::Off => Self::default(),
            HighlightDiagnostics::Clip(stats) => Self {
                suspected_or_affected: Some(stats.affected_sites),
                changed: Some(stats.changed_sites),
                ..Self::default()
            },
            HighlightDiagnostics::LocalRatios(stats) => Self {
                suspected_or_affected: Some(stats.suspected_clipped_sites),
                reconstructed: Some(stats.reconstructed_sites),
                changed: Some(stats.changed_sites),
                fallback: Some(stats.fallback_sites),
                fully_unsupported: Some(stats.fully_unsupported_sites),
            },
            HighlightDiagnostics::Opposed(stats) => Self {
                suspected_or_affected: Some(stats.suspected_clipped_sites),
                reconstructed: Some(stats.reconstructed_sites),
                changed: Some(stats.changed_sites),
                fallback: Some(stats.fallback_sites),
                fully_unsupported: Some(stats.fully_unsupported_sites),
            },
        }
    }
}

pub(super) fn highlight_compare(
    manifest_path: &Path,
    corpus: &Path,
    output: &Path,
    demosaic: DemosaicAlgorithm,
    full_exports: bool,
    force: bool,
) -> Result<()> {
    let manifest = load_quality_manifest(manifest_path)?;
    fs::create_dir_all(output)
        .with_context(|| format!("could not create output directory {}", output.display()))?;
    let report_path = output.join("report.json");
    if !force && report_path.exists() {
        anyhow::bail!(
            "output {} already exists; pass --force to replace the comparison",
            report_path.display()
        );
    }

    let render_options = RenderOptions {
        raw_crop_policy: RawCropPolicy::Recommended,
        demosaic,
        output_policy: OutputPolicy::ClipToSrgb,
    };
    let png = ExportSettings {
        format: ExportFormat::Png {
            bit_depth: PngBitDepth::Eight,
        },
        dithering: DitherMode::None,
        metadata: ExportMetadataPolicy::None,
        overwrite: force,
        size: OutputSize::Original,
    };
    let decoder = RawlerDecoder::default();
    let mut sources = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for requested in &manifest.crops {
        if !seen.insert(requested.source.clone()) {
            continue;
        }
        let crops: Vec<&QualityCropSpec> = manifest
            .crops
            .iter()
            .filter(|crop| crop.source == requested.source)
            .collect();
        let frame = decoder
            .decode(&corpus.join(&requested.source))
            .with_context(|| format!("could not decode private source {}", requested.source))?;
        let stem = source_stem_for_artifact(&requested.source)?;

        // Render one method at a time so only one full-resolution image is
        // alive; the (small) crops are kept for the Clip comparison.
        let mut rendered_methods = Vec::new();
        for method in METHODS {
            let mut recipe = EditRecipe::default();
            recipe.rendering.profile = RenderingProfileSelection::NEUTRAL;
            recipe.geometry.orientation_override = Some(Orientation::Normal);
            recipe.raw.highlights.method = method;

            let started = Instant::now();
            let rendered = CpuPipeline::default()
                .render_export(
                    &frame,
                    &recipe,
                    render_options,
                    OutputBitDepth::Eight,
                    DitherMode::None,
                    OutputSize::Original,
                )
                .with_context(|| {
                    format!("could not develop {} with {method:?}", requested.source)
                })?;
            let wall = started.elapsed();
            let ExportImage::Rgb8(image) = &rendered.image else {
                unreachable!("highlight comparison explicitly requests 8-bit output");
            };
            let cropped = crops
                .iter()
                .map(|crop| crop_display_image(image, crop))
                .collect::<Result<Vec<_>>>()?;
            let method_name = method_name(method);
            let full_export = if full_exports {
                let name = format!("{stem}--full--{method_name}.png");
                export_image(&output.join(&name), &rendered.image, &frame.info, png)?;
                Some(name)
            } else {
                None
            };
            rendered_methods.push(RenderedMethod {
                method: method_name,
                counts: rendered.highlight_diagnostics.into(),
                highlight_ms: milliseconds(rendered.timings.highlight_processing),
                pipeline_total_ms: milliseconds(wall),
                highlight_scratch_bytes: rendered.memory.highlight_scratch_bytes,
                estimated_peak_bytes: rendered.memory.estimated_peak_bytes,
                full_export,
                cropped,
            });
        }

        let clip_crops = rendered_methods
            .iter()
            .find(|rendered| rendered.method == method_name(HighlightMethod::Clip))
            .expect("Clip is always compared")
            .cropped
            .clone();
        let mut methods = Vec::new();
        for rendered in rendered_methods {
            let mut crop_reports = Vec::new();
            for (index, crop) in crops.iter().enumerate() {
                let name = format!(
                    "{stem}--{}--highlight-{}--100.png",
                    crop.name, rendered.method
                );
                export_image(
                    &output.join(&name),
                    &ExportImage::Rgb8(rendered.cropped[index].clone()),
                    &frame.info,
                    png,
                )?;
                let (mean, max) = difference(&rendered.cropped[index], &clip_crops[index]);
                crop_reports.push(CropReport {
                    name: crop.name.clone(),
                    category: crop.category.clone(),
                    coordinates: [crop.x, crop.y, crop.width, crop.height],
                    output_100_percent: name,
                    mean_abs_diff_vs_clip: mean,
                    max_abs_diff_vs_clip: max,
                });
            }
            methods.push(MethodReport {
                method: rendered.method,
                counts: rendered.counts,
                highlight_ms: rendered.highlight_ms,
                pipeline_total_ms: rendered.pipeline_total_ms,
                highlight_scratch_bytes: rendered.highlight_scratch_bytes,
                estimated_peak_bytes: rendered.estimated_peak_bytes,
                full_export: rendered.full_export,
                crops: crop_reports,
            });
        }
        sources.push(SourceReport {
            source: requested.source.clone(),
            decoded_dimensions: [frame.info.width, frame.info.height],
            methods,
        });
    }

    let report = HighlightCompareReport {
        schema_version: 1,
        pipeline_version: env!("CARGO_PKG_VERSION"),
        demosaic: demosaic.stable_name(),
        manifest: manifest_path.display().to_string(),
        sources,
    };
    let json = serde_json::to_vec_pretty(&report).context("could not encode comparison report")?;
    write_output_bytes(&report_path, &json, force)
        .with_context(|| format!("could not commit {}", report_path.display()))?;
    write_stdout(&format!(
        "Compared {} highlight methods on {} source(s) in {}",
        METHODS.len(),
        report.sources.len(),
        output.display()
    ))
}
