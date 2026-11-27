//! The histogram of a display texture, counted on the GPU.
//!
//! Only the bins come back (4 KB), not the pixels. The counts equal
//! [`Histogram::from_rgba8`] on the same pixels, so Auto Tone sees the same
//! input from the GPU and the CPU preview.

use std::sync::mpsc;

use rohditor_core::Histogram;

use crate::GpuPreviewError;

const BINS: usize = 256;
/// Red, green, blue and luminance, one after another.
const CHANNELS: usize = 4;
const BUFFER_BYTES: u64 = (CHANNELS * BINS * size_of::<u32>()) as u64;
/// Pixels a workgroup counts along each axis (`histogram.wgsl`).
const PIXELS_PER_GROUP: u32 = 64;

/// Counts histograms of display textures on the frontend's device.
pub(crate) struct GpuHistogramCounter {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
}

impl GpuHistogramCounter {
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("histogram shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("histogram.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("histogram"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            device: device.clone(),
            queue: queue.clone(),
            pipeline,
        }
    }

    /// Submit the count of `display`, a view of an 8-bit unorm texture of this
    /// size, without waiting for it.
    pub(crate) fn begin(
        &self,
        display: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) -> GpuHistogramPending {
        let buffer = |label, usage| {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: BUFFER_BYTES,
                usage,
                mapped_at_creation: false,
            })
        };
        // Buffers start at zero.
        let bins = buffer(
            "histogram bins",
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );
        let readback = buffer(
            "histogram readback",
            wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        );
        let bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("histogram"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(display),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: bins.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("histogram"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bindings, &[]);
            pass.dispatch_workgroups(
                width.div_ceil(PIXELS_PER_GROUP),
                height.div_ceil(PIXELS_PER_GROUP),
                1,
            );
        }
        encoder.copy_buffer_to_buffer(&bins, 0, &readback, 0, BUFFER_BYTES);
        self.queue.submit([encoder.finish()]);
        let (sender, ready) = mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                drop(sender.send(result));
            });
        GpuHistogramPending {
            buffer: readback,
            ready,
        }
    }
}

/// A histogram on its way back from the GPU. Dropping it does not block.
pub struct GpuHistogramPending {
    buffer: wgpu::Buffer,
    ready: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
}

impl GpuHistogramPending {
    /// Whether the GPU has finished, so that [`Self::finish`] can follow. Must
    /// not be called again once it returned `true`.
    pub fn poll_ready(&mut self) -> Result<bool, GpuPreviewError> {
        let result = match self.ready.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return Ok(false),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(GpuPreviewError::Readback {
                    reason: "histogram completion channel closed unexpectedly".to_owned(),
                });
            }
        };
        result.map_err(|error| GpuPreviewError::Readback {
            reason: error.to_string(),
        })?;
        Ok(true)
    }

    /// The histogram, after [`Self::poll_ready`] returned `true`.
    #[must_use]
    pub fn finish(self) -> Histogram {
        let mapped = self.buffer.slice(..).get_mapped_range();
        let counts: &[u32] = bytemuck::cast_slice(&mapped);
        let channel = |index: usize| -> [u64; BINS] {
            std::array::from_fn(|bin| u64::from(counts[index * BINS + bin]))
        };
        let histogram = Histogram::from_bins(channel(0), channel(1), channel(2), channel(3));
        drop(mapped);
        self.buffer.unmap();
        histogram
    }
}

#[cfg(test)]
mod tests {
    use wgpu::util::DeviceExt as _;

    use super::*;

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

    fn count(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        counter: &GpuHistogramCounter,
        width: u32,
        rgba: &[u8],
    ) -> Histogram {
        let height = rgba.len() as u32 / 4 / width;
        let texture = device.create_texture_with_data(
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
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut pending = counter.begin(&view, width, height);
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device is polled");
        assert!(pending.poll_ready().expect("bins are mapped"));
        pending.finish()
    }

    #[test]
    #[ignore = "requires a Vulkan adapter"]
    fn counts_exactly_like_the_cpu() {
        let (device, queue) = device();
        let counter = GpuHistogramCounter::new(&device, &queue);
        // Every level in every channel, uncorrelated between channels. 131x75 does not fill
        // workgroups in either direction, and its rows of 524 bytes need padding.
        let (width, height) = (131_u32, 75_u32);
        let mut state = 0x2545_f491_u32;
        let rgba: Vec<u8> = (0..width * height * 4)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect();
        let expected =
            Histogram::from_rgba8(width as usize, height as usize, &rgba).expect("valid buffer");
        assert_eq!(count(&device, &queue, &counter, width, &rgba), expected);
    }

    #[test]
    #[ignore = "requires a Vulkan adapter"]
    fn counts_a_flat_image_with_clipped_channels() {
        let (device, queue) = device();
        let counter = GpuHistogramCounter::new(&device, &queue);
        let (width, height) = (200_u32, 3_u32);
        let rgba = [255, 0, 37, 9].repeat((width * height) as usize);
        let histogram = count(&device, &queue, &counter, width, &rgba);
        assert_eq!(
            histogram,
            Histogram::from_rgba8(width as usize, height as usize, &rgba).expect("valid buffer")
        );
        assert_eq!(histogram.highlight_clipped, [600, 0, 0]);
        assert_eq!(histogram.shadow_clipped, [0, 600, 0]);
    }
}
