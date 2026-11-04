//! The Masks tool's layer on top of the viewport: the red mask tint, the handles of the selected
//! mask, the pins of the other masks, and the pointer events that drive them.
//!
//! Handle positions, hit tests and drag maths come from `MaskToolSession`; this module maps
//! them through the viewport's current zoom/pan (`image_rect`) and reports semantic events
//! ([`MaskOverlayOutput`]) for the app to forward to the session, like the crop overlay does.

use eframe::egui;
use rohditor_core::OutputGeometry;
use rohditor_edit::{EditRecipe, MaskLayer, MaskShape};
use rohditor_engine::tools::{HandleMarker, MaskEditMode, MaskHandle, MaskToolSession, MaskView};

use super::theme::colors;

const HANDLE_RADIUS: f32 = 5.0;
/// How close, in screen pixels, a click must be to another mask's pin to select it.
const PIN_HIT_RADIUS: f32 = 10.0;
/// Samples of an ellipse outline; enough that it looks smooth at any zoom.
const ELLIPSE_SEGMENTS: usize = 96;

pub(crate) struct MaskOverlayModel<'a> {
    pub session: &'a MaskToolSession,
    pub recipe: &'a EditRecipe,
    /// Orientation and crop of the displayed image over the developed frame.
    pub geometry: OutputGeometry,
    /// The selected mask's alpha, already tinted red. It covers the whole image.
    pub tint: Option<&'a egui::TextureHandle>,
}

#[derive(Debug, Default)]
pub(crate) struct MaskOverlayOutput {
    /// The pointer pressed on something the tool grabs, at a display position (`0..1` over the
    /// image). Always accompanied by `drag_to` for the current pointer position.
    pub press: Option<(f64, f64)>,
    pub drag_to: Option<(f64, f64)>,
    pub release: bool,
    /// A click on another mask's pin.
    pub select_layer: Option<u32>,
    /// A mask gesture owns the primary drag, so the viewport must not pan with it.
    pub captures_drag: bool,
    /// How the image was shown this frame; the positions above are only meaningful for it.
    pub view: Option<MaskView>,
}

fn mask_view(model: &MaskOverlayModel<'_>, image_rect: egui::Rect) -> MaskView {
    let (output_width, _) = model.geometry.output_dimensions();
    MaskView {
        geometry: model.geometry,
        screen_scale: f64::from(image_rect.width()) / output_width as f64,
    }
}

fn to_display(image_rect: egui::Rect, pointer: egui::Pos2) -> (f64, f64) {
    (
        f64::from((pointer.x - image_rect.left()) / image_rect.width()),
        f64::from((pointer.y - image_rect.top()) / image_rect.height()),
    )
}

fn to_screen(image_rect: egui::Rect, position: (f64, f64)) -> egui::Pos2 {
    egui::pos2(
        image_rect.left() + position.0 as f32 * image_rect.width(),
        image_rect.top() + position.1 as f32 * image_rect.height(),
    )
}

/// Whether a primary press at `pointer` would start a mask gesture, so the viewport knows not to
/// pan with it. Drawing a gradient grabs any press; otherwise only a handle of the selection.
fn press_grabs(model: &MaskOverlayModel<'_>, image_rect: egui::Rect, pointer: egui::Pos2) -> bool {
    match model.session.mode {
        MaskEditMode::Select => model
            .session
            .hit_test(
                model.recipe,
                &mask_view(model, image_rect),
                to_display(image_rect, pointer),
            )
            .is_some(),
        MaskEditMode::CreateLinear | MaskEditMode::CreateRadial | MaskEditMode::Paint { .. } => {
            true
        }
    }
}

/// Where the primary drag that just started was pressed, if the tool grabs that press.
fn grabbing_press(
    ui: &egui::Ui,
    response: &egui::Response,
    image_rect: egui::Rect,
    model: &MaskOverlayModel<'_>,
) -> Option<egui::Pos2> {
    if !response.drag_started_by(egui::PointerButton::Primary) {
        return None;
    }
    ui.input(|input| input.pointer.press_origin())
        .filter(|origin| press_grabs(model, image_rect, *origin))
}

