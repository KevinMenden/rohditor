//! Auto tone: a gentle refinement of the current base rendering.
//!
//! The Matched rendering already reproduces the camera's interpretation of the
//! scene, so Auto does not force every photo to one brightness. It moves the
//! median part of the way towards a typical value, which keeps dark scenes
//! darker and bright scenes brighter than average, and it never spends exposure
//! that the highlights have no room for. A dark high-contrast scene gets the
//! rest of its correction from Shadows instead.
//!
//! The histogram is mapped back to scene light through the recipe's own tone
//! stages. Every decision is therefore made as if from zero exposure and
//! neutral Shadows, Highlights, and Blacks, so the result does not depend on
//! those sliders' current values, and pressing Auto again changes nothing.

mod tone_stages;

use rohditor_edit::{EXPOSURE_EV_RANGE, EditRecipe, LightAdjustments};

use crate::{Histogram, base_rendering_lut};
use tone_stages::ToneStages;

/// Display code the median is drawn towards, near the middle of the camera
/// JPEG medians in the private corpus. Below 18% gray (code 118) on purpose.
const TARGET_MEDIAN_CODE: f32 = 105.0;
/// Fraction of the distance to the target, in stops, that exposure corrects.
const MEDIAN_CORRECTION_STRENGTH: f32 = 0.5;
const MAX_EXPOSURE_CHANGE_EV: f32 = 1.0;

/// Exposure may raise the 99.5th percentile of the brightest colour channel up
/// to this code and no further. Luminance alone would miss saturated colours.
const HIGHLIGHT_CEILING_CODE: f32 = 245.0;
const BRIGHT_FRACTION: f32 = 0.995;

const MAX_SHADOWS: f32 = 0.3;
const SHADOWS_SEARCH_ITERATIONS: usize = 12;

/// Highlights starts pulling down when the 99th percentile passes the first
/// code and reaches [`MAX_HIGHLIGHTS_PULL`] at the second.
const HIGHLIGHTS_PULL_CODES: (f32, f32) = (225.0, 250.0);
const MAX_HIGHLIGHTS_PULL: f32 = 0.4;

/// Blacks starts deepening when the 1st percentile passes the first code and
/// reaches [`MAX_BLACKS_PULL`] at the second.
const BLACKS_PULL_CODES: (f32, f32) = (24.0, 48.0);
const MAX_BLACKS_PULL: f32 = 0.4;

/// Differences below these come from histogram quantization and the luminance
/// approximation of the model; keeping the current value stops a repeated Auto
/// from moving the sliders.
const EXPOSURE_EPSILON_EV: f32 = 0.03;
const SLIDER_EPSILON: f32 = 0.05;

/// Set Exposure, Shadows, Highlights, and Blacks from the display histogram of
/// the current render. Other controls are kept and taken into account. Returns
/// `None` when the histogram has no usable midtone (e.g. an all-black image).
#[must_use]
pub fn auto_tone(recipe: &EditRecipe, histogram: &Histogram) -> Option<EditRecipe> {
    let median = histogram.luminance_percentile(0.5);
    if median == 0 {
        return None;
    }
    let base_lut = base_rendering_lut(&recipe.rendering);
    let current = ToneStages::new(base_lut.as_deref(), recipe);
    let neutral_light = LightAdjustments {
        shadows: 0.0,
        highlights: 0.0,
        blacks: 0.0,
        ..recipe.light.clone()
    };
    let neutral = current.with_light(&neutral_light);

    // Scene luminance of a display code as it would be at zero exposure.
    let current_gain = recipe.light.exposure_ev.exp2();
    let scene_of = |code: u8| current.scene_luminance(f32::from(code).max(0.5)) / current_gain;
    let scene_median = scene_of(median);

    let wanted_ev = (MEDIAN_CORRECTION_STRENGTH
        * stops_between(scene_median, neutral.scene_luminance(TARGET_MEDIAN_CODE)))
    .clamp(-MAX_EXPOSURE_CHANGE_EV, MAX_EXPOSURE_CHANGE_EV);
    let wanted_median_code = neutral.display_code(scene_median * wanted_ev.exp2());

    let exposure_ev = if wanted_ev > 0.0 {
        wanted_ev.min(highlight_room_ev(
            histogram,
            recipe.light.exposure_ev,
            &scene_of,
            &neutral,
        ))
    } else {
        wanted_ev
    };
    let gain = exposure_ev.exp2();

    let shadows = shadows_for_median(
        &neutral,
        &neutral_light,
        scene_median * gain,
        wanted_median_code,
    );
    let toned_light = LightAdjustments {
        shadows,
        ..neutral_light.clone()
    };
    let toned = neutral.with_light(&toned_light);
    let code_after = |fraction: f32| {
        toned.display_code(scene_of(histogram.luminance_percentile(fraction)) * gain)
    };
    let highlights = -MAX_HIGHLIGHTS_PULL * ramp(HIGHLIGHTS_PULL_CODES, code_after(0.99));
    let blacks = -MAX_BLACKS_PULL * ramp(BLACKS_PULL_CODES, code_after(0.01));

    let mut next = recipe.clone();
    next.light.exposure_ev = settled(
        recipe.light.exposure_ev,
        exposure_ev.clamp(EXPOSURE_EV_RANGE.minimum, EXPOSURE_EV_RANGE.maximum),
        EXPOSURE_EPSILON_EV,
    );
    next.light.shadows = settled(recipe.light.shadows, shadows, SLIDER_EPSILON);
    next.light.highlights = settled(recipe.light.highlights, highlights, SLIDER_EPSILON);
    next.light.blacks = settled(recipe.light.blacks, blacks, SLIDER_EPSILON);
    Some(next)
}

