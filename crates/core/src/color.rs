use rayon::prelude::*;
use rohditor_camera_profile::{CAMERA_PROFILE_EVALUATOR_VERSION, CalibrationIlluminant};
use rohditor_demosaic::WhiteBalanceGains;
use rohditor_edit::{CameraProfileSelection, WhiteBalance};
use rohditor_image::{
    DisplayRgbImage, DisplayTransfer, LinearRgbImage, LinearRgbSpace, allocate_zeroed_f32,
};
use rohditor_raw::{CameraColorMatrix, CameraMatrixOrigin, RawFileInfo};

use crate::PipelineError;
use crate::white_balance::{
    WhiteBalanceCoordinates, coordinates_from_camera_gains_relative_to_as_shot,
};

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

/// Compact camera facts retained after RAW metadata probing. It contains no
/// decoded pixels and is sufficient to resolve any supported recipe profile.
#[derive(Debug, Clone, PartialEq)]
pub struct CameraCalibration {
    pub make: String,
    pub model: String,
    pub clean_make: String,
    pub clean_model: String,
    pub as_shot_white_balance: [Option<f32>; 4],
    pub xyz_to_camera: [[f32; 3]; 4],
    pub color_matrices: Vec<CameraColorMatrix>,
}

impl CameraCalibration {
    #[must_use]
    pub fn from_raw_info(info: &RawFileInfo) -> Self {
        Self {
            make: info.make.clone(),
            model: info.model.clone(),
            clean_make: info.clean_make.clone(),
            clean_model: info.clean_model.clone(),
            as_shot_white_balance: info.as_shot_white_balance,
            xyz_to_camera: info.xyz_to_camera,
            color_matrices: info.color_matrices.clone(),
        }
    }
}

/// Provenance recorded alongside the resolved camera transform for diagnostics
/// and for distinguishing user-installed profiles from decoder defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CameraProfileProvenance {
    Automatic {
        origin: CameraMatrixOrigin,
        illuminant: String,
        camera_make: String,
        camera_model: String,
        evaluator_version: u8,
    },
    Matrix {
        name: String,
        source_sha256: String,
        camera_model: String,
        illuminant: CalibrationIlluminant,
        forward_matrix_used: bool,
        evaluator_version: u8,
    },
}

impl std::fmt::Display for CameraProfileProvenance {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Automatic {
                origin,
                illuminant,
                camera_make,
                camera_model,
                evaluator_version,
            } => {
                write!(
                    formatter,
                    "automatic/v{evaluator_version}/{origin:?}/{illuminant}/{camera_make} {camera_model}"
                )
            }
            Self::Matrix {
                name,
                source_sha256,
                camera_model,
                illuminant,
                forward_matrix_used,
                evaluator_version,
            } => write!(
                formatter,
                "matrix/v{evaluator_version}/{name}/{source_sha256}/{illuminant}/model={camera_model}/forward={forward_matrix_used}"
            ),
        }
    }
}

/// The transform and white-balance gains resolved for one recipe.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCameraColour {
    pub source_illuminant: String,
    pub camera_to_xyz_d65: Matrix3,
    pub camera_to_linear_rec2020: Matrix3,
    pub white_balance_gains: WhiteBalanceGains,
    /// Best-effort UI coordinates for the camera's As Shot gains. Temperature
    /// is projected from the calibration and Tint is zero at this anchor. The
    /// source gains remain authoritative when projection is unavailable.
    pub as_shot_coordinates: Option<WhiteBalanceCoordinates>,
    pub provenance: CameraProfileProvenance,
}

