use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use image::{ColorType, ImageDecoder, ImageReader};
use rohditor_core::{
    DitherMode, ExportFormat, ExportImage, ExportMetadataPolicy, ExportSettings, PngBitDepth,
    export_image, paths_refer_to_same_file, write_output_bytes,
};
use rohditor_image::{DisplayRgbImage, DisplayTransfer, Orientation};
use rohditor_raw::{
    CameraColorMatrix, CaptureMetadata, CfaPattern, LevelPattern, PhotometricInterpretation,
    RationalValue, RawFileInfo,
};

#[test]
fn png8_and_jpeg_embed_srgb_and_orientation_safe_exif() -> Result<(), Box<dyn Error>> {
    let outputs = TempDirectory::new("metadata")?;
    let source = source_info();
    let image = ExportImage::Rgb8(rgb8_pattern(96, 64)?);

    for (name, format) in [
        (
            "tagged.png",
            ExportFormat::Png {
                bit_depth: PngBitDepth::Eight,
            },
        ),
        ("tagged.jpg", ExportFormat::Jpeg { quality: 90 }),
    ] {
        let path = outputs.path().join(name);
        let report = export_image(
            &path,
            &image,
            &source,
            ExportSettings {
                format,
                dithering: DitherMode::None,
                metadata: ExportMetadataPolicy::Safe,
                overwrite: false,
            },
        )?;
        assert_eq!((report.width, report.height), (96, 64));
        assert!(report.metadata_embedded);

        let reader = ImageReader::open(&path)?.with_guessed_format()?;
        let mut decoder = reader.into_decoder()?;
        assert_eq!(decoder.dimensions(), (96, 64));
        let profile = decoder
            .icc_profile()?
            .ok_or("missing embedded ICC profile")?;
        assert_valid_srgb_profile(&profile);
        let exif = decoder.exif_metadata()?.ok_or("missing embedded EXIF")?;
        assert_eq!(ifd0_short(&exif, 0x0112), Some(1));
        assert!(contains_bytes(&exif, b"Sony"));
        assert!(contains_bytes(&exif, b"ILCE-6400"));
        assert!(contains_bytes(&exif, b"2026:08:29 12:34:56"));
    }
    Ok(())
}

#[test]
fn png16_contains_native_sixteen_bit_samples() -> Result<(), Box<dyn Error>> {
    let outputs = TempDirectory::new("png16")?;
    let path = outputs.path().join("native-16.png");
    let width = 257;
    let height = 3;
    let samples = (0..width * height * 3)
        .map(|index| ((index * 73 + 19) % 65_536) as u16)
        .collect::<Vec<_>>();
    let image = ExportImage::Rgb16(DisplayRgbImage::new(
        width,
        height,
        width * 3,
        DisplayTransfer::Srgb,
        samples.clone(),
    )?);
    export_image(
        &path,
        &image,
        &source_info(),
        ExportSettings {
            format: ExportFormat::Png {
                bit_depth: PngBitDepth::Sixteen,
            },
            metadata: ExportMetadataPolicy::None,
            ..ExportSettings::default()
        },
    )?;

    let decoded = image::open(&path)?;
    assert_eq!(decoded.color(), ColorType::Rgb16);
    let decoded_samples = decoded.into_rgb16().into_raw();
    assert_eq!(decoded_samples, samples);
    assert!(decoded_samples.iter().any(|value| value % 257 != 0));
    Ok(())
}

#[test]
fn jpeg_quality_changes_size_and_decoded_pixels() -> Result<(), Box<dyn Error>> {
    let outputs = TempDirectory::new("quality")?;
    let source = source_info();
    let image = ExportImage::Rgb8(rgb8_pattern(256, 192)?);
    let low = outputs.path().join("quality-20.jpg");
    let high = outputs.path().join("quality-95.jpg");

    for (path, quality) in [(&low, 20), (&high, 95)] {
        export_image(
            path,
            &image,
            &source,
            ExportSettings {
                format: ExportFormat::Jpeg { quality },
                metadata: ExportMetadataPolicy::None,
                ..ExportSettings::default()
            },
        )?;
    }

    assert!(fs::metadata(&high)?.len() > fs::metadata(&low)?.len());
    assert_ne!(
        image::open(&low)?.into_rgb8(),
        image::open(&high)?.into_rgb8()
    );
    Ok(())
}

