//! The validated description of a gain map (what ISO 21496-1 and Ultra HDR XMP both carry).

use crate::{DEFAULT_OFFSET, GainMapError};

/// Largest magnitude any log2 value may have. Far beyond real content (HDR headroom is a few
/// stops) and small enough that every field fits the ISO rational encoding.
pub(crate) const MAX_LOG2_MAGNITUDE: f32 = 64.0;

/// Gain-map parameters of one channel. All `*_log2` values are in stops (log2 of a ratio).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelMetadata {
    /// Boost stored as encoded value 0 (`GainMapMin`).
    pub min_log2: f32,
    /// Boost stored as encoded value 255 (`GainMapMax`).
    pub max_log2: f32,
    /// Encoding exponent: `encoded = normalised.powf(gamma)`.
    pub gamma: f32,
    /// Offset added to the SDR value before the ratio (`OffsetSDR`).
    pub offset_sdr: f32,
    /// Offset added to the HDR value before the ratio (`OffsetHDR`).
    pub offset_hdr: f32,
}

impl ChannelMetadata {
    /// A channel spanning `[min_log2, max_log2]` with gamma 1 and the standard offsets.
    #[must_use]
    pub const fn new(min_log2: f32, max_log2: f32) -> Self {
        Self {
            min_log2,
            max_log2,
            gamma: 1.0,
            offset_sdr: DEFAULT_OFFSET,
            offset_hdr: DEFAULT_OFFSET,
        }
    }

    fn validate(&self) -> Result<(), GainMapError> {
        let bad = |name, reason| Err(GainMapError::InvalidMetadata { name, reason });
        for (name, value) in [("min_log2", self.min_log2), ("max_log2", self.max_log2)] {
            if !value.is_finite() || value.abs() > MAX_LOG2_MAGNITUDE {
                return bad(name, "must be finite and within +-64 stops");
            }
        }
        if self.min_log2 > self.max_log2 {
            return bad("min_log2", "must not exceed max_log2");
        }
        if !(self.gamma.is_finite() && self.gamma > 0.0 && self.gamma <= MAX_LOG2_MAGNITUDE) {
            return bad("gamma", "must be positive and at most 64");
        }
        for (name, value) in [
            ("offset_sdr", self.offset_sdr),
            ("offset_hdr", self.offset_hdr),
        ] {
            if !(value.is_finite() && (0.0..=MAX_LOG2_MAGNITUDE).contains(&value)) {
                return bad(name, "must be finite and non-negative");
            }
        }
        Ok(())
    }
}

/// Everything a reader needs to decode a gain map.
///
/// The base image is always the SDR side and the alternate the HDR side (the only direction v1
/// writes), so offsets map as SDR = base, HDR = alternate.
#[derive(Debug, Clone, PartialEq)]
pub struct GainMapMetadata {
    /// One entry for a luminance map, three (R, G, B) for a colour map.
    pub channels: Vec<ChannelMetadata>,
    /// Headroom of the base image in stops (`HDRCapacityMin`); 0 for an SDR base.
    pub base_hdr_headroom_log2: f32,
    /// Headroom of the fully boosted image in stops (`HDRCapacityMax`).
    pub alternate_hdr_headroom_log2: f32,
}

impl GainMapMetadata {
    /// Metadata for a single-channel (luminance) map over an SDR base.
    #[must_use]
    pub fn single_channel(channel: ChannelMetadata, alternate_hdr_headroom_log2: f32) -> Self {
        Self {
            channels: vec![channel],
            base_hdr_headroom_log2: 0.0,
            alternate_hdr_headroom_log2,
        }
    }

    /// Checks every field. Serialisers and [`crate::apply_gain_map`] call this first.
    pub fn validate(&self) -> Result<(), GainMapError> {
        if self.channels.len() != 1 && self.channels.len() != 3 {
            return Err(GainMapError::UnsupportedChannelCount(self.channels.len()));
        }
        for channel in &self.channels {
            channel.validate()?;
        }
        let bad = |name, reason| Err(GainMapError::InvalidMetadata { name, reason });
        for (name, value) in [
            ("base_hdr_headroom_log2", self.base_hdr_headroom_log2),
            (
                "alternate_hdr_headroom_log2",
                self.alternate_hdr_headroom_log2,
            ),
        ] {
            if !(value.is_finite() && (0.0..=MAX_LOG2_MAGNITUDE).contains(&value)) {
                return bad(name, "must be finite and non-negative");
            }
        }
        if self.alternate_hdr_headroom_log2 < self.base_hdr_headroom_log2 {
            return bad(
                "alternate_hdr_headroom_log2",
                "must not be below the base headroom",
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> GainMapMetadata {
        GainMapMetadata::single_channel(ChannelMetadata::new(0.0, 3.0), 3.0)
    }

    #[test]
    fn accepts_valid_metadata() {
        valid().validate().unwrap();
    }

    #[test]
    fn rejects_each_invalid_field() {
        let cases: [fn(&mut GainMapMetadata); 8] = [
            |m| m.channels[0].min_log2 = f32::NAN,
            |m| m.channels[0].max_log2 = f32::INFINITY,
            |m| m.channels[0].min_log2 = 4.0,
            |m| m.channels[0].gamma = 0.0,
            |m| m.channels[0].offset_sdr = -0.1,
            |m| m.channels[0].offset_hdr = f32::NAN,
            |m| m.alternate_hdr_headroom_log2 = -1.0,
            |m| m.base_hdr_headroom_log2 = 4.0,
        ];
        for (index, mutate) in cases.iter().enumerate() {
            let mut metadata = valid();
            mutate(&mut metadata);
            assert!(metadata.validate().is_err(), "case {index} was accepted");
        }
    }

    #[test]
    fn rejects_two_channels() {
        let mut metadata = valid();
        metadata.channels.push(metadata.channels[0]);
        assert_eq!(
            metadata.validate(),
            Err(GainMapError::UnsupportedChannelCount(2))
        );
    }
}
