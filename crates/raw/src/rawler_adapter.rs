use std::any::Any;
use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use image::ImageDecoder as _;
use rawler::decoders::{Decoder, FormatHint, RawDecodeParams, RawMetadata};
use rawler::formats::tiff::reader::{GenericTiffReader, TiffReader};
use rawler::imgop::Rect;
use rawler::rawimage::{RawImage, RawImageData, RawPhotometricInterpretation};
use rawler::rawsource::RawSource;
use rawler::tags::{ExifTag, TiffCommonTag};
use rawler::{Orientation, RawlerError};
use tracing::{info_span, warn};

use crate::{
    CameraColorMatrix, CaptureMetadata, CfaPattern, DecoderLimits, EmbeddedPreviewInfo,
    EncodedPreview, EncodedPreviewFormat, ImageRect, LevelPattern, PhotometricInterpretation,
    RationalValue, RawDecoder, RawError, RawFileInfo, RawFrame, RawOrientation, RawSession,
    SourceIdentity,
};

/// `rawler` implementation of Rohditor's private decoder boundary.
#[derive(Debug, Clone, Copy, Default)]
pub struct RawlerDecoder {
    limits: DecoderLimits,
}

impl RawlerDecoder {
    #[must_use]
    pub const fn new(limits: DecoderLimits) -> Self {
        Self { limits }
    }

    fn open_impl(&self, path: &Path) -> Result<RawlerSession, RawError> {
        let metadata = source_metadata(path)?;
        let source_identity = map_source_identity(&metadata);
        if source_identity.size_bytes > self.limits.max_source_bytes {
            return Err(RawError::SourceTooLarge {
                path: path.to_path_buf(),
                actual_bytes: source_identity.size_bytes,
                max_bytes: self.limits.max_source_bytes,
            });
        }
        let source = open_source(path)?;
        let decoder =
            rawler::get_decoder(&source).map_err(|error| map_rawler_error(path, error))?;
        Ok(RawlerSession {
            limits: self.limits,
            path: path.to_path_buf(),
            source,
            decoder,
            params: RawDecodeParams::default(),
            source_identity,
            info: None,
            preview: PreviewState::default(),
        })
    }
}

struct RawlerSession {
    limits: DecoderLimits,
    path: PathBuf,
    source: RawSource,
    decoder: Box<dyn Decoder>,
    params: RawDecodeParams,
    source_identity: SourceIdentity,
    info: Option<RawFileInfo>,
    preview: PreviewState,
}

#[derive(Debug, Default)]
enum PreviewState {
    #[default]
    Unloaded,
    Missing,
    Present(EncodedPreview),
}

impl RawlerSession {
    fn probe_impl(&mut self) -> Result<RawFileInfo, RawError> {
        if let Some(info) = &self.info {
            return Ok(info.clone());
        }

        let raw_image = self
            .decoder
            .raw_image(&self.source, &self.params, true)
            .map_err(|error| map_rawler_error(&self.path, error))?;
        self.validate_dimensions(&raw_image)?;
        let metadata = self
            .decoder
            .raw_metadata(&self.source, &self.params)
            .map_err(|error| map_rawler_error(&self.path, error))?;
        let format_hint = self.decoder.format_hint();
        let encoding = source_encoding(&self.source, format_hint, &self.path)?;
        let preview = match self.preview_impl() {
            Ok(preview) => preview.map(|preview| EmbeddedPreviewInfo {
                width: preview.width,
                height: preview.height,
                color_type: preview.color_type,
            }),
            Err(error) => {
                warn!(error = %error, "ignoring unusable optional embedded preview");
                None
            }
        };

        let info = map_file_info(
            &raw_image,
            &metadata,
            self.source_identity,
            preview,
            encoding,
            format_hint,
        );
        self.info = Some(info.clone());
        Ok(info)
    }

    fn decode_impl(&mut self) -> Result<RawFrame, RawError> {
        // Probe first so dimensions are rejected before rawler allocates the
        // complete sensor buffer. The resulting metadata is cached in this
        // session and describes the same mapped source as the decoded pixels.
        let info = self.probe_impl()?;
        let raw_image = self
            .decoder
            .raw_image(&self.source, &self.params, false)
            .map_err(|error| map_rawler_error(&self.path, error))?;
        let expected_samples = self.validate_dimensions(&raw_image)?;
        let row_stride = raw_image
            .width
            .checked_mul(raw_image.cpp)
            .ok_or_else(|| invalid_dimensions(&self.path, &raw_image, self.limits))?;

        let mosaic = match raw_image.data {
            RawImageData::Integer(samples) => {
                if samples.len() != expected_samples {
                    return Err(RawError::InvalidSampleCount {
                        path: self.path.clone(),
                        expected: expected_samples,
                        actual: samples.len(),
                    });
                }
                Arc::from(samples)
            }
            RawImageData::Float(_) => {
                return Err(RawError::UnsupportedPixelData {
                    path: self.path.clone(),
                });
            }
        };

        Ok(RawFrame {
            info,
            row_stride,
            mosaic,
        })
    }

