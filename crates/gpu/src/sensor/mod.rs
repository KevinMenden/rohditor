//! Bounded GPU execution for RAW sensor-development stages.
//!
//! This module begins at immutable decoded u16 Bayer samples and keeps every
//! completed state on the device. Normalization and the Off/Clip highlight
//! methods are available as typed states; later reconstruction and demosaic
//! stages will advance them without a CPU readback bridge.

mod highlight;
mod normalize;
mod resources;
#[cfg(test)]
mod tests;

pub use highlight::{GpuHighlightedMosaic, HighlightMetrics};
pub use normalize::{GpuNormalizedMosaic, GpuSensorProcessor, SensorMetrics};
