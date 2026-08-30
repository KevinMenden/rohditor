use crate::GpuPreviewError;

/// Facts about the shared eframe `wgpu` adapter and device that matter for
/// Rohditor's downstream preview processor.
#[derive(Debug, Clone)]
pub struct GpuCapabilities {
    /// Human-readable adapter name reported by wgpu.
    pub adapter_name: String,
    /// Graphics backend selected by wgpu, for example `Vulkan`.
    pub backend: String,
    /// Adapter class, for example `DiscreteGpu` or `Cpu`.
    pub device_type: String,
    /// Driver name reported by wgpu.
    pub driver: String,
    /// Driver information reported by wgpu.
    pub driver_info: String,
    /// eframe's surface target format. It is recorded for diagnostics; the
    /// preview itself uses an egui-compatible `Rgba8Unorm` texture.
    pub target_format: String,
    /// Maximum usable two-dimensional texture edge on the shared device.
    pub max_texture_dimension_2d: u32,
    /// Maximum compute-workgroup count along one dimension.
    pub max_compute_workgroups_per_dimension: u32,
    /// Whether the shared device permits the `Rgba16Float` source upload.
    pub rgba16float_sampled: bool,
    /// Whether the shared device permits the `Rgba16Float` working target.
    pub rgba16float_storage: bool,
    /// Whether the shared device permits the egui-compatible `Rgba8Unorm`
    /// display target.
    pub rgba8unorm_storage: bool,
    /// Whether the shared device was created with timestamp-query support.
    pub timestamp_queries: bool,
    is_cpu_adapter: bool,
}

impl GpuCapabilities {
    /// Inspect the actual adapter and the already-created shared device.
    #[must_use]
    pub fn detect(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let info = adapter.get_info();
        let rgba16float = adapter.get_texture_format_features(wgpu::TextureFormat::Rgba16Float);
        let rgba8unorm = adapter.get_texture_format_features(wgpu::TextureFormat::Rgba8Unorm);
        let limits = device.limits();
        let rgba16float_sampled = rgba16float
            .allowed_usages
            .contains(wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING);
        let rgba16float_storage = rgba16float
            .allowed_usages
            .contains(wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING);
        let rgba8unorm_storage = rgba8unorm.allowed_usages.contains(
            wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
        );

        Self {
            adapter_name: info.name,
            backend: format!("{:?}", info.backend),
            device_type: format!("{:?}", info.device_type),
            driver: info.driver,
            driver_info: info.driver_info,
            target_format: format!("{target_format:?}"),
            max_texture_dimension_2d: limits.max_texture_dimension_2d,
            max_compute_workgroups_per_dimension: limits.max_compute_workgroups_per_dimension,
            rgba16float_sampled,
            rgba16float_storage,
            rgba8unorm_storage,
            timestamp_queries: device.features().contains(wgpu::Features::TIMESTAMP_QUERY),
            is_cpu_adapter: matches!(info.device_type, wgpu::DeviceType::Cpu),
        }
    }

    /// Return whether this is a hardware adapter suitable for automatic GPU
    /// processing. A CPU rasterizer may still draw an eframe window, but is not
    /// a useful replacement for the CPU reference processor.
    #[must_use]
    pub const fn is_hardware_adapter(&self) -> bool {
        !self.is_cpu_adapter
    }

    /// Validate the texture and binding support needed before creating pipeline
    /// resources.
    pub fn validate_preview_support(&self) -> Result<(), GpuPreviewError> {
        let mut missing = Vec::new();
        if !self.rgba16float_sampled {
            missing.push("Rgba16Float upload/sample support");
        }
        if !self.rgba16float_storage {
            missing.push("Rgba16Float storage-texture support");
        }
        if !self.rgba8unorm_storage {
            missing.push("Rgba8Unorm storage/display-texture support");
        }
        if self.max_compute_workgroups_per_dimension == 0 {
            missing.push("compute workgroups");
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(GpuPreviewError::Unsupported {
                reason: format!("missing {}", missing.join(", ")),
            })
        }
    }

    /// Validate a concrete source or output texture size.
    pub fn validate_dimensions(
        &self,
        width: usize,
        height: usize,
    ) -> Result<(u32, u32), GpuPreviewError> {
        let width_u32 = u32::try_from(width).map_err(|_| GpuPreviewError::InvalidDimensions {
            width,
            height,
            reason: "width does not fit into wgpu's u32 texture extent".to_owned(),
        })?;
        let height_u32 = u32::try_from(height).map_err(|_| GpuPreviewError::InvalidDimensions {
            width,
            height,
            reason: "height does not fit into wgpu's u32 texture extent".to_owned(),
        })?;
        if width_u32 == 0 || height_u32 == 0 {
            return Err(GpuPreviewError::InvalidDimensions {
                width,
                height,
                reason: "textures must have non-zero dimensions".to_owned(),
            });
        }
        if width_u32 > self.max_texture_dimension_2d || height_u32 > self.max_texture_dimension_2d {
            return Err(GpuPreviewError::InvalidDimensions {
                width,
                height,
                reason: format!(
                    "the shared device limit is {} pixels per dimension",
                    self.max_texture_dimension_2d
                ),
            });
        }
        Ok((width_u32, height_u32))
    }
}