    fn preview_impl(&mut self) -> Result<Option<EncodedPreview>, RawError> {
        match &self.preview {
            PreviewState::Missing => return Ok(None),
            PreviewState::Present(preview) => return Ok(Some(preview.clone())),
            PreviewState::Unloaded => {}
        }
        let preview = encoded_preview(
            self.decoder.as_ref(),
            &self.source,
            &self.params,
            self.decoder.format_hint(),
            &self.path,
            self.limits,
        )?;
        self.preview = preview.as_ref().map_or(PreviewState::Missing, |preview| {
            PreviewState::Present(preview.clone())
        });
        Ok(preview)
    }

    fn validate_dimensions(&self, image: &RawImage) -> Result<usize, RawError> {
        let samples = image
            .width
            .checked_mul(image.height)
            .and_then(|pixels| pixels.checked_mul(image.cpp));
        match samples {
            Some(sample_count)
                if image.width > 0
                    && image.height > 0
                    && image.cpp > 0
                    && image.width <= self.limits.max_width
                    && image.height <= self.limits.max_height
                    && sample_count <= self.limits.max_samples =>
            {
                Ok(sample_count)
            }
            _ => Err(invalid_dimensions(&self.path, image, self.limits)),
        }
    }
}

impl RawDecoder for RawlerDecoder {
    fn open(&self, path: &Path) -> Result<Box<dyn RawSession>, RawError> {
        let span = info_span!("raw.open", file = %file_name(path));
        let _guard = span.enter();
        catch_decoder_panic(path, "opening", || {
            self.open_impl(path)
                .map(|session| Box::new(session) as Box<dyn RawSession>)
        })
    }
}

impl RawSession for RawlerSession {
    fn probe(&mut self) -> Result<RawFileInfo, RawError> {
        let path = self.path.clone();
        let span = info_span!("raw.probe", file = %file_name(&path));
        let _guard = span.enter();
        catch_decoder_panic(&path, "probing", || self.probe_impl())
    }

    fn decode(&mut self) -> Result<RawFrame, RawError> {
        let path = self.path.clone();
        let span = info_span!("raw.decode", file = %file_name(&path));
        let _guard = span.enter();
        catch_decoder_panic(&path, "decoding", || self.decode_impl())
    }

    fn embedded_preview(&mut self) -> Result<Option<EncodedPreview>, RawError> {
        let path = self.path.clone();
        let span = info_span!("raw.embedded_preview", file = %file_name(&path));
        let _guard = span.enter();
        catch_decoder_panic(&path, "extracting the preview from", || self.preview_impl())
    }
}

