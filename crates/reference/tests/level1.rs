//! Level 1: demosaic quality against ground truth built from a real raw file.
//!
//! See `docs/plans/demosaic-reference-testing.md`.

use std::error::Error;
use std::path::{Path, PathBuf};

use rohditor_demosaic::{DemosaicAlgorithm, WhiteBalanceGains, demosaic};
use rohditor_image::{BayerPattern, LinearRgbImage, MosaicImage};
use rohditor_raw::{RawDecoder, RawlerDecoder};
use rohditor_reference::external::{self, SharedAlgorithm, Tool};
use rohditor_reference::fixtures::Fixture;
use rohditor_reference::{dng, metrics, output_dir, truth};

const PRIVATE_DIR: &str = "../../testdata/private";
const TRUTH_SIZE: (usize, usize) = (1024, 768);
/// Pixels excluded on every side: covers tool border crops and border policies.
const MARGIN: usize = 16;
/// Share of pixels (strongest gradient in the truth) counted as edges.
const EDGE_FRACTION: f64 = 0.10;
/// The identity check allows for 16-bit quantization in RawTherapee's output.
const CFA_IDENTITY_TOLERANCE: f64 = 2e-3;
/// Sites at or above this level are excluded from the identity check.
const CFA_IDENTITY_BELOW: f32 = 0.9;

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("rohditor-reference-{}-{name}", std::process::id()))
}

#[test]
fn dng_round_trips_through_the_raw_decoder() -> Result<(), Box<dyn Error>> {
    // Asymmetric size and a different Bayer phase catch stride/phase bugs.
    let (width, height) = (10, 6);
    let pattern = BayerPattern::Gbrg;
    let samples: Vec<f32> = (0..width * height)
        .map(|i| (i * 7 % 61) as f32 / 60.0)
        .collect();
    let mosaic = MosaicImage::new(width, height, width, pattern, samples.clone())?;
    let path = temp_path("roundtrip.dng");
    dng::write_mosaic_dng(&path, &mosaic, [1.0; 3])?;
    let frame = RawlerDecoder::default().decode(&path);
    std::fs::remove_file(&path)?;
    let frame = frame?;

    assert_eq!((frame.info.width, frame.info.height), (width, height));
    assert_eq!(frame.info.white_levels, vec![f32::from(dng::WHITE_LEVEL)]);
    for (index, expected) in samples.iter().enumerate() {
        let decoded = f32::from(frame.mosaic[index]) / f32::from(dng::WHITE_LEVEL);
        assert!((decoded - expected).abs() < 1e-4, "sample {index}");
    }
    Ok(())
}

struct Row {
    label: String,
    all: metrics::Comparison,
    edges: metrics::Comparison,
    flat: metrics::Comparison,
    cfa_rmse: f64,
}

/// Score `image` (any centred crop of the frame) against the truth window.
fn score(
    label: &str,
    image: &LinearRgbImage<f32>,
    truth_window: &LinearRgbImage<f32>,
    edge_mask: &[bool],
    mosaic: &MosaicImage<f32>,
) -> Result<Row, Box<dyn Error>> {
    let window = truth::interior_window(image, TRUTH_SIZE, MARGIN)?;
    let width = window.width();
    let at = |x: usize, y: usize| edge_mask[y * width + x];
    Ok(Row {
        label: label.to_owned(),
        all: metrics::compare(truth_window, &window, 0),
        edges: metrics::compare_where(truth_window, &window, 0, at),
        flat: metrics::compare_where(truth_window, &window, 0, |x, y| !at(x, y)),
        cfa_rmse: metrics::cfa_site_rmse(mosaic, &window, MARGIN, CFA_IDENTITY_BELOW),
    })
}

/// Build the truth for `source`; `extra_bin` adds a 2x2 box downsample that
/// halves the noise.
fn build_truth(source: &Path, extra_bin: bool) -> Result<LinearRgbImage<f32>, Box<dyn Error>> {
    if extra_bin {
        let large = truth::load_ground_truth(source, TRUTH_SIZE.0 * 2, TRUTH_SIZE.1 * 2)?;
        Ok(truth::downsample2(&large)?)
    } else {
        Ok(truth::load_ground_truth(
            source,
            TRUTH_SIZE.0,
            TRUTH_SIZE.1,
        )?)
    }
}

