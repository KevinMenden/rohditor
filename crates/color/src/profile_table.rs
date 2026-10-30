//! Evaluation of DNG hue/saturation/value delta tables (`ProfileHueSatMap`,
//! `ProfileLookTable`).
//!
//! The algorithm follows the DNG specification: RGB is converted to HSV, the
//! table is interpolated (bilinear in hue and saturation, trilinear when it has
//! several value divisions, hue wrapping around), and the resulting hue shift,
//! saturation scale and value scale are applied. The RGB space is the caller's
//! business; DNG tables are authored in linear ProPhoto RGB.

use crate::{
    ColorMathError, D50_WHITE, LINEAR_REC2020_TO_XYZ_D65, Matrix3, chromatic_adaptation_to_d65,
    linear_srgb_to_srgb, srgb_to_linear_srgb,
};

/// XYZ (D50) to linear ProPhoto RGB, the working space of DNG profile tables.
const XYZ_D50_TO_LINEAR_PROPHOTO: Matrix3 = Matrix3::new([
    [1.345_943_3, -0.255_607_5, -0.051_111_8],
    [-0.544_598_9, 1.508_167_3, 0.020_535_1],
    [0.0, 0.0, 1.211_812_8],
]);

/// Matrices between Rohditor's linear Rec.2020 (D65) working space and the
/// linear ProPhoto RGB (D50) space DNG tables are authored in, as
/// `(to_prophoto, from_prophoto)`. Adaptation uses Bradford.
pub fn rec2020_prophoto_matrices() -> Result<(Matrix3, Matrix3), ColorMathError> {
    let d65_to_d50 = chromatic_adaptation_to_d65(D50_WHITE)?.inverse()?;
    let forward = LINEAR_REC2020_TO_XYZ_D65
        .then(d65_to_d50)
        .then(XYZ_D50_TO_LINEAR_PROPHOTO);
    Ok((forward, forward.inverse()?))
}

/// Borrowed view of a delta table. `deltas` holds
/// `[hue shift in degrees, saturation scale, value scale]` entries,
/// saturation-fastest, then hue, then value.
#[derive(Debug, Clone, Copy)]
pub struct DeltaTable<'a> {
    pub hue_divisions: usize,
    pub sat_divisions: usize,
    pub val_divisions: usize,
    /// Encode RGB with the sRGB curve around the HSV lookup.
    pub srgb_encoded: bool,
    pub deltas: &'a [[f32; 3]],
}

/// Apply a delta table to one linear RGB triple.
///
/// Non-finite pixels and pixels with a negative component are returned
/// unchanged: the table is only defined on non-negative RGB. Values above 1
/// keep their brightness (the value scale applies, but is not clamped) so
/// over-range scene-linear data survives.
#[must_use]
pub fn apply_delta_table(table: &DeltaTable<'_>, rgb: [f32; 3]) -> [f32; 3] {
    if rgb.iter().any(|value| !value.is_finite() || *value < 0.0) {
        return rgb;
    }
    let encoded = if table.srgb_encoded {
        rgb.map(linear_srgb_to_srgb)
    } else {
        rgb
    };
    let [h, s, v] = rgb_to_hsv(encoded);
    let [hue_shift, sat_scale, val_scale] = interpolate(table, h, s, v.min(1.0));
    let h = wrap_hue(h + hue_shift * (6.0 / 360.0));
    let s = (s * sat_scale).min(1.0);
    let mapped = hsv_to_rgb([h, s, v * val_scale]);
    if table.srgb_encoded {
        mapped.map(srgb_to_linear_srgb)
    } else {
        mapped
    }
}

