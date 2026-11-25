//! What the panel shows.

use gpui::SharedString;
use rohditor_edit::{MaskComponent, MaskLayer, MaskMode, MaskShape, local_detail_is_full};
use rohditor_engine::tools::{BrushSettings, LocalSlider, MaskEditMode, MaskLimit};

use crate::session::Session;

/// Compared on every session change, so that the panel only redraws for its own content. Without
/// the shapes, as a brush can have a lot of points.
#[derive(Clone, Default, PartialEq)]
pub(super) struct Model {
    /// A photo is open and can be masked.
    pub ready: bool,
    pub masks: Vec<Mask>,
    pub selected: Option<Selected>,
    pub mode: MaskEditMode,
    /// While painting.
    pub brush: Option<Brush>,
    /// The selected mask is tinted.
    pub overlay: bool,
    /// The limit that stopped the last edit.
    pub limit: Option<MaskLimit>,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) struct Brush {
    pub settings: BrushSettings,
    pub erase: bool,
}

#[derive(Clone, PartialEq)]
pub(super) struct Mask {
    pub id: u32,
    pub name: SharedString,
    pub enabled: bool,
}

/// The selected mask.
#[derive(Clone, PartialEq)]
pub(super) struct Selected {
    pub mask: u32,
    pub component: Option<usize>,
    pub components: Vec<Component>,
    /// The selected component, if it is a range.
    pub range: Option<MaskShape>,
    /// In the order of [`LocalSlider::ALL`].
    values: [f32; LocalSlider::ALL.len()],
    /// It cannot get detail settings of its own.
    pub detail_is_full: bool,
}

#[derive(Clone, PartialEq)]
pub(super) struct Component {
    pub label: &'static str,
    pub mode: MaskMode,
    pub inverted: bool,
    /// More strokes can be painted into it.
    pub brush: bool,
}

impl Component {
    fn of(component: &MaskComponent) -> Self {
        Self {
            label: component.shape.label(),
            mode: component.mode,
            inverted: component.invert,
            brush: matches!(component.shape, MaskShape::Brush(_)),
        }
    }
}

impl Selected {
    fn of(layer: &MaskLayer, layers: &[MaskLayer], component: Option<usize>) -> Self {
        let chosen = component.and_then(|index| layer.components.get(index));
        Self {
            mask: layer.id,
            component: component.filter(|_| chosen.is_some()),
            range: chosen
                .map(|component| &component.shape)
                .filter(|shape| shape.is_range())
                .cloned(),
            components: layer.components.iter().map(Component::of).collect(),
            values: LocalSlider::ALL.map(|slider| slider.get(layer)),
            detail_is_full: local_detail_is_full(layers, layer),
        }
    }

    pub fn value(&self, slider: LocalSlider) -> f32 {
        let listed = std::iter::zip(LocalSlider::ALL, self.values);
        listed
            .into_iter()
            .find(|(one, _)| *one == slider)
            .map_or(slider.range().neutral, |(_, value)| value)
    }
}

impl Model {
    pub fn of(session: &Session) -> Self {
        let Some(masks) = session.masks() else {
            return Self::default();
        };
        let Some(document) = session.document().filter(|open| open.frame.is_some()) else {
            return Self::default();
        };
        let layers = &document.edits.recipe().local.layers;
        let selected = layers
            .iter()
            .find(|layer| Some(layer.id) == masks.selected_layer);
        Self {
            ready: true,
            masks: layers
                .iter()
                .map(|layer| Mask {
                    id: layer.id,
                    name: layer.name.clone().into(),
                    enabled: layer.enabled,
                })
                .collect(),
            selected: selected.map(|layer| Selected::of(layer, layers, masks.selected_component)),
            mode: masks.mode,
            brush: match masks.mode {
                MaskEditMode::Paint { erase } => Some(Brush {
                    settings: masks.brush,
                    erase,
                }),
                _ => None,
            },
            overlay: masks.show_overlay || masks.is_painting(),
            limit: masks.limit_reached(),
        }
    }

    pub fn selected_mask(&self) -> Option<u32> {
        self.selected.as_ref().map(|selected| selected.mask)
    }

    /// The selected mask and component.
    pub fn selection(&self) -> Option<(u32, Option<usize>)> {
        let selected = self.selected.as_ref()?;
        Some((selected.mask, selected.component))
    }

    /// The selected component, if it is a range.
    pub fn range(&self) -> Option<&MaskShape> {
        self.selected.as_ref()?.range.as_ref()
    }

    pub fn mask(&self, id: u32) -> Option<&Mask> {
        self.masks.iter().find(|mask| mask.id == id)
    }
}
