use super::*;

impl GpuCaptureProcessor {
    pub(super) fn process_resident_inner(
        &self,
        image: &LinearRgbImage<f32>,
        contract: &CaptureSharpeningContract,
        plan: &TilePlan,
        resident: &crate::spatial::source::ResidentCameraPlanes,
        scatter: &crate::spatial::CaptureScatter,
        cancellation: &CancellationToken,
    ) -> Result<CaptureMetrics, GpuPreviewError> {
        let mut packed = Vec::<f32>::new();
        packed.try_reserve_exact(plan.pixels * 3).map_err(error)?;
        self.process_resident_tiles(
            (image.width(), image.height()),
            contract,
            plan,
            resident,
            scatter,
            cancellation,
            None,
            |buffer, x0, y0, width, height| {
                packed.clear();
                for y in y0..y0 + height {
                    check_cancel(cancellation)?;
                    let row = &image.data()[y * image.row_stride() + x0 * 3..][..width * 3];
                    if row.iter().any(|value| !value.is_finite()) {
                        return Err(error(format!("non-finite capture input in row {y}")));
                    }
                    packed.extend_from_slice(row);
                }
                self.queue
                    .write_buffer(buffer, 0, bytemuck::cast_slice(&packed));
                Ok(packed.len() as u64 * 4)
            },
        )
    }

    /// Share the exact capture passes between CPU uploads and GPU-generated
    /// tiles. The producer fills just one halo-expanded input buffer.
    #[allow(clippy::too_many_arguments)]
    fn process_resident_tiles(
        &self,
        dimensions: (usize, usize),
        contract: &CaptureSharpeningContract,
        plan: &TilePlan,
        resident: &crate::spatial::source::ResidentCameraPlanes,
        scatter: &crate::spatial::CaptureScatter,
        cancellation: &CancellationToken,
        reservation: Option<crate::memory::Reservation>,
        mut fill: impl FnMut(&wgpu::Buffer, usize, usize, usize, usize) -> Result<u64, GpuPreviewError>,
    ) -> Result<CaptureMetrics, GpuPreviewError> {
        let started = Instant::now();
        let _reservation = match reservation {
            Some(held) => held,
            None => crate::memory::Reservation::try_new(
                plan.gpu_bytes,
                crate::spatial::resources::DEFAULT_BUDGET,
            )?,
        };
        let resources = Resources::new_resident(&self.device, &self.queue, plan.pixels, contract);
        let halo = contract.halo();
        let mut metrics = CaptureMetrics {
            tile_edge: plan.edge,
            halo,
            estimated_gpu_bytes: plan.gpu_bytes + resident.layout.resident_bytes,
            estimated_host_bytes: plan.host_bytes,
            ..Default::default()
        };
        for top in (0..dimensions.1).step_by(plan.edge) {
            for left in (0..dimensions.0).step_by(plan.edge) {
                check_cancel(cancellation)?;
                let x0 = left.saturating_sub(halo);
                let y0 = top.saturating_sub(halo);
                let right = (left + plan.edge).min(dimensions.0);
                let bottom = (top + plan.edge).min(dimensions.1);
                let x1 = (right + halo).min(dimensions.0);
                let y1 = (bottom + halo).min(dimensions.1);
                let (width, height) = (x1 - x0, y1 - y0);
                let upload_started = Instant::now();
                let uploaded = fill(&resources.rgb, x0, y0, width, height)?;
                metrics.uploaded_bytes += uploaded;
                if uploaded == 0 {
                    metrics.compute_and_wait += upload_started.elapsed();
                } else {
                    metrics.upload += upload_started.elapsed();
                }

                let compute_started = Instant::now();
                let mut encoder = self.encoder();
                self.pass(
                    &mut encoder,
                    &resources,
                    contract,
                    width,
                    height,
                    [0, 0, 0, 0],
                );
                self.blur(&mut encoder, &resources, contract, width, height, 1);
                self.pass(
                    &mut encoder,
                    &resources,
                    contract,
                    width,
                    height,
                    [2, 0, 0, 0],
                );
                self.blur(&mut encoder, &resources, contract, width, height, 0);
                self.pass(
                    &mut encoder,
                    &resources,
                    contract,
                    width,
                    height,
                    [3, 0, 0, 0],
                );
                self.submit(encoder, cancellation)?;
                for _ in 0..CAPTURE_SHARPENING_ITERATIONS {
                    check_cancel(cancellation)?;
                    let mut encoder = self.encoder();
                    self.blur(&mut encoder, &resources, contract, width, height, 2);
                    self.pass(
                        &mut encoder,
                        &resources,
                        contract,
                        width,
                        height,
                        [4, 0, 0, 0],
                    );
                    self.blur(&mut encoder, &resources, contract, width, height, 5);
                    self.pass(
                        &mut encoder,
                        &resources,
                        contract,
                        width,
                        height,
                        [5, 0, 0, 0],
                    );
                    self.submit(encoder, cancellation)?;
                }
                let mut encoder = self.encoder();
                self.pass(
                    &mut encoder,
                    &resources,
                    contract,
                    width,
                    height,
                    [6, 0, 0, 0],
                );
                scatter.encode(
                    &self.device,
                    &self.queue,
                    &mut encoder,
                    &resources.rgb,
                    resident,
                    width,
                    x0,
                    y0,
                    left,
                    top,
                    right - left,
                    bottom - top,
                )?;
                self.submit(encoder, cancellation)?;
                metrics.compute_and_wait += compute_started.elapsed();
                metrics.tiles += 1;
            }
        }
        check_cancel(cancellation)?;
        metrics.total = started.elapsed();
        metrics.combined_gpu_reservations = crate::gpu_memory_reservations();
        Ok(metrics)
    }

