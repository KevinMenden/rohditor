//! Right dock of the Crop tool.

use gpui::{Context, Entity, IntoElement, Render, Role, Window, div, prelude::*};

use crate::actions::{ApplyTool, CancelTool};
use crate::session::Session;
use crate::theme::{TAB_HEIGHT, theme};
use crate::ui::button::link;
use crate::ui::test_id::TestId as _;

/// What the panel shows. Compared on every session change, so that the panel only redraws for
/// its own content.
#[derive(Clone, Copy, Default, PartialEq)]
struct Model {
    /// The size of the cropped photo.
    size: Option<(usize, usize)>,
    /// The photo is shown without its crop.
    ready: bool,
}

impl Model {
    fn of(session: &Session) -> Self {
        Self {
            size: session.tool().crop().map(|crop| crop.output_dimensions()),
            ready: session
                .document()
                .is_some_and(|document| document.preview.full_frame_shown()),
        }
    }
}

pub struct CropPanel {
    model: Model,
}

impl CropPanel {
    pub fn new(session: Entity<Session>, cx: &mut Context<Self>) -> Self {
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
        }
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
                            .child(link("cancel-tool", "Cancel", CancelTool, true))
                            .child(link("apply-tool", "Apply", ApplyTool, true)),
                    ),
            )
            .child(
                div()
                    .p_3()
                    .text_color(theme.muted)
                    .children(size)
                    .when(!self.model.ready, |body| {
                        body.child(div().text_color(theme.faint).child("Preparing the photo…"))
                    }),
            )
    }
}
