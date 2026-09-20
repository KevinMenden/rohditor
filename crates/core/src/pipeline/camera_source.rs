//! Typed full-resolution camera-native preparation, capture, and CPU completion.
use super::*;
use crate::{CaptureSharpeningContract, CaptureSharpeningProvenance};
use rohditor_raw::RawFileInfo;

/// Immutable unsharpened camera RGB. No WB, optics, or reduction has run.
#[derive(Debug, Clone)]
pub struct DemosaicedCameraSource {
    identity: CameraSourceIdentity,
    image: Arc<LinearRgbImage<f32>>,
    info: RawFileInfo,
    recipe: EditRecipe,
    options: PreviewOptions,
    calibration: CameraCalibration,
    highlight_diagnostics: HighlightDiagnostics,
    decoded_raw_bytes: usize,
    normalized_mosaic_bytes: usize,
    highlight_scratch_bytes: usize,
    timings: StageTimings,
    gains: WhiteBalanceGains,
}

/// Capture-completed source; only this state can enter optics/reduction.
#[derive(Debug, Clone)]
pub struct CapturedCameraSource {
    source: DemosaicedCameraSource,
    stage: super::super::capture::CaptureStage,
}

impl DemosaicedCameraSource {
    pub fn buffer_bytes(&self) -> usize {
        std::mem::size_of_val(self.image.data())
    }

    /// Reuse demosaic only if every reconstruction dependency still matches.
    pub fn with_recipe(
        mut self,
        recipe: &EditRecipe,
        options: PreviewOptions,
    ) -> Result<Self, PipelineError> {
        recipe.validate()?;
        validate_optics_crop(options.render.raw_crop_policy, recipe)?;
        if self.options.render.raw_crop_policy != options.render.raw_crop_policy
            || self.options.render.demosaic != options.render.demosaic
            || !highlight_adjustments_match(self.recipe.raw.highlights, recipe.raw.highlights)
            || (recipe.raw.highlights.method == HighlightMethod::Clip
                && (self.recipe.color.white_balance != recipe.color.white_balance
                    || self.recipe.color.camera_profile != recipe.color.camera_profile))
        {
            return Err(PipelineError::InvalidRecipe {
                field: "raw.highlights",
                reason: "camera source reconstruction dependencies changed".into(),
            });
        }
        self.gains = resolve_camera_colour(
            &self.calibration,
            &recipe.color.camera_profile,
            recipe.color.white_balance,
        )?
        .white_balance_gains;
        self.recipe = recipe.clone();
        self.options = options;
        self.timings = StageTimings::default();
        Ok(self)
    }
    pub fn image(&self) -> &LinearRgbImage<f32> {
        &self.image
    }
    pub fn decoded_raw_bytes(&self) -> usize {
        self.decoded_raw_bytes
    }
    pub fn capture_contract(&self) -> Result<CaptureSharpeningContract, PipelineError> {
        CaptureSharpeningContract::new(
            self.recipe.capture_sharpening,
            super::super::capture::ceilings(&self.recipe, self.gains),
        )
    }

    pub fn capture_cpu(
        mut self,
        cancellation: &CancellationToken,
    ) -> Result<CapturedCameraSource, PipelineError> {
        let bytes = self
            .decoded_raw_bytes
            .checked_add(
                self.buffer_bytes()
                    * if Arc::strong_count(&self.image) > 1 {
                        2
                    } else {
                        1
                    },
            )
            .and_then(|n| {
                n.checked_add(
                    crate::sharpening::scratch_bytes(self.image.width(), self.image.height())
                        .ok()?,
                )
            })
            .ok_or_else(|| dimension_overflow(self.image.width(), self.image.height()))?;
        if self.recipe.capture_sharpening.is_active() {
            validate_working_set(bytes)?;
        }
        let stage = if self.recipe.capture_sharpening.is_active() {
            super::super::capture::apply(
                Arc::make_mut(&mut self.image),
                &self.recipe,
                self.gains,
                cancellation,
            )?
        } else {
            cancellation.checkpoint()?;
            super::super::capture::CaptureStage {
                provenance: None,
                elapsed: Duration::ZERO,
                scratch_bytes: 0,
            }
        };
        Ok(CapturedCameraSource {
            source: self,
            stage,
        })
    }

