//! What the develop panel lists: its groups of sections and how their sliders look. Values and
//! defaults are up to [`Parameter`].

use rohditor_edit::{
    HSL_CHANNEL_COUNT, HighlightMethod, Parameter, ParameterRange, RecipeSection,
    RenderingProfileSelection,
};

use crate::panels::controls::Slider;
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

    const fn amount(parameter: Parameter, label: &'static str) -> Self {
        Self::new(parameter, label, Format::Amount)
    }

    const fn hue(parameter: Parameter, label: &'static str) -> Self {
        Self::new(parameter, label, Format::Degrees).ramp(Ramp::Hues)
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

    pub fn slider(self) -> Slider {
        Slider {
            label: self.label.into(),
            format: self.format,
            range: self.range(),
            ramp: self.ramp,
        }
    }
}

pub const GROUPS: [Group; 4] = [
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
            Section {
                section: RecipeSection::ColorMixer,
                adjustments: &MIXER,
            },
            // Wheels.
            Section {
                section: RecipeSection::ColorGrading,
                adjustments: &[],
            },
            // Choices only.
            Section {
                section: RecipeSection::ColorProfile,
                adjustments: &[],
            },
        ],
    },
    Group {
        title: "Detail",
        sections: &[
            Section {
                section: RecipeSection::NoiseReduction,
                adjustments: &[
                    Adjustment::amount(Parameter::ChromaNoiseReduction, "Color"),
                    Adjustment::amount(Parameter::LuminanceNoiseReduction, "Luminance"),
                    Adjustment::amount(Parameter::LuminanceDetail, "Luminance detail"),
                ],
            },
            Section {
                section: RecipeSection::CaptureSharpening,
                adjustments: &[
                    Adjustment::amount(Parameter::CaptureAmount, "Amount"),
                    Adjustment::new(Parameter::CaptureRadius, "Radius", Format::Pixels),
                    Adjustment::amount(Parameter::CaptureNoise, "Noise protection"),
                ],
            },
            Section {
                section: RecipeSection::FalseColor,
                adjustments: &[Adjustment::new(
                    Parameter::FalseColorSteps,
                    "Steps",
                    Format::Count,
                )],
            },
            Section {
                section: RecipeSection::Defringe,
                adjustments: &[
                    Adjustment::amount(Parameter::DefringePurpleAmount, "Purple"),
                    Adjustment::hue(Parameter::DefringePurpleHueStart, "Purple hue from"),
                    Adjustment::hue(Parameter::DefringePurpleHueEnd, "Purple hue to"),
                    Adjustment::amount(Parameter::DefringeGreenAmount, "Green"),
                    Adjustment::hue(Parameter::DefringeGreenHueStart, "Green hue from"),
                    Adjustment::hue(Parameter::DefringeGreenHueEnd, "Green hue to"),
                ],
            },
        ],
    },
    Group {
        title: "Lens",
        sections: &[
            // A choice and switches.
            Section {
                section: RecipeSection::Optics,
                adjustments: &[],
            },
        ],
    },
];

pub fn sections() -> impl Iterator<Item = &'static Section> {
    GROUPS.iter().flat_map(|group| group.sections)
}

/// What a section does, where its title does not say it all.
pub const fn section_hint(section: RecipeSection) -> Option<&'static str> {
    match section {
        RecipeSection::NoiseReduction => Some(
            "Removes color noise, then luminance noise. More luminance detail keeps more fine \
             texture, and some grain. Best judged at 1:1.",
        ),
        RecipeSection::CaptureSharpening => {
            Some("Recovers fine detail lost in the lens and the sensor. Best judged at 1:1.")
        }
        RecipeSection::FalseColor => Some(
            "Removes rainbow bands on fine patterns such as fabric. Each step can wash out more \
             of the tiny color details. Best judged at 1:1.",
        ),
        RecipeSection::Defringe => Some(
            "Takes the color out of fringes along high-contrast edges whose hue is in the range. \
             Best judged at 1:1.",
        ),
        RecipeSection::Optics => {
            Some("Corrects what the lens does to the photo, as measured by the Lensfun project.")
        }
        _ => None,
    }
}

/// How the panel lists this parameter.
pub fn adjustment(parameter: Parameter) -> Adjustment {
    sections()
        .flat_map(|section| section.adjustments)
        .copied()
        .find(|adjustment| adjustment.parameter == parameter)
        .expect("the parameter is listed")
}

/// The colours of the mixer, at their hues in degrees.
const MIXER_HUES: [i32; HSL_CHANNEL_COUNT] = [0, 30, 60, 120, 180, 240, 270, 300];

pub const MIXER_LABELS: [&str; HSL_CHANNEL_COUNT] = [
    "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
];

