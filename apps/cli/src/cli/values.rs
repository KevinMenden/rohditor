//! Value enums and small argument types shared by the clap definitions and the commands.

use std::str::FromStr;

use anyhow::Result;
use clap::ValueEnum;
use rohditor_core::{ExportMetadataPolicy, OutputPolicy, PngBitDepth, RawCropPolicy};
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_edit::{HighlightMethod, RenderingProfileSelection};
use rohditor_gpu::ExportBackend;
use rohditor_image::Orientation;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(super) enum CliCropPolicy {
    ActiveArea,
    Recommended,
}

impl From<CliCropPolicy> for RawCropPolicy {
    fn from(value: CliCropPolicy) -> Self {
        match value {
            CliCropPolicy::ActiveArea => Self::ActiveArea,
            CliCropPolicy::Recommended => Self::Recommended,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(super) enum CliDemosaic {
    Bilinear,
    #[value(name = "mhc")]
    MalvarHeCutler,
    #[value(name = "rcd")]
    Rcd,
    #[value(name = "amaze")]
    Amaze,
    /// Joint demosaic + denoise network; needs the installed model
    /// (`rohditor-cli model install <FILE>`) or `--neural-model`.
    #[value(name = "neural")]
    Neural,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub(super) enum CliHighlightMethod {
    #[default]
    Off,
    Clip,
    #[value(name = "opposed")]
    Opposed,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub(super) enum CliRenderingProfile {
    Standard,
    Neutral,
    /// Tone and colour fitted to the camera's embedded JPEG; falls back to a
    /// typical camera look when the fit is not possible.
    #[default]
    Matched,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub(super) enum CliOutputGamut {
    #[default]
    Clip,
    #[value(name = "chroma-compress")]
    ChromaCompress,
}

impl From<CliOutputGamut> for OutputPolicy {
    fn from(value: CliOutputGamut) -> Self {
        match value {
            CliOutputGamut::Clip => Self::ClipToSrgb,
            CliOutputGamut::ChromaCompress => Self::ChromaCompressToSrgb,
        }
    }
}

impl From<CliRenderingProfile> for RenderingProfileSelection {
    fn from(value: CliRenderingProfile) -> Self {
        match value {
            CliRenderingProfile::Standard => Self::STANDARD,
            CliRenderingProfile::Neutral => Self::NEUTRAL,
            CliRenderingProfile::Matched => Self::MATCHED,
        }
    }
}

impl From<CliHighlightMethod> for HighlightMethod {
    fn from(value: CliHighlightMethod) -> Self {
        match value {
            CliHighlightMethod::Off => Self::Off,
            CliHighlightMethod::Clip => Self::Clip,
            CliHighlightMethod::Opposed => Self::Opposed,
        }
    }
}

impl CliDemosaic {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Bilinear => "bilinear",
            Self::MalvarHeCutler => "mhc",
            Self::Rcd => "rcd",
            Self::Amaze => "amaze",
            Self::Neural => "neural",
        }
    }
}

impl From<CliDemosaic> for DemosaicAlgorithm {
    fn from(value: CliDemosaic) -> Self {
        match value {
            CliDemosaic::Bilinear => Self::Bilinear,
            CliDemosaic::MalvarHeCutler => Self::MalvarHeCutler,
            CliDemosaic::Rcd => Self::Rcd,
            CliDemosaic::Amaze => Self::Amaze,
            CliDemosaic::Neural => Self::Neural,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(super) enum CliOrientation {
    Normal,
    HorizontalFlip,
    Rotate180,
    VerticalFlip,
    Transpose,
    Rotate90,
    Transverse,
    Rotate270,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(super) enum CliPngBitDepth {
    #[value(name = "8", alias = "eight")]
    Eight,
    #[value(name = "16", alias = "sixteen")]
    Sixteen,
}

impl From<CliPngBitDepth> for PngBitDepth {
    fn from(value: CliPngBitDepth) -> Self {
        match value {
            CliPngBitDepth::Eight => Self::Eight,
            CliPngBitDepth::Sixteen => Self::Sixteen,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub(super) enum CliMetadata {
    None,
    #[default]
    Safe,
}

impl From<CliMetadata> for ExportMetadataPolicy {
    fn from(value: CliMetadata) -> Self {
        match value {
            CliMetadata::None => Self::None,
            CliMetadata::Safe => Self::Safe,
        }
    }
}

impl From<CliOrientation> for Orientation {
    fn from(value: CliOrientation) -> Self {
        match value {
            CliOrientation::Normal => Self::Normal,
            CliOrientation::HorizontalFlip => Self::HorizontalFlip,
            CliOrientation::Rotate180 => Self::Rotate180,
            CliOrientation::VerticalFlip => Self::VerticalFlip,
            CliOrientation::Transpose => Self::Transpose,
            CliOrientation::Rotate90 => Self::Rotate90,
            CliOrientation::Transverse => Self::Transverse,
            CliOrientation::Rotate270 => Self::Rotate270,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RgbMultipliers {
    pub(super) red: f32,
    pub(super) green: f32,
    pub(super) blue: f32,
}

impl FromStr for RgbMultipliers {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let values = value
            .split(',')
            .map(|component| {
                component
                    .parse::<f32>()
                    .map_err(|error| format!("invalid RGB multiplier {component:?}: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let [red, green, blue] = values.as_slice() else {
            return Err("expected exactly three comma-separated values: RED,GREEN,BLUE".to_owned());
        };
        Ok(Self {
            red: *red,
            green: *green,
            blue: *blue,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(super) enum ExportProcessor {
    Auto,
    Cpu,
    Gpu,
}

impl From<ExportProcessor> for ExportBackend {
    fn from(processor: ExportProcessor) -> Self {
        match processor {
            ExportProcessor::Auto => Self::PreferGpu,
            ExportProcessor::Cpu => Self::Cpu,
            ExportProcessor::Gpu => Self::RequireGpu,
        }
    }
}
