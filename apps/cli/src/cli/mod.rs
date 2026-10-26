//! CLI facade for argument parsing, development commands, and output policy.

mod commands;
mod highlight_compare;

pub(crate) use commands::run;
