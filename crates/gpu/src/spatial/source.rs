use std::sync::Arc;

use rohditor_core::{
    CancellationToken, CaptureSharpeningContract, DemosaicedCameraSource,
    SpatialCompletionDescription,
};

use super::resources::ResidentLayout;
use crate::{GpuPreviewError, memory::Reservation};

pub(crate) struct ResidentCameraPlanes {
    _memory: Reservation,
    pub(crate) textures: [wgpu::Texture; 3],
    views: [wgpu::TextureView; 3],
    pub layout: ResidentLayout,
}

impl ResidentCameraPlanes {
    pub fn new(device: &wgpu::Device, layout: ResidentLayout) -> Result<Self, GpuPreviewError> {
        Self::with_budget(device, layout, super::resources::DEFAULT_BUDGET)
    }

    pub(crate) fn with_budget(
        device: &wgpu::Device,
        layout: ResidentLayout,
        budget: u64,
    ) -> Result<Self, GpuPreviewError> {
        // Reserve before touching the driver. This includes display frames
        // retained by the UI while the worker prepares a replacement.
        let memory = Reservation::try_new(layout.resident_bytes, budget)?;
        let texture = |label| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: layout.tile_width,
                    height: layout.tile_height,
                    depth_or_array_layers: layout.layers,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R32Float,
                usage: wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | if cfg!(test) {
                        wgpu::TextureUsages::COPY_SRC
                    } else {
                        wgpu::TextureUsages::empty()
                    },
                view_formats: &[],
            })
        };
        let textures = [
            texture("resident camera red planes"),
            texture("resident camera green planes"),
            texture("resident camera blue planes"),
        ];
        let view = |texture: &wgpu::Texture, label| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some(label),
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                base_array_layer: 0,
                array_layer_count: Some(layout.layers),
                ..Default::default()
            })
        };
        let views = [
            view(&textures[0], "resident camera red view"),
            view(&textures[1], "resident camera green view"),
            view(&textures[2], "resident camera blue view"),
        ];
        Ok(Self {
            _memory: memory,
            textures,
            views,
            layout,
        })
    }

    pub fn sampled_entries(&self) -> [wgpu::BindGroupEntry<'_>; 3] {
        [
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&self.views[0]),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&self.views[1]),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&self.views[2]),
            },
        ]
    }

    pub fn storage_views(&self) -> [&wgpu::TextureView; 3] {
        [&self.views[0], &self.views[1], &self.views[2]]
    }

    pub fn upload(
        &self,
        queue: &wgpu::Queue,
        source: &DemosaicedCameraSource,
        cancellation: &CancellationToken,
        mut drain_upload: impl FnMut() -> Result<(), GpuPreviewError>,
    ) -> Result<u64, GpuPreviewError> {
        let image = source.image();
        let mut packed = Vec::<f32>::new();
        let mut uploaded_bytes = 0_u64;
        for tile_y in 0..self.layout.rows {
            let top = tile_y * self.layout.tile_height;
            let height = self.layout.tile_height.min(self.layout.height - top);
            for tile_x in 0..self.layout.columns {
                let left = tile_x * self.layout.tile_width;
                let width = self.layout.tile_width.min(self.layout.width - left);
                let layer = tile_y * self.layout.columns + tile_x;
                let elements = usize::try_from(width)
                    .ok()
                    .and_then(|width| {
                        usize::try_from(height)
                            .ok()
                            .and_then(|height| width.checked_mul(height))
                    })
                    .ok_or_else(|| invalid("resident upload tile size overflowed"))?;
                packed
                    .try_reserve_exact(elements)
                    .map_err(|_| invalid("resident upload allocation failed"))?;
                for channel in 0..3 {
                    check_cancel(cancellation)?;
                    packed.clear();
                    for y in top..top + height {
                        let row = &image.data()[y as usize * image.row_stride()..];
                        for x in left..left + width {
                            let value = row[x as usize * 3 + channel];
                            if !value.is_finite() {
                                return Err(invalid("camera source contains non-finite samples"));
                            }
                            packed.push(value);
                        }
                    }
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &self.textures[channel],
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: 0,
                                y: 0,
                                z: layer,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        bytemuck::cast_slice(&packed),
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(width * 4),
                            rows_per_image: Some(height),
                        },
                        wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                    );
                    // Keep at most one plane's staging allocation in flight.
                    // Drain even on cancellation before releasing its backing.
                    queue.submit([]);
                    drain_upload()?;
                    uploaded_bytes += u64::from(width) * u64::from(height) * 4;
                }
            }
        }
        check_cancel(cancellation)?;
        Ok(uploaded_bytes)
    }
}

