//! CLI facade: parses the arguments and dispatches to one module per subcommand.

mod args;
mod demosaic_npy;
mod detail_compare;
mod detail_corpus;
mod develop;
mod extract_preview;
mod highlight_compare;
mod inspect;
mod look_report;
mod model;
mod neural_input;
mod npy;
mod output;
mod quality_crops;
mod values;
mod verify_libraw;

use std::io::{self};

use anyhow::Result;
use clap::Parser;
use rohditor_edit::NoiseReduction;
use tracing_subscriber::EnvFilter;

use args::{Cli, Command};
use demosaic_npy::{DemosaicNpy, demosaic_npy};
use detail_compare::{StageSettings, detail_compare};
use detail_corpus::detail_corpus;
use develop::{DevelopArguments, develop};
use extract_preview::extract_preview;
use highlight_compare::highlight_compare;
use inspect::inspect;
use look_report::look_report;
use model::model;
use neural_input::neural_input_npy;
use quality_crops::quality_crops;
use values::CliDemosaic;
use verify_libraw::verify_libraw;

pub(crate) fn run() -> Result<()> {
    initialize_tracing();
    let cli = Cli::parse();

    match cli.command {
        Command::Inspect {
            file,
            json,
            metadata_only,
        } => inspect(&file, json, metadata_only),
        Command::ExtractPreview {
            file,
            output,
            force,
        } => extract_preview(&file, &output, force),
        Command::Develop {
            file,
            output,
            exposure,
            contrast,
            highlights,
            shadows,
            whites,
            blacks,
            tone_points,
            saturation,
            vibrance,
            highlight_reconstruction,
            highlight_clip_threshold,
            highlight_detection_threshold,
            white_balance,
            camera_profile,
            rendering_profile,
            false_color_steps,
            defringe_purple,
            defringe_purple_hue,
            defringe_green,
            defringe_green_hue,
            chroma_nr,
            luma_nr,
            luma_nr_detail,
            capture_sharpening,
            capture_amount,
            capture_radius,
            capture_noise_protection,
            output_gamut,
            processor,
            temperature,
            tint,
            crop,
            demosaic,
            neural,
            lens_profile,
            no_lens_distortion,
            no_lens_vignetting,
            no_lens_tca,
            orientation,
            jpeg_quality,
            png_bit_depth,
            long_edge,
            fit,
            output_sharpen,
            dither,
            metadata,
            force,
        } => develop(
            &file,
            &output,
            DevelopArguments {
                exposure,
                contrast,
                highlights,
                shadows,
                whites,
                blacks,
                tone_points,
                saturation,
                vibrance,
                highlight_reconstruction,
                highlight_clip_threshold,
                highlight_detection_threshold,
                white_balance,
                camera_profile,
                rendering_profile,
                false_color_steps,
                defringe_purple,
                defringe_purple_hue,
                defringe_green,
                defringe_green_hue,
                chroma_nr,
                luma_nr,
                luma_nr_detail,
                capture_sharpening,
                capture_amount,
                capture_radius,
                capture_noise_protection,
                output_gamut,
                processor,
                temperature,
                tint,
                crop,
                demosaic,
                neural,
                lens_profile,
                no_lens_distortion,
                no_lens_vignetting,
                no_lens_tca,
                orientation,
                jpeg_quality,
                png_bit_depth,
                long_edge,
                fit,
                output_sharpen,
                dither,
                metadata,
                force,
            },
        ),
        Command::QualityCrops {
            manifest,
            corpus,
            output,
            demosaic,
            force,
        } => quality_crops(&manifest, &corpus, &output, demosaic, force),
        Command::DetailCompare {
            stage,
            strength,
            detail,
            steps,
            demosaic,
            raw_dir,
        } => detail_compare(
            stage,
            StageSettings {
                strength,
                detail,
                steps,
                demosaic: demosaic.into(),
            },
            raw_dir.as_deref(),
        ),
        Command::DetailCorpus {
            manifest,
            corpus,
            output,
            demosaic,
            force,
        } => detail_corpus(&manifest, &corpus, &output, demosaic.into(), force),
        Command::HighlightCompare {
            manifest,
            corpus,
            output,
            demosaic,
            full_exports,
            force,
        } => highlight_compare(
            &manifest,
            &corpus,
            &output,
            demosaic.into(),
            full_exports,
            force,
        ),
        Command::LookReport {
            files,
            camera_profile,
            json,
        } => look_report(&files, camera_profile.as_deref(), json),
        Command::VerifyLibraw {
            file,
            libraw_pgm,
            json,
            max_error,
            max_rmse,
        } => verify_libraw(&file, &libraw_pgm, json, max_error, max_rmse),
        Command::DemosaicNpy {
            input,
            wb,
            output,
            demosaic,
            neural,
            chroma,
            luminance,
            luminance_detail,
            force,
        } => demosaic_npy(&DemosaicNpy {
            input: &input,
            wb: &wb,
            output: &output,
            algorithm: demosaic.into(),
            neural: match demosaic {
                CliDemosaic::Neural => Some(neural.load()?),
                _ => None,
            },
            noise_reduction: NoiseReduction {
                chroma,
                luminance,
                luminance_detail,
            },
            force,
        }),
        Command::NeuralInputNpy {
            file,
            output,
            force,
        } => neural_input_npy(&file, &output, force),
        Command::Model { command } => model(&command),
    }
}

fn initialize_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("error"));
    let _subscriber_result = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(io::stderr)
        .try_init();
}
