use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use half::f16;
use rohditor_core::{
    CancellationToken, DemosaicedBase, EditRecipe, LIGHT_TONE_LUT_SIZE, LINEAR_REC2020_TO_XYZ_D65,
    LightToneLut, LinearRgbSpace, Matrix3, OrientationMap, ReconstructedPreview, WhiteBalance,
    WhiteBalanceGains, XYZ_D65_TO_LINEAR_SRGB,
};
use rohditor_raw::RawOrientation;

use crate::{GpuCapabilities, GpuPreviewError};

const WORKGROUP_EDGE: u32 = 16;
const PARAMETER_WORDS: usize = 48;

/// One uploaded, immutable camera-native preview source. White balance and the
/// camera transform are applied by the downstream shader, so changing either
/// the WB mode or its values does not require a CPU rebuild or texture upload.
pub struct GpuPreviewSource {
    // A texture view does not make the source's ownership explicit. Keep the
    // texture alongside it so the source remains valid for every later edit.
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    source_orientation: RawOrientation,
    white_balance: WhiteBalance,
    white_balance_dynamic: bool,
    white_balance_gains: WhiteBalanceGains,
    as_shot_white_balance: [Option<f32>; 4],
    camera_to_xyz_d65: Matrix3,
    camera_to_linear_rec2020: Matrix3,
}

impl GpuPreviewSource {
    /// Source dimensions before applying EXIF orientation.
    #[must_use]
    pub const fn source_dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// White-balance selection used when this source was first uploaded.
    #[must_use]
    pub const fn white_balance(&self) -> WhiteBalance {
        self.white_balance
    }

    /// Whether this source can resolve any recipe white balance on the GPU.
    #[must_use]
    pub const fn supports_dynamic_white_balance(&self) -> bool {
        self.white_balance_dynamic
    }

    fn resolve_white_balance(
        &self,
        selection: WhiteBalance,
    ) -> Result<WhiteBalanceGains, GpuPreviewError> {
        if !self.white_balance_dynamic {
            if selection != self.white_balance {
                return Err(GpuPreviewError::BaseMismatch {
                    reason: "this converted source cannot change white balance without a rebuild"
                        .to_owned(),
                });
            }
            return Ok(self.white_balance_gains);
        }
        rohditor_core::white_balance_gains_from_calibration(
            self.as_shot_white_balance,
            self.camera_to_xyz_d65,
            selection,
        )
        .map_err(|error| GpuPreviewError::InvalidInput {
            reason: error.to_string(),
        })
    }

    /// Estimated bytes occupied by the retained RGBA16Float source texture.
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        (self.width as usize)
            .saturating_mul(self.height as usize)
            .saturating_mul(8)
    }
}

/// CPU-packed half-float upload payload for one immutable linear preview source.
///
/// Creating this payload performs no `wgpu` work, so the desktop worker can do
/// the conversion before handing it to the UI thread for the single GPU upload.
#[derive(Debug)]
pub struct GpuPreviewUpload {
    texels: Vec<u16>,
    width: u32,
    height: u32,
    source_orientation: RawOrientation,
    white_balance: WhiteBalance,
    white_balance_dynamic: bool,
    white_balance_gains: WhiteBalanceGains,
    as_shot_white_balance: [Option<f32>; 4],
    camera_to_xyz_d65: Matrix3,
    camera_to_linear_rec2020: Matrix3,
}

impl GpuPreviewUpload {
    /// Pack a typed, camera-converted linear Rec.2020 base as RGBA16Float texels.
    pub fn from_demosaiced_base(base: &DemosaicedBase) -> Result<Self, GpuPreviewError> {
        Self::from_demosaiced_base_cancellable(base, &CancellationToken::new())
    }

    /// Pack a base while observing the desktop preview's cancellation token.
    pub fn from_demosaiced_base_cancellable(
        base: &DemosaicedBase,
        cancellation: &CancellationToken,
    ) -> Result<Self, GpuPreviewError> {
        let image = base.image();
        if image.space() != LinearRgbSpace::Rec2020D65 {
            return Err(GpuPreviewError::InvalidInput {
                reason: "the GPU boundary requires a linear Rec.2020/D65 base".to_owned(),
            });
        }
        let (width, height) = upload_dimensions(image.width(), image.height())?;
        let texels = pack_rgba16f(
            image.data(),
            image.width(),
            image.height(),
            image.row_stride(),
            cancellation,
        )?;
        Ok(Self {
            texels,
            width,
            height,
            source_orientation: base.source_orientation(),
            white_balance: base.white_balance(),
            white_balance_dynamic: false,
            white_balance_gains: WhiteBalanceGains::identity(),
            as_shot_white_balance: [Some(1.0), Some(1.0), Some(1.0), None],
            camera_to_xyz_d65: Matrix3::identity(),
            camera_to_linear_rec2020: Matrix3::identity(),
        })
    }

    /// Pack an immutable camera-native reconstruction and resolve its white
    /// balance as shader parameters. This is the interactive cache boundary.
    pub fn from_reconstructed_preview(
        reconstructed: &ReconstructedPreview,
        white_balance: WhiteBalance,
    ) -> Result<Self, GpuPreviewError> {
        Self::from_reconstructed_preview_cancellable(
            reconstructed,
            white_balance,
            &CancellationToken::new(),
        )
    }

    /// Cancellable form of [`Self::from_reconstructed_preview`].
    pub fn from_reconstructed_preview_cancellable(
        reconstructed: &ReconstructedPreview,
        white_balance: WhiteBalance,
        cancellation: &CancellationToken,
    ) -> Result<Self, GpuPreviewError> {
        let image = reconstructed.image();
        if image.space() != LinearRgbSpace::CameraNative {
            return Err(GpuPreviewError::InvalidInput {
                reason: "the GPU camera source requires camera-native linear RGB".to_owned(),
            });
        }
        let gains = reconstructed
            .white_balance_gains(white_balance)
            .map_err(|error| GpuPreviewError::InvalidInput {
                reason: error.to_string(),
            })?;
        let (width, height) = upload_dimensions(image.width(), image.height())?;
        let texels = pack_rgba16f(
            image.data(),
            image.width(),
            image.height(),
            image.row_stride(),
            cancellation,
        )?;
        Ok(Self {
            texels,
            width,
            height,
            source_orientation: reconstructed.source_orientation(),
            white_balance,
            white_balance_dynamic: true,
            white_balance_gains: gains,
            as_shot_white_balance: reconstructed.as_shot_white_balance(),
            camera_to_xyz_d65: reconstructed.camera_to_xyz_d65(),
            camera_to_linear_rec2020: reconstructed.camera_to_linear_rec2020(),
        })
    }

    /// Source dimensions before physical orientation.
    #[must_use]
    pub const fn source_dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// White-balance selection used when this upload was prepared.
    #[must_use]
    pub const fn white_balance(&self) -> WhiteBalance {
        self.white_balance
    }
}

/// GPU textures produced by one downstream preview dispatch.
pub struct GpuPreviewFrame {
    // Keep the linear texture alive for future stages even though the current
    // fused dispatch does not read it back in a second pass.
    _working_texture: wgpu::Texture,
    working_view: wgpu::TextureView,
    display_texture: wgpu::Texture,
    display_view: wgpu::TextureView,
    source_dimensions: (u32, u32),
    output_dimensions: (u32, u32),
    submission_time: Duration,
    queue_completion_nanos: Arc<AtomicU64>,
    textures_reused: bool,
}

