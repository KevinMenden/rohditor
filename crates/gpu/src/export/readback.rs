use super::*;

impl GpuExportProcessor {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn read_bands(
        &self,
        width: usize,
        height: usize,
        depth: OutputBitDepth,
        dithering: DitherMode,
        cancellation: &CancellationToken,
        output: &wgpu::Buffer,
        staging: &wgpu::Buffer,
        band_parameters: &wgpu::Buffer,
        bindings: &wgpu::BindGroup,
    ) -> Result<ExportImage, GpuPreviewError> {
        let count = width
            .checked_mul(height)
            .and_then(|n| n.checked_mul(3))
            .ok_or_else(|| input_error("export sample count overflow"))?;
        let mut eight = Vec::<u8>::new();
        let mut sixteen = Vec::<u16>::new();
        let maximum = match depth {
            OutputBitDepth::Eight => {
                eight.try_reserve_exact(count).map_err(input_error)?;
                255_u32
            }
            OutputBitDepth::Sixteen => {
                sixteen.try_reserve_exact(count).map_err(input_error)?;
                65535_u32
            }
        };
        let device = &self.processor.device;
        let queue = &self.processor.queue;
        for first in (0..height as u32).step_by(BAND_ROWS as usize) {
            check_cancel(cancellation)?;
            let rows = BAND_ROWS.min(height as u32 - first);
            let words = [
                first,
                rows,
                maximum,
                u32::from(dithering == DitherMode::Ordered8x8),
            ];
            queue.write_buffer(band_parameters, 0, bytemuck::cast_slice(&words));
            let bytes = width as u64 * u64::from(rows) * 12;
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rohditor export band"),
            });
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("rohditor export color band"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, bindings, &[]);
                pass.dispatch_workgroups(
                    (width as u32).div_ceil(WORKGROUP_EDGE),
                    rows.div_ceil(WORKGROUP_EDGE),
                    1,
                );
            }
            encoder.copy_buffer_to_buffer(output, 0, staging, 0, bytes);
            queue.submit([encoder.finish()]);
            let (sender, receiver) = mpsc::sync_channel(1);
            staging
                .slice(..bytes)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let _ = sender.send(result);
                });
            // Only one bounded band is submitted at a time. Poll cooperatively
            // so cancellation does not wait for an entire full-frame dispatch.
            loop {
                device
                    .poll(wgpu::PollType::Poll)
                    .map_err(|error| input_error(format!("{error:?}")))?;
                match receiver.try_recv() {
                    Ok(result) => {
                        result.map_err(input_error)?;
                        break;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(input_error("export readback disconnected"));
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        if cancellation.is_cancelled() {
                            staging.unmap();
                            return Err(GpuPreviewError::Cancelled);
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            }
            {
                let mapped = staging.slice(..bytes).get_mapped_range();
                let samples: &[u32] = bytemuck::cast_slice(&mapped);
                match depth {
                    OutputBitDepth::Eight => {
                        eight.extend(samples.iter().map(|&sample| sample as u8))
                    }
                    OutputBitDepth::Sixteen => {
                        sixteen.extend(samples.iter().map(|&sample| sample as u16))
                    }
                }
            }
            staging.unmap();
        }
        check_cancel(cancellation)?;
        match depth {
            OutputBitDepth::Eight => {
                DisplayRgbImage::new(width, height, width * 3, DisplayTransfer::Srgb, eight)
                    .map(ExportImage::Rgb8)
                    .map_err(input_error)
            }
            OutputBitDepth::Sixteen => {
                DisplayRgbImage::new(width, height, width * 3, DisplayTransfer::Srgb, sixteen)
                    .map(ExportImage::Rgb16)
                    .map_err(input_error)
            }
        }
    }
}
