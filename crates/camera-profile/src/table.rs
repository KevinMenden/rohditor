use serde::{Deserialize, Serialize};

use crate::CameraProfileError;

/// Largest accepted division count per axis.
pub const MAX_TABLE_DIVISIONS: u32 = 360;
/// Largest accepted number of table entries (hue x saturation x value).
pub const MAX_TABLE_ENTRIES: usize = 131_072;

/// A DNG hue/saturation/value delta table (`ProfileHueSatMap` or
/// `ProfileLookTable`).
///
/// Each entry holds `[hue shift in degrees, saturation scale, value scale]`
/// at a grid point of HSV space. Entries are stored saturation-fastest, then
/// hue, then value, as in the DNG specification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileTable {
    pub hue_divisions: u32,
    pub sat_divisions: u32,
    pub val_divisions: u32,
    /// Whether RGB is sRGB-encoded before the HSV conversion and lookup
    /// (`...Encoding` tag value 1) rather than used as linear light.
    pub srgb_encoded: bool,
    pub deltas: Vec<[f32; 3]>,
}

impl ProfileTable {
    pub(crate) fn validate(&self, field: &'static str) -> Result<(), CameraProfileError> {
        let invalid = |reason: String| CameraProfileError::InvalidProfile { field, reason };
        let divisions = [self.hue_divisions, self.sat_divisions, self.val_divisions];
        if divisions
            .iter()
            .any(|&count| count == 0 || count > MAX_TABLE_DIVISIONS)
        {
            return Err(invalid(format!(
                "division counts {divisions:?} must be within 1..={MAX_TABLE_DIVISIONS}"
            )));
        }
        // The DNG lookup interpolates between saturation grid points, so a
        // single saturation division cannot be evaluated.
        if self.sat_divisions < 2 {
            return Err(invalid(
                "at least two saturation divisions are required".to_owned(),
            ));
        }
        let entries = divisions
            .iter()
            .map(|&count| count as usize)
            .product::<usize>();
        if entries > MAX_TABLE_ENTRIES || entries != self.deltas.len() {
            return Err(invalid(format!(
                "{} entries do not match the {entries} implied by the divisions (maximum {MAX_TABLE_ENTRIES})",
                self.deltas.len()
            )));
        }
        for [hue_shift, sat_scale, val_scale] in &self.deltas {
            if !hue_shift.is_finite()
                || hue_shift.abs() > 360.0
                || !sat_scale.is_finite()
                || !(0.0..=16.0).contains(sat_scale)
                || !val_scale.is_finite()
                || !(0.0..=16.0).contains(val_scale)
            {
                return Err(invalid(
                    "contains an out-of-range or non-finite delta".to_owned(),
                ));
            }
        }
        Ok(())
    }
}
