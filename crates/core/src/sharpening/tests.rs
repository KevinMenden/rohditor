use super::*;
use rohditor_image::LinearRgbSpace;

fn image(width: usize, height: usize, guide: &[f32]) -> LinearRgbImage<f32> {
    LinearRgbImage::new(
        width,
        height,
        width * 3,
        LinearRgbSpace::CameraNative,
        guide.iter().flat_map(|v| [*v; 3]).collect(),
    )
    .expect("capture sharpening fixture should succeed")
}

fn enabled() -> CaptureSharpening {
    CaptureSharpening {
        enabled: true,
        ..CaptureSharpening::default()
    }
}

#[test]
fn disabled_and_zero_amount_are_bit_exact_with_signed_hdr_and_padding() {
    let data = vec![-0.0, -0.1, 2.0, 0.2, 0.3, 0.4, 99.0, 98.0];
    let original = LinearRgbImage::new(2, 1, 8, LinearRgbSpace::CameraNative, data)
        .expect("capture sharpening fixture should succeed");
    for settings in [
        CaptureSharpening::default(),
        CaptureSharpening {
            amount: 0.0,
            ..enabled()
        },
    ] {
        let mut result = original.clone();
        apply_cancellable(&mut result, settings, [1.0; 3], &CancellationToken::new())
            .expect("capture sharpening fixture should succeed");
        assert_eq!(
            result
                .data()
                .iter()
                .map(|v| v.to_bits())
                .collect::<Vec<_>>(),
            original
                .data()
                .iter()
                .map(|v| v.to_bits())
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn constants_tiny_images_and_clipped_highlights_remain_unchanged() {
    for (width, height) in [(1, 1), (1, 7), (9, 1), (3, 5)] {
        for value in [-0.1, 0.0, 0.2, 1.0, 2.0] {
            let mut result = image(width, height, &vec![value; width * height]);
            let before = result.data().to_vec();
            apply_cancellable(&mut result, enabled(), [1.0; 3], &CancellationToken::new())
                .expect("capture sharpening fixture should succeed");
            assert_eq!(result.data(), before);
        }
    }
}

fn fixture(width: usize, height: usize) -> Vec<f32> {
    (0..height)
        .flat_map(|y| {
            (0..width).map(move |x| {
                if (x > width / 3 && x < width * 2 / 3 && y > height / 4) || (x == 3 && y == 5) {
                    0.65
                } else {
                    0.2
                }
            })
        })
        .collect()
}

fn blurred(input: &[f32], width: usize) -> Vec<f32> {
    let mut result = vec![0.0; input.len()];
    blur(
        input,
        &mut vec![0.0; input.len()],
        &mut result,
        width,
        &gaussian(0.6),
        &CancellationToken::new(),
    )
    .expect("capture sharpening fixture should succeed");
    result
}

fn mse(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(a, b)| (a - b).powi(2)).sum::<f32>() / a.len() as f32
}

#[test]
fn known_blur_recovers_detail_with_bounded_overshoot_and_usm_comparison() {
    let original = fixture(53, 37);
    let observation = blurred(&original, 53);
    let usm_blur = blurred(&observation, 53);
    let usm: Vec<_> = observation
        .iter()
        .zip(usm_blur)
        .map(|(v, b)| v + 0.5 * (v - b))
        .collect();
    let mut result = image(53, 37, &observation);
    apply_cancellable(&mut result, enabled(), [1.0; 3], &CancellationToken::new())
        .expect("capture sharpening fixture should succeed");
    let sharpened: Vec<_> = result.data().chunks_exact(3).map(|p| p[0]).collect();
    let off_error = mse(&observation, &original);
    let rl_error = mse(&sharpened, &original);
    let usm_error = mse(&usm, &original);
    let overshoot = sharpened.iter().copied().fold(0.65_f32, f32::max) - 0.65;
    let undershoot = 0.2 - sharpened.iter().copied().fold(0.2_f32, f32::min);
    eprintln!(
        "capture fixture: Off MSE={off_error:.7}, USM MSE={usm_error:.7}, RL MSE={rl_error:.7}, overshoot={overshoot:.5}, undershoot={undershoot:.5}"
    );
    assert!(rl_error < off_error * 0.9);
    assert!(overshoot < 0.025 && undershoot < 0.025);
}

#[test]
fn mask_protects_flat_noise_and_highlight_boundaries() {
    let noise: Vec<_> = (0..53 * 37)
        .map(|i| 0.15 + (((i * 7919) % 101) as f32 / 100.0 - 0.5) * 0.002)
        .collect();
    let mut result = image(53, 37, &noise);
    apply_cancellable(&mut result, enabled(), [1.0; 3], &CancellationToken::new())
        .expect("capture sharpening fixture should succeed");
    let output: Vec<_> = result.data().chunks_exact(3).map(|p| p[0]).collect();
    let mean = vec![0.15; noise.len()];
    let noise_gain = (mse(&output, &mean) / mse(&noise, &mean)).sqrt();
    eprintln!("capture flat-patch noise gain={noise_gain:.6}");
    assert!(noise_gain <= 1.01);
    let mut clipped = image(
        9,
        7,
        &(0..63)
            .map(|i| if i % 9 > 3 { 1.0 } else { 0.1 })
            .collect::<Vec<_>>(),
    );
    apply_cancellable(&mut clipped, enabled(), [1.0; 3], &CancellationToken::new())
        .expect("capture sharpening fixture should succeed");
    for (i, rgb) in clipped.data().chunks_exact(3).enumerate() {
        if i % 9 > 3 {
            assert_eq!(rgb, [1.0; 3]);
        }
    }
}

#[test]
fn rgb_gain_preserves_signed_channel_ratios_and_is_thread_deterministic() {
    let guide = blurred(&fixture(53, 37), 53);
    let mut input = image(53, 37, &guide);
    for rgb in input.data_mut().chunks_exact_mut(3) {
        rgb[0] *= -0.2;
        rgb[2] *= 2.0;
    }
    let process = |threads| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .expect("capture sharpening fixture should succeed");
        let mut result = input.clone();
        pool.install(|| {
            apply_cancellable(&mut result, enabled(), [10.0; 3], &CancellationToken::new())
        })
        .expect("capture sharpening fixture should succeed");
        result
    };
    let result = process(1);
    assert_eq!(result.data(), process(3).data());
    assert_ne!(result.data(), input.data());
    for rgb in result.data().chunks_exact(3) {
        assert!((rgb[0] / rgb[1] + 0.2).abs() < 1e-6);
        assert!((rgb[2] / rgb[1] - 2.0).abs() < 1e-6);
    }
}

#[test]
fn cancellation_invalid_settings_nonfinite_and_overflow_are_rejected() {
    let mut input = image(3, 5, &fixture(3, 5));
    let token = CancellationToken::new();
    token.cancel();
    assert!(matches!(
        apply_cancellable(&mut input, enabled(), [1.0; 3], &token),
        Err(PipelineError::Cancelled)
    ));
    assert!(scratch_bytes(usize::MAX, 2).is_err());
    let invalid = CaptureSharpening {
        radius: f32::NAN,
        ..enabled()
    };
    assert!(apply_cancellable(&mut input, invalid, [1.0; 3], &CancellationToken::new()).is_err());
    input.data_mut()[4] = f32::INFINITY;
    assert!(matches!(
        apply_cancellable(&mut input, enabled(), [1.0; 3], &CancellationToken::new()),
        Err(PipelineError::NonFiniteImageData { .. })
    ));
}

#[test]
#[ignore = "full-resolution capture sharpening latency and cancellation benchmark"]
fn benchmark_capture_sharpening() {
    for (width, height) in [(6000, 4000), (8000, 6000)] {
        let mut input = image(width, height, &fixture(width, height));
        let started = std::time::Instant::now();
        apply_cancellable(&mut input, enabled(), [1.0; 3], &CancellationToken::new())
            .expect("capture sharpening fixture should succeed");
        eprintln!(
            "capture {width}x{height}: {:.1} ms, scratch {:.1} MiB, RGB + scratch {:.1} MiB",
            started.elapsed().as_secs_f64() * 1000.0,
            scratch_bytes(width, height).expect("capture sharpening fixture should succeed") as f64
                / 1048576.0,
            (scratch_bytes(width, height).expect("capture sharpening fixture should succeed")
                + input.data().len() * 4) as f64
                / 1048576.0
        );
        let token = CancellationToken::new();
        let worker_token = token.clone();
        std::thread::scope(|scope| {
            let worker =
                scope.spawn(|| apply_cancellable(&mut input, enabled(), [1.0; 3], &worker_token));
            std::thread::sleep(std::time::Duration::from_millis(100));
            let start = std::time::Instant::now();
            token.cancel();
            assert!(matches!(
                worker
                    .join()
                    .expect("capture sharpening fixture should succeed"),
                Err(PipelineError::Cancelled)
            ));
            eprintln!(
                "capture cancellation latency: {:.1} ms",
                start.elapsed().as_secs_f64() * 1000.0
            );
        });
    }
}