    /// Accept a privately assembled executor result for this exact source and
    /// contract. The caller must execute the contract before transferring it.
    pub fn with_capture_result(
        mut self,
        image: LinearRgbImage<f32>,
        provenance: Option<CaptureSharpeningProvenance>,
        elapsed: Duration,
        scratch_bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<CapturedCameraSource, PipelineError> {
        cancellation.checkpoint()?;
        if provenance != CaptureSharpeningProvenance::for_settings(self.recipe.capture_sharpening)
            || image.width() != self.image.width()
            || image.height() != self.image.height()
            || image.row_stride() != self.image.row_stride()
            || image.space() != rohditor_image::LinearRgbSpace::CameraNative
        {
            return Err(PipelineError::InvalidRecipe {
                field: "capture_sharpening",
                reason: "capture result does not match its camera source".into(),
            });
        }
        self.image = Arc::new(image);
        Ok(CapturedCameraSource {
            source: self,
            stage: super::super::capture::CaptureStage {
                provenance,
                elapsed,
                scratch_bytes,
            },
        })
    }
}

impl CapturedCameraSource {
    pub fn buffer_bytes(&self) -> usize {
        self.source.buffer_bytes()
    }
    pub fn with_recipe(
        mut self,
        recipe: &EditRecipe,
        options: PreviewOptions,
    ) -> Result<Self, PipelineError> {
        let previous = self.source.capture_contract()?;
        self.source = self.source.with_recipe(recipe, options)?;
        let next = self.source.capture_contract()?;
        if self.stage.provenance != CaptureSharpeningProvenance::for_settings(next.settings())
            || previous.ceilings() != next.ceilings()
        {
            return Err(PipelineError::InvalidRecipe {
                field: "capture_sharpening",
                reason: "capture settings or ceilings changed".into(),
            });
        }
        self.stage.elapsed = Duration::ZERO;
        Ok(self)
    }
}

impl CpuPipeline {
    pub fn prepare_camera_source(
        &self,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: PreviewOptions,
        cancellation: &CancellationToken,
    ) -> Result<DemosaicedCameraSource, PipelineError> {
        prepare_camera_source(frame, recipe, options, cancellation)
    }

    pub fn complete_camera_source(
        &self,
        captured: CapturedCameraSource,
        cancellation: &CancellationToken,
    ) -> Result<ReconstructedPreview, PipelineError> {
        complete_camera_source(
            self.optics_service().map(AsRef::as_ref),
            captured,
            cancellation,
        )
    }