/// Whether a mask gesture owns the primary drag (one is running, or one starts this frame), so
/// the viewport must not pan with it. `image_rect` is where the image is on screen.
pub(crate) fn captures_drag(
    ui: &egui::Ui,
    response: &egui::Response,
    image_rect: egui::Rect,
    model: &MaskOverlayModel<'_>,
) -> bool {
    model.session.is_dragging() || grabbing_press(ui, response, image_rect, model).is_some()
}

/// The pin of a mask in the viewport: where a click selects it.
fn layer_anchor(layer: &MaskLayer) -> Option<(f32, f32)> {
    match &layer.components.first()?.shape {
        MaskShape::Linear(line) => Some((
            (line.start.x + line.end.x) / 2.0,
            (line.start.y + line.end.y) / 2.0,
        )),
        MaskShape::Radial(radial) => Some((radial.center.x, radial.center.y)),
        MaskShape::Brush(brush) => brush
            .strokes
            .iter()
            .find_map(|stroke| stroke.points.first())
            .map(|point| (point.x, point.y)),
    }
}

/// Pins of the layers other than the selected one, at screen positions.
fn other_pins<'a>(
    model: &'a MaskOverlayModel<'_>,
    image_rect: egui::Rect,
) -> impl Iterator<Item = (&'a MaskLayer, egui::Pos2)> {
    model
        .recipe
        .local
        .layers
        .iter()
        .filter(|layer| model.session.selected_layer != Some(layer.id))
        .filter_map(move |layer| {
            let (x, y) = layer_anchor(layer)?;
            let display = model
                .geometry
                .developed_to_display((f64::from(x), f64::from(y)));
            Some((layer, to_screen(image_rect, display)))
        })
}

