//! `detail-corpus`: render a grid of detail settings on real RAW files.
//!
//! `detail-compare` scores stages on synthetic ground truth; this command shows
//! how they behave on the private corpus. For every manifest crop it writes a
//! 1:1 sheet with the untouched render on the left and one preset per row on
//! the right, and the report records each file's ISO, estimated `NoiseProfile`
//! and the per-stage timings of every preset on the full frame.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rohditor_core::{
    CpuPipeline, DitherMode, ExportFormat, ExportImage, ExportMetadataPolicy, ExportSettings,
    OutputBitDepth, OutputPolicy, OutputSize, PngBitDepth, RawCropPolicy, RenderOptions,
    StageTimings, estimate_noise_profile, export_image, with_default_noise_reduction,
    write_output_bytes,
};
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_detail::NoiseProfile;
use rohditor_edit::{
    CaptureSharpening, Defringe, EditRecipe, FalseColorSuppression, NoiseReduction,
    RenderingProfileSelection,
};
use rohditor_image::{DisplayRgbImage, Orientation};
use rohditor_raw::{RawDecoder, RawlerDecoder};
use serde::Serialize;

use super::commands::{
    QualityCropSpec, crop_display_image, load_quality_manifest, milliseconds,
    source_stem_for_artifact, write_stdout,
};

/// Gap between the two columns of a sheet, in pixels.
const SHEET_GAP: usize = 8;
const GAP_GREY: u8 = 128;

/// One detail configuration under review.
struct Preset {
    name: &'static str,
    apply: fn(&mut EditRecipe),
}

fn noise(chroma: f32, luminance: f32) -> NoiseReduction {
    NoiseReduction {
        chroma,
        luminance,
        ..NoiseReduction::default()
    }
}

fn sharpen(recipe: &mut EditRecipe) {
    recipe.capture_sharpening = CaptureSharpening {
        enabled: true,
        ..CaptureSharpening::default()
    };
}

/// Name of the row the corpus-derived default adds after [`PRESETS`]: the
/// base recipe with `with_default_noise_reduction` for the file's own profile.
const DERIVED_DEFAULT: &str = "derived-default";

/// The first preset is the untouched baseline every other row is compared to.
const PRESETS: &[Preset] = &[
    Preset {
        name: "off",
        apply: |_| {},
    },
    Preset {
        name: "chroma-25",
        apply: |r| r.noise_reduction = noise(0.25, 0.0),
    },
    Preset {
        name: "chroma-50",
        apply: |r| r.noise_reduction = noise(0.5, 0.0),
    },
    Preset {
        name: "chroma-100",
        apply: |r| r.noise_reduction = noise(1.0, 0.0),
    },
    Preset {
        name: "luma-25",
        apply: |r| r.noise_reduction = noise(0.0, 0.25),
    },
    Preset {
        name: "luma-50",
        apply: |r| r.noise_reduction = noise(0.0, 0.5),
    },
    Preset {
        name: "chroma-25-luma-25",
        apply: |r| r.noise_reduction = noise(0.25, 0.25),
    },
    Preset {
        name: "false-color-2",
        apply: |r| r.false_color = FalseColorSuppression { steps: 2 },
    },
    Preset {
        name: "defringe",
        apply: |r| {
            r.defringe = Defringe {
                purple_amount: 1.0,
                green_amount: 1.0,
                ..Defringe::default()
            }
        },
    },
    Preset {
        name: "sharpen",
        apply: sharpen,
    },
    Preset {
        name: "sharpen-no-protection",
        apply: |r| {
            sharpen(r);
            r.capture_sharpening.noise_protection = 0.0;
        },
    },
    Preset {
        name: "chroma-25-sharpen",
        apply: |r| {
            r.noise_reduction = noise(0.25, 0.0);
            sharpen(r);
        },
    },
    Preset {
        name: "chroma-25-luma-25-sharpen",
        apply: |r| {
            r.noise_reduction = noise(0.25, 0.25);
            sharpen(r);
        },
    },
    Preset {
        name: "luma-25-sharpen-no-protection",
        apply: |r| {
            r.noise_reduction = noise(0.0, 0.25);
            sharpen(r);
            r.capture_sharpening.noise_protection = 0.0;
        },
    },
    Preset {
        name: "luma-50-sharpen-no-protection",
        apply: |r| {
            r.noise_reduction = noise(0.0, 0.5);
            sharpen(r);
            r.capture_sharpening.noise_protection = 0.0;
        },
    },
    Preset {
        name: "luma-50-sharpen",
        apply: |r| {
            r.noise_reduction = noise(0.0, 0.5);
            sharpen(r);
        },
    },
    Preset {
        name: "chroma-25-false-color-2",
        apply: |r| {
            r.noise_reduction = noise(0.25, 0.0);
            r.false_color = FalseColorSuppression { steps: 2 };
        },
    },
    Preset {
        name: "everything",
        apply: |r| {
            r.noise_reduction = noise(0.25, 0.25);
            r.false_color = FalseColorSuppression { steps: 2 };
            r.defringe = Defringe {
                purple_amount: 1.0,
                green_amount: 1.0,
                ..Defringe::default()
            };
            sharpen(r);
        },
    },
];

