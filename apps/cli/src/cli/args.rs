//! The clap definitions: the `Cli` parser, its `Command` subcommands and their value parsers.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use rohditor_core::{
    JPEG_QUALITY_MAX, JPEG_QUALITY_MIN, OutputMedium, OutputSharpening, OutputSharpeningStrength,
};
use rohditor_edit::{
    BLACKS_RANGE, CONTRAST_RANGE, EXPOSURE_EV_RANGE, HIGHLIGHTS_RANGE, NoiseReduction,
    SATURATION_RANGE, SHADOWS_RANGE, TINT_RANGE, VIBRANCE_RANGE, WHITES_RANGE,
};

use super::detail_compare::DetailStage;
use super::model::{ModelCommand, NeuralArgs};
use super::values::{
    CliCropPolicy, CliDemosaic, CliHighlightMethod, CliMetadata, CliOrientation, CliOutputGamut,
    CliPngBitDepth, CliRenderingProfile, ExportProcessor, RgbMultipliers,
};

#[derive(Debug, Parser)]
#[command(
    name = "rohditor-cli",
    version,
    about = "Headless tools for Rohditor RAW files"
)]
pub(super) struct Cli {
    #[command(subcommand)]
    pub(super) command: Command,
}

// Parsed once at startup; boxing the large `Develop` arguments would only add noise.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
pub(super) enum Command {
    /// Inspect normalized metadata and, by default, decode the sensor mosaic.
    Inspect {
        /// RAW file to inspect. Detection uses file contents, not this extension.
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,

        /// Skip full sensor decoding and only probe metadata.
        #[arg(long)]
        metadata_only: bool,
    },

    /// Extract the embedded loading preview without developing the RAW mosaic.
    ExtractPreview {
        /// RAW file containing the preview.
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// JPEG destination (`.jpg` or `.jpeg`).
        #[arg(value_name = "OUTPUT")]
        output: PathBuf,

        /// Replace an existing output file.
        #[arg(long)]
        force: bool,
    },

