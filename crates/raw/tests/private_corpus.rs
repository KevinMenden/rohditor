use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use rohditor_raw::{PhotometricInterpretation, RawDecoder, RawOrientation, RawlerDecoder};

#[test]
#[ignore = "requires the ignored Sony ARW corpus in testdata/private"]
fn sony_a6400_corpus_passes_initial_decoder_checks() -> Result<(), Box<dyn Error>> {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/private");
    let mut files = raw_files(&corpus)?;
    files.sort();
    assert!(
        files.len() >= 6,
        "expected at least six private ARW samples"
    );
    let preview_file = files
        .first()
        .cloned()
        .ok_or("private RAW corpus unexpectedly empty")?;

    let decoder = RawlerDecoder::default();
    let mut bit_depths = Vec::new();
    let mut has_rotated_sample = false;

    for file in files {
        let frame = decoder.decode(&file)?;
        let info = &frame.info;
        let expected_samples = info
            .width
            .checked_mul(info.height)
            .and_then(|pixels| pixels.checked_mul(info.components_per_pixel))
            .ok_or("sample-count overflow in test")?;

        assert_eq!(info.format, "ARW");
        assert_eq!(info.model, "ILCE-6400");
        assert_eq!((info.width, info.height), (6048, 4024));
        assert_eq!(frame.mosaic.len(), expected_samples);
        assert_eq!(frame.row_stride, info.width * info.components_per_pixel);
        assert!(info.active_area.is_some());
        assert!(info.crop_area.is_some());
        assert!(!info.black_levels.values.is_empty());
        assert!(info.white_levels.iter().all(|level| *level > 0.0));
        assert!(info.as_shot_white_balance[..3].iter().all(Option::is_some));
        assert!(info.embedded_preview.is_some());
        assert!(matches!(
            &info.photometric_interpretation,
            PhotometricInterpretation::Cfa { pattern } if pattern.name == "RGGB"
        ));

        if let Some(bit_depth) = info.source_bits_per_sample {
            bit_depths.push(bit_depth);
        }
        has_rotated_sample |= info.orientation != RawOrientation::Normal;

        let name = file
            .file_name()
            .map_or_else(|| "<unknown>".into(), |value| value.to_string_lossy());
        println!(
            "{name}: {}x{}, {}-bit, {}, {} samples",
            info.width,
            info.height,
            info.source_bits_per_sample
                .map_or_else(|| "unknown".to_owned(), |bits| bits.to_string()),
            info.orientation,
            frame.mosaic.len()
        );
    }

    assert!(bit_depths.contains(&12), "corpus has no 12-bit sample");
    assert!(bit_depths.contains(&14), "corpus has no 14-bit sample");
    assert!(has_rotated_sample, "corpus has no rotated sample");

    let preview = decoder
        .embedded_preview(&preview_file)?
        .ok_or("first private sample has no embedded preview")?;
    let expected_preview_bytes = usize::try_from(preview.width)?
        .checked_mul(usize::try_from(preview.height)?)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or("preview sample-count overflow in test")?;
    assert_eq!(preview.rgb8.len(), expected_preview_bytes);
    Ok(())
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
