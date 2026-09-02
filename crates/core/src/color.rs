use rayon::prelude::*;
use rohditor_raw::{CameraColorMatrix, RawFileInfo};

use crate::{DisplayRgbImage, DisplayTransfer, LinearRgbImage, LinearRgbSpace, PipelineError};
use rohditor_image::allocate_zeroed_f32;

const D65_WHITE: [f32; 3] = [0.950_455_9, 1.0, 1.089_057_8];
const D50_WHITE: [f32; 3] = [0.964_22, 1.0, 0.825_21];
const A_WHITE: [f32; 3] = [1.098_5, 1.0, 0.355_85];

/// A row-major 3x3 matrix used by the explicit color pipeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix3 {
    values: [[f32; 3]; 3],
}

impl Matrix3 {
    #[must_use]
    pub const fn new(values: [[f32; 3]; 3]) -> Self {
        Self { values }
    }

    #[must_use]
    pub const fn identity() -> Self {
        Self::new([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]])
    }

    #[must_use]
    pub const fn values(self) -> [[f32; 3]; 3] {
        self.values
    }

    #[must_use]
    pub fn transform(self, vector: [f32; 3]) -> [f32; 3] {
        self.values.map(|row| dot(row, vector))
    }

    #[must_use]
    pub fn then(self, next: Self) -> Self {
        let mut result = [[0.0; 3]; 3];
        for (row_index, row) in result.iter_mut().enumerate() {
            for (column_index, value) in row.iter_mut().enumerate() {
                *value = (0..3)
                    .map(|inner| next.values[row_index][inner] * self.values[inner][column_index])
                    .sum();
            }
        }
        Self::new(result)
    }

    pub fn inverse(self) -> Result<Self, PipelineError> {
        let m = self.values;
        let determinant = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        if !determinant.is_finite() || determinant.abs() < 1.0e-8 {
            return Err(PipelineError::InvalidMetadata {
                field: "color_matrices",
                reason: "selected XYZ-to-camera matrix is singular".to_owned(),
            });
        }
        let reciprocal = determinant.recip();
        let inverse = [
            [
                (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * reciprocal,
                (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * reciprocal,
                (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * reciprocal,
            ],
            [
                (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * reciprocal,
                (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * reciprocal,
                (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * reciprocal,
            ],
            [
                (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * reciprocal,
                (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * reciprocal,
                (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * reciprocal,
            ],
        ];
        Ok(Self::new(inverse))
    }
}

/// D65 XYZ to linear Rec.2020.
pub const XYZ_D65_TO_LINEAR_REC2020: Matrix3 = Matrix3::new([
    [1.716_651_2, -0.355_670_78, -0.253_366_3],
    [-0.666_684_3, 1.616_481_2, 0.015_768_546],
    [0.017_639_857, -0.042_770_613, 0.942_103_15],
]);

/// Linear Rec.2020 to D65 XYZ.
pub const LINEAR_REC2020_TO_XYZ_D65: Matrix3 = Matrix3::new([
    [0.636_958_06, 0.144_616_9, 0.168_880_97],
    [0.262_700_2, 0.677_998_07, 0.059_301_715],
    [0.0, 0.028_072_694, 1.060_985_1],
]);

/// D65 XYZ to linear sRGB.
pub const XYZ_D65_TO_LINEAR_SRGB: Matrix3 = Matrix3::new([
    [3.240_97, -1.537_383_2, -0.498_610_76],
    [-0.969_243_65, 1.875_967_5, 0.041_555_06],
    [0.055_630_08, -0.203_976_96, 1.056_971_5],
]);

/// Validated transform selected from the decoder's camera calibration metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct CameraColorTransform {
    pub source_illuminant: String,
    pub camera_to_xyz_d65: Matrix3,
    pub camera_to_linear_rec2020: Matrix3,
}

/// Parse the camera calibration matrices and construct a D65 working transform.
///
/// A D65 matrix is preferred for the Phase 2 baseline. D50 and Standard Light A
/// matrices are supported with Bradford adaptation when no D65 matrix exists.
pub fn camera_color_transform(info: &RawFileInfo) -> Result<CameraColorTransform, PipelineError> {
    let parsed = info
        .color_matrices
        .iter()
        .map(parse_camera_matrix)
        .collect::<Result<Vec<_>, _>>()?;
    let selected = parsed
        .iter()
        .filter_map(|(matrix, name)| illuminant(name).map(|value| (matrix, name, value)))
        .min_by_key(|(_, _, (_, priority))| *priority);

    let (xyz_to_camera, source_name, (source_white, _)) = if let Some(selected) = selected {
        (*selected.0, selected.1.clone(), selected.2)
    } else if let Some(matrix) = fallback_xyz_to_camera(info)? {
        (matrix, "D65 fallback".to_owned(), (D65_WHITE, 0))
    } else {
        return Err(PipelineError::InvalidMetadata {
            field: "color_matrices",
            reason: "no supported D65, D50, or Standard Light A matrix is available".to_owned(),
        });
    };

    // The as-shot gains make a neutral sensor value [1, 1, 1]. Normalize each
    // calibration row so that this neutral maps back to the matrix illuminant's
    // reference white before inversion.
    let camera_white = xyz_to_camera.transform(source_white);
    let mut normalized = xyz_to_camera.values();
    for row in 0..3 {
        if !camera_white[row].is_finite() || camera_white[row].abs() < 1.0e-8 {
            return Err(PipelineError::InvalidMetadata {
                field: "color_matrices",
                reason: format!("matrix row {row} does not describe a usable reference white"),
            });
        }
        for value in &mut normalized[row] {
            *value /= camera_white[row];
        }
    }
    let camera_to_source_xyz = Matrix3::new(normalized).inverse()?;
    let adaptation = chromatic_adaptation_to_d65(source_white)?;
    let camera_to_xyz_d65 = camera_to_source_xyz.then(adaptation);
    let camera_to_linear_rec2020 = camera_to_xyz_d65.then(XYZ_D65_TO_LINEAR_REC2020);

    Ok(CameraColorTransform {
        source_illuminant: source_name,
        camera_to_xyz_d65,
        camera_to_linear_rec2020,
    })
}

/// Bradford-adapt one XYZ triplet from `source_white` to D65.
pub fn adapt_xyz_to_d65(xyz: [f32; 3], source_white: [f32; 3]) -> Result<[f32; 3], PipelineError> {
    Ok(chromatic_adaptation_to_d65(source_white)?.transform(xyz))
}

/// Convert a complete linear Rec.2020 image to clipped, transfer-encoded sRGB.
///
/// This is intentionally independent of PNG/JPEG encoding. Scene-linear input
/// is not modified; clipping is confined to this named output transform.
pub fn convert_rec2020_to_display_srgb(
    input: &LinearRgbImage<f32>,
) -> Result<DisplayRgbImage<f32>, PipelineError> {
    if input.space() != LinearRgbSpace::Rec2020D65 {
        return Err(PipelineError::WrongImageState {
            expected: LinearRgbSpace::Rec2020D65.description(),
            actual: input.space().description(),
        });
    }
    let row_stride =
        input
            .width()
            .checked_mul(3)
            .ok_or_else(|| PipelineError::InvalidDimensions {
                width: input.width(),
                height: input.height(),
                row_stride: input.row_stride(),
                reason: "display row-stride calculation overflowed".to_owned(),
            })?;
    let elements =
        row_stride
            .checked_mul(input.height())
            .ok_or_else(|| PipelineError::InvalidDimensions {
                width: input.width(),
                height: input.height(),
                row_stride,
                reason: "display sample-count calculation overflowed".to_owned(),
            })?;
    let mut output = allocate_zeroed_f32(elements)?;
    let rec2020_to_srgb = LINEAR_REC2020_TO_XYZ_D65.then(XYZ_D65_TO_LINEAR_SRGB);
    output
        .par_chunks_mut(row_stride)
        .enumerate()
        .for_each(|(y, output_row)| {
            let source_start = y * input.row_stride();
            let source_row = &input.data()[source_start..source_start + row_stride];
            for (source, destination) in source_row
                .chunks_exact(3)
                .zip(output_row.chunks_exact_mut(3))
            {
                destination.copy_from_slice(&encode_rec2020_for_srgb_output(
                    rec2020_to_srgb,
                    [source[0], source[1], source[2]],
                ));
            }
        });
    DisplayRgbImage::new(
        input.width(),
        input.height(),
        row_stride,
        DisplayTransfer::Srgb,
        output,
    )
    .map_err(Into::into)
}

/// The shared per-pixel output transform used by float display conversion and
/// integer CPU quantization. Future shaders must implement this same sequence.
pub(crate) fn encode_rec2020_for_srgb_output(
    rec2020_to_srgb: Matrix3,
    source: [f32; 3],
) -> [f32; 3] {
    clip_linear_srgb_for_output(rec2020_to_srgb.transform(source)).map(linear_srgb_to_srgb)
}

/// Phase 2's initial highlight and gamut policy: hard clip linear sRGB to [0, 1].
#[must_use]
pub fn clip_linear_srgb_for_output(rgb: [f32; 3]) -> [f32; 3] {
    rgb.map(|value| value.clamp(0.0, 1.0))
}

/// Apply the IEC sRGB transfer function to one linear-light component.
#[must_use]
pub fn linear_srgb_to_srgb(value: f32) -> f32 {
    if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

/// Decode one sRGB component back to linear light.
#[must_use]
pub fn srgb_to_linear_srgb(value: f32) -> f32 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn parse_camera_matrix(matrix: &CameraColorMatrix) -> Result<(Matrix3, String), PipelineError> {
    if matrix.values.len() != 9 {
        return Err(PipelineError::InvalidMetadata {
            field: "color_matrices",
            reason: format!(
                "{} matrix has {} values; a 3x3 matrix requires 9",
                matrix.illuminant,
                matrix.values.len()
            ),
        });
    }
    if matrix.values.iter().any(|value| !value.is_finite()) {
        return Err(PipelineError::InvalidMetadata {
            field: "color_matrices",
            reason: format!("{} matrix contains a non-finite value", matrix.illuminant),
        });
    }
    Ok((
        Matrix3::new([
            [matrix.values[0], matrix.values[1], matrix.values[2]],
            [matrix.values[3], matrix.values[4], matrix.values[5]],
            [matrix.values[6], matrix.values[7], matrix.values[8]],
        ]),
        matrix.illuminant.clone(),
    ))
}

fn fallback_xyz_to_camera(info: &RawFileInfo) -> Result<Option<Matrix3>, PipelineError> {
    let rows = [
        info.xyz_to_camera[0],
        info.xyz_to_camera[1],
        info.xyz_to_camera[2],
    ];
    if rows.iter().flatten().any(|value| !value.is_finite()) {
        return Err(PipelineError::InvalidMetadata {
            field: "xyz_to_camera",
            reason: "matrix contains a non-finite value".to_owned(),
        });
    }
    Ok(rows
        .iter()
        .flatten()
        .any(|value| value.abs() > f32::EPSILON)
        .then(|| Matrix3::new(rows)))
}

fn illuminant(name: &str) -> Option<([f32; 3], u8)> {
    let normalized = name.to_ascii_uppercase().replace([' ', '_', '-'], "");
    match normalized.as_str() {
        "D65" => Some((D65_WHITE, 0)),
        "D50" => Some((D50_WHITE, 1)),
        "A" | "STANDARDLIGHTA" => Some((A_WHITE, 2)),
        _ => None,
    }
}

fn chromatic_adaptation_to_d65(source_white: [f32; 3]) -> Result<Matrix3, PipelineError> {
    if source_white == D65_WHITE {
        return Ok(Matrix3::identity());
    }
    let bradford = Matrix3::new([
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ]);
    let inverse_bradford = Matrix3::new([
        [0.986_992_9, -0.147_054_3, 0.159_962_7],
        [0.432_305_3, 0.518_360_3, 0.049_291_2],
        [-0.008_528_7, 0.040_042_8, 0.968_486_7],
    ]);
    let source_cone = bradford.transform(source_white);
    let destination_cone = bradford.transform(D65_WHITE);
    if source_cone
        .iter()
        .any(|value| !value.is_finite() || value.abs() < 1.0e-8)
    {
        return Err(PipelineError::InvalidMetadata {
            field: "color_matrices",
            reason: "reference illuminant cannot be chromatically adapted".to_owned(),
        });
    }
    let scale = Matrix3::new([
        [destination_cone[0] / source_cone[0], 0.0, 0.0],
        [0.0, destination_cone[1] / source_cone[1], 0.0],
        [0.0, 0.0, destination_cone[2] / source_cone[2]],
    ]);
    Ok(bradford.then(scale).then(inverse_bradford))
}

fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

#[cfg(test)]
mod tests {
    use super::{
        D50_WHITE, D65_WHITE, Matrix3, XYZ_D65_TO_LINEAR_REC2020, adapt_xyz_to_d65,
        clip_linear_srgb_for_output, convert_rec2020_to_display_srgb, linear_srgb_to_srgb,
        srgb_to_linear_srgb,
    };
    use crate::{DisplayTransfer, LinearRgbImage, LinearRgbSpace};

    fn assert_close(actual: f32, expected: f32, tolerance: f32) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "{actual} differs from {expected} by more than {tolerance}"
        );
    }

    #[test]
    fn matrix_inverse_round_trips_a_vector() {
        let matrix = Matrix3::new([[2.0, 0.1, 0.0], [0.0, 3.0, 0.2], [0.1, 0.0, 4.0]]);
        let value = [0.25, 0.5, 0.75];
        let restored = matrix
            .inverse()
            .expect("test matrix is invertible")
            .transform(matrix.transform(value));
        for (actual, expected) in restored.into_iter().zip(value) {
            assert_close(actual, expected, 1.0e-6);
        }
    }

    #[test]
    fn bradford_adaptation_maps_d50_white_to_d65_white() {
        let adapted = adapt_xyz_to_d65(D50_WHITE, D50_WHITE).expect("D50 is valid");
        for (actual, expected) in adapted.into_iter().zip(D65_WHITE) {
            assert_close(actual, expected, 2.0e-4);
        }
    }

    #[test]
    fn rec2020_matrix_maps_d65_white_to_equal_rgb() {
        let rgb = XYZ_D65_TO_LINEAR_REC2020.transform(D65_WHITE);
        for value in rgb {
            assert_close(value, 1.0, 2.0e-5);
        }
    }

    #[test]
    fn srgb_transfer_round_trips_representative_values() {
        for linear in [0.0, 0.001, 0.18, 0.5, 1.0] {
            let restored = srgb_to_linear_srgb(linear_srgb_to_srgb(linear));
            assert_close(restored, linear, 2.0e-6);
        }
    }

    #[test]
    fn named_output_policy_clips_only_at_the_output_boundary() {
        assert_eq!(
            clip_linear_srgb_for_output([-0.2, 0.5, 1.4]),
            [0.0, 0.5, 1.0]
        );
    }

    #[test]
    fn codec_independent_display_conversion_preserves_typed_srgb_state() {
        let input = LinearRgbImage::new(1, 1, 3, LinearRgbSpace::Rec2020D65, vec![1.0, 1.0, 1.0])
            .expect("valid linear image");
        let output = convert_rec2020_to_display_srgb(&input).expect("valid display conversion");
        assert_eq!(output.transfer(), DisplayTransfer::Srgb);
        for channel in output.data() {
            assert_close(*channel, 1.0, 2.0e-5);
        }

        let wrong_space = LinearRgbImage::new(1, 1, 3, LinearRgbSpace::CameraNative, vec![1.0; 3])
            .expect("valid typed image");
        assert!(convert_rec2020_to_display_srgb(&wrong_space).is_err());
    }
}
