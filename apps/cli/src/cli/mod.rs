//! CLI facade for argument parsing, development commands, and output policy.

mod commands;
mod detail_compare;
mod detail_corpus;
mod highlight_compare;
mod look_report;

pub(crate) use commands::run;