#[test]
fn failed_encoding_preserves_destination_and_removes_temporary_file() -> Result<(), Box<dyn Error>>
{
    let outputs = TempDirectory::new("failed")?;
    let destination = outputs.path().join("existing.jpg");
    fs::write(&destination, b"existing complete file")?;
    let width = usize::from(u16::MAX) + 1;
    let image = ExportImage::Rgb8(DisplayRgbImage::new(
        width,
        1,
        width * 3,
        DisplayTransfer::Srgb,
        vec![128; width * 3],
    )?);

    let result = export_image(
        &destination,
        &image,
        &source_info(),
        ExportSettings {
            format: ExportFormat::Jpeg { quality: 90 },
            overwrite: true,
            ..ExportSettings::default()
        },
    );
    assert!(result.is_err());
    assert_eq!(fs::read(&destination)?, b"existing complete file");
    assert_eq!(fs::read_dir(outputs.path())?.count(), 2);
    Ok(())
}

#[test]
fn overwrite_must_be_explicit() -> Result<(), Box<dyn Error>> {
    let outputs = TempDirectory::new("overwrite")?;
    let destination = outputs.path().join("existing.png");
    fs::write(&destination, b"keep me")?;
    let result = export_image(
        &destination,
        &ExportImage::Rgb8(rgb8_pattern(2, 2)?),
        &source_info(),
        ExportSettings::default(),
    );
    assert!(result.is_err());
    assert_eq!(fs::read(destination)?, b"keep me");
    Ok(())
}

#[test]
fn padded_rows_are_rejected_without_creating_a_destination() -> Result<(), Box<dyn Error>> {
    let outputs = TempDirectory::new("padded")?;
    let destination = outputs.path().join("padded.png");
    let padded = ExportImage::Rgb8(DisplayRgbImage::new(
        2,
        1,
        8,
        DisplayTransfer::Srgb,
        vec![0; 8],
    )?);
    let result = export_image(
        &destination,
        &padded,
        &source_info(),
        ExportSettings::default(),
    );
    assert!(result.is_err());
    assert!(!destination.exists());
    Ok(())
}

#[test]
fn encoded_bytes_use_explicit_transactional_replacement() -> Result<(), Box<dyn Error>> {
    let outputs = TempDirectory::new("encoded-bytes")?;
    let destination = outputs.path().join("preview.jpg");
    fs::write(&destination, b"complete old preview")?;

    assert!(write_output_bytes(&destination, b"new preview", false).is_err());
    assert_eq!(fs::read(&destination)?, b"complete old preview");

    let bytes_written = write_output_bytes(&destination, b"new preview", true)?;
    assert_eq!(bytes_written, 11);
    assert_eq!(fs::read(&destination)?, b"new preview");
    assert_eq!(fs::read_dir(outputs.path())?.count(), 2);
    Ok(())
}

#[test]
fn same_file_detection_follows_hard_and_symbolic_links() -> Result<(), Box<dyn Error>> {
    let outputs = TempDirectory::new("same-file")?;
    let source = outputs.path().join("source.raw");
    let hard_link = outputs.path().join("hard-link.jpg");
    let distinct = outputs.path().join("distinct.jpg");
    fs::write(&source, b"RAW")?;
    fs::hard_link(&source, &hard_link)?;
    fs::write(&distinct, b"JPEG")?;

    assert!(paths_refer_to_same_file(&source, &source)?);
    assert!(paths_refer_to_same_file(&source, &hard_link)?);
    assert!(!paths_refer_to_same_file(&source, &distinct)?);
    assert!(!paths_refer_to_same_file(
        &source,
        &outputs.path().join("missing.jpg")
    )?);

    #[cfg(unix)]
    {
        let symbolic_link = outputs.path().join("symbolic-link.jpg");
        std::os::unix::fs::symlink(&source, &symbolic_link)?;
        assert!(paths_refer_to_same_file(&source, &symbolic_link)?);
    }
    Ok(())
}

