//! 24 MP neural demosaic through ONNX Runtime, on the GPU (WebGPU) and the CPU.
//!
//! The model is not in git: point `ROHDITOR_NEURAL_MODEL` at a `model.onnx`
//! with an absolute path (benches run in the crate directory), otherwise the
//! benchmark is skipped.
//!
//!     ROHDITOR_NEURAL_MODEL=$PWD/ml/runs/shippable/export/model.onnx \
//!         cargo bench -p rohditor-neural

use criterion::{Criterion, criterion_group, criterion_main};
use rohditor_image::{BayerPattern, MosaicImage};
use rohditor_neural::{Backend, NeuralDemosaicer};

const WIDTH: usize = 6000;
const HEIGHT: usize = 4000;

fn mosaic() -> MosaicImage<f32> {
    // Deterministic texture with some noise-like variation.
    let mut state = 0x1234_5678_u32;
    let data = (0..WIDTH * HEIGHT)
        .map(|i| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let (x, y) = (i % WIDTH, i / WIDTH);
            0.1 + 0.3 * ((x as f32 * 0.01).sin() * (y as f32 * 0.013).cos()).abs()
                + (state as f32 / u32::MAX as f32) * 0.02
        })
        .collect();
    MosaicImage::new(WIDTH, HEIGHT, WIDTH, BayerPattern::Rggb, data).expect("mosaic")
}

fn bench(c: &mut Criterion) {
    let Some(path) = std::env::var_os("ROHDITOR_NEURAL_MODEL") else {
        eprintln!("ROHDITOR_NEURAL_MODEL is not set; skipping the neural benchmark");
        return;
    };
    let mosaic = mosaic();
    let mut group = c.benchmark_group("neural");
    group.sample_size(10);
    for (name, backend) in [("gpu_ort_24mp", Backend::Gpu), ("cpu_ort_24mp", Backend::Cpu)] {
        // `Gpu` falls back to the CPU; the log says which one ran.
        let demosaicer = NeuralDemosaicer::load(path.as_ref(), backend).expect("load the model");
        group.bench_function(name, |b| {
            b.iter(|| {
                demosaicer
                    .reconstruct(&mosaic, [2.0, 1.0, 1.5], &|| false)
                    .expect("reconstruct")
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
