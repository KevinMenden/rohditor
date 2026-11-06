//! Local adjustments (mask layers) on the GPU.
//!
//! The colour pass evaluates masks per pixel (`masks.wgsl`), in the same frame and with the same
//! maths as the CPU stage in `rohditor-core`. This module prepares what it reads:
//!
//! * [`tables`]: gradients, ranges and per-layer Light LUTs, resolved once per render.
//! * [`coverage`]: brush strokes, which stay on the CPU and reach the shader as 16-bit planes. A
//!   fit preview keeps a full-frame plane per brush between renders; full-resolution work
//!   rasterizes one band at a time so memory stays bounded.
//!
//! The mask overlay is deliberately not produced here. The CPU overlay jobs already give a small
//! image at any time, including during a drag, and only while the Masks tool shows one. An
//! overlay output in this pass would add a texture and a readback to every masked render.

pub(crate) mod coverage;
pub(crate) mod tables;
#[cfg(test)]
pub(crate) mod test_recipes;
