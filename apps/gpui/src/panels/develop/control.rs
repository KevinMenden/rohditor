//! What the controls of the panel change.

use gpui::{App, Context, Entity};
use rohditor_core::ToneCurveHandle;
use rohditor_edit::{
    GradingTint, GradingTones, LensCorrection, Parameter, ParameterRange, RecipeSection,
};

use super::DevelopPanel;
use super::choices::Choice;
use crate::panels::adjustments::{adjustment, sections};
use crate::panels::controls::{ControlPanel, Controls, Gestures as _, Slider};
use crate::session::{EditPhase, Session};
use crate::ui::overlays::Overlays;
use crate::ui::slider::Format;

/// What in the panel can be changed or have the focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Control {
    /// The header of a section.
    Section(RecipeSection),
    Parameter(Parameter),
    Curve(ToneCurveHandle),
    Choice(Choice),
    MixerChannels,
    Wheel(GradingTones),
    Correction(LensCorrection),
    /// The headers below the sections.
    Presets,
    Info,
    PresetAmount,
}

const PRESET_AMOUNT_RANGE: ParameterRange = ParameterRange {
    minimum: 0.0,
    maximum: 1.0,
    neutral: 1.0,
};

impl Control {
    /// Those that can have the focus.
    pub fn focusable() -> impl Iterator<Item = Self> {
        let rows = sections().flat_map(|section| {
            let rows = section.adjustments.iter();
            let rows = rows.map(|row| Self::Parameter(row.parameter));
            std::iter::once(Self::Section(section.section)).chain(rows)
        });
        rows.chain(Choice::ALL.map(Self::Choice))
            .chain([
                Self::MixerChannels,
                Self::Presets,
                Self::Info,
                Self::PresetAmount,
            ])
            .chain(GradingTones::ALL.map(Self::Wheel))
            .chain(LensCorrection::ALL.map(Self::Correction))
    }

    /// What it changes, or the first of them.
    pub fn parameter(self) -> Option<Parameter> {
        match self {
            Self::Parameter(parameter) => Some(parameter),
            Self::Curve(handle) => Some(handle.parameter()),
            Self::Wheel(tones) => Some(tones.parameter()),
            _ => None,
        }
    }
}

impl ControlPanel for DevelopPanel {
    type Control = Control;

    fn session(&self) -> &Entity<Session> {
        &self.session
    }

    fn overlays(&self) -> &Entity<Overlays> {
        &self.overlays
    }

    fn controls(&self) -> &Controls<Control> {
        &self.controls
    }

    fn controls_mut(&mut self) -> &mut Controls<Control> {
        &mut self.controls
    }

    fn slider(&self, control: Control) -> Option<Slider> {
        match control {
            Control::Parameter(parameter) => Some(adjustment(parameter).slider()),
            Control::PresetAmount => {
                let (name, _) = self.model.applied_preset.as_ref()?;
                Some(Slider {
                    label: format!("Amount of {name}").into(),
                    format: Format::Amount,
                    range: PRESET_AMOUNT_RANGE,
                    ramp: None,
                })
            }
            _ => None,
        }
    }

    fn value(&self, control: Control, cx: &App) -> Option<f32> {
        if control == Control::PresetAmount {
            return self.model.applied_preset.as_ref().map(|applied| applied.1);
        }
        let session = self.session.read(cx);
        let document = session.document().filter(|open| open.frame.is_some())?;
        let defaults = document.default_recipe();
        Some(control.parameter()?.get(document.edits.recipe(), &defaults))
    }

    fn default_value(&self, control: Control, cx: &App) -> Option<f32> {
        if control == Control::PresetAmount {
            return Some(PRESET_AMOUNT_RANGE.neutral);
        }
        let defaults = self.session.read(cx).document()?.default_recipe();
        Some(control.parameter()?.get(&defaults, &defaults))
    }

    fn set_value(
        &mut self,
        control: Control,
        value: f32,
        phase: EditPhase,
        cx: &mut Context<Self>,
    ) {
        self.session.update(cx, |session, cx| match control {
            Control::PresetAmount => {
                session.set_preset_amount(value, cx);
                if phase == EditPhase::Finished {
                    session.finish_gesture(None, cx);
                }
            }
            _ => {
                if let Some(parameter) = control.parameter() {
                    session.set_parameter(parameter, value, phase, cx);
                }
            }
        });
    }

    fn move_by(&mut self, control: Control, right: f32, up: f32, cx: &mut Context<Self>) {
        match control {
            // The radius is half the size.
            Control::Wheel(tones) => self.move_wheel(tones, right * 2.0, up * 2.0, cx),
            Control::Curve(handle) => {
                let Some(document) = self.session.read(cx).document() else {
                    return;
                };
                let value = handle.dragged(&document.edits.recipe().light.tone_curve, up);
                self.set_value(control, value, EditPhase::Dragging, cx);
            }
            _ => {}
        }
    }

    fn finish_gesture(&mut self, control: Control, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| {
            session.finish_gesture(control.parameter(), cx);
        });
    }

    fn reset(&mut self, control: Control, cx: &mut Context<Self>) {
        self.finish_nudge(cx);
        if let Control::Wheel(tones) = control {
            self.set_wheel(tones, GradingTint::default(), EditPhase::Finished, cx);
        } else if let Some(value) = self.default_value(control, cx) {
            self.set_value(control, value, EditPhase::Finished, cx);
        }
    }

    fn is_open(&self, header: Control, cx: &App) -> bool {
        self.searching(cx) || !self.controls.collapsed.contains(&header)
    }
}
