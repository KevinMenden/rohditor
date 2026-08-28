use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;

use rawler::decoders::{Decoder, FormatHint, RawDecodeParams, RawMetadata};
use rawler::formats::tiff::reader::{GenericTiffReader, TiffReader};
use rawler::imgop::Rect;
use rawler::rawimage::{RawImage, RawImageData, RawPhotometricInterpretation};
use rawler::rawsource::RawSource;
use rawler::tags::TiffCommonTag;
use rawler::{Orientation, RawlerError};
use tracing::info_span;

use crate::{
    CameraColorMatrix, CaptureMetadata, CfaPattern, DecoderLimits, EmbeddedPreviewInfo, ImageRect,
    LevelPattern, PhotometricInterpretation, PreviewImage, RationalValue, RawDecoder, RawError,
    RawFileInfo, RawFrame, RawOrientation,
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

    fn probe_impl(&self, path: &Path) -> Result<RawFileInfo, RawError> {
        let source = open_source(path)?;
        let decoder =
            rawler::get_decoder(&source).map_err(|error| map_rawler_error(path, error))?;
        let params = RawDecodeParams::default();
        let raw_image = decoder
            .raw_image(&source, &params, true)
            .map_err(|error| map_rawler_error(path, error))?;
        self.validate_dimensions(path, &raw_image)?;
        let metadata = decoder
            .raw_metadata(&source, &params)
            .map_err(|error| map_rawler_error(path, error))?;
        let format_hint = decoder.format_hint();
        let encoding = source_encoding(&source, format_hint, path)?;
        let preview = preview_info(decoder.as_ref(), &source, &params, format_hint, path)?;
        let source_size_bytes = source_metadata(path)?.len();

        Ok(map_file_info(
            &raw_image,
            &metadata,
            source_size_bytes,
            preview,
            encoding,
            format_hint,
        ))
    }

    fn decode_impl(&self, path: &Path) -> Result<RawFrame, RawError> {
        let source = open_source(path)?;
        let decoder =
            rawler::get_decoder(&source).map_err(|error| map_rawler_error(path, error))?;
        let params = RawDecodeParams::default();

        // Ask for a dummy image first so dimensions can be rejected before the
        // decoder allocates the full sensor buffer.
        let header = decoder
            .raw_image(&source, &params, true)
            .map_err(|error| map_rawler_error(path, error))?;
        self.validate_dimensions(path, &header)?;

        let metadata = decoder
            .raw_metadata(&source, &params)
            .map_err(|error| map_rawler_error(path, error))?;
        let format_hint = decoder.format_hint();
        let encoding = source_encoding(&source, format_hint, path)?;
        let preview = preview_info(decoder.as_ref(), &source, &params, format_hint, path)?;
        let raw_image = decoder
            .raw_image(&source, &params, false)
            .map_err(|error| map_rawler_error(path, error))?;
        let expected_samples = self.validate_dimensions(path, &raw_image)?;
        let source_size_bytes = source_metadata(path)?.len();
        let info = map_file_info(
            &raw_image,
            &metadata,
            source_size_bytes,
            preview,
            encoding,
            format_hint,
        );
        let row_stride = raw_image
            .width
            .checked_mul(raw_image.cpp)
            .ok_or_else(|| invalid_dimensions(path, &raw_image, self.limits))?;

        let mosaic = match raw_image.data {
            RawImageData::Integer(samples) => {
                if samples.len() != expected_samples {
                    return Err(RawError::InvalidSampleCount {
                        path: path.to_path_buf(),
                        expected: expected_samples,
                        actual: samples.len(),
                    });
                }
                Arc::from(samples)
            }
            RawImageData::Float(_) => {
                return Err(RawError::UnsupportedPixelData {
                    path: path.to_path_buf(),
                });
            }
        };

        Ok(RawFrame {
            info,
            row_stride,
            mosaic,
        })
    }

    fn embedded_preview_impl(&self, path: &Path) -> Result<Option<PreviewImage>, RawError> {
        let source = open_source(path)?;
        let decoder =
            rawler::get_decoder(&source).map_err(|error| map_rawler_error(path, error))?;
        let params = RawDecodeParams::default();
        let Some(preview) = decoded_preview(
            decoder.as_ref(),
            &source,
            &params,
            decoder.format_hint(),
            path,
        )?
        else {
            return Ok(None);
        };
        let width = preview.width();
        let height = preview.height();
        let rgb8 = Arc::from(preview.into_rgb8().into_raw());

        Ok(Some(PreviewImage {
            width,
            height,
            rgb8,
        }))
    }

    fn validate_dimensions(&self, path: &Path, image: &RawImage) -> Result<usize, RawError> {
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
            _ => Err(invalid_dimensions(path, image, self.limits)),
        }
    }
}

impl RawDecoder for RawlerDecoder {
    fn probe(&self, path: &Path) -> Result<RawFileInfo, RawError> {
        let span = info_span!("raw.probe", file = %file_name(path));
        let _guard = span.enter();
        catch_decoder_panic(path, "probing", || self.probe_impl(path))
    }

    fn decode(&self, path: &Path) -> Result<RawFrame, RawError> {
        let span = info_span!("raw.decode", file = %file_name(path));
        let _guard = span.enter();
        catch_decoder_panic(path, "decoding", || self.decode_impl(path))
    }

    fn embedded_preview(&self, path: &Path) -> Result<Option<PreviewImage>, RawError> {
        let span = info_span!("raw.embedded_preview", file = %file_name(path));
        let _guard = span.enter();
        catch_decoder_panic(path, "extracting the preview from", || {
            self.embedded_preview_impl(path)
        })
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
        RawlerError::DecoderFailed(reason) => RawError::Decode {
            path: path.to_path_buf(),
            reason,
        },
    }
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

fn preview_info(
    decoder: &dyn Decoder,
    source: &RawSource,
    params: &RawDecodeParams,
    format_hint: FormatHint,
    path: &Path,
) -> Result<Option<EmbeddedPreviewInfo>, RawError> {
    Ok(
        decoded_preview(decoder, source, params, format_hint, path)?.map(|preview| {
            EmbeddedPreviewInfo {
                width: preview.width(),
                height: preview.height(),
                color_type: format!("{:?}", preview.color()),
            }
        }),
    )
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
        RawError::Decode {
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
    source_size_bytes: u64,
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
        source_size_bytes,
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
