//! What the develop panel lists: its groups of sections and how their sliders look. Values and defaults are
//! up to [`Parameter`].

use rohditor_edit::{HighlightMethod, Parameter, ParameterRange, RecipeSection};

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

pub struct Group {
    pub title: &'static str,
    pub sections: &'static [Section],
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

pub const GROUPS: [Group; 3] = [
    Group {
        title: "Light",
        sections: &[
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
                section: RecipeSection::ToneCurve,
                adjustments: &[
                    Adjustment::percent(Parameter::ToneCurveShadows, "Shadows"),
                    Adjustment::percent(Parameter::ToneCurveDarks, "Darks"),
                    Adjustment::percent(Parameter::ToneCurveLights, "Lights"),
                    Adjustment::percent(Parameter::ToneCurveHighlights, "Highlights"),
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
                section: RecipeSection::HighlightReconstruction,
                adjustments: &[
                    Adjustment::new(
                        Parameter::HighlightClipThreshold,
                        "Clip threshold",
                        Format::Factor,
                    ),
                    Adjustment::new(
                        Parameter::HighlightDetectionThreshold,
                        "Detection threshold",
                        Format::Factor,
                    ),
                ],
            },
        ],
    },
    Group {
        title: "Color",
        sections: &[
            Section {
                section: RecipeSection::WhiteBalance,
                adjustments: &[
                    Adjustment::new(Parameter::Temperature, "Temperature", Format::Kelvin)
                        .ramp(TEMPERATURE_RAMP),
                    Adjustment::percent(Parameter::Tint, "Tint").ramp(TINT_RAMP),
                ],
            },
            Section {
                section: RecipeSection::Saturation,
                adjustments: &[
                    Adjustment::percent(Parameter::Vibrance, "Vibrance"),
                    Adjustment::percent(Parameter::Saturation, "Saturation"),
                ],
            },
        ],
    },
    Group {
        title: "Detail",
        sections: &[
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
        ],
    },
];

pub fn sections() -> impl Iterator<Item = &'static Section> {
    GROUPS.iter().flat_map(|group| group.sections)
}

/// How the panel lists this parameter.
pub fn adjustment(parameter: Parameter) -> Adjustment {
    sections()
        .flat_map(|section| section.adjustments)
        .copied()
        .find(|adjustment| adjustment.parameter == parameter)
        .expect("the parameter is listed")
}

pub const HIGHLIGHT_METHODS: [HighlightMethod; 3] = [
    HighlightMethod::Off,
    HighlightMethod::Clip,
    HighlightMethod::Opposed,
];

pub const fn method_label(method: HighlightMethod) -> &'static str {
    match method {
        HighlightMethod::Off => "Off",
        HighlightMethod::Clip => "Clip",
        HighlightMethod::Opposed => "Reconstruct",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sliders_are_listed_once_and_in_their_section() {
        let mut seen = std::collections::HashSet::new();
        for section in sections() {
            for adjustment in section.adjustments {
                assert_eq!(adjustment.parameter.section(), section.section);
                assert!(seen.insert(adjustment.parameter), "{adjustment:?}");
            }
        }
    }
}
