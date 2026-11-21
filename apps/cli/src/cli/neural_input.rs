//! `neural-input-npy`: the neural demosaic's packed, balanced input tensor for
//! a RAW file, as Rohditor builds it. The Python parity test
//! (`ml/tests/eval/test_rust_parity.py`) compares it with the training
//! pipeline's packing of the same file.

use std::path::Path;

use anyhow::{Context, Result};
use rohditor_core::{CpuPipeline, RawCropPolicy, RenderOptions, normalize_raw, write_output_bytes};
use rohditor_edit::EditRecipe;
use rohditor_raw::{RawDecoder, RawlerDecoder};

use super::commands::write_stdout;
use super::npy::encode_f32;

/// Writes `[1, 4, H/2, W/2]` and prints the sensor position of packed `(0, 0)`
/// and the as-shot gains, so the caller can align it with another decoder.
pub(super) fn neural_input_npy(file: &Path, output: &Path, force: bool) -> Result<()> {
    let frame = RawlerDecoder::default()
        .decode(file)
        .with_context(|| format!("could not decode {}", file.display()))?;
    let crop = RawCropPolicy::ActiveArea;
    let options = RenderOptions {
        raw_crop_policy: crop,
        ..RenderOptions::default()
    };
    let sensor = CpuPipeline::default().describe_sensor_development(
        &frame,
        &EditRecipe::default(),
        options,
    )?;
    let gains = sensor.demosaic_balance();
    let mosaic = normalize_raw(&frame, crop)?;
    let (packed, (shift_x, shift_y)) =
        rohditor_neural::pack_image(&mosaic, [gains.red, gains.green, gains.blue])?;
    write_output_bytes(output, &encode_f32(&packed.shape(), packed.data()), force)?;
    let normalization = sensor.normalization();
    let (origin_x, origin_y) = normalization.crop().origin();
    // Black and white of the first whole RGGB block inside the image, in
    // packed channel order R, G1, G2, B.
    let (block_x, block_y) = (origin_x + shift_x % 2, origin_y + shift_y % 2);
    let mut black = [0.0; 4];
    let mut white = [0.0; 4];
    for (channel, (dx, dy)) in [(0, 0), (1, 0), (0, 1), (1, 1)].into_iter().enumerate() {
        (black[channel], white[channel]) =
            normalization.levels_at_sensor(block_x + dx, block_y + dy)?;
    }
    // Packed (0, 0) sits `shift` canvas pixels before the crop origin.
    write_stdout(&format!(
        "{{\"sensor_origin\": [{}, {}], \"crop_origin\": [{origin_x}, {origin_y}], \"image_size\": [{}, {}], \"gains\": [{}, 1.0, {}], \"black\": {:?}, \"white\": {:?}}}",
        origin_x as isize - shift_x as isize,
        origin_y as isize - shift_y as isize,
        mosaic.width(),
        mosaic.height(),
        gains.red / gains.green,
        gains.blue / gains.green,
        black,
        white,
    ))
}
