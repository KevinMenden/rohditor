use super::*;

fn setup() -> (SpatialCache, RawFrame) {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapter =
        pollster::block_on(instance.request_adapter(&Default::default())).expect("Vulkan adapter");
    eprintln!("Editor sensor adapter: {:?}", adapter.get_info());
    let (device, queue) =
        pollster::block_on(adapter.request_device(&Default::default())).expect("device");
    let mut cache = SpatialCache::default();
    cache.configure(adapter, device, queue, wgpu::TextureFormat::Rgba8Unorm);
    let mut frame = super::super::tests::frame();
    frame.info.width = 13;
    frame.info.height = 9;
    frame.row_stride = 13;
    frame.info.white_levels = vec![65535.0];
    frame.info.color_matrices = vec![rohditor_raw::CameraColorMatrix {
        illuminant: "D65".into(),
        values: vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        origin: rohditor_raw::CameraMatrixOrigin::DecoderDatabase,
    }];
    frame.mosaic = (0..117)
        .map(|i| (1000 + i * 421) as u16)
        .collect::<Vec<_>>()
        .into();
    (cache, frame)
}

fn prepare(
    cache: &mut SpatialCache,
    frame: &RawFrame,
    recipe: &EditRecipe,
    options: PreviewOptions,
) -> SpatialMetrics {
    let keys = PreviewCacheKeys::new(1, frame, recipe, options);
    cache
        .prepare_gpu(
            &CpuPipeline::default(),
            frame,
            recipe,
            options,
            &keys,
            &CancellationToken::new(),
        )
        .expect("editor GPU preparation")
        .1
}

#[test]
#[ignore = "requires Vulkan; exercises editor sensor cache and CPU recovery"]
fn editor_sensor_cache_reuses_camera_pixels_and_invalidates_upstream_dependencies() {
    let (mut cache, mut frame) = setup();
    let mut recipe = EditRecipe::default();
    let mut options = PreviewOptions {
        max_long_edge: 7,
        ..Default::default()
    };
    let first = prepare(&mut cache, &frame, &recipe, options);
    assert!(first.sensor_gpu);
    assert_eq!(first.uploaded_bytes, 117 * 2);
    assert!(cache.source.is_none() && cache.captured.is_none());
    assert_eq!(cache.resident_bytes(), 0);
    recipe.color.hsl.channels[0].hue = 0.25;
    recipe.color.grading.shadows[0] = 0.2;
    options.max_long_edge = 5;
    let repeated = prepare(&mut cache, &frame, &recipe, options);
    assert!(repeated.sensor_gpu);
    assert_eq!(repeated.uploaded_bytes, 0);
    recipe.optics.distortion = !recipe.optics.distortion;
    assert_eq!(
        prepare(&mut cache, &frame, &recipe, options).uploaded_bytes,
        0
    );
    let keys = PreviewCacheKeys::new(1, &frame, &recipe, options);
    let (full, metrics) = cache
        .prepare_gpu_full(
            &CpuPipeline::default(),
            &frame,
            &recipe,
            PreviewOptions {
                max_long_edge: usize::MAX,
                ..options
            },
            &keys,
            &CancellationToken::new(),
        )
        .expect("Source 1:1");
    assert!(metrics.sensor_gpu);
    assert_eq!(metrics.uploaded_bytes, 0);
    assert_eq!(full.description().normalized_mosaic_bytes(), 0);
    assert_eq!(full.dimensions(), (13, 9));
    cache
        .render_gpu_full(
            &full,
            &recipe,
            options.render.output_policy,
            &CancellationToken::new(),
        )
        .expect("native Source 1:1 frame");
    drop(full);

    recipe.color.white_balance = rohditor_edit::WhiteBalance::ManualMultipliers {
        red: 1.3,
        green: 1.0,
        blue: 1.1,
    };
    assert_eq!(
        prepare(&mut cache, &frame, &recipe, options).uploaded_bytes,
        117 * 2
    );
    recipe.capture_sharpening.enabled = true;
    assert_eq!(
        prepare(&mut cache, &frame, &recipe, options).uploaded_bytes,
        117 * 2
    );
    options.render.demosaic = DemosaicAlgorithm::Bilinear;
    assert_eq!(
        prepare(&mut cache, &frame, &recipe, options).uploaded_bytes,
        117 * 2
    );
    recipe.raw.highlights.method = rohditor_edit::HighlightMethod::Off;
    assert_eq!(
        prepare(&mut cache, &frame, &recipe, options).uploaded_bytes,
        117 * 2
    );
    recipe.color.white_balance = rohditor_edit::WhiteBalance::ManualMultipliers {
        red: 1.7,
        green: 1.0,
        blue: 1.2,
    };
    assert_eq!(
        prepare(&mut cache, &frame, &recipe, options).uploaded_bytes,
        0
    );
    frame.info.crop_area = Some(rohditor_raw::ImageRect {
        x: 1,
        y: 1,
        width: 9,
        height: 7,
    });
    assert_eq!(
        prepare(&mut cache, &frame, &recipe, options).uploaded_bytes,
        9 * 7 * 2
    );

    let token = CancellationToken::new();
    token.cancel();
    let keys = PreviewCacheKeys::new(1, &frame, &recipe, options);
    assert!(matches!(
        cache.prepare_gpu(
            &CpuPipeline::default(),
            &frame,
            &recipe,
            options,
            &keys,
            &token
        ),
        Err(GpuPreviewError::Cancelled)
    ));
    assert_eq!(
        prepare(&mut cache, &frame, &recipe, options).uploaded_bytes,
        0
    );
    cache.clear_images();
    assert_eq!(
        prepare(&mut cache, &frame, &recipe, options).uploaded_bytes,
        9 * 7 * 2
    );
    options.render.demosaic = DemosaicAlgorithm::Rcd;
    let rcd = prepare(&mut cache, &frame, &recipe, options);
    assert!(rcd.sensor_gpu);
    assert_eq!(rcd.uploaded_bytes, 9 * 7 * 2);
    options.render.demosaic = DemosaicAlgorithm::Amaze;
    assert!(!prepare(&mut cache, &frame, &recipe, options).sensor_gpu);
    assert!(
        cache
            .recovery
            .as_ref()
            .is_some_and(|reason| reason.contains("amaze"))
    );
    assert!(cache.sensor_source.is_none());
    options.render.demosaic = DemosaicAlgorithm::MalvarHeCutler;
    assert!(prepare(&mut cache, &frame, &recipe, options).sensor_gpu);
    cache.release_gpu_images();
    assert!(cache.sensor_source.is_none());
}

