use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use image::{ColorType, ImageDecoder, ImageReader};
use serde_json::Value;

#[test]
#[ignore = "requires the ignored Sony ARW corpus in testdata/private"]
fn inspect_extract_and_develop_cover_the_private_corpus() -> Result<(), Box<dyn Error>> {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/private");
    let mut samples = raw_files(&corpus)?;
    samples.sort();
    assert_eq!(samples.len(), 6, "review private CLI expectations");
    let outputs = TempDirectory::new()?;

    for sample in &samples {
        let inspection = run_cli(&["inspect", "--json"], &[sample.as_path()])?;
        assert_success(&inspection, "inspect");
        let json: Value = serde_json::from_slice(&inspection.stdout)?;
        assert_eq!(json["decoded"], true);
        assert_eq!(json["metadata"]["format"], "ARW");
        assert_eq!(json["metadata"]["model"], "ILCE-6400");

        let stem = sample
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or("private sample has no UTF-8 stem")?;
        let preview_path = outputs.path().join(format!("{stem}.jpg"));
        let extraction = run_cli(
            &["extract-preview"],
            &[sample.as_path(), preview_path.as_path()],
        )?;
        assert_success(&extraction, "extract-preview");
        let preview = fs::read(&preview_path)?;
        assert!(preview.starts_with(&[0xff, 0xd8, 0xff]));
        assert!(preview.ends_with(&[0xff, 0xd9]));
    }

    let first = samples.first().ok_or("private corpus is empty")?;
    let existing_output = outputs.path().join("DSC00851.jpg");
    let overwrite = run_cli(
        &["extract-preview"],
        &[first.as_path(), existing_output.as_path()],
    )?;
    assert!(
        !overwrite.status.success(),
        "overwrite unexpectedly succeeded"
    );

    let forced = run_cli(
        &["extract-preview", "--force"],
        &[first.as_path(), existing_output.as_path()],
    )?;
    assert_success(&forced, "extract-preview --force");

    let wrong_extension = outputs.path().join("preview.png");
    let wrong_extension_result = run_cli(
        &["extract-preview"],
        &[first.as_path(), wrong_extension.as_path()],
    )?;
    assert!(
        !wrong_extension_result.status.success(),
        "mismatched preview extension unexpectedly succeeded"
    );

    let renamed_input = outputs.path().join("camera-data.bin");
    if fs::hard_link(first, &renamed_input).is_err() {
        fs::copy(first, &renamed_input)?;
    }
    let content_probe = run_cli(
        &["inspect", "--json", "--metadata-only"],
        &[renamed_input.as_path()],
    )?;
    assert_success(&content_probe, "content probe with a non-RAW extension");
    let json: Value = serde_json::from_slice(&content_probe.stdout)?;
    assert_eq!(json["metadata"]["format"], "ARW");

    let source_alias = outputs.path().join("source-alias.png");
    fs::hard_link(first, &source_alias)?;
    let source_overwrite = run_cli(
        &["develop", "--force"],
        &[first.as_path(), source_alias.as_path()],
    )?;
    assert!(
        !source_overwrite.status.success(),
        "develop must never replace a hard-linked source RAW"
    );
    assert!(
        String::from_utf8_lossy(&source_overwrite.stderr)
            .contains("refusing to replace source RAW file")
    );

    for (name, expected_dimensions) in [
        ("DSC00851.ARW", (6_000, 4_000)),
        ("DSC03270.ARW", (4_000, 6_000)),
    ] {
        let sample = corpus.join(name);
        let output_path = outputs.path().join(format!("{name}.png"));
        let development = run_cli(
            &[
                "develop",
                "--exposure",
                "0",
                "--contrast",
                "0",
                "--saturation",
                "1",
                "--white-balance",
                "1,1,1",
                "--crop",
                "recommended",
                "--demosaic",
                "bilinear",
            ],
            &[sample.as_path(), output_path.as_path()],
        )?;
        assert_success(&development, "develop");
        let developed = image::open(&output_path)?;
        assert_eq!(
            (developed.width(), developed.height()),
            expected_dimensions,
            "{name}"
        );
        assert_eq!(developed.color(), image::ColorType::Rgb8, "{name}");

        let overwrite = run_cli(&["develop"], &[sample.as_path(), output_path.as_path()])?;
        assert!(
            !overwrite.status.success(),
            "develop overwrite unexpectedly succeeded for {name}"
        );
    }

    let deterministic_sample = corpus.join("DSC00851.ARW");
    let first_development = outputs.path().join("DSC00851.ARW.png");
    let repeated_development = outputs.path().join("DSC00851-repeat.png");
    let repeated = run_cli(
        &[
            "develop",
            "--exposure",
            "0",
            "--contrast",
            "0",
            "--saturation",
            "1",
            "--white-balance",
            "1,1,1",
            "--crop",
            "recommended",
            "--demosaic",
            "bilinear",
        ],
        &[
            deterministic_sample.as_path(),
            repeated_development.as_path(),
        ],
    )?;
    assert_success(&repeated, "repeated develop");
    assert_eq!(
        fs::read(first_development)?,
        fs::read(repeated_development)?,
        "PNG development must be byte-for-byte deterministic"
    );

    let png16 = outputs.path().join("DSC00851-16.png");
    let png16_export = run_cli(
        &["develop", "--png-bit-depth", "16", "--dither"],
        &[deterministic_sample.as_path(), png16.as_path()],
    )?;
    assert_success(&png16_export, "16-bit PNG develop");
    let png16_image = image::open(&png16)?;
    assert_eq!((png16_image.width(), png16_image.height()), (6_000, 4_000));
    assert_eq!(png16_image.color(), ColorType::Rgb16);
    assert!(
        png16_image
            .into_rgb16()
            .as_raw()
            .iter()
            .any(|sample| sample % 257 != 0),
        "16-bit PNG looks like an up-converted 8-bit buffer"
    );
    assert_tagged_srgb_and_normal_orientation(&png16)?;

    let low_quality = outputs.path().join("DSC00851-q20.jpg");
    let high_quality = outputs.path().join("DSC00851-q95.jpg");
    for (destination, quality) in [(&low_quality, "20"), (&high_quality, "95")] {
        let export = run_cli(
            &["develop", "--jpeg-quality", quality],
            &[deterministic_sample.as_path(), destination.as_path()],
        )?;
        assert_success(&export, "JPEG develop");
        let decoded = image::open(destination)?;
        assert_eq!((decoded.width(), decoded.height()), (6_000, 4_000));
        assert_eq!(decoded.color(), ColorType::Rgb8);
        assert_tagged_srgb_and_normal_orientation(destination)?;
    }
    assert!(fs::metadata(&high_quality)?.len() > fs::metadata(&low_quality)?.len());
    let reference = image::open(outputs.path().join("DSC00851.ARW.png"))?
        .into_rgb8()
        .into_raw();
    let low_error = mean_absolute_error(
        &reference,
        &image::open(&low_quality)?.into_rgb8().into_raw(),
    );
    let high_error = mean_absolute_error(
        &reference,
        &image::open(&high_quality)?.into_rgb8().into_raw(),
    );
    assert!(
        high_error < low_error,
        "quality 95: {high_error}, quality 20: {low_error}"
    );

    let portrait_jpeg = outputs.path().join("DSC03270-oriented.jpg");
    let portrait_export = run_cli(
        &["develop", "--jpeg-quality", "90"],
        &[
            corpus.join("DSC03270.ARW").as_path(),
            portrait_jpeg.as_path(),
        ],
    )?;
    assert_success(&portrait_export, "portrait JPEG develop");
    let portrait = image::open(&portrait_jpeg)?;
    assert_eq!((portrait.width(), portrait.height()), (4_000, 6_000));
    assert_tagged_srgb_and_normal_orientation(&portrait_jpeg)?;

    Ok(())
}