fn open_source(path: &Path) -> Result<RawSource, RawError> {
    RawSource::new(path).map_err(|source| RawError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn source_metadata(path: &Path) -> Result<std::fs::Metadata, RawError> {
    std::fs::metadata(path).map_err(|source| RawError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn map_source_identity(metadata: &std::fs::Metadata) -> SourceIdentity {
    let modified_unix_nanos = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
    let (filesystem_device, filesystem_inode) = filesystem_identity(metadata);
    SourceIdentity {
        size_bytes: metadata.len(),
        modified_unix_nanos,
        filesystem_device,
        filesystem_inode,
    }
}

#[cfg(unix)]
fn filesystem_identity(metadata: &std::fs::Metadata) -> (Option<u64>, Option<u64>) {
    use std::os::unix::fs::MetadataExt as _;

    (Some(metadata.dev()), Some(metadata.ino()))
}

#[cfg(not(unix))]
const fn filesystem_identity(_metadata: &std::fs::Metadata) -> (Option<u64>, Option<u64>) {
    (None, None)
}

fn invalid_dimensions(path: &Path, image: &RawImage, limits: DecoderLimits) -> RawError {
    RawError::InvalidDimensions {
        path: path.to_path_buf(),
        width: image.width,
        height: image.height,
        components: image.cpp,
        max_width: limits.max_width,
        max_height: limits.max_height,
        max_samples: limits.max_samples,
    }
}

fn map_rawler_error(path: &Path, error: RawlerError) -> RawError {
    match error {
        RawlerError::Unsupported {
            what,
            model,
            make,
            mode,
        } => RawError::Unsupported {
            path: path.to_path_buf(),
            reason: format!("{what}; make={make}, model={model}, mode={mode}"),
        },
        RawlerError::DecoderFailed(reason) => RawError::Corrupt {
            path: path.to_path_buf(),
            reason,
        },
    }
}

fn encoded_preview(
    decoder: &dyn Decoder,
    source: &RawSource,
    params: &RawDecodeParams,
    format_hint: FormatHint,
    path: &Path,
    limits: DecoderLimits,
) -> Result<Option<EncodedPreview>, RawError> {
    if format_hint == FormatHint::ARW {
        let Some(bytes) = sony_embedded_jpeg(source, path)? else {
            return Ok(None);
        };
        validate_preview_byte_count(bytes.len(), path, limits)?;
        let decoder =
            image::codecs::jpeg::JpegDecoder::new(Cursor::new(bytes)).map_err(|error| {
                RawError::Corrupt {
                    path: path.to_path_buf(),
                    reason: format!("embedded JPEG header cannot be decoded: {error}"),
                }
            })?;
        let (width, height) = decoder.dimensions();
        validate_preview_dimensions(width as usize, height as usize, path, limits)?;
        let color_type = format!("{:?}", decoder.color_type());

        return Ok(Some(EncodedPreview {
            width,
            height,
            color_type,
            format: EncodedPreviewFormat::Jpeg,
            bytes: Arc::from(bytes),
            is_original_encoding: true,
        }));
    }

    let Some(image) = decoded_preview(decoder, source, params, format_hint, path)? else {
        return Ok(None);
    };
    let width = image.width();
    let height = image.height();
    validate_preview_dimensions(width as usize, height as usize, path, limits)?;
    let color_type = format!("{:?}", image.color());
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90)
        .encode_image(&image)
        .map_err(|error| RawError::Decode {
            path: path.to_path_buf(),
            reason: format!("could not normalize the embedded preview as JPEG: {error}"),
        })?;
    validate_preview_byte_count(bytes.len(), path, limits)?;

    Ok(Some(EncodedPreview {
        width,
        height,
        color_type,
        format: EncodedPreviewFormat::Jpeg,
        bytes: Arc::from(bytes),
        is_original_encoding: false,
    }))
}

fn validate_preview_byte_count(
    bytes: usize,
    path: &Path,
    limits: DecoderLimits,
) -> Result<(), RawError> {
    if bytes <= limits.max_preview_bytes {
        Ok(())
    } else {
        Err(RawError::PreviewTooLarge {
            path: path.to_path_buf(),
            actual_bytes: bytes,
            max_bytes: limits.max_preview_bytes,
        })
    }
}

fn validate_preview_dimensions(
    width: usize,
    height: usize,
    path: &Path,
    limits: DecoderLimits,
) -> Result<(), RawError> {
    let pixels = width.checked_mul(height);
    if width > 0
        && height > 0
        && width <= limits.max_preview_width
        && height <= limits.max_preview_height
        && pixels.is_some_and(|pixels| pixels <= limits.max_preview_pixels)
    {
        Ok(())
    } else {
        Err(RawError::InvalidPreviewDimensions {
            path: path.to_path_buf(),
            width,
            height,
            max_width: limits.max_preview_width,
            max_height: limits.max_preview_height,
            max_pixels: limits.max_preview_pixels,
        })
    }
}

fn sony_embedded_jpeg<'a>(
    source: &'a RawSource,
    path: &Path,
) -> Result<Option<&'a [u8]>, RawError> {
    let tiff = GenericTiffReader::new_with_buffer(source.buf(), 0, 0, None).map_err(|error| {
        RawError::Corrupt {
            path: path.to_path_buf(),
            reason: format!("could not inspect the embedded-preview TIFF tags: {error}"),
        }
    })?;
    let root = tiff.root_ifd();
    let offset = root.get_entry(ExifTag::JPEGInterchangeFormat);
    let length = root.get_entry(ExifTag::JPEGInterchangeFormatLength);
    let (Some(offset), Some(length)) = (offset, length) else {
        if offset.is_some() || length.is_some() {
            return Err(RawError::Corrupt {
                path: path.to_path_buf(),
                reason: "embedded JPEG has an offset or length tag, but not both".to_owned(),
            });
        }
        return Ok(None);
    };

    let offset = usize::try_from(offset.force_u64(0)).map_err(|_| RawError::Corrupt {
        path: path.to_path_buf(),
        reason: "embedded JPEG offset cannot be represented on this system".to_owned(),
    })?;
    let length = usize::try_from(length.force_u64(0)).map_err(|_| RawError::Corrupt {
        path: path.to_path_buf(),
        reason: "embedded JPEG length cannot be represented on this system".to_owned(),
    })?;
    let end = offset
        .checked_add(length)
        .ok_or_else(|| RawError::Corrupt {
            path: path.to_path_buf(),
            reason: "embedded JPEG byte range overflows".to_owned(),
        })?;
    if length == 0 {
        return Err(RawError::Corrupt {
            path: path.to_path_buf(),
            reason: "embedded JPEG has zero length".to_owned(),
        });
    }
    source
        .buf()
        .get(offset..end)
        .map(Some)
        .ok_or_else(|| RawError::Corrupt {
            path: path.to_path_buf(),
            reason: format!(
                "embedded JPEG range {offset}..{end} exceeds the {}-byte file",
                source.buf().len()
            ),
        })
}

fn decoded_preview(
    decoder: &dyn Decoder,
    source: &RawSource,
    params: &RawDecodeParams,
    format_hint: FormatHint,
    path: &Path,
) -> Result<Option<image::DynamicImage>, RawError> {
    // Sony's decoder exposes the embedded JPEG through `full_image`; calling
    // its default `preview_image` first only emits a misleading warning.
    if format_hint == FormatHint::ARW {
        return decoder
            .full_image(source, params)
            .map_err(|error| map_rawler_error(path, error));
    }
    let preview = decoder
        .preview_image(source, params)
        .map_err(|error| map_rawler_error(path, error))?;
    if preview.is_some() {
        return Ok(preview);
    }
    decoder
        .full_image(source, params)
        .map_err(|error| map_rawler_error(path, error))
}

#[derive(Debug, Clone, Default)]
struct SourceEncoding {
    bits_per_sample: Option<usize>,
    compression: Option<String>,
}

fn source_encoding(
    source: &RawSource,
    format_hint: FormatHint,
    path: &Path,
) -> Result<SourceEncoding, RawError> {
    if format_hint != FormatHint::ARW {
        return Ok(SourceEncoding::default());
    }

    // `rawler::RawImage::bps` describes its expanded u16 output for Sony ARW,
    // not the 12- or 14-bit precision declared in the RAW IFD. Read these two
    // source-encoding tags through rawler's TIFF parser while keeping every
    // rawler type private to this adapter.
    let tiff = GenericTiffReader::new_with_buffer(source.buf(), 0, 0, None).map_err(|error| {
        RawError::Corrupt {
            path: path.to_path_buf(),
            reason: format!("could not inspect source TIFF encoding: {error}"),
        }
    })?;
    let raw_ifd = tiff
        .find_ifds_with_tag(TiffCommonTag::StripOffsets)
        .into_iter()
        .next();
    let Some(raw_ifd) = raw_ifd else {
        return Ok(SourceEncoding::default());
    };

    let bits_per_sample = raw_ifd
        .get_entry(TiffCommonTag::BitsPerSample)
        .map(|entry| entry.force_usize(0));
    let compression = raw_ifd
        .get_entry(TiffCommonTag::Compression)
        .map(|entry| describe_compression(entry.force_u32(0)));

    Ok(SourceEncoding {
        bits_per_sample,
        compression,
    })
}

fn describe_compression(value: u32) -> String {
    match value {
        1 => "uncompressed (1)".to_owned(),
        7 => "lossless JPEG (7)".to_owned(),
        32_767 => "Sony compressed ARW (32767)".to_owned(),
        other => format!("unknown ({other})"),
    }
}

fn map_file_info(
    image: &RawImage,
    metadata: &RawMetadata,
    source_identity: SourceIdentity,
    embedded_preview: Option<EmbeddedPreviewInfo>,
    encoding: SourceEncoding,
    format_hint: FormatHint,
) -> RawFileInfo {
    let mut color_matrices = image
        .color_matrix
        .iter()
        .map(|(illuminant, values)| CameraColorMatrix {
            illuminant: format!("{illuminant:?}"),
            values: values.clone(),
        })
        .collect::<Vec<_>>();
    color_matrices.sort_by(|left, right| left.illuminant.cmp(&right.illuminant));

    RawFileInfo {
        format: format!("{format_hint:?}"),
        make: image.make.clone(),
        model: image.model.clone(),
        clean_make: image.clean_make.clone(),
        clean_model: image.clean_model.clone(),
        source_size_bytes: source_identity.size_bytes,
        source_identity: Some(source_identity),
        width: image.width,
        height: image.height,
        components_per_pixel: image.cpp,
        source_bits_per_sample: encoding.bits_per_sample,
        decoded_bits_per_sample: image.bps,
        compression: encoding.compression,
        active_area: image.active_area.map(map_rect),
        crop_area: image.crop_area.map(map_rect),
        photometric_interpretation: map_photometric(&image.photometric),
        black_levels: LevelPattern {
            values: image.blacklevel.as_vec(),
            repeat_width: image.blacklevel.width,
            repeat_height: image.blacklevel.height,
            components_per_pixel: image.blacklevel.cpp,
        },
        white_levels: image.whitelevel.as_vec(),
        as_shot_white_balance: image.wb_coeffs.map(finite_value),
        xyz_to_camera: image.xyz_to_cam,
        color_matrices,
        orientation: metadata
            .exif
            .orientation
            .map(Orientation::from_u16)
            .map_or_else(|| map_orientation(image.orientation), map_orientation),
        capture: map_capture_metadata(metadata),
        embedded_preview,
    }
}

fn map_rect(rect: Rect) -> ImageRect {
    ImageRect {
        x: rect.p.x,
        y: rect.p.y,
        width: rect.d.w,
        height: rect.d.h,
    }
}

fn map_photometric(value: &RawPhotometricInterpretation) -> PhotometricInterpretation {
    match value {
        RawPhotometricInterpretation::Cfa(config) => PhotometricInterpretation::Cfa {
            pattern: CfaPattern {
                name: config.cfa.name.clone(),
                width: config.cfa.width,
                height: config.cfa.height,
            },
        },
        RawPhotometricInterpretation::LinearRaw => PhotometricInterpretation::LinearRaw,
        RawPhotometricInterpretation::BlackIsZero => PhotometricInterpretation::BlackIsZero,
    }
}

fn map_orientation(value: Orientation) -> RawOrientation {
    match value {
        Orientation::Normal => RawOrientation::Normal,
        Orientation::HorizontalFlip => RawOrientation::HorizontalFlip,
        Orientation::Rotate180 => RawOrientation::Rotate180,
        Orientation::VerticalFlip => RawOrientation::VerticalFlip,
        Orientation::Transpose => RawOrientation::Transpose,
        Orientation::Rotate90 => RawOrientation::Rotate90,
        Orientation::Transverse => RawOrientation::Transverse,
        Orientation::Rotate270 => RawOrientation::Rotate270,
        Orientation::Unknown => RawOrientation::Unknown,
    }
}

fn map_capture_metadata(metadata: &RawMetadata) -> CaptureMetadata {
    let exif = &metadata.exif;
    CaptureMetadata {
        iso: exif
            .iso_speed
            .or(exif.recommended_exposure_index)
            .or(exif.iso_speed_ratings.map(u32::from)),
        exposure_time: exif.exposure_time.map(map_rational),
        aperture: exif.fnumber.map(map_rational),
        focal_length: exif.focal_length.map(map_rational),
        captured_at: exif
            .date_time_original
            .clone()
            .or_else(|| exif.create_date.clone()),
        lens_make: exif.lens_make.clone(),
        lens_model: exif.lens_model.clone(),
    }
}

fn map_rational(value: rawler::formats::tiff::Rational) -> RationalValue {
    RationalValue {
        numerator: value.n,
        denominator: value.d,
    }
}

fn finite_value(value: f32) -> Option<f32> {
    value.is_finite().then_some(value)
}

fn catch_decoder_panic<T>(
    path: &Path,
    operation: &'static str,
    action: impl FnOnce() -> Result<T, RawError>,
) -> Result<T, RawError> {
    match catch_unwind(AssertUnwindSafe(action)) {
        Ok(result) => result,
        Err(payload) => Err(RawError::DecoderPanic {
            path: path.to_path_buf(),
            operation,
            reason: panic_message(payload.as_ref()),
        }),
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || "<unknown>".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}
