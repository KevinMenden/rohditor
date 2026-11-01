//! Noise profiles for the detail stage. The estimate is a small CPU
//! reduction over sampled rows of the stage input, shared with the CPU
//! pipeline; only those rows ever leave the GPU.
use super::resident::ResidentLoader;
use super::*;
use crate::spatial::source::ResidentCameraPlanes;
use rohditor_detail::{
    DetailError, estimate_noise_profiles, estimate_noise_profiles_sampled, noise_sample_rows,
};

/// Pixels read back per submission.
const READBACK_PIXELS: usize = 1 << 20;

fn detail_error(e: DetailError) -> GpuPreviewError {
    match e {
        DetailError::Cancelled => GpuPreviewError::Cancelled,
        other => error(other),
    }
}

/// Profiles of a host-resident `image`; `None` when noise reduction is off
/// or the image is too small to profile (the filters are then skipped, as on
/// the CPU).
pub(super) fn estimate_from_image(
    image: &LinearRgbImage<f32>,
    contract: &DetailContract,
    cancellation: &CancellationToken,
) -> Result<Option<NoiseProfiles>, GpuPreviewError> {
    if !contract.noise_reduction().is_active() {
        return Ok(None);
    }
    estimate_noise_profiles(image, contract.balance(), &|| cancellation.is_cancelled())
        .map_err(detail_error)
}

impl GpuCaptureProcessor {
    /// Profiles of resident camera planes, reading back only the sampled rows.
    pub(crate) fn estimate_noise_from_resident(
        &self,
        source: &ResidentCameraPlanes,
        contract: &DetailContract,
        cancellation: &CancellationToken,
    ) -> Result<Option<NoiseProfiles>, GpuPreviewError> {
        if !contract.noise_reduction().is_active() {
            return Ok(None);
        }
        let (width, height) = (source.layout.width as usize, source.layout.height as usize);
        let chunk_rows = (READBACK_PIXELS / width).max(1);
        let buffer = |label, usage| {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: (chunk_rows.min(height) * width * 12) as u64,
                usage,
                mapped_at_creation: false,
            })
        };
        let rgb = buffer(
            "noise sample RGB",
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );
        let staging = buffer(
            "noise sample readback",
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );
        let loader = ResidentLoader::new(&self.device);
        let mut samples = Vec::<f32>::new();
        let sampled_rows: usize = noise_sample_rows(height).iter().map(|r| r.len()).sum();
        samples
            .try_reserve_exact(sampled_rows * width * 3)
            .map_err(error)?;
        let mut rows = 0;
        for band in noise_sample_rows(height) {
            for top in band.clone().step_by(chunk_rows) {
                check_cancel(cancellation)?;
                let chunk = chunk_rows.min(band.end - top);
                let bytes = (chunk * width * 12) as u64;
                let mut encoder = self.encoder();
                loader.encode(
                    &self.device,
                    &self.queue,
                    &mut encoder,
                    source,
                    &rgb,
                    [0, top, width, chunk],
                );
                encoder.copy_buffer_to_buffer(&rgb, 0, &staging, 0, bytes);
                self.submit(encoder, cancellation)?;
                let (sender, receiver) = mpsc::sync_channel(1);
                staging
                    .slice(..bytes)
                    .map_async(wgpu::MapMode::Read, move |r| {
                        let _ = sender.send(r);
                    });
                if let Err(e) = self.wait(&receiver, cancellation) {
                    staging.unmap();
                    return Err(e);
                }
                samples.extend_from_slice(bytemuck::cast_slice(
                    &staging.slice(..bytes).get_mapped_range(),
                ));
                staging.unmap();
                rows += chunk;
            }
        }
        let sampled = LinearRgbImage::new(
            width,
            rows,
            width * 3,
            rohditor_image::LinearRgbSpace::CameraNative,
            samples,
        )
        .map_err(error)?;
        estimate_noise_profiles_sampled(&sampled, contract.balance(), &|| {
            cancellation.is_cancelled()
        })
        .map_err(detail_error)
    }
}