    pub(crate) fn process_generated_to_resident(
        &self,
        contract: &CaptureSharpeningContract,
        resident: &crate::spatial::source::ResidentCameraPlanes,
        cancellation: &CancellationToken,
        fill: impl FnMut(&wgpu::Buffer, usize, usize, usize, usize) -> Result<u64, GpuPreviewError>,
    ) -> Result<CaptureMetrics, GpuPreviewError> {
        let dimensions = (
            resident.layout.width as usize,
            resident.layout.height as usize,
        );
        let plan = TilePlan::new_generated(
            dimensions.0,
            dimensions.1,
            contract.halo(),
            self.budget,
            &self.device.limits(),
            self.maximum_tile_edge,
        )?;
        let scatter = crate::spatial::CaptureScatter::new(&self.device);
        self.process_resident_tiles(
            dimensions,
            contract,
            &plan,
            resident,
            &scatter,
            cancellation,
            None,
            fill,
        )
    }

    /// Hold capture's bounded scratch before the preceding RCD work begins.
    pub(crate) fn reserve_generated(
        &self,
        contract: &CaptureSharpeningContract,
        dimensions: (usize, usize),
    ) -> Result<crate::memory::Reservation, GpuPreviewError> {
        let plan = TilePlan::new_generated(
            dimensions.0,
            dimensions.1,
            contract.halo(),
            self.budget,
            &self.device.limits(),
            self.maximum_tile_edge,
        )?;
        crate::memory::Reservation::try_new(
            plan.gpu_bytes,
            crate::spatial::resources::DEFAULT_BUDGET,
        )
    }

