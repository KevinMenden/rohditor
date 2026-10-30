//! Frontend-agnostic application runtime on top of the processing crates.
//!
//! Owns what an interactive RAW editor needs beyond algorithms: render and
//! export workers with preview scheduling, preview caches, undo-aware edit
//! sessions, recipe sidecars, settings, and the catalog thumbnail worker.
//!
//! Nothing here depends on a UI toolkit. Frontends are woken through a plain
//! callback ([`WakeFn`]), receive plain pixel buffers, inject their own wgpu
//! device, and pass configuration directories in explicitly. This crate does
//! not wrap or re-export the lower crates; depend on them directly.

pub mod camera_profiles;
pub mod catalog;
pub mod coordinator;
pub mod document;
pub mod persistence;
pub mod preview_cache;
pub mod session;
pub mod settings;
pub mod storage;
pub mod wake;

pub use wake::WakeFn;
