//! Bounded GPU execution for RAW sensor-development stages.
//!
//! This module begins at immutable decoded u16 Bayer samples and keeps every
//! completed state on the device. Normalization and the Off/Clip highlight
//! methods feed bilinear/MHC and optional capture into resident camera planes.
//! Application backend selection and other reconstruction methods remain separate.

mod demosaic;
#[cfg(test)]
mod demosaic_tests;
mod highlight;
mod normalize;
mod resources;
#[cfg(test)]
mod tests;

pub use demosaic::{DemosaicMetrics, GpuSensorCameraSource};
pub use highlight::{GpuHighlightedMosaic, HighlightMetrics};
pub use normalize::{GpuNormalizedMosaic, GpuSensorProcessor, SensorMetrics};
