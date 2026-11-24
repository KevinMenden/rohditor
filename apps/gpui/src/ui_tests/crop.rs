//! Changing to the Crop tool and back.

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

/// The size of the shown frame.
fn shown(harness: &mut Harness) -> (u32, u32) {
    harness.with_session(|session, _| {
        let display = session.display().expect("a frame is shown");
        (display.width, display.height)
    })
}

fn draft_square(harness: &mut Harness) {
    harness.with_session(|session, cx| session.draft_aspect(CropAspect::Square, cx));
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
        session.draft_aspect(CropAspect::SixteenNine, cx);
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
