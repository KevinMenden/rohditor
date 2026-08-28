use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn unsupported_and_truncated_inputs_exit_with_errors() -> Result<(), Box<dyn Error>> {
    let unsupported =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/synthetic/README.md");
    assert_cli_rejects(&unsupported, "unsupported RAW file")?;

    let truncated = TempInput::new(&[0x49, 0x49, 0x2a, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00])?;
    assert_cli_rejects(truncated.path(), "RAW file")?;

    let wrong_extension = Command::new(env!("CARGO_BIN_EXE_rohditor-cli"))
        .arg("develop")
        .arg(&unsupported)
        .arg(truncated.path().with_extension("jpg"))
        .output()?;
    assert_error_contains(&wrong_extension, ".png extension");

    let invalid_recipe = Command::new(env!("CARGO_BIN_EXE_rohditor-cli"))
        .arg("develop")
        .arg("--exposure")
        .arg("9")
        .arg(&unsupported)
        .arg(truncated.path().with_extension("png"))
        .output()?;
    assert_error_contains(&invalid_recipe, "outside the inclusive range");
    Ok(())
}

fn assert_cli_rejects(path: &Path, expected_message: &str) -> Result<(), std::io::Error> {
    let output = Command::new(env!("CARGO_BIN_EXE_rohditor-cli"))
        .arg("inspect")
        .arg(path)
        .output()?;
    assert!(
        !output.status.success(),
        "CLI unexpectedly accepted {path:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected_message),
        "CLI error did not contain {expected_message:?}: {stderr}"
    );
    Ok(())
}

fn assert_error_contains(output: &std::process::Output, expected_message: &str) {
    assert!(!output.status.success(), "CLI unexpectedly succeeded");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected_message),
        "CLI error did not contain {expected_message:?}: {stderr}"
    );
}

#[derive(Debug)]
struct TempInput {
    path: PathBuf,
}

impl TempInput {
    fn new(contents: &[u8]) -> Result<Self, std::io::Error> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "rohditor-cli-test-{}-{unique}.arw",
            std::process::id()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(contents)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempInput {
    fn drop(&mut self) {
        let _remove_result = fs::remove_file(&self.path);
    }
}
