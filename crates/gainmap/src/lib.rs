//! HDR gain maps: computation, inverse, and metadata serialisation.
//!
//! A gain-map image is an SDR base plus a small second image that stores, per pixel, how many
//! stops brighter the HDR rendition is. A viewer scales that boost to the headroom of the
//! display; any other viewer shows the base. This crate holds the pure maths and the metadata
//! byte formats. It has no I/O and no codec: container assembly (JPEG, HEIC) is the caller's.
//!
//! # Inputs
//!
//! [`compute_gain_map`] takes two *linear* RGB images in the **same primaries** (the base
//! image's, Display P3 for Apple) at the same size: the SDR base with its transfer function
//! already removed, and the extended-range HDR rendition relative to the same diffuse white
//! (1.0). Luminance is computed in those primaries, so the caller does the conversion once.
//!
//! # Gain-map encoding
//!
//! Per pixel `g = log2((hdr_luma + offset_hdr) / (sdr_luma + offset_sdr))`. The gain is clamped
//! to `[min, max]`, averaged over each `downsample x downsample` block (an area filter in the
//! log domain), normalised to `[0, 1]`, raised to `gamma` and quantised to 8 bits. Decoding
//! ([`apply_gain_map`]) inverts each step. This is the ISO 21496-1 / Ultra HDR formulation.
//!
//! A single luminance gain reproduces the HDR rendition's *luminance* exactly, and its colour
//! when hue is preserved. With non-zero offsets (the Ultra HDR default is 1/64) a dark channel
//! of a saturated pixel overshoots slightly, as in every single-channel gain-map file.
//!
//! # Metadata
//!
//! [`GainMapMetadata`] is the one validated description of a map. [`iso21496`] and [`xmp`] turn
//! it into the two byte payloads; the JPEG segment framing around them belongs to the exporter.
//!
//! # Channels
//!
//! v1 computes single-channel (luminance) maps. [`GainMapImage`] and [`GainMapMetadata`] already
//! carry a channel count so three-channel maps can be added without breaking the types.

#![cfg_attr(test, allow(clippy::unwrap_used))]

mod apply;
mod compute;
mod image;
pub mod iso21496;
mod metadata;
pub mod xmp;

pub use apply::{apply_gain_map, boost_weight};
pub use compute::{GainMapParams, compute_gain_map};
pub use image::{GainMapImage, LinearRgbView};
pub use metadata::{ChannelMetadata, GainMapMetadata};

use thiserror::Error;

/// Version of the gain-map maths. Bump it when a change alters the produced bytes, so cached
/// or stored results can be invalidated (like the other stages' `*_ALGORITHM_VERSION`).
pub const GAINMAP_ALGORITHM_VERSION: u32 = 1;

/// Luminance weights of the Display P3 primaries (D65), the base primaries Apple uses.
pub const P3_LUMA_WEIGHTS: [f32; 3] = [0.228_975, 0.691_738, 0.079_287];

/// Offset both Ultra HDR and Apple use to keep the ratio finite near black (1/64).
pub const DEFAULT_OFFSET: f32 = 1.0 / 64.0;

/// Errors from gain-map computation, metadata validation and (de)serialisation.
#[derive(Debug, Error, PartialEq)]
pub enum GainMapError {
    #[error("invalid image dimensions {width}x{height} with {channels} channel(s): {reason}")]
    InvalidDimensions {
        width: usize,
        height: usize,
        channels: usize,
        reason: &'static str,
    },

    #[error("SDR image is {sdr_width}x{sdr_height} but HDR image is {hdr_width}x{hdr_height}")]
    SizeMismatch {
        sdr_width: usize,
        sdr_height: usize,
        hdr_width: usize,
        hdr_height: usize,
    },

    #[error("invalid gain-map parameter {name}: {reason}")]
    InvalidParams {
        name: &'static str,
        reason: &'static str,
    },

    #[error("invalid gain-map metadata {name}: {reason}")]
    InvalidMetadata {
        name: &'static str,
        reason: &'static str,
    },

    #[error("{0}-channel gain maps are not supported here")]
    UnsupportedChannelCount(usize),

    #[error("malformed ISO 21496-1 payload: {0}")]
    MalformedIsoPayload(&'static str),

    #[error("unsupported ISO 21496-1 minimum version {0}")]
    UnsupportedIsoVersion(u16),
}
