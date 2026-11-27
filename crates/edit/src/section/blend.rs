//! Copying and blending sections between recipes.
//!
//! Copying is blending at full strength, so there is one place that knows
//! which fields a section owns. Both are pure: they return the new recipe and
//! leave undo and revisions to the caller.
//!
//! How a blend treats each kind of field, at strength `t` in `0..=1`:
//!
//! - **Numbers** (`f32`, and arrays of them) interpolate linearly, so `t = 0`
//!   is exactly the base value and `t = 1` exactly the preset value.
//! - **Counts** (`u8`) interpolate and round to the nearest whole number.
//! - **Everything else** (switches, enums, profile selections, the crop, mask
//!   layers, fitted looks) cannot be halfway, so it switches from the base
//!   value to the preset value at `t >= 0.5`.
//! - **The tone curve** interpolates its outputs when both curves have points
//!   at the same inputs, and switches at `t >= 0.5` otherwise.
//! - **White balance** interpolates its numbers when both sides use the same
//!   mode, and switches at `t >= 0.5` when the modes differ.
//!
//! Interpolating two valid values gives a valid value, so a blend of valid
//! recipes is valid.

use crate::{
    CaptureSharpening, ClipAdjustments, ColorGradingAdjustments, Defringe, FalseColorSuppression,
    HighlightAdjustments, HslChannelAdjustments, NoiseReduction, OpposedAdjustments,
    PresenceAdjustments, ToneCurve, WhiteBalance,
};
use crate::{EditRecipe, RecipeSection};

/// `base` with `sections` taken from `source`; all other sections are kept.
#[must_use]
pub fn copy_sections(
    base: &EditRecipe,
    source: &EditRecipe,
    sections: &[RecipeSection],
) -> EditRecipe {
    blend_sections(base, source, sections, 1.0)
}

/// `base` with `sections` moved towards `preset` by `amount`, as described in
/// the module documentation. `amount` is clamped to `0.0..=1.0` (NaN counts as
/// 0). Sections not listed, and the schema version, are kept from `base`.
#[must_use]
pub fn blend_sections(
    base: &EditRecipe,
    preset: &EditRecipe,
    sections: &[RecipeSection],
    amount: f32,
) -> EditRecipe {
    let mix = Mix::new(amount);
    let mut blended = base.clone();
    for &section in sections {
        // Read from `base`, not `blended`, so a section listed twice is not
        // blended twice.
        blend_section(&mut blended, base, preset, section, mix);
    }
    blended
}

/// The blend strength and the rules for combining values at that strength.
#[derive(Clone, Copy)]
struct Mix {
    t: f32,
}

impl Mix {
    fn new(amount: f32) -> Self {
        let t = if amount.is_nan() {
            0.0
        } else {
            amount.clamp(0.0, 1.0)
        };
        Self { t }
    }

    fn num(self, base: f32, preset: f32) -> f32 {
        // The ends are exact: `base + (preset - base)` need not give `preset`.
        if self.t <= 0.0 {
            base
        } else if self.t >= 1.0 {
            preset
        } else {
            base + (preset - base) * self.t
        }
    }

    fn array<const N: usize>(self, base: [f32; N], preset: [f32; N]) -> [f32; N] {
        std::array::from_fn(|i| self.num(base[i], preset[i]))
    }

    fn count(self, base: u8, preset: u8) -> u8 {
        // Both ends are whole numbers in `0..=255`, so the rounded value is too.
        self.num(f32::from(base), f32::from(preset)).round() as u8
    }

    fn pick<T: Clone>(self, base: &T, preset: &T) -> T {
        if self.t >= 0.5 { preset } else { base }.clone()
    }

    /// Curves with points at the same inputs move their outputs. Others
    /// cannot be halfway and switch.
    fn tone_curve(self, base: &ToneCurve, preset: &ToneCurve) -> ToneCurve {
        let same_inputs = base.points.len() == preset.points.len()
            && base
                .points
                .iter()
                .zip(&preset.points)
                .all(|(b, p)| b[0] == p[0]);
        if !same_inputs {
            return self.pick(base, preset);
        }
        let points = base.points.iter().zip(&preset.points);
        ToneCurve {
            points: points.map(|(b, p)| [b[0], self.num(b[1], p[1])]).collect(),
        }
    }

    fn white_balance(self, base: WhiteBalance, preset: WhiteBalance) -> WhiteBalance {
        match (base, preset) {
            (
                WhiteBalance::TemperatureTint {
                    temperature: base_temperature,
                    tint: base_tint,
                },
                WhiteBalance::TemperatureTint { temperature, tint },
            ) => WhiteBalance::TemperatureTint {
                temperature: self.num(base_temperature, temperature),
                tint: self.num(base_tint, tint),
            },
            (
                WhiteBalance::ManualMultipliers {
                    red: base_red,
                    green: base_green,
                    blue: base_blue,
                },
                WhiteBalance::ManualMultipliers { red, green, blue },
            ) => WhiteBalance::ManualMultipliers {
                red: self.num(base_red, red),
                green: self.num(base_green, green),
                blue: self.num(base_blue, blue),
            },
            _ => self.pick(&base, &preset),
        }
    }
}