impl GpuPreviewFrame {
    /// The egui-compatible display texture view. Register this view with
    /// `egui_wgpu::Renderer`; it is never read back for normal display.
    #[must_use]
    pub const fn display_view(&self) -> &wgpu::TextureView {
        &self.display_view
    }

    /// Dimensions after physical orientation.
    #[must_use]
    pub const fn output_dimensions(&self) -> (u32, u32) {
        self.output_dimensions
    }

    /// CPU time spent encoding and submitting the dispatch. This is not a GPU
    /// execution measurement; timestamp-query availability is reported through
    /// [`GpuCapabilities`].
    #[must_use]
    pub const fn submission_time(&self) -> Duration {
        self.submission_time
    }

    /// Wall time until the shared queue reports all work submitted before this
    /// preview as complete. This includes queueing delay and is an intentionally
    /// conservative fallback when timestamp queries are unavailable.
    #[must_use]
    pub fn queue_completion_time(&self) -> Option<Duration> {
        let nanos = self.queue_completion_nanos.load(Ordering::Acquire);
        (nanos != 0).then(|| Duration::from_nanos(nanos))
    }

    /// Whether this dispatch reused both output textures from the prior frame.
    #[must_use]
    pub const fn textures_reused(&self) -> bool {
        self.textures_reused
    }

    /// Estimated bytes occupied by the working and display textures.
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        let working = (self.source_dimensions.0 as usize)
            .saturating_mul(self.source_dimensions.1 as usize)
            .saturating_mul(8);
        let display = (self.output_dimensions.0 as usize)
            .saturating_mul(self.output_dimensions.1 as usize)
            .saturating_mul(4);
        working.saturating_add(display)
    }

    fn can_reuse(&self, source_dimensions: (u32, u32), output_dimensions: (u32, u32)) -> bool {
        self.source_dimensions == source_dimensions && self.output_dimensions == output_dimensions
    }
}

/// RGBA8 pixels obtained through an explicit test/diagnostics readback.
/// Normal desktop preview display never constructs this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuDisplayReadback {
    /// Width of the oriented display image.
    pub width: u32,
    /// Height of the oriented display image.
    pub height: u32,
    /// Packed, row-major RGBA8 texels.
    pub rgba: Vec<u8>,
}

/// An asynchronous display-texture readback.
///
/// The request owns the staging buffer until the map callback reports that it
/// is ready. Dropping an unfinished request cancels its consumer without
/// blocking the caller; the application can poll it from its normal event
/// loop and keep the UI responsive while the GPU finishes.
pub struct GpuDisplayReadbackPending {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    unpadded_bytes_per_row: usize,
    padded_bytes_per_row: usize,
    ready: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
}

impl GpuDisplayReadbackPending {
    /// Try to consume the completed readback without waiting.
    ///
    /// `Ok(None)` means that the GPU has not completed the copy yet.
    pub fn try_finish(&mut self) -> Result<Option<GpuDisplayReadback>, GpuPreviewError> {
        let result = match self.ready.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(GpuPreviewError::Readback {
                    reason: "GPU readback completion channel closed unexpectedly".to_owned(),
                });
            }
        };
        result.map_err(|error| GpuPreviewError::Readback {
            reason: error.to_string(),
        })?;

        let mapped = self.buffer.slice(..).get_mapped_range();
        let output_len = self
            .unpadded_bytes_per_row
            .checked_mul(self.height as usize)
            .ok_or_else(|| GpuPreviewError::Readback {
                reason: "readback output size overflowed".to_owned(),
            })?;
        let mut rgba = Vec::with_capacity(output_len);
        for row in mapped
            .chunks_exact(self.padded_bytes_per_row)
            .take(self.height as usize)
        {
            rgba.extend_from_slice(&row[..self.unpadded_bytes_per_row]);
        }
        drop(mapped);
        self.buffer.unmap();
        Ok(Some(GpuDisplayReadback {
            width: self.width,
            height: self.height,
            rgba,
        }))
    }
}

/// Downstream GPU preview processor using the device and queue created by
/// eframe. It intentionally never creates a second adapter or device.
pub struct GpuPreviewProcessor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    capabilities: GpuCapabilities,
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    parameters: wgpu::Buffer,
    light_tone_lut: wgpu::Buffer,
}

