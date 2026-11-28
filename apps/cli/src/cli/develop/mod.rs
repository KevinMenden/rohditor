//! The `develop` subcommand: build a recipe from the arguments, render and export.

mod arguments;
mod report;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rohditor_camera_profile::parse_dcp_file;
use rohditor_core::{
    CameraCalibration, CpuPipeline, HighlightDiagnostics, OpticsService, OutputPolicy,
    OutputSharpening, RenderOptions, export_image, paths_refer_to_same_file, resolve_camera_colour,
};
use rohditor_edit::{
    CameraProfileSelection, EditRecipe, HIGHLIGHT_THRESHOLD_RANGE, HighlightMethod,
    LensProfileSelection, RenderingProfileSelection, ToneCurve, WhiteBalance,
};
use rohditor_gpu::{ExportRender, FallbackExporter};
use rohditor_raw::{RawDecoder, RawlerDecoder};

use super::look_report::fit_recipe_camera_tone;
use super::model::NeuralArgs;
use super::output::write_stdout;
use super::values::{
    CliCropPolicy, CliDemosaic, CliHighlightMethod, CliMetadata, CliOrientation, CliOutputGamut,
    CliPngBitDepth, CliRenderingProfile, ExportProcessor, RgbMultipliers,
};
use arguments::{
    develop_export_settings, optics_adjustments, validate_highlight_options,
    white_balance_from_arguments,
};
use report::{bytes_to_mib, format_optics_report, format_quality, format_stage_timings};

#[derive(Debug, Clone)]
pub(super) struct DevelopArguments {
    pub(super) processor: ExportProcessor,
    pub(super) exposure: f32,
    pub(super) contrast: f32,
    pub(super) highlights: f32,
    pub(super) shadows: f32,
    pub(super) whites: f32,
    pub(super) blacks: f32,
    pub(super) tone_points: Vec<[f32; 2]>,
    pub(super) saturation: f32,
    pub(super) vibrance: f32,
    pub(super) highlight_reconstruction: CliHighlightMethod,
    pub(super) highlight_clip_threshold: Option<f32>,
    pub(super) highlight_detection_threshold: Option<f32>,
    pub(super) white_balance: Option<RgbMultipliers>,
    pub(super) camera_profile: Option<PathBuf>,
    pub(super) rendering_profile: CliRenderingProfile,
    pub(super) false_color_steps: u8,
    pub(super) defringe_purple: f32,
    pub(super) defringe_purple_hue: [f32; 2],
    pub(super) defringe_green: f32,
    pub(super) defringe_green_hue: [f32; 2],
    pub(super) chroma_nr: f32,
    pub(super) luma_nr: f32,
    pub(super) luma_nr_detail: f32,
    pub(super) capture_sharpening: bool,
    pub(super) capture_amount: f32,
    pub(super) capture_radius: f32,
    pub(super) capture_noise_protection: f32,
    pub(super) output_gamut: CliOutputGamut,
    pub(super) temperature: Option<f32>,
    pub(super) tint: f32,
    pub(super) crop: CliCropPolicy,
    pub(super) demosaic: CliDemosaic,
    pub(super) neural: NeuralArgs,
    pub(super) lens_profile: String,
    pub(super) no_lens_distortion: bool,
    pub(super) no_lens_vignetting: bool,
    pub(super) no_lens_tca: bool,
    pub(super) orientation: Option<CliOrientation>,
    pub(super) jpeg_quality: Option<u8>,
    pub(super) png_bit_depth: Option<CliPngBitDepth>,
    pub(super) long_edge: Option<u32>,
    pub(super) fit: Option<(u32, u32)>,
    pub(super) output_sharpen: Option<OutputSharpening>,
    pub(super) dither: bool,
    pub(super) metadata: CliMetadata,
    pub(super) force: bool,
}

