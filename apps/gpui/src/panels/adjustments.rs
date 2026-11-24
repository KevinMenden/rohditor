//! What the develop panel lists: its sections and how their sliders look. Values and defaults are
//! up to [`Parameter`].

use rohditor_edit::{Parameter, ParameterRange, RecipeSection};

use crate::ui::slider::{Format, Ramp, TEMPERATURE_RAMP, TINT_RAMP};

/// A parameter shown as a slider.
#[derive(Debug, Clone, Copy)]
pub struct Adjustment {
    pub parameter: Parameter,
    pub label: &'static str,
    pub format: Format,
    pub ramp: Option<Ramp>,
}

pub struct Section {
    pub section: RecipeSection,
    pub adjustments: &'static [Adjustment],
}

impl Adjustment {
    const fn new(parameter: Parameter, label: &'static str, format: Format) -> Self {
        Self {
            parameter,
            label,
            format,
            ramp: None,
        }
    }

    const fn percent(parameter: Parameter, label: &'static str) -> Self {
        Self::new(parameter, label, Format::Percent)
    }

    const fn ramp(self, ramp: Ramp) -> Self {
        Self {
            ramp: Some(ramp),
            ..self
        }
    }

    pub const fn range(self) -> ParameterRange {
        self.parameter.range()
    }
}

pub const SECTIONS: [Section; 6] = [
    Section {
        section: RecipeSection::WhiteBalance,
        adjustments: &[
            Adjustment::new(Parameter::Temperature, "Temperature", Format::Kelvin)
                .ramp(TEMPERATURE_RAMP),
            Adjustment::percent(Parameter::Tint, "Tint").ramp(TINT_RAMP),
        ],
    },
    Section {
        section: RecipeSection::Tone,
        adjustments: &[
            Adjustment::new(Parameter::Exposure, "Exposure", Format::Stops),
            Adjustment::percent(Parameter::Contrast, "Contrast"),
            Adjustment::percent(Parameter::Highlights, "Highlights"),
            Adjustment::percent(Parameter::Shadows, "Shadows"),
            Adjustment::percent(Parameter::Whites, "Whites"),
            Adjustment::percent(Parameter::Blacks, "Blacks"),
        ],
    },
    Section {
        section: RecipeSection::Saturation,
        adjustments: &[
            Adjustment::percent(Parameter::Vibrance, "Vibrance"),
            Adjustment::percent(Parameter::Saturation, "Saturation"),
        ],
    },
    Section {
        section: RecipeSection::Presence,
        adjustments: &[
            Adjustment::percent(Parameter::Texture, "Texture"),
            Adjustment::percent(Parameter::Clarity, "Clarity"),
            Adjustment::percent(Parameter::Dehaze, "Dehaze"),
        ],
    },
    Section {
        section: RecipeSection::CaptureSharpening,
        adjustments: &[Adjustment::percent(Parameter::CaptureAmount, "Amount")],
    },
    Section {
        section: RecipeSection::NoiseReduction,
        adjustments: &[
            Adjustment::percent(Parameter::LuminanceNoiseReduction, "Luminance"),
            Adjustment::percent(Parameter::ChromaNoiseReduction, "Color"),
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sliders_are_listed_once_and_in_their_section() {
        let mut seen = std::collections::HashSet::new();
        for section in &SECTIONS {
            for adjustment in section.adjustments {
                assert_eq!(adjustment.parameter.section(), section.section);
                assert!(seen.insert(adjustment.parameter), "{adjustment:?}");
            }
        }
    }
}
