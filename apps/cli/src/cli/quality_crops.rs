//! The `quality-crops` subcommand and the crop manifest shared with the corpus reports.

use std::fs;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rohditor_core::{
    CollisionPolicy, CpuPipeline, DitherMode, ExportFormat, ExportImage, ExportMetadataPolicy,
    ExportSettings, OutputPolicy, OutputSize, PngBitDepth, RawCropPolicy, RenderOptions,
    StageTimings, export_image, write_output_bytes,
};
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_edit::{EditRecipe, RenderingProfileSelection};
use rohditor_image::{DisplayColorSpace, DisplayRgbImage, Orientation};
use rohditor_raw::{RawDecoder, RawlerDecoder};
use serde::{Deserialize, Serialize};

use super::output::{milliseconds, write_stdout};
use super::values::CliDemosaic;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QualityCropManifest {
    schema_version: u32,
    pub(super) crops: Vec<QualityCropSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QualityCropSpec {
    pub(super) source: String,
    pub(super) name: String,
    pub(super) category: String,
    pub(super) x: usize,
    pub(super) y: usize,
    pub(super) width: usize,
    pub(super) height: usize,
}

#[derive(Debug, Serialize)]
struct QualityCropReport {
    schema_version: u32,
    pipeline_version: &'static str,
    recipe: &'static str,
    crop_policy: &'static str,
    orientation: &'static str,
    algorithm: &'static str,
    reconstruction_policy: &'static str,
    manifest: String,
    sources: Vec<QualitySourceReport>,
    crops: Vec<QualityCropArtifact>,
}

#[derive(Debug, Serialize)]
struct QualitySourceReport {
    source: String,
    source_identity: Option<rohditor_raw::SourceIdentity>,
    source_bits_per_sample: Option<usize>,
    decoded_dimensions: [usize; 2],
    developed_dimensions: [usize; 2],
    decode_ms: f64,
    timings_ms: QualityTimingReport,
    estimated_peak_bytes: usize,
}

#[derive(Debug, Serialize)]
struct QualityTimingReport {
    metadata: f64,
    normalization: f64,
    highlight_processing: f64,
    demosaic: f64,
    false_color: f64,
    chroma_noise_reduction: f64,
    luminance_noise_reduction: f64,
    capture_sharpening: f64,
    optics: f64,
    defringe: f64,
    resampling: f64,
    color_conversion: f64,
    adjustments: f64,
    output_conversion: f64,
    total: f64,
}

impl From<StageTimings> for QualityTimingReport {
    fn from(value: StageTimings) -> Self {
        Self {
            metadata: milliseconds(value.metadata),
            normalization: milliseconds(value.normalization),
            highlight_processing: milliseconds(value.highlight_processing),
            demosaic: milliseconds(value.demosaic),
            false_color: milliseconds(value.false_color),
            chroma_noise_reduction: milliseconds(value.chroma_noise_reduction),
            luminance_noise_reduction: milliseconds(value.luminance_noise_reduction),
            capture_sharpening: milliseconds(value.capture_sharpening),
            optics: milliseconds(value.optics),
            defringe: milliseconds(value.defringe),
            resampling: milliseconds(value.resampling),
            color_conversion: milliseconds(value.color_conversion),
            adjustments: milliseconds(value.adjustments),
            output_conversion: milliseconds(value.output_conversion),
            total: milliseconds(value.total),
        }
    }
}

#[derive(Debug, Serialize)]
struct QualityCropArtifact {
    source: String,
    name: String,
    category: String,
    coordinates: [usize; 4],
    output_100_percent: String,
    output_200_percent: String,
}

/// Read, version-check, and validate a quality-crop manifest.
pub(super) fn load_quality_manifest(manifest_path: &Path) -> Result<QualityCropManifest> {
    let manifest_bytes = fs::read(manifest_path)
        .with_context(|| format!("could not read crop manifest {}", manifest_path.display()))?;
    let manifest: QualityCropManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("could not parse crop manifest {}", manifest_path.display()))?;
    if manifest.schema_version != 1 {
        bail!(
            "unsupported quality-crop manifest schema {}; expected 1",
            manifest.schema_version
        );
    }
    if manifest.crops.is_empty() {
        bail!("quality-crop manifest contains no crops");
    }
    for crop in &manifest.crops {
        validate_quality_crop_spec(crop)?;
    }
    Ok(manifest)
}