    /// Develop the RAW mosaic and transactionally export an sRGB JPEG or PNG.
    Develop {
        /// RAW file to develop. Detection uses file contents, not this extension.
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// sRGB destination (`.jpg`, `.jpeg`, or `.png`).
        #[arg(value_name = "OUTPUT")]
        output: PathBuf,

        /// Exposure compensation in stops (-5 to +5).
        #[arg(long, default_value_t = EXPOSURE_EV_RANGE.neutral, allow_hyphen_values = true)]
        exposure: f32,

        /// Contrast in stops of slope around 18% gray (-1 to +1).
        #[arg(long, default_value_t = CONTRAST_RANGE.neutral, allow_hyphen_values = true)]
        contrast: f32,

        /// Recover or increase highlight luminance (-1 to +1).
        #[arg(long, default_value_t = HIGHLIGHTS_RANGE.neutral, allow_hyphen_values = true)]
        highlights: f32,

        /// Recover or increase shadow luminance (-1 to +1).
        #[arg(long, default_value_t = SHADOWS_RANGE.neutral, allow_hyphen_values = true)]
        shadows: f32,

        /// Adjust the brightest white tones (-1 to +1).
        #[arg(long, default_value_t = WHITES_RANGE.neutral, allow_hyphen_values = true)]
        whites: f32,

        /// Adjust the darkest black tones (-1 to +1).
        #[arg(long, default_value_t = BLACKS_RANGE.neutral, allow_hyphen_values = true)]
        blacks: f32,

        /// A tone curve point as sRGB-encoded `INPUT,OUTPUT` (0 to 1), between the
        /// fixed ends (0,0) and (1,1). Repeat for more points, in input order.
        #[arg(long = "tone-point", value_name = "INPUT,OUTPUT", value_parser = parse_tone_point)]
        tone_points: Vec<[f32; 2]>,

        /// Rec.2020 luminance-relative saturation (0 to 2; 1 is neutral).
        #[arg(long, default_value_t = SATURATION_RANGE.neutral)]
        saturation: f32,

        /// Increase or reduce saturation preferentially in less-saturated colors (-1 to +1).
        #[arg(long, default_value_t = VIBRANCE_RANGE.neutral, allow_hyphen_values = true)]
        vibrance: f32,

        /// RAW-stage highlight handling.
        #[arg(long, value_enum, default_value_t = CliHighlightMethod::Clip)]
        highlight_reconstruction: CliHighlightMethod,

        /// Effective normalized white threshold for highlight clipping (0.5 to 1.5);
        /// levels resolve at threshold x 0.987 safety margin below nominal white.
        #[arg(
            long = "highlight-clip-threshold",
            alias = "highlight-threshold",
            allow_hyphen_values = true
        )]
        highlight_clip_threshold: Option<f32>,

        /// Camera-native detection threshold for Opposed (0.5 to 1.5);
        /// detection resolves at threshold x 0.987 safety margin below nominal white.
        #[arg(long, allow_hyphen_values = true)]
        highlight_detection_threshold: Option<f32>,

        /// R,G,B multipliers relative to the as-shot white balance.
        #[arg(long, value_name = "RED,GREEN,BLUE")]
        white_balance: Option<RgbMultipliers>,

        /// DNG Camera Profile (DCP) to embed in the recipe: its matrices, hue/saturation map, and look table.
        #[arg(long, value_name = "PROFILE.dcp")]
        camera_profile: Option<PathBuf>,

        /// Moiré / false-colour suppression: median passes over the colour
        /// differences (0 to 5; 0 is off). Runs on the CPU.
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u8).range(0..=5))]
        false_color_steps: u8,

        /// Purple defringe strength (0 to 1; 0 is off). Runs on the CPU.
        #[arg(long, default_value_t = 0.0)]
        defringe_purple: f32,

        /// Hue range of purple fringes in degrees, `START,END` (10 to 90 wide).
        #[arg(long, value_name = "START,END", default_value = "270,330", value_parser = parse_hue_range)]
        defringe_purple_hue: [f32; 2],

        /// Green defringe strength (0 to 1; 0 is off). Runs on the CPU.
        #[arg(long, default_value_t = 0.0)]
        defringe_green: f32,

        /// Hue range of green fringes in degrees, `START,END` (10 to 90 wide).
        #[arg(long, value_name = "START,END", default_value = "75,150", value_parser = parse_hue_range)]
        defringe_green_hue: [f32; 2],

        /// Chroma noise reduction strength (0 to 1; 0 is off). Runs on the CPU.
        #[arg(long, default_value_t = 0.0)]
        chroma_nr: f32,

        /// Luminance noise reduction strength (0 to 1; 0 is off). Runs on the CPU.
        #[arg(long, default_value_t = 0.0)]
        luma_nr: f32,
        /// Fine texture the luminance noise reduction keeps (0 to 1; higher keeps more).
        #[arg(long, default_value_t = 0.5)]
        luma_nr_detail: f32,

        /// Enable source-resolution capture sharpening (eight RL iterations).
        #[arg(long)]
        capture_sharpening: bool,
        /// Capture sharpening blend (0 to 1).
        #[arg(long, default_value_t = 0.5)]
        capture_amount: f32,
        /// Capture Gaussian sigma in source pixels (0.3 to 1.2).
        #[arg(long, default_value_t = 0.6)]
        capture_radius: f32,
        /// Suppress sharpening in low-contrast/noisy areas (0 to 1).
        #[arg(long, default_value_t = 0.5)]
        capture_noise_protection: f32,

        /// Base rendering: Matched (tone and colour fitted to the camera JPEG),
        /// Standard tone rendering, or Neutral identity rendering.
        #[arg(long, value_enum, default_value_t = CliRenderingProfile::Matched)]
        rendering_profile: CliRenderingProfile,

        /// Target sRGB gamut handling after rendering and creative edits.
        #[arg(long, value_enum, default_value_t = CliOutputGamut::Clip)]
        output_gamut: CliOutputGamut,

        /// Color execution backend for full-resolution export.
        #[arg(long, value_enum, default_value_t = ExportProcessor::Auto)]
        processor: ExportProcessor,

        /// Absolute camera-calibrated white-balance temperature in Kelvin
        /// (2000 to 25000). If omitted, the source's As Shot estimate is used.
        #[arg(long, allow_hyphen_values = true)]
        temperature: Option<f32>,

        /// Green/magenta tint offset from As Shot (-1 to +1); implies
        /// Temperature/Tint white balance.
        #[arg(long, default_value_t = TINT_RANGE.neutral, allow_hyphen_values = true)]
        tint: f32,

        /// RAW sensor crop policy, applied before normalization.
        #[arg(long, value_enum, default_value_t = CliCropPolicy::Recommended)]
        crop: CliCropPolicy,

        /// CPU demosaic algorithm.
        #[arg(long, value_enum, default_value_t = CliDemosaic::Amaze)]
        demosaic: CliDemosaic,

        #[command(flatten)]
        neural: NeuralArgs,

        /// Lensfun profile selection: off, auto, or a stable profile ID.
        #[arg(long, default_value = "off")]
        lens_profile: String,

        /// Do not apply the selected profile's distortion correction.
        #[arg(long)]
        no_lens_distortion: bool,

        /// Do not apply the selected profile's vignetting correction.
        #[arg(long)]
        no_lens_vignetting: bool,

        /// Do not apply the selected profile's chromatic-aberration correction.
        #[arg(long)]
        no_lens_tca: bool,

        /// Replace the RAW orientation metadata with an explicit transform.
        #[arg(long, value_enum)]
        orientation: Option<CliOrientation>,

        /// JPEG quality from 1 (smallest) to 100 (highest); JPEG only.
        #[arg(
            long,
            value_name = "1-100",
            value_parser = clap::value_parser!(u8).range(i64::from(JPEG_QUALITY_MIN)..=i64::from(JPEG_QUALITY_MAX))
        )]
        jpeg_quality: Option<u8>,

        /// Integer sample depth; PNG only (default: 8).
        #[arg(long, value_enum, value_name = "8|16")]
        png_bit_depth: Option<CliPngBitDepth>,

        /// Resize so the longer edge has this many pixels (never upscales).
        #[arg(long, value_name = "PIXELS", conflicts_with = "fit", value_parser = clap::value_parser!(u32).range(1..))]
        long_edge: Option<u32>,

        /// Resize to fit inside a WxH bounding box (never upscales).
        #[arg(long, value_name = "WxH", value_parser = parse_fit_box)]
        fit: Option<(u32, u32)>,

        /// Sharpen after resizing: MEDIUM[:STRENGTH] with medium screen, matte
        /// or glossy and strength low, standard (default) or high.
        #[arg(long, value_name = "MEDIUM[:STRENGTH]", value_parser = parse_output_sharpening)]
        output_sharpen: Option<OutputSharpening>,

        /// Apply deterministic ordered dithering before integer quantization.
        #[arg(long)]
        dither: bool,

        /// Preserve selected safe capture EXIF fields or omit source metadata.
        #[arg(long, value_enum, default_value_t = CliMetadata::Safe)]
        metadata: CliMetadata,

        /// Replace an existing output file.
        #[arg(long)]
        force: bool,
    },

    /// Emit named 100% and 200% neutral crops for Phase 9 quality review.
    QualityCrops {
        /// JSON manifest containing private source names and crop coordinates.
        #[arg(value_name = "MANIFEST")]
        manifest: PathBuf,

        /// Directory containing the private RAW sources named by the manifest.
        #[arg(value_name = "CORPUS_DIR")]
        corpus: PathBuf,

        /// Destination directory for generated PNGs and report.json.
        #[arg(value_name = "OUTPUT_DIR")]
        output: PathBuf,

        /// CPU demosaic algorithm under review.
        #[arg(long, value_enum, default_value_t = CliDemosaic::Amaze)]
        demosaic: CliDemosaic,

        /// Replace existing generated crops and report.json.
        #[arg(long)]
        force: bool,
    },

    /// Score a detail stage (default: no-op baseline) on the synthetic
    /// ground-truth scenes at several noise levels, and print a score table.
    DetailCompare {
        /// Stage to evaluate.
        #[arg(long, value_enum, default_value_t = DetailStage::Noop)]
        stage: DetailStage,

        /// Stage strength in 0..=1, for stages that have one.
        #[arg(long, default_value_t = 0.5)]
        strength: f32,

        /// Texture preservation in 0..=1, for stages that have one.
        #[arg(long, default_value_t = 0.5)]
        detail: f32,

        /// Median passes of the false-colour stages (0 to 5).
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(0..=5))]
        steps: u8,

        /// Demosaic the synthetic scenes go through before the stage.
        #[arg(long, value_enum, default_value_t = CliDemosaic::Bilinear)]
        demosaic: CliDemosaic,

        /// Also score every readable RAW file in this directory.
        #[arg(long, value_name = "RAW_DIR")]
        raw_dir: Option<PathBuf>,
    },

    /// Render a grid of detail settings (noise reduction, false colour,
    /// defringe, capture sharpening) on the quality-crop manifest, and write
    /// 1:1 before/after sheets with the noise profile and timings per file.
    DetailCorpus {
        /// JSON manifest containing private source names and crop coordinates.
        #[arg(value_name = "MANIFEST")]
        manifest: PathBuf,

        /// Directory containing the private RAW sources named by the manifest.
        #[arg(value_name = "CORPUS_DIR")]
        corpus: PathBuf,

        /// Destination directory for the sheet PNGs and report.json.
        #[arg(value_name = "OUTPUT_DIR")]
        output: PathBuf,

        /// CPU demosaic algorithm used for every preset.
        #[arg(long, value_enum, default_value_t = CliDemosaic::Amaze)]
        demosaic: CliDemosaic,

        /// Replace existing generated files and report.json.
        #[arg(long)]
        force: bool,
    },

    /// Render every highlight method on the quality-crop manifest and report
    /// reconstruction counts, scratch bytes, wall time, and differences from Clip.
    HighlightCompare {
        /// JSON manifest containing private source names and crop coordinates.
        #[arg(value_name = "MANIFEST")]
        manifest: PathBuf,

        /// Directory containing the private RAW sources named by the manifest.
        #[arg(value_name = "CORPUS_DIR")]
        corpus: PathBuf,

        /// Destination directory for per-method crop PNGs and report.json.
        #[arg(value_name = "OUTPUT_DIR")]
        output: PathBuf,

        /// CPU demosaic algorithm used for every method.
        #[arg(long, value_enum, default_value_t = CliDemosaic::Amaze)]
        demosaic: CliDemosaic,

        /// Also write each method's full-resolution PNG export.
        #[arg(long)]
        full_exports: bool,

        /// Replace existing generated files and report.json.
        #[arg(long)]
        force: bool,
    },

    /// Compare Neutral, Standard and camera-matched renderings with each RAW's
    /// embedded camera JPEG and report the tone and colour error.
    LookReport {
        /// RAW files to measure. Each must contain an embedded JPEG preview.
        #[arg(value_name = "FILE", required = true)]
        files: Vec<PathBuf>,

        /// Render with this DCP camera profile instead of the built-in matrices.
        #[arg(long, value_name = "DCP")]
        camera_profile: Option<PathBuf>,

        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Compare Rohditor's decoded sensor mosaic with LibRaw unprocessed_raw PGM output.
    VerifyLibraw {
        /// RAW source decoded by Rohditor/rawler.
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// 16-bit PGM emitted by LibRaw's unprocessed_raw sample tool.
        #[arg(value_name = "LIBRAW_PGM")]
        libraw_pgm: PathBuf,

        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,

        /// Largest accepted per-sample decoder difference in digital numbers.
        #[arg(long, default_value_t = 8)]
        max_error: u16,

        /// Largest accepted aggregate decoder RMSE in digital numbers.
        #[arg(long, default_value_t = 1.1)]
        max_rmse: f64,
    },

    /// Demosaic contract-scaled `.npy` mosaics ([H, W] or [N, H, W]) with
    /// optional noise reduction: the neural-demosaic evaluation baseline.
    DemosaicNpy {
        /// RGGB mosaic in the scaling of `ml/contract.json`, not white balanced.
        #[arg(value_name = "MOSAIC_NPY")]
        input: PathBuf,

        /// As-shot white-balance gains, `[3]` or `[N, 3]` float32 `.npy`.
        #[arg(long, value_name = "GAINS_NPY")]
        wb: PathBuf,

        /// Output `.npy`: balanced linear RGB, `[3, H, W]` or `[N, 3, H, W]`.
        #[arg(long, short)]
        output: PathBuf,

        /// Demosaic algorithm. `neural` runs the network alone (contract
        /// tiling, no highlight fallback), as the evaluation compares it.
        #[arg(long, value_enum, default_value_t = CliDemosaic::Rcd)]
        demosaic: CliDemosaic,

        #[command(flatten)]
        neural: NeuralArgs,

        /// Chroma noise-reduction strength in [0, 1]; 0 is off.
        #[arg(long, default_value_t = 0.0)]
        chroma: f32,

        /// Luminance noise-reduction strength in [0, 1]; 0 is off.
        #[arg(long, default_value_t = 0.0)]
        luminance: f32,

        /// Texture preservation of luminance noise reduction in [0, 1].
        #[arg(long, default_value_t = NoiseReduction::default().luminance_detail)]
        luminance_detail: f32,

        /// Replace an existing output file.
        #[arg(long)]
        force: bool,
    },

    /// Write the neural demosaic's packed, balanced input for a RAW file
    /// (`[1, 4, H/2, W/2]` `.npy`), for the Python parity test.
    NeuralInputNpy {
        #[arg(value_name = "FILE")]
        file: PathBuf,

        #[arg(long, short)]
        output: PathBuf,

        /// Replace an existing output file.
        #[arg(long)]
        force: bool,
    },

    /// Install or check the neural demosaic model.
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
}

