//! The colour grading wheels. A drag anywhere on a wheel moves its point relative to where it was,
//! Shift drags finer, and a double click resets it. With the focus, the horizontal arrows and Page
//! Up and Page Down move it.

use std::sync::Arc;

use gpui::{
    Context, Corners, CursorStyle, IntoElement, Pixels, RenderImage, Role, canvas, div, prelude::*,
    px, relative,
};
use rohditor_edit::{EditRecipe, GradingTint, GradingTones};

use super::DevelopPanel;
use super::control::Control;
use crate::panels::controls::{
    ControlPanel as _, Decrease, DecreaseMore, FocusControls as _, Gestures as _, Increase,
    IncreaseMore, ResetValue, SLIDER_CONTEXT, press_area,
};
use crate::session::EditPhase;
use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::test_id::TestId as _;

const DIAMETER: Pixels = px(80.0);
const POINT: Pixels = px(9.0);
/// Pixels of the wheel's picture, for sharp edges on dense screens.
const PICTURE: u32 = 240;
/// What an arrow key moves.
const STEP: f32 = 0.02;

/// The tints a wheel offers, one per position.
pub(super) fn wheel_picture() -> Arc<RenderImage> {
    let picture = image::RgbaImage::from_fn(PICTURE, PICTURE, |column, row| {
        let at = |pixel: u32| (pixel as f32 + 0.5) / PICTURE as f32 * 2.0 - 1.0;
        let tint = GradingTint::new(at(column), -at(row));
        let [red, green, blue] = tint
            .to_rgb()
            .map(|value| ((0.33 + 0.3 * value) * 255.0) as u8);
        // GPUI images are BGRA.
        image::Rgba([blue, green, red, 255])
    });
    Arc::new(RenderImage::new(vec![image::Frame::new(picture)]))
}

impl DevelopPanel {
    pub(super) fn wheels(
        &self,
        recipe: &EditRecipe,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let wheels = GradingTones::ALL.map(|tones| {
            self.wheel(tones, tones.get(&recipe.color.grading), cx)
                .into_any_element()
        });
        div().flex().justify_between().children(wheels)
    }

    fn wheel(
        &self,
        tones: GradingTones,
        tint: GradingTint,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = theme();
        let control = Control::Wheel(tones);
        let strength = (tint.strength() * 100.0).round();
        let readout = if strength == 0.0 {
            "0".to_owned()
        } else {
            format!("{:.0}° · {strength:.0}%", tint.hue_degrees())
        };
        let active = self.controls.dragged() == Some(control);
        let disc = div()
            .test_id(format!("wheel:{tones:?}"))
            .relative()
            .size(DIAMETER)
            .cursor(CursorStyle::Crosshair)
            .child(disc(Arc::clone(&self.wheel_picture)))
            .child(
                div()
                    .test_id(format!("tint:{tones:?}"))
                    .absolute()
                    .left(relative(0.5 + tint.x / 2.0))
                    .top(relative(0.5 - tint.y / 2.0))
                    .ml(-POINT / 2.0)
                    .mt(-POINT / 2.0)
                    .size(POINT)
                    .rounded_full()
                    .border_1()
                    .border_color(theme.editor)
                    .bg(if active { theme.accent } else { theme.text }),
            )
            .child(press_area(cx.entity(), control));
        div()
            .id(tones.label())
            .role(Role::Slider)
            .aria_label(format!("{} tint", tones.label()))
            .aria_value(readout.clone())
            .key_context(SLIDER_CONTEXT)
            .track_focus(self.controls.focus.control(control))
            .relative()
            .child(self.revealer(control, cx))
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .p_1()
            .rounded(RADIUS)
            .border_1()
            .border_color(gpui::transparent_black())
            .focus(move |style| style.border_color(theme.accent))
            .on_action(cx.listener(move |this, _: &Decrease, _, cx| {
                this.nudge_wheel(tones, -1.0, 0.0, cx);
            }))
            .on_action(cx.listener(move |this, _: &Increase, _, cx| {
                this.nudge_wheel(tones, 1.0, 0.0, cx);
            }))
            .on_action(cx.listener(move |this, _: &DecreaseMore, _, cx| {
                this.nudge_wheel(tones, 0.0, -1.0, cx);
            }))
            .on_action(cx.listener(move |this, _: &IncreaseMore, _, cx| {
                this.nudge_wheel(tones, 0.0, 1.0, cx);
            }))
            .on_action(cx.listener(move |this, _: &ResetValue, _, cx| this.reset(control, cx)))
            .text_size(SMALL_FONT_SIZE)
            .child(div().text_color(theme.muted).child(tones.label()))
            .child(disc)
            .child(
                div()
                    .text_color(if strength == 0.0 {
                        theme.faint
                    } else {
                        theme.text
                    })
                    .child(readout),
            )
    }

    fn nudge_wheel(&mut self, tones: GradingTones, x: f32, y: f32, cx: &mut Context<Self>) {
        self.hold_nudge(Control::Wheel(tones), cx);
        self.move_wheel(tones, x * STEP, y * STEP, cx);
    }

    pub(super) fn set_wheel(
        &self,
        tones: GradingTones,
        tint: GradingTint,
        phase: EditPhase,
        cx: &mut Context<Self>,
    ) {
        self.session.update(cx, |session, cx| {
            session.set_grading(tones, tint, phase, cx);
        });
    }

    /// Move the point by a part of the radius.
    pub(super) fn move_wheel(&self, tones: GradingTones, x: f32, y: f32, cx: &mut Context<Self>) {
        let session = self.session.read(cx);
        let Some(document) = session.document() else {
            return;
        };
        let tint = tones.get(&document.edits.recipe().color.grading);
        let moved = GradingTint::new(tint.x + x, tint.y + y);
        self.set_wheel(tones, moved, EditPhase::Dragging, cx);
    }
}

fn disc(picture: Arc<RenderImage>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let corners = Corners::all(bounds.size.width / 2.0);
            if let Err(error) =
                window.paint_image(bounds, bounds, corners, Arc::clone(&picture), 0, false)
            {
                log::error!("could not paint the colour wheel: {error:#}");
            }
        },
    )
    .absolute()
    .size_full()
}
