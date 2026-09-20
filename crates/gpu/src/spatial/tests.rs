use std::sync::{Arc, mpsc};

use rohditor_core::{CancellationToken, CpuPipeline, PreviewOptions};
use rohditor_edit::EditRecipe;
use rohditor_image::Orientation;
use rohditor_raw::{CaptureMetadata, RationalValue};
use wgpu::util::DeviceExt;

use super::*;

const SMALL_FIXTURE_COORDINATE_TOLERANCE: f32 = 2.0e-4;
// Lensfun resolves the real profile in f64 before producing f32 coordinates,
// whereas WGSL executes the same Newton path in f32. This bound is in source
// pixels and is paired below with the unchanged camera-linear output gate.
const REAL_RAW_COORDINATE_TOLERANCE: f32 = 1.0e-3;
const REAL_RAW_OUTPUT_ABSOLUTE_TOLERANCE: f32 = 2.0e-3;
const REAL_RAW_OUTPUT_RELATIVE_TOLERANCE: f32 = 2.0e-4;

fn processors() -> (GpuSpatialProcessor, crate::GpuPreviewProcessor) {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .expect("Vulkan adapter");
    eprintln!("Spatial adapter: {:?}", adapter.get_info());
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .expect("spatial device");
    let spatial = GpuSpatialProcessor::new(&device, &queue).expect("spatial processor");
    let preview =
        crate::GpuPreviewProcessor::new(&adapter, &device, &queue, wgpu::TextureFormat::Rgba8Unorm)
            .expect("preview processor");
    (spatial, preview)
}

