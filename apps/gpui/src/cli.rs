//! The app's command line: what to open, and options for development and UI checks.

use std::path::PathBuf;
use std::time::Duration;

use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{Parser, ValueEnum};
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_engine::preview::ProcessorPreference;
use rohditor_engine::settings::{SAVED_DEMOSAICS, saved_demosaic};

use crate::session::StartupChoices;

#[derive(Debug, Parser)]
#[command(version, about = "Linux-first Sony RAW photo editor")]
pub struct Arguments {
    /// A RAW file to open, or a folder to show in the library.
    pub open: Option<PathBuf>,

    /// Image processor for this run, overriding the "CPU only" setting.
    #[arg(long, value_enum)]
    processor: Option<ProcessorChoice>,

    /// Demosaic algorithm. Saved as the setting, like a choice in the settings dialog.
    #[arg(long, value_parser = demosaic_parser())]
    demosaic: Option<DemosaicAlgorithm>,

    /// Show the diagnostics panel at startup.
    #[arg(long)]
    pub diagnostics: bool,

    /// Save a screenshot of the window to this PNG path, then exit.
    #[arg(long, value_name = "PATH")]
    screenshot: Option<PathBuf>,

    /// Seconds to wait at least before the `--screenshot` capture; it also waits for the preview
    /// to settle.
    #[arg(
        long,
        value_name = "SECONDS",
        default_value_t = 5.0,
        requires = "screenshot"
    )]
    screenshot_delay: f32,
}

/// A `--screenshot` request.
#[derive(Debug, Clone, PartialEq)]
pub struct Screenshot {
    pub path: PathBuf,
    pub delay: Duration,
}

impl Arguments {
    pub fn startup_choices(&self) -> StartupChoices {
        StartupChoices {
            demosaic: self.demosaic,
            processor: self.processor.map(Into::into),
        }
    }

    pub fn screenshot(&self) -> Option<Screenshot> {
        self.screenshot.clone().map(|path| Screenshot {
            path,
            delay: Duration::from_secs_f32(self.screenshot_delay.max(0.0)),
        })
    }
}

/// Command-line spelling of [`ProcessorPreference`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ProcessorChoice {
    /// Prefer the GPU, with CPU fallback.
    Auto,
    /// Require GPU previews; exports still recover to the CPU.
    Gpu,
    /// Use the CPU reference for previews and exports.
    Cpu,
}

impl From<ProcessorChoice> for ProcessorPreference {
    fn from(choice: ProcessorChoice) -> Self {
        match choice {
            ProcessorChoice::Auto => Self::Auto,
            ProcessorChoice::Gpu => Self::Gpu,
            ProcessorChoice::Cpu => Self::Cpu,
        }
    }
}

/// The algorithms the settings can hold, by their stable names.
fn demosaic_parser() -> impl TypedValueParser<Value = DemosaicAlgorithm> {
    PossibleValuesParser::new(SAVED_DEMOSAICS.map(DemosaicAlgorithm::stable_name))
        .map(|name| saved_demosaic(&name).expect("only saved names are possible"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Arguments, clap::Error> {
        Arguments::try_parse_from(std::iter::once("rohditor-gpui").chain(arguments.iter().copied()))
    }

    #[test]
    fn startup_options_parse() {
        let arguments = parse(&[
            "photos",
            "--processor",
            "cpu",
            "--demosaic",
            "rcd",
            "--diagnostics",
            "--screenshot",
            "/tmp/ui.png",
            "--screenshot-delay",
            "2.5",
        ])
        .expect("valid options parse");
        assert_eq!(arguments.open, Some(PathBuf::from("photos")));
        assert!(arguments.diagnostics);
        let choices = arguments.startup_choices();
        assert_eq!(choices.processor, Some(ProcessorPreference::Cpu));
        assert_eq!(choices.demosaic, Some(DemosaicAlgorithm::Rcd));
        assert_eq!(
            arguments.screenshot(),
            Some(Screenshot {
                path: PathBuf::from("/tmp/ui.png"),
                delay: Duration::from_millis(2500),
            })
        );
    }

    #[test]
    fn nothing_is_chosen_by_default() {
        let arguments = parse(&[]).expect("no arguments parse");
        assert_eq!(arguments.open, None);
        assert!(!arguments.diagnostics);
        assert_eq!(arguments.screenshot(), None);
        let choices = arguments.startup_choices();
        assert_eq!((choices.demosaic, choices.processor), (None, None));
    }

    #[test]
    fn only_saved_demosaics_are_offered() {
        for algorithm in SAVED_DEMOSAICS {
            let arguments = parse(&["--demosaic", algorithm.stable_name()]).expect("saved name");
            assert_eq!(arguments.startup_choices().demosaic, Some(algorithm));
        }
        // Bilinear cannot be saved, so it cannot be chosen here.
        assert!(parse(&["--demosaic", "bilinear"]).is_err());
    }

    #[test]
    fn the_screenshot_delay_needs_a_screenshot() {
        assert!(parse(&["--screenshot-delay", "1"]).is_err());
    }
}
