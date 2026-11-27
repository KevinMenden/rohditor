//! The `extract-preview` subcommand: write a RAW file's embedded preview.

use std::path::Path;

use anyhow::{Context, Result, bail};
use rohditor_core::{paths_refer_to_same_file, write_output_bytes};
use rohditor_raw::{EncodedPreviewFormat, RawDecoder, RawlerDecoder};

use super::output::write_stdout;

pub(super) fn extract_preview(file: &Path, output: &Path, force: bool) -> Result<()> {
    if paths_refer_to_same_file(file, output).with_context(|| {
        format!(
            "could not compare preview source {} with output {}",
            file.display(),
            output.display()
        )
    })? {
        bail!(
            "refusing to replace source RAW file {} with its embedded preview",
            file.display()
        );
    }

    let decoder = RawlerDecoder::default();
    let preview = decoder
        .embedded_preview(file)
        .with_context(|| format!("could not extract a preview from {}", file.display()))?;
    let Some(preview) = preview else {
        bail!("{} does not contain an embedded preview", file.display());
    };
    validate_preview_extension(output, preview.format)?;

    write_output_bytes(output, &preview.bytes, force)
        .with_context(|| format!("could not commit preview output {}", output.display()))?;

    let representation = if preview.is_original_encoding {
        "original embedded"
    } else {
        "normalized"
    };
    write_stdout(&format!(
        "Extracted {representation} {} preview ({}x{}, {} bytes) to {}",
        preview.format,
        preview.width,
        preview.height,
        preview.bytes.len(),
        output.display()
    ))
}

fn validate_preview_extension(output: &Path, format: EncodedPreviewFormat) -> Result<()> {
    let extension = output.extension().and_then(|value| value.to_str());
    if extension.is_some_and(|extension| format.accepts_extension(extension)) {
        return Ok(());
    }

    bail!(
        "preview output {} must use a .{} extension for {} data",
        output.display(),
        format.extension(),
        format.media_type()
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn preview_extension_must_match_its_encoding() {
        assert!(
            validate_preview_extension(Path::new("preview.JPEG"), EncodedPreviewFormat::Jpeg)
                .is_ok()
        );
        assert!(
            validate_preview_extension(Path::new("preview.png"), EncodedPreviewFormat::Jpeg)
                .is_err()
        );
    }

    #[test]
    fn preview_extraction_rejects_a_hard_link_to_its_source_before_decoding() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "preview-alias-test-{}-{unique}",
                std::process::id()
            ));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("source.ARW");
        let destination = directory.join("source-alias.jpg");
        fs::write(&source, b"sentinel RAW bytes").expect("write source sentinel");
        fs::hard_link(&source, &destination).expect("create hard link");

        let error = extract_preview(&source, &destination, true)
            .expect_err("source alias must be rejected before RAW decoding");
        assert!(error.to_string().contains("refusing to replace source RAW"));
        assert_eq!(
            fs::read(&source).expect("read preserved source"),
            b"sentinel RAW bytes"
        );

        fs::remove_dir_all(directory).expect("remove test directory");
    }
}