fn private_raw_files() -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(PRIVATE_DIR);
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("arw"))
        })
        .collect();
    files.sort();
    Ok(files)
}

struct Tools {
    rawtherapee: Option<Tool>,
    darktable: Option<Tool>,
}

impl Tools {
    fn from_env() -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            rawtherapee: Tool::from_env("ROHDITOR_RAWTHERAPEE", "rawtherapee")?,
            darktable: Tool::from_env("ROHDITOR_DARKTABLE", "darktable")?,
        })
    }
}

/// White-balance gains that equalize the channel means of `image`, with green
/// as 1 (the convention RawTherapee and darktable use for their multipliers).
fn balancing_gains(image: &LinearRgbImage<f32>) -> [f64; 3] {
    let mut mean = [0.0_f64; 3];
    for pixel in image.data().chunks_exact(3) {
        for (sum, value) in mean.iter_mut().zip(pixel) {
            *sum += f64::from(*value);
        }
    }
    mean.map(|sum| mean[1] / sum)
}

/// Multiply each CFA sample (or RGB channel) by its channel gain.
fn scale_mosaic(
    mosaic: &MosaicImage<f32>,
    gains: [f64; 3],
) -> Result<MosaicImage<f32>, Box<dyn Error>> {
    let width = mosaic.width();
    let data = mosaic
        .data()
        .iter()
        .enumerate()
        .map(|(i, value)| {
            let channel = mosaic
                .pattern()
                .color_at(i % width, i / width)
                .channel_index();
            value * gains[channel] as f32
        })
        .collect();
    Ok(MosaicImage::new(
        width,
        mosaic.height(),
        width,
        mosaic.pattern(),
        data,
    )?)
}

fn scale_rgb(
    image: &LinearRgbImage<f32>,
    gains: [f64; 3],
) -> Result<LinearRgbImage<f32>, Box<dyn Error>> {
    let data = image
        .data()
        .iter()
        .enumerate()
        .map(|(i, value)| value * gains[i % 3] as f32)
        .collect();
    Ok(LinearRgbImage::new(
        image.width(),
        image.height(),
        image.row_stride(),
        image.space(),
        data,
    )?)
}

