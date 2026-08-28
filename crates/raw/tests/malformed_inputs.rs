use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rohditor_raw::{RawDecoder, RawError, RawlerDecoder};

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

#[test]
fn unsupported_content_is_rejected_by_every_decoder_operation() -> Result<(), Box<dyn Error>> {
    let input = TempInput::new("unsupported.bin", b"this is not a camera RAW file")?;

    assert_all_operations_fail(input.path());
    assert!(matches!(
        RawlerDecoder::default().probe(input.path()),
        Err(RawError::Unsupported { .. } | RawError::Corrupt { .. })
    ));
    Ok(())
}

#[test]
fn incomplete_tiff_is_rejected_by_every_decoder_operation() -> Result<(), Box<dyn Error>> {
    // Valid little-endian TIFF marker and root-IFD offset, followed by a cut-off IFD.
    let input = TempInput::new(
        "truncated.arw",
        &[0x49, 0x49, 0x2a, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00],
    )?;

    assert_all_operations_fail(input.path());
    Ok(())
}

#[test]
#[ignore = "derives a temporary truncated input from the ignored private Sony corpus"]
fn truncated_private_arw_is_rejected_by_every_decoder_operation() -> Result<(), Box<dyn Error>> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/private/DSC00851.ARW");
    let mut prefix = Vec::new();
    File::open(source)?
        .take(64 * 1024)
        .read_to_end(&mut prefix)?;
    let input = TempInput::new("private-truncated.arw", &prefix)?;

    assert_all_operations_fail(input.path());
    Ok(())
}

fn assert_all_operations_fail(path: &Path) {
    let decoder = RawlerDecoder::default();
    let results = [
        ("probe", decoder.probe(path).map(|_| ())),
        ("decode", decoder.decode(path).map(|_| ())),
        (
            "embedded-preview extraction",
            decoder.embedded_preview(path).map(|_| ()),
        ),
    ];

    for (operation, result) in results {
        assert!(
            matches!(
                &result,
                Err(RawError::Io { .. }
                    | RawError::Unsupported { .. }
                    | RawError::Corrupt { .. }
                    | RawError::Decode { .. })
            ),
            "{operation} did not return a bounded input error for {path:?}: {result:?}"
        );
    }
}

#[derive(Debug)]
struct TempInput {
    path: PathBuf,
}

impl TempInput {
    fn new(name: &str, contents: &[u8]) -> Result<Self, std::io::Error> {
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rohditor-raw-test-{}-{sequence}-{name}",
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