#[test]
#[ignore = "requires Vulkan; RCD fit and Source 1:1 share cached camera pixels"]
fn rcd_fit_and_source_one_to_one_reuse_camera_pixels() {
    let (mut cache, mut frame) = setup();
    frame.info.width = 29;
    frame.info.height = 31;
    frame.info.crop_area = None;
    frame.info.active_area = None;
    frame.row_stride = 29;
    frame.mosaic = (0..29 * 31)
        .map(|i| (1000 + (i * 137 + (i / 29) * 503) % 50000) as u16)
        .collect::<Vec<_>>()
        .into();
    let mut recipe = EditRecipe::default();
    let mut options = PreviewOptions {
        max_long_edge: 21,
        ..Default::default()
    };
    options.render.demosaic = DemosaicAlgorithm::Rcd;
    let first = prepare(&mut cache, &frame, &recipe, options);
    assert!(first.sensor_gpu);
    assert_eq!(first.uploaded_bytes, 29 * 31 * 2);
    recipe.color.hsl.channels[0].hue = 0.3;
    recipe.color.grading.shadows[0] = 0.2;
    assert_eq!(
        prepare(&mut cache, &frame, &recipe, options).uploaded_bytes,
        0
    );
    let keys = PreviewCacheKeys::new(1, &frame, &recipe, options);
    let (full, full_metrics) = cache
        .prepare_gpu_full(
            &CpuPipeline::default(),
            &frame,
            &recipe,
            PreviewOptions {
                max_long_edge: usize::MAX,
                ..options
            },
            &keys,
            &CancellationToken::new(),
        )
        .expect("RCD Source 1:1");
    assert!(full_metrics.sensor_gpu);
    assert_eq!(full_metrics.uploaded_bytes, 0);
    assert_eq!(full.dimensions(), (29, 31));
    drop(full);
    recipe.raw.highlights.method = rohditor_edit::HighlightMethod::Off;
    assert_eq!(
        prepare(&mut cache, &frame, &recipe, options).uploaded_bytes,
        29 * 31 * 2
    );
    recipe.color.white_balance = rohditor_edit::WhiteBalance::ManualMultipliers {
        red: 1.3,
        green: 1.0,
        blue: 1.1,
    };
    assert_eq!(
        prepare(&mut cache, &frame, &recipe, options).uploaded_bytes,
        0
    );
    recipe.raw.highlights.method = rohditor_edit::HighlightMethod::Clip;
    assert_eq!(
        prepare(&mut cache, &frame, &recipe, options).uploaded_bytes,
        29 * 31 * 2
    );
    recipe.color.white_balance = rohditor_edit::WhiteBalance::ManualMultipliers {
        red: 1.5,
        green: 1.0,
        blue: 1.2,
    };
    assert_eq!(
        prepare(&mut cache, &frame, &recipe, options).uploaded_bytes,
        29 * 31 * 2
    );
    options.render.demosaic = DemosaicAlgorithm::MalvarHeCutler;
    assert_eq!(
        prepare(&mut cache, &frame, &recipe, options).uploaded_bytes,
        29 * 31 * 2
    );
    options.render.demosaic = DemosaicAlgorithm::Rcd;
    assert_eq!(
        prepare(&mut cache, &frame, &recipe, options).uploaded_bytes,
        29 * 31 * 2
    );
}

#[test]
#[ignore = "requires Vulkan; GPU resource rejection restarts from immutable RAW"]
fn editor_sensor_budget_failure_recovers_on_cpu_and_reports_mixed_backend() {
    let (mut cache, frame) = setup();
    let mut recipe = EditRecipe::default();
    let options = PreviewOptions::default();
    assert!(prepare(&mut cache, &frame, &recipe, options).sensor_gpu);
    cache
        .sensor
        .as_mut()
        .expect("initialized sensor")
        .set_budget(64);
    recipe.capture_sharpening.enabled = true;
    let metrics = prepare(&mut cache, &frame, &recipe, options);
    assert!(!metrics.sensor_gpu);
    assert!(cache.recovery.is_some());
    assert!(cache.sensor_source.is_none());
    assert!(metrics.uploaded_bytes > 0);
    cache.release_gpu_images();
    assert!(
        cache.recovery.is_none(),
        "explicit CPU selection clears mixed-backend recovery status"
    );
    cache.device.as_ref().expect("device").0.destroy();
    cache.clear_images();
    let keys = PreviewCacheKeys::new(1, &frame, &recipe, options);
    assert!(
        cache
            .prepare_gpu(
                &CpuPipeline::default(),
                &frame,
                &recipe,
                options,
                &keys,
                &CancellationToken::new()
            )
            .is_err()
    );
    assert!(cache.sensor_source.is_none());
    CpuPipeline::default()
        .render_preview(&frame, &recipe, options)
        .expect("CPU recovery from immutable RAW");
}
