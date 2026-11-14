//! Per-algorithm qualification of GPU sensor development (Off/Clip highlights)
//! against the CPU reference on the private Sony corpus. Each algorithm and
//! highlight mode is judged separately so `auto` can admit them independently.
//! Adapter identity is printed first; software adapters only validate structure.
use super::demosaic_tests::{description, readback};
use super::tests::processor;
use rohditor_core::{CancellationToken, HighlightExecution, RawCropPolicy, normalize_raw};
use rohditor_demosaic::{ChannelBalance, DemosaicAlgorithm, demosaic};
use rohditor_edit::CaptureSharpening;
use rohditor_raw::{RawDecoder, RawlerDecoder};
use std::time::{Duration, Instant};

const CORPUS: [&str; 6] = [
    "DSC00851.ARW",
    "DSC00554.ARW",
    "DSC01109.ARW",
    "DSC02382.ARW",
    "DSC03211.ARW",
    "DSC03883.ARW",
];
const ALGORITHMS: [DemosaicAlgorithm; 3] = [
    DemosaicAlgorithm::Bilinear,
    DemosaicAlgorithm::MalvarHeCutler,
    DemosaicAlgorithm::Rcd,
];
/// Repeated dispatches per image after the first; the first one is reported as cold.
const WARM_RUNS: usize = 3;

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

/// Aggregated results for one (algorithm, highlight mode) cell of the matrix.
#[derive(Default)]
struct Cell {
    cold: Vec<Duration>,
    warm: Vec<Duration>,
    cpu: Vec<Duration>,
    max_error: f32,
    outlier_fraction: f64,
    upload: u64,
    readback: u64,
    submissions: u64,
    peak: u64,
    images: usize,
}

#[test]
#[ignore = "private 24 MP Sony RAWs and Vulkan; prints the Off/Clip x bilinear/MHC/RCD matrix"]
fn private_off_clip_qualification_matrix() -> Result<(), Box<dyn std::error::Error>> {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let gpu = processor();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/private");
    let capture = crate::capture_only_contract(CaptureSharpening::default(), [1.0; 3])?;
    let token = CancellationToken::new();
    let mut cells: Vec<((DemosaicAlgorithm, bool), Cell)> = ALGORITHMS
        .iter()
        .flat_map(|&a| [(a, false), (a, true)])
        .map(|key| (key, Cell::default()))
        .collect();
    for name in CORPUS {
        let frame = RawlerDecoder::default().decode(&root.join(name))?;
        for ((algorithm, clip), cell) in &mut cells {
            let desc = description(&frame, *algorithm, *clip);
            let mut mosaic = normalize_raw(&frame, RawCropPolicy::Recommended)?;
            if let HighlightExecution::Clip(levels) = desc.highlight_execution() {
                mosaic = rohditor_highlight::clip(mosaic, levels)?.mosaic;
            }
            let started = Instant::now();
            let reference = demosaic(&mosaic, desc.demosaic_balance(), *algorithm)?;
            cell.cpu.push(started.elapsed());
            drop(mosaic);
            // f32 rounding differences scale with the inverse balance gain.
            let slack = ChannelBalance::new(desc.demosaic_balance())
                .inverse()
                .into_iter()
                .fold(1.0_f32, f32::max);
            for run in 0..=WARM_RUNS {
                let started = Instant::now();
                let (normalized, norm) = gpu.normalize(&frame, desc.normalization(), &token)?;
                let (highlighted, _, hl) = gpu.apply_highlight(normalized, &desc, &token)?;
                let (source, metrics) = gpu.demosaic(highlighted, &desc, &capture, &token)?;
                let total = started.elapsed();
                if run == 0 {
                    cell.cold.push(total);
                    // Parity and byte counters are deterministic; check them once per image.
                    let actual = readback(&gpu, &source);
                    let (mut outliers, mut maximum) = (0usize, 0.0_f32);
                    for (&a, &b) in actual.iter().zip(reference.data()) {
                        let error = (a - b).abs();
                        outliers += (error > slack * (2e-5 + 2e-5 * b.abs())) as usize;
                        maximum = maximum.max(error);
                    }
                    if maximum > 5e-3 {
                        eprintln!(
                            "{name} {algorithm:?} clip={clip}: max error {maximum:e}, {outliers} outliers"
                        );
                    }
                    cell.max_error = cell.max_error.max(maximum);
                    cell.outlier_fraction = cell
                        .outlier_fraction
                        .max(outliers as f64 / actual.len() as f64);
                    cell.upload = norm.uploaded_bytes;
                    cell.readback =
                        hl.diagnostic_readback_bytes + metrics.diagnostic_readback_bytes;
                    cell.submissions = metrics.submissions as u64;
                    cell.peak = cell.peak.max(metrics.estimated_gpu_bytes);
                } else {
                    cell.warm.push(total);
                }
                drop(source);
            }
            cell.images += 1;
        }
    }
    eprintln!(
        "algorithm  highlight  images  cpu_p50  cold_p50  warm_p50  warm_p95  max_err  outlier_frac  upload  diag_readback  submissions  peak_gpu"
    );
    for ((algorithm, clip), mut cell) in cells {
        for v in [&mut cell.cold, &mut cell.warm, &mut cell.cpu] {
            v.sort_unstable();
        }
        eprintln!(
            "{algorithm:?}  {}  {}  {:?}  {:?}  {:?}  {:?}  {:e}  {:e}  {}  {}  {}  {}",
            if clip { "Clip" } else { "Off" },
            cell.images,
            percentile(&cell.cpu, 0.5),
            percentile(&cell.cold, 0.5),
            percentile(&cell.warm, 0.5),
            percentile(&cell.warm, 0.95),
            cell.max_error,
            cell.outlier_fraction,
            cell.upload,
            cell.readback,
            cell.submissions,
            cell.peak
        );
        assert!(
            cell.max_error <= 5e-3 && cell.outlier_fraction <= 1e-5,
            "{algorithm:?} {clip}: parity outside tolerance"
        );
    }
    Ok(())
}
