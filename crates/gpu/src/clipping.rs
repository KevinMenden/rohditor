//! The clipping overlay: a tint over the clipped pixels of a display texture.
//!
//! Runs on the texture that is shown, so the overlay always belongs to the
//! displayed frame and nothing is read back.

use wgpu::util::DeviceExt as _;

/// Straight-alpha RGBA colours of the overlay.
#[derive(Debug, Clone, Copy)]
pub struct ClippingTints {
    /// Pixels with a channel clipped to white. Wins when a pixel is clipped
    /// at both ends, for example pure red.
    pub highlight: [f32; 4],
    /// Pixels with a channel clipped to black.
    pub shadow: [f32; 4],
}

/// Renders clipping overlays on the frontend's device.
pub struct GpuClippingOverlay {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    tints: wgpu::Buffer,
    /// Reused while the size of the display stays the same.
    target: Option<wgpu::Texture>,
}

impl GpuClippingOverlay {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, tints: ClippingTints) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("clipping overlay shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("clipping.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("clipping overlay"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let tints = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("clipping overlay tints"),
            contents: bytemuck::cast_slice(&[tints.highlight, tints.shadow]),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        Self {
            device: device.clone(),
            queue: queue.clone(),
            pipeline,
            tints,
            target: None,
        }
    }

    /// The overlay of `display`, a view of an 8-bit texture of this size:
    /// transparent where nothing is clipped. Earlier results of the same size
    /// are overwritten.
    pub fn render(
        &mut self,
        display: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) -> wgpu::TextureView {
        let target = self.render_texture(display, width, height);
        target.create_view(&wgpu::TextureViewDescriptor::default())
    }

    fn render_texture(
        &mut self,
        display: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) -> wgpu::Texture {
        let target = self
            .target
            .take()
            .filter(|target| (target.width(), target.height()) == (width, height))
            .unwrap_or_else(|| self.create_target(width, height));
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("clipping overlay"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(display),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.tints.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bindings, &[]);
            pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
        }
        self.queue.submit([encoder.finish()]);
        self.target.insert(target).clone()
    }

    fn create_target(&self, width: u32, height: u32) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("clipping overlay"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }
}

#[cfg(test)]
mod tests {
    use rohditor_core::ClippingMask;

    use super::*;

    const TINTS: ClippingTints = ClippingTints {
        highlight: [1.0, 0.0, 0.0, 1.0],
        shadow: [0.0, 0.0, 1.0, 1.0],
    };

    fn device() -> (wgpu::Device, wgpu::Queue) {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .expect("Vulkan adapter");
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("device")
    }

    fn display(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        rgba: &[u8],
    ) -> wgpu::Texture {
        let height = rgba.len() as u32 / 4 / width;
        device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            rgba,
        )
    }

    fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
        let row = (texture.width() * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(row * texture.height()),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |result| {
            result.expect("buffer is mapped");
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device is polled");
        let mapped = buffer.slice(..).get_mapped_range();
        mapped
            .chunks(row as usize)
            .flat_map(|line| line[..texture.width() as usize * 4].to_vec())
            .collect()
    }

    #[test]
    #[ignore = "requires a Vulkan adapter"]
    fn tints_exactly_the_pixels_of_the_core_mask() {
        let (device, queue) = device();
        let mut overlay = GpuClippingOverlay::new(&device, &queue, TINTS);
        // The levels around both ends in every channel, in sizes that do not fill workgroups.
        let levels = [0, 1, 2, 128, 253, 254, 255];
        for width in [49_u32, 7] {
            let rgba: Vec<u8> = (0..343)
                .flat_map(|i| [levels[i % 7], levels[i / 7 % 7], levels[i / 49], 255])
                .collect();
            let height = 343 / width;
            let source = display(&device, &queue, width, &rgba);
            let view = source.create_view(&wgpu::TextureViewDescriptor::default());
            let target = overlay.render_texture(&view, width, height);
            let mask =
                ClippingMask::from_rgba8(width as usize, height as usize, &rgba).expect("mask");
            let expected: Vec<u8> = mask
                .flags()
                .iter()
                .flat_map(|flags| {
                    if flags.highlight() {
                        [255, 0, 0, 255]
                    } else if flags.shadow() {
                        [0, 0, 255, 255]
                    } else {
                        [0; 4]
                    }
                })
                .collect();
            assert_eq!(read_back(&device, &queue, &target), expected, "{width}");
        }
    }
}