/// Complete camera-native GPU source after the optional capture stage.
pub struct GpuCapturedSource {
    pub(super) planes: Arc<ResidentCameraPlanes>,
    pub(super) description: SpatialCompletionDescription,
    pub(super) capture_contract: CaptureSharpeningContract,
    pub(super) uploaded_bytes: u64,
}

impl GpuCapturedSource {
    #[must_use]
    pub fn dimensions(&self) -> (usize, usize) {
        self.description.source_dimensions()
    }

    #[must_use]
    pub fn estimated_bytes(&self) -> u64 {
        self.planes.layout.resident_bytes
    }

    #[must_use]
    pub const fn uploaded_bytes(&self) -> u64 {
        self.uploaded_bytes
    }

    pub(super) fn matches(
        &self,
        description: &SpatialCompletionDescription,
        capture_contract: &CaptureSharpeningContract,
    ) -> bool {
        self.description.shares_camera_source(description)
            && self.description.capture_sharpening() == description.capture_sharpening()
            && self.description.source_dimensions() == description.source_dimensions()
            && self.capture_contract.settings() == capture_contract.settings()
            && self.capture_contract.ceilings() == capture_contract.ceilings()
            && self.description.highlight_adjustments() == description.highlight_adjustments()
            && self.description.camera_profile_key() == description.camera_profile_key()
            && self.description.calibration() == description.calibration()
            && (description.highlight_adjustments().method != rohditor_edit::HighlightMethod::Clip
                || self.description.highlight_white_balance()
                    == description.highlight_white_balance())
    }
}

/// Reduced camera-native RGBA32Float source for the existing color processor.
pub struct GpuSpatialPreview {
    pub(crate) _memory: Reservation,
    pub(crate) texture: wgpu::Texture,
    pub(crate) view: wgpu::TextureView,
    pub(crate) description: SpatialCompletionDescription,
}

/// Full-resolution resident camera source evaluated directly into a display
/// or export output without a full RGBA working texture.
#[derive(Clone)]
pub struct GpuSpatialFullSource {
    pub(crate) planes: Arc<ResidentCameraPlanes>,
    pub(crate) description: SpatialCompletionDescription,
}

impl GpuSpatialFullSource {
    #[must_use]
    pub fn dimensions(&self) -> (usize, usize) {
        self.description.source_dimensions()
    }

    #[must_use]
    pub fn estimated_bytes(&self) -> u64 {
        self.planes.layout.resident_bytes
    }

    #[must_use]
    pub const fn description(&self) -> &SpatialCompletionDescription {
        &self.description
    }

    #[must_use]
    pub fn optics_provenance(&self) -> Option<&rohditor_core::OpticsProvenance> {
        self.description.optics().provenance()
    }

    #[must_use]
    pub fn optics_matches_recipe(&self, adjustments: &rohditor_edit::OpticsAdjustments) -> bool {
        crate::preview::processor::optics_provenance_matches(self.optics_provenance(), adjustments)
    }
}

impl std::fmt::Debug for GpuSpatialFullSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GpuSpatialFullSource")
            .field("dimensions", &self.dimensions())
            .field("optics", &self.optics_provenance())
            .finish_non_exhaustive()
    }
}

impl GpuSpatialPreview {
    #[must_use]
    pub fn dimensions(&self) -> (usize, usize) {
        self.description.target_dimensions()
    }

    #[must_use]
    pub fn optics_provenance(&self) -> Option<&rohditor_core::OpticsProvenance> {
        self.description.optics().provenance()
    }

    #[must_use]
    pub const fn description(&self) -> &SpatialCompletionDescription {
        &self.description
    }
}

impl std::fmt::Debug for GpuSpatialPreview {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GpuSpatialPreview")
            .field("dimensions", &self.dimensions())
            .field("optics", &self.optics_provenance())
            .finish_non_exhaustive()
    }
}

fn invalid(reason: &str) -> GpuPreviewError {
    GpuPreviewError::InvalidInput {
        reason: reason.to_owned(),
    }
}

fn check_cancel(token: &CancellationToken) -> Result<(), GpuPreviewError> {
    if token.is_cancelled() {
        Err(GpuPreviewError::Cancelled)
    } else {
        Ok(())
    }
}