pub(super) fn quality_crops(
    manifest_path: &Path,
    corpus: &Path,
    output: &Path,
    demosaic: CliDemosaic,
    force: bool,
) -> Result<()> {
    let manifest = load_quality_manifest(manifest_path)?;

    fs::create_dir_all(output).with_context(|| {
        format!(
            "could not create crop output directory {}",
            output.display()
        )
    })?;
    let report_path = output.join("report.json");
    if !force && report_path.exists() {
        bail!(
            "output {} already exists; pass --force to replace the crop set",
            report_path.display()
        );
    }

    let decoder = RawlerDecoder::default();
    let algorithm: DemosaicAlgorithm = demosaic.into();
    let render_options = RenderOptions {
        raw_crop_policy: RawCropPolicy::Recommended,
        demosaic: algorithm,
        output_policy: OutputPolicy::ClipToSrgb,
    };
    let mut recipe = EditRecipe::default();
    recipe.rendering.profile = RenderingProfileSelection::NEUTRAL;
    recipe.geometry.orientation_override = Some(Orientation::Normal);
    let settings = ExportSettings {
        format: ExportFormat::Png {
            bit_depth: PngBitDepth::Eight,
        },
        dithering: DitherMode::None,
        metadata: ExportMetadataPolicy::None,
        collision: CollisionPolicy::overwrite_if(force),
        size: OutputSize::Original,
        sharpening: None,
    };
    let mut source_reports = Vec::new();
    let mut artifacts = Vec::new();
    let mut rendered_sources = std::collections::HashSet::new();

    for requested in &manifest.crops {
        if !rendered_sources.insert(requested.source.clone()) {
            continue;
        }
        let source_path = corpus.join(&requested.source);
        let decode_started = Instant::now();
        let frame = decoder.decode(&source_path).with_context(|| {
            format!(
                "could not decode private quality source {}",
                source_path.display()
            )
        })?;
        let decode_time = decode_started.elapsed();
        let rendered = CpuPipeline::default()
            .render_export(
                &frame,
                &recipe,
                render_options,
                rohditor_core::OutputBitDepth::Eight,
                DitherMode::None,
                OutputSize::Original,
                None,
            )
            .with_context(|| format!("could not develop quality source {}", requested.source))?;
        let image = match &rendered.image {
            ExportImage::Rgb8(image) => image,
            ExportImage::Rgb16(_) => unreachable!("quality crops explicitly request 8-bit output"),
        };

        for crop in manifest
            .crops
            .iter()
            .filter(|candidate| candidate.source == requested.source)
        {
            let cropped = crop_display_image(image, crop)
                .with_context(|| format!("invalid crop {} for {}", crop.name, crop.source))?;
            let enlarged = nearest_neighbor_2x(&cropped)?;
            let stem = source_stem_for_artifact(&crop.source)?;
            let base = format!("{stem}--{}--{}", crop.name, algorithm.stable_name());
            let name_100 = format!("{base}--100.png");
            let name_200 = format!("{base}--200.png");
            export_image(
                &output.join(&name_100),
                &ExportImage::Rgb8(cropped),
                &frame.info,
                settings,
            )?;
            export_image(
                &output.join(&name_200),
                &ExportImage::Rgb8(enlarged),
                &frame.info,
                settings,
            )?;
            artifacts.push(QualityCropArtifact {
                source: crop.source.clone(),
                name: crop.name.clone(),
                category: crop.category.clone(),
                coordinates: [crop.x, crop.y, crop.width, crop.height],
                output_100_percent: name_100,
                output_200_percent: name_200,
            });
        }

        source_reports.push(QualitySourceReport {
            source: requested.source.clone(),
            source_identity: frame.info.source_identity,
            source_bits_per_sample: frame.info.source_bits_per_sample,
            decoded_dimensions: [frame.info.width, frame.info.height],
            developed_dimensions: [rendered.image.width(), rendered.image.height()],
            decode_ms: milliseconds(decode_time),
            timings_ms: rendered.timings.into(),
            estimated_peak_bytes: rendered.memory.estimated_peak_bytes,
        });
    }

    let report = QualityCropReport {
        schema_version: 1,
        pipeline_version: env!("CARGO_PKG_VERSION"),
        recipe: "neutral-as-shot-v1",
        crop_policy: "recommended",
        orientation: "unrotated-sensor-crop",
        algorithm: algorithm.stable_name(),
        reconstruction_policy: "full-crop-demosaic-no-preview-resampling",
        manifest: manifest_path.display().to_string(),
        sources: source_reports,
        crops: artifacts,
    };
    let report_json = serde_json::to_vec_pretty(&report).context("could not encode crop report")?;
    write_output_bytes(&report_path, &report_json, force)
        .with_context(|| format!("could not commit crop report {}", report_path.display()))?;
    write_stdout(&format!(
        "Generated {} named crop pairs from {} source(s) with {} demosaic in {}",
        report.crops.len(),
        report.sources.len(),
        algorithm.stable_name(),
        output.display()
    ))
}

