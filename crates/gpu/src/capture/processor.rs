use super::*;

impl GpuCaptureProcessor {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn process_resident_inner(
        &self,
        image: &LinearRgbImage<f32>,
        contract: &DetailContract,
        plan: &TilePlan,
        noise: Option<NoiseProfiles>,
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
            noise,
            resident,
            scatter,
            cancellation,
            None,
            |_encoder, buffer, x0, y0, width, height| {
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
        contract: &DetailContract,
        plan: &TilePlan,
        noise: Option<NoiseProfiles>,
        resident: &crate::spatial::source::ResidentCameraPlanes,
        scatter: &crate::spatial::CaptureScatter,
        cancellation: &CancellationToken,
        reservation: Option<crate::memory::Reservation>,
        mut fill: impl FnMut(
            &mut wgpu::CommandEncoder,
            &wgpu::Buffer,
            usize,
            usize,
            usize,
            usize,
        ) -> Result<u64, GpuPreviewError>,
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
                // The producer, every capture pass and the scatter share one
                // submission; the shared scratch is idle again once it completes.
                let tile_started = Instant::now();
                let mut encoder = self.encoder();
                let uploaded = fill(&mut encoder, &resources.rgb, x0, y0, width, height)?;
                metrics.uploaded_bytes += uploaded;
                let produced = tile_started.elapsed();
                if uploaded == 0 {
                    metrics.compute_and_wait += produced;
                } else {
                    metrics.upload += produced;
                }
                let compute_started = Instant::now();
                self.encode_tile(
                    &mut encoder,
                    &resources,
                    contract,
                    noise.as_ref(),
                    (width, height),
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
                metrics.submissions += 1;
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
        contract: &DetailContract,
        resident: &crate::spatial::source::ResidentCameraPlanes,
        cancellation: &CancellationToken,
        fill: impl FnMut(
            &mut wgpu::CommandEncoder,
            &wgpu::Buffer,
            usize,
            usize,
            usize,
            usize,
        ) -> Result<u64, GpuPreviewError>,
    ) -> Result<CaptureMetrics, GpuPreviewError> {
        // Tiles are produced one at a time, so there is no image to profile
        // before the first one is filtered.
        if contract.noise_reduction().is_active() {
            return Err(error(
                "noise reduction needs the whole image before tiles are generated",
            ));
        }
        let dimensions = (
            resident.layout.width as usize,
            resident.layout.height as usize,
        );
        let plan = TilePlan::new_generated(
            dimensions.0,
            dimensions.1,
            contract,
            self.budget,
            &self.device.limits(),
            self.maximum_tile_edge,
        )?;
        let scatter = crate::spatial::CaptureScatter::new(&self.device);
        self.process_resident_tiles(
            dimensions,
            contract,
            &plan,
            None,
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
        contract: &DetailContract,
        dimensions: (usize, usize),
    ) -> Result<crate::memory::Reservation, GpuPreviewError> {
        let plan = TilePlan::new_generated(
            dimensions.0,
            dimensions.1,
            contract,
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
        contract: &DetailContract,
        source: &crate::spatial::source::ResidentCameraPlanes,
        destination: &crate::spatial::source::ResidentCameraPlanes,
        reservation: crate::memory::Reservation,
        cancellation: &CancellationToken,
    ) -> Result<CaptureMetrics, GpuPreviewError> {
        let dimensions = (source.layout.width as usize, source.layout.height as usize);
        let plan = TilePlan::new_generated(
            dimensions.0,
            dimensions.1,
            contract,
            self.budget,
            &self.device.limits(),
            self.maximum_tile_edge,
        )?;
        let noise = self.estimate_noise_from_resident(source, contract, cancellation)?;
        let loader = ResidentLoader::new(&self.device);
        let scatter = crate::spatial::CaptureScatter::new(&self.device);
        self.process_resident_tiles(
            dimensions,
            contract,
            &plan,
            noise,
            destination,
            &scatter,
            cancellation,
            Some(reservation),
            |encoder, buffer, x, y, width, height| {
                loader.encode(
                    &self.device,
                    &self.queue,
                    encoder,
                    source,
                    buffer,
                    [x, y, width, height],
                );
                Ok(0)
            },
        )
    }

    pub(super) fn process_inner(
        &self,
        image: &LinearRgbImage<f32>,
        contract: &DetailContract,
        plan: &TilePlan,
        noise: Option<NoiseProfiles>,
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
                self.encode_tile(
                    &mut encoder,
                    &resources,
                    contract,
                    noise.as_ref(),
                    (width, height),
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
                metrics.submissions += 1;
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

    /// Every pass for one tile, in one submission's worth of encoding: the
    /// detail steps, then capture sharpening if it is on.
    fn encode_tile(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        resources: &Resources,
        contract: &DetailContract,
        noise: Option<&NoiseProfiles>,
        dimensions: (usize, usize),
    ) {
        self.detail.encode(
            &self.device,
            &self.queue,
            encoder,
            resources,
            contract,
            noise,
            dimensions,
        );
        if contract.capture().settings().is_active() {
            self.encode_ops(
                encoder,
                resources,
                contract.capture(),
                dimensions,
                tile_schedule(),
            );
        }
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

    pub(super) fn wait<E: std::fmt::Display>(
        &self,
        receiver: &mpsc::Receiver<Result<(), E>>,
        cancellation: &CancellationToken,
    ) -> Result<(), GpuPreviewError> {
        // Always drain the submitted unit, even after cancellation: the next
        // request must never overlap still-running capture scratch. Blocking in
        // the driver avoids the scheduler-granularity sleep of a polling loop.
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| error(format!("{e:?}")))?;
        match receiver.try_recv() {
            Ok(r) => {
                r.map_err(error)?;
                check_cancel(cancellation)
            }
            Err(mpsc::TryRecvError::Disconnected) => Err(error("capture completion disconnected")),
            Err(mpsc::TryRecvError::Empty) => Err(error("capture completion was not reported")),
        }
    }

    /// Record `ops` into one compute pass. Dispatches in a pass see each
    /// other's storage writes, so a whole tile needs one submission instead
    /// of a blocking round trip per stage. Parameter blocks and bind groups
    /// are built once per distinct op and reused across RL iterations.
    pub(super) fn encode_ops(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        resources: &Resources,
        contract: &CaptureSharpeningContract,
        (width, height): (usize, usize),
        ops: impl IntoIterator<Item = Op>,
    ) {
        let mut bindings: Vec<(Op, wgpu::BindGroup)> = Vec::new();
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("capture tile"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        for op in ops {
            let index = match bindings.iter().position(|(known, _)| *known == op) {
                Some(index) => index,
                None => {
                    let group = self.op_bindings(resources, contract, width, height, op);
                    bindings.push((op, group));
                    bindings.len() - 1
                }
            };
            pass.set_bind_group(0, &bindings[index].1, &[]);
            pass.dispatch_workgroups((width as u32).div_ceil(8), (height as u32).div_ceil(8), 1);
        }
    }

    fn op_bindings(
        &self,
        resources: &Resources,
        contract: &CaptureSharpeningContract,
        width: usize,
        height: usize,
        op: Op,
    ) -> wgpu::BindGroup {
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
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
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
        })
    }
}

/// One scalar-pass invocation: `[mode, input plane, output plane, axis]`,
/// matching `Parameters` in `capture.wgsl`.
pub(super) type Op = [u32; 4];

// Pass schedule, shared by production and the stage-by-stage tests. Planes:
// 0 guide, 1 mask, 2 estimate, 3 temporary, 4 blurred, 5 ratio.
/// Guide, raw protection mask and initial estimate.
pub(super) const GUIDE: [Op; 1] = [[0, 0, 0, 0]];
/// Smooth the mask, then gate it by local contrast and shadow level.
pub(super) const MASK: [Op; 6] = [
    [1, 1, 3, 0],
    [1, 3, 4, 1],
    [2, 0, 0, 0],
    [1, 0, 3, 0],
    [1, 3, 4, 1],
    [3, 0, 0, 0],
];
/// One RL iteration: blur estimate, form ratio, blur ratio, update estimate.
pub(super) const ITERATION: [Op; 6] = [
    [1, 2, 3, 0],
    [1, 3, 4, 1],
    [4, 0, 0, 0],
    [1, 5, 3, 0],
    [1, 3, 4, 1],
    [5, 0, 0, 0],
];
/// Blend the estimate into the RGB tile.
pub(super) const APPLY: [Op; 1] = [[6, 0, 0, 0]];

/// Every pass for one tile, in order.
fn tile_schedule() -> impl Iterator<Item = Op> {
    GUIDE
        .into_iter()
        .chain(MASK)
        .chain((0..CAPTURE_SHARPENING_ITERATIONS).flat_map(|_| ITERATION))
        .chain(APPLY)
}
