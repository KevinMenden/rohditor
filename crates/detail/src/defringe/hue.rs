//! Hue membership of a pixel: which fringe colour (if any) it looks like.

use std::f32::consts::PI;

/// Fraction of a hue range, at each end, over which membership rises from 0
/// to 1 (raised cosine). The middle of the range is fully inside.
const HUE_ROLLOFF: f32 = 0.25;
/// Colourfulness (`(max - min) / max`) below which a pixel counts as neutral,
/// and above which its hue is trusted completely.
const NEUTRAL_SATURATION: f32 = 0.03;
const TRUSTED_SATURATION: f32 = 0.12;

/// Hue in degrees `[0, 360)` and saturation (`(max - min) / max`) of an
/// RGB triple, or `None` if it is not finite or has no positive channel.
///
/// The hue definition is the one of the HSL mixer (`rgb_to_hsl` in
/// `rohditor-core`), kept here so this crate need not depend on it. Negative
/// values are shifted up first, as the mixer does.
pub(super) fn hue_and_saturation(rgb: [f32; 3]) -> Option<(f32, f32)> {
    if rgb.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let offset = (-rgb[0].min(rgb[1]).min(rgb[2])).max(0.0);
    let [r, g, b] = rgb.map(|v| v + offset);
    let max = r.max(g).max(b);
    if max <= f32::EPSILON {
        return None;
    }
    let chroma = max - r.min(g).min(b);
    if chroma <= f32::EPSILON {
        return Some((0.0, 0.0));
    }
    let sector = if max == r {
        ((g - b) / chroma).rem_euclid(6.0)
    } else if max == g {
        (b - r) / chroma + 2.0
    } else {
        (r - g) / chroma + 4.0
    };
    Some((sector * 60.0, chroma / max))
}

/// Membership of `hue` in `[start, end]` degrees: 1 in the middle, easing to 0
/// at both ends and 0 outside.
pub(super) fn hue_window(hue: f32, [start, end]: [f32; 2]) -> f32 {
    if hue < start || hue > end {
        return 0.0;
    }
    let t = (hue - start) / (end - start);
    let edge = t.min(1.0 - t) / HUE_ROLLOFF;
    if edge >= 1.0 {
        1.0
    } else {
        0.5 * (1.0 - (PI * edge).cos())
    }
}

/// 0 for neutral pixels, 1 for clearly coloured ones, smooth in between, so
/// near-grey pixels (whose hue is noise) are never corrected.
pub(super) fn saturation_ramp(saturation: f32) -> f32 {
    let t = ((saturation - NEUTRAL_SATURATION) / (TRUSTED_SATURATION - NEUTRAL_SATURATION))
        .clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hue_matches_the_primary_and_secondary_colours() {
        let hue = |rgb| hue_and_saturation(rgb).unwrap().0;
        assert_eq!(hue([1.0, 0.0, 0.0]), 0.0);
        assert_eq!(hue([1.0, 1.0, 0.0]), 60.0);
        assert_eq!(hue([0.0, 1.0, 0.0]), 120.0);
        assert_eq!(hue([0.0, 1.0, 1.0]), 180.0);
        assert_eq!(hue([0.0, 0.0, 1.0]), 240.0);
        assert_eq!(hue([1.0, 0.0, 1.0]), 300.0);
        assert_eq!(hue([1.0, 0.0, 0.5]), 330.0);
        // Scale-invariant, so HDR values keep their hue and saturation.
        assert_eq!(hue_and_saturation([4.0, 0.0, 4.0]), Some((300.0, 1.0)));
        assert_eq!(hue_and_saturation([0.5; 3]), Some((0.0, 0.0)));
    }

    #[test]
    fn undefined_colours_have_no_hue() {
        assert_eq!(hue_and_saturation([f32::NAN, 0.1, 0.2]), None);
        assert_eq!(hue_and_saturation([0.0; 3]), None);
        // Negative values are shifted, not clipped.
        assert!(hue_and_saturation([-0.1, 0.2, 0.3]).is_some());
    }

    #[test]
    fn window_is_flat_inside_eases_at_the_ends_and_zero_outside() {
        let range = [270.0, 330.0];
        assert_eq!(hue_window(269.0, range), 0.0);
        assert_eq!(hue_window(331.0, range), 0.0);
        assert_eq!(hue_window(270.0, range), 0.0);
        assert_eq!(hue_window(300.0, range), 1.0);
        assert_eq!(hue_window(285.0, range), 1.0); // start of the flat top
        let eased = hue_window(277.5, range);
        assert!((eased - 0.5).abs() < 1e-6, "{eased}");
        assert!((hue_window(322.5, range) - eased).abs() < 1e-5);
    }

    #[test]
    fn saturation_ramp_ignores_neutrals_and_trusts_colour() {
        assert_eq!(saturation_ramp(0.0), 0.0);
        assert_eq!(saturation_ramp(NEUTRAL_SATURATION), 0.0);
        assert_eq!(saturation_ramp(TRUSTED_SATURATION), 1.0);
        assert_eq!(saturation_ramp(0.9), 1.0);
        let mid = saturation_ramp((NEUTRAL_SATURATION + TRUSTED_SATURATION) / 2.0);
        assert!((mid - 0.5).abs() < 1e-6);
    }
}