fn validate_quality_crop_spec(crop: &QualityCropSpec) -> Result<()> {
    let source = Path::new(&crop.source);
    if source.file_name().and_then(|name| name.to_str()) != Some(crop.source.as_str()) {
        bail!(
            "quality source must be one plain file name: {:?}",
            crop.source
        );
    }
    if crop.name.is_empty()
        || !crop
            .name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!(
            "crop name {:?} must use lowercase ASCII letters, digits, and hyphens",
            crop.name
        );
    }
    if crop.category.trim().is_empty() {
        bail!("crop {} has an empty quality category", crop.name);
    }
    if crop.width == 0 || crop.height == 0 {
        bail!("crop {} must have non-zero dimensions", crop.name);
    }
    Ok(())
}

pub(super) fn source_stem_for_artifact(source: &str) -> Result<&str> {
    Path::new(source)
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("quality source {source:?} has no UTF-8 file stem"))
}

pub(super) fn crop_display_image(
    image: &DisplayRgbImage<u8>,
    crop: &QualityCropSpec,
) -> Result<DisplayRgbImage<u8>> {
    let end_x = crop
        .x
        .checked_add(crop.width)
        .context("crop x overflowed")?;
    let end_y = crop
        .y
        .checked_add(crop.height)
        .context("crop y overflowed")?;
    if end_x > image.width() || end_y > image.height() {
        bail!(
            "crop [{}, {}, {}, {}] exceeds developed image {}x{}",
            crop.x,
            crop.y,
            crop.width,
            crop.height,
            image.width(),
            image.height()
        );
    }
    let row_stride = crop
        .width
        .checked_mul(3)
        .context("crop stride overflowed")?;
    let mut pixels = Vec::with_capacity(
        row_stride
            .checked_mul(crop.height)
            .context("crop size overflowed")?,
    );
    for y in crop.y..end_y {
        let start = y * image.row_stride() + crop.x * 3;
        pixels.extend_from_slice(&image.data()[start..start + row_stride]);
    }
    Ok(DisplayRgbImage::new(
        crop.width,
        crop.height,
        row_stride,
        image.color_space(),
        pixels,
    )?)
}

fn nearest_neighbor_2x(image: &DisplayRgbImage<u8>) -> Result<DisplayRgbImage<u8>> {
    let width = image
        .width()
        .checked_mul(2)
        .context("200% width overflowed")?;
    let height = image
        .height()
        .checked_mul(2)
        .context("200% height overflowed")?;
    let row_stride = width.checked_mul(3).context("200% stride overflowed")?;
    let mut pixels = Vec::with_capacity(
        row_stride
            .checked_mul(height)
            .context("200% size overflowed")?,
    );
    for y in 0..image.height() {
        let source_row =
            &image.data()[y * image.row_stride()..y * image.row_stride() + image.width() * 3];
        for _ in 0..2 {
            for pixel in source_row.as_chunks::<3>().0 {
                pixels.extend_from_slice(pixel);
                pixels.extend_from_slice(pixel);
            }
        }
    }
    Ok(DisplayRgbImage::new(
        width,
        height,
        row_stride,
        DisplayColorSpace::SRGB,
        pixels,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_crop_and_pixel_enlargement_preserve_exact_samples() {
        let image = DisplayRgbImage::new(
            3,
            2,
            9,
            DisplayColorSpace::SRGB,
            vec![
                1, 2, 3, 4, 5, 6, 7, 8, 9, // row 0
                10, 11, 12, 13, 14, 15, 16, 17, 18, // row 1
            ],
        )
        .expect("valid display image");
        let spec = QualityCropSpec {
            source: "sample.ARW".to_owned(),
            name: "detail".to_owned(),
            category: "fine detail".to_owned(),
            x: 1,
            y: 0,
            width: 2,
            height: 2,
        };
        let crop = crop_display_image(&image, &spec).expect("crop succeeds");
        assert_eq!(crop.data(), &[4, 5, 6, 7, 8, 9, 13, 14, 15, 16, 17, 18]);
        let enlarged = nearest_neighbor_2x(&crop).expect("enlargement succeeds");
        assert_eq!((enlarged.width(), enlarged.height()), (4, 4));
        assert_eq!(enlarged.pixel(0, 0), enlarged.pixel(1, 0));
        assert_eq!(enlarged.pixel(0, 0), enlarged.pixel(0, 1));
        assert_eq!(enlarged.pixel(2, 2), Some(&[16, 17, 18][..]));
    }
}