    /// Load each halo-expanded input tile from resident demosaic planes.
    /// No camera RGB crosses the CPU boundary.
    pub(crate) fn process_resident_from_resident(
        &self,
        contract: &CaptureSharpeningContract,
        source: &crate::spatial::source::ResidentCameraPlanes,
        destination: &crate::spatial::source::ResidentCameraPlanes,
        reservation: crate::memory::Reservation,
        cancellation: &CancellationToken,
    ) -> Result<CaptureMetrics, GpuPreviewError> {
        let dimensions = (source.layout.width as usize, source.layout.height as usize);
        let plan = TilePlan::new_generated(
            dimensions.0,
            dimensions.1,
            contract.halo(),
            self.budget,
            &self.device.limits(),
            self.maximum_tile_edge,
        )?;
        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("resident camera capture input"),
                source: wgpu::ShaderSource::Wgsl(include_str!("resident_load.wgsl").into()),
            });
        let pipeline = self
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("load resident capture tile"),
                layout: None,
                module: &shader,
                entry_point: Some("load_resident"),
                compilation_options: Default::default(),
                cache: None,
            });
        let scatter = crate::spatial::CaptureScatter::new(&self.device);
        self.process_resident_tiles(
            dimensions,
            contract,
            &plan,
            destination,
            &scatter,
            cancellation,
            Some(reservation),
            |buffer, x, y, width, height| {
                let layout = source.layout;
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
                    &self.device,
                    &self.queue,
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
                let bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("resident capture input bindings"),
                    layout: &pipeline.get_bind_group_layout(0),
                    entries: &entries,
                });
                let mut encoder = self.encoder();
                {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("load resident capture input"),
                        timestamp_writes: None,
                    });
                    pass.set_pipeline(&pipeline);
                    pass.set_bind_group(0, &bindings, &[]);
                    pass.dispatch_workgroups(
                        (width as u32).div_ceil(8),
                        (height as u32).div_ceil(8),
                        1,
                    );
                }
                self.submit(encoder, cancellation)?;
                Ok(0)
            },
        )
    }

    pub(super) fn process_inner(
        &self,
        image: &LinearRgbImage<f32>,
        contract: &CaptureSharpeningContract,
        plan: &TilePlan,
        cancellation: &CancellationToken,
    ) -> Result<GpuCaptureResult, GpuPreviewError> {
        let started = Instant::now();
        let _reservation = crate::memory::Reservation::try_new(
            plan.gpu_bytes,
            crate::spatial::resources::DEFAULT_BUDGET,
        )?;
        let mut data = Vec::new();
        data.try_reserve_exact(image.data().len()).map_err(error)?;
        data.extend_from_slice(image.data());
        let mut result = LinearRgbImage::new(
            image.width(),
            image.height(),
            image.row_stride(),
            image.space(),
            data,
        )
        .map_err(error)?;
        let resources = Resources::new(&self.device, &self.queue, plan.pixels, contract);
        let mut packed = Vec::<f32>::new();
        packed.try_reserve_exact(plan.pixels * 3).map_err(error)?;
        let halo = contract.halo();
        let mut metrics = CaptureMetrics {
            tile_edge: plan.edge,
            halo,
            estimated_gpu_bytes: plan.gpu_bytes,
            estimated_host_bytes: image.data().len() * 4 + plan.host_bytes,
            ..Default::default()
        };
        for top in (0..image.height()).step_by(plan.edge) {
            for left in (0..image.width()).step_by(plan.edge) {
                check_cancel(cancellation)?;
                let x0 = left.saturating_sub(halo);
                let y0 = top.saturating_sub(halo);
                let right = (left + plan.edge).min(image.width());
                let bottom = (top + plan.edge).min(image.height());
                let x1 = (right + halo).min(image.width());
                let y1 = (bottom + halo).min(image.height());
                let (width, height) = (x1 - x0, y1 - y0);
                let upload_started = Instant::now();
                packed.clear();
                for y in y0..y1 {
                    check_cancel(cancellation)?;
                    let row = &image.data()[y * image.row_stride() + x0 * 3..][..width * 3];
                    if row.iter().any(|v| !v.is_finite()) {
                        return Err(error(format!("non-finite capture input in row {y}")));
                    }
                    packed.extend_from_slice(row);
                }
                self.queue
                    .write_buffer(&resources.rgb, 0, bytemuck::cast_slice(&packed));
                let bytes = packed.len() as u64 * 4;
                metrics.uploaded_bytes += bytes;
                metrics.upload += upload_started.elapsed();
                let compute_started = Instant::now();
                // At actual image edges this domain has the global boundary.
                // At artificial edges the halo prevents their mirrored samples
                // from influencing a retained core in any of the eight iterations.
                let mut encoder = self.encoder();
                self.pass(
                    &mut encoder,
                    &resources,
                    contract,
                    width,
                    height,
                    [0, 0, 0, 0],
                );
                self.blur(&mut encoder, &resources, contract, width, height, 1);
                self.pass(
                    &mut encoder,
                    &resources,
                    contract,
                    width,
                    height,
                    [2, 0, 0, 0],
                );
                self.blur(&mut encoder, &resources, contract, width, height, 0);
                self.pass(
                    &mut encoder,
                    &resources,
                    contract,
                    width,
                    height,
                    [3, 0, 0, 0],
                );
                self.submit(encoder, cancellation)?;
                for _ in 0..CAPTURE_SHARPENING_ITERATIONS {
                    check_cancel(cancellation)?;
                    let mut encoder = self.encoder();
                    self.blur(&mut encoder, &resources, contract, width, height, 2);
                    self.pass(
                        &mut encoder,
                        &resources,
                        contract,
                        width,
                        height,
                        [4, 0, 0, 0],
                    );
                    self.blur(&mut encoder, &resources, contract, width, height, 5);
                    self.pass(
                        &mut encoder,
                        &resources,
                        contract,
                        width,
                        height,
                        [5, 0, 0, 0],
                    );
                    self.submit(encoder, cancellation)?;
                }
                let mut encoder = self.encoder();
                self.pass(
                    &mut encoder,
                    &resources,
                    contract,
                    width,
                    height,
                    [6, 0, 0, 0],
                );
                let staging = resources
                    .staging
                    .as_ref()
                    .expect("legacy capture allocates readback staging");
                encoder.copy_buffer_to_buffer(&resources.rgb, 0, staging, 0, bytes);
                self.submit(encoder, cancellation)?;
                metrics.compute_and_wait += compute_started.elapsed();
                let readback_started = Instant::now();
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
                let assembly = (|| {
                    let mapped = staging.slice(..bytes).get_mapped_range();
                    let samples: &[f32] = bytemuck::cast_slice(&mapped);
                    for y in top..bottom {
                        check_cancel(cancellation)?;
                        let offset = ((y - y0) * width + left - x0) * 3;
                        let source = &samples[offset..][..(right - left) * 3];
                        if source.iter().any(|v| !v.is_finite()) {
                            return Err(error("non-finite capture readback"));
                        }
                        result.data_mut()[y * image.row_stride() + left * 3..][..source.len()]
                            .copy_from_slice(source);
                    }
                    Ok(())
                })();
                staging.unmap();
                assembly?;
                metrics.readback += readback_started.elapsed();
                metrics.readback_bytes += bytes;
                metrics.tiles += 1;
            }
        }
        check_cancel(cancellation)?;
        metrics.total = started.elapsed();
        metrics.combined_gpu_reservations = crate::gpu_memory_reservations();
        Ok(GpuCaptureResult {
            image: result,
            metrics,
        })
    }

    pub(super) fn encoder(&self) -> wgpu::CommandEncoder {
        self.device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("bounded capture work"),
            })
    }

    pub(super) fn submit(
        &self,
        encoder: wgpu::CommandEncoder,
        cancellation: &CancellationToken,
    ) -> Result<(), GpuPreviewError> {
        check_cancel(cancellation)?;
        self.queue.submit([encoder.finish()]);
        let (sender, receiver) = mpsc::sync_channel(1);
        self.queue.on_submitted_work_done(move || {
            let _ = sender.send(Ok::<(), String>(()));
        });
        self.wait(&receiver, cancellation)
    }

    fn wait<E: std::fmt::Display>(
        &self,
        receiver: &mpsc::Receiver<Result<(), E>>,
        cancellation: &CancellationToken,
    ) -> Result<(), GpuPreviewError> {
        loop {
            self.device
                .poll(wgpu::PollType::Poll)
                .map_err(|e| error(format!("{e:?}")))?;
            match receiver.try_recv() {
                Ok(r) => {
                    r.map_err(error)?;
                    return check_cancel(cancellation);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(error("capture completion disconnected"));
                }
                Err(mpsc::TryRecvError::Empty) => {
                    // Drain this bounded unit even after cancellation. The next
                    // request must never overlap still-running capture scratch.
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn blur(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        resources: &Resources,
        contract: &CaptureSharpeningContract,
        width: usize,
        height: usize,
        input: u32,
    ) {
        self.pass(
            encoder,
            resources,
            contract,
            width,
            height,
            [1, input, 3, 0],
        );
        self.pass(encoder, resources, contract, width, height, [1, 3, 4, 1]);
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        resources: &Resources,
        contract: &CaptureSharpeningContract,
        width: usize,
        height: usize,
        op: [u32; 4],
    ) {
        let words = [
            width as u32,
            height as u32,
            op[0],
            (contract.weights().len() / 2) as u32,
            op[1],
            op[2],
            op[3],
            0,
            contract.settings().amount.to_bits(),
            contract.settings().noise_protection.to_bits(),
            CAPTURE_SHARPENING_FLOOR.to_bits(),
            0,
            contract.ceilings()[0].to_bits(),
            contract.ceilings()[1].to_bits(),
            contract.ceilings()[2].to_bits(),
            0,
        ];
        // Avoid mapped-at-creation helpers: a lost device can make their
        // immediate get_mapped_range panic before an error scope is drained.
        let uniform = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture pass parameters"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue
            .write_buffer(&uniform, 0, bytemuck::cast_slice(&words));
        let bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("capture pass bindings"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: resources.rgb.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: resources.planes.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: resources.weights.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("capture scalar pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bindings, &[]);
        pass.dispatch_workgroups((width as u32).div_ceil(8), (height as u32).div_ceil(8), 1);
    }
}
