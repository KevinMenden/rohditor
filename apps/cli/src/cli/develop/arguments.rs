//! Interpreting the `develop` arguments: white balance, optics, highlights, export settings.

use std::path::Path;

use anyhow::{Context, Result, bail};
use rohditor_core::{
    CollisionPolicy, DitherMode, ExportFormat, ExportSettings, JPEG_QUALITY_DEFAULT, OutputSize,
    WhiteBalanceCoordinates,
};
use rohditor_edit::{
    HIGHLIGHT_THRESHOLD_RANGE, LensProfileSelection, OpticsAdjustments, TEMPERATURE_RANGE,
    TINT_RANGE, WhiteBalance,
};

use super::DevelopArguments;
use crate::cli::values::{CliHighlightMethod, CliPngBitDepth};

pub(super) fn white_balance_from_arguments(
    arguments: &DevelopArguments,
    as_shot: Option<WhiteBalanceCoordinates>,
) -> WhiteBalance {
    match (
        arguments.white_balance,
        arguments.temperature,
        arguments.tint,
    ) {
        (Some(value), None, 0.0) => WhiteBalance::ManualMultipliers {
            red: value.red,
            green: value.green,
            blue: value.blue,
        },
        (None, temperature, tint) if temperature.is_some() || tint != 0.0 => {
            let as_shot = as_shot.unwrap_or(WhiteBalanceCoordinates {
                temperature: TEMPERATURE_RANGE.neutral,
                tint: TINT_RANGE.neutral,
            });
            WhiteBalance::TemperatureTint {
                temperature: temperature.unwrap_or(as_shot.temperature),
                tint: if tint == TINT_RANGE.neutral {
                    as_shot.tint
                } else {
                    tint
                },
            }
        }
        (None, None, 0.0) => WhiteBalance::AsShot,
        _ => unreachable!("white balance conflict was rejected above"),
    }
}

pub(super) fn optics_adjustments(arguments: &DevelopArguments) -> Result<OpticsAdjustments> {
    Ok(OpticsAdjustments {
        profile: parse_lens_profile(&arguments.lens_profile)?,
        distortion: !arguments.no_lens_distortion,
        vignetting: !arguments.no_lens_vignetting,
        chromatic_aberration: !arguments.no_lens_tca,
    })
}

fn parse_lens_profile(value: &str) -> Result<LensProfileSelection> {
    let value = value.trim();
    if value.is_empty() {
        bail!("--lens-profile cannot be empty");
    }
    if value.eq_ignore_ascii_case("off") {
        return Ok(LensProfileSelection::Off);
    }
    if value.eq_ignore_ascii_case("auto") || value.eq_ignore_ascii_case("automatic") {
        return Ok(LensProfileSelection::Automatic);
    }
    Ok(LensProfileSelection::Lensfun {
        profile_id: value.to_owned(),
    })
}

pub(super) fn validate_highlight_options(
    method: CliHighlightMethod,
    clip_threshold: Option<f32>,
    detection_threshold: Option<f32>,
) -> Result<()> {
    if clip_threshold.is_some() && !matches!(method, CliHighlightMethod::Clip) {
        bail!("--highlight-clip-threshold requires --highlight-reconstruction clip");
    }
    if detection_threshold.is_some() && !matches!(method, CliHighlightMethod::Opposed) {
        bail!("--highlight-detection-threshold requires --highlight-reconstruction opposed");
    }
    for (name, value) in [
        ("--highlight-clip-threshold", clip_threshold),
        ("--highlight-detection-threshold", detection_threshold),
    ] {
        if let Some(value) = value
            && !HIGHLIGHT_THRESHOLD_RANGE.contains(value)
        {
            bail!(
                "{name} must be finite and within {}..={}",
                HIGHLIGHT_THRESHOLD_RANGE.minimum,
                HIGHLIGHT_THRESHOLD_RANGE.maximum
            );
        }
    }
    Ok(())
}

