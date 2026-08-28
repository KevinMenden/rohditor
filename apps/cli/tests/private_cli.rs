use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

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

    Ok(())
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