impl GpuPreviewProcessor {
    /// Create a processor from eframe's shared render state.
    ///
    /// `adapter`, `device`, and `queue` must all originate from the same
    /// eframe `wgpu_render_state`; no device is created here.
    pub fn new(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
    ) -> Result<Self, GpuPreviewError> {
        let capabilities = GpuCapabilities::detect(adapter, device, target_format);
        capabilities.validate_preview_support()?;

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rohditor GPU preview bindings"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rohditor GPU preview pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rohditor GPU preview shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("preview.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rohditor GPU downstream preview"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("develop_preview"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let parameters = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rohditor GPU preview parameters"),
            size: (PARAMETER_WORDS * size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let light_tone_lut = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rohditor shared Light tone LUT"),
            size: (LIGHT_TONE_LUT_SIZE * size_of::<f32>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            device: device.clone(),
            queue: queue.clone(),
            capabilities,
            pipeline,
            bind_group_layout,
            parameters,
            light_tone_lut,
        })
    }

    /// Facts about the shared adapter/device and preview format support.
    #[must_use]
    pub const fn capabilities(&self) -> &GpuCapabilities {
        &self.capabilities
    }

    /// Whether the current shader implements every adjustment in a recipe.
    /// HSL and three-way grading remain CPU stages until their GPU formulas
    /// have a defined parity contract.
    #[must_use]
    pub fn supports_recipe(recipe: &EditRecipe) -> bool {
        recipe.color.hsl.channels.iter().all(|channel| {
            channel.hue == 0.0 && channel.saturation == 0.0 && channel.luminance == 0.0
        }) && recipe.color.grading.shadows == [0.0; 3]
            && recipe.color.grading.midtones == [0.0; 3]
            && recipe.color.grading.highlights == [0.0; 3]
    }

    /// Poll callbacks for non-blocking GPU work, including asynchronous
    /// display readbacks. This never waits for the device to become idle.
    pub fn poll(&self) -> Result<(), GpuPreviewError> {
        self.device.poll(wgpu::PollType::Poll).map_err(|error| {
            GpuPreviewError::Synchronization {
                reason: format!("{error:?}"),
            }
        })?;
        Ok(())
    }

    /// Pack and upload one legacy linear Rec.2020 base. Callers retain the
    /// returned value for downstream edits with the same white balance.
    pub fn upload_base(&self, base: &DemosaicedBase) -> Result<GpuPreviewSource, GpuPreviewError> {
        self.upload_prepared(GpuPreviewUpload::from_demosaiced_base(base)?)
    }

    /// Upload a worker-prepared linear source. This performs one queue write but
    /// deliberately does not perform per-pixel packing on the UI thread.
    pub fn upload_prepared(
        &self,
        upload: GpuPreviewUpload,
    ) -> Result<GpuPreviewSource, GpuPreviewError> {
        let source_width =
            usize::try_from(upload.width).map_err(|_| GpuPreviewError::InvalidInput {
                reason: "prepared source width does not fit this platform's usize".to_owned(),
            })?;
        let source_height =
            usize::try_from(upload.height).map_err(|_| GpuPreviewError::InvalidInput {
                reason: "prepared source height does not fit this platform's usize".to_owned(),
            })?;
        let (width, height) = self
            .capabilities
            .validate_dimensions(source_width, source_height)?;
        let bytes_per_row =
            width
                .checked_mul(8)
                .ok_or_else(|| GpuPreviewError::InvalidDimensions {
                    width: source_width,
                    height: source_height,
                    reason: "RGBA16Float row byte count overflowed".to_owned(),
                })?;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rohditor camera-native preview source"),
            size: extent((width, height)),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&upload.texels),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(height),
            },
            extent((width, height)),
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(GpuPreviewSource {
            _texture: texture,
            view,
            width,
            height,
            source_orientation: upload.source_orientation,
            white_balance: upload.white_balance,
            white_balance_dynamic: upload.white_balance_dynamic,
            white_balance_gains: upload.white_balance_gains,
            as_shot_white_balance: upload.as_shot_white_balance,
            camera_to_xyz_d65: upload.camera_to_xyz_d65,
            camera_to_linear_rec2020: upload.camera_to_linear_rec2020,
        })
    }

    /// Apply downstream edits to an uploaded base and write an egui-compatible
    /// oriented display texture. Passing the prior frame reuses its GPU textures
    /// whenever dimensions are unchanged.
    pub fn render(
        &self,
        source: &GpuPreviewSource,
        recipe: &EditRecipe,
        reusable: Option<GpuPreviewFrame>,
    ) -> Result<GpuPreviewFrame, GpuPreviewError> {
        recipe
            .validate()
            .map_err(|error| GpuPreviewError::InvalidInput {
                reason: error.to_string(),
            })?;
        if !Self::supports_recipe(recipe) {
            return Err(GpuPreviewError::UnsupportedEdits {
                reason: "HSL and three-way color grading are CPU-only".to_owned(),
            });
        }
        let white_balance_gains = source.resolve_white_balance(recipe.color.white_balance)?;
        let orientation = recipe
            .geometry
            .orientation_override
            .unwrap_or(source.source_orientation);
        let source_width =
            usize::try_from(source.width).map_err(|_| GpuPreviewError::InvalidInput {
                reason: "source width does not fit this platform's usize".to_owned(),
            })?;
        let source_height =
            usize::try_from(source.height).map_err(|_| GpuPreviewError::InvalidInput {
                reason: "source height does not fit this platform's usize".to_owned(),
            })?;
        let orientation_map = OrientationMap::new(source_width, source_height, orientation)
            .map_err(|error| GpuPreviewError::InvalidInput {
                reason: error.to_string(),
            })?;
        let (output_width, output_height) = orientation_map.output_dimensions();
        let (output_width, output_height) = self
            .capabilities
            .validate_dimensions(output_width, output_height)?;
        let workgroups_x = output_width.div_ceil(WORKGROUP_EDGE);
        let workgroups_y = output_height.div_ceil(WORKGROUP_EDGE);
        if workgroups_x > self.capabilities.max_compute_workgroups_per_dimension
            || workgroups_y > self.capabilities.max_compute_workgroups_per_dimension
        {
            return Err(GpuPreviewError::InvalidDimensions {
                width: output_width as usize,
                height: output_height as usize,
                reason: format!(
                    "requires {workgroups_x}x{workgroups_y} workgroups, but the shared device limit is {}",
                    self.capabilities.max_compute_workgroups_per_dimension
                ),
            });
        }
        let source_dimensions = (source.width, source.height);
        let output_dimensions = (output_width, output_height);
        let (mut frame, textures_reused) = match reusable {
            Some(frame) if frame.can_reuse(source_dimensions, output_dimensions) => (frame, true),
            _ => (
                self.create_frame(source_dimensions, output_dimensions),
                false,
            ),
        };
        frame.textures_reused = textures_reused;

        let parameters = build_parameters(
            source_dimensions,
            output_dimensions,
            orientation,
            recipe,
            white_balance_gains,
            source.camera_to_linear_rec2020,
        );
        self.queue
            .write_buffer(&self.parameters, 0, bytemuck::cast_slice(&parameters));
        let light_tone_lut = LightToneLut::new(&recipe.light);
        self.queue.write_buffer(
            &self.light_tone_lut,
            0,
            bytemuck::cast_slice(light_tone_lut.values()),
        );
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rohditor GPU preview bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&frame.working_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&frame.display_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.parameters.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.light_tone_lut.as_entire_binding(),
                },
            ],
        });
        let submitted = Instant::now();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rohditor GPU preview commands"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("rohditor GPU downstream preview pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
        }
        self.queue.submit([encoder.finish()]);
        frame.submission_time = submitted.elapsed();
        let completion = Arc::new(AtomicU64::new(0));
        let completion_writer = Arc::clone(&completion);
        self.queue.on_submitted_work_done(move || {
            let elapsed_nanos = submitted.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
            completion_writer.store(elapsed_nanos.max(1), Ordering::Release);
        });
        frame.queue_completion_nanos = completion;
        Ok(frame)
    }

    /// Explicitly copy a display texture back for tests, diagnostics, or a
    /// one-shot picker sample. Normal UI display must use
    /// [`GpuPreviewFrame::display_view`] directly instead.
    pub fn readback_display(
        &self,
        frame: &GpuPreviewFrame,
    ) -> Result<GpuDisplayReadback, GpuPreviewError> {
        let mut pending = self.begin_display_readback(frame)?;
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| GpuPreviewError::Readback {
                reason: format!("{error:?}"),
            })?;
        pending
            .try_finish()?
            .ok_or_else(|| GpuPreviewError::Readback {
                reason: "GPU readback did not complete after a blocking poll".to_owned(),
            })
    }

    /// Submit a display-texture readback without waiting for completion.
    pub fn begin_display_readback(
        &self,
        frame: &GpuPreviewFrame,
    ) -> Result<GpuDisplayReadbackPending, GpuPreviewError> {
        let (width, height) = frame.output_dimensions;
        let unpadded_bytes_per_row = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or_else(|| GpuPreviewError::Readback {
                reason: "RGBA8 row byte count overflowed".to_owned(),
            })?;
        let alignment = usize::try_from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT).map_err(|_| {
            GpuPreviewError::Readback {
                reason: "GPU row alignment does not fit in memory".to_owned(),
            }
        })?;
        let padded_bytes_per_row = unpadded_bytes_per_row
            .checked_add(alignment - 1)
            .map(|value| value / alignment * alignment)
            .ok_or_else(|| GpuPreviewError::Readback {
                reason: "padded row byte count overflowed".to_owned(),
            })?;
        let buffer_size = u64::try_from(padded_bytes_per_row)
            .ok()
            .and_then(|row| row.checked_mul(u64::from(height)))
            .ok_or_else(|| GpuPreviewError::Readback {
                reason: "readback buffer size overflowed".to_owned(),
            })?;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rohditor GPU preview asynchronous readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rohditor GPU preview readback commands"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &frame.display_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(u32::try_from(padded_bytes_per_row).map_err(|_| {
                        GpuPreviewError::Readback {
                            reason: "padded row byte count does not fit wgpu".to_owned(),
                        }
                    })?),
                    rows_per_image: Some(height),
                },
            },
            extent((width, height)),
        );
        self.queue.submit([encoder.finish()]);
        let (ready_sender, ready_receiver) = mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                drop(ready_sender.send(result));
            });
        Ok(GpuDisplayReadbackPending {
            buffer,
            width,
            height,
            unpadded_bytes_per_row,
            padded_bytes_per_row,
            ready: ready_receiver,
        })
    }

    /// Block until already-submitted queue work completes.
    ///
    /// The desktop UI does not call this method; it is intended for controlled
    /// benchmarks and diagnostics that need a completed wall-time sample.
    pub fn wait_for_queue(&self) -> Result<(), GpuPreviewError> {
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| GpuPreviewError::Synchronization {
                reason: format!("{error:?}"),
            })?;
        Ok(())
    }

    fn create_frame(
        &self,
        source_dimensions: (u32, u32),
        output_dimensions: (u32, u32),
    ) -> GpuPreviewFrame {
        let working_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rohditor GPU linear working preview"),
            size: extent(source_dimensions),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let display_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rohditor GPU sRGB preview"),
            size: extent(output_dimensions),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let working_view = working_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let display_view = display_texture.create_view(&wgpu::TextureViewDescriptor::default());
        GpuPreviewFrame {
            _working_texture: working_texture,
            working_view,
            display_texture,
            display_view,
            source_dimensions,
            output_dimensions,
            submission_time: Duration::ZERO,
            queue_completion_nanos: Arc::new(AtomicU64::new(0)),
            textures_reused: false,
        }
    }
}

