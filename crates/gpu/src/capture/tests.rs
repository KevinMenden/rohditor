use super::*;
use rohditor_edit::CaptureSharpening;

pub(super) fn processor() -> GpuCaptureProcessor {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .expect("capture fixture");
    eprintln!("Capture adapter: {:?}", adapter.get_info());
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("capture fixture");
    GpuCaptureProcessor::new(&device, &queue).expect("capture fixture")
}

#[test]
#[ignore = "requires Vulkan; simulates loss of an isolated test device"]
fn destroyed_device_returns_recoverable_error() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let mut gpu = processor();
    let input = LinearRgbImage::new(
        3,
        5,
        9,
        rohditor_image::LinearRgbSpace::CameraNative,
        vec![0.2; 45],
    )
    .expect("fixture");
    let contract = CaptureSharpeningContract::new(
        CaptureSharpening {
            enabled: true,
            ..Default::default()
        },
        [1.0; 3],
    )
    .expect("contract");
    gpu.device.destroy();
    assert!(
        gpu.process(&input, &contract, 0, &CancellationToken::new())
            .is_err()
    );
    let mut reference = input.clone();
    contract
        .apply_cpu(&mut reference, &CancellationToken::new())
        .expect("CPU recovery from original");
    assert_eq!(input.data(), reference.data());
}

#[test]
#[ignore = "requires Vulkan; cancellation must drain bounded work and preserve recovery source"]
fn cancellation_and_constrained_budget_recovery() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let mut gpu = processor();
    let contract = CaptureSharpeningContract::new(
        CaptureSharpening {
            enabled: true,
            radius: 1.2,
            noise_protection: 0.0,
            ..Default::default()
        },
        [1.0; 3],
    )
    .expect("contract");
    let input = LinearRgbImage::new(
        513,
        271,
        513 * 3,
        rohditor_image::LinearRgbSpace::CameraNative,
        (0..513 * 271 * 3)
            .map(|i| ((i * 19) % 199) as f32 / 250.0)
            .collect(),
    )
    .expect("fixture");
    let original = input.data().to_vec();
    let token = CancellationToken::new();
    let other = token.clone();
    let cancel = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(5));
        other.cancel();
    });
    assert!(matches!(
        gpu.process(&input, &contract, 0, &token),
        Err(GpuPreviewError::Cancelled)
    ));
    cancel.join().expect("cancellation thread");
    assert_eq!(input.data(), original);
    gpu.set_remaining_budget(1);
    assert!(
        gpu.process(&input, &contract, 0, &CancellationToken::new())
            .is_err()
    );
    gpu.set_remaining_budget(2 * 1024 * 1024);
    let result = gpu
        .process(&input, &contract, 0, &CancellationToken::new())
        .expect("recovered processor")
        .expect("active capture");
    assert!(result.metrics.tile_edge < 512);
    let mut reference = input.clone();
    contract
        .apply_cpu(&mut reference, &CancellationToken::new())
        .expect("CPU recovery");
    for (a, b) in reference.data().iter().zip(result.image.data()) {
        assert!((a - b).abs() <= 2e-5 + 2e-5 * a.abs());
    }
}

#[test]
#[ignore = "requires Vulkan; checks each CPU reference stage before RGB application"]
fn intermediate_guide_mask_and_every_iteration_match() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let gpu = processor();
    let token = CancellationToken::new();
    let (w, h) = (17, 11);
    let pixels: Vec<f32> = (0..w * h * 3)
        .map(|i| ((i * 17) % 251) as f32 / 200.0 - 0.1)
        .collect();
    let input = LinearRgbImage::new(
        w,
        h,
        w * 3,
        rohditor_image::LinearRgbSpace::CameraNative,
        pixels,
    )
    .expect("fixture");
    let contract = CaptureSharpeningContract::new(
        CaptureSharpening {
            enabled: true,
            radius: 1.2,
            noise_protection: 0.0,
            ..Default::default()
        },
        [0.9, 1.0, 0.8],
    )
    .expect("contract");
    let reference = contract
        .reference_stages(&input, &token)
        .expect("CPU reference stages");
    assert_eq!(reference.len(), 10);
    let resources = Resources::new(&gpu.device, &gpu.queue, w * h, &contract);
    gpu.queue
        .write_buffer(&resources.rgb, 0, bytemuck::cast_slice(input.data()));
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("capture intermediate readback"),
        size: (w * h * 12) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    for (stage, expected) in reference.iter().enumerate() {
        let mut encoder = gpu.encoder();
        match stage {
            0 => gpu.pass(&mut encoder, &resources, &contract, w, h, [0, 0, 0, 0]),
            1 => {
                gpu.blur(&mut encoder, &resources, &contract, w, h, 1);
                gpu.pass(&mut encoder, &resources, &contract, w, h, [2, 0, 0, 0]);
                gpu.blur(&mut encoder, &resources, &contract, w, h, 0);
                gpu.pass(&mut encoder, &resources, &contract, w, h, [3, 0, 0, 0]);
            }
            _ => {
                gpu.blur(&mut encoder, &resources, &contract, w, h, 2);
                gpu.pass(&mut encoder, &resources, &contract, w, h, [4, 0, 0, 0]);
                gpu.blur(&mut encoder, &resources, &contract, w, h, 5);
                gpu.pass(&mut encoder, &resources, &contract, w, h, [5, 0, 0, 0]);
            }
        }
        encoder.copy_buffer_to_buffer(&resources.planes, 0, &staging, 0, (w * h * 12) as u64);
        gpu.submit(encoder, &token).expect("capture stage");
        let (send, receive) = mpsc::sync_channel(1);
        staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = send.send(r);
        });
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        receive.recv().expect("map callback").expect("map");
        {
            let mapped = staging.slice(..).get_mapped_range();
            let actual: &[f32] = bytemuck::cast_slice(&mapped);
            for (i, (a, b)) in expected
                .guide
                .iter()
                .chain(&expected.mask)
                .chain(&expected.estimate)
                .zip(actual)
                .enumerate()
            {
                assert!(
                    (a - b).abs() <= 2e-5 + 2e-5 * a.abs(),
                    "stage={stage} sample={i}: CPU={a} GPU={b}"
                );
            }
        }
        staging.unmap();
    }
}