/// Score every algorithm and available tool on one ground-truth image and
/// print the table. `gains` is the as-shot white balance: the DNG carries it,
/// tools apply it before demosaicing, and everything is scored against the
/// balanced truth. Rohditor runs as the pipeline does (`demosaic()` with the
/// as-shot balance, then the white-balance gains applied to its camera-native
/// output) and, as a baseline, without any balance. `gains` must have green = 1.
/// Asserts only that each tool reproduced the measured samples.
fn run_table(
    title: &str,
    ground_truth: &LinearRgbImage<f32>,
    gains: [f64; 3],
    tools: &Tools,
) -> Result<(), Box<dyn Error>> {
    let work_dir = output_dir();
    std::fs::create_dir_all(&work_dir)?;
    let dng_path = work_dir.join("level1.dng");
    // Darken the truth so that balanced values stay <= 1: the tools output
    // 16-bit sRGB or float and must not clip.
    let headroom = 1.0 / gains.iter().copied().fold(0.0, f64::max);
    let ground_truth = &scale_rgb(ground_truth, [headroom; 3])?;
    let mosaic = truth::remosaic(ground_truth, BayerPattern::Rggb)?;
    // What the tools should output: the camera data after white balance.
    let balanced_truth = scale_rgb(ground_truth, gains)?;
    let balanced_mosaic = scale_mosaic(&mosaic, gains)?;
    let truth_window = truth::interior_window(&balanced_truth, TRUTH_SIZE, MARGIN)?;
    let edge_mask = metrics::edge_mask(&truth_window, EDGE_FRACTION);
    dng::write_mosaic_dng(&dng_path, &mosaic, gains)?;
    let balance = WhiteBalanceGains {
        red: gains[0] as f32,
        green: gains[1] as f32,
        blue: gains[2] as f32,
    };

    let mut rows = Vec::new();
    let mut add = |label: String, image: &LinearRgbImage<f32>| -> Result<(), Box<dyn Error>> {
        rows.push(score(
            &label,
            image,
            &truth_window,
            &edge_mask,
            &balanced_mosaic,
        )?);
        Ok(())
    };
    for (name, algorithm, shared) in [
        (
            "amaze",
            DemosaicAlgorithm::Amaze,
            Some(SharedAlgorithm::Amaze),
        ),
        ("rcd", DemosaicAlgorithm::Rcd, Some(SharedAlgorithm::Rcd)),
        ("mhc", DemosaicAlgorithm::MalvarHeCutler, None),
        ("bilinear", DemosaicAlgorithm::Bilinear, None),
    ] {
        add(
            format!("rohditor {name}"),
            &scale_rgb(&demosaic(&mosaic, balance, algorithm)?, gains)?,
        )?;
        if shared.is_some() {
            add(
                format!("rohditor {name} no balance"),
                &scale_rgb(
                    &demosaic(&mosaic, WhiteBalanceGains::identity(), algorithm)?,
                    gains,
                )?,
            )?;
        }
        let Some(shared) = shared else { continue };
        if let Some(tool) = &tools.rawtherapee {
            add(
                format!("rawtherapee {name}"),
                &external::run_rawtherapee(tool, &dng_path, shared, &work_dir)?,
            )?;
        }
        if let Some(tool) = &tools.darktable {
            add(
                format!("darktable {name}"),
                &external::run_darktable(tool, &dng_path, shared, &work_dir)?,
            )?;
        }
    }

    println!(
        "\n{title} (gains {:.2} {:.2} {:.2})",
        gains[0], gains[1], gains[2]
    );
    println!(
        "{:<26} {:>8} {:>8} {:>8} {:>10}",
        "", "all dB", "edge dB", "flat dB", "CFA RMSE"
    );
    for row in &rows {
        println!(
            "{:<26} {:>8.2} {:>8.2} {:>8.2} {:>10.6}",
            row.label, row.all.psnr_db, row.edges.psnr_db, row.flat.psnr_db, row.cfa_rmse
        );
        // A harness that misreads the input would fail here, not in the table.
        assert!(
            row.cfa_rmse < CFA_IDENTITY_TOLERANCE,
            "{} does not reproduce the measured samples (CFA RMSE {})",
            row.label,
            row.cfa_rmse
        );
    }
    Ok(())
}

#[test]
#[ignore = "requires testdata/private; RawTherapee/darktable rows need ROHDITOR_RAWTHERAPEE / ROHDITOR_DARKTABLE"]
fn level1_real_images() -> Result<(), Box<dyn Error>> {
    let tools = Tools::from_env()?;
    for source in private_raw_files()? {
        for extra_bin in [false, true] {
            let name = source.file_name().and_then(|n| n.to_str()).unwrap_or("?");
            let noise = if extra_bin {
                "4x4 binned, low noise"
            } else {
                "2x2 binned"
            };
            let ground_truth = build_truth(&source, extra_bin)?;
            let gains = balancing_gains(&ground_truth);
            run_table(&format!("{name} ({noise})"), &ground_truth, gains, &tools)?;
        }
    }
    Ok(())
}

/// Noise-free fixtures: differences between tools here are reconstruction
/// quality, not noise handling.
#[test]
#[ignore = "needs ROHDITOR_RAWTHERAPEE / ROHDITOR_DARKTABLE for the external rows"]
fn level1_clean_fixtures() -> Result<(), Box<dyn Error>> {
    let tools = Tools::from_env()?;
    for fixture in Fixture::ALL {
        let image = fixture.render(TRUTH_SIZE.0, TRUTH_SIZE.1)?;
        run_table(
            &format!("{} (noise-free)", fixture.name()),
            &image,
            [1.0; 3],
            &tools,
        )?;
    }
    Ok(())
}
