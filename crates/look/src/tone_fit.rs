use rohditor_edit::CameraTone;
use thiserror::Error;

use crate::{LUMA_BINS, LuminanceHistogram};

/// Fewer samples than this make the distributions too noisy to match.
const MIN_SAMPLES: u64 = 4_096;
/// The 1st-to-99th percentile range must span at least this many display
/// levels; flat or near-black frames carry no tone information.
const MIN_SPREAD_LEVELS: usize = 24;
/// Largest accepted gap between the camera distribution and the neutral render
/// pushed through the fitted curve (Kolmogorov-Smirnov distance).
const MAX_DISTRIBUTION_ERROR: f32 = 0.05;
/// Largest accepted slope of the fitted curve between knots. Camera curves are
/// well below this; a steeper one means the JPEG is not a plain tone map.
const MAX_SLOPE: f32 = 8.0;

/// Granularity, in display levels, at which the fit error is measured.
const ERROR_BIN_LEVELS: usize = 8;

/// Display-encoded luminance positions where the fitted curve is sampled,
/// denser in the shadows where the camera toe and the sRGB slope differ most.
const KNOT_INPUTS: [f32; 19] = [
    0.0, 0.02, 0.04, 0.07, 0.10, 0.14, 0.18, 0.23, 0.28, 0.34, 0.40, 0.47, 0.54, 0.62, 0.70, 0.78,
    0.86, 0.93, 1.0,
];

/// A fitted camera tone and how well it explains the camera JPEG.
#[derive(Debug, Clone, PartialEq)]
pub struct ToneFit {
    pub tone: CameraTone,
    /// Kolmogorov-Smirnov distance between the camera luminance distribution
    /// and the neutral render mapped through `tone`, in `0..=1`.
    pub distribution_error: f32,
}

