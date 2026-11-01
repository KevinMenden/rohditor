//! Everything an independent executor (the GPU) needs to run the
//! source-resolution detail stage exactly as [`super::detail::apply`] does.
//!
//! The CPU stays authoritative: the contract carries resolved settings,
//! balance and ceilings, plus the sizes an executor has to plan around. Noise
//! profiles are estimated from the stage input by [`rohditor_detail::estimate_noise_profiles`] and
//! handed over separately, since they depend on pixels.
use rohditor_demosaic::WhiteBalanceGains;
use rohditor_detail::{CHROMA_HALO, LUMA_HALO};
use rohditor_edit::{EditRecipe, FalseColorSuppression, NoiseReduction};

use crate::{CaptureSharpeningContract, PipelineError};

/// Resolved detail stage: false colour, noise reduction, capture sharpening.
#[derive(Debug, Clone)]
pub struct DetailContract {
    capture: CaptureSharpeningContract,
    false_color: FalseColorSuppression,
    noise_reduction: NoiseReduction,
    balance: [f32; 3],
}

impl DetailContract {
    /// `balance` is the as-shot demosaic balance and `gains` the user's white
    /// balance (which only shapes the highlight ceilings), as for
    /// [`super::detail::apply`].
    pub fn new(
        recipe: &EditRecipe,
        balance: WhiteBalanceGains,
        gains: WhiteBalanceGains,
    ) -> Result<Self, PipelineError> {
        recipe.false_color.validate()?;
        recipe.noise_reduction.validate()?;
        Ok(Self {
            capture: CaptureSharpeningContract::new(
                recipe.capture_sharpening,
                super::detail::ceilings(recipe, gains),
            )?,
            false_color: recipe.false_color,
            noise_reduction: recipe.noise_reduction,
            balance: [balance.red, balance.green, balance.blue],
        })
    }

    /// A contract with only capture sharpening, for executors and qualification
    /// code that exercise that step alone.
    #[must_use]
    pub fn capture_only(capture: CaptureSharpeningContract) -> Self {
        Self {
            capture,
            false_color: FalseColorSuppression::default(),
            noise_reduction: NoiseReduction::default(),
            balance: [1.0; 3],
        }
    }

    /// Capture sharpening, the last step.
    #[must_use]
    pub const fn capture(&self) -> &CaptureSharpeningContract {
        &self.capture
    }

    #[must_use]
    pub const fn false_color(&self) -> FalseColorSuppression {
        self.false_color
    }

    #[must_use]
    pub const fn noise_reduction(&self) -> NoiseReduction {
        self.noise_reduction
    }

    /// As-shot balance that defines the luma and colour differences.
    #[must_use]
    pub const fn balance(&self) -> [f32; 3] {
        self.balance
    }

    /// Per-channel values at or above which a pixel counts as blown.
    #[must_use]
    pub fn ceilings(&self) -> [f32; 3] {
        self.capture.ceilings()
    }

    /// Whether any step has work to do.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.false_color.is_active()
            || self.noise_reduction.is_active()
            || self.capture.settings().is_active()
    }

    /// Source pixels a tile needs beyond the pixels it produces (per side) so
    /// that every step sees what it would see on the whole image. Steps run
    /// one after another, so their reaches add.
    #[must_use]
    pub fn halo(&self) -> usize {
        let mut halo = 0;
        if self.false_color.is_active() {
            // One 3x3 median per step.
            halo += usize::from(self.false_color.steps);
        }
        if self.noise_reduction.chroma_active() {
            halo += CHROMA_HALO;
        }
        if self.noise_reduction.luminance_active() {
            halo += LUMA_HALO;
        }
        if self.capture.settings().is_active() {
            halo += self.capture.halo();
        }
        halo
    }

    /// Run the whole stage on the CPU: the reference every executor is
    /// qualified against.
    pub fn apply_cpu(
        &self,
        image: &mut rohditor_image::LinearRgbImage<f32>,
        cancellation: &crate::CancellationToken,
    ) -> Result<(), PipelineError> {
        super::detail::run_steps(
            image,
            self,
            cancellation,
            &mut super::detail::DetailStage::skipped(),
        )
    }

    /// Scalar planes of tile size an executor needs as scratch.
    #[must_use]
    pub fn scratch_planes(&self) -> usize {
        // Capture sharpening: guide, mask, estimate, temporary, blurred,
        // ratio. Chroma NR additionally needs luma, two current, two
        // smoothed and two accumulated colour planes.
        if self.noise_reduction.chroma_active() {
            7
        } else {
            6
        }
    }
}
