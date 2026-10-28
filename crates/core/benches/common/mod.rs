//! Fixtures shared by the core benchmarks.
#![allow(dead_code)]

use std::fmt::Display;
use std::sync::Arc;

use rohditor_image::Orientation;
use rohditor_raw::{
    CameraColorMatrix, CameraMatrixOrigin, CaptureMetadata, CfaPattern, LevelPattern,
    PhotometricInterpretation, RawFileInfo, RawFrame,
};

/// Synthetic RGGB sensor frame of the given size.
pub fn raw_frame(width: usize, height: usize) -> RawFrame {
    let samples = width * height;
    let mosaic = (0..samples)
        .map(|index| 512_u16.saturating_add((index % 15_000) as u16))
        .collect::<Vec<_>>();
    RawFrame {
        info: raw_info(width, height),
        row_stride: width,
        mosaic: Arc::from(mosaic),
    }
}

pub fn raw_info(width: usize, height: usize) -> RawFileInfo {
    RawFileInfo {
        format: "synthetic benchmark".to_owned(),
        make: "Rohditor".to_owned(),
        model: "Representative sensor".to_owned(),
        clean_make: "Rohditor".to_owned(),
        clean_model: "Representative sensor".to_owned(),
        source_size_bytes: 0,
        source_identity: None,
        width,
        height,
        components_per_pixel: 1,
        source_bits_per_sample: Some(14),
        decoded_bits_per_sample: 16,
        compression: None,
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
        // Identity D65 matrix: the colour pipeline needs one to run, and the
        // values do not change its cost.
        color_matrices: vec![CameraColorMatrix {
            illuminant: "D65".to_owned(),
            values: vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            origin: CameraMatrixOrigin::DecoderDatabase,
        }],
        orientation: Orientation::Normal,
        capture: CaptureMetadata::default(),
        embedded_preview: None,
    }
}

pub fn must<T, E: Display>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("benchmark fixture failed: {error}"),
    }
}
