//! Changing to the Crop tool and back, and dragging its rectangle.

use rohditor_edit::NormalizedCropRect;
use rohditor_engine::tools::{CropAspect, Picker, ToolKind};

use super::*;

fn tool(harness: &mut Harness) -> ToolKind {
    harness.with_session(|session, _| session.tool().kind())
}

fn crop(harness: &mut Harness) -> Option<NormalizedCropRect> {
    harness.with_session(|session, _| {
        let document = session.document().expect("a photo is open");
        document.edits.recipe().geometry.crop
    })
}

fn draft_square(harness: &mut Harness) {
    harness.with_session(|session, cx| {
        session.edit_crop(|crop| crop.set_aspect(CropAspect::Square), cx)
    });
}

fn enter(harness: &mut Harness) {
    harness.keys("c");
    harness.wait_until("the photo without its crop", |session| {
        session.tool().crop().is_some()
            && session
                .document()
                .is_some_and(|document| document.preview.full_frame_shown())
    });
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn a_crop_is_one_undo_step_and_shows_the_whole_photo_while_open() {
    let Some((mut harness, ..)) = opened("crop") else {
        return;
    };
    let full = shown(&mut harness);
    enter(&mut harness);
    harness.find("crop-panel");
    harness.capture("crop");
    assert!(
        harness.with_session(|session, _| session.display().is_some_and(Display::is_zero_copy))
    );
    draft_square(&mut harness);
    harness.keys("enter");
    assert_eq!(tool(&mut harness), ToolKind::Edit);
    assert!(crop(&mut harness).is_some());
    harness.wait_until("the cropped preview", settled);
    let cropped = shown(&mut harness);
    assert!(cropped.0.abs_diff(cropped.1) <= 1, "{cropped:?}");

    enter(&mut harness);
    assert_eq!(shown(&mut harness), full);
    // Anything that renders meanwhile keeps the whole photo.
    harness.with_session(|session, cx| {
        session.set_highlight_method(rohditor_edit::HighlightMethod::Opposed, cx);
    });
    harness.wait_until("the edited preview", |session| {
        session
            .document()
            .is_some_and(|d| d.preview.status.is_none())
    });
    assert_eq!(shown(&mut harness), full);
    harness.keys("ctrl-z");
    harness.keys("escape");
    harness.wait_until("the cropped preview again", settled);
    assert_eq!(shown(&mut harness), cropped);
    harness.keys("ctrl-z");
    assert_eq!(crop(&mut harness), None);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn a_draft_is_undone_cancelled_or_applied() {
    let Some((mut harness, first, _)) = opened("crop-draft") else {
        return;
    };
    let modified = |harness: &mut Harness| harness.with_session(|s, _| s.tool().is_modified());
    harness.with_session(|session, cx| {
        session.set_parameter(
            rohditor_edit::Parameter::Exposure,
            1.0,
            crate::session::EditPhase::Finished,
            cx,
        );
    });
    enter(&mut harness);
    draft_square(&mut harness);
    harness.keys("ctrl-z");
    assert_eq!(harness.exposure(), 1.0, "only the draft is gone");
    assert!(!modified(&mut harness));
    assert_eq!(tool(&mut harness), ToolKind::Crop);

    draft_square(&mut harness);
    harness.keys("escape");
    assert_eq!(
        (tool(&mut harness), crop(&mut harness)),
        (ToolKind::Edit, None)
    );

    // A picker changes the tool, which applies.
    enter(&mut harness);
    draft_square(&mut harness);
    harness.keys("w");
    assert!(crop(&mut harness).is_some());
    let picker = harness.with_session(|session, _| session.picker());
    assert_eq!(picker, Some(Picker::WhiteBalance));
    harness.keys("escape ctrl-z");
    assert_eq!(crop(&mut harness), None);

    // Enter presses what has the focus.
    enter(&mut harness);
    draft_square(&mut harness);
    harness.keys("tab enter");
    assert_eq!(
        tool(&mut harness),
        ToolKind::Crop,
        "the first button was pressed"
    );
    harness.keys("escape");
    assert_eq!(
        tool(&mut harness),
        ToolKind::Crop,
        "only the palette closed"
    );
    harness.click("button:apply-tool");
    assert!(crop(&mut harness).is_some());

    // So does closing the photo.
    enter(&mut harness);
    draft_square(&mut harness);
    harness.with_session(|session, cx| {
        session.undo(cx);
        session.edit_crop(|crop| crop.set_aspect(CropAspect::SixteenNine), cx);
        session.close_document(cx);
    });
    assert_eq!(tool(&mut harness), ToolKind::Edit);
    let drafted = harness.with_session(|session, cx| {
        session.open_file(first, cx);
        session
            .document()
            .and_then(|d| d.edits.recipe().geometry.crop)
    });
    let drafted = drafted.expect("the crop was saved");
    assert!(drafted.bottom - drafted.top < 0.999);
}

#[track_caller]
fn assert_draft(harness: &mut Harness, expected: [f64; 4]) {
    let draft = harness.with_session(|session, _| session.crop_draft().expect("drafting").draft());
    let edges = [draft.left, draft.top, draft.right, draft.bottom];
    let close = std::iter::zip(edges, expected).all(|(a, b)| (a - b).abs() < 1.0e-3);
    assert!(close, "{edges:?} is not {expected:?}");
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn the_rectangle_is_resized_and_moved_by_dragging() {
    let Some((mut harness, ..)) = opened("crop-drag") else {
        return;
    };
    let pan = |harness: &mut Harness| {
        harness.with_session(|session, _| session.document().map(|d| d.preview.view.pan()))
    };
    enter(&mut harness);
    // A handle keeps its distance to the pointer.
    drag(&mut harness, (0.0, 0.0), (0.2, 0.3), px(6.0));
    assert_draft(&mut harness, [0.2, 0.3, 1.0, 1.0]);
    drag(&mut harness, (1.0, 0.65), (0.7, 0.1), px(0.0));
    assert_draft(&mut harness, [0.2, 0.3, 0.7, 1.0]);
    drag(&mut harness, (0.45, 1.0), (0.1, 0.8), px(-4.0));
    assert_draft(&mut harness, [0.2, 0.3, 0.7, 0.8]);
    drag(&mut harness, (0.3, 0.4), (0.4, 0.5), px(0.0));
    assert_draft(&mut harness, [0.3, 0.4, 0.8, 0.9]);
    let shot = harness.capture("crop-dragged");
    // It stops at the edge, although the pointer goes on over the panels.
    drag(&mut harness, (0.4, 0.5), (3.0, 0.5), px(0.0));
    assert_draft(&mut harness, [0.5, 0.4, 1.0, 0.9]);
    assert!(harness.capture("crop-moved") != shot, "it is drawn");
    assert_eq!(pan(&mut harness), Some([0.0, 0.0]));

    // Leaving the tool ends a drag.
    let photo = photo(&mut harness);
    let inside = photo.origin + point(photo.size.width * 0.6, photo.size.height * 0.5);
    harness.press(inside, 1);
    harness.keys("enter");
    harness.drag_to(inside - point(px(50.0), px(0.0)));
    harness.release(inside);
    let committed = crop(&mut harness).expect("the crop was applied");
    assert!((committed.left - 0.5).abs() < 1.0e-3, "{committed:?}");
    harness.wait_until("the cropped preview", settled);
    harness.keys("ctrl-z");
    assert_eq!(crop(&mut harness), None, "all drags are one undo step");
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn dragging_beside_the_rectangle_pans() {
    let Some((mut harness, ..)) = opened("crop-pan") else {
        return;
    };
    let pan = |harness: &mut Harness| {
        harness.with_session(|session, _| session.document().map(|d| d.preview.view.pan()))
    };
    enter(&mut harness);
    // A full-frame rectangle cannot be moved.
    let center = harness.find("viewport").center();
    harness.press(center, 1);
    harness.drag_to(center + point(px(30.0), px(0.0)));
    harness.release(center);
    assert_eq!(pan(&mut harness), Some([30.0, 0.0]));
    harness.press(center, 2);
    harness.release(center);
    assert_eq!(pan(&mut harness), Some([0.0, 0.0]));

    drag(&mut harness, (0.0, 0.0), (0.5, 0.5), px(0.0));
    drag(&mut harness, (0.2, 0.2), (0.3, 0.2), px(0.0));
    assert_draft(&mut harness, [0.5, 0.5, 1.0, 1.0]);
    assert_ne!(pan(&mut harness), Some([0.0, 0.0]));
}

/// The size of the photo that the panel promises.
fn promised(harness: &mut Harness) -> (u32, u32) {
    let (width, height) = harness.with_session(|session, _| {
        let crop = session.tool().crop().expect("drafting");
        crop.output_dimensions()
    });
    (width as u32, height as u32)
}

fn ratio((width, height): (u32, u32)) -> f64 {
    f64::from(width) / f64::from(height)
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn ratios_are_chosen_locked_flipped_and_reset() {
    let Some((mut harness, ..)) = opened("crop-ratio") else {
        return;
    };
    enter(&mut harness);
    let full = promised(&mut harness);
    harness.click("button:crop-flip");
    assert_eq!(promised(&mut harness), full, "there is no ratio to flip");
    harness.click("dropdown:crop-aspect");
    harness.capture("crop-aspects");
    harness.click("menu:4:5");
    assert!((ratio(promised(&mut harness)) - 0.8).abs() < 1.0e-3);
    harness.click("button:crop-flip");
    assert!((ratio(promised(&mut harness)) - 1.25).abs() < 1.0e-3);
    let photo = photo(&mut harness);
    let draft = harness.with_session(|session, _| session.crop_draft().expect("drafting").draft());
    let corner = photo.origin
        + point(
            photo.size.width * draft.right as f32,
            photo.size.height * draft.bottom as f32,
        );
    harness.press(corner, 1);
    harness.drag_to(corner - point(px(200.0), px(50.0)));
    harness.release(corner);
    assert!(promised(&mut harness) < full);
    assert!((ratio(promised(&mut harness)) - 1.25).abs() < 1.0e-3);
    harness.capture("crop-ratio");

    let at_ratio = promised(&mut harness);
    harness.click("toggle:crop-lock");
    assert_eq!(promised(&mut harness), at_ratio, "unlocking keeps it");
    let locked = |harness: &mut Harness| {
        harness.with_session(|session, _| session.tool().crop().is_some_and(|c| c.locked()))
    };
    assert!(!locked(&mut harness));
    harness.click("toggle:crop-lock");
    assert!(locked(&mut harness));
    harness.click("button:crop-reset");
    assert_eq!(
        (promised(&mut harness), locked(&mut harness)),
        (full, false)
    );
    harness.click("button:cancel-tool");
    assert_eq!(
        (tool(&mut harness), crop(&mut harness)),
        (ToolKind::Edit, None)
    );
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn the_crop_tool_is_used_by_keyboard_alone() {
    let Some((mut harness, ..)) = opened("crop-keys") else {
        return;
    };
    enter(&mut harness);
    harness.keys("ctrl-shift-p");
    for key in "a s p e c t space 1 6 : 9".split(' ') {
        harness.keys(key);
    }
    harness.keys("enter");
    assert!((ratio(promised(&mut harness)) - 16.0 / 9.0).abs() < 1.0e-3);
    harness.keys("x");
    assert!((ratio(promised(&mut harness)) - 9.0 / 16.0).abs() < 1.0e-3);
    harness.keys("enter");
    assert!(crop(&mut harness).is_some());
    // Without the tool its commands do nothing.
    harness.keys("x");
    assert_eq!(tool(&mut harness), ToolKind::Edit);
    harness.keys("ctrl-z");
    assert_eq!(crop(&mut harness), None);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn the_exported_photo_has_the_promised_size() {
    let Some((mut harness, first, _)) = opened("crop-export") else {
        return;
    };
    enter(&mut harness);
    harness.with_session(|session, cx| {
        session.edit_crop(|crop| crop.set_aspect(CropAspect::FourFive), cx);
    });
    drag(&mut harness, (0.5, 0.5), (0.47, 0.5), px(0.0));
    let size = promised(&mut harness);
    // Exporting applies the crop.
    harness.keys("ctrl-e");
    assert_eq!(tool(&mut harness), ToolKind::Edit);
    assert!(crop(&mut harness).is_some());
    let exported = first.with_extension("jpg");
    let destination = exported.clone();
    harness.with_session(|session, _| {
        let id = session.document().expect("a photo is open").id;
        session.export_to(id, destination);
    });
    harness.wait_until("the export", |session| {
        session
            .document()
            .is_some_and(|document| document.export_status.is_none())
    });
    let written = image::image_dimensions(exported).expect("the photo was exported");
    assert_eq!(written, size);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn cropping_works_without_a_gpu() {
    let Some((first, _)) = scratch_photos("crop-cpu") else {
        return;
    };
    let mut harness = Harness::start(WINDOW, || None);
    harness.with_session(|session, cx| session.open_file(first, cx));
    let rendered = |session: &Session| {
        session.document().is_some_and(|document| {
            document.preview.status.is_none()
                && document.edits.recipe().rendering.camera_tone.is_some()
                && document.preview.histogram_revision == Some(document.edits.revision())
        })
    };
    harness.wait_until("the developed preview", rendered);
    let full = shown(&mut harness);
    enter(&mut harness);
    harness.with_session(|session, cx| {
        session.edit_crop(|crop| crop.set_aspect(CropAspect::Square), cx);
    });
    harness.keys("enter");
    harness.wait_until("the cropped preview", rendered);
    let cropped = shown(&mut harness);
    assert!(cropped.0.abs_diff(cropped.1) <= 1, "{cropped:?}");
    enter(&mut harness);
    assert_eq!(shown(&mut harness), full);
    harness.keys("escape");
    harness.wait_until("the cropped preview again", |session| {
        rendered(session)
            && session
                .document()
                .is_some_and(|document| !document.preview.full_frame_shown())
    });
    assert_eq!(shown(&mut harness), cropped);
}