impl ResolvedCameraColour {
    #[must_use]
    pub fn camera_color_transform(&self) -> CameraColorTransform {
        CameraColorTransform {
            source_illuminant: self.source_illuminant.clone(),
            camera_to_xyz_d65: self.camera_to_xyz_d65,
            camera_to_linear_rec2020: self.camera_to_linear_rec2020,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CameraProfileKeyMode {
    Automatic,
    Matrix {
        calibrations: Vec<ProfileCalibrationKey>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProfileCalibrationKey {
    illuminant: CalibrationIlluminant,
    xyz_to_camera: [u32; 9],
    forward_camera_to_xyz_d50: Option<[u32; 9]>,
}

/// Exact cache identity for the selected camera-profile evaluator input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraProfileKey {
    evaluator_version: u8,
    mode: CameraProfileKeyMode,
}

/// Build a cache identity from pixel-producing profile payload, excluding
/// labels and paths that do not affect evaluation.
#[must_use]
pub fn camera_profile_key(selection: &CameraProfileSelection) -> CameraProfileKey {
    let mode = match selection {
        CameraProfileSelection::Automatic => CameraProfileKeyMode::Automatic,
        CameraProfileSelection::Matrix(profile) => {
            let mut calibrations = profile
                .calibrations
                .iter()
                .map(|calibration| ProfileCalibrationKey {
                    illuminant: calibration.illuminant,
                    xyz_to_camera: calibration
                        .xyz_to_camera
                        .into_iter()
                        .flatten()
                        .map(f32::to_bits)
                        .collect::<Vec<_>>()
                        .try_into()
                        .expect("a 3x3 matrix always has nine values"),
                    forward_camera_to_xyz_d50: calibration.forward_camera_to_xyz_d50.map(
                        |matrix| {
                            matrix
                                .into_iter()
                                .flatten()
                                .map(f32::to_bits)
                                .collect::<Vec<_>>()
                                .try_into()
                                .expect("a 3x3 matrix always has nine values")
                        },
                    ),
                })
                .collect::<Vec<_>>();
            calibrations.sort_by_key(|calibration| calibration.illuminant.priority());
            CameraProfileKeyMode::Matrix { calibrations }
        }
    };
    CameraProfileKey {
        evaluator_version: CAMERA_PROFILE_EVALUATOR_VERSION,
        mode,
    }
}

/// Parse the camera calibration matrices and construct a D65 working transform.
///
/// A D65 matrix is preferred for the Phase 2 baseline. D50 and Standard Light A
/// matrices are supported with Bradford adaptation when no D65 matrix exists.
pub fn camera_color_transform(info: &RawFileInfo) -> Result<CameraColorTransform, PipelineError> {
    let calibration = CameraCalibration::from_raw_info(info);
    Ok(automatic_camera_color(&calibration)?.0)
}

/// Resolve the selected camera profile and recipe white balance without
/// changing the retained camera-native image.
pub fn resolve_camera_colour(
    calibration: &CameraCalibration,
    selection: &CameraProfileSelection,
    white_balance: WhiteBalance,
) -> Result<ResolvedCameraColour, PipelineError> {
    let (transform, provenance) = match selection {
        CameraProfileSelection::Automatic => {
            let (transform, origin, illuminant) = automatic_camera_color(calibration)?;
            let (camera_make, camera_model) = camera_identity(calibration);
            (
                transform,
                CameraProfileProvenance::Automatic {
                    origin,
                    illuminant,
                    camera_make,
                    camera_model,
                    evaluator_version: CAMERA_PROFILE_EVALUATOR_VERSION,
                },
            )
        }
        CameraProfileSelection::Matrix(profile) => {
            profile
                .validate()
                .map_err(|error| PipelineError::InvalidRecipe {
                    field: "color.camera_profile",
                    reason: error.to_string(),
                })?;
            if !profile.matches_camera(
                &calibration.make,
                &calibration.model,
                &calibration.clean_make,
                &calibration.clean_model,
            ) {
                return Err(PipelineError::InvalidRecipe {
                    field: "color.camera_profile",
                    reason: format!(
                        "profile camera model {:?} does not match {:?}",
                        profile.camera_model,
                        camera_model_description(calibration)
                    ),
                });
            }
            let selected =
                profile
                    .preferred_calibration()
                    .ok_or_else(|| PipelineError::InvalidRecipe {
                        field: "color.camera_profile",
                        reason: "profile has no supported calibration".to_owned(),
                    })?;
            let forward_matrix_used = selected.forward_camera_to_xyz_d50.is_some();
            let transform = profile_camera_color(selected, &profile.name)?;
            (
                transform,
                CameraProfileProvenance::Matrix {
                    name: profile.name.clone(),
                    source_sha256: profile.source_sha256.clone(),
                    camera_model: profile.camera_model.clone(),
                    illuminant: selected.illuminant,
                    forward_matrix_used,
                    evaluator_version: CAMERA_PROFILE_EVALUATOR_VERSION,
                },
            )
        }
    };
    let white_balance_gains = crate::cpu::white_balance_gains_from_calibration(
        calibration.as_shot_white_balance,
        transform.camera_to_xyz_d65,
        white_balance,
    )?;
    let as_shot_gains = crate::cpu::white_balance_gains_from_calibration(
        calibration.as_shot_white_balance,
        transform.camera_to_xyz_d65,
        WhiteBalance::AsShot,
    )
    .ok();
    let as_shot_coordinates = as_shot_gains.and_then(|gains| {
        coordinates_from_camera_gains_relative_to_as_shot(transform.camera_to_xyz_d65, gains, gains)
            .ok()
    });
    let resolved = ResolvedCameraColour {
        source_illuminant: transform.source_illuminant.clone(),
        camera_to_xyz_d65: transform.camera_to_xyz_d65,
        camera_to_linear_rec2020: transform.camera_to_linear_rec2020,
        white_balance_gains,
        as_shot_coordinates,
        provenance,
    };
    tracing::debug!(
        profile = %resolved.provenance,
        white_balance = ?white_balance,
        "resolved camera colour"
    );
    Ok(resolved)
}

fn automatic_camera_color(
    calibration: &CameraCalibration,
) -> Result<(CameraColorTransform, CameraMatrixOrigin, String), PipelineError> {
    let parsed = calibration
        .color_matrices
        .iter()
        .map(parse_camera_matrix)
        .collect::<Result<Vec<_>, _>>()?;
    let selected = parsed
        .iter()
        .filter_map(|(matrix, name, origin)| {
            illuminant(name).map(|value| (matrix, name, origin, value))
        })
        .min_by_key(|(_, _, _, (_, priority))| *priority);

    let (transform, origin, source_name) = if let Some(selected) = selected {
        let transform = matrix_camera_color(*selected.0, selected.1.clone(), selected.3.0)?;
        (transform, *selected.2, selected.1.clone())
    } else if let Some(matrix) = fallback_xyz_to_camera(calibration)? {
        (
            matrix_camera_color(matrix, "D65 fallback".to_owned(), D65_WHITE)?,
            CameraMatrixOrigin::LegacyDecoderFallback,
            "D65 fallback".to_owned(),
        )
    } else {
        return Err(PipelineError::InvalidMetadata {
            field: "color_matrices",
            reason: "no supported D65, D50, or Standard Light A matrix is available".to_owned(),
        });
    };

    Ok((transform, origin, source_name))
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
                .as_chunks::<3>()
                .0
                .iter()
                .zip(output_row.as_chunks_mut::<3>().0.iter_mut())
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

fn parse_camera_matrix(
    matrix: &CameraColorMatrix,
) -> Result<(Matrix3, String, CameraMatrixOrigin), PipelineError> {
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
        matrix.origin,
    ))
}

fn fallback_xyz_to_camera(
    calibration: &CameraCalibration,
) -> Result<Option<Matrix3>, PipelineError> {
    let rows = [
        calibration.xyz_to_camera[0],
        calibration.xyz_to_camera[1],
        calibration.xyz_to_camera[2],
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

fn profile_camera_color(
    calibration: &rohditor_camera_profile::MatrixCalibration,
    source_name: &str,
) -> Result<CameraColorTransform, PipelineError> {
    let camera_to_xyz_d65 = if let Some(forward) = calibration.forward_camera_to_xyz_d50 {
        Matrix3::new(forward).then(chromatic_adaptation_to_d65(D50_WHITE)?)
    } else {
        matrix_camera_to_xyz_d65(
            Matrix3::new(calibration.xyz_to_camera),
            profile_white(calibration.illuminant),
        )?
    };
    Ok(CameraColorTransform {
        source_illuminant: format!("{source_name} ({})", calibration.illuminant),
        camera_to_linear_rec2020: camera_to_xyz_d65.then(XYZ_D65_TO_LINEAR_REC2020),
        camera_to_xyz_d65,
    })
}

fn matrix_camera_color(
    xyz_to_camera: Matrix3,
    source_name: String,
    source_white: [f32; 3],
) -> Result<CameraColorTransform, PipelineError> {
    let camera_to_xyz_d65 = matrix_camera_to_xyz_d65(xyz_to_camera, source_white)?;
    Ok(CameraColorTransform {
        source_illuminant: source_name,
        camera_to_linear_rec2020: camera_to_xyz_d65.then(XYZ_D65_TO_LINEAR_REC2020),
        camera_to_xyz_d65,
    })
}

fn matrix_camera_to_xyz_d65(
    xyz_to_camera: Matrix3,
    source_white: [f32; 3],
) -> Result<Matrix3, PipelineError> {
    // Normalize each calibration row so a neutral camera value maps to the
    // matrix illuminant's reference white before inversion. This is the
    // existing Automatic evaluator and is also the fallback for profiles that
    // do not provide a ForwardMatrix.
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
    Ok(camera_to_source_xyz.then(chromatic_adaptation_to_d65(source_white)?))
}

fn profile_white(illuminant: CalibrationIlluminant) -> [f32; 3] {
    match illuminant {
        CalibrationIlluminant::D65 => D65_WHITE,
        CalibrationIlluminant::D50 => D50_WHITE,
        CalibrationIlluminant::StandardLightA => A_WHITE,
    }
}

fn camera_model_description(calibration: &CameraCalibration) -> String {
    [
        calibration.clean_make.as_str(),
        calibration.clean_model.as_str(),
    ]
    .into_iter()
    .filter(|value| !value.trim().is_empty())
    .collect::<Vec<_>>()
    .join(" ")
}

fn camera_identity(calibration: &CameraCalibration) -> (String, String) {
    (
        if calibration.clean_make.trim().is_empty() {
            calibration.make.clone()
        } else {
            calibration.clean_make.clone()
        },
        if calibration.clean_model.trim().is_empty() {
            calibration.model.clone()
        } else {
            calibration.clean_model.clone()
        },
    )
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
    use rohditor_camera_profile::{CalibrationIlluminant, MatrixCalibration, MatrixCameraProfile};
    use rohditor_edit::{CameraProfileSelection, WhiteBalance};
    use rohditor_raw::{CameraColorMatrix, CameraMatrixOrigin};

    use super::{
        CameraCalibration, D50_WHITE, D65_WHITE, Matrix3, XYZ_D65_TO_LINEAR_REC2020,
        adapt_xyz_to_d65, camera_profile_key, clip_linear_srgb_for_output,
        convert_rec2020_to_display_srgb, linear_srgb_to_srgb, resolve_camera_colour,
        srgb_to_linear_srgb,
    };
    use rohditor_image::{DisplayTransfer, LinearRgbImage, LinearRgbSpace};

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

    #[test]
    fn automatic_resolution_records_decoder_matrix_provenance() {
        let calibration = calibration();
        let resolved = resolve_camera_colour(
            &calibration,
            &CameraProfileSelection::Automatic,
            WhiteBalance::AsShot,
        )
        .expect("automatic matrix should resolve");
        assert_eq!(
            resolved.provenance,
            super::CameraProfileProvenance::Automatic {
                origin: CameraMatrixOrigin::DecoderDatabase,
                illuminant: "D65".to_owned(),
                camera_make: "Sony".to_owned(),
                camera_model: "ILCE-6400".to_owned(),
                evaluator_version: super::CAMERA_PROFILE_EVALUATOR_VERSION,
            }
        );
    }

    #[test]
    fn forward_matrix_profile_uses_d50_to_d65_adaptation() {
        let profile = profile(Some([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]));
        let resolved = resolve_camera_colour(
            &calibration(),
            &CameraProfileSelection::Matrix(profile),
            WhiteBalance::AsShot,
        )
        .expect("matching forward matrix profile should resolve");
        let expected = adapt_xyz_to_d65([1.0, 1.0, 1.0], D50_WHITE).expect("D50 adaptation");
        let actual = resolved.camera_to_xyz_d65.transform([1.0, 1.0, 1.0]);
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert_close(actual, expected, 1.0e-5);
        }
    }

    #[test]
    fn as_shot_and_manual_gains_do_not_depend_on_profile_choice() {
        let first = CameraProfileSelection::Matrix(profile(None));
        let second = CameraProfileSelection::Matrix(profile(Some([
            [1.0, 0.1, 0.0],
            [0.0, 1.0, 0.1],
            [0.1, 0.0, 1.0],
        ])));
        let calibration = calibration();
        let as_shot = resolve_camera_colour(&calibration, &first, WhiteBalance::AsShot)
            .expect("first as-shot profile");
        let as_shot_other = resolve_camera_colour(&calibration, &second, WhiteBalance::AsShot)
            .expect("second as-shot profile");
        assert_eq!(
            as_shot.white_balance_gains,
            as_shot_other.white_balance_gains
        );
        let manual = WhiteBalance::ManualMultipliers {
            red: 1.2,
            green: 0.9,
            blue: 0.8,
        };
        let manual_first =
            resolve_camera_colour(&calibration, &first, manual).expect("first manual profile");
        let manual_second =
            resolve_camera_colour(&calibration, &second, manual).expect("second manual profile");
        assert_eq!(
            manual_first.white_balance_gains,
            manual_second.white_balance_gains
        );
        let temperature_tint = WhiteBalance::TemperatureTint {
            temperature: 4_800.0,
            tint: 0.2,
        };
        let temperature_first = resolve_camera_colour(&calibration, &first, temperature_tint)
            .expect("first TT profile");
        let temperature_second = resolve_camera_colour(&calibration, &second, temperature_tint)
            .expect("second TT profile");
        assert_ne!(
            temperature_first.white_balance_gains,
            temperature_second.white_balance_gains
        );
    }

    #[test]
    fn camera_matrix_projects_a_realistic_as_shot_balance_into_ui_coordinates() {
        let calibration = CameraCalibration {
            make: "SONY".to_owned(),
            model: "ILCE-6400".to_owned(),
            clean_make: "Sony".to_owned(),
            clean_model: "ILCE-6400".to_owned(),
            as_shot_white_balance: [Some(2.511_718_8), Some(1.0), Some(1.851_562_5), None],
            xyz_to_camera: [[0.0; 3]; 4],
            color_matrices: vec![CameraColorMatrix {
                illuminant: "D65".to_owned(),
                values: vec![
                    0.7657, -0.2847, -0.0607, -0.4083, 1.1966, 0.2389, -0.0684, 0.1418, 0.5844,
                ],
                origin: CameraMatrixOrigin::DecoderDatabase,
            }],
        };
        let resolved = resolve_camera_colour(
            &calibration,
            &CameraProfileSelection::Automatic,
            WhiteBalance::AsShot,
        )
        .expect("Sony-like calibration should resolve");
        let coordinates = resolved
            .as_shot_coordinates
            .expect("As Shot should be visible in Temperature/Tint controls");
        assert!((2_000.0..=25_000.0).contains(&coordinates.temperature));
        assert!((-1.0..=1.0).contains(&coordinates.tint));
        assert!((coordinates.temperature - 5_200.0).abs() < 500.0);
        assert!(coordinates.tint.abs() < 1.0e-6);
        let reconstructed = crate::camera_gains_from_as_shot_coordinates(
            resolved.camera_to_xyz_d65,
            resolved.white_balance_gains,
            coordinates,
        )
        .expect("the displayed As Shot coordinates should preserve the source balance");
        assert!((reconstructed.red - resolved.white_balance_gains.red).abs() < 2.0e-3);
        assert!((reconstructed.blue - resolved.white_balance_gains.blue).abs() < 2.0e-3);
    }

    #[test]
    fn profile_key_includes_matrix_bits_and_forward_presence() {
        let first = CameraProfileSelection::Matrix(profile(None));
        let second = CameraProfileSelection::Matrix(profile(Some([
            [1.0, 0.1, 0.0],
            [0.0, 1.0, 0.1],
            [0.1, 0.0, 1.0],
        ])));
        assert_ne!(camera_profile_key(&first), camera_profile_key(&second));
    }

    fn calibration() -> CameraCalibration {
        CameraCalibration {
            make: "Sony".to_owned(),
            model: "ILCE-6400".to_owned(),
            clean_make: "Sony".to_owned(),
            clean_model: "ILCE-6400".to_owned(),
            as_shot_white_balance: [Some(2.0), Some(1.0), Some(1.5), None],
            xyz_to_camera: [[0.0; 3]; 4],
            color_matrices: vec![CameraColorMatrix {
                illuminant: "D65".to_owned(),
                values: vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
                origin: CameraMatrixOrigin::DecoderDatabase,
            }],
        }
    }

    fn profile(forward: Option<[[f32; 3]; 3]>) -> MatrixCameraProfile {
        let profile = MatrixCameraProfile {
            format_version: 1,
            source_sha256: "a".repeat(64),
            name: "Test profile".to_owned(),
            camera_model: "Sony ILCE-6400".to_owned(),
            copyright: None,
            calibrations: vec![MatrixCalibration {
                illuminant: CalibrationIlluminant::D50,
                xyz_to_camera: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                forward_camera_to_xyz_d50: forward,
            }],
        };
        profile.validate().expect("test profile is valid");
        profile
    }
}
