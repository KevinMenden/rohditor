use rayon::prelude::*;
use rohditor_image::MosaicImage;

use crate::detect::is_suspected_clipped;
use crate::{ChannelDetectionLevels, HighlightError, checkpoint};

/// Compact immutable evidence for one logical 2x2 Bayer cell.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct CellSummary {
    pub(crate) means: [f32; 3],
    pub(crate) flags: u8,
}

impl CellSummary {
    #[must_use]
    pub(crate) const fn is_usable(self, channel: usize) -> bool {
        self.flags & (1 << channel) != 0
    }

    #[must_use]
    pub(crate) const fn is_suspected_clipped(self, channel: usize) -> bool {
        self.flags & (1 << (channel + 3)) != 0
    }

    #[must_use]
    pub(crate) const fn is_clean(self, channel: usize) -> bool {
        !self.is_suspected_clipped(channel)
    }
}

/// Structure-of-arrays storage for immutable local-ratio evidence.
#[derive(Debug)]
pub(crate) struct CellSummaries {
    pub(crate) width: usize,
    pub(crate) height: usize,
    means: Vec<[f32; 3]>,
    flags: Vec<u8>,
}

impl CellSummaries {
    pub(crate) fn build(
        mosaic: &MosaicImage<f32>,
        levels: ChannelDetectionLevels,
        cancellation: &dyn crate::CancellationCheck,
    ) -> Result<Self, HighlightError> {
        let width = ceil_halved(mosaic.width());
        let height = ceil_halved(mosaic.height());
        let cell_count = width.checked_mul(height).ok_or_else(|| {
            HighlightError::Image(rohditor_image::ImageError::InvalidDimensions {
                width: mosaic.width(),
                height: mosaic.height(),
                row_stride: mosaic.row_stride(),
                reason: "local-ratio cell count overflowed".to_owned(),
            })
        })?;
        let mut means = Vec::new();
        means
            .try_reserve_exact(cell_count)
            .map_err(|_| allocation_error(cell_count))?;
        means.resize(cell_count, [0.0; 3]);
        let mut flags = Vec::new();
        flags
            .try_reserve_exact(cell_count)
            .map_err(|_| allocation_error(cell_count))?;
        flags.resize(cell_count, 0);

        means
            .par_chunks_mut(width)
            .zip(flags.par_chunks_mut(width))
            .enumerate()
            .try_for_each(
                |(cell_y, (mean_row, flag_row))| -> Result<(), HighlightError> {
                    checkpoint(cancellation)?;
                    for cell_x in 0..width {
                        let mut sums = [0.0_f32; 3];
                        let mut counts = [0_u32; 3];
                        let mut flags = 0_u8;
                        let origin_x = cell_x * 2;
                        let origin_y = cell_y * 2;
                        for y in origin_y..origin_y.saturating_add(2).min(mosaic.height()) {
                            for x in origin_x..origin_x.saturating_add(2).min(mosaic.width()) {
                                let color = mosaic.pattern().color_at(x, y);
                                let channel = color.channel_index();
                                let sample = *mosaic.sample(x, y);
                                if is_suspected_clipped(sample, levels.for_color(color)) {
                                    flags |= 1 << (channel + 3);
                                } else if sample > 0.0 {
                                    sums[channel] += sample;
                                    counts[channel] += 1;
                                }
                            }
                        }
                        let mut means_for_cell = [0.0; 3];
                        for channel in 0..3 {
                            if counts[channel] != 0 {
                                means_for_cell[channel] = sums[channel] / counts[channel] as f32;
                                flags |= 1 << channel;
                            }
                        }
                        mean_row[cell_x] = means_for_cell;
                        flag_row[cell_x] = flags;
                    }
                    Ok(())
                },
            )?;

        Ok(Self {
            width,
            height,
            means,
            flags,
        })
    }

    #[must_use]
    pub(crate) fn get(&self, x: usize, y: usize) -> CellSummary {
        let index = y * self.width + x;
        CellSummary {
            means: self.means[index],
            flags: self.flags[index],
        }
    }
}

#[must_use]
pub(crate) const fn ceil_halved(value: usize) -> usize {
    value / 2 + value % 2
}

fn allocation_error(elements: usize) -> HighlightError {
    HighlightError::Image(rohditor_image::ImageError::Allocation { elements })
}