fn rgb8_pattern(width: usize, height: usize) -> Result<DisplayRgbImage<u8>, Box<dyn Error>> {
    let samples = (0..width * height)
        .flat_map(|index| {
            let x = index % width;
            let y = index / width;
            [
                ((x * 31 + y * 17) & 0xff) as u8,
                ((x * 7 + y * 47) & 0xff) as u8,
                (((x ^ y) * 29) & 0xff) as u8,
            ]
        })
        .collect();
    Ok(DisplayRgbImage::new(
        width,
        height,
        width * 3,
        DisplayTransfer::Srgb,
        samples,
    )?)
}

fn assert_valid_srgb_profile(profile: &[u8]) {
    assert!(profile.len() >= 132);
    assert_eq!(read_be_u32(profile, 0), Some(profile.len() as u32));
    assert_eq!(profile.get(16..20), Some(&b"RGB "[..]));
    assert_eq!(profile.get(20..24), Some(&b"XYZ "[..]));
    assert_eq!(profile.get(36..40), Some(&b"acsp"[..]));
    assert!(contains_bytes(profile, b"Rohditor sRGB"));
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn ifd0_short(exif: &[u8], wanted_tag: u16) -> Option<u16> {
    if exif.get(0..2)? != b"II" || read_le_u16(exif, 2)? != 42 {
        return None;
    }
    let offset = usize::try_from(read_le_u32(exif, 4)?).ok()?;
    let count = usize::from(read_le_u16(exif, offset)?);
    for index in 0..count {
        let entry = offset + 2 + index * 12;
        if read_le_u16(exif, entry)? == wanted_tag && read_le_u16(exif, entry + 2)? == 3 {
            return read_le_u16(exif, entry + 8);
        }
    }
    None
}

fn read_le_u16(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_le_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_be_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn source_info() -> RawFileInfo {
    RawFileInfo {
        format: "ARW".to_owned(),
        make: "Sony".to_owned(),
        model: "ILCE-6400".to_owned(),
        clean_make: "Sony".to_owned(),
        clean_model: "ILCE-6400".to_owned(),
        source_size_bytes: 42,
        source_identity: None,
        width: 2,
        height: 2,
        components_per_pixel: 1,
        source_bits_per_sample: Some(14),
        decoded_bits_per_sample: 16,
        compression: Some("compressed".to_owned()),
        active_area: None,
        crop_area: None,
        photometric_interpretation: PhotometricInterpretation::Cfa {
            pattern: CfaPattern {
                name: "RGGB".to_owned(),
                width: 2,
                height: 2,
            },
        },
        black_levels: LevelPattern {
            values: vec![512.0; 4],
            repeat_width: 2,
            repeat_height: 2,
            components_per_pixel: 1,
        },
        white_levels: vec![16_383.0],
        as_shot_white_balance: [Some(1.0); 4],
        xyz_to_camera: [[0.0; 3]; 4],
        color_matrices: vec![CameraColorMatrix {
            illuminant: "D65".to_owned(),
            values: vec![1.0; 9],
        }],
        orientation: Orientation::Rotate270,
        capture: CaptureMetadata {
            iso: Some(640),
            exposure_time: Some(RationalValue {
                numerator: 1,
                denominator: 125,
            }),
            aperture: Some(RationalValue {
                numerator: 28,
                denominator: 10,
            }),
            focal_length: Some(RationalValue {
                numerator: 35,
                denominator: 1,
            }),
            captured_at: Some("2026-08-29 12:34:56+02:00".to_owned()),
            lens_make: Some("Sony".to_owned()),
            lens_model: Some("E 35mm F2.8".to_owned()),
        },
        embedded_preview: None,
    }
}

#[derive(Debug)]
struct TempDirectory {
    path: PathBuf,
}

impl TempDirectory {
    fn new(label: &str) -> Result<Self, std::io::Error> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "phase3-export-{label}-{}-{unique}",
                std::process::id()
            ));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir(&path)?;
        let mut ownership = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path.join(".rohditor-test-directory"))?;
        ownership.write_all(b"owned by the Phase 3 export test")?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let marker = self.path.join(".rohditor-test-directory");
        if marker.is_file() {
            let _remove_result = fs::remove_dir_all(&self.path);
        }
    }
}