/// Parse `START,END` degrees. Range checks are left to recipe validation.
fn parse_hue_range(value: &str) -> Result<[f32; 2], String> {
    parse_pair(value, "expected START,END in degrees")
}

fn parse_tone_point(value: &str) -> Result<[f32; 2], String> {
    parse_pair(value, "expected INPUT,OUTPUT from 0 to 1")
}

/// Two comma-separated numbers.
fn parse_pair(value: &str, expected: &str) -> Result<[f32; 2], String> {
    let (start, end) = value.split_once(',').ok_or_else(|| expected.to_owned())?;
    let parse = |text: &str| {
        text.trim()
            .parse::<f32>()
            .map_err(|error| error.to_string())
    };
    Ok([parse(start)?, parse(end)?])
}

/// Parse `WxH` (e.g. `2048x1365`) with both sides at least one pixel.
fn parse_fit_box(value: &str) -> Result<(u32, u32), String> {
    let (width, height) = value
        .split_once(['x', 'X'])
        .ok_or_else(|| "expected WIDTHxHEIGHT, e.g. 2048x1365".to_owned())?;
    let parse = |part: &str| {
        part.trim()
            .parse::<u32>()
            .ok()
            .filter(|&pixels| pixels > 0)
            .ok_or_else(|| format!("{part:?} is not a positive pixel count"))
    };
    Ok((parse(width)?, parse(height)?))
}