pub(super) fn develop(file: &Path, output: &Path, arguments: DevelopArguments) -> Result<()> {
    validate_highlight_options(
        arguments.highlight_reconstruction,
        arguments.highlight_clip_threshold,
        arguments.highlight_detection_threshold,
    )?;
    let export_settings = develop_export_settings(output, &arguments)?;
    if paths_refer_to_same_file(file, output).with_context(|| {
        format!(
            "could not compare RAW source {} with output {}",
            file.display(),
            output.display()
        )
    })? {
        bail!(
            "refusing to replace source RAW file {} with developed output",
            file.display()
        );
    }
    if !export_settings.collision.replaces_existing() && output.exists() {
        bail!(
            "output {} already exists; pass --force to replace it",
            output.display()
        );
    }

    if arguments.white_balance.is_some()
        && (arguments.temperature.is_some() || arguments.tint != 0.0)
    {
        bail!("--white-balance cannot be combined with --temperature or --tint");
    }
    let mut recipe = EditRecipe {
        optics: optics_adjustments(&arguments)?,
        ..EditRecipe::default()
    };
    let camera_profile = arguments
        .camera_profile
        .as_deref()
        .map(parse_dcp_file)
        .transpose()
        .with_context(|| "could not parse the requested camera profile")?;
    if let Some(profile) = camera_profile {
        recipe.color.camera_profile = CameraProfileSelection::Matrix(profile);
    }
    recipe.false_color = rohditor_edit::FalseColorSuppression {
        steps: arguments.false_color_steps,
    };
    recipe.defringe = rohditor_edit::Defringe {
        purple_amount: arguments.defringe_purple,
        purple_hue: arguments.defringe_purple_hue,
        green_amount: arguments.defringe_green,
        green_hue: arguments.defringe_green_hue,
    };
    recipe.noise_reduction = rohditor_edit::NoiseReduction {
        chroma: arguments.chroma_nr,
        luminance: arguments.luma_nr,
        luminance_detail: arguments.luma_nr_detail,
    };
    recipe.capture_sharpening = rohditor_edit::CaptureSharpening {
        enabled: arguments.capture_sharpening,
        amount: arguments.capture_amount,
        radius: arguments.capture_radius,
        noise_protection: arguments.capture_noise_protection,
    };
    recipe.rendering.profile = arguments.rendering_profile.into();
    recipe.light.exposure_ev = arguments.exposure;
    recipe.light.contrast = arguments.contrast;
    recipe.light.highlights = arguments.highlights;
    recipe.light.shadows = arguments.shadows;
    recipe.light.whites = arguments.whites;
    recipe.light.blacks = arguments.blacks;
    recipe.light.tone_curve = ToneCurve::through(&arguments.tone_points);
    recipe.color.saturation = arguments.saturation;
    recipe.color.vibrance = arguments.vibrance;
    recipe.raw.highlights.method = arguments.highlight_reconstruction.into();
    recipe.raw.highlights.clip.threshold = arguments
        .highlight_clip_threshold
        .unwrap_or(HIGHLIGHT_THRESHOLD_RANGE.neutral);
    let detection_threshold = arguments
        .highlight_detection_threshold
        .unwrap_or(HIGHLIGHT_THRESHOLD_RANGE.neutral);
    match recipe.raw.highlights.method {
        HighlightMethod::Opposed => {
            recipe.raw.highlights.opposed.detection_threshold = detection_threshold;
        }
        HighlightMethod::Off | HighlightMethod::Clip => {}
    }
    recipe.geometry.orientation_override = arguments.orientation.map(Into::into);
    // Validate explicitly supplied values before decoding so malformed CLI
    // input reports a recipe error even when the source itself is unreadable.
    recipe.color.white_balance = white_balance_from_arguments(&arguments, None);
    recipe
        .validate()
        .context("could not validate the development recipe")?;

    let decoder = RawlerDecoder::default();
    let pipeline = match recipe.optics.profile {
        LensProfileSelection::Off => CpuPipeline::default(),
        LensProfileSelection::Automatic | LensProfileSelection::Lensfun { .. } => {
            let service = OpticsService::load_bundled()
                .context("could not load the bundled Lensfun profile database")?;
            CpuPipeline::new(Arc::new(service))
        }
    };
    let pipeline = match arguments.demosaic {
        CliDemosaic::Neural => pipeline.with_neural(arguments.neural.load()?),
        _ => pipeline,
    };
    let decode_started = Instant::now();
    let frame = decoder
        .decode(file)
        .with_context(|| format!("could not decode {} for development", file.display()))?;
    let decode_time = decode_started.elapsed();
    let as_shot_coordinates = resolve_camera_colour(
        &CameraCalibration::from_raw_info(&frame.info),
        &recipe.color.camera_profile,
        WhiteBalance::AsShot,
    )
    .ok()
    .and_then(|resolved| resolved.as_shot_coordinates);
    recipe.color.white_balance = white_balance_from_arguments(&arguments, as_shot_coordinates);
    recipe
        .validate()
        .context("could not validate the development recipe")?;
    if recipe.rendering.profile == RenderingProfileSelection::MATCHED {
        fit_recipe_camera_tone(&decoder, &pipeline, file, &frame, &mut recipe)?;
    }
    let rendered = FallbackExporter::default()
        .render(
            arguments.processor.into(),
            &pipeline,
            &frame,
            &recipe,
            RenderOptions {
                raw_crop_policy: arguments.crop.into(),
                demosaic: arguments.demosaic.into(),
                output_policy: arguments.output_gamut.into(),
            },
            export_settings.format.bit_depth(),
            export_settings.dithering,
            export_settings.size,
            export_settings.sharpening,
            |error| eprintln!("GPU export unavailable; using CPU: {error}"),
        )
        .with_context(|| format!("could not develop {}", file.display()))?;
    let result = match rendered {
        ExportRender::Gpu {
            result,
            capabilities,
        } => {
            let encode_started = Instant::now();
            let report = export_image(output, &result.image, &frame.info, export_settings)
                .with_context(|| {
                    format!("could not export developed image to {}", output.display())
                })?;
            return write_stdout(&format!(
                "Developed {}x{} {}-bit sRGB {} to {} ({} bytes)\nColor processor: GPU, {} ({}, {} {})\nSensor processor: {}\nRAW decode: {:?}; Full-quality preparation: {:?}; GPU upload: {:?}; color and readback: {:?}; encoding: {:?}\nGPU capture: {} tiles of {} pixels, halo {}, compute/wait {:?}, upload {:?}, readback {:?}\nGPU allocation estimate: {} MiB; CPU buffer estimate: {} MiB; uploaded {} bytes; read back {} bytes in {} submissions\nOutput gamut pixel counts: unavailable on GPU",
                report.width,
                report.height,
                report.bit_depth.bits(),
                export_settings.format.description(),
                output.display(),
                report.bytes_written,
                capabilities.adapter_name,
                capabilities.backend,
                capabilities.driver,
                capabilities.driver_info,
                if result.sensor_gpu { "GPU" } else { "CPU" },
                decode_time,
                result.preparation,
                result.upload_time,
                result.color_and_readback_time,
                encode_started.elapsed(),
                result.capture.tiles,
                result.capture.tile_edge,
                result.capture.halo,
                result.capture.compute_and_wait,
                result.capture.upload,
                result.capture.readback,
                result.estimated_gpu_bytes / (1024 * 1024),
                result.estimated_cpu_bytes / (1024 * 1024),
                result.uploaded_bytes,
                result.readback_bytes,
                result.submissions,
            ));
        }
        ExportRender::Cpu { result, .. } => result,
    };
    let source_info = frame.info.clone();
    drop(frame);
    let encode_started = Instant::now();
    let report = export_image(output, &result.image, &source_info, export_settings)
        .with_context(|| format!("could not export developed image to {}", output.display()))?;
    let encode_time = encode_started.elapsed();

    let highlight_report = match result.highlight_diagnostics {
        HighlightDiagnostics::Off => "Highlight reconstruction: off".to_owned(),
        HighlightDiagnostics::Clip(stats) => format!(
            "Highlight Clip: {} affected, {} changed, {} nominally over-white (R {}, G {}, B {})",
            stats.affected_sites,
            stats.changed_sites,
            stats.nominal_over_white_sites,
            stats.affected_by_channel[0],
            stats.affected_by_channel[1],
            stats.affected_by_channel[2],
        ),
        HighlightDiagnostics::Opposed(stats) => format!(
            "Highlight Opposed: {} suspected, {} reconstructed, {} changed, {} fallback, {} fully unsupported, {} neutralised (R {}, G {}, B {})",
            stats.suspected_clipped_sites,
            stats.reconstructed_sites,
            stats.changed_sites,
            stats.fallback_sites,
            stats.fully_unsupported_sites,
            stats.neutralised_sites,
            stats.suspected_by_channel[0],
            stats.suspected_by_channel[1],
            stats.suspected_by_channel[2],
        ),
    };
    let optics_report = format!(
        "{}\nFalse colour: {:?} (algorithm v{})\nDefringe: {:?} (algorithm v{})\nNoise reduction: {:?} (algorithm v{})\nCapture sharpening: {:?} (algorithm v{}, {} iterations)",
        format_optics_report(&recipe.optics, result.optics_provenance.as_ref()),
        recipe.false_color,
        rohditor_core::FALSE_COLOR_ALGORITHM_VERSION,
        recipe.defringe,
        rohditor_core::DEFRINGE_ALGORITHM_VERSION,
        recipe.noise_reduction,
        rohditor_core::NOISE_REDUCTION_ALGORITHM_VERSION,
        recipe.capture_sharpening,
        rohditor_core::CAPTURE_SHARPENING_ALGORITHM_VERSION,
        rohditor_core::CAPTURE_SHARPENING_ITERATIONS
    );
    write_stdout(&format!(
        "Developed {}x{} {}-bit sRGB {}{} with {} demosaic to {} ({} bytes, {})\nRendering profile: {}{}\nOutput gamut policy: {}{}\nOutput gamut pixels: {} in gamut, {} compressed, {} limited, {} clipped fallback, {} invalid\n{}\n{}\n{}\nEstimated CPU buffer peak: {} MiB",
        report.width,
        report.height,
        report.bit_depth.bits(),
        export_settings.format.description(),
        format_quality(export_settings.format),
        arguments.demosaic.label(),
        output.display(),
        report.bytes_written,
        if report.metadata_embedded {
            "safe EXIF"
        } else {
            "no EXIF"
        },
        recipe.rendering.profile.display_name(),
        recipe
            .rendering
            .profile
            .process_version()
            .map_or_else(String::new, |version| format!(" v{version}")),
        OutputPolicy::from(arguments.output_gamut).display_name(),
        OutputPolicy::from(arguments.output_gamut)
            .algorithm_version()
            .map_or_else(String::new, |version| format!(" v{version}")),
        result.output_gamut_diagnostics.in_gamut_pixels,
        result.output_gamut_diagnostics.compressed_pixels,
        result.output_gamut_diagnostics.limited_pixels,
        result.output_gamut_diagnostics.clipped_fallback_pixels,
        result.output_gamut_diagnostics.invalid_pixels,
        format_stage_timings(decode_time, result.timings, encode_time),
        highlight_report,
        optics_report,
        bytes_to_mib(result.memory.estimated_peak_bytes),
    ))
}
