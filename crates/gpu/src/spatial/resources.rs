use crate::GpuPreviewError;

pub(crate) const DEFAULT_BUDGET: u64 = 768 * 1024 * 1024;
const RESOURCE_OVERHEAD: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResidentLayout {
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

impl ResidentLayout {
    pub(crate) fn new_bounded(
        width: usize,
        height: usize,
        retained_bytes: u64,
        work_bytes: u64,
        budget: u64,
        limits: &wgpu::Limits,
        maximum_tile_edge: u32,
    ) -> Result<Self, GpuPreviewError> {
        let width_u32 =
            u32::try_from(width).map_err(|_| dimensions(width, height, "width exceeds u32"))?;
        let height_u32 =
            u32::try_from(height).map_err(|_| dimensions(width, height, "height exceeds u32"))?;
        if width_u32 == 0 || height_u32 == 0 {
            return Err(dimensions(width, height, "camera source is empty"));
        }
        if limits.max_texture_array_layers == 0
            || limits.max_sampled_textures_per_shader_stage < 3
            || limits.max_storage_textures_per_shader_stage < 3
            || limits.max_bindings_per_bind_group < 5
        {
            return Err(GpuPreviewError::Unsupported {
                reason:
                    "device limits do not support three sampled/write-only R32Float plane arrays"
                        .into(),
            });
        }

        let max_layers = limits.max_texture_array_layers;
        let max_edge = limits
            .max_texture_dimension_2d
            .min(maximum_tile_edge.max(1));
        let mut best: Option<Self> = None;
        for columns in 1..=max_layers {
            let tile_width = width_u32.div_ceil(columns);
            if tile_width == 0 || tile_width > max_edge {
                continue;
            }
            let maximum_rows = max_layers / columns;
            for rows in 1..=maximum_rows {
                let tile_height = height_u32.div_ceil(rows);
                if tile_height == 0 || tile_height > max_edge {
                    continue;
                }
                let layers = columns * rows;
                let Some(padded_pixels) = u64::from(tile_width)
                    .checked_mul(u64::from(tile_height))
                    .and_then(|value| value.checked_mul(u64::from(layers)))
                else {
                    continue;
                };
                let Some(resident_bytes) = padded_pixels.checked_mul(12) else {
                    continue;
                };
                let upload_staging = u64::from(tile_width) * u64::from(tile_height) * 4;
                let Some(estimated_peak_bytes) = resident_bytes
                    .checked_add(upload_staging)
                    .and_then(|value| value.checked_add(work_bytes))
                    .and_then(|value| value.checked_add(retained_bytes))
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
                if best.is_none_or(|current| {
                    (candidate.padded_pixels, candidate.layers)
                        < (current.padded_pixels, current.layers)
                }) {
                    best = Some(candidate);
                }
            }
        }
        best.ok_or_else(|| GpuPreviewError::Unsupported {
            reason: format!(
                "camera source {width}x{height} cannot fit a tiled planar R32Float backing and minimum work unit within the {budget}-byte budget/device limits"
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
    fn layout_minimizes_padding_and_rejects_constrained_budgets() {
        let limits = wgpu::Limits::default();
        let layout = ResidentLayout::new_bounded(
            6000,
            4000,
            0,
            24 * 1024 * 1024,
            DEFAULT_BUDGET,
            &limits,
            limits.max_texture_dimension_2d,
        )
        .expect("24 MP resident source");
        assert!(layout.layers <= limits.max_texture_array_layers);
        assert!(layout.tile_width <= limits.max_texture_dimension_2d);
        assert!(layout.tile_height <= limits.max_texture_dimension_2d);
        assert!(layout.padded_pixels >= 24_000_000);
        assert!(layout.padded_pixels < 24_200_000);
        assert!(ResidentLayout::new_bounded(6000, 4000, 0, 0, 1024, &limits, 1024).is_err());
        assert!(
            ResidentLayout::new_bounded(
                usize::MAX,
                4,
                0,
                0,
                DEFAULT_BUDGET,
                &limits,
                limits.max_texture_dimension_2d,
            )
            .is_err()
        );
    }
}