#[test]
#[ignore = "requires Vulkan; software validates structure and numerical parity only"]
fn capture_residency_and_exact_reduction_match_cpu_without_camera_readback() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let (mut processor, preview_processor) = processors();
    let cpu = CpuPipeline::default();
    let frame = crate::preview::processor::tests::synthetic_frame(Orientation::Normal);
    let token = CancellationToken::new();
    for capture in [false, true] {
        let mut recipe = EditRecipe::default();
        recipe.capture_sharpening.enabled = capture;
        recipe.capture_sharpening.radius = 1.2;
        recipe.capture_sharpening.noise_protection = 0.0;
        let options = PreviewOptions {
            max_long_edge: 4,
            ..Default::default()
        };
        let source = cpu
            .prepare_camera_source(&frame, &recipe, options, &token)
            .expect("camera source");
        let reference = cpu
            .complete_camera_source(
                source.clone().capture_cpu(&token).expect("CPU capture"),
                &token,
            )
            .expect("CPU completion");
        let description = cpu
            .describe_spatial_completion(&source, &token)
            .expect("spatial description");
        let (resident, upload) = processor
            .upload_captured_source(&cpu, &source, &token)
            .expect("resident source");
        assert_eq!(upload.capture.readback_bytes, 0);
        let unrelated = cpu
            .prepare_camera_source(&frame, &recipe, options, &token)
            .expect("independent camera source");
        let unrelated_description = cpu
            .describe_spatial_completion(&unrelated, &token)
            .expect("independent description");
        assert!(matches!(
            processor.reduce_preview(&resident, unrelated_description, &token),
            Err(GpuPreviewError::BaseMismatch { .. })
        ));
        let (preview, metrics) = processor
            .reduce_preview(&resident, description, &token)
            .expect("GPU reduction");
        assert_eq!(metrics.readback_bytes, 4);
        let actual = readback(&processor, &preview);
        for (index, expected) in reference
            .image()
            .data()
            .as_chunks::<3>()
            .0
            .iter()
            .enumerate()
        {
            let actual = &actual[index * 4..index * 4 + 3];
            for (actual, expected) in actual.iter().zip(expected) {
                let tolerance = if capture { 4.0e-5 } else { 2.0e-6 };
                assert!(
                    (actual - expected).abs() <= tolerance + tolerance * expected.abs(),
                    "capture={capture}, {actual} vs {expected}"
                );
            }
        }
    }

    let mut optics_frame = frame.clone();
    optics_frame.info.make = "Sony".into();
    optics_frame.info.model = "ILCE-6400".into();
    optics_frame.info.clean_make = "Sony".into();
    optics_frame.info.clean_model = "Alpha 6400".into();
    optics_frame.info.capture = CaptureMetadata {
        focal_length: Some(RationalValue {
            numerator: 35,
            denominator: 1,
        }),
        aperture: Some(RationalValue {
            numerator: 28,
            denominator: 10,
        }),
        focus_distance: Some(RationalValue {
            numerator: 10,
            denominator: 1,
        }),
        lens_make: Some("Tamron".into()),
        lens_model: Some("Tamron 17-70mm F/2.8 Di III-A VC RXD".into()),
        ..CaptureMetadata::default()
    };
    let optics_cpu = CpuPipeline::new(Arc::new(
        rohditor_core::OpticsService::load_bundled().expect("bundled optics"),
    ));
    let mut optics_recipe = EditRecipe::default();
    optics_recipe.optics.profile = rohditor_edit::LensProfileSelection::Automatic;
    let optics_options = PreviewOptions {
        max_long_edge: 4,
        ..Default::default()
    };
    let optics_source = optics_cpu
        .prepare_camera_source(&optics_frame, &optics_recipe, optics_options, &token)
        .expect("optics camera source");
    let optics_reference = optics_cpu
        .complete_camera_source(
            optics_source
                .clone()
                .capture_cpu(&token)
                .expect("CPU capture bypass"),
            &token,
        )
        .expect("CPU optics and reduction");
    let optics_description = optics_cpu
        .describe_spatial_completion(&optics_source, &token)
        .expect("optics spatial description");
    assert!(
        optics_description
            .optics()
            .provenance()
            .is_some_and(|provenance| provenance.applied.any())
    );
    let (optics_resident, _) = processor
        .upload_captured_source(&optics_cpu, &optics_source, &token)
        .expect("optics resident source");
    let (optics_preview, optics_metrics) = processor
        .reduce_preview(&optics_resident, optics_description, &token)
        .expect("GPU optics and reduction");
    assert_eq!(optics_metrics.readback_bytes, 4);
    let optics_actual = readback(&processor, &optics_preview);
    for (actual, expected) in optics_actual
        .chunks_exact(4)
        .zip(optics_reference.image().data().chunks_exact(3))
    {
        for channel in 0..3 {
            let tolerance = 2.0e-4 + 2.0e-4 * expected[channel].abs();
            assert!(
                (actual[channel] - expected[channel]).abs() <= tolerance,
                "optics channel mismatch: {} vs {}",
                actual[channel],
                expected[channel]
            );
        }
    }

    let mut recipe = EditRecipe::default();
    recipe.capture_sharpening.enabled = true;
    recipe.capture_sharpening.radius = 1.2;
    recipe.capture_sharpening.noise_protection = 0.0;
    recipe.light.exposure_ev = 0.35;
    recipe.color.saturation = 0.2;
    recipe.geometry.orientation_override = Some(Orientation::Rotate90);
    let options = PreviewOptions {
        max_long_edge: frame.info.width.max(frame.info.height),
        ..Default::default()
    };
    let source = cpu
        .prepare_camera_source(&frame, &recipe, options, &token)
        .expect("full camera source");
    let description = cpu
        .describe_spatial_completion(&source, &token)
        .expect("full spatial description");
    let camera_reference = cpu
        .complete_camera_source(
            source
                .clone()
                .capture_cpu(&token)
                .expect("full CPU capture"),
            &token,
        )
        .expect("full CPU spatial completion");
    let (resident, metrics) = processor
        .upload_captured_source(&cpu, &source, &token)
        .expect("full resident source");
    assert_eq!(metrics.capture.readback_bytes, 0);
    let (materialized, materialized_metrics) = processor
        .reduce_preview(&resident, description.clone(), &token)
        .expect("no-reduction optics materialization");
    assert_eq!(materialized_metrics.submissions, 1);
    let materialized = readback(&processor, &materialized);
    for (actual, expected) in materialized
        .chunks_exact(4)
        .zip(camera_reference.image().data().chunks_exact(3))
    {
        for channel in 0..3 {
            let tolerance = 4.0e-5 + 4.0e-5 * expected[channel].abs();
            assert!((actual[channel] - expected[channel]).abs() <= tolerance);
        }
    }
    let full = processor
        .full_resolution_source(&resident, description)
        .expect("full spatial source");
    let gpu_frame = preview_processor
        .render_spatial_full(
            &full,
            &recipe,
            rohditor_core::OutputPolicy::ClipToSrgb,
            None,
        )
        .expect("direct full display");
    let actual = preview_processor
        .readback_display(&gpu_frame)
        .expect("diagnostic display readback");
    let expected = cpu
        .render_source_scale_preview_cancellable(&frame, &recipe, options.render, &token)
        .expect("CPU source-scale reference");
    assert_eq!(
        (actual.width as usize, actual.height as usize),
        (expected.image.width(), expected.image.height()),
    );
    for (actual, expected) in actual
        .rgba
        .chunks_exact(4)
        .zip(expected.image.data().chunks_exact(3))
    {
        for channel in 0..3 {
            assert!(
                actual[channel].abs_diff(expected[channel]) <= 2,
                "direct source 1:1 channel mismatch: {} vs {}",
                actual[channel],
                expected[channel]
            );
        }
    }
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        preview_processor.render_spatial_full_cancellable(
            &full,
            &recipe,
            rohditor_core::OutputPolicy::ClipToSrgb,
            None,
            &cancelled,
        ),
        Err(GpuPreviewError::Cancelled)
    ));
}