/// Handles pointer input and paints. `response` is the viewport's click-and-drag response.
pub(crate) fn interact_and_paint(
    context: &egui::Context,
    ui: &egui::Ui,
    response: &egui::Response,
    image_rect: egui::Rect,
    model: &MaskOverlayModel<'_>,
) -> MaskOverlayOutput {
    let mut output = MaskOverlayOutput::default();
    if let Some(tint) = model.tint {
        ui.painter().image(
            tint.id(),
            image_rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    }

    let view = mask_view(model, image_rect);
    output.view = Some(view);
    let primary = egui::PointerButton::Primary;
    let pointer = response.interact_pointer_pos();

    output.press = grabbing_press(ui, response, image_rect, model)
        .map(|origin| to_display(image_rect, origin));
    output.captures_drag = captures_drag(ui, response, image_rect, model);
    if output.captures_drag
        && response.dragged_by(primary)
        && let Some(pointer) = pointer
    {
        output.drag_to = Some(to_display(image_rect, pointer));
    }
    if response.drag_stopped_by(primary) && model.session.is_dragging() {
        output.release = true;
    }

    let hover = response.hover_pos();
    let hovered_pin = hover.and_then(|hover| {
        other_pins(model, image_rect)
            .find(|(_, pin)| pin.distance(hover) <= PIN_HIT_RADIUS)
            .map(|(layer, _)| layer)
    });
    if response.clicked_by(primary)
        && let Some(layer) = hovered_pin
    {
        output.select_layer = Some(layer.id);
    }
    if let Some(layer) = hovered_pin {
        context.set_cursor_icon(egui::CursorIcon::PointingHand);
        let _ = response.clone().on_hover_text_at_pointer(&layer.name);
    } else if let Some(hover) = hover.filter(|hover| image_rect.contains(*hover)) {
        if model.session.mode != MaskEditMode::Select {
            context.set_cursor_icon(egui::CursorIcon::Crosshair);
        } else if let Some(handle) =
            model
                .session
                .hit_test(model.recipe, &view, to_display(image_rect, hover))
        {
            context.set_cursor_icon(cursor_for(handle));
        }
    }

    paint_pins(ui, model, image_rect, hovered_pin.map(|layer| layer.id));
    paint_selection(ui, model, &view, image_rect);
    output
}

const fn cursor_for(handle: MaskHandle) -> egui::CursorIcon {
    match handle {
        MaskHandle::LinearCenter | MaskHandle::RadialCenter => egui::CursorIcon::Move,
        MaskHandle::LinearRotate | MaskHandle::RadialRotate => egui::CursorIcon::Alias,
        MaskHandle::LinearStart
        | MaskHandle::LinearEnd
        | MaskHandle::RadialRadiusX
        | MaskHandle::RadialRadiusY
        | MaskHandle::RadialFeather => egui::CursorIcon::Grab,
    }
}

fn paint_pins(
    ui: &egui::Ui,
    model: &MaskOverlayModel<'_>,
    image_rect: egui::Rect,
    hovered: Option<u32>,
) {
    let painter = ui.painter();
    for (layer, position) in other_pins(model, image_rect) {
        let fill = if hovered == Some(layer.id) {
            colors::ACCENT_HOVER
        } else {
            egui::Color32::from_black_alpha(160)
        };
        let outline = if layer.enabled {
            egui::Color32::WHITE
        } else {
            egui::Color32::from_white_alpha(110)
        };
        painter.circle_filled(position, 4.0, fill);
        painter.circle_stroke(position, 4.0, egui::Stroke::new(1.5_f32, outline));
    }
}

/// A line that stays readable on any image: a dark halo under a light stroke.
fn halo_line(painter: &egui::Painter, points: &[egui::Pos2], width: f32) {
    painter.add(egui::Shape::line(
        points.to_vec(),
        egui::Stroke::new(width + 2.0, egui::Color32::from_black_alpha(150)),
    ));
    painter.add(egui::Shape::line(
        points.to_vec(),
        egui::Stroke::new(width, egui::Color32::WHITE),
    ));
}

fn paint_handle(painter: &egui::Painter, position: egui::Pos2, radius: f32) {
    painter.circle_filled(position, radius, colors::ACCENT_HOVER);
    painter.circle_stroke(
        position,
        radius,
        egui::Stroke::new(1.0_f32, egui::Color32::BLACK),
    );
}

fn paint_selection(
    ui: &egui::Ui,
    model: &MaskOverlayModel<'_>,
    view: &MaskView,
    image_rect: egui::Rect,
) {
    let markers = model.session.handle_markers(model.recipe, view);
    if markers.is_empty() {
        return;
    }
    let marker = |handle: MaskHandle| -> Option<egui::Pos2> {
        markers
            .iter()
            .find(|HandleMarker { handle: found, .. }| *found == handle)
            .map(|found| to_screen(image_rect, found.position))
    };
    let painter = ui.painter();
    let linear = (
        marker(MaskHandle::LinearStart),
        marker(MaskHandle::LinearEnd),
        marker(MaskHandle::LinearCenter),
    );
    if let (Some(start), Some(end), Some(center)) = linear {
        paint_linear(painter, ui.clip_rect(), start, end, center);
    }
    let radial = (
        marker(MaskHandle::RadialCenter),
        marker(MaskHandle::RadialRadiusX),
        marker(MaskHandle::RadialRadiusY),
        marker(MaskHandle::RadialFeather),
    );
    if let (Some(center), Some(x), Some(y), Some(feather)) = radial {
        // The feather handle sits on the inner ellipse's 45 degree diagonal, which tells how
        // far the inner ellipse is scaled in: the marker is at `(1 - feather)` of the outline's
        // diagonal.
        let inner = inner_scale(center, x, y, feather);
        paint_radial(painter, [center, x, y, feather], inner);
    }
}

/// Linear gradient: a line each at the start, centre and end across the image, the axis between
/// them, a pin at the centre and a ring on the axis that stands for rotating it.
fn paint_linear(
    painter: &egui::Painter,
    bounds: egui::Rect,
    start: egui::Pos2,
    end: egui::Pos2,
    center: egui::Pos2,
) {
    let axis = end - start;
    let length = axis.length();
    if length <= f32::EPSILON {
        return;
    }
    let unit = axis / length;
    let across = egui::vec2(-unit.y, unit.x) * bounds.size().length();
    for (anchor, width) in [(start, 1.0), (center, 1.5), (end, 1.0)] {
        halo_line(painter, &[anchor - across, anchor + across], width);
    }
    halo_line(painter, &[start, end], 1.0);
    let ring = start.lerp(center, 0.5);
    painter.circle_stroke(
        ring,
        5.0,
        egui::Stroke::new(3.0_f32, egui::Color32::from_black_alpha(150)),
    );
    painter.circle_stroke(ring, 5.0, egui::Stroke::new(1.5_f32, egui::Color32::WHITE));
    paint_handle(painter, start, HANDLE_RADIUS - 1.0);
    paint_handle(painter, end, HANDLE_RADIUS - 1.0);
    paint_handle(painter, center, HANDLE_RADIUS + 1.0);
}

/// Scale of the inner (feather) ellipse relative to the outline, from the feather handle.
fn inner_scale(center: egui::Pos2, x: egui::Pos2, y: egui::Pos2, feather: egui::Pos2) -> f32 {
    let diagonal = (x - center) + (y - center);
    let length_squared = diagonal.length_sq();
    if length_squared <= f32::EPSILON {
        return 0.0;
    }
    // The marker is `scale / sqrt(2)` along the diagonal.
    ((feather - center).dot(diagonal) / length_squared * std::f32::consts::SQRT_2).clamp(0.0, 1.0)
}

/// Radial gradient: the outer ellipse, the inner feather ellipse, the radius handles, the
/// feather handle and a pin at the centre.
fn paint_radial(painter: &egui::Painter, [center, x, y, feather]: [egui::Pos2; 4], inner: f32) {
    let axis_x = x - center;
    let axis_y = y - center;
    let ellipse = |scale: f32| -> Vec<egui::Pos2> {
        (0..=ELLIPSE_SEGMENTS)
            .map(|step| {
                let angle = step as f32 / ELLIPSE_SEGMENTS as f32 * std::f32::consts::TAU;
                center + (axis_x * angle.cos() + axis_y * angle.sin()) * scale
            })
            .collect()
    };
    halo_line(painter, &ellipse(1.0), 1.5);
    if inner > 0.0 {
        halo_line(painter, &ellipse(inner), 1.0);
    }
    paint_handle(painter, x, HANDLE_RADIUS - 1.0);
    paint_handle(painter, y, HANDLE_RADIUS - 1.0);
    paint_handle(painter, feather, HANDLE_RADIUS - 1.0);
    paint_handle(painter, center, HANDLE_RADIUS + 1.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inner_scale_follows_the_feather_marker_along_the_diagonal() {
        let center = egui::pos2(100.0, 100.0);
        let x = egui::pos2(160.0, 100.0);
        let y = egui::pos2(100.0, 140.0);
        // A marker at scale `s` is `s / sqrt(2)` of the way along the summed axes.
        for scale in [0.0_f32, 0.25, 0.5, 1.0] {
            let marker =
                center + ((x - center) + (y - center)) * (std::f32::consts::FRAC_1_SQRT_2 * scale);
            assert!(
                (inner_scale(center, x, y, marker) - scale).abs() < 1e-5,
                "{scale}"
            );
        }
    }

    #[test]
    fn screen_and_display_positions_round_trip() {
        let rect = egui::Rect::from_min_size(egui::pos2(40.0, 20.0), egui::vec2(300.0, 200.0));
        let display = to_display(rect, egui::pos2(190.0, 70.0));
        assert_eq!(display, (0.5, 0.25));
        assert_eq!(to_screen(rect, display), egui::pos2(190.0, 70.0));
    }
}