pub const MIXER_COLORS: [u32; HSL_CHANNEL_COUNT] = {
    let mut colors = [0; HSL_CHANNEL_COUNT];
    let mut channel = 0;
    while channel < HSL_CHANNEL_COUNT {
        colors[channel] = hsl(MIXER_HUES[channel], 65, 52);
        channel += 1;
    }
    colors
};

/// Names of the sliders of each colour, which a search finds.
const MIXER_ROWS: [[&str; 3]; HSL_CHANNEL_COUNT] = [
    ["Red hue", "Red saturation", "Red luminance"],
    ["Orange hue", "Orange saturation", "Orange luminance"],
    ["Yellow hue", "Yellow saturation", "Yellow luminance"],
    ["Green hue", "Green saturation", "Green luminance"],
    ["Aqua hue", "Aqua saturation", "Aqua luminance"],
    ["Blue hue", "Blue saturation", "Blue luminance"],
    ["Purple hue", "Purple saturation", "Purple luminance"],
    ["Magenta hue", "Magenta saturation", "Magenta luminance"],
];

/// A full hue slider shifts a colour by this many degrees.
const HUE_SHIFT: i32 = 45;

const MIXER: [Adjustment; HSL_CHANNEL_COUNT * 3] = {
    let mut rows = [Adjustment::percent(Parameter::HslHue(0), ""); HSL_CHANNEL_COUNT * 3];
    let mut channel = 0;
    while channel < HSL_CHANNEL_COUNT {
        let hue = MIXER_HUES[channel];
        let [hue_label, saturation_label, luminance_label] = MIXER_ROWS[channel];
        let color = MIXER_COLORS[channel];
        rows[channel * 3] =
            Adjustment::percent(Parameter::HslHue(channel), hue_label).ramp(Ramp::Through([
                hsl(hue - HUE_SHIFT, 65, 52),
                color,
                hsl(hue + HUE_SHIFT, 65, 52),
            ]));
        rows[channel * 3 + 1] =
            Adjustment::percent(Parameter::HslSaturation(channel), saturation_label).ramp(
                Ramp::Through([hsl(hue, 0, 52), hsl(hue, 40, 52), hsl(hue, 85, 52)]),
            );
        rows[channel * 3 + 2] =
            Adjustment::percent(Parameter::HslLuminance(channel), luminance_label)
                .ramp(Ramp::Through([hsl(hue, 65, 18), color, hsl(hue, 65, 82)]));
        channel += 1;
    }
    rows
};

/// A colour of this hue in degrees, and saturation and lightness in percent, as `0xrrggbb`.
const fn hsl(hue: i32, saturation: i32, lightness: i32) -> u32 {
    let hue = hue.rem_euclid(360);
    // Scaled by 10000.
    let chroma = (100 - (2 * lightness - 100).abs()) * saturation;
    let middle = chroma * (60 - (hue % 120 - 60).abs()) / 60;
    let low = lightness * 100 - chroma / 2;
    let (red, green, blue) = match hue / 60 {
        0 => (chroma, middle, 0),
        1 => (middle, chroma, 0),
        2 => (0, chroma, middle),
        3 => (0, middle, chroma),
        4 => (middle, 0, chroma),
        _ => (chroma, 0, middle),
    };
    const fn level(value: i32) -> u32 {
        (value * 255 / 10_000) as u32
    }
    level(red + low) << 16 | level(green + low) << 8 | level(blue + low)
}

pub const RENDERINGS: [RenderingProfileSelection; 3] = [
    RenderingProfileSelection::MATCHED,
    RenderingProfileSelection::STANDARD,
    RenderingProfileSelection::NEUTRAL,
];

pub const fn rendering_label(rendering: RenderingProfileSelection) -> &'static str {
    match rendering {
        RenderingProfileSelection::RohditorMatched { .. } => "Matched",
        RenderingProfileSelection::RohditorStandard { .. } => "Standard",
        RenderingProfileSelection::RohditorNeutral => "Neutral",
    }
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
    fn mixer_colors_follow_their_hues() {
        assert_eq!(hsl(0, 100, 50), 0xff0000);
        assert_eq!(hsl(60, 100, 50), 0xffff00);
        assert_eq!(hsl(-120, 100, 50), 0x0000ff);
        assert_eq!(hsl(200, 0, 50), 0x7f7f7f);
        let centers =
            rohditor_core::HSL_CHANNEL_CENTERS.map(|turns| (turns * 360.0).round() as i32);
        assert_eq!(MIXER_HUES, centers);
        let shift = rohditor_core::HSL_HUE_SHIFT_PER_FULL_VALUE * 360.0;
        assert_eq!(HUE_SHIFT as f32, shift);
    }

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