/// Interpolated `[hue shift, saturation scale, value scale]` at an HSV point
/// (hue in `0..6`, saturation and value in `0..=1`).
fn interpolate(table: &DeltaTable<'_>, h: f32, s: f32, v: f32) -> [f32; 3] {
    let hue_scale = if table.hue_divisions < 2 {
        0.0
    } else {
        table.hue_divisions as f32 / 6.0
    };
    let hue_scaled = h * hue_scale;
    let sat_scaled = s * (table.sat_divisions - 1) as f32;
    let max_hue = table.hue_divisions - 1;
    let max_sat = table.sat_divisions - 2;

    let mut hue0 = hue_scaled as usize;
    let sat0 = (sat_scaled as usize).min(max_sat);
    let mut hue1 = hue0 + 1;
    if hue0 >= max_hue {
        // The hue axis wraps: past the last division, blend back to the first.
        hue0 = max_hue;
        hue1 = 0;
    }
    let hue_fraction = hue_scaled - hue0 as f32;
    let sat_fraction = sat_scaled - sat0 as f32;

    let (val0, val1, val_fraction) = if table.val_divisions < 2 {
        (0, 0, 0.0)
    } else {
        let scaled = v * (table.val_divisions - 1) as f32;
        let max_val = table.val_divisions - 1;
        let val0 = (scaled as usize).min(max_val);
        let val1 = if val0 < max_val { val0 + 1 } else { val0 };
        (val0, val1, scaled - val0 as f32)
    };

    let hue_step = table.sat_divisions;
    let val_step = table.sat_divisions * table.hue_divisions;
    let entry =
        |val: usize, hue: usize, sat: usize| table.deltas[val * val_step + hue * hue_step + sat];
    let plane = |val: usize| -> [f32; 3] {
        let blend = |first: [f32; 3], second: [f32; 3], fraction: f32| {
            [0, 1, 2].map(|channel| first[channel] * (1.0 - fraction) + second[channel] * fraction)
        };
        let low = blend(
            entry(val, hue0, sat0),
            entry(val, hue0, sat0 + 1),
            sat_fraction,
        );
        let high = blend(
            entry(val, hue1, sat0),
            entry(val, hue1, sat0 + 1),
            sat_fraction,
        );
        blend(low, high, hue_fraction)
    };
    let (lower, upper) = (plane(val0), plane(val1));
    [0, 1, 2].map(|channel| lower[channel] * (1.0 - val_fraction) + upper[channel] * val_fraction)
}

/// RGB to HSV with hue in `0..6`.
fn rgb_to_hsv([r, g, b]: [f32; 3]) -> [f32; 3] {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max <= 0.0 {
        return [0.0, 0.0, 0.0];
    }
    let delta = max - min;
    let s = delta / max;
    if delta <= 0.0 {
        return [0.0, s, max];
    }
    let h = if max == r {
        (g - b) / delta
    } else if max == g {
        2.0 + (b - r) / delta
    } else {
        4.0 + (r - g) / delta
    };
    [wrap_hue(h), s, max]
}

/// Wrap a hue into `0..6`. `rem_euclid` can return exactly 6.0 for a tiny
/// negative input in f32, which would select the wrong HSV sector.
fn wrap_hue(hue: f32) -> f32 {
    let wrapped = hue.rem_euclid(6.0);
    if wrapped >= 6.0 { 0.0 } else { wrapped }
}