fn extent((width, height): (u32, u32)) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}

fn upload_dimensions(
    source_width: usize,
    source_height: usize,
) -> Result<(u32, u32), GpuPreviewError> {
    let width = u32::try_from(source_width).map_err(|_| GpuPreviewError::InvalidDimensions {
        width: source_width,
        height: source_height,
        reason: "width does not fit into the RGBA16Float upload extent".to_owned(),
    })?;
    let height = u32::try_from(source_height).map_err(|_| GpuPreviewError::InvalidDimensions {
        width: source_width,
        height: source_height,
        reason: "height does not fit into the RGBA16Float upload extent".to_owned(),
    })?;
    if width == 0 || height == 0 {
        return Err(GpuPreviewError::InvalidDimensions {
            width: source_width,
            height: source_height,
            reason: "GPU upload textures must have non-zero dimensions".to_owned(),
        });
    }
    Ok((width, height))
}

fn pack_rgba16f(
    data: &[f32],
    width: usize,
    height: usize,
    row_stride: usize,
    cancellation: &CancellationToken,
) -> Result<Vec<u16>, GpuPreviewError> {
    let width_samples = width
        .checked_mul(3)
        .ok_or_else(|| GpuPreviewError::InvalidDimensions {
            width,
            height,
            reason: "RGB source row size overflowed".to_owned(),
        })?;
    let texels = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| GpuPreviewError::InvalidDimensions {
            width,
            height,
            reason: "RGBA16Float upload allocation overflowed".to_owned(),
        })?;
    let mut packed = Vec::with_capacity(texels);
    for row in data.chunks(row_stride).take(height) {
        if cancellation.is_cancelled() {
            return Err(GpuPreviewError::Cancelled);
        }
        if row.len() < width_samples {
            return Err(GpuPreviewError::InvalidInput {
                reason: "linear base row stride is shorter than its active RGB samples".to_owned(),
            });
        }
        for pixel in row[..width_samples].chunks_exact(3) {
            let mut encoded = [0_u16; 4];
            for (channel, value) in pixel.iter().copied().enumerate() {
                if !value.is_finite() {
                    return Err(GpuPreviewError::InvalidInput {
                        reason: "linear base contains non-finite camera-native samples".to_owned(),
                    });
                }
                let converted = f16::from_f32(value);
                if !converted.is_finite() {
                    return Err(GpuPreviewError::InvalidInput {
                        reason: "linear base contains samples outside the RGBA16Float range"
                            .to_owned(),
                    });
                }
                encoded[channel] = converted.to_bits();
            }
            encoded[3] = f16::ONE.to_bits();
            packed.extend(encoded);
        }
    }
    if packed.len() != texels {
        return Err(GpuPreviewError::InvalidInput {
            reason: "linear base does not contain the declared number of rows".to_owned(),
        });
    }
    Ok(packed)
}

fn build_parameters(
    source_dimensions: (u32, u32),
    output_dimensions: (u32, u32),
    orientation: RawOrientation,
    recipe: &EditRecipe,
    white_balance_gains: WhiteBalanceGains,
    camera_to_linear_rec2020: Matrix3,
) -> [u32; PARAMETER_WORDS] {
    let rec2020_to_srgb = LINEAR_REC2020_TO_XYZ_D65.then(XYZ_D65_TO_LINEAR_SRGB);
    let mut words = [0_u32; PARAMETER_WORDS];
    words[0] = recipe.light.exposure_ev.exp2().to_bits();
    words[1] = recipe.light.contrast.exp2().to_bits();
    words[2] = recipe.color.saturation.to_bits();
    words[3] = recipe.color.vibrance.to_bits();
    words[4] = recipe.light.highlights.to_bits();
    words[5] = recipe.light.shadows.to_bits();
    words[6] = recipe.light.whites.to_bits();
    words[7] = recipe.light.blacks.to_bits();
    words[8] = recipe.light.tone_curve.shadows.to_bits();
    words[9] = recipe.light.tone_curve.darks.to_bits();
    words[10] = recipe.light.tone_curve.lights.to_bits();
    words[11] = recipe.light.tone_curve.highlights.to_bits();
    words[12] = orientation_code(orientation);
    words[13] = source_dimensions.0;
    words[14] = source_dimensions.1;
    words[15] = output_dimensions.0;
    words[16] = output_dimensions.1;
    words[20] = white_balance_gains.red.to_bits();
    words[21] = white_balance_gains.green.to_bits();
    words[22] = white_balance_gains.blue.to_bits();
    write_matrix_rows(&mut words[24..36], camera_to_linear_rec2020);
    write_matrix_rows(&mut words[36..48], rec2020_to_srgb);
    words
}

fn write_matrix_rows(destination: &mut [u32], matrix: Matrix3) {
    for (row_index, row) in matrix.values().into_iter().enumerate() {
        let offset = row_index * 4;
        destination[offset] = row[0].to_bits();
        destination[offset + 1] = row[1].to_bits();
        destination[offset + 2] = row[2].to_bits();
        destination[offset + 3] = 0;
    }
}

