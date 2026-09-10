//! Small, dependency-free 3x3 color-matrix operations.

/// Failure from a pure color-math operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMathError {
    /// The matrix cannot be inverted at the precision used by the pipeline.
    SingularMatrix,
    /// The reference white cannot be used for chromatic adaptation.
    InvalidReferenceWhite,
}

impl std::fmt::Display for ColorMathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::SingularMatrix => "selected color matrix is singular",
            Self::InvalidReferenceWhite => "reference illuminant cannot be chromatically adapted",
        })
    }
}

impl std::error::Error for ColorMathError {}

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

    /// Compose this transform with `next`, applying `self` first.
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

    /// Invert this matrix while rejecting non-finite and near-singular input.
    pub fn inverse(self) -> Result<Self, ColorMathError> {
        let m = self.values;
        let determinant = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        if !determinant.is_finite() || determinant.abs() < 1.0e-8 {
            return Err(ColorMathError::SingularMatrix);
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

/// CIE XYZ reference white for D65.
pub const D65_WHITE: [f32; 3] = [0.950_455_9, 1.0, 1.089_057_8];
/// CIE XYZ reference white for D50.
pub const D50_WHITE: [f32; 3] = [0.964_22, 1.0, 0.825_21];
/// CIE XYZ reference white for standard illuminant A.
pub const A_WHITE: [f32; 3] = [1.098_5, 1.0, 0.355_85];

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

/// Bradford adaptation matrix from `source_white` to D65.
pub fn chromatic_adaptation_to_d65(source_white: [f32; 3]) -> Result<Matrix3, ColorMathError> {
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
        return Err(ColorMathError::InvalidReferenceWhite);
    }
    let scale = Matrix3::new([
        [destination_cone[0] / source_cone[0], 0.0, 0.0],
        [0.0, destination_cone[1] / source_cone[1], 0.0],
        [0.0, 0.0, destination_cone[2] / source_cone[2]],
    ]);
    Ok(bradford.then(scale).then(inverse_bradford))
}

/// Bradford-adapt one XYZ triplet from `source_white` to D65.
pub fn adapt_xyz_to_d65(xyz: [f32; 3], source_white: [f32; 3]) -> Result<[f32; 3], ColorMathError> {
    Ok(chromatic_adaptation_to_d65(source_white)?.transform(xyz))
}

fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32, tolerance: f32) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "{actual} != {expected}"
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
}
