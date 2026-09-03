use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::RgbImage;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use rohditor_image::{Orientation, OrientationMap};
use rohditor_raw::{RawDecoder, RawError};
use thiserror::Error;

/// Default longest edge for catalog thumbnails.
pub const DEFAULT_THUMBNAIL_LONG_EDGE: u32 = 512;

/// Default JPEG quality for persistent catalog thumbnails.
pub const DEFAULT_THUMBNAIL_JPEG_QUALITY: u8 = 85;

/// Options that affect generated thumbnail bytes and cache identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbnailOptions {
    max_long_edge: u32,
    jpeg_quality: u8,
}

impl ThumbnailOptions {
    /// Construct thumbnail options with a positive edge and JPEG quality.
    pub const fn new(max_long_edge: u32, jpeg_quality: u8) -> Result<Self, ThumbnailOptionsError> {
        if max_long_edge == 0 {
            return Err(ThumbnailOptionsError::ZeroLongEdge);
        }
        if jpeg_quality == 0 || jpeg_quality > 100 {
            return Err(ThumbnailOptionsError::InvalidJpegQuality { jpeg_quality });
        }
        Ok(Self {
            max_long_edge,
            jpeg_quality,
        })
    }

    #[must_use]
    pub const fn max_long_edge(self) -> u32 {
        self.max_long_edge
    }

    #[must_use]
    pub const fn jpeg_quality(self) -> u8 {
        self.jpeg_quality
    }
}

impl Default for ThumbnailOptions {
    fn default() -> Self {
        Self {
            max_long_edge: DEFAULT_THUMBNAIL_LONG_EDGE,
            jpeg_quality: DEFAULT_THUMBNAIL_JPEG_QUALITY,
        }
    }
}

/// Validation errors for thumbnail generation options.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ThumbnailOptionsError {
    #[error("thumbnail longest edge must be greater than zero")]
    ZeroLongEdge,

    #[error("thumbnail JPEG quality must be between 1 and 100, got {jpeg_quality}")]
    InvalidJpegQuality { jpeg_quality: u8 },
}

/// Encoded thumbnail data ready for a UI texture loader or cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    width: u32,
    height: u32,
    bytes: Arc<[u8]>,
}

impl Thumbnail {
    /// Construct encoded thumbnail data.
    ///
    /// The cache validates the encoded dimensions when storing or loading this
    /// value. Keeping construction lightweight also lets callers represent
    /// thumbnails produced by another encoder in later phases.
    pub fn new(width: u32, height: u32, bytes: Vec<u8>) -> Self {
        Self {
            width,
            height,
            bytes: Arc::from(bytes),
        }
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn into_bytes(self) -> Arc<[u8]> {
        self.bytes
    }
}

/// Why the catalog should display a placeholder instead of a thumbnail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceholderReason {
    NoEmbeddedPreview,
}

/// Result of trying to make a thumbnail from one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThumbnailOutcome {
    Ready(Thumbnail),
    Placeholder(PlaceholderReason),
}

/// A generated thumbnail outcome plus metadata read from the source.
///
/// Capture dates travel with the thumbnail so catalogs can sort without a
/// second metadata pass, including for cache hits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedThumbnail {
    pub outcome: ThumbnailOutcome,
    pub captured_at: Option<String>,
}

/// Errors while opening, decoding, orienting, or encoding a thumbnail.
#[derive(Debug, Error)]
pub enum ThumbnailError {
    #[error("could not open RAW file {path} for its catalog thumbnail: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: RawError,
    },

    #[error("could not read RAW metadata for catalog thumbnail {path}: {source}")]
    Probe {
        path: PathBuf,
        #[source]
        source: RawError,
    },

    #[error("could not extract the embedded preview for catalog thumbnail {path}: {source}")]
    EmbeddedPreview {
        path: PathBuf,
        #[source]
        source: RawError,
    },

    #[error("could not decode the embedded preview for catalog thumbnail {path}: {source}")]
    Decode {
        path: PathBuf,
        #[source]
        source: image::ImageError,
    },

    #[error("could not orient the embedded preview for catalog thumbnail {path}: {reason}")]
    Orientation { path: PathBuf, reason: String },

    #[error("could not encode catalog thumbnail {path}: {source}")]
    Encode {
        path: PathBuf,
        #[source]
        source: image::ImageError,
    },
}