fn stops_between(from: f32, to: f32) -> f32 {
    (to / from).log2()
}

/// Exposure at which the brightest colour channel reaches the ceiling. Channels
/// are not luminance, but the tone stages scale them about equally, which is
/// enough for a limit.
///
/// A channel already at the ceiling may be clipped and says little about how
/// far it is over, so the exposure in use is the most that can be justified.
/// Answering zero instead would make a second Auto undo the first whenever a
/// saturated colour outgrew the luminance model on the way up.
fn highlight_room_ev(
    histogram: &Histogram,
    current_exposure_ev: f32,
    scene_of: &impl Fn(u8) -> f32,
    neutral: &ToneStages<'_>,
) -> f32 {
    let bright = histogram.brightest_channel_percentile(BRIGHT_FRACTION);
    if f32::from(bright) >= HIGHLIGHT_CEILING_CODE {
        return current_exposure_ev.max(0.0);
    }
    stops_between(
        scene_of(bright),
        neutral.scene_luminance(HIGHLIGHT_CEILING_CODE),
    )
    .max(0.0)
}

/// Smallest Shadows value bringing the median to the wanted code, up to
/// [`MAX_SHADOWS`]. Zero when exposure alone already got there.
fn shadows_for_median(
    neutral: &ToneStages<'_>,
    neutral_light: &LightAdjustments,
    scene_median: f32,
    wanted_code: f32,
) -> f32 {
    if neutral.display_code(scene_median) >= wanted_code {
        return 0.0;
    }
    let reaches = |shadows: f32| {
        let stages = neutral.with_light(&LightAdjustments {
            shadows,
            ..neutral_light.clone()
        });
        stages.display_code(scene_median) >= wanted_code
    };
    let (mut low, mut high) = (0.0, MAX_SHADOWS);
    if !reaches(high) {
        return MAX_SHADOWS;
    }
    for _ in 0..SHADOWS_SEARCH_ITERATIONS {
        let middle = 0.5 * (low + high);
        if reaches(middle) {
            high = middle;
        } else {
            low = middle;
        }
    }
    high
}

/// Position of a code on a pull range, from 0 at its start to 1 at its end.
///
/// Auto only ever pulls these two sliders down. Raising Highlights just
/// brightens the image, and raising Blacks adds a flat lift that turns true
/// black into a visible gray: +0.1 already lands near display code 43.
fn ramp((start, full): (f32, f32), code: f32) -> f32 {
    ((code - start) / (full - start)).clamp(0.0, 1.0)
}

fn settled(current: f32, computed: f32, epsilon: f32) -> f32 {
    if (computed - current).abs() < epsilon {
        current
    } else {
        computed
    }
}

#[cfg(test)]
mod tests;
