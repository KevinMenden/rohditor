//! Reading regions of resident camera planes into a linear RGB buffer.
use crate::spatial::source::ResidentCameraPlanes;

/// Copies a region of resident camera planes into an interleaved RGB
/// storage buffer, the layout every capture pass works on.
pub(super) struct ResidentLoader {
    pipeline: wgpu::ComputePipeline,
}

impl ResidentLoader {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("resident camera capture input"),
            source: wgpu::ShaderSource::Wgsl(include_str!("resident_load.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("load resident capture tile"),
            layout: None,
            module: &shader,
            entry_point: Some("load_resident"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self { pipeline }
    }

    /// Record the copy of the `region` (`x, y, width, height`) of `source`
    /// into the start of `buffer`.
    pub fn encode(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &ResidentCameraPlanes,
        buffer: &wgpu::Buffer,
        region: [usize; 4],
    ) {
        let layout = source.layout;
        let [x, y, width, height] = region;
        let words = [
            layout.tile_width,
            layout.tile_height,
            layout.columns,
            0,
            x as u32,
            y as u32,
            width as u32,
            height as u32,
        ];
        let uniform = crate::memory::initialized_buffer(
            device,
            queue,
            &wgpu::util::BufferInitDescriptor {
                label: Some("resident capture region"),
                contents: bytemuck::cast_slice(&words),
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        let mut entries = source.sampled_entries().to_vec();
        entries.push(wgpu::BindGroupEntry {
            binding: 3,
            resource: buffer.as_entire_binding(),
        });
        entries.push(wgpu::BindGroupEntry {
            binding: 4,
            resource: uniform.as_entire_binding(),
        });
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("resident capture input bindings"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &entries,
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("load resident capture input"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bindings, &[]);
        pass.dispatch_workgroups((width as u32).div_ceil(8), (height as u32).div_ceil(8), 1);
    }
}
