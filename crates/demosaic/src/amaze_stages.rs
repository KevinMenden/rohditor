//! Directional stages for the tiled AMaZE reconstruction.

use super::amaze::{
    AR_THRESHOLD, AmazeScratch, BORDER, CLIP_POINT, EPSILON, EPSILON_SQUARED, TILE_SIZE,
};
use super::{CancellationCheck, DemosaicError, checkpoint};
use rohditor_image::{BayerPattern, CfaColor};

/// Compute the horizontal and vertical CFA gradients used by both the green
/// and chroma reconstructions.
pub(super) fn calculate_gradients(
    scratch: &mut AmazeScratch,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DemosaicError> {
    for row in 2..TILE_SIZE - 2 {
        checkpoint(cancellation)?;
        for col in 2..TILE_SIZE - 2 {
            let index = row * TILE_SIZE + col;
            let horizontal = (scratch.cfa[index + 1] - scratch.cfa[index - 1]).abs();
            let vertical = (scratch.cfa[index + TILE_SIZE] - scratch.cfa[index - TILE_SIZE]).abs();
            scratch.vertical_weights[index] = EPSILON
                + (scratch.cfa[index + 2 * TILE_SIZE] - scratch.cfa[index]).abs()
                + (scratch.cfa[index] - scratch.cfa[index - 2 * TILE_SIZE]).abs()
                + vertical;
            scratch.horizontal_weights[index] = EPSILON
                + (scratch.cfa[index + 2] - scratch.cfa[index]).abs()
                + (scratch.cfa[index] - scratch.cfa[index - 2]).abs()
                + horizontal;
        }
    }
    Ok(())
}

/// Reconstruct green at red and blue sites with AMaZE's adaptive-ratio and
/// Hamilton-Adams cardinal estimates.
pub(super) fn interpolate_green(
    scratch: &mut AmazeScratch,
    pattern: BayerPattern,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DemosaicError> {
    for row in 4..TILE_SIZE - 4 {
        checkpoint(cancellation)?;
        for col in 4..TILE_SIZE - 4 {
            let index = row * TILE_SIZE + col;
            if pattern.color_at(col, row) == CfaColor::Green {
                scratch.green[index] = scratch.cfa[index];
                continue;
            }

            let center = scratch.cfa[index];
            let up_ha = scratch.cfa[index - TILE_SIZE]
                + 0.5 * (center - scratch.cfa[index - 2 * TILE_SIZE]);
            let down_ha = scratch.cfa[index + TILE_SIZE]
                + 0.5 * (center - scratch.cfa[index + 2 * TILE_SIZE]);
            let left_ha = scratch.cfa[index - 1] + 0.5 * (center - scratch.cfa[index - 2]);
            let right_ha = scratch.cfa[index + 1] + 0.5 * (center - scratch.cfa[index + 2]);

            let up_ratio = ratio(
                scratch.cfa[index - TILE_SIZE],
                center,
                scratch.cfa[index - 2 * TILE_SIZE],
                scratch.vertical_weights[index - 2 * TILE_SIZE],
                scratch.vertical_weights[index],
            );
            let down_ratio = ratio(
                scratch.cfa[index + TILE_SIZE],
                center,
                scratch.cfa[index + 2 * TILE_SIZE],
                scratch.vertical_weights[index + 2 * TILE_SIZE],
                scratch.vertical_weights[index],
            );
            let left_ratio = ratio(
                scratch.cfa[index - 1],
                center,
                scratch.cfa[index - 2],
                scratch.horizontal_weights[index - 2],
                scratch.horizontal_weights[index],
            );
            let right_ratio = ratio(
                scratch.cfa[index + 1],
                center,
                scratch.cfa[index + 2],
                scratch.horizontal_weights[index + 2],
                scratch.horizontal_weights[index],
            );

            let up = adaptive(center, up_ratio, up_ha);
            let down = adaptive(center, down_ratio, down_ha);
            let left = adaptive(center, left_ratio, left_ha);
            let right = adaptive(center, right_ratio, right_ha);
            let vertical_weight = directional_weight(
                scratch.vertical_weights[index - TILE_SIZE],
                scratch.vertical_weights[index + TILE_SIZE],
            );
            let horizontal_weight = directional_weight(
                scratch.horizontal_weights[index - 1],
                scratch.horizontal_weights[index + 1],
            );
            let vertical = vertical_weight * down + (1.0 - vertical_weight) * up;
            let horizontal = horizontal_weight * right + (1.0 - horizontal_weight) * left;

            scratch.vertical_difference[index] = vertical - center;
            scratch.horizontal_difference[index] = horizontal - center;
            scratch.vertical_alternative[index] = (down_ha + up_ha) * 0.5 - center;
            scratch.horizontal_alternative[index] = (right_ha + left_ha) * 0.5 - center;

            // Strong CFA gradients indicate that the corresponding axis is
            // less reliable. This is the directional discrimination that
            // prevents zippering on slanted edges.
            let vertical_axis_weight = scratch.horizontal_weights[index]
                / nonzero(scratch.horizontal_weights[index] + scratch.vertical_weights[index]);
            let mut result =
                vertical_axis_weight * vertical + (1.0 - vertical_axis_weight) * horizontal;
            if center > 0.8 * CLIP_POINT
                || vertical > 0.8 * CLIP_POINT
                || horizontal > 0.8 * CLIP_POINT
            {
                result = 0.5 * (up_ha + down_ha + left_ha + right_ha) * 0.5;
            }
            scratch.green[index] = result;
        }
    }

    // Select the smoother of the ratio and Hamilton-Adams colour-difference
    // estimates before chroma interpolation consumes the green plane.
    for row in 4..TILE_SIZE - 4 {
        checkpoint(cancellation)?;
        for col in 4..TILE_SIZE - 4 {
            let index = row * TILE_SIZE + col;
            if pattern.color_at(col, row) == CfaColor::Green {
                continue;
            }
            let vertical_variance = variance(
                scratch.vertical_difference[index - 2 * TILE_SIZE],
                scratch.vertical_difference[index],
                scratch.vertical_difference[index + 2 * TILE_SIZE],
            );
            let vertical_alternative_variance = variance(
                scratch.vertical_alternative[index - 2 * TILE_SIZE],
                scratch.vertical_alternative[index],
                scratch.vertical_alternative[index + 2 * TILE_SIZE],
            );
            let horizontal_variance = variance(
                scratch.horizontal_difference[index - 2],
                scratch.horizontal_difference[index],
                scratch.horizontal_difference[index + 2],
            );
            let horizontal_alternative_variance = variance(
                scratch.horizontal_alternative[index - 2],
                scratch.horizontal_alternative[index],
                scratch.horizontal_alternative[index + 2],
            );
            let vertical_difference = if vertical_alternative_variance < vertical_variance {
                scratch.vertical_alternative[index]
            } else {
                scratch.vertical_difference[index]
            };
            let horizontal_difference = if horizontal_alternative_variance < horizontal_variance {
                scratch.horizontal_alternative[index]
            } else {
                scratch.horizontal_difference[index]
            };
            if vertical_variance > horizontal_variance {
                scratch.green[index] = scratch.cfa[index] + horizontal_difference;
            } else {
                scratch.green[index] = scratch.cfa[index] + vertical_difference;
            }
        }
    }
    Ok(())
}

/// Fill red and blue at the opposite-color and green sites. The opposite
/// color uses four diagonal ratio estimates; green sites use the same-color
/// color differences along the appropriate Bayer axis.
pub(super) fn interpolate_chroma(
    scratch: &mut AmazeScratch,
    pattern: BayerPattern,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DemosaicError> {
    // First establish the missing opposite color at every red/blue site.
    // Green-site interpolation depends on both neighboring same-color
    // differences, so it must happen in a second pass.
    for row in BORDER / 2..TILE_SIZE - BORDER / 2 {
        checkpoint(cancellation)?;
        for col in BORDER / 2..TILE_SIZE - BORDER / 2 {
            let index = row * TILE_SIZE + col;
            match pattern.color_at(col, row) {
                CfaColor::Red => {
                    scratch.blue[index] = diagonal_color(scratch, index);
                    scratch.red[index] = scratch.cfa[index];
                }
                CfaColor::Blue => {
                    scratch.red[index] = diagonal_color(scratch, index);
                    scratch.blue[index] = scratch.cfa[index];
                }
                CfaColor::Green => {}
            }
        }
    }
    for row in BORDER / 2..TILE_SIZE - BORDER / 2 {
        checkpoint(cancellation)?;
        for col in BORDER / 2..TILE_SIZE - BORDER / 2 {
            let index = row * TILE_SIZE + col;
            if pattern.color_at(col, row) == CfaColor::Green {
                scratch.red[index] =
                    scratch.green[index] - green_site_difference(scratch, pattern, index, 0);
                scratch.blue[index] =
                    scratch.green[index] - green_site_difference(scratch, pattern, index, 2);
            }
        }
    }
    Ok(())
}

fn diagonal_color(scratch: &AmazeScratch, index: usize) -> f32 {
    let candidates = [
        diagonal_candidate(scratch, index, -1, -1),
        diagonal_candidate(scratch, index, 1, -1),
        diagonal_candidate(scratch, index, -1, 1),
        diagonal_candidate(scratch, index, 1, 1),
    ];
    let weights = [
        diagonal_weight(scratch, index, -1, -1),
        diagonal_weight(scratch, index, 1, -1),
        diagonal_weight(scratch, index, -1, 1),
        diagonal_weight(scratch, index, 1, 1),
    ];
    let weight_sum: f32 = weights.iter().sum();
    candidates
        .iter()
        .zip(weights)
        .map(|(candidate, weight)| candidate * weight)
        .sum::<f32>()
        / nonzero(weight_sum)
}

fn diagonal_candidate(scratch: &AmazeScratch, index: usize, dx: isize, dy: isize) -> f32 {
    let neighbor = offset(index, dx, dy);
    let far = offset(index, 2 * dx, 2 * dy);
    let center = scratch.cfa[index];
    let ratio = 2.0 * scratch.cfa[neighbor] / nonzero(EPSILON + center + scratch.cfa[far]);
    if (1.0 - ratio).abs() < AR_THRESHOLD {
        center * ratio
    } else {
        scratch.cfa[neighbor] + 0.5 * (center - scratch.cfa[far])
    }
}

fn diagonal_weight(scratch: &AmazeScratch, index: usize, dx: isize, dy: isize) -> f32 {
    let neighbor = offset(index, dx, dy);
    let opposite = offset(index, -dx, -dy);
    let far = offset(index, 2 * dx, 2 * dy);
    1.0 / (EPSILON
        + (scratch.cfa[neighbor] - scratch.cfa[opposite]).abs()
        + (scratch.cfa[index] - scratch.cfa[far]).abs())
}

fn green_site_difference(
    scratch: &AmazeScratch,
    pattern: BayerPattern,
    index: usize,
    channel: usize,
) -> f32 {
    let row = index / TILE_SIZE;
    let col = index % TILE_SIZE;
    let red_horizontal = pattern.color_at(col + 1, row) == CfaColor::Red;
    let channel_horizontal = if channel == 0 {
        red_horizontal
    } else {
        !red_horizontal
    };
    let (negative, positive) = if channel_horizontal {
        (index - 1, index + 1)
    } else {
        (index - TILE_SIZE, index + TILE_SIZE)
    };
    let negative_difference = difference(scratch, negative, channel);
    let positive_difference = difference(scratch, positive, channel);
    let negative_weight =
        EPSILON + local_difference_gradient(scratch, negative, channel, channel_horizontal);
    let positive_weight =
        EPSILON + local_difference_gradient(scratch, positive, channel, channel_horizontal);
    (positive_weight * negative_difference + negative_weight * positive_difference)
        / nonzero(negative_weight + positive_weight)
}

fn difference(scratch: &AmazeScratch, index: usize, channel: usize) -> f32 {
    scratch.green[index]
        - if channel == 0 {
            scratch.red[index]
        } else {
            scratch.blue[index]
        }
}

fn local_difference_gradient(
    scratch: &AmazeScratch,
    index: usize,
    channel: usize,
    horizontal: bool,
) -> f32 {
    let step = if horizontal { 2 } else { 2 * TILE_SIZE };
    (difference(scratch, index - step, channel) - difference(scratch, index + step, channel)).abs()
}

fn ratio(sample: f32, center: f32, far: f32, outer_weight: f32, inner_weight: f32) -> f32 {
    sample * (outer_weight + inner_weight)
        / nonzero(outer_weight * (EPSILON + center) + inner_weight * (EPSILON + far))
}

fn adaptive(center: f32, ratio: f32, hamilton_adams: f32) -> f32 {
    if (1.0 - ratio).abs() < AR_THRESHOLD {
        center * ratio
    } else {
        hamilton_adams
    }
}

fn directional_weight(negative: f32, positive: f32) -> f32 {
    negative / nonzero(negative + positive)
}

fn variance(first: f32, center: f32, last: f32) -> f32 {
    (3.0 * (square(first) + square(center) + square(last)) - square(first + center + last)).max(0.0)
}

fn square(value: f32) -> f32 {
    value * value
}

fn nonzero(value: f32) -> f32 {
    if value.abs() >= EPSILON_SQUARED {
        value
    } else if value.is_sign_negative() {
        -EPSILON
    } else {
        EPSILON
    }
}

fn offset(index: usize, dx: isize, dy: isize) -> usize {
    index
        .checked_add_signed(dy * TILE_SIZE as isize + dx)
        .expect("AMaZE neighborhood must remain within its tile")
}