#[test]
#[ignore = "requires Vulkan; software validates optics model structure only"]
fn every_optics_model_matches_the_cpu_reference_with_fractional_reduction() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let (mut processor, _) = processors();
    processor.force_maximum_tile_edge(11);
    let mut frame = crate::preview::processor::tests::synthetic_frame(Orientation::Normal);
    frame.info.width = 57;
    frame.info.height = 43;
    frame.row_stride = 57;
    frame.mosaic = (0..57 * 43)
        .map(|index| ((index * 977 + (index / 57) * 131) % 65536) as u16)
        .collect::<Vec<_>>()
        .into();
    frame.info.make = "Fixture Camera".into();
    frame.info.model = "Body".into();
    frame.info.clean_make = "Fixture Camera".into();
    frame.info.clean_model = "Body".into();
    frame.info.capture = CaptureMetadata {
        aperture: Some(RationalValue {
            numerator: 28,
            denominator: 10,
        }),
        focus_distance: Some(RationalValue {
            numerator: 10,
            denominator: 1,
        }),
        lens_make: Some("Fixture Lens".into()),
        lens_model: Some("Models".into()),
        ..CaptureMetadata::default()
    };
    let token = CancellationToken::new();
    let mut recipe = EditRecipe::default();
    recipe.optics.profile = rohditor_edit::LensProfileSelection::Automatic;
    let options = PreviewOptions {
        max_long_edge: 23,
        ..Default::default()
    };
    for (focal, distortion, tca) in [
        (
            20_u32,
            r#"<distortion model="ptlens" focal="20" a="0.001" b="-0.002" c="0.001"/>"#,
            r#"<tca model="linear" focal="20" kr="1.001" kb="0.999"/>"#,
        ),
        (
            35,
            r#"<distortion model="poly3" focal="35" k1="0.01"/>"#,
            r#"<tca model="poly3" focal="35" vr="1.0002" br="-0.00004" cr="0.00001" vb="0.9998" bb="0.00004" cb="-0.00001"/>"#,
        ),
        (
            50,
            r#"<distortion model="poly5" focal="50" k1="0.005" k2="-0.001"/>"#,
            r#"<tca model="poly3" focal="50" vr="1.0001" br="-0.00002" vb="0.9999" bb="0.00002"/>"#,
        ),
    ] {
        let xml = format!(
            r#"<lensdatabase version="2">
<camera><maker>Fixture Camera</maker><model>Body</model><mount>Fixture Mount</mount><cropfactor>1</cropfactor></camera>
<lens><maker>Fixture Lens</maker><model>Models</model><mount>Fixture Mount</mount><cropfactor>1</cropfactor>
<focal min="{focal}" max="{focal}"/><aperture min="2.8" max="16"/><calibration>
{distortion}{tca}
<vignetting model="pa" focal="{focal}" aperture="2.8" distance="10000" k1="0.04" k2="0.004" k3="0.0004"/>
</calibration></lens></lensdatabase>"#,
        );
        let cpu = CpuPipeline::new(Arc::new(
            rohditor_core::OpticsService::from_xml(&xml).expect("fixture optics"),
        ));
        frame.info.capture.focal_length = Some(RationalValue {
            numerator: focal,
            denominator: 1,
        });
        let base_source = cpu
            .prepare_camera_source(&frame, &recipe, options, &token)
            .expect("model camera source");
        let (resident, upload) = processor
            .upload_captured_source(&cpu, &base_source, &token)
            .expect("model resident source");
        assert_eq!(upload.readback_bytes, 0);
        for mask in 0_u8..8 {
            let mut component_recipe = recipe.clone();
            component_recipe.optics.distortion = mask & 1 != 0;
            component_recipe.optics.vignetting = mask & 2 != 0;
            component_recipe.optics.chromatic_aberration = mask & 4 != 0;
            let source = base_source
                .clone()
                .with_recipe(&component_recipe, options)
                .expect("optics-only recipe change");
            let reference = cpu
                .complete_camera_source(
                    source.clone().capture_cpu(&token).expect("capture bypass"),
                    &token,
                )
                .expect("CPU model reference");
            let description = cpu
                .describe_spatial_completion(&source, &token)
                .expect("model description");
            let execution = description
                .optics()
                .execution()
                .expect("enabled model execution");
            assert_eq!(
                execution.components(),
                rohditor_core::CorrectionComponents {
                    distortion: component_recipe.optics.distortion,
                    vignetting: component_recipe.optics.vignetting,
                    chromatic_aberration: component_recipe.optics.chromatic_aberration,
                }
            );
            if mask == 7 {
                match (focal, execution.distortion(), execution.tca()) {
                    (
                        20,
                        Some(rohditor_core::DistortionModel::Ptlens { .. }),
                        Some(rohditor_core::TcaModel::Linear { .. }),
                    )
                    | (
                        35,
                        Some(rohditor_core::DistortionModel::Poly3 { .. }),
                        Some(rohditor_core::TcaModel::Poly3 { .. }),
                    )
                    | (
                        50,
                        Some(rohditor_core::DistortionModel::Poly5 { .. }),
                        Some(rohditor_core::TcaModel::Poly3 { .. }),
                    ) => {}
                    models => panic!("unexpected interpolated models: {models:?}"),
                }
                assert!(matches!(
                    execution.vignetting(),
                    Some(rohditor_core::VignettingModel::Pa { .. })
                ));
                qualify_coordinates(
                    &processor,
                    &resident,
                    execution,
                    SMALL_FIXTURE_COORDINATE_TOLERANCE,
                );
            }
            let (preview, metrics) = processor
                .reduce_preview(&resident, description, &token)
                .expect("model GPU completion");
            assert_eq!(metrics.uploaded_bytes, upload.uploaded_bytes);
            assert_eq!(metrics.readback_bytes, 4);
            let actual = readback(&processor, &preview);
            for (actual, expected) in actual
                .chunks_exact(4)
                .zip(reference.image().data().chunks_exact(3))
            {
                for channel in 0..3 {
                    let tolerance = 2.0e-4 + 2.0e-4 * expected[channel].abs();
                    assert!(
                        (actual[channel] - expected[channel]).abs() <= tolerance,
                        "focal={focal} mask={mask} channel={channel}: {} vs {}",
                        actual[channel],
                        expected[channel]
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "requires private Sony RAW and Vulkan; software is not hardware qualification"]
fn private_full_resolution_optics_and_fractional_preview_parity() {
    use rohditor_raw::{RawDecoder, RawlerDecoder};

    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let (mut processor, _) = processors();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/private/DSC00851.ARW");
    let frame = RawlerDecoder::default().decode(&path).expect("private RAW");
    let cpu = CpuPipeline::new(Arc::new(
        rohditor_core::OpticsService::load_bundled().expect("bundled optics"),
    ));
    let mut recipe = EditRecipe::default();
    recipe.optics.profile = rohditor_edit::LensProfileSelection::Automatic;
    let options = PreviewOptions {
        max_long_edge: 1001,
        ..Default::default()
    };
    let token = CancellationToken::new();
    let source = cpu
        .prepare_camera_source(&frame, &recipe, options, &token)
        .expect("private camera source");
    let description = cpu
        .describe_spatial_completion(&source, &token)
        .expect("private optics description");
    let execution = description
        .optics()
        .execution()
        .expect("private lens correction");
    assert!(execution.provenance().applied.any());
    eprintln!(
        "Private spatial source {:?}: {:?}",
        description.source_dimensions(),
        execution.provenance()
    );
    let reference = cpu
        .complete_camera_source(
            source.clone().capture_cpu(&token).expect("CPU capture"),
            &token,
        )
        .expect("CPU spatial reference");
    let (resident, upload) = processor
        .upload_captured_source(&cpu, &source, &token)
        .expect("private resident source");
    assert_eq!(upload.capture.readback_bytes, 0);
    qualify_coordinates(
        &processor,
        &resident,
        execution,
        REAL_RAW_COORDINATE_TOLERANCE,
    );
    let (preview, metrics) = processor
        .reduce_preview(&resident, description, &token)
        .expect("private GPU reduction");
    assert_eq!(metrics.readback_bytes, 4);
    let actual = readback(&processor, &preview);
    let mut maximum = 0.0_f32;
    let mut maximum_allowed = 0.0_f32;
    for (actual, expected) in actual
        .chunks_exact(4)
        .zip(reference.image().data().chunks_exact(3))
    {
        for channel in 0..3 {
            let difference = (actual[channel] - expected[channel]).abs();
            maximum = maximum.max(difference);
            maximum_allowed = maximum_allowed.max(
                REAL_RAW_OUTPUT_ABSOLUTE_TOLERANCE
                    + REAL_RAW_OUTPUT_RELATIVE_TOLERANCE * expected[channel].abs(),
            );
        }
    }
    eprintln!(
        "Private spatial preview max camera error={maximum:e}, upload={} bytes, failure readback={} bytes",
        upload.uploaded_bytes, metrics.readback_bytes
    );
    assert!(
        maximum <= maximum_allowed,
        "private spatial preview maximum camera error {maximum:e} exceeds {maximum_allowed:e}"
    );
}

fn qualify_coordinates(
    processor: &GpuSpatialProcessor,
    resident: &GpuCapturedSource,
    execution: &rohditor_core::OpticsExecution,
    coordinate_tolerance: f32,
) {
    let (width, height) = execution.dimensions();
    let mut points = vec![
        [0_u32, 0_u32],
        [(width - 1) as u32, 0],
        [0, (height - 1) as u32],
        [(width - 1) as u32, (height - 1) as u32],
        [(width / 2) as u32, (height / 2) as u32],
        [(width / 4) as u32, (height / 2) as u32],
        [(width * 3 / 4) as u32, (height / 3) as u32],
    ];
    // The profile's largest f32/f64 divergence is not guaranteed to be a
    // corner. Qualify a sparse interior grid without turning the private RAW
    // regression into a full-coordinate readback.
    for y in [height / 8, height * 3 / 8, height * 5 / 8, height * 7 / 8] {
        for x in [width / 8, width * 3 / 8, width * 5 / 8, width * 7 / 8] {
            points.push([x as u32, y as u32]);
        }
    }
    let shader = processor
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("optics coordinate qualification"),
            source: wgpu::ShaderSource::Wgsl(
                concat!(
                    include_str!("spatial.wgsl"),
                    "\n",
                    include_str!("coordinate_qualification.wgsl")
                )
                .into(),
            ),
        });
    let pipeline = processor
        .device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("optics coordinate qualification"),
            layout: None,
            module: &shader,
            entry_point: Some("qualify_coordinates"),
            compilation_options: Default::default(),
            cache: None,
        });
    let input = processor
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("optics coordinate qualification points"),
            contents: bytemuck::cast_slice(&points),
            usage: wgpu::BufferUsages::STORAGE,
        });
    let bytes = (points.len() * 3 * 2 * size_of::<f32>()) as u64;
    let output = processor.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("optics coordinate qualification output"),
        size: bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let staging = processor.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("optics coordinate qualification readback"),
        size: bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let optics = processor
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("optics coordinate qualification parameters"),
            contents: bytemuck::cast_slice(&super::reduction::pack_optics_parameters(
                &resident.planes.layout,
                Some(execution),
            )),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    let empty_zero = processor
        .device
        .create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("coordinate qualification empty group zero"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[],
        });
    let empty_one = processor
        .device
        .create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("coordinate qualification empty group one"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[],
        });
    let optics_bindings = processor
        .device
        .create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("coordinate qualification optics"),
            layout: &pipeline.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 3,
                resource: optics.as_entire_binding(),
            }],
        });
    let qualification_bindings = processor
        .device
        .create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("coordinate qualification buffers"),
            layout: &pipeline.get_bind_group_layout(3),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: input.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: output.as_entire_binding(),
                },
            ],
        });
    let mut encoder = processor
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &empty_zero, &[]);
        pass.set_bind_group(1, &empty_one, &[]);
        pass.set_bind_group(2, &optics_bindings, &[]);
        pass.set_bind_group(3, &qualification_bindings, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&output, 0, &staging, 0, bytes);
    processor.queue.submit([encoder.finish()]);
    let (sender, receiver) = mpsc::sync_channel(1);
    staging
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
    processor
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("coordinate qualification wait");
    receiver
        .recv()
        .expect("coordinate map callback")
        .expect("coordinate map");
    let mapped = staging.slice(..).get_mapped_range();
    let actual: &[f32] = bytemuck::cast_slice(&mapped);
    for (point_index, point) in points.iter().enumerate() {
        let expected = execution
            .map_coordinates(point[0] as usize, point[1] as usize)
            .expect("CPU coordinate mapping");
        for (channel, expected) in expected.iter().enumerate() {
            let offset = (point_index * 3 + channel) * 2;
            assert!(
                (actual[offset] - expected.0).abs() <= coordinate_tolerance
                    && (actual[offset + 1] - expected.1).abs() <= coordinate_tolerance,
                "coordinate mismatch at {point:?}, channel {channel}: ({}, {}) vs {:?}",
                actual[offset],
                actual[offset + 1],
                expected,
            );
        }
    }
    drop(mapped);
    staging.unmap();
}

fn readback(processor: &GpuSpatialProcessor, preview: &GpuSpatialPreview) -> Vec<f32> {
    let (width, height) = preview.dimensions();
    let bytes_per_row = width * 16;
    let padded_bytes_per_row = bytes_per_row.div_ceil(256) * 256;
    let buffer = processor.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("spatial test readback"),
        size: (padded_bytes_per_row * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = processor
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &preview.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row as u32),
                rows_per_image: Some(height as u32),
            },
        },
        wgpu::Extent3d {
            width: width as u32,
            height: height as u32,
            depth_or_array_layers: 1,
        },
    );
    processor.queue.submit([encoder.finish()]);
    let (sender, receiver) = mpsc::sync_channel(1);
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
    processor
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .expect("wait for readback");
    receiver.recv().expect("mapping callback").expect("mapping");
    let mapped = buffer.slice(..).get_mapped_range();
    let mut result = Vec::with_capacity(width * height * 4);
    for row in mapped.chunks(padded_bytes_per_row).take(height) {
        let samples: &[f32] = bytemuck::cast_slice(&row[..bytes_per_row]);
        result.extend_from_slice(samples);
    }
    drop(mapped);
    buffer.unmap();
    result
}