#[derive(Debug, Serialize)]
struct DetailCorpusReport {
    schema_version: u32,
    pipeline_version: &'static str,
    demosaic: &'static str,
    manifest: String,
    presets: Vec<&'static str>,
    sources: Vec<SourceReport>,
}

#[derive(Debug, Serialize)]
struct SourceReport {
    source: String,
    iso: Option<u32>,
    decoded_dimensions: [usize; 2],
    /// `None` when the frame is too small to profile.
    noise: Option<NoiseReport>,
    /// Chroma strength `with_default_noise_reduction` picks for this file.
    derived_chroma: f32,
    presets: Vec<PresetReport>,
    /// One sheet per manifest crop, rows in preset order after `off`.
    sheets: Vec<SheetReport>,
}

/// Luma noise model `sigma^2(x) = a * x + b` of the demosaiced frame, plus the
/// standard deviation it implies at shadow, mid and bright signal levels.
#[derive(Debug, Serialize)]
struct NoiseReport {
    a: f32,
    b: f32,
    bins: usize,
    sigma_at_0_05: f32,
    sigma_at_0_18: f32,
    sigma_at_0_50: f32,
}

#[derive(Debug, Serialize)]
struct PresetReport {
    preset: &'static str,
    /// Time spent in the detail stages alone.
    detail_ms: f64,
    false_color_ms: f64,
    chroma_noise_reduction_ms: f64,
    luminance_noise_reduction_ms: f64,
    capture_sharpening_ms: f64,
    defringe_ms: f64,
    /// Whole render, including the detail stages.
    pipeline_total_ms: f64,
}

impl PresetReport {
    fn new(preset: &'static str, timings: StageTimings) -> Self {
        let detail = timings.false_color
            + timings.chroma_noise_reduction
            + timings.luminance_noise_reduction
            + timings.capture_sharpening
            + timings.defringe;
        Self {
            preset,
            detail_ms: milliseconds(detail),
            false_color_ms: milliseconds(timings.false_color),
            chroma_noise_reduction_ms: milliseconds(timings.chroma_noise_reduction),
            luminance_noise_reduction_ms: milliseconds(timings.luminance_noise_reduction),
            capture_sharpening_ms: milliseconds(timings.capture_sharpening),
            defringe_ms: milliseconds(timings.defringe),
            pipeline_total_ms: milliseconds(timings.total),
        }
    }
}

#[derive(Debug, Serialize)]
struct SheetReport {
    name: String,
    category: String,
    coordinates: [usize; 4],
    output_100_percent: String,
    /// Per preset, in preset order (the first is the baseline).
    stats: Vec<CropStats>,
}

/// Texture and colour energy of one 8-bit crop, in code values. The high-pass
/// is the crop minus its 5x5 box mean, so it measures noise and fine texture
/// together: compare presets on the same crop rather than reading absolutes.
#[derive(Debug, Serialize)]
struct CropStats {
    preset: &'static str,
    luma_highpass_rms: f64,
    chroma_highpass_rms: f64,
    /// RMS of (B-Y, R-Y): how much colour the crop carries overall.
    chroma_rms: f64,
}