fn assert_tagged_srgb_and_normal_orientation(path: &Path) -> Result<(), Box<dyn Error>> {
    let reader = ImageReader::open(path)?.with_guessed_format()?;
    let mut decoder = reader.into_decoder()?;
    let profile = decoder.icc_profile()?.ok_or("export has no ICC profile")?;
    assert_eq!(profile.get(16..20), Some(&b"RGB "[..]));
    assert_eq!(profile.get(36..40), Some(&b"acsp"[..]));
    assert!(
        profile
            .windows(b"Rohditor sRGB".len())
            .any(|window| window == b"Rohditor sRGB")
    );
    let exif = decoder
        .exif_metadata()?
        .ok_or("export has no EXIF metadata")?;
    assert_eq!(ifd0_short(&exif, 0x0112), Some(1));
    Ok(())
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

fn mean_absolute_error(reference: &[u8], candidate: &[u8]) -> f64 {
    assert_eq!(reference.len(), candidate.len());
    reference
        .iter()
        .zip(candidate)
        .map(|(left, right)| f64::from(left.abs_diff(*right)))
        .sum::<f64>()
        / reference.len() as f64
}

fn run_cli(options: &[&str], paths: &[&Path]) -> Result<Output, std::io::Error> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rohditor-cli"));
    command.args(options);
    command.args(paths);
    command.output()
}

fn assert_success(output: &Output, operation: &str) {
    assert!(
        output.status.success(),
        "{operation} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn raw_files(directory: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let files = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("arw"))
        })
        .collect();
    Ok(files)
}

#[derive(Debug)]
struct TempDirectory {
    path: PathBuf,
}

impl TempDirectory {
    fn new() -> Result<Self, std::io::Error> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "phase1-private-cli-{}-{unique}",
                std::process::id()
            ));
        fs::create_dir(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _remove_result = fs::remove_dir_all(&self.path);
    }
}
