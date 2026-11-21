//! CLI facade for argument parsing, development commands, and output policy.

mod commands;
mod demosaic_npy;
mod detail_compare;
mod detail_corpus;
mod highlight_compare;
mod look_report;
mod model;
mod neural_input;
mod npy;

pub(crate) use commands::run;