/// Writes the fields `section` owns into `blended`. This is the single
/// section-to-field mapping; struct literals (rather than field assignments)
/// are used where a section owns a whole struct, so adding a field there fails
/// to compile until it is blended.
fn blend_section(
    blended: &mut EditRecipe,
    base: &EditRecipe,
    preset: &EditRecipe,
    section: RecipeSection,
    mix: Mix,
) {
    match section {
        RecipeSection::Tone => {
            let (b, p) = (&base.light, &preset.light);
            blended.light.exposure_ev = mix.num(b.exposure_ev, p.exposure_ev);
            blended.light.contrast = mix.num(b.contrast, p.contrast);
            blended.light.highlights = mix.num(b.highlights, p.highlights);
            blended.light.shadows = mix.num(b.shadows, p.shadows);
            blended.light.whites = mix.num(b.whites, p.whites);
            blended.light.blacks = mix.num(b.blacks, p.blacks);
        }
        RecipeSection::ToneCurve => {
            blended.light.tone_curve =
                mix.tone_curve(&base.light.tone_curve, &preset.light.tone_curve);
        }
        RecipeSection::Presence => {
            let (b, p) = (base.presence, preset.presence);
            blended.presence = PresenceAdjustments {
                texture: mix.num(b.texture, p.texture),
                clarity: mix.num(b.clarity, p.clarity),
                dehaze: mix.num(b.dehaze, p.dehaze),
            };
        }
        RecipeSection::WhiteBalance => {
            blended.color.white_balance =
                mix.white_balance(base.color.white_balance, preset.color.white_balance);
        }
        RecipeSection::Saturation => {
            let (b, p) = (&base.color, &preset.color);
            blended.color.saturation = mix.num(b.saturation, p.saturation);
            blended.color.vibrance = mix.num(b.vibrance, p.vibrance);
        }
        RecipeSection::ColorMixer => {
            let (b, p) = (&base.color.hsl.channels, &preset.color.hsl.channels);
            blended.color.hsl.channels = std::array::from_fn(|i| HslChannelAdjustments {
                hue: mix.num(b[i].hue, p[i].hue),
                saturation: mix.num(b[i].saturation, p[i].saturation),
                luminance: mix.num(b[i].luminance, p[i].luminance),
            });
        }
        RecipeSection::ColorGrading => {
            let (b, p) = (base.color.grading, preset.color.grading);
            blended.color.grading = ColorGradingAdjustments {
                shadows: mix.array(b.shadows, p.shadows),
                midtones: mix.array(b.midtones, p.midtones),
                highlights: mix.array(b.highlights, p.highlights),
            };
        }
        RecipeSection::ColorProfile => {
            blended.color.camera_profile =
                mix.pick(&base.color.camera_profile, &preset.color.camera_profile);
            blended.rendering.profile =
                mix.pick(&base.rendering.profile, &preset.rendering.profile);
        }
        RecipeSection::CameraLook => {
            blended.rendering.camera_tone =
                mix.pick(&base.rendering.camera_tone, &preset.rendering.camera_tone);
            blended.rendering.camera_chroma = mix.pick(
                &base.rendering.camera_chroma,
                &preset.rendering.camera_chroma,
            );
        }
        RecipeSection::HighlightReconstruction => {
            let (b, p) = (base.raw.highlights, preset.raw.highlights);
            blended.raw.highlights = HighlightAdjustments {
                method: mix.pick(&b.method, &p.method),
                clip: ClipAdjustments {
                    threshold: mix.num(b.clip.threshold, p.clip.threshold),
                },
                opposed: OpposedAdjustments {
                    detection_threshold: mix
                        .num(b.opposed.detection_threshold, p.opposed.detection_threshold),
                },
            };
        }
        RecipeSection::NoiseReduction => {
            let (b, p) = (base.noise_reduction, preset.noise_reduction);
            blended.noise_reduction = NoiseReduction {
                chroma: mix.num(b.chroma, p.chroma),
                luminance: mix.num(b.luminance, p.luminance),
                luminance_detail: mix.num(b.luminance_detail, p.luminance_detail),
            };
        }
        RecipeSection::CaptureSharpening => {
            let (b, p) = (base.capture_sharpening, preset.capture_sharpening);
            blended.capture_sharpening = CaptureSharpening {
                enabled: mix.pick(&b.enabled, &p.enabled),
                amount: mix.num(b.amount, p.amount),
                radius: mix.num(b.radius, p.radius),
                noise_protection: mix.num(b.noise_protection, p.noise_protection),
            };
        }
        RecipeSection::FalseColor => {
            blended.false_color = FalseColorSuppression {
                steps: mix.count(base.false_color.steps, preset.false_color.steps),
            };
        }
        RecipeSection::Defringe => {
            let (b, p) = (base.defringe, preset.defringe);
            blended.defringe = Defringe {
                purple_amount: mix.num(b.purple_amount, p.purple_amount),
                purple_hue: mix.array(b.purple_hue, p.purple_hue),
                green_amount: mix.num(b.green_amount, p.green_amount),
                green_hue: mix.array(b.green_hue, p.green_hue),
            };
        }
        RecipeSection::Optics => blended.optics = mix.pick(&base.optics, &preset.optics),
        RecipeSection::Geometry => blended.geometry = mix.pick(&base.geometry, &preset.geometry),
        RecipeSection::LocalMasks => blended.local = mix.pick(&base.local, &preset.local),
    }
}
