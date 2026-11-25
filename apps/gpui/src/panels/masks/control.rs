//! What the controls of the panel change.

use gpui::{App, Context, Entity};
use rohditor_edit::MaskShape;
use rohditor_engine::tools::{
    BrushSettings, BrushSlider, LocalSection, LocalSlider, MaskAction, RangeSlider, SliderPhase,
};

use super::MasksPanel;
use super::brush::brush_slider;
use super::range::range_slider;
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
    Brush(BrushSlider),
    /// Of the selected component, if it is a range.
    Range(RangeSlider),
    /// A handle of the bar that shows a luminance range.
    Handle(RangeSlider),
}

impl MasksPanel {
    fn brush_settings(&self, cx: &App) -> Option<BrushSettings> {
        Some(self.session.read(cx).masks()?.brush)
    }

    /// Change the selected range.
    fn set_range(
        &self,
        slider: RangeSlider,
        value: f32,
        phase: SliderPhase,
        cx: &mut Context<Self>,
    ) {
        self.session.update(cx, |session, cx| {
            let Some(masks) = session.masks() else {
                return;
            };
            if let (Some(layer), Some(component)) = (masks.selected_layer, masks.selected_component)
            {
                let action = MaskAction::RangeSlider {
                    layer,
                    component,
                    slider,
                    value,
                    phase,
                };
                session.edit_masks(action, cx);
            }
        });
    }
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
            Control::Brush(slider) => Some(brush_slider(slider)),
            Control::Range(slider) => Some(range_slider(slider)),
            Control::Mask(_) | Control::Component(_) | Control::Section(_) | Control::Handle(_) => {
                None
            }
        }
    }

    fn value(&self, control: Control, cx: &App) -> Option<f32> {
        let session = self.session.read(cx);
        let masks = session.masks()?;
        let layers = &session.document()?.edits.recipe().local;
        match control {
            Control::Slider(slider) => Some(slider.get(layers.layer(masks.selected_layer?)?)),
            Control::Brush(slider) => Some(slider.get(masks.brush)),
            Control::Range(slider) | Control::Handle(slider) => {
                let layer = layers.layer(masks.selected_layer?)?;
                slider.get(&layer.components.get(masks.selected_component?)?.shape)
            }
            Control::Mask(_) | Control::Component(_) | Control::Section(_) => None,
        }
    }

    fn default_value(&self, control: Control, _: &App) -> Option<f32> {
        match control {
            Control::Slider(slider) => Some(slider.range().neutral),
            Control::Brush(slider) => Some(slider.range().neutral),
            Control::Range(slider) | Control::Handle(slider) => Some(slider.default_value()),
            Control::Mask(_) | Control::Component(_) | Control::Section(_) => None,
        }
    }

    fn set_value(
        &mut self,
        control: Control,
        value: f32,
        phase: EditPhase,
        cx: &mut Context<Self>,
    ) {
        let phase = SliderPhase {
            changed: true,
            dragged: phase == EditPhase::Dragging,
            reset: phase == EditPhase::Finished,
            ..SliderPhase::default()
        };
        match control {
            Control::Slider(slider) => self.session.update(cx, |session, cx| {
                if let Some(mask) = session.masks().and_then(|masks| masks.selected_layer) {
                    session.edit_masks(MaskAction::Slider(mask, slider, value, phase), cx);
                }
            }),
            Control::Brush(slider) => {
                if let Some(brush) = self.brush_settings(cx) {
                    let action = MaskAction::SetBrush(slider.with(brush, value));
                    self.session
                        .update(cx, |session, cx| session.edit_masks(action, cx));
                }
            }
            Control::Range(slider) | Control::Handle(slider) => {
                self.set_range(slider, value, phase, cx);
            }
            Control::Mask(_) | Control::Component(_) | Control::Section(_) => {}
        }
    }

    fn move_by(&mut self, control: Control, right: f32, _: f32, cx: &mut Context<Self>) {
        if let Control::Handle(slider) = control
            && let Some(MaskShape::Luminance(range)) = self.model.range()
        {
            let position = slider.handle_position(range) + right;
            let value = slider.value_at_handle(range, position);
            self.set_value(control, value, EditPhase::Dragging, cx);
        }
    }
}