fn orientation_code(orientation: RawOrientation) -> u32 {
    match orientation {
        RawOrientation::Normal | RawOrientation::Unknown => 0,
        RawOrientation::HorizontalFlip => 1,
        RawOrientation::Rotate180 => 2,
        RawOrientation::VerticalFlip => 3,
        RawOrientation::Transpose => 4,
        RawOrientation::Rotate90 => 5,
        RawOrientation::Transverse => 6,
        RawOrientation::Rotate270 => 7,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

    use rohditor_core::{CpuPipeline, PreviewOptions, ToneCurve};
    use rohditor_raw::{
        CameraColorMatrix, CaptureMetadata, CfaPattern, LevelPattern, PhotometricInterpretation,
        RawDecoder, RawFileInfo, RawFrame, RawlerDecoder,
    };

    use super::*;

    #[test]
    fn shader_parameters_match_the_cpu_orientation_and_transform_contract() {
        let mut recipe = EditRecipe::default();
        recipe.light.exposure_ev = 1.0;
        recipe.light.contrast = -0.5;
        recipe.light.highlights = 0.2;
        recipe.light.shadows = -0.3;
        recipe.light.whites = 0.1;
        recipe.light.blacks = -0.1;
        recipe.light.tone_curve.shadows = 0.05;
        recipe.light.tone_curve.highlights = -0.04;
        recipe.color.saturation = 1.25;
        recipe.color.vibrance = 0.4;
        recipe.geometry.orientation_override = Some(RawOrientation::Rotate90);
        let parameters = build_parameters(
            (7, 5),
            (5, 7),
            RawOrientation::Rotate90,
            &recipe,
            WhiteBalanceGains {
                red: 1.0,
                green: 1.0,
                blue: 1.0,
            },
            Matrix3::identity(),
        );
        assert_eq!(parameters[12], 5);
        assert_eq!(parameters[13..17], [7, 5, 5, 7]);
        assert_eq!(f32::from_bits(parameters[0]), 2.0);
        assert_eq!(f32::from_bits(parameters[1]), 2_f32.powf(-0.5));
        assert_eq!(f32::from_bits(parameters[2]), 1.25);
        assert_eq!(f32::from_bits(parameters[3]), 0.4);
        assert_eq!(f32::from_bits(parameters[4]), 0.2);
        assert_eq!(f32::from_bits(parameters[7]), -0.1);
        assert_eq!(f32::from_bits(parameters[8]), 0.05);
        assert_eq!(f32::from_bits(parameters[11]), -0.04);
    }

    #[test]
    fn packs_rgb_rows_without_copying_padding() {
        let data = vec![
            0.0, 0.5, 1.0, 0.25, 0.75, 0.125, 99.0, 99.0, 99.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 99.0,
            99.0, 99.0,
        ];
        let packed =
            pack_rgba16f(&data, 2, 2, 9, &CancellationToken::new()).expect("valid padded rows");
        assert_eq!(packed.len(), 16);
        assert_eq!(f16::from_bits(packed[0]).to_f32(), 0.0);
        assert_eq!(f16::from_bits(packed[4]).to_f32(), 0.25);
        assert!((f16::from_bits(packed[8]).to_f32() - 0.1).abs() < 0.001);
        assert_eq!(f16::from_bits(packed[15]), f16::ONE);
    }

    #[test]
    fn rejects_source_values_that_cannot_be_represented_by_rgba16float() {
        let non_finite = [f32::NAN, 0.5, 0.5];
        assert!(pack_rgba16f(&non_finite, 1, 1, 3, &CancellationToken::new()).is_err());

        let out_of_range = [f32::MAX, 0.5, 0.5];
        assert!(pack_rgba16f(&out_of_range, 1, 1, 3, &CancellationToken::new()).is_err());
    }

    #[test]
    fn gpu_capability_predicate_rejects_edits_the_shader_does_not_implement() {
        let mut recipe = EditRecipe::default();
        assert!(GpuPreviewProcessor::supports_recipe(&recipe));

        recipe.color.hsl.channels[0].hue = 0.1;
        assert!(!GpuPreviewProcessor::supports_recipe(&recipe));

        recipe = EditRecipe::default();
        recipe.color.grading.highlights[2] = 0.1;
        assert!(!GpuPreviewProcessor::supports_recipe(&recipe));
    }

    #[test]
    #[ignore = "requires a locally available Vulkan-capable GPU; run cargo test -p rohditor-gpu -- --ignored"]
    fn gpu_preview_matches_cpu_reference_for_every_exif_orientation() {
        let _gpu_test_guard = gpu_test_guard();
        let Some(processor) = gpu_test_processor() else {
            return;
        };
        let mut recipe = EditRecipe::default();
        recipe.light.exposure_ev = 0.8;
        recipe.light.contrast = -0.35;
        recipe.color.saturation = 1.4;
        for orientation in all_test_orientations() {
            let frame = synthetic_frame(orientation);
            let options = PreviewOptions {
                max_long_edge: 8,
                ..PreviewOptions::default()
            };
            let reconstructed = CpuPipeline
                .prepare_preview_reconstruction(&frame, options)
                .expect("synthetic reconstruction should develop");
            let source = processor
                .upload_prepared(
                    GpuPreviewUpload::from_reconstructed_preview(
                        &reconstructed,
                        recipe.color.white_balance,
                    )
                    .expect("camera-native source upload should work"),
                )
                .expect("camera-native source should upload");
            let gpu = processor
                .render(&source, &recipe, None)
                .expect("GPU preview should render");
            assert_gpu_reconstructed_frame_matches_cpu(&processor, &frame, options, &recipe, &gpu);
        }
    }

    #[test]
    #[ignore = "requires a locally available Vulkan-capable GPU; run cargo test -p rohditor-gpu -- --ignored"]
    fn gpu_preview_control_matrix_reports_cpu_parity() {
        let _gpu_test_guard = gpu_test_guard();
        let Some(processor) = gpu_test_processor() else {
            return;
        };
        let controls = gpu_control_matrix();
        let options = PreviewOptions {
            max_long_edge: 16,
            ..PreviewOptions::default()
        };
        let mut aggregate = vec![GpuParityStats::default(); controls.len()];
        let mut queue_samples = Vec::with_capacity(controls.len() * 8);
        let mut submission_samples = Vec::with_capacity(controls.len() * 8);

        for orientation in all_test_orientations() {
            let frame = synthetic_frame(orientation);
            let reconstructed = CpuPipeline
                .prepare_preview_reconstruction(&frame, options)
                .expect("synthetic reconstruction should develop");
            let source = processor
                .upload_prepared(
                    GpuPreviewUpload::from_reconstructed_preview(
                        &reconstructed,
                        WhiteBalance::AsShot,
                    )
                    .expect("camera-native source upload should work"),
                )
                .expect("camera-native source should upload");
            let mut reusable = None;

            for (control_index, (label, recipe)) in controls.iter().enumerate() {
                let gpu = processor
                    .render(&source, recipe, reusable.take())
                    .unwrap_or_else(|error| panic!("{label} should render: {error}"));
                processor
                    .wait_for_queue()
                    .expect("GPU control matrix dispatch should complete");
                let stats =
                    gpu_reconstructed_frame_parity_stats(&processor, &frame, options, recipe, &gpu);
                assert!(
                    stats.max_abs <= 2,
                    "{label} orientation {orientation:?}: max CPU/GPU difference was {} sRGB codes at pixel {}, channel {} (CPU {}, GPU {}; mean {:.3})",
                    stats.max_abs,
                    stats.max_pixel,
                    stats.max_channel,
                    stats.max_cpu,
                    stats.max_gpu,
                    stats.mean_abs()
                );
                aggregate[control_index].merge(stats);
                queue_samples.push(
                    gpu.queue_completion_time()
                        .expect("queue callback should publish a completion time"),
                );
                submission_samples.push(gpu.submission_time());
                reusable = Some(gpu);
            }
        }

        queue_samples.sort_unstable();
        submission_samples.sort_unstable();
        let queue_median = queue_samples[queue_samples.len() / 2];
        let queue_max = queue_samples.last().copied().unwrap_or(Duration::ZERO);
        let submission_median = submission_samples[submission_samples.len() / 2];
        for ((label, _), stats) in controls.iter().zip(&aggregate) {
            eprintln!(
                "GPU control parity: {label}, samples={}, max_abs={}, mean_abs={:.3}, over_two={}",
                stats.samples,
                stats.max_abs,
                stats.mean_abs(),
                stats.over_two,
            );
        }
        eprintln!(
            "GPU control matrix: {} controls x 8 orientations, queue median={:.3} ms, max={:.3} ms, encode+submit median={:.3} ms",
            controls.len(),
            queue_median.as_secs_f64() * 1_000.0,
            queue_max.as_secs_f64() * 1_000.0,
            submission_median.as_secs_f64() * 1_000.0,
        );
    }

    #[test]
    #[ignore = "requires a locally available Vulkan-capable GPU; run cargo test -p rohditor-gpu -- --ignored"]
    fn camera_native_gpu_source_applies_white_balance_without_reupload() {
        let _gpu_test_guard = gpu_test_guard();
        let Some(processor) = gpu_test_processor() else {
            return;
        };
        let frame = synthetic_frame(RawOrientation::Normal);
        let options = PreviewOptions {
            max_long_edge: 8,
            ..PreviewOptions::default()
        };
        let reconstructed = CpuPipeline
            .prepare_preview_reconstruction(&frame, options)
            .expect("synthetic reconstruction should develop");
        let source = processor
            .upload_prepared(
                GpuPreviewUpload::from_reconstructed_preview(&reconstructed, WhiteBalance::AsShot)
                    .expect("camera-native source upload should work"),
            )
            .expect("camera-native source should upload");
        let first = processor
            .render(&source, &EditRecipe::default(), None)
            .expect("initial GPU preview should render");
        let mut adjusted_recipe = EditRecipe::default();
        adjusted_recipe.color.white_balance = WhiteBalance::ManualMultipliers {
            red: 0.75,
            green: 1.0,
            blue: 1.35,
        };
        adjusted_recipe.light.exposure_ev = 0.4;
        let second = processor
            .render(&source, &adjusted_recipe, Some(first))
            .expect("white balance should be a resident GPU edit");
        assert!(second.textures_reused());
        assert_gpu_reconstructed_frame_matches_cpu(
            &processor,
            &frame,
            options,
            &adjusted_recipe,
            &second,
        );
    }

    #[test]
    #[ignore = "requires a locally available Vulkan-capable GPU; run cargo test -p rohditor-gpu -- --ignored"]
    fn resident_gpu_source_accepts_downstream_edits_without_a_reupload() {
        let _gpu_test_guard = gpu_test_guard();
        let Some(processor) = gpu_test_processor() else {
            return;
        };
        let frame = synthetic_frame(RawOrientation::Normal);
        let initial_recipe = EditRecipe::default();
        let base = CpuPipeline
            .prepare_preview_base(
                &frame,
                &initial_recipe,
                PreviewOptions {
                    max_long_edge: 8,
                    ..PreviewOptions::default()
                },
            )
            .expect("synthetic base should develop");
        let source = processor
            .upload_base(&base)
            .expect("base upload should work");
        let first = processor
            .render(&source, &initial_recipe, None)
            .expect("initial GPU preview should render");
        assert!(!first.textures_reused());
        let mut adjusted_recipe = EditRecipe::default();
        adjusted_recipe.light.exposure_ev = 1.1;
        adjusted_recipe.light.contrast = 0.3;
        adjusted_recipe.color.saturation = 0.8;
        let second = processor
            .render(&source, &adjusted_recipe, Some(first))
            .expect("resident source should render downstream edits");
        assert!(second.textures_reused());
        assert_gpu_frame_matches_cpu(&processor, &base, &adjusted_recipe, &second);
    }

    #[test]
    #[ignore = "requires the private Sony ARW corpus and a Vulkan-capable GPU"]
    fn private_arw_gpu_preview_matches_cpu_reference_within_two_srgb_codes() {
        let _gpu_test_guard = gpu_test_guard();
        let Some(processor) = gpu_test_processor() else {
            return;
        };
        let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/private/DSC00851.ARW");
        if !source.is_file() {
            eprintln!(
                "skipping private GPU parity test because {} is unavailable",
                source.display()
            );
            return;
        }
        let decoder = RawlerDecoder::default();
        let mut session = decoder.open(&source).expect("private ARW should open");
        let frame = session.decode().expect("private ARW should decode");
        let mut recipe = EditRecipe::default();
        recipe.light.exposure_ev = 0.65;
        recipe.light.contrast = -0.2;
        recipe.color.saturation = 1.15;
        recipe.color.white_balance = WhiteBalance::ManualMultipliers {
            red: 0.85,
            green: 1.0,
            blue: 1.15,
        };
        let options = PreviewOptions::default();
        let reconstructed = CpuPipeline
            .prepare_preview_reconstruction(&frame, options)
            .expect("private preview reconstruction should develop");
        let source = processor
            .upload_prepared(
                GpuPreviewUpload::from_reconstructed_preview(
                    &reconstructed,
                    recipe.color.white_balance,
                )
                .expect("private camera-native source should pack"),
            )
            .expect("private camera-native source should upload");
        let gpu = processor
            .render(&source, &recipe, None)
            .expect("private GPU preview should render");
        let stats =
            gpu_reconstructed_frame_parity_stats(&processor, &frame, options, &recipe, &gpu);
        assert!(
            stats.max_abs <= 2,
            "private ARW maximum CPU/GPU difference was {} sRGB codes (mean {:.3})",
            stats.max_abs,
            stats.mean_abs()
        );
        eprintln!(
            "GPU private ARW parity: {}x{}, max_abs={}, mean_abs={:.3}, over_two={}",
            gpu.output_dimensions().0,
            gpu.output_dimensions().1,
            stats.max_abs,
            stats.mean_abs(),
            stats.over_two,
        );
    }

    #[test]
    #[ignore = "requires the private Sony ARW corpus and a Vulkan-capable GPU"]
    fn private_arw_gpu_control_matrix_reports_cpu_parity() {
        let _gpu_test_guard = gpu_test_guard();
        let Some(processor) = gpu_test_processor() else {
            return;
        };
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/private/DSC00851.ARW");
        if !path.is_file() {
            eprintln!(
                "skipping private GPU control matrix because {} is unavailable",
                path.display()
            );
            return;
        }
        let decoder = RawlerDecoder::default();
        let mut session = decoder.open(&path).expect("private ARW should open");
        let frame = session.decode().expect("private ARW should decode");
        let options = PreviewOptions::default();
        let reconstructed = CpuPipeline
            .prepare_preview_reconstruction(&frame, options)
            .expect("private preview reconstruction should develop");
        let source = processor
            .upload_prepared(
                GpuPreviewUpload::from_reconstructed_preview(&reconstructed, WhiteBalance::AsShot)
                    .expect("private camera-native source should pack"),
            )
            .expect("private camera-native source should upload");
        let controls = gpu_control_matrix();
        let mut aggregate = vec![GpuParityStats::default(); controls.len()];
        let mut queue_samples = Vec::with_capacity(controls.len());
        let mut submission_samples = Vec::with_capacity(controls.len());
        let mut reusable = None;

        for (control_index, (label, recipe)) in controls.iter().enumerate() {
            let gpu = processor
                .render(&source, recipe, reusable.take())
                .unwrap_or_else(|error| panic!("{label} should render: {error}"));
            processor
                .wait_for_queue()
                .expect("private GPU control matrix dispatch should complete");
            let stats =
                gpu_reconstructed_frame_parity_stats(&processor, &frame, options, recipe, &gpu);
            assert!(
                stats.max_abs <= 2,
                "private {label}: max CPU/GPU difference was {} sRGB codes at pixel {}, channel {} (CPU {}, GPU {}; mean {:.3})",
                stats.max_abs,
                stats.max_pixel,
                stats.max_channel,
                stats.max_cpu,
                stats.max_gpu,
                stats.mean_abs()
            );
            aggregate[control_index].merge(stats);
            queue_samples.push(
                gpu.queue_completion_time()
                    .expect("queue callback should publish a completion time"),
            );
            submission_samples.push(gpu.submission_time());
            reusable = Some(gpu);
        }

        queue_samples.sort_unstable();
        submission_samples.sort_unstable();
        for ((label, _), stats) in controls.iter().zip(&aggregate) {
            eprintln!(
                "GPU private control parity: {label}, samples={}, max_abs={}, mean_abs={:.3}, over_two={}",
                stats.samples,
                stats.max_abs,
                stats.mean_abs(),
                stats.over_two,
            );
        }
        eprintln!(
            "GPU private control matrix: {}x{}, {} controls, queue median={:.3} ms, max={:.3} ms, encode+submit median={:.3} ms",
            reusable
                .as_ref()
                .map_or(0, |frame| frame.output_dimensions().0),
            reusable
                .as_ref()
                .map_or(0, |frame| frame.output_dimensions().1),
            controls.len(),
            queue_samples[queue_samples.len() / 2].as_secs_f64() * 1_000.0,
            queue_samples
                .last()
                .map_or(0.0, |sample| sample.as_secs_f64() * 1_000.0),
            submission_samples[submission_samples.len() / 2].as_secs_f64() * 1_000.0,
        );
    }

    #[test]
    #[ignore = "requires the private Sony ARW corpus and a Vulkan-capable GPU"]
    fn private_arw_cached_gpu_adjustment_performance_is_reported() {
        let _gpu_test_guard = gpu_test_guard();
        let Some(processor) = gpu_test_processor() else {
            return;
        };
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/private/DSC00851.ARW");
        if !path.is_file() {
            eprintln!(
                "skipping private GPU measurement because {} is unavailable",
                path.display()
            );
            return;
        }
        let decoder = RawlerDecoder::default();
        let mut session = decoder.open(&path).expect("private ARW should open");
        let frame = session.decode().expect("private ARW should decode");
        let options = PreviewOptions::default();
        let reconstructed = CpuPipeline
            .prepare_preview_reconstruction(&frame, options)
            .expect("private preview reconstruction should develop");
        let source = processor
            .upload_prepared(
                GpuPreviewUpload::from_reconstructed_preview(&reconstructed, WhiteBalance::AsShot)
                    .expect("private camera-native source should pack"),
            )
            .expect("private camera-native source should upload");
        let mut gpu_frame = processor
            .render(&source, &EditRecipe::default(), None)
            .expect("initial GPU preview should render");
        processor
            .wait_for_queue()
            .expect("initial GPU preview should complete");
        let mut samples = Vec::new();

        for index in 1..=40 {
            let mut recipe = EditRecipe::default();
            recipe.color.white_balance = WhiteBalance::ManualMultipliers {
                red: 0.8 + index as f32 / 200.0,
                green: 1.0,
                blue: 1.2 - index as f32 / 200.0,
            };
            recipe.light.exposure_ev = index as f32 / 20.0 - 1.0;
            recipe.light.contrast = index as f32 / 80.0;
            recipe.color.saturation = 0.75 + index as f32 / 80.0;
            gpu_frame = processor
                .render(&source, &recipe, Some(gpu_frame))
                .expect("cached GPU adjustment should render");
            processor
                .wait_for_queue()
                .expect("cached GPU adjustment should complete");
            assert!(gpu_frame.textures_reused());
            samples.push(
                gpu_frame
                    .queue_completion_time()
                    .expect("queue callback should publish a completion time"),
            );
        }

        samples.sort_unstable();
        let median = samples[samples.len() / 2];
        let worst = samples.last().copied().unwrap_or(Duration::ZERO);
        let resident_bytes = source
            .estimated_bytes()
            .saturating_add(gpu_frame.estimated_bytes());
        eprintln!(
            "GPU resident-source measurement: {}x{}, completion median={:.3} ms, max={:.3} ms, encode+submit={:.3} ms, textures={:.1} MiB",
            gpu_frame.output_dimensions().0,
            gpu_frame.output_dimensions().1,
            median.as_secs_f64() * 1_000.0,
            worst.as_secs_f64() * 1_000.0,
            gpu_frame.submission_time().as_secs_f64() * 1_000.0,
            resident_bytes as f64 / 1_048_576.0,
        );
    }

    /// The reference RADV stack is reliable for these tests one device at a
    /// time, but concurrent device creation can destabilize the driver. Keep
    /// this guard local to opt-in hardware tests; production preview work is
    fn gpu_test_guard() -> MutexGuard<'static, ()> {
        static GPU_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        GPU_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn gpu_test_processor() -> Option<GpuPreviewProcessor> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .expect("a Vulkan adapter is required for the GPU parity test");
        let adapter_info = adapter.get_info();
        if matches!(adapter_info.device_type, wgpu::DeviceType::Cpu) {
            eprintln!("skipping GPU parity test because Vulkan selected a CPU rasterizer");
            return None;
        }
        eprintln!(
            "GPU parity test uses {} ({:?}, {:?})",
            adapter_info.name, adapter_info.backend, adapter_info.device_type
        );
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("rohditor GPU parity test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            ..wgpu::DeviceDescriptor::default()
        }))
        .expect("could not create a Vulkan device for the GPU parity test");
        Some(
            GpuPreviewProcessor::new(&adapter, &device, &queue, wgpu::TextureFormat::Rgba8Unorm)
                .expect("the selected GPU must support the Rohditor preview formats"),
        )
    }

    fn assert_gpu_frame_matches_cpu(
        processor: &GpuPreviewProcessor,
        base: &DemosaicedBase,
        recipe: &EditRecipe,
        gpu: &GpuPreviewFrame,
    ) {
        let cpu = CpuPipeline
            .render_preview_from_base(base, recipe, PreviewOptions::default().render.output_policy)
            .expect("CPU reference should render")
            .image;
        let readback = processor
            .readback_display(gpu)
            .expect("GPU display texture should be readable for parity testing");
        let stats = measure_u8_parity((cpu.width(), cpu.height()), cpu.data(), &readback);
        assert!(
            stats.max_abs <= 2,
            "maximum CPU/GPU difference was {} sRGB codes (mean {:.3})",
            stats.max_abs,
            stats.mean_abs()
        );
    }

    fn assert_gpu_reconstructed_frame_matches_cpu(
        processor: &GpuPreviewProcessor,
        frame: &RawFrame,
        options: PreviewOptions,
        recipe: &EditRecipe,
        gpu: &GpuPreviewFrame,
    ) {
        let stats = gpu_reconstructed_frame_parity_stats(processor, frame, options, recipe, gpu);
        assert!(
            stats.max_abs <= 2,
            "maximum CPU/GPU difference was {} sRGB codes (mean {:.3})",
            stats.max_abs,
            stats.mean_abs()
        );
    }

    fn gpu_reconstructed_frame_parity_stats(
        processor: &GpuPreviewProcessor,
        frame: &RawFrame,
        options: PreviewOptions,
        recipe: &EditRecipe,
        gpu: &GpuPreviewFrame,
    ) -> GpuParityStats {
        let cpu = CpuPipeline
            .render_preview(frame, recipe, options)
            .expect("CPU reference should render")
            .image;
        let readback = processor
            .readback_display(gpu)
            .expect("GPU display texture should be readable for parity testing");
        measure_u8_parity((cpu.width(), cpu.height()), cpu.data(), &readback)
    }

    #[derive(Debug, Default, Clone, Copy)]
    struct GpuParityStats {
        max_abs: u8,
        max_pixel: u64,
        max_channel: u8,
        max_cpu: u8,
        max_gpu: u8,
        total_abs: u64,
        samples: u64,
        over_two: u64,
    }

    impl GpuParityStats {
        fn merge(&mut self, other: Self) {
            if other.max_abs > self.max_abs {
                self.max_abs = other.max_abs;
                self.max_pixel = other.max_pixel;
                self.max_channel = other.max_channel;
                self.max_cpu = other.max_cpu;
                self.max_gpu = other.max_gpu;
            }
            self.total_abs = self.total_abs.saturating_add(other.total_abs);
            self.samples = self.samples.saturating_add(other.samples);
            self.over_two = self.over_two.saturating_add(other.over_two);
        }

        fn mean_abs(self) -> f64 {
            if self.samples == 0 {
                0.0
            } else {
                self.total_abs as f64 / self.samples as f64
            }
        }
    }

    fn measure_u8_parity(
        dimensions: (usize, usize),
        cpu: &[u8],
        gpu: &GpuDisplayReadback,
    ) -> GpuParityStats {
        let pixel_count = dimensions
            .0
            .checked_mul(dimensions.1)
            .expect("test dimensions should fit");
        assert_eq!(cpu.len(), pixel_count * 3);
        assert_eq!(
            gpu.rgba.len(),
            pixel_count * 4,
            "GPU readback dimensions should match its byte length"
        );
        assert_eq!(
            (
                usize::try_from(gpu.width).expect("fits"),
                usize::try_from(gpu.height).expect("fits"),
            ),
            dimensions
        );
        let mut stats = GpuParityStats::default();
        for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu
            .chunks_exact(3)
            .zip(gpu.rgba.chunks_exact(4))
            .enumerate()
        {
            for channel in 0..3 {
                let difference = i16::from(cpu_pixel[channel]) - i16::from(gpu_pixel[channel]);
                let absolute = difference.unsigned_abs() as u8;
                if absolute > stats.max_abs {
                    stats.max_abs = absolute;
                    stats.max_pixel = pixel_index as u64;
                    stats.max_channel = channel as u8;
                    stats.max_cpu = cpu_pixel[channel];
                    stats.max_gpu = gpu_pixel[channel];
                }
                stats.total_abs += u64::from(absolute);
                stats.samples += 1;
                if absolute > 2 {
                    stats.over_two += 1;
                }
            }
        }
        stats
    }

    fn gpu_control_matrix() -> Vec<(&'static str, EditRecipe)> {
        let mut controls = Vec::new();
        let mut recipe = EditRecipe::default();
        recipe.light.exposure_ev = 0.8;
        controls.push(("exposure", recipe));

        let mut recipe = EditRecipe::default();
        recipe.light.contrast = -0.35;
        controls.push(("contrast", recipe));

        let mut recipe = EditRecipe::default();
        recipe.light.highlights = 0.6;
        controls.push(("highlights", recipe));

        let mut recipe = EditRecipe::default();
        recipe.light.shadows = -0.55;
        controls.push(("shadows", recipe));

        let mut recipe = EditRecipe::default();
        recipe.light.whites = 0.45;
        controls.push(("whites", recipe));

        let mut recipe = EditRecipe::default();
        recipe.light.blacks = -0.45;
        controls.push(("blacks", recipe));

        let mut recipe = EditRecipe::default();
        recipe.light.tone_curve = ToneCurve {
            shadows: 0.15,
            darks: -0.1,
            lights: 0.08,
            highlights: -0.06,
        };
        controls.push(("tone_curve", recipe));

        let mut recipe = EditRecipe::default();
        recipe.color.saturation = 1.35;
        controls.push(("saturation", recipe));

        let mut recipe = EditRecipe::default();
        recipe.color.vibrance = 0.65;
        controls.push(("vibrance", recipe));

        let mut recipe = EditRecipe::default();
        recipe.color.white_balance = WhiteBalance::ManualMultipliers {
            red: 0.78,
            green: 1.0,
            blue: 1.28,
        };
        controls.push(("white_balance_manual", recipe));

        let mut recipe = EditRecipe::default();
        recipe.color.white_balance = WhiteBalance::TemperatureTint {
            temperature: 4_800.0,
            tint: 0.2,
        };
        controls.push(("white_balance_temperature", recipe));

        let mut recipe = EditRecipe::default();
        recipe.light.exposure_ev = 0.65;
        recipe.light.contrast = -0.25;
        recipe.light.highlights = 0.35;
        recipe.light.shadows = -0.3;
        recipe.light.whites = 0.25;
        recipe.light.blacks = -0.2;
        recipe.light.tone_curve = ToneCurve {
            shadows: 0.08,
            darks: -0.05,
            lights: 0.06,
            highlights: -0.04,
        };
        recipe.color.saturation = 1.2;
        recipe.color.vibrance = 0.35;
        recipe.color.white_balance = WhiteBalance::ManualMultipliers {
            red: 0.84,
            green: 1.0,
            blue: 1.16,
        };
        controls.push(("combined_supported", recipe));
        controls
    }

    fn all_test_orientations() -> [RawOrientation; 8] {
        [
            RawOrientation::Normal,
            RawOrientation::HorizontalFlip,
            RawOrientation::Rotate180,
            RawOrientation::VerticalFlip,
            RawOrientation::Transpose,
            RawOrientation::Rotate90,
            RawOrientation::Transverse,
            RawOrientation::Rotate270,
        ]
    }

    fn synthetic_frame(orientation: RawOrientation) -> RawFrame {
        let width = 8;
        let height = 6;
        let mosaic = (0..width * height)
            .map(|index| match (index / width % 2, index % width % 2) {
                (0, 0) => 18_000 + u16::try_from(index * 137).expect("small fixture"),
                (1, 1) => 35_000 + u16::try_from(index * 89).expect("small fixture"),
                _ => 26_000 + u16::try_from(index * 101).expect("small fixture"),
            })
            .collect::<Vec<_>>();
        RawFrame {
            info: RawFileInfo {
                format: "synthetic".to_owned(),
                make: "Rohditor".to_owned(),
                model: "GPU fixture".to_owned(),
                clean_make: "Rohditor".to_owned(),
                clean_model: "GPU fixture".to_owned(),
                source_size_bytes: 4,
                source_identity: None,
                width,
                height,
                components_per_pixel: 1,
                source_bits_per_sample: Some(16),
                decoded_bits_per_sample: 16,
                compression: None,
                active_area: None,
                crop_area: None,
                photometric_interpretation: PhotometricInterpretation::Cfa {
                    pattern: CfaPattern {
                        name: "RGGB".to_owned(),
                        width: 2,
                        height: 2,
                    },
                },
                black_levels: LevelPattern {
                    values: vec![0.0; 4],
                    repeat_width: 2,
                    repeat_height: 2,
                    components_per_pixel: 1,
                },
                white_levels: vec![u16::MAX.into()],
                as_shot_white_balance: [Some(1.0); 4],
                xyz_to_camera: [[0.0; 3]; 4],
                color_matrices: vec![CameraColorMatrix {
                    illuminant: "D65".to_owned(),
                    values: vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
                }],
                orientation,
                capture: CaptureMetadata::default(),
                embedded_preview: None,
            },
            row_stride: width,
            mosaic: Arc::from(mosaic),
        }
    }
}