/// HSV (hue in `0..6`) to RGB.
fn hsv_to_rgb([h, s, v]: [f32; 3]) -> [f32; 3] {
    let sector = h.floor();
    let f = h - sector;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match sector as i32 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: [f32; 3], expected: [f32; 3]) {
        for (a, e) in actual.iter().zip(expected) {
            assert!((a - e).abs() < 1.0e-4, "{actual:?} != {expected:?}");
        }
    }

    fn uniform(hue: usize, sat: usize, val: usize, delta: [f32; 3]) -> Vec<[f32; 3]> {
        vec![delta; hue * sat * val]
    }

    #[test]
    fn prophoto_matrices_map_white_to_white_and_round_trip() {
        let (forward, inverse) = rec2020_prophoto_matrices().expect("matrices");
        close(forward.transform([1.0, 1.0, 1.0]), [1.0, 1.0, 1.0]);
        let colour = [0.7, 0.2, 0.05];
        close(inverse.transform(forward.transform(colour)), colour);
    }

    #[test]
    fn identity_table_preserves_colour_including_over_range_values() {
        let deltas = uniform(6, 4, 1, [0.0, 1.0, 1.0]);
        let table = DeltaTable {
            hue_divisions: 6,
            sat_divisions: 4,
            val_divisions: 1,
            srgb_encoded: false,
            deltas: &deltas,
        };
        for rgb in [
            [0.8, 0.3, 0.1],
            [0.1, 0.6, 0.9],
            [0.5, 0.5, 0.5],
            [0.0, 0.0, 0.0],
            [3.0, 1.5, 0.2],
        ] {
            close(apply_delta_table(&table, rgb), rgb);
        }
    }

    #[test]
    fn constant_hue_shift_rotates_hue_and_saturation_scale_desaturates() {
        let shift = uniform(6, 4, 1, [60.0, 1.0, 1.0]);
        let table = DeltaTable {
            hue_divisions: 6,
            sat_divisions: 4,
            val_divisions: 1,
            srgb_encoded: false,
            deltas: &shift,
        };
        // Red rotated by +60 degrees is yellow.
        close(apply_delta_table(&table, [1.0, 0.0, 0.0]), [1.0, 1.0, 0.0]);

        let desaturate = uniform(6, 4, 1, [0.0, 0.5, 1.0]);
        let table = DeltaTable {
            deltas: &desaturate,
            ..table
        };
        close(apply_delta_table(&table, [0.8, 0.4, 0.0]), [0.8, 0.6, 0.4]);
    }

    #[test]
    fn value_axis_is_interpolated_and_hue_wraps() {
        // Two value planes: darks keep their value, brights are halved.
        let mut deltas = uniform(2, 2, 1, [0.0, 1.0, 1.0]);
        deltas.extend(uniform(2, 2, 1, [0.0, 1.0, 0.5]));
        let table = DeltaTable {
            hue_divisions: 2,
            sat_divisions: 2,
            val_divisions: 2,
            srgb_encoded: false,
            deltas: &deltas,
        };
        close(apply_delta_table(&table, [0.0, 0.0, 0.0]), [0.0, 0.0, 0.0]);
        close(
            apply_delta_table(&table, [1.0, 0.5, 0.25]),
            [0.5, 0.25, 0.125],
        );
        // Halfway up the value axis the scale is halfway between 1.0 and 0.5.
        close(
            apply_delta_table(&table, [0.5, 0.25, 0.125]),
            [0.375, 0.1875, 0.09375],
        );

        // Hue shift only on the last hue division: a hue just below 6 wraps to
        // blend with division 0 instead of reading past the table.
        let mut wrap = uniform(2, 2, 1, [0.0, 1.0, 1.0]);
        wrap[2] = [30.0, 1.0, 1.0];
        wrap[3] = [30.0, 1.0, 1.0];
        let table = DeltaTable {
            hue_divisions: 2,
            sat_divisions: 2,
            val_divisions: 1,
            srgb_encoded: false,
            deltas: &wrap,
        };
        let almost_red = [1.0, 0.0, 0.01];
        let shifted = apply_delta_table(&table, almost_red);
        assert!(shifted.iter().all(|value| value.is_finite()));
    }

    #[test]
    fn srgb_encoding_changes_where_the_table_is_sampled_not_the_identity() {
        let deltas = uniform(4, 3, 1, [0.0, 1.0, 1.0]);
        let table = DeltaTable {
            hue_divisions: 4,
            sat_divisions: 3,
            val_divisions: 1,
            srgb_encoded: true,
            deltas: &deltas,
        };
        close(
            apply_delta_table(&table, [0.6, 0.2, 0.05]),
            [0.6, 0.2, 0.05],
        );

        // A saturation scale applies to the encoded chroma, so the same table
        // gives a different linear result than the linear encoding.
        let halved = uniform(4, 3, 1, [0.0, 0.5, 1.0]);
        let encoded = apply_delta_table(
            &DeltaTable {
                deltas: &halved,
                ..table
            },
            [0.6, 0.2, 0.05],
        );
        let linear = apply_delta_table(
            &DeltaTable {
                deltas: &halved,
                srgb_encoded: false,
                ..table
            },
            [0.6, 0.2, 0.05],
        );
        assert!((encoded[2] - linear[2]).abs() > 1.0e-3);
    }

    #[test]
    fn negative_and_non_finite_pixels_are_left_alone() {
        let deltas = uniform(2, 2, 1, [90.0, 0.0, 0.0]);
        let table = DeltaTable {
            hue_divisions: 2,
            sat_divisions: 2,
            val_divisions: 1,
            srgb_encoded: false,
            deltas: &deltas,
        };
        assert_eq!(
            apply_delta_table(&table, [-0.1, 0.5, 0.2]),
            [-0.1, 0.5, 0.2]
        );
        assert!(apply_delta_table(&table, [f32::NAN, 0.5, 0.2])[0].is_nan());
    }
}
