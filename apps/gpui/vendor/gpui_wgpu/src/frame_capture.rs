//! ROHDITOR: capture one frame of a real window as an image, for the app's `--screenshot`.
//! Upstream can only capture headless windows, and only in tests. The readback is shared with
//! the headless renderer.

use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

type Capture = anyhow::Result<image::RgbaImage>;

thread_local! {
    // GPUI draws its windows on the main thread, where the app asks for the capture.
    static PENDING: RefCell<Option<Sender<Capture>>> = const { RefCell::new(None) };
}

/// Capture the next frame a window draws. The caller must make a window draw (for example with
/// `Window::refresh`); the image arrives once it has.
pub fn capture_next_frame() -> Receiver<Capture> {
    let (sender, receiver) = mpsc::channel();
    PENDING.with(|pending| *pending.borrow_mut() = Some(sender));
    receiver
}

/// The waiting request, if any; the next drawn frame answers it.
pub(crate) fn take_request() -> Option<Sender<Capture>> {
    PENDING.with(|pending| pending.borrow_mut().take())
}

/// Copy a rendered 8-bit RGBA or BGRA `texture` (with `COPY_SRC` usage) back to the CPU. Waits for
/// the GPU.
pub(crate) fn read_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Capture {
    let bgra = match texture.format() {
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => true,
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => false,
        other => anyhow::bail!("cannot read back a {other:?} texture"),
    };
    let width = texture.width();
    let height = texture.height();
    let bytes_per_row = width
        .checked_mul(4)
        .ok_or_else(|| anyhow::anyhow!("readback row size overflowed"))?;
    let padded_bytes_per_row = bytes_per_row
        .checked_next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        .ok_or_else(|| anyhow::anyhow!("readback padded row size overflowed"))?;
    let buffer_size = u64::from(padded_bytes_per_row)
        .checked_mul(u64::from(height))
        .ok_or_else(|| anyhow::anyhow!("readback buffer size overflowed"))?;
    anyhow::ensure!(
        buffer_size <= device.limits().max_buffer_size,
        "readback buffer size {buffer_size} exceeds maximum buffer size {}",
        device.limits().max_buffer_size
    );
    let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback_buffer"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("readback_encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback_buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let submission = queue.submit(std::iter::once(encoder.finish()));
    let (sender, receiver) = mpsc::channel();
    readback_buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            if sender.send(result).is_err() {
                log::error!("readback receiver was dropped before mapping completed");
            }
        });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(Duration::from_secs(30)),
        })
        .map_err(|error| anyhow::anyhow!("failed to wait for the readback: {error}"))?;
    receiver
        .recv_timeout(Duration::from_secs(30))
        .map_err(|error| anyhow::anyhow!("failed to receive the mapping result: {error}"))?
        .map_err(|error| anyhow::anyhow!("failed to map the readback buffer: {error}"))?;

    let mapped_data = readback_buffer.slice(..).get_mapped_range();
    let pixel_capacity = usize::try_from(u64::from(bytes_per_row) * u64::from(height))
        .map_err(|_| anyhow::anyhow!("readback image size exceeds addressable memory"))?;
    let mut pixels = Vec::with_capacity(pixel_capacity);
    for row in mapped_data
        .chunks_exact(padded_bytes_per_row as usize)
        .take(height as usize)
    {
        pixels.extend_from_slice(&row[..bytes_per_row as usize]);
    }
    drop(mapped_data);
    readback_buffer.unmap();

    if bgra {
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
    }
    image::RgbaImage::from_raw(width, height, pixels)
        .ok_or_else(|| anyhow::anyhow!("failed to create an image from the readback"))
}
