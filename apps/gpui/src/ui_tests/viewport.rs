//! The photo area: overlays, clipping and how frames reach the GPU.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{App, Context, CursorStyle, Window};
use rohditor_engine::view::ImagePlacement;

use super::*;
use crate::viewport::Viewport;
use crate::viewport::overlay::{Overlay, pair};

/// Records what it is told, as image positions. Claims presses on the left half of the photo.
struct Probe(Rc<RefCell<Vec<(&'static str, f64)>>>);

impl Probe {
    fn record(&self, what: &'static str, position: Point<Pixels>, photo: ImagePlacement) {
        self.0
            .borrow_mut()
            .push((what, photo.to_image(pair(position)).0));
    }
}

impl Overlay for Probe {
    fn paint(&self, photo: ImagePlacement, window: &mut Window, _: &mut App) {
        let [x, y] = photo.to_screen((0.0, 0.0)).map(px);
        let corner = Bounds::new(point(x, y), size(px(20.0), px(20.0)));
        window.paint_quad(gpui::fill(corner, gpui::green()));
    }

    fn press(
        &mut self,
        event: &MouseDownEvent,
        photo: ImagePlacement,
        _: &mut Window,
        _: &mut Context<Viewport>,
    ) -> bool {
        self.record("press", event.position, photo);
        photo.to_image(pair(event.position)).0 < 0.5
    }

    fn drag(
        &mut self,
        event: &MouseMoveEvent,
        photo: ImagePlacement,
        _: &mut Window,
        _: &mut Context<Viewport>,
    ) {
        self.record("drag", event.position, photo);
    }

    fn release(&mut self, _: &mut Window, _: &mut Context<Viewport>) {
        self.0.borrow_mut().push(("release", 0.0));
    }

    fn cursor(&self, _: Point<Pixels>, _: ImagePlacement, _: &App) -> Option<CursorStyle> {
        Some(CursorStyle::Crosshair)
    }
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn overlays_draw_on_the_photo_and_claim_drags() {
    let Some((mut harness, ..)) = opened("overlays") else {
        return;
    };
    let log = Rc::new(RefCell::new(Vec::new()));
    let probe = Probe(Rc::clone(&log));
    let window = harness.window;
    harness
        .cx
        .update(|cx| {
            window.update(cx, |workspace, _, cx| {
                let viewport = workspace.viewport().clone();
                viewport.update(cx, |viewport, cx| {
                    viewport.push_overlay(Box::new(probe), cx);
                });
            })
        })
        .expect("window is open");
    let shot = harness.capture("overlay-probe");
    assert!(
        shot.pixels().any(|pixel| pixel.0 == [0, 255, 0, 255]
            || (pixel[1] > 100 && pixel[0] == 0 && pixel[2] == 0)),
        "the overlay is drawn"
    );

    let pan = |harness: &mut Harness| {
        harness.with_session(|session, _| session.document().map(|d| d.preview.view.pan()))
    };
    let center = harness.find("viewport").center();
    let left = center - point(px(50.0), px(0.0));
    harness.press(left, 1);
    // The develop panel: a claimed drag goes on outside the viewport.
    let outside = point(WINDOW.width - px(10.0), center.y);
    harness.drag_to(outside);
    harness.release(outside);
    harness.drag_to(left);
    assert_eq!(pan(&mut harness), Some([0.0, 0.0]), "the photo did not pan");
    let seen = log.take();
    let names: Vec<_> = seen.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, ["press", "drag", "release"]);
    assert!((0.3..0.5).contains(&seen[0].1), "{seen:?}");
    assert!(seen[1].1 > 1.0, "{seen:?}");

    let right = center + point(px(50.0), px(0.0));
    harness.press(right, 1);
    harness.drag_to(right + point(px(30.0), px(0.0)));
    harness.release(right);
    assert_eq!(
        pan(&mut harness),
        Some([30.0, 0.0]),
        "an unclaimed drag pans"
    );
    assert_eq!(log.take().len(), 1, "only the press was offered");
}

/// Whether each pixel of the photo area changed between two shots, and has a channel at either
/// end of the range in the first.
fn clipped_and_changed(
    area: Bounds<Pixels>,
    before: &image::RgbaImage,
    after: &image::RgbaImage,
) -> Vec<(bool, bool)> {
    let scale = before.width() as f32 / WINDOW.width.as_f32();
    let device = |value: Pixels| (value.as_f32() * scale) as u32;
    let mut pixels = Vec::new();
    // Not up to the border, which other elements may overlap.
    for y in device(area.top()) + 20..device(area.bottom()) - 20 {
        for x in device(area.left()) + 20..device(area.right()) - 20 {
            let (old, new) = (before.get_pixel(x, y), after.get_pixel(x, y));
            let clipped = old.0[..3].iter().any(|&level| level == 0 || level == 255);
            pixels.push((clipped, old != new));
        }
    }
    pixels
}

/// Toggle the overlay over a 1:1 frame and compare it with the pixels below.
fn check_clipping_at_source_scale(harness: &mut Harness, name: &str, source: PreviewSource) {
    harness.keys("ctrl-1");
    harness.wait_until("the source-resolution frame", |session| {
        session.document().is_some_and(|document| {
            document.preview.status.is_none() && document.preview.source == Some(source)
        })
    });
    let area = harness.find("viewport");
    let before = harness.capture(&format!("{name}-off"));
    harness.keys("j");
    let after = harness.capture(&format!("{name}-on"));
    let pixels = clipped_and_changed(area, &before, &after);
    let clipped = pixels.iter().filter(|(clipped, _)| *clipped).count();
    let wrong = pixels
        .iter()
        .filter(|(clipped, changed)| clipped != changed);
    let wrong = wrong.count();
    println!(
        "{name}: {clipped} of {} pixels clipped, {wrong} wrong",
        pixels.len()
    );
    assert!(clipped > 1000 && clipped < pixels.len());
    assert_eq!(wrong, 0, "exactly the clipped pixels are tinted");
    harness.keys("j");
    assert!(harness.capture(&format!("{name}-again")) == before);
}

fn overexpose(harness: &mut Harness) {
    let row = harness.find(EXPOSURE);
    let value = point(row.right() - px(20.0), row.center().y);
    harness.press(value, 1);
    harness.release(value);
    // The overlay's key is only a letter to an input.
    harness.keys("2 j backspace enter");
    assert_eq!(harness.exposure(), 2.0);
    assert!(!harness.with_session(|session, _| session.clipping_shown()));
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn clipping_tints_the_clipped_pixels_of_gpu_frames() {
    let Some((mut harness, _, second)) = opened("clipping") else {
        return;
    };
    overexpose(&mut harness);
    harness.wait_until("the edited preview", settled);
    check_clipping_at_source_scale(&mut harness, "clipping-gpu", PreviewSource::OneToOneGpu);

    // It follows edits, and stays on for the next photo.
    harness.keys("ctrl-0");
    harness.wait_until("the fitted frame", settled);
    harness.click("button:toggle-clipping");
    let shot = harness.capture("clipping-fit");
    harness.keys("ctrl-z");
    harness.wait_until("the undone edit", settled);
    let undone = harness.capture("clipping-undone");
    let red = |shot: &image::RgbaImage| {
        let tinted = |pixel: &&image::Rgba<u8>| pixel[0] > 180 && pixel[1] < 130 && pixel[2] < 130;
        shot.pixels().filter(tinted).count()
    };
    assert!(red(&shot) > red(&undone) + 1000, "fewer pixels are clipped");
    harness.with_session(|session, cx| session.open_file(second, cx));
    assert!(harness.with_session(|session, _| session.clipping_image().is_none()));
    harness.wait_until("the second photo", settled);
    assert!(harness.with_session(|session, _| session.clipping_image().is_some()));
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn clipping_tints_the_clipped_pixels_of_cpu_frames() {
    let Some((first, _)) = scratch_photos("clipping-cpu") else {
        return;
    };
    let mut harness = Harness::start(WINDOW, || None);
    harness.with_session(|session, cx| session.open_file(first, cx));
    let shown = |session: &Session| {
        session.document().is_some_and(|document| {
            document.preview.status.is_none()
                && document.edits.recipe().rendering.camera_tone.is_some()
                && document.preview.histogram_revision == Some(document.edits.revision())
        })
    };
    harness.wait_until("the developed preview", shown);
    overexpose(&mut harness);
    harness.wait_until("the edited preview", shown);
    assert!(
        !harness.with_session(|session, _| session.display().is_some_and(Display::is_zero_copy))
    );
    check_clipping_at_source_scale(&mut harness, "clipping-cpu", PreviewSource::OneToOneCpu);
}

/// Prints what showing a 24 MP CPU frame costs. Compared with the frames after it, the first
/// one also uploads.
#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn large_cpu_frame_cost() {
    let Some((mut harness, ..)) = opened("cpu-frame") else {
        return;
    };
    let (width, height) = (6000, 4000);
    for run in 0..4_u8 {
        let rgb: Vec<u8> = (0..width * height * 3)
            .map(|i| (i as u8).wrapping_add(run))
            .collect();
        let begun = Instant::now();
        harness.with_session(|session, cx| session.show_test_image(width, height, &rgb, cx));
        let converted = begun.elapsed();
        let mut captures = Vec::new();
        for _ in 0..3 {
            let begun = Instant::now();
            harness.draw();
            let drawn = begun.elapsed();
            let begun = Instant::now();
            harness
                .cx
                .capture_screenshot(harness.window.into())
                .expect("screenshot");
            captures.push((drawn, begun.elapsed()));
        }
        println!("convert {converted:?}, (draw, render) {captures:?}");
    }
}
