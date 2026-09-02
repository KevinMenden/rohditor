use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rohditor_raw::{
    CameraColorMatrix, DecoderLimits, EmbeddedPreviewInfo, EncodedPreviewFormat, ImageRect,
    LevelPattern, PhotometricInterpretation, RationalValue, RawDecoder, RawError, RawlerDecoder,
};
use rohditor_image::Orientation;
use serde::Deserialize;

const EXPECTATIONS_JSON: &str = include_str!("fixtures/sony_a6400_expectations.json");

#[derive(Debug, Deserialize)]
struct CorpusExpectations {
    common: CommonExpectation,
    samples: Vec<SampleExpectation>,
}

#[derive(Debug, Deserialize)]
struct CommonExpectation {
    format: String,
    make: String,
    model: String,
    clean_make: String,
    clean_model: String,
    width: usize,
    height: usize,
    components_per_pixel: usize,
    decoded_bits_per_sample: usize,
    compression: String,
    active_area: ImageRect,
    crop_area: ImageRect,
    cfa_name: String,
    cfa_width: usize,
    cfa_height: usize,
    black_levels: LevelPattern,
    white_levels: Vec<f32>,
    embedded_preview: EmbeddedPreviewInfo,
    color_matrices: Vec<CameraColorMatrix>,
}

#[derive(Debug, Deserialize)]
struct SampleExpectation {
    file_name: String,
    source_bits_per_sample: usize,
    as_shot_white_balance: [Option<f32>; 4],
    orientation: Orientation,
    iso: Option<u32>,
    exposure_time: Option<RationalValue>,
    aperture: Option<RationalValue>,
    focal_length: Option<RationalValue>,
    lens_make: Option<String>,
    lens_model: Option<String>,
}

#[test]
#[ignore = "requires the ignored Sony ARW corpus in testdata/private"]
fn sony_a6400_corpus_matches_scrubbed_expectations() -> Result<(), Box<dyn Error>> {
    let corpus = private_corpus_directory();
    let mut files = raw_files(&corpus)?;
    files.sort();
    let expectations: CorpusExpectations = serde_json::from_str(EXPECTATIONS_JSON)?;
    assert_eq!(
        files.len(),
        expectations.samples.len(),
        "the private corpus changed; review and scrub its expected metadata"
    );

    let decoder = RawlerDecoder::default();
    for file in files {
        let file_name = file
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or("private sample has no UTF-8 file name")?;
        let expected = expectations
            .samples
            .iter()
            .find(|candidate| candidate.file_name == file_name)
            .ok_or("private sample has no scrubbed expectation")?;
        let frame = decoder.decode(&file)?;
        let info = &frame.info;
        let common = &expectations.common;
        let expected_samples = info
            .width
            .checked_mul(info.height)
            .and_then(|pixels| pixels.checked_mul(info.components_per_pixel))
            .ok_or("sample-count overflow in test")?;

        assert_eq!(info.format, common.format, "{file_name}");
        assert_eq!(info.make, common.make, "{file_name}");
        assert_eq!(info.model, common.model, "{file_name}");
        assert_eq!(info.clean_make, common.clean_make, "{file_name}");
        assert_eq!(info.clean_model, common.clean_model, "{file_name}");
        let source_identity = info
            .source_identity
            .as_ref()
            .ok_or("decoded file has no source identity")?;
        assert_eq!(source_identity.size_bytes, fs::metadata(&file)?.len());
        assert!(source_identity.modified_unix_nanos.is_some());
        #[cfg(unix)]
        assert!(
            source_identity.filesystem_device.is_some()
                && source_identity.filesystem_inode.is_some()
        );
        assert_eq!((info.width, info.height), (common.width, common.height));
        assert_eq!(
            info.components_per_pixel, common.components_per_pixel,
            "{file_name}"
        );
        assert_eq!(
            info.source_bits_per_sample,
            Some(expected.source_bits_per_sample),
            "{file_name}"
        );
        assert_eq!(
            info.decoded_bits_per_sample, common.decoded_bits_per_sample,
            "{file_name}"
        );
        assert_eq!(
            info.compression.as_deref(),
            Some(common.compression.as_str()),
            "{file_name}"
        );
        assert_eq!(info.active_area, Some(common.active_area), "{file_name}");
        assert_eq!(info.crop_area, Some(common.crop_area), "{file_name}");
        assert_eq!(info.black_levels, common.black_levels, "{file_name}");
        assert_eq!(info.white_levels, common.white_levels, "{file_name}");
        assert_eq!(
            info.as_shot_white_balance, expected.as_shot_white_balance,
            "{file_name}"
        );
        assert_eq!(info.orientation, expected.orientation, "{file_name}");
        assert_eq!(info.capture.iso, expected.iso, "{file_name}");
        assert_eq!(
            info.capture.exposure_time, expected.exposure_time,
            "{file_name}"
        );
        assert_eq!(info.capture.aperture, expected.aperture, "{file_name}");
        assert_eq!(
            info.capture.focal_length, expected.focal_length,
            "{file_name}"
        );
        assert_eq!(info.capture.lens_make, expected.lens_make, "{file_name}");
        assert_eq!(info.capture.lens_model, expected.lens_model, "{file_name}");
        assert_eq!(
            info.embedded_preview.as_ref(),
            Some(&common.embedded_preview),
            "{file_name}"
        );
        assert_eq!(info.color_matrices, common.color_matrices, "{file_name}");
        assert!(matches!(
            &info.photometric_interpretation,
            PhotometricInterpretation::Cfa { pattern }
                if pattern.name == common.cfa_name
                    && pattern.width == common.cfa_width
                    && pattern.height == common.cfa_height
        ));
        assert_eq!(frame.mosaic.len(), expected_samples, "{file_name}");
        assert_eq!(
            frame.row_stride,
            info.width * info.components_per_pixel,
            "{file_name}"
        );

        let preview = decoder
            .embedded_preview(&file)?
            .ok_or("private sample has no embedded preview")?;
        assert_eq!(preview.width, common.embedded_preview.width, "{file_name}");
        assert_eq!(
            preview.height, common.embedded_preview.height,
            "{file_name}"
        );
        assert_eq!(
            preview.color_type, common.embedded_preview.color_type,
            "{file_name}"
        );
        assert_eq!(preview.format, EncodedPreviewFormat::Jpeg, "{file_name}");
        assert!(preview.is_original_encoding, "{file_name}");
        assert!(
            preview.bytes.starts_with(&[0xff, 0xd8, 0xff]),
            "{file_name}"
        );
        assert!(preview.bytes.ends_with(&[0xff, 0xd9]), "{file_name}");

        println!(
            "{file_name}: {}x{}, {}-bit, {}, {} samples, {}-byte preview",
            info.width,
            info.height,
            expected.source_bits_per_sample,
            info.orientation,
            frame.mosaic.len(),
            preview.bytes.len()
        );
    }

    Ok(())
}

