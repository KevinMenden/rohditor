use crate::{LensCorrectionPlan, OpticsError};

pub(crate) fn cubic_sample(
    data: &[f32],
    row_stride: usize,
    width: usize,
    height: usize,
    channel: usize,
    x: f32,
    y: f32,
) -> Result<f32, OpticsError> {
    if !x.is_finite() || !y.is_finite() {
        return Err(OpticsError::Correction {
            reason: "non-finite cubic source coordinate".to_owned(),
        });
    }
    let floor_x = x.floor() as isize;
    let floor_y = y.floor() as isize;
    if floor_x - 1 < 0
        || floor_y - 1 < 0
        || floor_x + 2 >= width as isize
        || floor_y + 2 >= height as isize
    {
        return Err(OpticsError::InvalidFootprint);
    }
    let mut value = 0.0_f32;
    for offset_y in -1..=2 {
        let weight_y = cubic_weight(y - (floor_y + offset_y) as f32);
        let source_y = (floor_y + offset_y) as usize;
        for offset_x in -1..=2 {
            let weight = weight_y * cubic_weight(x - (floor_x + offset_x) as f32);
            let source_x = (floor_x + offset_x) as usize;
            value += data[source_y * row_stride + source_x * 3 + channel] * weight;
        }
    }
    if !value.is_finite() {
        return Err(OpticsError::Correction {
            reason: "cubic resampling produced a non-finite pixel".to_owned(),
        });
    }
    Ok(value)
}

/// Catmull-Rom cubic interpolation with a fixed tension of -0.5.
#[must_use]
pub(crate) fn cubic_weight(distance: f32) -> f32 {
    let absolute = distance.abs();
    if absolute <= 1.0 {
        1.5 * absolute * absolute * absolute - 2.5 * absolute * absolute + 1.0
    } else if absolute < 2.0 {
        -0.5 * absolute * absolute * absolute + 2.5 * absolute * absolute - 4.0 * absolute + 2.0
    } else {
        0.0
    }
}

#[allow(dead_code)]
pub(crate) fn _plan_dimensions(plan: &LensCorrectionPlan) -> (usize, usize) {
    plan.dimensions()
}
