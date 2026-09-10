use rohditor_demosaic::WhiteBalanceGains;
use rohditor_edit::{TEMPERATURE_RANGE, TINT_RANGE};

use crate::{Matrix3, PipelineError};

/// Version of the camera-calibrated Temperature/Tint conversion.
pub const WHITE_BALANCE_ALGORITHM_VERSION: u8 = 2;

/// User-facing white-balance coordinates for a RAW image.
///
/// Temperature is a correlated colour temperature in Kelvin. Tint is a
/// normalized green-to-magenta control: negative values move toward green and
/// positive values move toward magenta.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WhiteBalanceCoordinates {
    pub temperature: f32,
    pub tint: f32,
}

/// Convert normalized camera gains into the same coordinates shown by the UI.
pub fn coordinates_from_camera_gains(
    camera_to_xyz_d65: Matrix3,
    gains: WhiteBalanceGains,
) -> Result<WhiteBalanceCoordinates, PipelineError> {
    gains
        .validate()
        .map_err(|error| PipelineError::InvalidMetadata {
            field: "white_balance",
            reason: error.to_string(),
        })?;
    let camera_white = [1.0 / gains.red, 1.0 / gains.green, 1.0 / gains.blue];
    coordinates_from_xyz(camera_to_xyz_d65.transform(camera_white))
}

/// Convert absolute Temperature/Tint coordinates into normalized camera gains.
pub fn camera_gains_from_coordinates(
    camera_to_xyz_d65: Matrix3,
    coordinates: WhiteBalanceCoordinates,
) -> Result<WhiteBalanceGains, PipelineError> {
    if !TEMPERATURE_RANGE.contains(coordinates.temperature)
        || !TINT_RANGE.contains(coordinates.tint)
    {
        return Err(PipelineError::InvalidRecipe {
            field: "color.white_balance",
            reason: "temperature or tint is outside its declared range".to_owned(),
        });
    }
    let xyz = xyz_from_coordinates(coordinates)?;
    let camera_white = camera_to_xyz_d65.inverse()?.transform(xyz);
    if camera_white
        .iter()
        .any(|value| !value.is_finite() || *value <= 1.0e-8)
    {
        return Err(PipelineError::InvalidMetadata {
            field: "temperature_tint",
            reason: "the requested white point is outside the calibrated camera gamut".to_owned(),
        });
    }
    let green = camera_white[1];
    let gains = WhiteBalanceGains {
        red: green / camera_white[0],
        green: 1.0,
        blue: green / camera_white[2],
    };
    gains
        .validate()
        .map_err(|error| PipelineError::InvalidMetadata {
            field: "temperature_tint",
            reason: error.to_string(),
        })?;
    Ok(gains)
}

fn coordinates_from_xyz(xyz: [f32; 3]) -> Result<WhiteBalanceCoordinates, PipelineError> {
    if xyz.iter().any(|value| !value.is_finite() || *value <= 0.0) {
        return Err(PipelineError::InvalidMetadata {
            field: "white_balance",
            reason: "camera white point is not a finite positive XYZ value".to_owned(),
        });
    }
    let denominator = xyz[0] + 15.0 * xyz[1] + 3.0 * xyz[2];
    if !denominator.is_finite() || denominator <= 1.0e-8 {
        return Err(PipelineError::InvalidMetadata {
            field: "white_balance",
            reason: "camera white point has an invalid chromaticity denominator".to_owned(),
        });
    }
    let uv = [4.0 * xyz[0] / denominator, 9.0 * xyz[1] / denominator];
    let (temperature, normal) = nearest_locus_point(uv);
    let locus = locus_uv(temperature);
    let distance = dot2(sub2(uv, locus), normal);
    let tint = -distance / TINT_DUV_PER_FULL_VALUE;
    if !TINT_RANGE.contains(tint) {
        return Err(PipelineError::InvalidMetadata {
            field: "white_balance",
            reason: "camera white point is outside the Temperature/Tint range".to_owned(),
        });
    }
    Ok(WhiteBalanceCoordinates { temperature, tint })
}

fn xyz_from_coordinates(coordinates: WhiteBalanceCoordinates) -> Result<[f32; 3], PipelineError> {
    let locus = locus_uv(coordinates.temperature);
    let normal = green_normal(coordinates.temperature);
    let uv = add2(
        locus,
        scale2(normal, -coordinates.tint * TINT_DUV_PER_FULL_VALUE),
    );
    if !uv[1].is_finite() || uv[1] <= 1.0e-8 {
        return Err(PipelineError::InvalidRecipe {
            field: "color.white_balance",
            reason: "temperature and tint produce an invalid chromaticity".to_owned(),
        });
    }
    let xyz = [
        9.0 * uv[0] / (4.0 * uv[1]),
        1.0,
        (12.0 - 3.0 * uv[0] - 20.0 * uv[1]) / (4.0 * uv[1]),
    ];
    if xyz
        .iter()
        .any(|value| !value.is_finite() || *value <= 1.0e-8)
    {
        return Err(PipelineError::InvalidRecipe {
            field: "color.white_balance",
            reason: "temperature and tint produce an invalid white point".to_owned(),
        });
    }
    Ok(xyz)
}