#[test]
#[ignore = "requires the ignored Sony ARW corpus in testdata/private"]
fn configured_limits_reject_the_private_image_before_full_decode() -> Result<(), Box<dyn Error>> {
    let sample = private_corpus_directory().join("DSC00851.ARW");
    let decoder = RawlerDecoder::new(DecoderLimits {
        max_width: 1_000,
        max_height: 1_000,
        max_samples: 1_000_000,
        ..DecoderLimits::default()
    });

    assert!(matches!(
        decoder.probe(&sample),
        Err(RawError::InvalidDimensions { .. })
    ));
    assert!(matches!(
        decoder.decode(&sample),
        Err(RawError::InvalidDimensions { .. })
    ));
    Ok(())
}

#[test]
#[ignore = "derives a temporary damaged-preview input from the ignored private Sony corpus"]
fn damaged_optional_preview_does_not_block_metadata_or_sensor_decode() -> Result<(), Box<dyn Error>>
{
    let source = private_corpus_directory().join("DSC00851.ARW");
    let decoder = RawlerDecoder::default();
    let preview = decoder
        .embedded_preview(&source)?
        .ok_or("private sample has no embedded preview")?;
    let mut damaged = fs::read(&source)?;
    let prefix_length = preview.bytes.len().min(64);
    let prefix = &preview.bytes[..prefix_length];
    let preview_offset = damaged
        .windows(prefix.len())
        .position(|window| window == prefix)
        .ok_or("could not locate embedded preview bytes in private sample")?;
    damaged[preview_offset] = 0;

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!(
            "damaged-preview-{}-{unique}.ARW",
            std::process::id()
        ));
    fs::write(&temporary, damaged)?;

    let result = (|| -> Result<(), Box<dyn Error>> {
        assert!(matches!(
            decoder.embedded_preview(&temporary),
            Err(RawError::Corrupt { .. })
        ));
        let info = decoder.probe(&temporary)?;
        assert_eq!(info.embedded_preview, None);
        let frame = decoder.decode(&temporary)?;
        assert_eq!(frame.info.embedded_preview, None);
        assert_eq!(
            frame.mosaic.len(),
            frame.info.width * frame.info.height * frame.info.components_per_pixel
        );
        Ok(())
    })();
    let cleanup = fs::remove_file(&temporary);
    result?;
    cleanup?;
    Ok(())
}

fn private_corpus_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/private")
}

fn raw_files(directory: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let entries = fs::read_dir(directory)?;
    let files = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("arw"))
        })
        .collect();
    Ok(files)
}
