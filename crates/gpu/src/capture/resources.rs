use super::*;

pub(super) const DEFAULT_BUDGET: u64 = 768 * 1024 * 1024;
const OVERHEAD: u64 = 64 * 1024;

pub(super) struct TilePlan {
    pub edge: usize,
    pub pixels: usize,
    pub gpu_bytes: u64,
    pub host_bytes: usize,
}

impl TilePlan {
    pub fn new(
        width: usize,
        height: usize,
        halo: usize,
        budget: u64,
        limits: &wgpu::Limits,
        maximum: usize,
    ) -> Result<Self, GpuPreviewError> {
        Self::new_with_cost(width, height, halo, budget, limits, maximum, 60, 24)
    }

    pub(crate) fn new_resident(
        width: usize,
        height: usize,
        halo: usize,
        budget: u64,
        limits: &wgpu::Limits,
        maximum: usize,
    ) -> Result<Self, GpuPreviewError> {
        // RGB input/output, six working planes, and the queue's upload staging.
        Self::new_with_cost(width, height, halo, budget, limits, maximum, 48, 12)
    }

    #[allow(clippy::too_many_arguments)]
    fn new_with_cost(
        width: usize,
        height: usize,
        halo: usize,
        budget: u64,
        limits: &wgpu::Limits,
        maximum: usize,
        gpu_bytes_per_pixel: u64,
        host_bytes_per_pixel: usize,
    ) -> Result<Self, GpuPreviewError> {
        if width == 0
            || height == 0
            || width > i32::MAX as usize / 2
            || height > i32::MAX as usize / 2
        {
            return Err(error("invalid capture dimensions"));
        }
        let mut edge = maximum.min(512);
        while edge > 0 {
            let w = width.min(edge + 2 * halo);
            let h = height.min(edge + 2 * halo);
            let pixels = w
                .checked_mul(h)
                .ok_or_else(|| error("capture tile size overflow"))?;
            // Six planes, RGB input/output, upload staging, readback staging.
            let gpu_bytes = (pixels as u64)
                .checked_mul(gpu_bytes_per_pixel)
                .and_then(|n| n.checked_add(OVERHEAD))
                .ok_or_else(|| error("capture allocation overflow"))?;
            let binding = pixels as u64 * 24;
            if gpu_bytes <= budget
                && binding <= limits.max_buffer_size
                && binding <= u64::from(limits.max_storage_buffer_binding_size)
                && w.div_ceil(8).max(h.div_ceil(8))
                    <= limits.max_compute_workgroups_per_dimension as usize
                && limits.max_storage_buffers_per_shader_stage >= 3
                && limits.max_uniform_buffers_per_shader_stage >= 1
                && limits.max_uniform_buffer_binding_size >= 64
                && limits.max_bindings_per_bind_group >= 4
                && limits.max_bind_groups >= 1
                && limits.max_compute_invocations_per_workgroup >= 64
                && limits.max_compute_workgroup_size_x >= 8
                && limits.max_compute_workgroup_size_y >= 8
            {
                return Ok(Self {
                    edge,
                    pixels,
                    gpu_bytes,
                    host_bytes: pixels * host_bytes_per_pixel,
                });
            }
            edge /= 2;
        }
        Err(GpuPreviewError::Unsupported { reason: "capture cannot fit a minimum tile and its iterative halo within the remaining budget/device limits".into() })
    }
}

pub(super) struct Resources {
    pub rgb: wgpu::Buffer,
    pub planes: wgpu::Buffer,
    pub weights: wgpu::Buffer,
    pub staging: Option<wgpu::Buffer>,
}

impl Resources {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pixels: usize,
        contract: &CaptureSharpeningContract,
    ) -> Self {
        Self::with_readback(device, queue, pixels, contract, true)
    }

    pub(crate) fn new_resident(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pixels: usize,
        contract: &CaptureSharpeningContract,
    ) -> Self {
        Self::with_readback(device, queue, pixels, contract, false)
    }

    fn with_readback(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pixels: usize,
        contract: &CaptureSharpeningContract,
        readback: bool,
    ) -> Self {
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let weights = buffer(
            "capture CPU Gaussian",
            std::mem::size_of_val(contract.weights()) as u64,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        queue.write_buffer(&weights, 0, bytemuck::cast_slice(contract.weights()));
        Self {
            rgb: buffer(
                "capture RGB",
                pixels as u64 * 12,
                wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
            ),
            planes: buffer(
                "capture six f32 planes",
                pixels as u64 * 24,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            ),
            weights,
            staging: readback.then(|| {
                buffer(
                    "capture readback",
                    pixels as u64 * 12,
                    wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                )
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budgets_and_limits_bound_tiles() {
        let limits = wgpu::Limits::default();
        let full =
            TilePlan::new(6000, 4000, 64, DEFAULT_BUDGET, &limits, 512).expect("capture fixture");
        assert_eq!(full.edge, 512);
        assert!(full.gpu_bytes < 26 * 1024 * 1024);
        let small =
            TilePlan::new(6000, 4000, 64, 2 * 1024 * 1024, &limits, 512).expect("capture fixture");
        assert!(small.edge < full.edge);
        assert!(TilePlan::new(6000, 4000, 64, 64 * 1024, &limits, 512).is_err());
        assert!(TilePlan::new(usize::MAX, 2, 64, DEFAULT_BUDGET, &limits, 512).is_err());
        let mut limits = limits;
        limits.max_storage_buffer_binding_size = 4;
        assert!(TilePlan::new(100, 100, 64, DEFAULT_BUDGET, &limits, 512).is_err());
    }
}