fn nearest_locus_point(uv: [f32; 2]) -> (f32, [f32; 2]) {
    const SAMPLES: usize = 192;
    let minimum = TEMPERATURE_RANGE.minimum;
    let maximum = TEMPERATURE_RANGE.maximum;
    let step = (maximum - minimum) / SAMPLES as f32;
    let mut best_temperature = minimum;
    let mut best_distance = f32::INFINITY;
    for index in 0..=SAMPLES {
        let temperature = (minimum + index as f32 * step).min(maximum);
        let distance = distance_squared(uv, locus_uv(temperature));
        if distance < best_distance {
            best_distance = distance;
            best_temperature = temperature;
        }
    }

    let mut low = (best_temperature - step).max(minimum);
    let mut high = (best_temperature + step).min(maximum);
    for _ in 0..24 {
        let left = low + (high - low) / 3.0;
        let right = high - (high - low) / 3.0;
        if distance_squared(uv, locus_uv(left)) <= distance_squared(uv, locus_uv(right)) {
            high = right;
        } else {
            low = left;
        }
    }
    best_temperature = (low + high) * 0.5;
    (best_temperature, green_normal(best_temperature))
}

/// Return the CIE 1976 u'v' white point for a daylight/black-body temperature.
/// The piecewise approximation is smooth enough for editing and is shared by
/// the forward and inverse transformations.
fn locus_uv(temperature: f32) -> [f32; 2] {
    let temperature = temperature.clamp(TEMPERATURE_RANGE.minimum, TEMPERATURE_RANGE.maximum);
    let xyz = xyz_from_temperature(temperature);
    let denominator = xyz[0] + 15.0 * xyz[1] + 3.0 * xyz[2];
    [4.0 * xyz[0] / denominator, 9.0 * xyz[1] / denominator]
}

pub(crate) fn xyz_from_temperature(temperature: f32) -> [f32; 3] {
    let temperature = temperature.clamp(TEMPERATURE_RANGE.minimum, TEMPERATURE_RANGE.maximum);
    let (x, y) = xy_from_temperature(temperature);
    [x / y, 1.0, (1.0 - x - y) / y]
}

fn xy_from_temperature(temperature: f32) -> (f32, f32) {
    if temperature <= 4_000.0 {
        let x = -0.266_123_9e9 / temperature.powi(3) - 2.343_58e5 / temperature.powi(2)
            + 0.877_695_6e3 / temperature
            + 0.179_910;
        let y = -1.106_381_4 * x.powi(3) - 1.348_110_2 * x.powi(2) + 2.185_558_3 * x - 0.202_196_83;
        (x, y)
    } else {
        let x = if temperature <= 7_000.0 {
            -4_607_000_000.0 / temperature.powi(3)
                + 2_967_800.0 / temperature.powi(2)
                + 99.11 / temperature
                + 0.244_063
        } else {
            -2_006_400_000.0 / temperature.powi(3)
                + 1_901_800.0 / temperature.powi(2)
                + 247.48 / temperature
                + 0.237_040
        };
        let y = -3.0 * x.powi(2) + 2.87 * x - 0.275;
        (x, y)
    }
}

fn green_normal(temperature: f32) -> [f32; 2] {
    let span = (TEMPERATURE_RANGE.maximum - TEMPERATURE_RANGE.minimum) * 1.0e-4;
    let low = (temperature - span).max(TEMPERATURE_RANGE.minimum);
    let high = (temperature + span).min(TEMPERATURE_RANGE.maximum);
    let tangent = scale2(
        sub2(locus_uv(high), locus_uv(low)),
        1.0 / (high - low).max(f32::EPSILON),
    );
    let length = (tangent[0] * tangent[0] + tangent[1] * tangent[1])
        .sqrt()
        .max(f32::EPSILON);
    // This normal points toward the green side of the locus. Positive UI tint
    // is magenta, so the forward transform applies its negative.
    [-tangent[1] / length, tangent[0] / length]
}

fn distance_squared(first: [f32; 2], second: [f32; 2]) -> f32 {
    let delta = sub2(first, second);
    dot2(delta, delta)
}

fn add2(first: [f32; 2], second: [f32; 2]) -> [f32; 2] {
    [first[0] + second[0], first[1] + second[1]]
}

fn sub2(first: [f32; 2], second: [f32; 2]) -> [f32; 2] {
    [first[0] - second[0], first[1] - second[1]]
}

fn scale2(value: [f32; 2], scale: f32) -> [f32; 2] {
    [value[0] * scale, value[1] * scale]
}

fn dot2(first: [f32; 2], second: [f32; 2]) -> f32 {
    first[0] * second[0] + first[1] * second[1]
}

const TINT_DUV_PER_FULL_VALUE: f32 = 0.02;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temperature_tint_round_trips_through_camera_gains() {
        let matrix = Matrix3::new([[0.90, 0.08, 0.02], [0.03, 0.94, 0.03], [0.01, 0.08, 0.91]]);
        let coordinates = WhiteBalanceCoordinates {
            temperature: 3_200.0,
            tint: 0.18,
        };
        let gains = camera_gains_from_coordinates(matrix, coordinates).expect("valid gains");
        let round_trip = coordinates_from_camera_gains(matrix, gains).expect("valid coordinates");
        assert!((round_trip.temperature - coordinates.temperature).abs() < 2.0);
        assert!((round_trip.tint - coordinates.tint).abs() < 0.01);
    }

    #[test]
    fn as_shot_coordinates_are_camera_dependent() {
        let source = WhiteBalanceCoordinates {
            temperature: 5_200.0,
            tint: 0.1,
        };
        let first_gains =
            camera_gains_from_coordinates(Matrix3::identity(), source).expect("first gains");
        let first = coordinates_from_camera_gains(Matrix3::identity(), first_gains)
            .expect("first coordinates");
        let camera_matrix =
            Matrix3::new([[0.90, 0.08, 0.02], [0.03, 0.94, 0.03], [0.01, 0.08, 0.91]]);
        let second =
            coordinates_from_camera_gains(camera_matrix, first_gains).expect("second coordinates");
        assert!(
            (first.temperature - second.temperature).abs() > 1.0
                || (first.tint - second.tint).abs() > 0.01
        );
    }
}
