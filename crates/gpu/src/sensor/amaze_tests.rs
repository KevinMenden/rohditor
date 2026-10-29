//! GPU AMaZE against the CPU reference.
//!
//! Parity contract. AMaZE makes many threshold decisions on cancelling f32
//! arithmetic (variances such as `3 * sum(x^2) - (sum x)^2`), and a GPU's fused
//! multiply-adds and non-exact division can flip a decision that the CPU takes
//! the other way. Smooth, gradient and clipped inputs have no such near-ties and
//! must match within [`close`]. Noisy inputs and real photographs may have
//! isolated flipped samples; they are bounded by a small outlier fraction and a
//! maximum error (measured on a 24 MP photo: 1.6e-5 of samples, 3.2e-3 max).

use rohditor_core::{CancellationToken, CaptureSharpeningContract, RawCropPolicy, normalize_raw};
use rohditor_demosaic::{
    ChannelBalance, DemosaicAlgorithm, WhiteBalanceGains, amaze_tiles, demosaic,
};
use rohditor_edit::CaptureSharpening;

use super::amaze::AmazeExecutor;
use super::demosaic_tests::{description, frame, highlighted, readback, readback_planes};
use super::tests::processor;
use super::{GpuHighlightedMosaic, GpuSensorProcessor};
use crate::spatial::{resources::ResidentLayout, source::ResidentCameraPlanes};

/// Pixel patterns of `demosaic_tests::frame`: flat, gradient and clipped inputs
/// match exactly; the two noisy ones only statistically.
const EXACT_KINDS: [usize; 3] = [0, 2, 5];
const NOISY_KINDS: [usize; 2] = [6, 7];

fn close(actual: f32, expected: f32, slack: f32) -> bool {
    (actual - expected).abs() <= slack * (2e-5 + 2e-5 * expected.abs())
}

fn capture_off() -> CaptureSharpeningContract {
    CaptureSharpeningContract::new(CaptureSharpening::default(), [1.0; 3]).expect("capture off")
}

/// Compare a full image; `exact` kinds must all be close, noisy ones may have a
/// few flipped decisions.
fn compare(label: &str, actual: &[f32], expected: &[f32], exact: bool, slack: f32) {
    let outliers = actual
        .iter()
        .zip(expected)
        .filter(|&(&a, &b)| !close(a, b, slack))
        .count();
    let worst = actual
        .iter()
        .zip(expected)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    if exact {
        assert_eq!(outliers, 0, "{label}: {outliers} outliers, worst {worst:e}");
    } else {
        assert!(
            outliers * 200 <= actual.len() && worst <= 4e-3 * slack,
            "{label}: {outliers}/{} outliers, worst {worst:e}",
            actual.len()
        );
    }
}

