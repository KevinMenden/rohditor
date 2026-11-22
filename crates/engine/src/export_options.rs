//! What a user chooses in an export dialog, and how it maps to the core
//! [`ExportSettings`]. Toolkit-free, so every frontend offers the same choices
//! and defaults.

use std::path::Path;

use rohditor_core::{
    DitherMode, ExportFormat, ExportMetadataPolicy, ExportSettings, OutputSharpening, OutputSize,
    PngBitDepth,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExportKind {
    #[default]
    Jpeg,
    Png,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PngDepth {
    #[default]
    Eight,
    Sixteen,
}

/// Which size control the dialog shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SizeMode {
    #[default]
    Original,
    LongEdge,
    Fit,
}

/// The export dialog's editable state.
#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub kind: ExportKind,
    pub jpeg_quality: u8,
    pub png_depth: PngDepth,
    pub size_mode: SizeMode,
    pub long_edge: u32,
    pub fit_width: u32,
    pub fit_height: u32,
    /// Export-only; `None` is off.
    pub sharpening: Option<OutputSharpening>,
    pub dither: bool,
    pub safe_metadata: bool,
    pub overwrite: bool,
}

impl ExportOptions {
    pub const fn with_jpeg_quality(jpeg_quality: u8) -> Self {
        Self {
            kind: ExportKind::Jpeg,
            jpeg_quality,
            png_depth: PngDepth::Eight,
            size_mode: SizeMode::Original,
            long_edge: 2048,
            fit_width: 2048,
            fit_height: 2048,
            sharpening: None,
            dither: false,
            safe_metadata: true,
            overwrite: false,
        }
    }

    /// The core settings an export job runs with.
    pub fn core(&self) -> ExportSettings {
        ExportSettings {
            format: match self.kind {
                ExportKind::Jpeg => ExportFormat::Jpeg {
                    quality: self.jpeg_quality,
                },
                ExportKind::Png => ExportFormat::Png {
                    bit_depth: match self.png_depth {
                        PngDepth::Eight => PngBitDepth::Eight,
                        PngDepth::Sixteen => PngBitDepth::Sixteen,
                    },
                },
            },
            dithering: if self.dither {
                DitherMode::Ordered8x8
            } else {
                DitherMode::None
            },
            metadata: if self.safe_metadata {
                ExportMetadataPolicy::Safe
            } else {
                ExportMetadataPolicy::None
            },
            overwrite: self.overwrite,
            size: match self.size_mode {
                SizeMode::Original => OutputSize::Original,
                SizeMode::LongEdge => OutputSize::LongEdge {
                    pixels: self.long_edge,
                },
                SizeMode::Fit => OutputSize::Fit {
                    width: self.fit_width,
                    height: self.fit_height,
                },
            },
            sharpening: self.sharpening,
        }
    }

    pub const fn extension(&self) -> &'static str {
        match self.kind {
            ExportKind::Jpeg => "jpg",
            ExportKind::Png => "png",
        }
    }

    /// A file name for exporting `source`: only its stem is used, so a private
    /// directory name never leaks into the suggestion.
    pub fn suggested_file_name(&self, source: &Path) -> String {
        format!("{}-edited.{}", source_stem(source), self.extension())
    }
}

fn source_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("developed")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_map_to_ui_independent_core_settings() {
        let settings = ExportOptions {
            kind: ExportKind::Png,
            jpeg_quality: 42,
            png_depth: PngDepth::Sixteen,
            size_mode: SizeMode::Fit,
            long_edge: 1000,
            fit_width: 1200,
            fit_height: 800,
            sharpening: None,
            dither: true,
            safe_metadata: false,
            overwrite: true,
        }
        .core();

        assert_eq!(
            settings.format,
            ExportFormat::Png {
                bit_depth: PngBitDepth::Sixteen
            }
        );
        assert_eq!(settings.dithering, DitherMode::Ordered8x8);
        assert_eq!(settings.metadata, ExportMetadataPolicy::None);
        assert!(settings.overwrite);
        assert_eq!(
            settings.size,
            OutputSize::Fit {
                width: 1200,
                height: 800
            }
        );
    }

    #[test]
    fn export_name_uses_only_the_source_stem() {
        let options = ExportOptions::with_jpeg_quality(90);
        assert_eq!(
            options.suggested_file_name(Path::new("/private/photos/DSC00001.ARW")),
            "DSC00001-edited.jpg"
        );
        assert_eq!(
            options.suggested_file_name(Path::new(".hidden")),
            ".hidden-edited.jpg"
        );
    }
}
