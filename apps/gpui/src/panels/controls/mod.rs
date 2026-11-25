//! What the panels of adjustments share: sliders, section headers and the keyboard focus.
//!
//! A panel names its controls by a type of its own and implements [`ControlPanel`] for what they
//! mean. The rest comes with the traits of the submodules, which every such panel has.

mod focus;
mod gestures;
mod row;
mod section;

use std::collections::HashSet;
use std::fmt::Debug;
use std::hash::Hash;

use gpui::{App, Context, Entity, Render, SharedString};
use rohditor_edit::ParameterRange;

pub use focus::{FocusControls, PanelFocus};
pub use gestures::{Gestures, drag_tracker};
pub use row::{
    CONTEXT as SLIDER_CONTEXT, Decrease, DecreaseMore, Increase, IncreaseMore, ResetValue,
    RowState, SliderRows, press_area,
};
pub use section::{Header, SectionHeaders};

use crate::session::{EditPhase, Session};
use crate::ui::overlays::Overlays;
use crate::ui::slider::{Format, Ramp};
use gestures::{Drag, Nudge, Typing};

pub fn bind_keys(cx: &mut App) {
    focus::bind_keys(cx);
    row::bind_keys(cx);
    section::bind_keys(cx);
}

/// How a slider is listed and its value is written.
#[derive(Debug, Clone)]
pub struct Slider {
    pub label: SharedString,
    pub format: Format,
    pub range: ParameterRange,
    pub ramp: Option<Ramp>,
}

pub trait ControlPanel: Render {
    /// What in the panel can be changed or have the focus.
    type Control: Copy + Eq + Hash + Debug + 'static;

    fn session(&self) -> &Entity<Session>;
    fn overlays(&self) -> &Entity<Overlays>;
    fn controls(&self) -> &Controls<Self::Control>;
    fn controls_mut(&mut self) -> &mut Controls<Self::Control>;

    /// The slider that the control is, if it is one.
    fn slider(&self, control: Self::Control) -> Option<Slider>;

    /// Its value, or `None` while it cannot be changed.
    fn value(&self, control: Self::Control, cx: &App) -> Option<f32>;

    fn default_value(&self, control: Self::Control, cx: &App) -> Option<f32>;

    fn set_value(
        &mut self,
        control: Self::Control,
        value: f32,
        phase: EditPhase,
        cx: &mut Context<Self>,
    );

    /// Drag a control that is no slider, by parts of its width and upwards of its height.
    fn move_by(&mut self, _control: Self::Control, _right: f32, _up: f32, _cx: &mut Context<Self>) {
    }

    /// A gesture that changed the control is over.
    fn finish_gesture(&mut self, _control: Self::Control, cx: &mut Context<Self>) {
        let session = self.session().clone();
        session.update(cx, |session, cx| session.finish_gesture(None, cx));
    }

    fn reset(&mut self, control: Self::Control, cx: &mut Context<Self>) {
        self.finish_nudge(cx);
        if let Some(value) = self.default_value(control, cx) {
            self.set_value(control, value, EditPhase::Finished, cx);
        }
    }

    /// Whether what is below the header is shown.
    fn is_open(&self, header: Self::Control, _cx: &App) -> bool {
        !self.controls().collapsed.contains(&header)
    }
}

/// The state of a panel's controls.
pub struct Controls<C> {
    pub focus: PanelFocus<C>,
    /// The headers of the collapsed sections.
    pub collapsed: HashSet<C>,
    drag: Option<Drag<C>>,
    nudge: Option<Nudge<C>>,
    typing: Option<Typing<C>>,
}

impl<C: Copy + Eq + Hash> Controls<C> {
    /// `controls` can have the focus.
    pub fn new(controls: impl IntoIterator<Item = C>, cx: &mut App) -> Self {
        Self {
            focus: PanelFocus::new(controls, cx),
            collapsed: HashSet::new(),
            drag: None,
            nudge: None,
            typing: None,
        }
    }

    /// The control being dragged.
    pub fn dragged(&self) -> Option<C> {
        self.drag.as_ref().map(|drag| drag.control)
    }

    /// The control whose value is being typed.
    pub fn typed(&self) -> Option<C> {
        self.typing.as_ref().map(|typing| typing.control)
    }
}
