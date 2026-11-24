//! Right dock of the Crop tool.

use gpui::{Context, Entity, IntoElement, Render, Role, SharedString, Window, div, prelude::*};
use rohditor_engine::tools::{CropAspect, CropToolSession};

use crate::actions::{ApplyTool, CancelTool, FlipCrop, LockCrop, ResetCrop, SetCropAspect};
use crate::session::Session;
use crate::theme::{TAB_HEIGHT, theme};
use crate::ui::button::link;
use crate::ui::dropdown::dropdown;
use crate::ui::menu::MenuEntry;
use crate::ui::overlays::Overlays;
use crate::ui::test_id::TestId as _;
use crate::ui::toggle::toggle;

/// What the panel shows. Compared on every session change, so that the panel only redraws for
/// its own content.
#[derive(Clone, Copy, Default, PartialEq)]
struct Model {
    /// The size of the cropped photo.
    size: Option<(usize, usize)>,
    aspect: CropAspect,
    locked: bool,
    /// The ratio is flipped.
    flipped: bool,
    /// The photo is shown without its crop.
    ready: bool,
}

impl Model {
    fn of(session: &Session) -> Self {
        let crop = session.tool().crop();
        Self {
            size: crop.map(CropToolSession::output_dimensions),
            aspect: crop.map(CropToolSession::aspect).unwrap_or_default(),
            locked: crop.is_some_and(CropToolSession::locked),
            flipped: crop.is_some_and(CropToolSession::portrait),
            ready: session.crop_draft().is_some(),
        }
    }

    fn aspect_label(self) -> SharedString {
        match self.aspect {
            CropAspect::Free if self.locked => "Custom".into(),
            aspect => match aspect.label().split_once(':') {
                Some((width, height)) if self.flipped => format!("{height}:{width}").into(),
                _ => aspect.label().into(),
            },
        }
    }
}

pub struct CropPanel {
    model: Model,
    overlays: Entity<Overlays>,
}

impl CropPanel {
    pub fn new(
        session: Entity<Session>,
        overlays: Entity<Overlays>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&session, |panel, session, cx| {
            let model = Model::of(session.read(cx));
            if panel.model != model {
                panel.model = model;
                cx.notify();
            }
        })
        .detach();
        Self {
            model: Model::of(session.read(cx)),
            overlays,
        }
    }

    fn aspect(&self) -> impl IntoElement + use<> {
        let model = self.model;
        let overlays = self.overlays.clone();
        dropdown(
            "crop-aspect",
            "Aspect ratio",
            model.aspect_label(),
            overlays,
            move |_, _| {
                let entries = CropAspect::ALL.into_iter().map(|aspect| {
                    let current = model.aspect == aspect;
                    MenuEntry::new(aspect.label(), move |window, cx| {
                        window.dispatch_action(Box::new(SetCropAspect(aspect)), cx);
                    })
                    .checked(current && (aspect != CropAspect::Free || !model.locked))
                });
                entries.collect()
            },
        )
    }

    fn controls(&self) -> impl IntoElement + use<> {
        let theme = theme();
        let locked = self.model.locked;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_color(theme.muted).child("Aspect"))
                    .child(self.aspect()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(toggle("crop-lock", "Lock ratio", locked, |window, cx| {
                        window.dispatch_action(Box::new(LockCrop), cx);
                    }))
                    .child(link("crop-flip", "Flip", FlipCrop, locked)),
            )
    }
}

impl Render for CropPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = theme();
        let size = self
            .model
            .size
            .map(|(width, height)| format!("{width} × {height} px"));
        div()
            .id("crop-panel")
            .role(Role::Group)
            .aria_label("Crop")
            .test_id("crop-panel")
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.panel)
            .border_l_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .h(TAB_HEIGHT)
                    .px_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child("Crop")
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(link("crop-reset", "Reset", ResetCrop, true))
                            .child(link("cancel-tool", "Cancel", CancelTool, true))
                            .child(link("apply-tool", "Apply", ApplyTool, true)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_3()
                    .child(self.controls())
                    .child(div().text_color(theme.muted).children(size).when(
                        !self.model.ready,
                        |body| {
                            body.child(div().text_color(theme.faint).child("Preparing the photo…"))
                        },
                    )),
            )
    }
}