pub(super) fn detail_corpus(
    manifest_path: &Path,
    corpus: &Path,
    output: &Path,
    demosaic: DemosaicAlgorithm,
    force: bool,
) -> Result<()> {
    let manifest = load_quality_manifest(manifest_path)?;
    fs::create_dir_all(output)
        .with_context(|| format!("could not create output directory {}", output.display()))?;
    let report_path = output.join("report.json");
    if !force && report_path.exists() {
        bail!(
            "output {} already exists; pass --force to replace the comparison",
            report_path.display()
        );
    }

    let options = RenderOptions {
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
        sharpening: None,
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
        let pipeline = CpuPipeline::default();

        let base_recipe = base_recipe();
        let profile = estimate_noise_profile(&pipeline, &frame, &base_recipe, options)
            .with_context(|| format!("could not profile {}", requested.source))?;
        let derived = with_default_noise_reduction(base_recipe.clone(), profile.as_ref());
        let derived_chroma = derived.noise_reduction.chroma;
        let mut recipes: Vec<(&'static str, EditRecipe)> = PRESETS
            .iter()
            .map(|preset| {
                let mut recipe = base_recipe.clone();
                (preset.apply)(&mut recipe);
                (preset.name, recipe)
            })
            .collect();
        recipes.push((DERIVED_DEFAULT, derived));

        // One preset at a time so a single full-resolution render is alive;
        // only the (small) crops are kept for the sheets.
        let mut preset_reports = Vec::new();
        let mut crops_by_preset: Vec<Vec<DisplayRgbImage<u8>>> = Vec::new();
        for (name, recipe) in &recipes {
            let rendered = pipeline
                .render_export(
                    &frame,
                    recipe,
                    options,
                    OutputBitDepth::Eight,
                    DitherMode::None,
                    OutputSize::Original,
                    None,
                )
                .with_context(|| format!("could not develop {} with {name}", requested.source))?;
            let ExportImage::Rgb8(image) = &rendered.image else {
                unreachable!("detail corpus explicitly requests 8-bit output");
            };
            crops_by_preset.push(
                crops
                    .iter()
                    .map(|crop| crop_display_image(image, crop))
                    .collect::<Result<_>>()?,
            );
            preset_reports.push(PresetReport::new(name, rendered.timings));
        }

        let mut sheets = Vec::new();
        for (index, crop) in crops.iter().enumerate() {
            let name = format!("{stem}--{}--detail--100.png", crop.name);
            let rows: Vec<&DisplayRgbImage<u8>> = crops_by_preset
                .iter()
                .skip(1)
                .map(|preset_crops| &preset_crops[index])
                .collect();
            let sheet = build_sheet(&crops_by_preset[0][index], &rows)?;
            export_image(
                &output.join(&name),
                &ExportImage::Rgb8(sheet),
                &frame.info,
                png,
            )?;
            sheets.push(SheetReport {
                name: crop.name.clone(),
                category: crop.category.clone(),
                coordinates: [crop.x, crop.y, crop.width, crop.height],
                output_100_percent: name,
                stats: recipes
                    .iter()
                    .zip(&crops_by_preset)
                    .map(|((name, _), preset_crops)| crop_stats(name, &preset_crops[index]))
                    .collect(),
            });
        }
        sources.push(SourceReport {
            source: requested.source.clone(),
            iso: frame.info.capture.iso,
            decoded_dimensions: [frame.info.width, frame.info.height],
            noise: profile.as_ref().map(noise_report),
            derived_chroma,
            presets: preset_reports,
            sheets,
        });
    }

    let report = DetailCorpusReport {
        schema_version: 1,
        pipeline_version: env!("CARGO_PKG_VERSION"),
        demosaic: demosaic.stable_name(),
        manifest: manifest_path.display().to_string(),
        presets: PRESETS
            .iter()
            .map(|preset| preset.name)
            .chain([DERIVED_DEFAULT])
            .collect(),
        sources,
    };
    let json = serde_json::to_vec_pretty(&report).context("could not encode comparison report")?;
    write_output_bytes(&report_path, &json, force)
        .with_context(|| format!("could not commit {}", report_path.display()))?;
    write_stdout(&format!(
        "Rendered {} detail presets on {} source(s) in {}",
        report.presets.len(),
        report.sources.len(),
        output.display()
    ))
}

/// Neutral, unrotated recipe all presets start from.
fn base_recipe() -> EditRecipe {
    let mut recipe = EditRecipe::default();
    recipe.rendering.profile = RenderingProfileSelection::NEUTRAL;
    recipe.geometry.orientation_override = Some(Orientation::Normal);
    recipe
}

fn noise_report(profile: &NoiseProfile) -> NoiseReport {
    NoiseReport {
        a: profile.a,
        b: profile.b,
        bins: profile.bins,
        sigma_at_0_05: profile.sigma(0.05),
        sigma_at_0_18: profile.sigma(0.18),
        sigma_at_0_50: profile.sigma(0.5),
    }
}

/// Planes (Y, B-Y, R-Y) of an 8-bit crop, row-major.
fn yuv_planes(image: &DisplayRgbImage<u8>) -> [Vec<f32>; 3] {
    let mut planes = [(); 3].map(|()| Vec::with_capacity(image.width() * image.height()));
    for y in 0..image.height() {
        let row = &image.data()[y * image.row_stride()..][..image.width() * 3];
        for pixel in row.as_chunks::<3>().0 {
            let [r, g, b] = pixel.map(f32::from);
            let luma = 0.299 * r + 0.587 * g + 0.114 * b;
            planes[0].push(luma);
            planes[1].push(b - luma);
            planes[2].push(r - luma);
        }
    }
    planes
}

/// RMS of `plane` minus its (2*radius+1)-square box mean, skipping a border of
/// `radius` pixels.
fn highpass_rms(plane: &[f32], width: usize, height: usize, radius: usize) -> f64 {
    let (mut sum, mut count) = (0.0_f64, 0_u64);
    for y in radius..height.saturating_sub(radius) {
        for x in radius..width.saturating_sub(radius) {
            let mut mean = 0.0_f32;
            for yy in y - radius..=y + radius {
                for xx in x - radius..=x + radius {
                    mean += plane[yy * width + xx];
                }
            }
            let side = (2 * radius + 1) as f32;
            let delta = f64::from(plane[y * width + x] - mean / (side * side));
            sum += delta * delta;
            count += 1;
        }
    }
    (sum / count.max(1) as f64).sqrt()
}

fn crop_stats(preset: &'static str, crop: &DisplayRgbImage<u8>) -> CropStats {
    let [luma, blue, red] = yuv_planes(crop);
    let (w, h) = (crop.width(), crop.height());
    let chroma_highpass =
        highpass_rms(&blue, w, h, 2).powi(2) + highpass_rms(&red, w, h, 2).powi(2);
    let chroma_energy: f64 = blue
        .iter()
        .zip(&red)
        .map(|(b, r)| f64::from(b * b + r * r))
        .sum();
    CropStats {
        preset,
        luma_highpass_rms: highpass_rms(&luma, w, h, 2),
        chroma_highpass_rms: chroma_highpass.sqrt(),
        chroma_rms: (chroma_energy / (w * h) as f64).sqrt(),
    }
}

/// Stack `rows` vertically on the right, with `baseline` repeated on the left
/// of every row, separated by a grey gap. All crops share one size.
fn build_sheet(
    baseline: &DisplayRgbImage<u8>,
    rows: &[&DisplayRgbImage<u8>],
) -> Result<DisplayRgbImage<u8>> {
    let (w, h) = (baseline.width(), baseline.height());
    let width = 2 * w + SHEET_GAP;
    let height = rows.len() * h + rows.len().saturating_sub(1) * SHEET_GAP;
    let stride = width * 3;
    let mut pixels = vec![GAP_GREY; stride * height];
    let row_bytes = w * 3;
    for (row, image) in rows.iter().enumerate() {
        for y in 0..h {
            let target = (row * (h + SHEET_GAP) + y) * stride;
            let left = &baseline.data()[y * baseline.row_stride()..][..row_bytes];
            let right = &image.data()[y * image.row_stride()..][..row_bytes];
            pixels[target..target + row_bytes].copy_from_slice(left);
            let right_at = target + (w + SHEET_GAP) * 3;
            pixels[right_at..right_at + row_bytes].copy_from_slice(right);
        }
    }
    Ok(DisplayRgbImage::new(
        width,
        height,
        stride,
        baseline.transfer(),
        pixels,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rohditor_image::DisplayTransfer;

    #[test]
    fn presets_start_with_an_inert_baseline_and_all_validate() {
        let mut baseline = base_recipe();
        (PRESETS[0].apply)(&mut baseline);
        assert_eq!(baseline, base_recipe());
        let mut names = std::collections::HashSet::new();
        for preset in PRESETS {
            assert!(names.insert(preset.name), "duplicate {}", preset.name);
            let mut recipe = base_recipe();
            (preset.apply)(&mut recipe);
            recipe.noise_reduction.validate().expect(preset.name);
            recipe.capture_sharpening.validate().expect(preset.name);
            recipe.defringe.validate().expect(preset.name);
        }
    }

    #[test]
    fn sheet_puts_the_baseline_left_of_every_row() {
        let image = |value: u8| {
            DisplayRgbImage::new(2, 1, 6, DisplayTransfer::Srgb, vec![value; 6]).expect("image")
        };
        let (base, a, b) = (image(10), image(20), image(30));
        let sheet = build_sheet(&base, &[&a, &b]).expect("sheet");
        assert_eq!(
            (sheet.width(), sheet.height()),
            (2 * 2 + SHEET_GAP, 2 + SHEET_GAP)
        );
        assert_eq!(sheet.pixel(0, 0), Some(&[10, 10, 10][..]));
        assert_eq!(sheet.pixel(2 + SHEET_GAP, 0), Some(&[20, 20, 20][..]));
        assert_eq!(sheet.pixel(2, 0), Some(&[GAP_GREY; 3][..]));
        assert_eq!(
            sheet.pixel(2 + SHEET_GAP, 1 + SHEET_GAP),
            Some(&[30, 30, 30][..])
        );
    }
}
