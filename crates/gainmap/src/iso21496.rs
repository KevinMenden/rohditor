//! ISO 21496-1 gain-map metadata payload (the bytes of the metadata box/segment body).
//!
//! Layout, all integers big-endian:
//!
//! | field | type |
//! |---|---|
//! | `minimum_version`, `writer_version` | `u16`, `u16` |
//! | flags: bit 7 multichannel, bit 6 use-base-colour-space, rest unsupported | `u8` |
//! | base headroom (log2) | `u32` numerator, `u32` denominator |
//! | alternate headroom (log2) | `u32` numerator, `u32` denominator |
//! | per channel: min, max | `i32`/`u32` fraction each |
//! | per channel: gamma | `u32`/`u32` fraction |
//! | per channel: base offset, alternate offset | `i32`/`u32` fraction each |
//!
//! Every real value is a fraction; this writer uses a fixed denominator of one million, so
//! values are exact to 1e-6 (1/64 and other binary fractions up to six decimals are exact).
//! The reader accepts any non-zero denominator, so it can read files from other writers.
//!
//! Only the payload is produced here. The surrounding framing (the JPEG APP2 `urn:iso:std:iso:ts:21496:-1`
//! segment, or the HEIF item property) belongs to the container writer.

use crate::{ChannelMetadata, GainMapError, GainMapMetadata};

/// Version this module writes and the newest one it reads.
const VERSION: u16 = 0;
const FLAG_MULTICHANNEL: u8 = 1 << 7;
const FLAG_USE_BASE_COLOUR_SPACE: u8 = 1 << 6;
/// Denominator used for every fraction this writer emits.
const DENOMINATOR: u32 = 1_000_000;

/// Serialises validated metadata. The gain map is declared to live in the base image's
/// colour space, which is how luminance is computed in this crate.
pub fn to_bytes(metadata: &GainMapMetadata) -> Result<Vec<u8>, GainMapError> {
    metadata.validate()?;
    let mut out = Vec::with_capacity(21 + 40 * metadata.channels.len());
    out.extend_from_slice(&VERSION.to_be_bytes()); // minimum_version
    out.extend_from_slice(&VERSION.to_be_bytes()); // writer_version
    let mut flags = FLAG_USE_BASE_COLOUR_SPACE;
    if metadata.channels.len() == 3 {
        flags |= FLAG_MULTICHANNEL;
    }
    out.push(flags);
    push_unsigned(&mut out, metadata.base_hdr_headroom_log2);
    push_unsigned(&mut out, metadata.alternate_hdr_headroom_log2);
    for channel in &metadata.channels {
        push_signed(&mut out, channel.min_log2);
        push_signed(&mut out, channel.max_log2);
        push_unsigned(&mut out, channel.gamma);
        push_signed(&mut out, channel.offset_sdr);
        push_signed(&mut out, channel.offset_hdr);
    }
    Ok(out)
}

/// Parses a payload written by this module or by another ISO 21496-1 writer.
///
/// Flag bits other than multichannel and use-base-colour-space are rejected rather than
/// ignored: one of them is the backward-direction flag (the base image is the HDR side),
/// which would make a reader apply the gain the wrong way round. Trailing bytes are an error
/// so a truncated or concatenated payload is not silently accepted.
pub fn from_bytes(bytes: &[u8]) -> Result<GainMapMetadata, GainMapError> {
    let mut reader = Reader { bytes };
    let minimum_version = reader.u16()?;
    if minimum_version > VERSION {
        return Err(GainMapError::UnsupportedIsoVersion(minimum_version));
    }
    let _writer_version = reader.u16()?;
    let flags = reader.u8()?;
    if flags & !(FLAG_MULTICHANNEL | FLAG_USE_BASE_COLOUR_SPACE) != 0 {
        return Err(GainMapError::UnsupportedIsoFlags(flags));
    }
    let channel_count = if flags & FLAG_MULTICHANNEL != 0 { 3 } else { 1 };
    let base_hdr_headroom_log2 = reader.unsigned()?;
    let alternate_hdr_headroom_log2 = reader.unsigned()?;
    let mut channels = Vec::with_capacity(channel_count);
    for _ in 0..channel_count {
        channels.push(ChannelMetadata {
            min_log2: reader.signed()?,
            max_log2: reader.signed()?,
            gamma: reader.unsigned()?,
            offset_sdr: reader.signed()?,
            offset_hdr: reader.signed()?,
        });
    }
    if !reader.bytes.is_empty() {
        return Err(GainMapError::MalformedIsoPayload("trailing bytes"));
    }
    let metadata = GainMapMetadata {
        channels,
        base_hdr_headroom_log2,
        alternate_hdr_headroom_log2,
    };
    metadata.validate()?;
    Ok(metadata)
}

fn push_unsigned(out: &mut Vec<u8>, value: f32) {
    // `validate` bounds every value to [0, 64], so the product fits a u32 (64e6 < 4.29e9).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let numerator = (f64::from(value) * f64::from(DENOMINATOR)).round() as u32;
    out.extend_from_slice(&numerator.to_be_bytes());
    out.extend_from_slice(&DENOMINATOR.to_be_bytes());
}

fn push_signed(out: &mut Vec<u8>, value: f32) {
    // Bounded to +-64 by `validate`, so the product fits an i32.
    #[allow(clippy::cast_possible_truncation)]
    let numerator = (f64::from(value) * f64::from(DENOMINATOR)).round() as i32;
    out.extend_from_slice(&numerator.to_be_bytes());
    out.extend_from_slice(&DENOMINATOR.to_be_bytes());
}

