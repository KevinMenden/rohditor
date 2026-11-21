//! The ONNX Runtime backends against PyTorch, on the tiny fixture model from
//! `ml/rohditor_ml/train/fixture.py` (random weights, the v1 layout), and the
//! real model when it is available. GPU and real-model tests are ignored.
#![cfg(feature = "onnx-runtime")]

use std::path::{Path, PathBuf};

use rohditor_image::{BayerPattern, MosaicImage};
use rohditor_neural::{
    Backend, ModelFile, ModelId, NeuralDemosaicer, OrtRunner, TileRunner, TileTensor, Tiling,
};

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// Minimal little-endian float32 C-order `.npy` reader (format 1.0).
fn read_npy(path: &Path) -> (Vec<usize>, Vec<f32>) {
    let bytes = std::fs::read(path).expect("read npy");
    assert_eq!(&bytes[..6], b"\x93NUMPY");
    let header_len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
    let header = std::str::from_utf8(&bytes[10..10 + header_len]).expect("ascii header");
    assert!(header.contains("'descr': '<f4'") && header.contains("'fortran_order': False"));
    let shape_text = header
        .split("'shape': (")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .expect("shape");
    let shape = shape_text
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().parse().expect("dimension"))
        .collect();
    let values = bytes[10 + header_len..]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
        .collect();
    (shape, values)
}

fn tiny_model(backend: Backend) -> (OrtRunner, ModelId) {
    let model = ModelFile::read(&data("tiny.onnx")).expect("fixture model");
    let runner = OrtRunner::new(&model, backend).expect("session");
    assert_eq!(runner.backend(), backend);
    (runner, model.id())
}

#[test]
fn onnx_runtime_on_the_cpu_matches_pytorch_on_the_fixture() {
    matches_pytorch_on_the_fixture(Backend::Cpu);
}

#[test]
#[ignore = "needs a GPU for ONNX Runtime's WebGPU provider"]
fn onnx_runtime_on_the_gpu_matches_pytorch_on_the_fixture() {
    matches_pytorch_on_the_fixture(Backend::Gpu);
}

fn matches_pytorch_on_the_fixture(backend: Backend) {
    let (runner, _) = tiny_model(backend);
    let (input_shape, input) = read_npy(&data("tiny_input.npy"));
    let (expected_shape, expected) = read_npy(&data("tiny_expected.npy"));
    let tensor = TileTensor::new(input_shape[1], input_shape[2], input_shape[3], input)
        .expect("input tensor");
    let output = runner.run_tile(&tensor).expect("inference");
    assert_eq!(output.shape().to_vec(), expected_shape);
    let worst = output
        .data()
        .iter()
        .zip(&expected)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    eprintln!("{backend} vs PyTorch: max |diff| {worst:.2e}");
    assert!(
        worst < 1e-4,
        "ONNX Runtime ({backend}) differs from PyTorch by {worst}"
    );
}

/// A smooth asymmetric scene with deterministic noise.
fn scene(width: usize, height: usize, pattern: BayerPattern) -> MosaicImage<f32> {
    let mut state = 0x9e37_79b9_u32;
    let data = (0..width * height)
        .map(|i| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let (x, y) = (i % width, i / width);
            let base = 0.15 + 0.4 * (x as f32 / width as f32) + 0.25 * (y as f32 / height as f32);
            base * [0.45, 1.0, 0.6][pattern.color_at(x, y).channel_index()]
                + 0.01 * (state as f32 / u32::MAX as f32 - 0.5)
        })
        .collect();
    MosaicImage::new(width, height, width, pattern, data).expect("mosaic")
}

fn max_difference(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

#[test]
fn tiles_run_through_onnx_runtime_and_stitch() {
    let tiled = |tiling: Tiling| {
        let (runner, id) = tiny_model(Backend::Cpu);
        NeuralDemosaicer::new(Box::new(runner), id).with_tiling(tiling)
    };
    let mosaic = scene(150, 90, BayerPattern::Grbg);
    let whole = tiled(Tiling::new(512, 32).expect("tiling"))
        .reconstruct(&mosaic, [2.1, 1.0, 1.5], &|| false)
        .expect("whole");
    let stitched = tiled(Tiling::new(64, 32).expect("tiling"))
        .reconstruct(&mosaic, [2.1, 1.0, 1.5], &|| false)
        .expect("tiles");
    let difference = max_difference(whole.data(), stitched.data());
    eprintln!("fixture tiles vs one pass: {difference:.2e}");
    assert!(difference < 1e-5, "stitched tiles differ by {difference}");
}

fn real_model() -> Option<PathBuf> {
    let path = std::env::var_os("ROHDITOR_NEURAL_MODEL").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ml/runs/shippable/export/model.onnx"),
        PathBuf::from,
    );
    path.exists().then_some(path)
}

/// With the real model, contract tiles (1024 + 128 halo) must equal one
/// whole-image pass: the halo covers the receptive field.
#[test]
#[ignore = "needs the model (ROHDITOR_NEURAL_MODEL or ml/runs/shippable/export)"]
fn real_model_tiles_match_one_big_tile() {
    let Some(path) = real_model() else {
        eprintln!("model not found; skipping");
        return;
    };
    let mosaic = scene(2100, 1300, BayerPattern::Rggb);
    let run = |tiling: Tiling| {
        NeuralDemosaicer::load(&path, Backend::Gpu)
            .expect("load")
            .with_tiling(tiling)
            .reconstruct(&mosaic, [2.0, 1.0, 1.6], &|| false)
            .expect("reconstruct")
    };
    let whole = run(Tiling::new(2048, 128).expect("tiling"));
    let tiled = run(Tiling::CONTRACT);
    let difference = max_difference(whole.data(), tiled.data());
    eprintln!("contract tiles vs one pass: max |diff| {difference:.2e}");
    assert!(difference < 1e-5, "stitched tiles differ by {difference}");
}
