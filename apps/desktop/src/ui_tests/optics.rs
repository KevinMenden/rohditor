//! The lens profile, its corrections and the search for a lens.

use rohditor_edit::{LensProfileSelection, OpticsAdjustments};

use super::*;

fn optics(harness: &mut Harness) -> OpticsAdjustments {
    harness.with_session(|session, _| {
        let document = session.document().expect("a photo is open");
        document.edits.recipe().optics.clone()
    })
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn a_lens_is_searched_and_its_corrections_switched() {
    let Some((mut harness, ..)) = opened_in(size(px(1440.0), px(1800.0)), "optics") else {
        return;
    };
    let original = optics(&mut harness);
    harness.capture("optics-off");
    harness.click("dropdown:lens-profile");
    harness.capture("optics-menu");
    harness.click("menu:Choose Lens…");
    harness.keys("s i g m a space 3 0 m m");
    harness.capture("optics-search");
    harness.keys("enter");
    harness.draw();
    let LensProfileSelection::Lensfun { profile_id } = optics(&mut harness).profile else {
        panic!("a lens is chosen");
    };
    assert!(profile_id.contains("sigma") && profile_id.contains("30mm"));
    harness.find("changed:Optics");
    harness.wait_until("the corrected preview", settled);
    harness.capture("optics");

    harness.click("toggle:lens-distortion");
    assert!(!optics(&mut harness).distortion);
    harness.wait_until("the preview without distortion correction", settled);
    harness.keys("ctrl-z");
    assert!(optics(&mut harness).distortion, "it is one undo step");
    harness.click("button:reset-Optics");
    assert_eq!(optics(&mut harness), original);

    // The whole list is reachable by key.
    harness.keys("ctrl-shift-p");
    harness.keys("c h o o s e l e n s enter");
    harness.keys("up down down");
    harness.capture("optics-list");
    harness.keys("escape");
    assert_eq!(optics(&mut harness), original);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn a_detected_lens_is_corrected_automatically() {
    let Some((mut harness, ..)) = opened("optics-automatic") else {
        return;
    };
    let detected = harness.with_session(|session, _| {
        let document = session.document().expect("a photo is open");
        document.lens_profiles.detected().is_some()
    });
    harness.keys("ctrl-shift-p");
    harness.keys("p r o f i l e a u t o enter");
    harness.draw();
    let automatic = optics(&mut harness).profile == LensProfileSelection::Automatic;
    assert_eq!(automatic, detected);
    if detected {
        harness.wait_until("the corrected preview", settled);
    } else {
        harness.find("toast");
    }
}