/// Generates browse-sized images from embedded RAW previews.
pub struct ThumbnailGenerator<D> {
    decoder: D,
    options: ThumbnailOptions,
}

impl<D> ThumbnailGenerator<D>
where
    D: RawDecoder,
{
    #[must_use]
    pub const fn new(decoder: D, options: ThumbnailOptions) -> Self {
        Self { decoder, options }
    }

    #[must_use]
    pub const fn options(&self) -> ThumbnailOptions {
        self.options
    }

    /// Generate one thumbnail without decoding the source sensor buffer.
    pub fn generate(&self, path: impl AsRef<Path>) -> Result<GeneratedThumbnail, ThumbnailError> {
        let path = path.as_ref();
        let mut session = self
            .decoder
            .open(path)
            .map_err(|source| ThumbnailError::Open {
                path: path.to_path_buf(),
                source,
            })?;
        let info = session.probe().map_err(|source| ThumbnailError::Probe {
            path: path.to_path_buf(),
            source,
        })?;
        let captured_at = info.capture.captured_at.clone();
        let Some(preview) =
            session
                .embedded_preview()
                .map_err(|source| ThumbnailError::EmbeddedPreview {
                    path: path.to_path_buf(),
                    source,
                })?
        else {
            return Ok(GeneratedThumbnail {
                outcome: ThumbnailOutcome::Placeholder(PlaceholderReason::NoEmbeddedPreview),
                captured_at,
            });
        };

        let decoded = image::load_from_memory(&preview.bytes)
            .map_err(|source| ThumbnailError::Decode {
                path: path.to_path_buf(),
                source,
            })?
            .to_rgb8();
        let oriented = orient_rgb8(&decoded, info.orientation, path)?;
        let (width, height) = thumbnail_dimensions(
            oriented.width(),
            oriented.height(),
            self.options.max_long_edge(),
        );
        let resized = if (width, height) == oriented.dimensions() {
            oriented
        } else {
            image::imageops::resize(&oriented, width, height, FilterType::Triangle)
        };

        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, self.options.jpeg_quality())
            .encode_image(&resized)
            .map_err(|source| ThumbnailError::Encode {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(GeneratedThumbnail {
            outcome: ThumbnailOutcome::Ready(Thumbnail::new(
                resized.width(),
                resized.height(),
                bytes,
            )),
            captured_at,
        })
    }
}

fn thumbnail_dimensions(width: u32, height: u32, max_long_edge: u32) -> (u32, u32) {
    let long_edge = width.max(height);
    if long_edge <= max_long_edge {
        return (width, height);
    }
    if width >= height {
        (
            max_long_edge,
            scaled_dimension(height, max_long_edge, width),
        )
    } else {
        (
            scaled_dimension(width, max_long_edge, height),
            max_long_edge,
        )
    }
}

fn scaled_dimension(value: u32, numerator: u32, denominator: u32) -> u32 {
    let scaled = (u64::from(value) * u64::from(numerator) + u64::from(denominator) / 2)
        / u64::from(denominator);
    scaled.max(1) as u32
}

fn orient_rgb8(
    source: &RgbImage,
    orientation: Orientation,
    path: &Path,
) -> Result<RgbImage, ThumbnailError> {
    let source_width =
        usize::try_from(source.width()).map_err(|_| ThumbnailError::Orientation {
            path: path.to_path_buf(),
            reason: "preview width exceeds this system's usize".to_owned(),
        })?;
    let source_height =
        usize::try_from(source.height()).map_err(|_| ThumbnailError::Orientation {
            path: path.to_path_buf(),
            reason: "preview height exceeds this system's usize".to_owned(),
        })?;
    let orientation_map =
        OrientationMap::new(source_width, source_height, orientation).map_err(|error| {
            ThumbnailError::Orientation {
                path: path.to_path_buf(),
                reason: error.to_string(),
            }
        })?;
    let (output_width, output_height) = orientation_map.output_dimensions();
    let output_samples = output_width
        .checked_mul(output_height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| ThumbnailError::Orientation {
            path: path.to_path_buf(),
            reason: "oriented preview dimensions overflowed".to_owned(),
        })?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(output_samples)
        .map_err(|error| ThumbnailError::Orientation {
            path: path.to_path_buf(),
            reason: format!("could not allocate oriented preview: {error}"),
        })?;
    output.resize(output_samples, 0);

    for output_y in 0..output_height {
        for output_x in 0..output_width {
            let (source_x, source_y) = orientation_map
                .source_coordinate(output_x, output_y)
                .ok_or_else(|| ThumbnailError::Orientation {
                    path: path.to_path_buf(),
                    reason: "oriented preview coordinate was out of range".to_owned(),
                })?;
            let source_x = u32::try_from(source_x).map_err(|_| ThumbnailError::Orientation {
                path: path.to_path_buf(),
                reason: "oriented preview x coordinate exceeds u32".to_owned(),
            })?;
            let source_y = u32::try_from(source_y).map_err(|_| ThumbnailError::Orientation {
                path: path.to_path_buf(),
                reason: "oriented preview y coordinate exceeds u32".to_owned(),
            })?;
            let output_index = (output_y * output_width + output_x) * 3;
            output[output_index..output_index + 3]
                .copy_from_slice(&source.get_pixel(source_x, source_y).0);
        }
    }

    let output_width = u32::try_from(output_width).map_err(|_| ThumbnailError::Orientation {
        path: path.to_path_buf(),
        reason: "oriented preview width exceeds u32".to_owned(),
    })?;
    let output_height = u32::try_from(output_height).map_err(|_| ThumbnailError::Orientation {
        path: path.to_path_buf(),
        reason: "oriented preview height exceeds u32".to_owned(),
    })?;
    RgbImage::from_raw(output_width, output_height, output).ok_or_else(|| {
        ThumbnailError::Orientation {
            path: path.to_path_buf(),
            reason: "could not construct the oriented embedded preview".to_owned(),
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use image::GenericImageView;
    use image::codecs::jpeg::JpegEncoder;
    use rohditor_image::Orientation;
    use rohditor_raw::{
        CameraColorMatrix, CaptureMetadata, CfaPattern, EncodedPreview, EncodedPreviewFormat,
        ImageRect, LevelPattern, PhotometricInterpretation, RawFileInfo, RawSession,
    };

    use super::*;

    struct MockDecoder {
        info: RawFileInfo,
        preview: Option<EncodedPreview>,
    }

    struct MockSession {
        info: RawFileInfo,
        preview: Option<EncodedPreview>,
    }

    impl RawDecoder for MockDecoder {
        fn open(&self, _path: &Path) -> Result<Box<dyn RawSession>, RawError> {
            Ok(Box::new(MockSession {
                info: self.info.clone(),
                preview: self.preview.clone(),
            }))
        }
    }

    impl RawSession for MockSession {
        fn probe(&mut self) -> Result<RawFileInfo, RawError> {
            Ok(self.info.clone())
        }

        fn decode(&mut self) -> Result<rohditor_raw::RawFrame, RawError> {
            unreachable!("thumbnail generation must not decode the sensor buffer")
        }

        fn embedded_preview(&mut self) -> Result<Option<EncodedPreview>, RawError> {
            Ok(self.preview.clone())
        }
    }

    fn test_info(orientation: Orientation) -> RawFileInfo {
        RawFileInfo {
            format: "ARW".to_owned(),
            make: "Sony".to_owned(),
            model: "ILCE-6400".to_owned(),
            clean_make: "Sony".to_owned(),
            clean_model: "ILCE-6400".to_owned(),
            source_size_bytes: 1,
            source_identity: None,
            width: 2,
            height: 3,
            components_per_pixel: 1,
            source_bits_per_sample: Some(14),
            decoded_bits_per_sample: 16,
            compression: None,
            active_area: Some(ImageRect {
                x: 0,
                y: 0,
                width: 2,
                height: 3,
            }),
            crop_area: None,
            photometric_interpretation: PhotometricInterpretation::Cfa {
                pattern: CfaPattern {
                    name: "RGGB".to_owned(),
                    width: 2,
                    height: 2,
                },
            },
            black_levels: LevelPattern {
                values: vec![0.0],
                repeat_width: 1,
                repeat_height: 1,
                components_per_pixel: 1,
            },
            white_levels: vec![1.0],
            as_shot_white_balance: [None; 4],
            xyz_to_camera: [[0.0; 3]; 4],
            color_matrices: Vec::<CameraColorMatrix>::new(),
            orientation,
            capture: CaptureMetadata::default(),
            embedded_preview: None,
        }
    }

    fn encoded_preview(image: &RgbImage) -> EncodedPreview {
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, 100)
            .encode_image(image)
            .expect("encode test preview");
        EncodedPreview {
            width: image.width(),
            height: image.height(),
            color_type: "Rgb8".to_owned(),
            format: EncodedPreviewFormat::Jpeg,
            bytes: Arc::from(bytes),
            is_original_encoding: false,
        }
    }

    #[test]
    fn generates_oriented_bounded_jpeg_without_sensor_decode() {
        let source = RgbImage::from_fn(2, 3, |x, y| image::Rgb([x as u8, y as u8, 0]));
        let decoder = MockDecoder {
            info: test_info(Orientation::Rotate90),
            preview: Some(encoded_preview(&source)),
        };
        let options = ThumbnailOptions::new(2, 90).expect("valid options");
        let generator = ThumbnailGenerator::new(decoder, options);

        let generated = generator.generate("mock.ARW").expect("generate thumbnail");
        let ThumbnailOutcome::Ready(thumbnail) = generated.outcome else {
            panic!("expected generated thumbnail");
        };
        assert_eq!((thumbnail.width(), thumbnail.height()), (2, 1));
        let decoded = image::load_from_memory(thumbnail.bytes()).expect("decode thumbnail");
        assert_eq!(decoded.dimensions(), (2, 1));
    }

    #[test]
    fn missing_embedded_preview_is_a_placeholder() {
        let generator = ThumbnailGenerator::new(
            MockDecoder {
                info: test_info(Orientation::Normal),
                preview: None,
            },
            ThumbnailOptions::default(),
        );

        assert_eq!(
            generator
                .generate("missing-preview.ARW")
                .expect("generate result")
                .outcome,
            ThumbnailOutcome::Placeholder(PlaceholderReason::NoEmbeddedPreview)
        );
    }

    #[test]
    fn rejects_invalid_options() {
        assert_eq!(
            ThumbnailOptions::new(0, 85),
            Err(ThumbnailOptionsError::ZeroLongEdge)
        );
        assert_eq!(
            ThumbnailOptions::new(512, 101),
            Err(ThumbnailOptionsError::InvalidJpegQuality { jpeg_quality: 101 })
        );
    }

    #[test]
    fn orientation_mapping_preserves_asymmetric_dimensions() {
        let source = RgbImage::from_fn(2, 3, |x, y| image::Rgb([(x + y * 2) as u8, 0, 0]));
        let oriented = orient_rgb8(&source, Orientation::Rotate90, Path::new("test.ARW"))
            .expect("orient image");
        assert_eq!(oriented.dimensions(), (3, 2));
        let values = oriented
            .pixels()
            .map(|pixel| pixel.0[0])
            .collect::<Vec<_>>();
        assert_eq!(values, [4, 2, 0, 5, 3, 1]);
    }
}