/// Big-endian cursor over the payload.
struct Reader<'a> {
    bytes: &'a [u8],
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], GainMapError> {
        let (head, rest) = self
            .bytes
            .split_first_chunk::<N>()
            .ok_or(GainMapError::MalformedIsoPayload("payload is truncated"))?;
        self.bytes = rest;
        Ok(*head)
    }

    fn u8(&mut self) -> Result<u8, GainMapError> {
        Ok(self.take::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, GainMapError> {
        Ok(u16::from_be_bytes(self.take()?))
    }

    fn denominator(&mut self) -> Result<f64, GainMapError> {
        match u32::from_be_bytes(self.take()?) {
            0 => Err(GainMapError::MalformedIsoPayload("zero denominator")),
            d => Ok(f64::from(d)),
        }
    }

    fn unsigned(&mut self) -> Result<f32, GainMapError> {
        let numerator = f64::from(u32::from_be_bytes(self.take()?));
        Ok((numerator / self.denominator()?) as f32)
    }

    fn signed(&mut self) -> Result<f32, GainMapError> {
        let numerator = f64::from(i32::from_be_bytes(self.take()?));
        Ok((numerator / self.denominator()?) as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> GainMapMetadata {
        GainMapMetadata {
            channels: vec![ChannelMetadata {
                min_log2: -0.5,
                max_log2: 2.75,
                gamma: 1.25,
                offset_sdr: 1.0 / 64.0,
                offset_hdr: 0.03125,
            }],
            base_hdr_headroom_log2: 0.0,
            alternate_hdr_headroom_log2: 2.75,
        }
    }

    #[test]
    fn writes_the_documented_layout() {
        let bytes = to_bytes(&sample()).unwrap();
        #[rustfmt::skip]
        let expected: Vec<u8> = [
            &[0, 0][..], &[0, 0], &[0x40],                          // versions, flags
            &0_u32.to_be_bytes(), &1_000_000_u32.to_be_bytes(),     // base headroom 0
            &2_750_000_u32.to_be_bytes(), &1_000_000_u32.to_be_bytes(),
            &(-500_000_i32).to_be_bytes(), &1_000_000_u32.to_be_bytes(),
            &2_750_000_i32.to_be_bytes(), &1_000_000_u32.to_be_bytes(),
            &1_250_000_u32.to_be_bytes(), &1_000_000_u32.to_be_bytes(),
            &15_625_i32.to_be_bytes(), &1_000_000_u32.to_be_bytes(),
            &31_250_i32.to_be_bytes(), &1_000_000_u32.to_be_bytes(),
        ].concat();
        assert_eq!(bytes, expected);
    }

    #[test]
    fn round_trips() {
        let metadata = sample();
        assert_eq!(from_bytes(&to_bytes(&metadata).unwrap()).unwrap(), metadata);
    }

    #[test]
    fn round_trips_three_channels() {
        let mut metadata = sample();
        metadata.channels = vec![
            metadata.channels[0],
            ChannelMetadata::new(0.0, 3.0),
            ChannelMetadata::new(-1.0, 1.0),
        ];
        let bytes = to_bytes(&metadata).unwrap();
        assert_eq!(bytes[4] & FLAG_MULTICHANNEL, FLAG_MULTICHANNEL);
        assert_eq!(from_bytes(&bytes).unwrap(), metadata);
    }

    #[test]
    fn reads_other_denominators() {
        // Headroom 3 written as 3/1 and 6/2, as another writer might.
        let mut bytes = to_bytes(&GainMapMetadata::single_channel(
            ChannelMetadata::new(0.0, 3.0),
            3.0,
        ))
        .unwrap();
        bytes[13..17].copy_from_slice(&6_u32.to_be_bytes());
        bytes[17..21].copy_from_slice(&2_u32.to_be_bytes());
        assert_eq!(from_bytes(&bytes).unwrap().alternate_hdr_headroom_log2, 3.0);
    }

    #[test]
    fn rejects_malformed_payloads() {
        let good = to_bytes(&sample()).unwrap();
        assert!(from_bytes(&good[..good.len() - 1]).is_err(), "truncated");
        let mut long = good.clone();
        long.push(0);
        assert!(from_bytes(&long).is_err(), "trailing byte");
        let mut zero_den = good.clone();
        zero_den[9..13].fill(0);
        assert!(from_bytes(&zero_den).is_err(), "zero denominator");
        let mut future = good;
        future[1] = 1;
        assert_eq!(
            from_bytes(&future),
            Err(GainMapError::UnsupportedIsoVersion(1))
        );
        assert!(from_bytes(&[]).is_err());
    }

    #[test]
    fn rejects_unsupported_flag_bits() {
        // Bit 5 is the backward-direction flag in the spec as I know it; every bit below the
        // two supported ones must be refused instead of silently misread.
        for bit in 0..6 {
            let mut bytes = to_bytes(&sample()).unwrap();
            bytes[4] |= 1 << bit;
            assert_eq!(
                from_bytes(&bytes),
                Err(GainMapError::UnsupportedIsoFlags(bytes[4])),
                "bit {bit}"
            );
        }
    }

    #[test]
    fn rejects_invalid_metadata_on_write() {
        let mut metadata = sample();
        metadata.channels[0].gamma = f32::NAN;
        assert!(to_bytes(&metadata).is_err());
    }
}