/// Why a fit was refused. The caller keeps rendering with Standard.
#[derive(Debug, Clone, Copy, PartialEq, Error)]
pub enum ToneFitError {
    #[error("the {image} image has too few pixels ({samples}) for tone matching")]
    TooFewSamples { image: &'static str, samples: u64 },
    #[error("the {image} image has too little tonal range to match")]
    LowContrast { image: &'static str },
    #[error("the fitted curve is too steep ({slope:.1}) to be a plain tone map")]
    CurveTooSteep { slope: f32 },
    #[error("the fitted curve explains the camera image poorly (distribution error {error:.3})")]
    PoorFit { error: f32 },
}

/// Fit the global tone curve that turns the neutral render into the camera's
/// rendering by matching their luminance distributions.
///
/// Both histograms must come from images of the same framing. Only the
/// distributions are compared, so no pixel alignment is needed; the price is
/// that local tone mapping in the camera JPEG is folded into one global curve.
/// The result is accepted only if it passes the sample, contrast, slope, and
/// distribution-error gates.
pub fn fit_camera_tone(
    neutral: &LuminanceHistogram,
    camera: &LuminanceHistogram,
) -> Result<ToneFit, ToneFitError> {
    for (name, histogram) in [("neutral render", neutral), ("camera preview", camera)] {
        let samples = histogram.iter().sum::<u64>();
        if samples < MIN_SAMPLES {
            return Err(ToneFitError::TooFewSamples {
                image: name,
                samples,
            });
        }
        if percentile_level(histogram, 0.99) - percentile_level(histogram, 0.01) < MIN_SPREAD_LEVELS
        {
            return Err(ToneFitError::LowContrast { image: name });
        }
    }

    let camera_quantiles = QuantileFunction::new(camera);
    let neutral_midpoints = bin_midpoints(neutral);
    // Matching points (neutral level -> camera level). The clipped end bins say
    // nothing about the curve's shape, so the curve is pinned there instead.
    let mut points = vec![[0.0, 0.0]];
    for level in 1..LUMA_BINS - 1 {
        if neutral[level] > 0 {
            points.push([
                level as f32 / 255.0,
                camera_quantiles.level_at(neutral_midpoints[level]) / 255.0,
            ]);
        }
    }
    points.push([1.0, 1.0]);

    let mut previous = 0.0_f32;
    let knots = KNOT_INPUTS
        .iter()
        .map(|&input| {
            // Running maximum keeps the sampled curve monotone even when sparse
            // bins make neighbouring matches wobble.
            previous = interpolate(&points, input).clamp(previous, 1.0);
            [input, previous]
        })
        .collect::<Vec<_>>();
    let tone = CameraTone { knots };

    let slope = tone
        .knots
        .windows(2)
        .map(|pair| (pair[1][1] - pair[0][1]) / (pair[1][0] - pair[0][0]))
        .fold(0.0_f32, f32::max);
    if slope > MAX_SLOPE {
        return Err(ToneFitError::CurveTooSteep { slope });
    }

    let distribution_error = distribution_error(neutral, camera, &tone);
    if distribution_error > MAX_DISTRIBUTION_ERROR {
        return Err(ToneFitError::PoorFit {
            error: distribution_error,
        });
    }
    Ok(ToneFit {
        tone,
        distribution_error,
    })
}

/// Lowest level whose cumulative share reaches `fraction`.
fn percentile_level(histogram: &LuminanceHistogram, fraction: f32) -> usize {
    let total = histogram.iter().sum::<u64>();
    let target = (fraction * total as f32).ceil() as u64;
    let mut cumulative = 0;
    for (level, count) in histogram.iter().enumerate() {
        cumulative += count;
        if cumulative >= target {
            return level;
        }
    }
    LUMA_BINS - 1
}

/// Cumulative share at the middle of each bin. Using the midpoint rather than
/// the upper edge avoids biasing every match toward brighter levels.
fn bin_midpoints(histogram: &LuminanceHistogram) -> [f32; LUMA_BINS] {
    let total = histogram.iter().sum::<u64>() as f64;
    let mut midpoints = [0.0; LUMA_BINS];
    let mut before = 0_u64;
    for (level, &count) in histogram.iter().enumerate() {
        midpoints[level] = ((before as f64 + count as f64 / 2.0) / total) as f32;
        before += count;
    }
    midpoints
}

/// Piecewise-linear inverse of a histogram's cumulative distribution.
struct QuantileFunction {
    /// `[cumulative share, level]`, increasing in both.
    points: Vec<[f32; 2]>,
}

impl QuantileFunction {
    fn new(histogram: &LuminanceHistogram) -> Self {
        let midpoints = bin_midpoints(histogram);
        let mut occupied = (0..LUMA_BINS).filter(|&level| histogram[level] > 0);
        let first = occupied.next().unwrap_or(0);
        let last = occupied.next_back().unwrap_or(first);
        let mut points = vec![[0.0, first as f32]];
        for level in first..=last {
            if histogram[level] > 0 {
                points.push([midpoints[level], level as f32]);
            }
        }
        points.push([1.0, last as f32]);
        Self { points }
    }

    fn level_at(&self, share: f32) -> f32 {
        interpolate(&self.points, share)
    }
}

/// Linear interpolation through `[x, y]` points sorted by `x`.
fn interpolate(points: &[[f32; 2]], x: f32) -> f32 {
    let upper = points.partition_point(|point| point[0] <= x);
    if upper == 0 {
        return points[0][1];
    }
    if upper == points.len() {
        return points[upper - 1][1];
    }
    let [x0, y0] = points[upper - 1];
    let [x1, y1] = points[upper];
    if x1 <= x0 {
        return y1;
    }
    y0 + (y1 - y0) * (x - x0) / (x1 - x0)
}

/// Kolmogorov-Smirnov distance between the camera distribution and the neutral
/// distribution after mapping every neutral level through the curve.
///
/// The CDFs are compared only at the edges of [`ERROR_BIN_LEVELS`]-level groups.
/// Skies, clipped areas and flat walls put several percent of all pixels into a
/// single 8-bit level, so a per-level comparison would report that unavoidable
/// quantisation step as fit error.
fn distribution_error(
    neutral: &LuminanceHistogram,
    camera: &LuminanceHistogram,
    tone: &CameraTone,
) -> f32 {
    let mut mapped = [0_u64; LUMA_BINS];
    for (level, &count) in neutral.iter().enumerate() {
        let output = tone.evaluate(level as f32 / 255.0) * 255.0;
        mapped[output.round().clamp(0.0, 255.0) as usize] += count;
    }
    let mapped_total = mapped.iter().sum::<u64>() as f64;
    let camera_total = camera.iter().sum::<u64>() as f64;
    let (mut mapped_sum, mut camera_sum) = (0_u64, 0_u64);
    let mut worst = 0.0_f64;
    for level in 0..LUMA_BINS {
        mapped_sum += mapped[level];
        camera_sum += camera[level];
        if (level + 1) % ERROR_BIN_LEVELS != 0 {
            continue;
        }
        worst =
            worst.max((mapped_sum as f64 / mapped_total - camera_sum as f64 / camera_total).abs());
    }
    worst as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Triangular, deliberately asymmetric luminance distribution peaking at 70.
    fn neutral_histogram() -> LuminanceHistogram {
        let mut histogram = [0_u64; LUMA_BINS];
        for (level, count) in histogram.iter_mut().enumerate() {
            let level = level as i64;
            *count = if level <= 70 {
                level * 40 + 400
            } else {
                ((255 - level) * 15 + 400).max(0)
            } as u64;
        }
        histogram
    }

    /// Push every neutral level through a known curve to build the camera side.
    fn camera_histogram(
        neutral: &LuminanceHistogram,
        curve: impl Fn(f32) -> f32,
    ) -> LuminanceHistogram {
        let mut camera = [0_u64; LUMA_BINS];
        for (level, &count) in neutral.iter().enumerate() {
            let output = (curve(level as f32 / 255.0) * 255.0).round() as usize;
            camera[output.min(255)] += count;
        }
        camera
    }

    #[test]
    fn recovers_a_known_curve_from_an_asymmetric_distribution() {
        // Brightening toe plus a soft shoulder, similar to a camera standard style.
        let curve = |x: f32| x.powf(0.7) * (1.0 - 0.15 * x * x);
        let neutral = neutral_histogram();
        let camera = camera_histogram(&neutral, curve);
        let fit = fit_camera_tone(&neutral, &camera).expect("clean synthetic fit");
        fit.tone
            .validate()
            .expect("fitted curve satisfies the recipe contract");
        // Judge the interior where the neutral image has data; the curve is
        // pinned (0,0)/(1,1), which the synthetic curve only approximately hits.
        for input in [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7] {
            let expected = curve(input);
            let actual = fit.tone.evaluate(input);
            assert!(
                (actual - expected).abs() < 0.03,
                "at {input}: expected {expected}, got {actual}"
            );
        }
        assert!(fit.distribution_error < MAX_DISTRIBUTION_ERROR);
    }

    #[test]
    fn identical_distributions_fit_the_identity() {
        let neutral = neutral_histogram();
        let fit = fit_camera_tone(&neutral, &neutral).expect("identity fit");
        for input in [0.1, 0.3, 0.5, 0.8] {
            assert!((fit.tone.evaluate(input) - input).abs() < 0.02);
        }
    }

    #[test]
    fn rejects_flat_and_near_black_frames() {
        let neutral = neutral_histogram();
        let mut flat = [0_u64; LUMA_BINS];
        flat[128] = 100_000;
        let mut near_black = [0_u64; LUMA_BINS];
        near_black[..6].fill(20_000);
        for degenerate in [flat, near_black] {
            assert!(matches!(
                fit_camera_tone(&neutral, &degenerate),
                Err(ToneFitError::LowContrast {
                    image: "camera preview"
                })
            ));
            assert!(matches!(
                fit_camera_tone(&degenerate, &neutral),
                Err(ToneFitError::LowContrast {
                    image: "neutral render"
                })
            ));
        }
    }

    #[test]
    fn rejects_tiny_images() {
        let neutral = neutral_histogram();
        let mut tiny = [0_u64; LUMA_BINS];
        tiny[10] = 10;
        tiny[200] = 10;
        assert!(matches!(
            fit_camera_tone(&neutral, &tiny),
            Err(ToneFitError::TooFewSamples { .. })
        ));
    }

    #[test]
    fn rejects_a_curve_the_knots_cannot_represent() {
        // A narrow neutral band stretched over the whole camera range needs a
        // near-vertical step that the knots cannot follow.
        let mut neutral = [0_u64; LUMA_BINS];
        neutral[100..=130].fill(2_000);
        let camera = camera_histogram(&neutral, |x| {
            ((x - 100.0 / 255.0) * 255.0 / 30.0).clamp(0.0, 1.0)
        });
        assert!(matches!(
            fit_camera_tone(&neutral, &camera),
            Err(ToneFitError::PoorFit { .. } | ToneFitError::CurveTooSteep { .. })
        ));
    }
}
