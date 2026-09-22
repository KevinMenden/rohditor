use crate::GpuPreviewError;

pub(super) const DEFAULT_BUDGET: u64 = 768 * 1024 * 1024;
const RESOURCE_OVERHEAD: u64 = 64 * 1024;

/// Tiled backing for one crop-local R32Float Bayer mosaic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MosaicLayout {
    pub width: u32,
    pub height: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    pub columns: u32,
    pub rows: u32,
    pub layers: u32,
    pub padded_pixels: u64,
    pub resident_bytes: u64,
    pub estimated_peak_bytes: u64,
}

impl MosaicLayout {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new_bounded(
        width: usize,
        height: usize,
        level_bytes: u64,
        budget: u64,
        limits: &wgpu::Limits,
        maximum_tile_edge: u32,
    ) -> Result<Self, GpuPreviewError> {
        let width_u32 =
            u32::try_from(width).map_err(|_| dimensions(width, height, "width exceeds u32"))?;
        let height_u32 =
            u32::try_from(height).map_err(|_| dimensions(width, height, "height exceeds u32"))?;
        if width_u32 < 2 || height_u32 < 2 {
            return Err(dimensions(
                width,
                height,
                "sensor normalization requires a crop of at least 2x2 pixels",
            ));
        }
        if limits.max_texture_array_layers == 0
            || limits.max_sampled_textures_per_shader_stage < 1
            || limits.max_storage_textures_per_shader_stage < 1
            || limits.max_storage_buffers_per_shader_stage < 2
            || limits.max_uniform_buffers_per_shader_stage < 1
            || limits.max_bindings_per_bind_group < 5
            || limits.max_bind_groups == 0
            || limits.max_uniform_buffer_binding_size < 48
            || limits.max_compute_invocations_per_workgroup < 64
            || limits.max_compute_workgroup_size_x < 8
            || limits.max_compute_workgroup_size_y < 8
        {
            return Err(GpuPreviewError::Unsupported {
                reason: "device limits do not support integer RAW normalization bindings".into(),
            });
        }
        if level_bytes > limits.max_buffer_size
            || level_bytes > u64::from(limits.max_storage_buffer_binding_size)
        {
            return Err(GpuPreviewError::Unsupported {
                reason: "RAW black/white level tables exceed a storage-buffer limit".into(),
            });
        }

        let workgroup_edge = 8_u32;
        let dispatch_edge = limits
            .max_compute_workgroups_per_dimension
            .saturating_mul(workgroup_edge);
        let maximum_edge = limits
            .max_texture_dimension_2d
            .min(maximum_tile_edge.max(1))
            .min(dispatch_edge);
        if maximum_edge == 0 {
            return Err(GpuPreviewError::Unsupported {
                reason: "device cannot dispatch RAW normalization workgroups".into(),
            });
        }

        let mut best = None;
        let max_layers = limits.max_texture_array_layers;
        for columns in 1..=max_layers {
            let tile_width = width_u32.div_ceil(columns);
            if tile_width == 0 || tile_width > maximum_edge {
                continue;
            }
            let maximum_rows = max_layers / columns;
            for rows in 1..=maximum_rows {
                let tile_height = height_u32.div_ceil(rows);
                if tile_height == 0 || tile_height > maximum_edge {
                    continue;
                }
                let layers = columns * rows;
                let Some(padded_pixels) = u64::from(tile_width)
                    .checked_mul(u64::from(tile_height))
                    .and_then(|value| value.checked_mul(u64::from(layers)))
                else {
                    continue;
                };
                let Some(resident_bytes) = padded_pixels.checked_mul(4) else {
                    continue;
                };
                let Some(input_bytes) = u64::from(tile_width)
                    .checked_mul(u64::from(tile_height))
                    .and_then(|value| value.checked_mul(2))
                else {
                    continue;
                };
                // `Queue::write_texture` owns an in-flight copy until the
                // submission completes. Account for it separately from the
                // reusable R16Uint tile texture rather than assuming a driver
                // can alias the two allocations.
                let upload_staging_bytes = input_bytes;
                let Some(estimated_peak_bytes) = resident_bytes
                    .checked_add(input_bytes)
                    .and_then(|value| value.checked_add(upload_staging_bytes))
                    .and_then(|value| value.checked_add(level_bytes))
                    .and_then(|value| value.checked_add(RESOURCE_OVERHEAD))
                else {
                    continue;
                };
                if estimated_peak_bytes > budget {
                    continue;
                }
                let candidate = Self {
                    width: width_u32,
                    height: height_u32,
                    tile_width,
                    tile_height,
                    columns,
                    rows,
                    layers,
                    padded_pixels,
                    resident_bytes,
                    estimated_peak_bytes,
                };
                if best.is_none_or(|current: Self| {
                    (candidate.padded_pixels, candidate.layers)
                        < (current.padded_pixels, current.layers)
                }) {
                    best = Some(candidate);
                }
            }
        }
        best.ok_or_else(|| GpuPreviewError::Unsupported {
            reason: format!(
                "normalized RAW mosaic {width}x{height} cannot fit a bounded R32Float backing and u16 upload tile within the {budget}-byte GPU budget/device limits"
            ),
        })
    }
}

fn dimensions(width: usize, height: usize, reason: &str) -> GpuPreviewError {
    GpuPreviewError::InvalidDimensions {
        width,
        height,
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_tiles_and_accounts_for_one_u16_upload() {
        let limits = wgpu::Limits::default();
        let layout = MosaicLayout::new_bounded(
            6000,
            4000,
            64,
            DEFAULT_BUDGET,
            &limits,
            limits.max_texture_dimension_2d,
        )
        .expect("24 MP normalized mosaic fits the default budget");
        assert!(layout.layers <= limits.max_texture_array_layers);
        assert!(layout.padded_pixels >= 24_000_000);
        assert!(layout.estimated_peak_bytes > layout.resident_bytes);
        assert!(MosaicLayout::new_bounded(6000, 4000, 64, 1024, &limits, 1024).is_err());
        assert!(
            MosaicLayout::new_bounded(
                usize::MAX,
                2,
                64,
                DEFAULT_BUDGET,
                &limits,
                limits.max_texture_dimension_2d,
            )
            .is_err()
        );
    }
}