#[test]
#[ignore = "requires Vulkan; AMaZE parity for all Bayer phases, partial tiles, ring and seams"]
fn amaze_matches_cpu_with_partial_tiles_border_ring_and_mosaic_seams() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let mut gpu = processor();
    gpu.force_maximum_tile_edge(31);
    let capture = capture_off();
    for pattern in ["RGGB", "BGGR", "GRBG", "GBRG"] {
        for (width, height) in [
            // Entirely bilinear: not larger than twice the 16 px border.
            (24, 39),
            (32, 200),
            // One partial tile; the 2x2 tile grids; and sizes where the last
            // tile column or row lies inside the border ring (450 = 2 * 224 + 2).
            (33, 33),
            (97, 70),
            (241, 257),
            (450, 300),
            (230, 460),
        ] {
            for kind in EXACT_KINDS.into_iter().chain(NOISY_KINDS) {
                let frame = frame(pattern, width, height, kind);
                let description = description(&frame, DemosaicAlgorithm::Amaze, false);
                let mosaic = normalize_raw(&frame, RawCropPolicy::Recommended)
                    .expect("CPU normalized mosaic");
                let reference = demosaic(
                    &mosaic,
                    description.demosaic_balance(),
                    DemosaicAlgorithm::Amaze,
                )
                .expect("CPU AMaZE reference");
                let (source, metrics) = gpu
                    .demosaic(
                        highlighted(&gpu, &frame, &description),
                        &description,
                        &capture,
                        &CancellationToken::new(),
                    )
                    .expect("GPU AMaZE");
                assert_eq!(metrics.diagnostic_readback_bytes, 4);
                let actual = readback(&gpu, &source);
                let label = format!("AMaZE {pattern} {width}x{height} kind={kind}");
                compare(
                    &label,
                    &actual,
                    reference.data(),
                    EXACT_KINDS.contains(&kind),
                    1.0,
                );
                // The measured sample always survives bit for bit.
                for y in 0..height {
                    for x in 0..width {
                        let channel = mosaic.pattern().color_at(x, y).channel_index();
                        assert_eq!(
                            actual[(y * width + x) * 3 + channel].to_bits(),
                            mosaic.sample(x, y).to_bits(),
                            "{label}: measured sample at {x},{y}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "requires Vulkan; GPU AMaZE with a non-unit as-shot channel balance"]
fn amaze_matches_cpu_under_a_non_unit_channel_balance() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let gpu = processor();
    let capture = capture_off();
    for pattern in ["RGGB", "GBRG"] {
        for (width, height) in [(199, 201), (260, 250)] {
            for kind in EXACT_KINDS {
                let mut frame = frame(pattern, width, height, kind);
                frame.info.as_shot_white_balance = [Some(2.1), Some(1.0), Some(2.6), None];
                let description = description(&frame, DemosaicAlgorithm::Amaze, false);
                let balance = description.demosaic_balance();
                assert!((balance.red - 2.1).abs() < 1e-6 && (balance.blue - 2.6).abs() < 1e-6);
                let mosaic = normalize_raw(&frame, RawCropPolicy::Recommended)
                    .expect("CPU normalized mosaic");
                let reference = demosaic(&mosaic, balance, DemosaicAlgorithm::Amaze)
                    .expect("CPU balanced AMaZE reference");
                let (source, _) = gpu
                    .demosaic(
                        highlighted(&gpu, &frame, &description),
                        &description,
                        &capture,
                        &CancellationToken::new(),
                    )
                    .expect("GPU balanced AMaZE");
                // Reconstruction is on the balanced mosaic; errors grow by the
                // inverse gain when scaled back.
                let slack = ChannelBalance::new(balance)
                    .inverse()
                    .into_iter()
                    .fold(1.0_f32, f32::max);
                compare(
                    &format!("balanced AMaZE {pattern} {width}x{height} kind={kind}"),
                    &readback(&gpu, &source),
                    reference.data(),
                    true,
                    slack,
                );
            }
        }
    }
}

/// Everything an executor needs to run on its own, without the
/// `GpuSensorProcessor::demosaic` wrapper: used to drive batches directly.
struct ExecutorFixture {
    executor: AmazeExecutor,
    planes: ResidentCameraPlanes,
    validation: wgpu::Buffer,
    view: wgpu::TextureView,
    mosaic: GpuHighlightedMosaic,
}

impl ExecutorFixture {
    fn new(gpu: &GpuSensorProcessor, mosaic: GpuHighlightedMosaic, batch: usize) -> Self {
        let (width, height) = mosaic.dimensions();
        let layout = ResidentLayout::new_generated(
            width,
            height,
            mosaic.estimated_bytes(),
            AmazeExecutor::reserved_bytes(batch) + 64 * 1024,
            gpu.budget,
            &gpu.device.limits(),
            gpu.maximum_tile_edge,
        )
        .expect("AMaZE diagnostic layout");
        let planes = ResidentCameraPlanes::with_budget(&gpu.device, layout, gpu.budget)
            .expect("AMaZE diagnostic planes");
        let validation = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("AMaZE test flag"),
            size: 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let view = mosaic._texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let executor = AmazeExecutor::new(
            gpu,
            ChannelBalance::new(WhiteBalanceGains::identity()),
            batch,
        )
        .expect("AMaZE executor");
        Self {
            executor,
            planes,
            validation,
            view,
            mosaic,
        }
    }
}

#[test]
#[ignore = "requires Vulkan; any batch size over poisoned scratch reproduces the CPU core"]
fn amaze_batches_of_any_size_ignore_stale_scratch() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let gpu = processor();
    for (width, height) in [(241, 257), (450, 300)] {
        for kind in [2, 5] {
            let frame = frame("GRBG", width, height, kind);
            let description = description(&frame, DemosaicAlgorithm::Amaze, false);
            let mosaic =
                normalize_raw(&frame, RawCropPolicy::Recommended).expect("CPU normalized mosaic");
            let reference = demosaic(
                &mosaic,
                WhiteBalanceGains::identity(),
                DemosaicAlgorithm::Amaze,
            )
            .expect("CPU AMaZE reference");
            // Batches of 1 and 3 reuse one slot or run a ragged last batch.
            for batch in [1, 3, 16] {
                let fixture =
                    ExecutorFixture::new(&gpu, highlighted(&gpu, &frame, &description), batch);
                fixture.executor.poison_scratch(&gpu);
                let submissions = fixture
                    .executor
                    .run(
                        &gpu,
                        &fixture.mosaic,
                        &fixture.view,
                        &fixture.planes,
                        &fixture.validation,
                        &CancellationToken::new(),
                    )
                    .expect("GPU AMaZE batches");
                let tiles = amaze_tiles(width, height).count();
                assert_eq!(submissions, tiles.div_ceil(batch));
                // The executor writes only the tiles' cores; the border ring is
                // the bilinear base that `demosaic` lays down first.
                let actual = readback_planes(&gpu, &fixture.planes);
                for tile in amaze_tiles(width, height) {
                    let window = tile.write_window(width, height);
                    for y in window.y {
                        for x in window.x.clone() {
                            for channel in 0..3 {
                                let i = (y * width + x) * 3 + channel;
                                assert!(
                                    close(actual[i], reference.data()[i], 1.0),
                                    "batch={batch} {width}x{height} kind={kind} ({x}, {y}, {channel}): GPU {}, CPU {}",
                                    actual[i],
                                    reference.data()[i]
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The fixture of the CPU `stage_fixtures` test, as an RGGB mosaic.
fn stage_fixture_values() -> Vec<f32> {
    let (width, height) = (53, 47);
    (0..width * height)
        .map(|i| {
            let (x, y) = (i % width, i / width);
            match (x + 3 * y) % 11 {
                0 => -0.0,
                1 => 1.0e-6,
                2 => -1.0e-6,
                3 => 1.5,
                4 => -0.2,
                5 => 2.0,
                _ => ((x * 37 + y * 19 + x * y) % 101) as f32 / 63.0 - 0.4,
            }
        })
        .collect()
}

/// Scratch word offset of a named plane (see the plane table in `prelude.wgsl`)
/// and whether it is a half-resolution "pair" plane.
fn plane_offset(name: &str) -> (usize, bool) {
    const FULL: usize = 65536;
    const HALF: usize = 32768;
    let full = [
        "cfa",
        "green",
        "delhvsqsum",
        "dirwts0",
        "dirwts1",
        "vcd",
        "hcd",
        "vcdalt",
        "hcdalt",
        "cddiffsq",
        "dgintv",
        "dginth",
    ];
    let pairs = [
        "hvwt", "dgrb0", "dgrb1", "dgrb2_h", "dgrb2_v", "delp", "delm", "sq1m", "sq1p", "nyqutest",
        "pmwt", "rbm", "rbp", "rbint", "nyquist", "nyquist2",
    ];
    if let Some(index) = full.iter().position(|&plane| plane == name) {
        (index * FULL, false)
    } else {
        let index = pairs
            .iter()
            .position(|&plane| plane == name)
            .expect("known plane");
        (12 * FULL + index * HALF, true)
    }
}

/// (plane, tile row, tile column, value) of the fixture after every stage, from
/// the CPU `stage_fixtures` test. Sites (25, 57) and (26, 56) are voted Nyquist
/// sites, (32, 32) is flagged but outvoted, (31, 33) and (33, 35) are plain.
const PROBES: &[(&str, usize, usize, f32)] = &[
    ("nyqutest", 25, 57, 0.08931726),
    ("nyquist", 25, 57, 1.0),
    ("nyquist2", 25, 57, 1.0),
    ("hvwt", 25, 57, 0.6249956),
    ("dgrb2_h", 25, 57, 0.087723866),
    ("dgrb2_v", 25, 57, 0.13902828),
    ("dgrb0", 25, 57, 0.7487581),
    ("dgrb1", 25, 57, 1.1797017),
    ("green", 25, 57, 1.1797007),
    ("pmwt", 25, 57, 0.59881574),
    ("rbint", 25, 57, -0.018249098),
    ("nyquist2", 26, 56, 1.0),
    ("hvwt", 26, 56, 0.77032536),
    ("dgrb0", 26, 56, 0.043839097),
    ("dgrb1", 26, 56, 0.51848173),
    ("green", 26, 56, -0.1561609),
    ("nyqutest", 32, 32, 0.051671088),
    ("nyquist", 32, 32, 1.0),
    ("nyquist2", 32, 32, 0.0),
    ("hvwt", 32, 32, 0.22237617),
    ("dgrb2_h", 32, 32, 0.0),
    ("green", 32, 32, 0.7930795),
    ("cfa", 31, 33, 0.5206349),
    ("vcd", 31, 33, 0.1482284),
    ("hcd", 31, 33, -0.8253968),
    ("vcdalt", 31, 33, 0.1482284),
    ("hcdalt", 31, 33, -0.8630527),
    ("cddiffsq", 31, 33, 0.947946),
    ("dgintv", 31, 33, 1.6694309),
    ("dginth", 31, 33, 0.58292013),
    ("delhvsqsum", 31, 33, 1.671696),
    ("dirwts0", 31, 33, 2.3333435),
    ("dirwts1", 31, 33, 1.5746133),
    ("delp", 31, 33, 1.7492063),
    ("delm", 31, 33, 1.5),
    ("sq1m", 31, 33, 2.0440006),
    ("sq1p", 31, 33, 1.7466767),
    ("hvwt", 31, 33, 0.16582216),
    ("pmwt", 31, 33, 0.5514841),
    ("rbm", 31, 33, 0.69720316),
    ("rbp", 31, 33, 0.85232896),
    ("rbint", 31, 33, 0.6516937),
    ("dgrb0", 31, 33, -0.929778),
    ("dgrb1", 31, 33, -0.6639482),
    ("green", 31, 33, -0.14331329),
    ("vcd", 33, 35, 0.4021777),
    ("hcd", 33, 35, 1.7952101),
    ("cddiffsq", 33, 35, 1.9405394),
    ("delp", 33, 35, 0.098411694),
    ("delm", 33, 35, 0.04444444),
    ("hvwt", 33, 35, 0.1975245),
    ("pmwt", 33, 35, 0.60838723),
    ("rbint", 33, 35, -0.12558362),
    ("dgrb0", 33, 35, 1.3346572),
    ("dgrb1", 33, 35, 1.5200521),
    ("green", 33, 35, 1.320052),
];

#[test]
#[ignore = "requires Vulkan; compares GPU AMaZE scratch stages with frozen CPU probes"]
fn amaze_intermediate_stages_match_frozen_cpu_probes() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let gpu = processor();
    let frame = frame("BGGR", 53, 47, 0); // odd crop shifts the selected phase to RGGB
    let desc = description(&frame, DemosaicAlgorithm::Amaze, false);
    let mosaic = highlighted(&gpu, &frame, &desc);
    assert_eq!(
        mosaic.normalization.crop().pattern(),
        rohditor_image::BayerPattern::Rggb
    );
    assert_eq!(mosaic.layout.layers, 1);
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &mosaic._texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(&stage_fixture_values()),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(53 * 4),
            rows_per_image: Some(47),
        },
        wgpu::Extent3d {
            width: 53,
            height: 47,
            depth_or_array_layers: 1,
        },
    );
    let fixture = ExecutorFixture::new(&gpu, mosaic, 1);
    // Stale data from an earlier tile must not matter.
    fixture.executor.poison_scratch(&gpu);
    let scratch = fixture
        .executor
        .run_and_read_scratch(
            &gpu,
            &fixture.mosaic,
            &fixture.view,
            &fixture.planes,
            &fixture.validation,
            &CancellationToken::new(),
        )
        .expect("GPU AMaZE scratch snapshot");
    for &(name, row, col, expected) in PROBES {
        let (offset, pairs) = plane_offset(name);
        let index = if pairs {
            row * 128 + col / 2
        } else {
            row * 256 + col
        };
        let actual = scratch[offset + index];
        assert!(
            close(actual, expected, 1.0),
            "AMaZE {name} at ({row}, {col}): GPU {actual}, CPU {expected}"
        );
    }
}

#[test]
#[ignore = "requires Vulkan; cancellation between AMaZE tile batches"]
fn amaze_cancellation_between_batches_keeps_immutable_raw_for_recovery() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let gpu = processor();
    let before = crate::gpu_memory_reservations().current_bytes;
    let frame = frame("RGGB", 241, 257, 6);
    let original = frame.mosaic.clone();
    let description = description(&frame, DemosaicAlgorithm::Amaze, false);
    let fixture = ExecutorFixture::new(&gpu, highlighted(&gpu, &frame, &description), 1);
    let token = CancellationToken::new();
    assert!(matches!(
        fixture.executor.cancel_after_first_batch(
            &gpu,
            &fixture.mosaic,
            &fixture.view,
            &fixture.planes,
            &fixture.validation,
            &token
        ),
        Err(crate::GpuPreviewError::Cancelled)
    ));
    assert_eq!(frame.mosaic, original);
    drop(fixture);
    assert_eq!(crate::gpu_memory_reservations().current_bytes, before);
    normalize_raw(&frame, RawCropPolicy::Recommended).expect("CPU recovery after cancellation");
}