pub(super) fn develop_export_settings(
    output: &Path,
    arguments: &DevelopArguments,
) -> Result<ExportSettings> {
    let extension = output
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let format = if extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg")
    {
        if arguments.png_bit_depth.is_some() {
            bail!("--png-bit-depth can only be used with a .png destination");
        }
        ExportFormat::Jpeg {
            quality: arguments.jpeg_quality.unwrap_or(JPEG_QUALITY_DEFAULT),
        }
    } else if extension.eq_ignore_ascii_case("png") {
        if arguments.jpeg_quality.is_some() {
            bail!("--jpeg-quality can only be used with a .jpg or .jpeg destination");
        }
        ExportFormat::Png {
            bit_depth: arguments
                .png_bit_depth
                .unwrap_or(CliPngBitDepth::Eight)
                .into(),
        }
    } else {
        bail!(
            "developed output {} must use a .jpg, .jpeg, or .png extension",
            output.display()
        );
    };
    let settings = ExportSettings {
        format,
        dithering: if arguments.dither {
            DitherMode::Ordered8x8
        } else {
            DitherMode::None
        },
        metadata: arguments.metadata.into(),
        collision: CollisionPolicy::overwrite_if(arguments.force),
        size: match (arguments.long_edge, arguments.fit) {
            (Some(pixels), _) => OutputSize::LongEdge { pixels },
            (None, Some((width, height))) => OutputSize::Fit { width, height },
            (None, None) => OutputSize::Original,
        },
        sharpening: arguments.output_sharpen,
    };
    settings
        .validate_destination(output)
        .context("could not validate export settings")?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::str::FromStr;

    use clap::Parser;

    use super::*;
    use crate::cli::args::{Cli, Command};
    use crate::cli::model::NeuralArgs;
    use crate::cli::values::{
        CliCropPolicy, CliDemosaic, CliHighlightMethod, CliMetadata, CliOutputGamut,
        CliRenderingProfile, ExportProcessor, RgbMultipliers,
    };

    #[test]
    fn develop_selects_export_format_and_parses_three_relative_wb_values() {
        let base = DevelopArguments {
            processor: ExportProcessor::Cpu,
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            tone_points: Vec::new(),
            saturation: 1.0,
            vibrance: 0.0,
            highlight_reconstruction: CliHighlightMethod::Off,
            highlight_clip_threshold: None,
            highlight_detection_threshold: None,
            white_balance: None,
            camera_profile: None,
            rendering_profile: CliRenderingProfile::Standard,
            false_color_steps: 0,
            defringe_purple: 0.0,
            defringe_purple_hue: [270.0, 330.0],
            defringe_green: 0.0,
            defringe_green_hue: [75.0, 150.0],
            chroma_nr: 0.0,
            luma_nr: 0.0,
            luma_nr_detail: 0.5,
            capture_sharpening: false,
            capture_amount: 0.5,
            capture_radius: 0.6,
            capture_noise_protection: 0.5,
            output_gamut: CliOutputGamut::Clip,
            temperature: None,
            tint: 0.0,
            crop: CliCropPolicy::Recommended,
            demosaic: CliDemosaic::Bilinear,
            neural: NeuralArgs::default(),
            lens_profile: "off".to_owned(),
            no_lens_distortion: false,
            no_lens_vignetting: false,
            no_lens_tca: false,
            orientation: None,
            jpeg_quality: None,
            png_bit_depth: None,
            long_edge: None,
            fit: None,
            output_sharpen: None,
            dither: false,
            metadata: CliMetadata::Safe,
            force: false,
        };
        assert!(develop_export_settings(Path::new("developed.PNG"), &base).is_ok());
        assert!(develop_export_settings(Path::new("developed.jpg"), &base).is_ok());
        assert!(develop_export_settings(Path::new("developed.tiff"), &base).is_err());

        let values = RgbMultipliers::from_str("1.2,1.0,0.8").expect("valid multipliers");
        assert_eq!((values.red, values.green, values.blue), (1.2, 1.0, 0.8));
        assert!(RgbMultipliers::from_str("1.0,1.0").is_err());

        let mut custom = base.clone();
        custom.temperature = Some(4_500.0);
        assert_eq!(
            white_balance_from_arguments(
                &custom,
                Some(rohditor_core::WhiteBalanceCoordinates {
                    temperature: 5_800.0,
                    tint: -0.15,
                }),
            ),
            WhiteBalance::TemperatureTint {
                temperature: 4_500.0,
                tint: -0.15,
            }
        );
        custom.temperature = None;
        custom.tint = 0.2;
        assert_eq!(
            white_balance_from_arguments(
                &custom,
                Some(rohditor_core::WhiteBalanceCoordinates {
                    temperature: 5_800.0,
                    tint: -0.15,
                }),
            ),
            WhiteBalance::TemperatureTint {
                temperature: 5_800.0,
                tint: 0.2,
            }
        );
    }

    #[test]
    fn lens_profile_values_and_component_flags_parse() {
        assert!(matches!(
            parse_lens_profile("off").expect("off should parse"),
            rohditor_edit::LensProfileSelection::Off
        ));
        assert!(matches!(
            parse_lens_profile("AUTO").expect("auto should parse"),
            rohditor_edit::LensProfileSelection::Automatic
        ));
        let explicit =
            parse_lens_profile("lensfun:sony:profile").expect("opaque profile IDs should parse");
        assert!(matches!(
            explicit,
            rohditor_edit::LensProfileSelection::Lensfun { profile_id }
                if profile_id == "lensfun:sony:profile"
        ));
        assert!(parse_lens_profile(" ").is_err());

        let parsed = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--lens-profile",
            "auto",
            "--no-lens-distortion",
            "--no-lens-tca",
        ])
        .expect("lens profile flags should parse");
        let Command::Develop {
            lens_profile,
            no_lens_distortion,
            no_lens_vignetting,
            no_lens_tca,
            ..
        } = parsed.command
        else {
            panic!("expected develop command");
        };
        assert_eq!(lens_profile, "auto");
        assert!(no_lens_distortion);
        assert!(!no_lens_vignetting);
        assert!(no_lens_tca);
    }

    #[test]
    fn highlight_cli_options_require_clip_and_validate_the_threshold() {
        let parsed = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--highlight-reconstruction",
            "clip",
            "--highlight-clip-threshold",
            "1.25",
        ])
        .expect("clip threshold options should parse");
        let Command::Develop {
            highlight_reconstruction,
            highlight_clip_threshold,
            highlight_detection_threshold,
            ..
        } = parsed.command
        else {
            panic!("expected develop command");
        };
        assert!(matches!(highlight_reconstruction, CliHighlightMethod::Clip));
        assert_eq!(highlight_clip_threshold, Some(1.25));
        assert_eq!(highlight_detection_threshold, None);
        assert!(
            validate_highlight_options(highlight_reconstruction, highlight_clip_threshold, None)
                .is_ok()
        );
        assert!(validate_highlight_options(CliHighlightMethod::Off, Some(1.0), None).is_err());
        assert!(validate_highlight_options(CliHighlightMethod::Clip, Some(2.0), None).is_err());
        assert!(
            validate_highlight_options(CliHighlightMethod::Clip, Some(f32::NAN), None).is_err()
        );

        // #67: the removed method is rejected outright rather than silently
        // aliasing to a different reconstruction algorithm.
        assert!(
            Cli::try_parse_from([
                "rohditor-cli",
                "develop",
                "input.arw",
                "output.jpg",
                "--highlight-reconstruction",
                "local-ratios",
            ])
            .is_err(),
            "the removed local-ratios method must not parse"
        );

        let opposed = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--highlight-reconstruction",
            "opposed",
            "--highlight-detection-threshold",
            "0.8",
        ])
        .expect("Opposed threshold options should parse");
        let Command::Develop {
            highlight_reconstruction,
            highlight_detection_threshold,
            ..
        } = opposed.command
        else {
            panic!("expected develop command");
        };
        assert!(matches!(
            highlight_reconstruction,
            CliHighlightMethod::Opposed
        ));
        assert_eq!(highlight_detection_threshold, Some(0.8));
        assert!(
            validate_highlight_options(
                highlight_reconstruction,
                None,
                highlight_detection_threshold,
            )
            .is_ok()
        );
    }
}
