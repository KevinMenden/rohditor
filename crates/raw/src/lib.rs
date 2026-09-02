//! Camera-RAW decoding behind Rohditor-owned types.
//!
//! No `rawler` type is part of this crate's public API. That boundary is
//! intentional because `rawler` does not currently promise a stable API.

mod decoder;
mod model;
mod rawler_adapter;

pub use decoder::{DecoderLimits, RawDecoder, RawError, RawSession};
pub use model::{
    CameraColorMatrix, CaptureMetadata, CfaPattern, EmbeddedPreviewInfo, EncodedPreview,
    EncodedPreviewFormat, ImageRect, LevelPattern, PhotometricInterpretation, RationalValue,
    RawFileInfo, RawFrame, SourceIdentity,
};
pub use rohditor_image::Orientation as RawOrientation;
pub use rawler_adapter::RawlerDecoder;
