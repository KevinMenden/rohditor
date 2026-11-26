//! The settings the dialog lists, with what they are called and do.

/// One row of the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Demosaic,
    OutputGamut,
    AutoNoiseReduction,
    CpuOnly,
}

/// The sections of the dialog, in order.
pub const SECTIONS: [(&str, &[Setting]); 3] = [
    ("Processing", &[Setting::Demosaic, Setting::OutputGamut]),
    ("Editing", &[Setting::AutoNoiseReduction]),
    ("Performance", &[Setting::CpuOnly]),
];

impl Setting {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Demosaic => "Demosaic algorithm",
            Self::OutputGamut => "Output gamut mapping",
            Self::AutoNoiseReduction => "Automatic noise reduction",
            Self::CpuOnly => "CPU only",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Demosaic => {
                "How the sensor's colour mosaic becomes full-colour pixels, for the preview, 1:1 \
                 and export. AMaZE is the default."
            }
            Self::OutputGamut => {
                "How colours outside sRGB are brought into it. Clipping is the most compatible."
            }
            Self::AutoNoiseReduction => {
                "Estimate each new photo's noise and start noisy ones with colour noise \
                 reduction; low-ISO photos are left alone. Photos with saved edits keep them."
            }
            Self::CpuOnly => {
                "Don't use the GPU for image processing. Takes effect after a restart."
            }
        }
    }

    /// What a search looks through.
    pub fn search_text(self, section: &str) -> String {
        format!("{section} {} {}", self.title(), self.description())
    }
}
