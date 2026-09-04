//! RCD's directional reconstruction stages.

use super::rcd::{EPSILON, EPSILON_SQUARED, RcdScratch, TILE_SIZE};
use super::{CancellationCheck, DemosaicError, checkpoint};
use rohditor_image::BayerPattern;

pub(super) fn find_directions(
    scratch: &mut RcdScratch,
    tile_width: usize,
    tile_height: usize,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DemosaicError> {
    let initial_end = tile_height.saturating_sub(3).min(5);
    for row in 3..initial_end {
        checkpoint(cancellation)?;
        for col in 4..tile_width - 4 {
            let index = row * TILE_SIZE + col;
            scratch.vertical_buffer[row - 3][col - 4] =
                square(vertical_high_pass(&scratch.cfa, index));
        }
    }

    let mut vertical_0 = 0;
    let mut vertical_1 = 1;
    let mut vertical_2 = 2;
    for row in 4..tile_height - 4 {
        checkpoint(cancellation)?;
        for col in 3..tile_width - 3 {
            let index = row * TILE_SIZE + col;
            scratch.horizontal_buffer[col - 3] = square(horizontal_high_pass(&scratch.cfa, index));
        }
        for col in 4..tile_width - 4 {
            let index = (row + 1) * TILE_SIZE + col;
            scratch.vertical_buffer[vertical_2][col - 4] =
                square(vertical_high_pass(&scratch.cfa, index));
        }
        for col in 4..tile_width - 4 {
            let index = row * TILE_SIZE + col;
            let vertical_stat = (scratch.vertical_buffer[vertical_0][col - 4]
                + scratch.vertical_buffer[vertical_1][col - 4]
                + scratch.vertical_buffer[vertical_2][col - 4])
                .max(EPSILON_SQUARED);
            let horizontal_stat = (scratch.horizontal_buffer[col - 4]
                + scratch.horizontal_buffer[col - 3]
                + scratch.horizontal_buffer[col - 2])
                .max(EPSILON_SQUARED);
            scratch.vh_direction[index] = vertical_stat / (vertical_stat + horizontal_stat);
        }
        (vertical_0, vertical_1, vertical_2) = (vertical_1, vertical_2, vertical_0);
    }
    Ok(())
}

pub(super) fn calculate_low_pass(
    scratch: &mut RcdScratch,
    tile_pattern: BayerPattern,
    tile_width: usize,
    tile_height: usize,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DemosaicError> {
    for row in 2..tile_height - 2 {
        checkpoint(cancellation)?;
        let start = 2 + (tile_pattern.color_at(0, row).channel_index() & 1);
        for col in (start..tile_width - 2).step_by(2) {
            let index = row * TILE_SIZE + col;
            let low_pass = scratch.cfa[index]
                + 0.5
                    * (scratch.cfa[index - TILE_SIZE]
                        + scratch.cfa[index + TILE_SIZE]
                        + scratch.cfa[index - 1]
                        + scratch.cfa[index + 1])
                + 0.25
                    * (scratch.cfa[index - TILE_SIZE - 1]
                        + scratch.cfa[index - TILE_SIZE + 1]
                        + scratch.cfa[index + TILE_SIZE - 1]
                        + scratch.cfa[index + TILE_SIZE + 1]);
            scratch.pq_direction[index / 2] = low_pass;
        }
    }
    Ok(())
}

pub(super) fn interpolate_green(
    scratch: &mut RcdScratch,
    tile_pattern: BayerPattern,
    tile_width: usize,
    tile_height: usize,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DemosaicError> {
    for row in 4..tile_height - 4 {
        checkpoint(cancellation)?;
        let start = 4 + (tile_pattern.color_at(0, row).channel_index() & 1);
        for col in (start..tile_width - 4).step_by(2) {
            let index = row * TILE_SIZE + col;
            let north_gradient = cardinal_gradient(&scratch.cfa, index, 0, -1);
            let south_gradient = cardinal_gradient(&scratch.cfa, index, 0, 1);
            let west_gradient = cardinal_gradient(&scratch.cfa, index, -1, 0);
            let east_gradient = cardinal_gradient(&scratch.cfa, index, 1, 0);

            let low_pass_index = index / 2;
            let low_pass = scratch.pq_direction[low_pass_index];
            let north_estimate = ratio_estimate(
                scratch.cfa[index - TILE_SIZE],
                low_pass,
                scratch.pq_direction[low_pass_index - TILE_SIZE / 2],
            );
            let south_estimate = ratio_estimate(
                scratch.cfa[index + TILE_SIZE],
                low_pass,
                scratch.pq_direction[low_pass_index + TILE_SIZE / 2],
            );
            let west_estimate = ratio_estimate(
                scratch.cfa[index - 1],
                low_pass,
                scratch.pq_direction[low_pass_index - 1],
            );
            let east_estimate = ratio_estimate(
                scratch.cfa[index + 1],
                low_pass,
                scratch.pq_direction[low_pass_index + 1],
            );

            let vertical_estimate = weighted_pair(
                south_gradient,
                north_estimate,
                north_gradient,
                south_estimate,
            );
            let horizontal_estimate =
                weighted_pair(west_gradient, east_estimate, east_gradient, west_estimate);
            let direction = refined_direction(
                scratch.vh_direction[index],
                scratch.vh_direction[index - TILE_SIZE - 1],
                scratch.vh_direction[index - TILE_SIZE + 1],
                scratch.vh_direction[index + TILE_SIZE - 1],
                scratch.vh_direction[index + TILE_SIZE + 1],
            );
            scratch.rgb[1][index] = interpolate(direction, horizontal_estimate, vertical_estimate);
        }
    }
    Ok(())
}

pub(super) fn interpolate_red_blue(
    scratch: &mut RcdScratch,
    tile_pattern: BayerPattern,
    tile_width: usize,
    tile_height: usize,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DemosaicError> {
    calculate_diagonal_high_pass(scratch, tile_width, tile_height, cancellation)?;
    calculate_diagonal_directions(scratch, tile_pattern, tile_width, tile_height, cancellation)?;
    interpolate_opposite_color(scratch, tile_pattern, tile_width, tile_height, cancellation)?;
    interpolate_at_green(scratch, tile_pattern, tile_width, tile_height, cancellation)
}

pub(super) fn calculate_diagonal_high_pass(
    scratch: &mut RcdScratch,
    tile_width: usize,
    tile_height: usize,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DemosaicError> {
    for row in 3..tile_height - 3 {
        checkpoint(cancellation)?;
        for col in (3..tile_width - 3).step_by(2) {
            let index = row * TILE_SIZE + col;
            let half_index = index / 2;
            scratch.p_color_difference[half_index] = square(
                (scratch.cfa[index - 3 * TILE_SIZE - 3]
                    - scratch.cfa[index - TILE_SIZE - 1]
                    - scratch.cfa[index + TILE_SIZE + 1]
                    + scratch.cfa[index + 3 * TILE_SIZE + 3])
                    - 3.0
                        * (scratch.cfa[index - 2 * TILE_SIZE - 2]
                            + scratch.cfa[index + 2 * TILE_SIZE + 2])
                    + 6.0 * scratch.cfa[index],
            );
            scratch.q_color_difference[half_index] = square(
                (scratch.cfa[index - 3 * TILE_SIZE + 3]
                    - scratch.cfa[index - TILE_SIZE + 1]
                    - scratch.cfa[index + TILE_SIZE - 1]
                    + scratch.cfa[index + 3 * TILE_SIZE - 3])
                    - 3.0
                        * (scratch.cfa[index - 2 * TILE_SIZE + 2]
                            + scratch.cfa[index + 2 * TILE_SIZE - 2])
                    + 6.0 * scratch.cfa[index],
            );
        }
    }
    Ok(())
}

pub(super) fn calculate_diagonal_directions(
    scratch: &mut RcdScratch,
    tile_pattern: BayerPattern,
    tile_width: usize,
    tile_height: usize,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DemosaicError> {
    for row in 4..tile_height - 4 {
        checkpoint(cancellation)?;
        let start = 4 + (tile_pattern.color_at(0, row).channel_index() & 1);
        for col in (start..tile_width - 4).step_by(2) {
            let index = row * TILE_SIZE + col;
            let half_index = index / 2;
            let previous_row = (index - TILE_SIZE - 1) / 2;
            let next_row = (index + TILE_SIZE - 1) / 2;
            let p_stat = (scratch.p_color_difference[previous_row]
                + scratch.p_color_difference[half_index]
                + scratch.p_color_difference[next_row + 1])
                .max(EPSILON_SQUARED);
            let q_stat = (scratch.q_color_difference[previous_row + 1]
                + scratch.q_color_difference[half_index]
                + scratch.q_color_difference[next_row])
                .max(EPSILON_SQUARED);
            scratch.pq_direction[half_index] = p_stat / (p_stat + q_stat);
        }
    }
    Ok(())
}

pub(super) fn interpolate_opposite_color(
    scratch: &mut RcdScratch,
    tile_pattern: BayerPattern,
    tile_width: usize,
    tile_height: usize,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DemosaicError> {
    for row in 4..tile_height - 4 {
        checkpoint(cancellation)?;
        let start = 4 + (tile_pattern.color_at(0, row).channel_index() & 1);
        for col in (start..tile_width - 4).step_by(2) {
            let index = row * TILE_SIZE + col;
            let channel = 2 - tile_pattern.color_at(col, row).channel_index();
            let half_index = index / 2;
            let previous_row = (index - TILE_SIZE - 1) / 2;
            let next_row = (index + TILE_SIZE - 1) / 2;
            let direction = refined_direction(
                scratch.pq_direction[half_index],
                scratch.pq_direction[previous_row],
                scratch.pq_direction[previous_row + 1],
                scratch.pq_direction[next_row],
                scratch.pq_direction[next_row + 1],
            );

            let north_west_gradient =
                diagonal_gradient(&scratch.rgb[channel], &scratch.rgb[1], index, -1, -1);
            let north_east_gradient =
                diagonal_gradient(&scratch.rgb[channel], &scratch.rgb[1], index, 1, -1);
            let south_west_gradient =
                diagonal_gradient(&scratch.rgb[channel], &scratch.rgb[1], index, -1, 1);
            let south_east_gradient =
                diagonal_gradient(&scratch.rgb[channel], &scratch.rgb[1], index, 1, 1);

            let north_west_estimate =
                color_difference(&scratch.rgb, channel, index - TILE_SIZE - 1);
            let north_east_estimate =
                color_difference(&scratch.rgb, channel, index - TILE_SIZE + 1);
            let south_west_estimate =
                color_difference(&scratch.rgb, channel, index + TILE_SIZE - 1);
            let south_east_estimate =
                color_difference(&scratch.rgb, channel, index + TILE_SIZE + 1);
            let diagonal_p = weighted_pair(
                north_west_gradient,
                south_east_estimate,
                south_east_gradient,
                north_west_estimate,
            );
            let diagonal_q = weighted_pair(
                north_east_gradient,
                south_west_estimate,
                south_west_gradient,
                north_east_estimate,
            );
            let green = scratch.rgb[1][index];
            scratch.rgb[channel][index] = green + interpolate(direction, diagonal_q, diagonal_p);
        }
    }
    Ok(())
}

pub(super) fn interpolate_at_green(
    scratch: &mut RcdScratch,
    tile_pattern: BayerPattern,
    tile_width: usize,
    tile_height: usize,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DemosaicError> {
    for row in 4..tile_height - 4 {
        checkpoint(cancellation)?;
        let start = 4 + (tile_pattern.color_at(1, row).channel_index() & 1);
        for col in (start..tile_width - 4).step_by(2) {
            let index = row * TILE_SIZE + col;
            let direction = refined_direction(
                scratch.vh_direction[index],
                scratch.vh_direction[index - TILE_SIZE - 1],
                scratch.vh_direction[index - TILE_SIZE + 1],
                scratch.vh_direction[index + TILE_SIZE - 1],
                scratch.vh_direction[index + TILE_SIZE + 1],
            );
            let green = scratch.rgb[1][index];
            let north_1 = EPSILON + (green - scratch.rgb[1][index - 2 * TILE_SIZE]).abs();
            let south_1 = EPSILON + (green - scratch.rgb[1][index + 2 * TILE_SIZE]).abs();
            let west_1 = EPSILON + (green - scratch.rgb[1][index - 2]).abs();
            let east_1 = EPSILON + (green - scratch.rgb[1][index + 2]).abs();
            let green_north = scratch.rgb[1][index - TILE_SIZE];
            let green_south = scratch.rgb[1][index + TILE_SIZE];
            let green_west = scratch.rgb[1][index - 1];
            let green_east = scratch.rgb[1][index + 1];

            for channel in (0..=2).step_by(2) {
                let north_south_abs = (scratch.rgb[channel][index - TILE_SIZE]
                    - scratch.rgb[channel][index + TILE_SIZE])
                    .abs();
                let east_west_abs =
                    (scratch.rgb[channel][index - 1] - scratch.rgb[channel][index + 1]).abs();
                let north_gradient = north_1
                    + north_south_abs
                    + (scratch.rgb[channel][index - TILE_SIZE]
                        - scratch.rgb[channel][index - 3 * TILE_SIZE])
                        .abs();
                let south_gradient = south_1
                    + north_south_abs
                    + (scratch.rgb[channel][index + TILE_SIZE]
                        - scratch.rgb[channel][index + 3 * TILE_SIZE])
                        .abs();
                let west_gradient = west_1
                    + east_west_abs
                    + (scratch.rgb[channel][index - 1] - scratch.rgb[channel][index - 3]).abs();
                let east_gradient = east_1
                    + east_west_abs
                    + (scratch.rgb[channel][index + 1] - scratch.rgb[channel][index + 3]).abs();
                let north_estimate = scratch.rgb[channel][index - TILE_SIZE] - green_north;
                let south_estimate = scratch.rgb[channel][index + TILE_SIZE] - green_south;
                let west_estimate = scratch.rgb[channel][index - 1] - green_west;
                let east_estimate = scratch.rgb[channel][index + 1] - green_east;
                let vertical = weighted_pair(
                    north_gradient,
                    south_estimate,
                    south_gradient,
                    north_estimate,
                );
                let horizontal =
                    weighted_pair(east_gradient, west_estimate, west_gradient, east_estimate);
                scratch.rgb[channel][index] = green + interpolate(direction, horizontal, vertical);
            }
        }
    }
    Ok(())
}

fn cardinal_gradient(values: &[f32], index: usize, direction_x: isize, direction_y: isize) -> f32 {
    let one = offset(index, direction_x, direction_y);
    let opposite = offset(index, -direction_x, -direction_y);
    let two = offset(index, 2 * direction_x, 2 * direction_y);
    let three = offset(index, 3 * direction_x, 3 * direction_y);
    let four = offset(index, 4 * direction_x, 4 * direction_y);
    EPSILON
        + (values[one] - values[opposite]).abs()
        + (values[index] - values[two]).abs()
        + (values[one] - values[three]).abs()
        + (values[two] - values[four]).abs()
}

fn diagonal_gradient(
    values: &[f32],
    green: &[f32],
    index: usize,
    direction_x: isize,
    direction_y: isize,
) -> f32 {
    let near = offset(index, direction_x, direction_y);
    let opposite = offset(index, -direction_x, -direction_y);
    let far = offset(index, 3 * direction_x, 3 * direction_y);
    let green_two = offset(index, 2 * direction_x, 2 * direction_y);
    EPSILON
        + (values[near] - values[opposite]).abs()
        + (values[near] - values[far]).abs()
        + (green[index] - green[green_two]).abs()
}

fn offset(index: usize, direction_x: isize, direction_y: isize) -> usize {
    index
        .checked_add_signed(direction_y * TILE_SIZE as isize + direction_x)
        .expect("RCD neighborhood must remain within its tile")
}

fn vertical_high_pass(values: &[f32], index: usize) -> f32 {
    (values[index - 3 * TILE_SIZE] - values[index - TILE_SIZE] - values[index + TILE_SIZE]
        + values[index + 3 * TILE_SIZE])
        - 3.0 * (values[index - 2 * TILE_SIZE] + values[index + 2 * TILE_SIZE])
        + 6.0 * values[index]
}

fn horizontal_high_pass(values: &[f32], index: usize) -> f32 {
    (values[index - 3] - values[index - 1] - values[index + 1] + values[index + 3])
        - 3.0 * (values[index - 2] + values[index + 2])
        + 6.0 * values[index]
}

fn square(value: f32) -> f32 {
    value * value
}

fn ratio_estimate(sample: f32, low_pass: f32, neighboring_low_pass: f32) -> f32 {
    let denominator = nonzero_denominator(EPSILON + low_pass + neighboring_low_pass);
    sample * (2.0 * low_pass) / denominator
}

fn color_difference(rgb: &[Vec<f32>; 3], channel: usize, index: usize) -> f32 {
    rgb[channel][index] - rgb[1][index]
}

fn weighted_pair(
    first_weight: f32,
    first_value: f32,
    second_weight: f32,
    second_value: f32,
) -> f32 {
    (first_weight * first_value + second_weight * second_value)
        / nonzero_denominator(first_weight + second_weight)
}

fn nonzero_denominator(value: f32) -> f32 {
    if value.abs() >= EPSILON {
        value
    } else if value.is_sign_negative() {
        -EPSILON
    } else {
        EPSILON
    }
}

fn refined_direction(
    center: f32,
    north_west: f32,
    north_east: f32,
    south_west: f32,
    south_east: f32,
) -> f32 {
    let neighborhood = 0.25 * (north_west + north_east + south_west + south_east);
    let selected = if (0.5 - center).abs() < (0.5 - neighborhood).abs() {
        neighborhood
    } else {
        center
    };
    selected.clamp(0.0, 1.0)
}

fn interpolate(direction: f32, horizontal: f32, vertical: f32) -> f32 {
    direction.mul_add(horizontal, (1.0 - direction) * vertical)
}
