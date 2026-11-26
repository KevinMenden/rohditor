//! Uploads a CPU image to a texture on GPUI's device.
//!
//! Not through GPUI's sprite atlas: for a source-resolution frame that takes almost twice as
//! long and keeps a second copy of the pixels (docs/gpui-migration.md, step 2.4).

use crate::device;

/// Rows converted and uploaded at a time. The conversion buffer then stays in the CPU caches.
const BAND_ROWS: usize = 64;

/// A texture showing these packed 8-bit RGB pixels. `None` without a device, or when the image
/// does not fit it.
pub fn upload_rgb(width: usize, height: usize, rgb: &[u8]) -> Option<wgpu::TextureView> {
    if rgb.len() != width.checked_mul(height)?.checked_mul(3)? {
        return None;
    }
    let context = device::shared_context()?;
    let texture = texture(&context.device, width, height)?;
    let mut rgba = vec![255; width * BAND_ROWS * 4];
    for (band, rgb) in rgb.chunks(width * BAND_ROWS * 3).enumerate() {
        let pixels = rgb.as_chunks::<3>().0;
        for (target, &[r, g, b]) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(pixels) {
            *target = [r, g, b, 255];
        }
        let rgba = &rgba[..pixels.len() * 4];
        write_rows(&context.queue, &texture, band * BAND_ROWS, rgba);
    }
    Some(texture.create_view(&wgpu::TextureViewDescriptor::default()))
}

/// As [`upload_rgb`], for pixels with an opacity (not premultiplied).
pub fn upload_rgba(width: usize, height: usize, pixels: &[[u8; 4]]) -> Option<wgpu::TextureView> {
    if pixels.len() != width.checked_mul(height)? {
        return None;
    }
    let context = device::shared_context()?;
    let texture = texture(&context.device, width, height)?;
    write_rows(&context.queue, &texture, 0, pixels.as_flattened());
    Some(texture.create_view(&wgpu::TextureViewDescriptor::default()))
}

fn texture(device: &wgpu::Device, width: usize, height: usize) -> Option<wgpu::Texture> {
    let largest = device.limits().max_texture_dimension_2d as usize;
    let fits = (1..=largest).contains(&width) && (1..=largest).contains(&height);
    fits.then(|| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("CPU image"),
            size: wgpu::Extent3d {
                width: width as u32,
                height: height as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    })
}

/// Fill the texture from row `first` on.
fn write_rows(queue: &wgpu::Queue, texture: &wgpu::Texture, first: usize, rgba: &[u8]) {
    let width = texture.width();
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            origin: wgpu::Origin3d {
                x: 0,
                y: first as u32,
                z: 0,
            },
            ..texture.as_image_copy()
        },
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width,
            height: rgba.len() as u32 / (width * 4),
            depth_or_array_layers: 1,
        },
    );
}
