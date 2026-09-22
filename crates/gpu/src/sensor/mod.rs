//! Bounded GPU execution for RAW sensor-development stages.
//!
//! This module begins at immutable decoded u16 Bayer samples and keeps every
//! completed state on the device. The initial slice provides normalization
//! only; highlight reconstruction and demosaic will advance its typed mosaic
//! state without a CPU readback bridge.

mod normalize;
mod resources;
#[cfg(test)]
mod tests;

pub use normalize::{GpuNormalizedMosaic, GpuSensorProcessor, SensorMetrics};