/// Parse `MEDIUM[:STRENGTH]`, e.g. `screen` or `glossy:high`.
fn parse_output_sharpening(value: &str) -> Result<OutputSharpening, String> {
    let (medium, strength) = value.split_once(':').unwrap_or((value, "standard"));
    let medium = match medium.to_ascii_lowercase().as_str() {
        "screen" => OutputMedium::Screen,
        "matte" => OutputMedium::Matte,
        "glossy" => OutputMedium::Glossy,
        other => return Err(format!("{other:?} is not screen, matte or glossy")),
    };
    let strength = match strength.to_ascii_lowercase().as_str() {
        "low" => OutputSharpeningStrength::Low,
        "standard" => OutputSharpeningStrength::Standard,
        "high" => OutputSharpeningStrength::High,
        other => return Err(format!("{other:?} is not low, standard or high")),
    };
    Ok(OutputSharpening { medium, strength })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser;
    use rohditor_core::{OutputMedium, OutputSharpening, OutputSharpeningStrength};
    use rohditor_demosaic::DemosaicAlgorithm;

    use super::*;

    #[test]
    fn develop_accepts_the_stable_mhc_cli_value() {
        let parsed = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--demosaic",
            "mhc",
        ])
        .expect("mhc is a supported development algorithm");
        let Command::Develop { demosaic, .. } = parsed.command else {
            panic!("expected develop command");
        };
        assert!(matches!(demosaic, CliDemosaic::MalvarHeCutler));

        let rcd = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--demosaic",
            "rcd",
        ])
        .expect("rcd is a supported development algorithm");
        let Command::Develop { demosaic, .. } = rcd.command else {
            panic!("expected develop command");
        };
        assert!(matches!(demosaic, CliDemosaic::Rcd));
        assert_eq!(DemosaicAlgorithm::from(demosaic), DemosaicAlgorithm::Rcd);

        let amaze = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--demosaic",
            "amaze",
        ])
        .expect("amaze is a supported development algorithm");
        let Command::Develop { demosaic, .. } = amaze.command else {
            panic!("expected develop command");
        };
        assert!(matches!(demosaic, CliDemosaic::Amaze));
        assert_eq!(DemosaicAlgorithm::from(demosaic), DemosaicAlgorithm::Amaze);

        let defaulted = Cli::try_parse_from(["rohditor-cli", "develop", "input.arw", "output.jpg"])
            .expect("development defaults parse");
        let Command::Develop {
            demosaic,
            highlight_reconstruction,
            ..
        } = defaulted.command
        else {
            panic!("expected develop command");
        };
        assert!(matches!(demosaic, CliDemosaic::Amaze));
        assert!(matches!(highlight_reconstruction, CliHighlightMethod::Clip));
    }

    #[test]
    fn develop_carries_a_camera_profile_path_without_installing_it() {
        let parsed = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--camera-profile",
            "studio.dcp",
        ])
        .expect("camera profile option should parse");
        let Command::Develop { camera_profile, .. } = parsed.command else {
            panic!("expected develop command");
        };
        assert_eq!(camera_profile, Some(PathBuf::from("studio.dcp")));
    }

    #[test]
    fn develop_parses_capture_controls_and_defaults_off() {
        let defaults = Cli::try_parse_from(["rohditor-cli", "develop", "photo.ARW", "out.png"])
            .expect("default CLI");
        let Command::Develop {
            capture_sharpening, ..
        } = defaults.command
        else {
            panic!("develop command");
        };
        assert!(!capture_sharpening);
        let parsed = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "photo.ARW",
            "out.png",
            "--capture-sharpening",
            "--capture-amount",
            "0.7",
            "--capture-radius",
            "0.8",
            "--capture-noise-protection",
            "0.3",
        ])
        .expect("capture CLI");
        let Command::Develop {
            capture_sharpening,
            capture_amount,
            capture_radius,
            capture_noise_protection,
            ..
        } = parsed.command
        else {
            panic!("develop command");
        };
        assert!(capture_sharpening);
        assert_eq!(
            (capture_amount, capture_radius, capture_noise_protection),
            (0.7, 0.8, 0.3)
        );
    }

    #[test]
    fn develop_parses_rendering_profile_and_defaults_to_matched() {
        let defaulted = Cli::try_parse_from(["rohditor-cli", "develop", "input.arw", "output.jpg"])
            .expect("default rendering should parse");
        let Command::Develop {
            rendering_profile, ..
        } = defaulted.command
        else {
            panic!("expected develop command");
        };
        assert!(matches!(rendering_profile, CliRenderingProfile::Matched));

        let neutral = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--rendering-profile",
            "neutral",
        ])
        .expect("neutral rendering should parse");
        let Command::Develop {
            rendering_profile, ..
        } = neutral.command
        else {
            panic!("expected develop command");
        };
        assert!(matches!(rendering_profile, CliRenderingProfile::Neutral));
    }

    #[test]
    fn develop_parses_output_gamut_and_defaults_to_clip() {
        let defaulted = Cli::try_parse_from(["rohditor-cli", "develop", "input.arw", "output.jpg"])
            .expect("default output gamut should parse");
        let Command::Develop { output_gamut, .. } = defaulted.command else {
            panic!("expected develop command");
        };
        assert!(matches!(output_gamut, CliOutputGamut::Clip));

        let compressed = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--output-gamut",
            "chroma-compress",
        ])
        .expect("chroma compression should parse");
        let Command::Develop { output_gamut, .. } = compressed.command else {
            panic!("expected develop command");
        };
        assert!(matches!(output_gamut, CliOutputGamut::ChromaCompress));
    }

    #[test]
    fn quality_crops_default_to_amaze() {
        let parsed = Cli::try_parse_from([
            "rohditor-cli",
            "quality-crops",
            "crops.json",
            "private",
            "generated",
        ])
        .expect("quality crop command parses");
        let Command::QualityCrops { demosaic, .. } = parsed.command else {
            panic!("expected quality-crops command");
        };
        assert!(matches!(demosaic, CliDemosaic::Amaze));
    }

    #[test]
    fn output_sharpening_argument_defaults_to_standard_strength() {
        assert_eq!(
            parse_output_sharpening("Screen"),
            Ok(OutputSharpening {
                medium: OutputMedium::Screen,
                strength: OutputSharpeningStrength::Standard,
            })
        );
        assert_eq!(
            parse_output_sharpening("glossy:high"),
            Ok(OutputSharpening {
                medium: OutputMedium::Glossy,
                strength: OutputSharpeningStrength::High,
            })
        );
        assert!(parse_output_sharpening("paper").is_err());
        assert!(parse_output_sharpening("screen:max").is_err());
    }
}
