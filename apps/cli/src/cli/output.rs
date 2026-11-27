//! Output helpers shared by the subcommands.

use std::io::{self, Write as _};
use std::time::Duration;

use anyhow::{Context, Result};

pub(super) fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

pub(super) fn write_stdout(value: &str) -> Result<()> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    match writeln!(handle, "{value}") {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error).context("could not write command output"),
    }
}
