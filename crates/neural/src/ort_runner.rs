//! ONNX Runtime on the GPU (WebGPU) or the CPU.
//!
//! The GPU path is ONNX Runtime's WebGPU execution provider, which runs on
//! Dawn and therefore on Vulkan on Linux: no ROCm/CUDA and no shaders of our
//! own. Its shared library (`libwebgpu_dawn.so`) is copied next to the binaries
//! at build time and found through the rpath in `.cargo/config.toml`.
//!
//! Tiles run one after another with deterministic compute, so a given machine
//! and backend always produce the same output. The GPU and CPU differ by
//! float rounding (about one 8-bit level in rare pixels), and neither is
//! guaranteed bit-identical across devices.

use std::fmt;
use std::sync::{Mutex, OnceLock};

use ort::environment::Environment;
use ort::session::Session;
use ort::session::builder::{GraphOptimizationLevel, SessionBuilder};
use ort::value::TensorRef;

use crate::NeuralError;
use crate::model::ModelFile;
use crate::runner::{TileRunner, TileTensor};

/// Where inference runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// ONNX Runtime's WebGPU provider (Vulkan through Dawn on Linux).
    Gpu,
    /// ONNX Runtime's CPU provider, one intra-op thread per core.
    Cpu,
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Gpu => "GPU (WebGPU)",
            Self::Cpu => "CPU",
        })
    }
}

pub struct OrtRunner {
    // `Session::run` needs `&mut`; tiles are sequential, so a lock is free.
    session: Mutex<Session>,
    backend: Backend,
}

impl OrtRunner {
    /// A session on exactly `backend`; fails if it is unavailable.
    pub fn new(model: &ModelFile, backend: Backend) -> Result<Self, NeuralError> {
        let mut builder = base_builder()?;
        if backend == Backend::Gpu {
            builder = builder
                .with_execution_providers([ort::ep::WebGPU::default().build().error_on_failure()])
                .map_err(runtime)?;
        }
        let session = builder.commit_from_memory(model.bytes()).map_err(runtime)?;
        if session.inputs().len() != 1 || session.outputs().len() != 1 {
            return Err(NeuralError::Runtime {
                reason: format!(
                    "the model has {} inputs and {} outputs, expected one each",
                    session.inputs().len(),
                    session.outputs().len()
                ),
            });
        }
        Ok(Self {
            session: Mutex::new(session),
            backend,
        })
    }

    /// The GPU if it works, otherwise the CPU.
    pub fn preferring_gpu(model: &ModelFile) -> Result<Self, NeuralError> {
        Self::new(model, Backend::Gpu).or_else(|error| {
            tracing::warn!(%error, "neural inference falls back to the CPU");
            Self::new(model, Backend::Cpu)
        })
    }

    #[must_use]
    pub const fn backend(&self) -> Backend {
        self.backend
    }
}

fn base_builder() -> Result<SessionBuilder, NeuralError> {
    let threads = std::thread::available_parallelism().map_or(1, usize::from);
    Session::builder(environment()?)
        .map_err(runtime)?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(runtime)?
        .with_intra_threads(threads)
        .map_err(runtime)?
        .with_inter_threads(1)
        .map_err(runtime)?
        .with_deterministic_compute(true)
        .map_err(runtime)
}

impl TileRunner for OrtRunner {
    fn run_tile(&self, input: &TileTensor) -> Result<TileTensor, NeuralError> {
        let tensor = TensorRef::from_array_view((input.shape(), input.data())).map_err(runtime)?;
        let mut session = self.session.lock().map_err(|_| NeuralError::Runtime {
            reason: "the inference session was poisoned by an earlier panic".to_owned(),
        })?;
        let outputs = session.run(ort::inputs![tensor]).map_err(runtime)?;
        let (shape, data) = outputs[0].try_extract_tensor::<f32>().map_err(runtime)?;
        let dims: Vec<usize> = shape
            .iter()
            .map(|&d| usize::try_from(d).unwrap_or(0))
            .collect();
        match dims[..] {
            [1, channels, height, width] => TileTensor::new(channels, height, width, data.to_vec()),
            _ => Err(NeuralError::UnexpectedOutputShape {
                expected: [3, 2 * input.height(), 2 * input.width()],
                actual: dims,
            }),
        }
    }
}

/// ONNX Runtime keeps one environment per process; it is created on first use.
fn environment() -> Result<&'static Environment, NeuralError> {
    static ENVIRONMENT: OnceLock<Result<Environment, String>> = OnceLock::new();
    ENVIRONMENT
        .get_or_init(|| {
            ort::init()
                .with_name("rohditor")
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|reason| NeuralError::Runtime {
            reason: reason.clone(),
        })
}

fn runtime(error: impl std::fmt::Display) -> NeuralError {
    NeuralError::Runtime {
        reason: error.to_string(),
    }
}