#[test]
#[ignore = "requires Vulkan; software adapter results are structural qualification only"]
fn asymmetric_tiled_camera_rgb_parity_and_bypass() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let mut gpu = processor();
    let token = CancellationToken::new();
    for (w, h) in [(1, 1), (1, 19), (23, 1), (83, 71), (257, 193)] {
        let stride = w * 3 + 5;
        let mut values = vec![-123.0; stride * h];
        for y in 0..h {
            for x in 0..w {
                for c in 0..3 {
                    values[y * stride + x * 3 + c] = match (x * 17 + y * 31 + c * 7) % 19 {
                        0 => -0.2,
                        1 => 1.7,
                        2 => 1e-7,
                        _ => ((x * 37 + y * 13 + c * 23) % 997) as f32 / 1300.0,
                    };
                }
            }
        }
        let input = LinearRgbImage::new(
            w,
            h,
            stride,
            rohditor_image::LinearRgbSpace::CameraNative,
            values,
        )
        .expect("capture fixture");
        for radius in [0.3, 0.6, 1.2] {
            for noise_protection in [0.0, 0.5, 1.0] {
                let contract = CaptureSharpeningContract::new(
                    CaptureSharpening {
                        enabled: true,
                        amount: 1.0,
                        radius,
                        noise_protection,
                    },
                    [0.7, 1.0, 0.9],
                )
                .expect("capture fixture");
                let mut cpu = input.clone();
                contract
                    .apply_cpu(&mut cpu, &token)
                    .expect("capture fixture");
                for edge in [512, 31] {
                    gpu.maximum_tile_edge = edge;
                    let result = gpu
                        .process(&input, &contract, 0, &token)
                        .expect("capture fixture")
                        .expect("capture fixture");
                    for (i, (a, b)) in cpu.data().iter().zip(result.image.data()).enumerate() {
                        assert!(
                            (a - b).abs() <= 2e-5 + 2e-5 * a.abs(),
                            "{w}x{h} radius={radius} noise={noise_protection} edge={edge} i={i}: cpu={a} gpu={b}"
                        );
                    }
                }
            }
        }
        let bypass = CaptureSharpeningContract::new(CaptureSharpening::default(), [1.0; 3])
            .expect("capture fixture");
        gpu.set_remaining_budget(0);
        assert!(
            gpu.process(&input, &bypass, usize::MAX, &token)
                .expect("capture fixture")
                .is_none()
        );
        gpu.set_remaining_budget(resources::DEFAULT_BUDGET);
    }
    let contract = CaptureSharpeningContract::new(
        CaptureSharpening {
            enabled: true,
            ..Default::default()
        },
        [1.0; 3],
    )
    .expect("capture fixture");
    let invalid = LinearRgbImage::new(
        1,
        1,
        3,
        rohditor_image::LinearRgbSpace::CameraNative,
        vec![f32::NAN, 0.0, 1.0],
    )
    .expect("capture fixture");
    assert!(gpu.process(&invalid, &contract, 0, &token).is_err());
    token.cancel();
    assert!(matches!(
        gpu.process(&invalid, &contract, 0, &token),
        Err(GpuPreviewError::Cancelled)
    ));
}