    /// Resolve the immutable spatial contract without allocating output pixel
    /// storage. CPU and GPU completion consume this same description.
    pub fn describe_spatial_completion(
        &self,
        source: &DemosaicedCameraSource,
        cancellation: &CancellationToken,
    ) -> Result<SpatialCompletionDescription, PipelineError> {
        describe_spatial_completion(
            self.optics_service().map(AsRef::as_ref),
            source,
            cancellation,
        )
    }
}

fn describe_spatial_completion(
    optics: Option<&OpticsService>,
    source: &DemosaicedCameraSource,
    cancellation: &CancellationToken,
) -> Result<SpatialCompletionDescription, PipelineError> {
    cancellation.checkpoint()?;
    let source_dimensions = (source.image.width(), source.image.height());
    let target_dimensions = preview_dimensions(
        source_dimensions.0,
        source_dimensions.1,
        source.options.max_long_edge,
    )?;
    let execution = crate::optics::resolve_execution(
        optics,
        &source.info,
        &source.recipe.optics,
        source_dimensions.0,
        source_dimensions.1,
    )?;
    let area_reduction = crate::AreaReductionPlan::new(
        source_dimensions.0,
        source_dimensions.1,
        target_dimensions.0,
        target_dimensions.1,
    )?;
    cancellation.checkpoint()?;
    Ok(SpatialCompletionDescription {
        source_identity: source.identity.clone(),
        source_dimensions,
        target_dimensions,
        optics: execution.map_or(SpatialOptics::Off, |execution| {
            SpatialOptics::Enabled(Box::new(execution))
        }),
        area_reduction,
        calibration: source.calibration.clone(),
        source_orientation: source.info.orientation,
        profile_selection: source.recipe.color.camera_profile.clone(),
        camera_profile: camera_profile_key(&source.recipe.color.camera_profile),
        highlight_adjustments: source.recipe.raw.highlights,
        highlight_white_balance: source.recipe.color.white_balance,
        highlight_diagnostics: source.highlight_diagnostics,
        capture_sharpening: CaptureSharpeningProvenance::for_settings(
            source.recipe.capture_sharpening,
        ),
        preparation_timings: source.timings,
        decoded_raw_bytes: source.decoded_raw_bytes,
        normalized_mosaic_bytes: source.normalized_mosaic_bytes,
        highlight_scratch_bytes: source.highlight_scratch_bytes,
    })
}

fn prepare_camera_source(
    frame: &RawFrame,
    recipe: &EditRecipe,
    options: PreviewOptions,
    cancellation: &CancellationToken,
) -> Result<DemosaicedCameraSource, PipelineError> {
    let total_started = Instant::now();
    cancellation.checkpoint()?;
    validate_preview_working_set(
        frame,
        options.max_long_edge,
        recipe.raw.highlights.method,
        crate::optics::optics_enabled(&recipe.optics),
    )?;

    let metadata_started = Instant::now();
    let metadata_span = tracing::info_span!(
        "cpu.metadata",
        width = frame.info.width,
        height = frame.info.height,
        purpose = "preview reconstruction"
    );
    let metadata_guard = metadata_span.enter();
    recipe.validate()?;
    validate_optics_crop(options.render.raw_crop_policy, recipe)?;
    let calibration = CameraCalibration::from_raw_info(&frame.info);
    let resolved = resolve_camera_colour(
        &calibration,
        &recipe.color.camera_profile,
        recipe.color.white_balance,
    )?;
    let highlight_gains = (recipe.raw.highlights.method == HighlightMethod::Clip)
        .then_some(resolved.white_balance_gains);
    let metadata = metadata_started.elapsed();
    drop(metadata_guard);

    let normalization_started = Instant::now();
    let mosaic = normalize_raw_cancellable(frame, options.render.raw_crop_policy, cancellation)?;
    let normalization = normalization_started.elapsed();
    let decoded_raw_bytes = frame
        .mosaic
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?;
    let source_width = mosaic.width();
    let source_height = mosaic.height();
    let normalized_mosaic_bytes = mosaic
        .data()
        .len()
        .checked_mul(size_of::<f32>())
        .ok_or_else(|| dimension_overflow(source_width, source_height))?;

    let highlight_started = Instant::now();
    let highlighted = apply_highlight_cancellable(
        mosaic,
        recipe.raw.highlights,
        highlight_gains.unwrap_or(WhiteBalanceGains::identity()),
        cancellation,
    )?;
    let highlight_processing = highlight_started.elapsed();
    let highlight_diagnostics = highlighted.diagnostics;
    let highlight_scratch_bytes = estimated_highlight_scratch_bytes(
        recipe.raw.highlights.method,
        highlight_diagnostics,
        source_width,
        source_height,
    )?;
    let mosaic = highlighted.mosaic;

    let demosaic_started = Instant::now();
    let full_linear = demosaic_cancellable(
        &mosaic,
        WhiteBalanceGains::identity(),
        options.render.demosaic,
        cancellation,
    )?;
    let demosaic = demosaic_started.elapsed();
    drop(mosaic);

    Ok(DemosaicedCameraSource {
        identity: CameraSourceIdentity::default(),
        image: Arc::new(full_linear),
        info: frame.info.clone(),
        recipe: recipe.clone(),
        options,
        calibration,
        highlight_diagnostics,
        decoded_raw_bytes,
        normalized_mosaic_bytes,
        highlight_scratch_bytes,
        gains: resolved.white_balance_gains,
        timings: StageTimings {
            metadata,
            normalization,
            highlight_processing,
            highlight_clipping: highlight_processing,
            demosaic,
            total: total_started.elapsed(),
            ..StageTimings::default()
        },
    })
}

pub(super) fn prepare_full_capture_base(
    optics: Option<&OpticsService>,
    frame: &RawFrame,
    recipe: &EditRecipe,
    options: RenderOptions,
    cancellation: &CancellationToken,
) -> Result<DemosaicedBase, PipelineError> {
    let source = prepare_reconstructed_preview(
        optics,
        frame,
        recipe,
        PreviewOptions {
            render: options,
            max_long_edge: frame.info.width.max(frame.info.height),
        },
        cancellation,
    )?;
    let preparation = source.timings;
    let mut base = prepare_demosaiced_preview_owned(source, recipe, cancellation)?;
    let color = base.timings.color_conversion;
    base.timings = preparation;
    base.timings.color_conversion = color;
    base.timings.total += color;
    Ok(base)
}

pub(super) fn prepare_reconstructed_preview(
    optics: Option<&OpticsService>,
    frame: &RawFrame,
    recipe: &EditRecipe,
    options: PreviewOptions,
    cancellation: &CancellationToken,
) -> Result<ReconstructedPreview, PipelineError> {
    super::super::capture::validate_working_set(frame, recipe)?;
    let source = prepare_camera_source(frame, recipe, options, cancellation)?;
    complete_camera_source(optics, source.capture_cpu(cancellation)?, cancellation)
}

fn complete_camera_source(
    optics: Option<&OpticsService>,
    captured: CapturedCameraSource,
    cancellation: &CancellationToken,
) -> Result<ReconstructedPreview, PipelineError> {
    cancellation.checkpoint()?;
    let total_started = Instant::now();
    let description = describe_spatial_completion(optics, &captured.source, cancellation)?;
    let CapturedCameraSource {
        source,
        stage: capture,
    } = captured;
    let DemosaicedCameraSource {
        image: full_linear,
        info,
        recipe,
        options: _,
        calibration,
        highlight_diagnostics,
        decoded_raw_bytes,
        normalized_mosaic_bytes,
        highlight_scratch_bytes,
        timings,
        ..
    } = source;
    // Validate before copy-on-write, optics, or reduction allocate. Two image
    // buffers cover either optics input/output or a reduction pass. A retained
    // capture cache adds a third buffer at the same lifetime.
    let image_bytes = std::mem::size_of_val(full_linear.data());
    let live_images = if Arc::strong_count(&full_linear) > 1 {
        3
    } else {
        2
    };
    let completion_peak = image_bytes
        .checked_mul(live_images)
        .and_then(|n| n.checked_add(decoded_raw_bytes))
        .and_then(|n| {
            full_linear
                .width()
                .checked_mul(24)
                .and_then(|scratch| n.checked_add(scratch))
        })
        .ok_or_else(|| dimension_overflow(full_linear.width(), full_linear.height()))?;
    validate_working_set(completion_peak)?;
    let full_linear = match Arc::try_unwrap(full_linear) {
        Ok(image) => image,
        Err(image) => {
            let mut data = Vec::new();
            data.try_reserve_exact(image.data().len())
                .map_err(|_| PipelineError::Allocation {
                    elements: image.data().len(),
                })?;
            data.extend_from_slice(image.data());
            LinearRgbImage::new(
                image.width(),
                image.height(),
                image.row_stride(),
                image.space(),
                data,
            )?
        }
    };
    let recipe = &recipe;
    let source_width = full_linear.width();
    let source_height = full_linear.height();
    let StageTimings {
        metadata,
        normalization,
        highlight_processing,
        demosaic,
        total: prior_total,
        ..
    } = timings;
    let optics_started = Instant::now();
    let optics_result = crate::optics::apply_execution_cancellable(
        description.optics.execution(),
        full_linear,
        cancellation,
    )?;
    let optics = optics_result.image;
    let optics_provenance = optics_result.provenance;
    let optics_output_bytes = optics_result.output_bytes;
    let optics_scratch_bytes = optics_result.scratch_bytes;
    let optics_timing = optics_started.elapsed();

    let (target_width, target_height) = description.target_dimensions;
    let unchanged_dimensions = source_width == target_width && source_height == target_height;
    let resample_intermediate_bytes = if unchanged_dimensions {
        0
    } else {
        target_width
            .checked_mul(source_height)
            .and_then(|pixels| pixels.checked_mul(3 * size_of::<f32>()))
            .ok_or_else(|| dimension_overflow(target_width, source_height))?
    };
    let reduced_linear_bytes = target_width
        .checked_mul(target_height)
        .and_then(|pixels| pixels.checked_mul(3 * size_of::<f32>()))
        .ok_or_else(|| dimension_overflow(target_width, target_height))?;
    let full_linear_bytes = source_width
        .checked_mul(source_height)
        .and_then(|pixels| pixels.checked_mul(3 * size_of::<f32>()))
        .ok_or_else(|| dimension_overflow(source_width, source_height))?;
    let preparation_peak_bytes = preview_preparation_peak(PreviewPreparationInputs {
        decoded_raw_bytes,
        normalized_mosaic_bytes,
        full_linear_bytes,
        highlight_scratch_bytes,
        optics_output_bytes,
        optics_scratch_bytes,
        resample_intermediate_bytes,
        reduced_linear_bytes,
        unchanged_dimensions,
    })?;
    let preparation_peak_bytes = preparation_peak_bytes.max(
        decoded_raw_bytes
            .checked_add(full_linear_bytes)
            .and_then(|n| n.checked_add(capture.scratch_bytes))
            .ok_or_else(|| dimension_overflow(source_width, source_height))?,
    );
    validate_working_set(preparation_peak_bytes)?;

    let resampling_started = Instant::now();
    let image =
        resize_area_with_plan_cancellable(optics, &description.area_reduction, cancellation)?;
    let resampling = resampling_started.elapsed();
    let timings = StageTimings {
        metadata,
        normalization,
        highlight_processing,
        highlight_clipping: highlight_processing,
        demosaic,
        capture_sharpening: capture.elapsed,
        optics: optics_timing,
        resampling,
        total: prior_total + capture.elapsed + total_started.elapsed(),
        ..StageTimings::default()
    };

    Ok(ReconstructedPreview {
        image,
        calibration,
        source_orientation: info.orientation,
        profile_selection: recipe.color.camera_profile.clone(),
        camera_profile: camera_profile_key(&recipe.color.camera_profile),
        highlight_adjustments: recipe.raw.highlights,
        highlight_white_balance: recipe.color.white_balance,
        highlight_diagnostics,
        capture_sharpening: capture.provenance,
        capture_sharpening_scratch_bytes: capture.scratch_bytes,
        optics_provenance,
        optics_output_bytes,
        optics_scratch_bytes,
        timings,
        decoded_raw_bytes,
        normalized_mosaic_bytes,
        highlight_scratch_bytes,
        resample_intermediate_bytes,
        preparation_peak_bytes,
    })
}
