//! What the controls of the panel change.

use gpui::{App, Context, Entity};
use rohditor_engine::tools::{LocalSection, LocalSlider, MaskAction, SliderPhase};

use super::MasksPanel;
use super::sliders::local_slider;
use crate::panels::controls::{ControlPanel, Controls, Slider};
use crate::session::{EditPhase, Session};
use crate::ui::overlays::Overlays;

/// What in the panel can be changed or have the focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Control {
    Mask(u32),
    /// Of the selected mask, as the sliders.
    Component(usize),
    Section(LocalSection),
    Slider(LocalSlider),
}

impl ControlPanel for MasksPanel {
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
            Control::Slider(slider) => Some(local_slider(slider)),
            _ => None,
        }
    }

    fn value(&self, control: Control, cx: &App) -> Option<f32> {
        let Control::Slider(slider) = control else {
            return None;
        };
        let session = self.session.read(cx);
        let mask = session.masks()?.selected_layer?;
        let recipe = session.document()?.edits.recipe();
        Some(slider.get(recipe.local.layer(mask)?))
    }

    fn default_value(&self, control: Control, _: &App) -> Option<f32> {
        match control {
            Control::Slider(slider) => Some(slider.range().neutral),
            _ => None,
        }
    }

    fn set_value(
        &mut self,
        control: Control,
        value: f32,
        phase: EditPhase,
        cx: &mut Context<Self>,
    ) {
        let Control::Slider(slider) = control else {
            return;
        };
        let phase = SliderPhase {
            changed: true,
            dragged: phase == EditPhase::Dragging,
            reset: phase == EditPhase::Finished,
            ..SliderPhase::default()
        };
        self.session.update(cx, |session, cx| {
            if let Some(mask) = session.masks().and_then(|masks| masks.selected_layer) {
                session.edit_masks(MaskAction::Slider(mask, slider, value, phase), cx);
            }
        });
    }
}
