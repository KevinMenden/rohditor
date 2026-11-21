//! Reconstruction tests with in-process runners (no ONNX Runtime needed).

use std::sync::atomic::{AtomicUsize, Ordering};

use rohditor_image::BayerPattern;

use super::*;

/// The model's fixed bilinear branch (`BilinearDemosaic` in
/// `ml/rohditor_ml/train/model.py`): scatter the packed channels, then a 3x3
/// kernel per colour with reflect padding. Its support is one pixel, well
/// inside any halo, so tiling must not change its output at all.
struct BilinearRunner;

impl TileRunner for BilinearRunner {
    fn run_tile(&self, input: &TileTensor) -> Result<TileTensor, NeuralError> {
        let (half_w, half_h) = (input.width(), input.height());
        let (width, height) = (2 * half_w, 2 * half_h);
        let colour_of = [0, 1, 1, 2];
        let mut sparse = vec![0.0; 3 * width * height];
        for y in 0..height {
            for x in 0..width {
                let channel = (y % 2) * 2 + x % 2;
                sparse[colour_of[channel] * width * height + y * width + x] =
                    input.plane(channel)[(y / 2) * half_w + x / 2];
            }
        }
        let rb = [[1.0, 2.0, 1.0], [2.0, 4.0, 2.0], [1.0, 2.0, 1.0]];
        let g = [[0.0, 1.0, 0.0], [1.0, 4.0, 1.0], [0.0, 1.0, 0.0]];
        let reflect = |i: isize, n: usize| layout::mirror(i, n);
        let mut out = vec![0.0; 3 * width * height];
        for (colour, kernel) in [rb, g, rb].into_iter().enumerate() {
            let plane = &sparse[colour * width * height..(colour + 1) * width * height];
            for y in 0..height {
                for x in 0..width {
                    let mut sum = 0.0_f32;
                    for (ky, row) in kernel.iter().enumerate() {
                        for (kx, weight) in row.iter().enumerate() {
                            let sy = reflect(y as isize + ky as isize - 1, height);
                            let sx = reflect(x as isize + kx as isize - 1, width);
                            sum += weight / 4.0 * plane[sy * width + sx];
                        }
                    }
                    out[colour * width * height + y * width + x] = sum;
                }
            }
        }
        TileTensor::new(3, height, width, out)
    }
}

fn test_id() -> ModelId {
    ModelId {
        contract_version: 1,
        sha256: [7; 32],
    }
}

/// A smooth asymmetric scene, sampled through the CFA.
fn scene_mosaic(width: usize, height: usize, pattern: BayerPattern) -> MosaicImage<f32> {
    let data = (0..height)
        .flat_map(|y| {
            (0..width).map(move |x| {
                let base = 0.2 + 0.5 * (x as f32 / width as f32) + 0.2 * (y as f32 / height as f32);
                base * [0.5, 1.0, 0.7][pattern.color_at(x, y).channel_index()]
            })
        })
        .collect();
    MosaicImage::new(width, height, width, pattern, data).expect("mosaic")
}

fn reconstruct(tiling: Tiling, mosaic: &MosaicImage<f32>) -> LinearRgbImage<f32> {
    NeuralDemosaicer::new(Box::new(BilinearRunner), test_id())
        .with_tiling(tiling)
        .reconstruct(mosaic, [2.0, 1.0, 1.6], &|| false)
        .expect("reconstruct")
}

#[test]
fn stitched_tiles_match_one_big_tile() {
    for pattern in [BayerPattern::Rggb, BayerPattern::Gbrg] {
        let mosaic = scene_mosaic(150, 85, pattern);
        let tiled = reconstruct(Tiling::new(32, 16).expect("tiling"), &mosaic);
        let whole = reconstruct(Tiling::new(512, 16).expect("tiling"), &mosaic);
        assert_eq!((tiled.width(), tiled.height()), (150, 85));
        assert_eq!(tiled.data(), whole.data(), "{pattern:?}");
    }
}

#[test]
fn a_flat_field_stays_flat_in_camera_space() {
    for pattern in [BayerPattern::Rggb, BayerPattern::Grbg, BayerPattern::Bggr] {
        let (width, height) = (37, 21);
        let levels = [0.25, 0.5, 0.4];
        let data = (0..width * height)
            .map(|i| levels[pattern.color_at(i % width, i / width).channel_index()])
            .collect();
        let mosaic = MosaicImage::new(width, height, width, pattern, data).expect("mosaic");
        let rgb = reconstruct(Tiling::new(16, 16).expect("tiling"), &mosaic);
        assert_eq!(rgb.space(), LinearRgbSpace::CameraNative);
        for pixel in rgb.data().chunks(3) {
            for (value, level) in pixel.iter().zip(levels) {
                assert!((value - level).abs() < 1e-6, "{pattern:?}: {pixel:?}");
            }
        }
    }
}

#[test]
fn cancellation_between_tiles_returns_cancelled() {
    struct Counting(AtomicUsize);
    impl TileRunner for Counting {
        fn run_tile(&self, input: &TileTensor) -> Result<TileTensor, NeuralError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            BilinearRunner.run_tile(input)
        }
    }
    let runner = std::sync::Arc::new(Counting(AtomicUsize::new(0)));
    struct Shared(std::sync::Arc<Counting>);
    impl TileRunner for Shared {
        fn run_tile(&self, input: &TileTensor) -> Result<TileTensor, NeuralError> {
            self.0.run_tile(input)
        }
    }
    let demosaicer = NeuralDemosaicer::new(Box::new(Shared(runner.clone())), test_id())
        .with_tiling(Tiling::new(16, 16).expect("tiling"));
    let mosaic = scene_mosaic(64, 64, BayerPattern::Rggb);
    let cancel_after_two = || runner.0.load(Ordering::SeqCst) >= 2;
    let error = demosaicer
        .reconstruct(&mosaic, [1.0; 3], &cancel_after_two)
        .expect_err("cancelled");
    assert_eq!(error, NeuralError::Cancelled);
    assert_eq!(
        runner.0.load(Ordering::SeqCst),
        2,
        "no tile runs after cancellation"
    );
}

#[test]
fn a_wrongly_shaped_model_output_is_rejected() {
    struct Shrinking;
    impl TileRunner for Shrinking {
        fn run_tile(&self, input: &TileTensor) -> Result<TileTensor, NeuralError> {
            TileTensor::new(
                3,
                input.height(),
                input.width(),
                vec![0.0; 3 * input.height() * input.width()],
            )
        }
    }
    let error = NeuralDemosaicer::new(Box::new(Shrinking), test_id())
        .reconstruct(&scene_mosaic(20, 20, BayerPattern::Rggb), [1.0; 3], &|| {
            false
        })
        .expect_err("half-size output");
    assert!(
        matches!(error, NeuralError::UnexpectedOutputShape { .. }),
        "{error}"
    );
}
