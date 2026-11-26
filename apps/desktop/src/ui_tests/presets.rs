//! Presets and the photo's information.

use super::*;

pub(super) fn type_text(harness: &mut Harness, text: &str) {
    for letter in text.chars() {
        harness.keys(&letter.to_string());
    }
    harness.draw();
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn presets_are_saved_applied_and_managed() {
    let Some((mut harness, ..)) = opened_in(size(px(1440.0), px(1900.0)), "presets-saved") else {
        return;
    };
    let start = harness.find(EXPOSURE).center();
    harness.press(start, 1);
    harness.drag_to(start + point(px(40.0), px(0.0)));
    harness.release(start + point(px(40.0), px(0.0)));
    let brighter = harness.exposure();
    assert!(brighter > 0.0);

    // The edited section is included from the start.
    harness.click("button:save-preset");
    harness.keys("enter");
    harness.find("preset-error");
    type_text(&mut harness, "bright");
    harness.capture("preset-form");
    harness.keys("enter");
    harness.draw();
    harness.click("section:Presets");
    harness.find("preset:bright");

    harness.keys("ctrl-z");
    assert_eq!(harness.exposure(), 0.0);
    harness.click("preset:bright");
    assert_eq!(harness.exposure(), brighter);
    let amount = harness.find("adjustment:preset-amount").center();
    harness.press(amount, 1);
    harness.drag_to(amount - point(px(60.0), px(0.0)));
    harness.drag_to(amount - point(px(100.0), px(0.0)));
    harness.release(amount - point(px(100.0), px(0.0)));
    let less = harness.exposure();
    assert!(less > 0.0 && less < brighter, "{less}");
    harness.wait_until("the preview", settled);
    harness.capture("presets");
    // The press gave the slider the focus.
    harness.keys("right");
    assert!(harness.exposure() > less);
    harness.keys("ctrl-z ctrl-z");
    assert_eq!(harness.exposure(), brighter, "a drag is one undo step");
    harness.keys("ctrl-z");
    assert_eq!(harness.exposure(), 0.0);
    assert_eq!(harness.lookup("adjustment:preset-amount"), None);

    // By key.
    harness.keys("escape p");
    type_text(&mut harness, "bri");
    harness.keys("enter");
    harness.draw();
    assert_eq!(harness.exposure(), brighter);

    harness.right_click("preset:bright");
    harness.click("menu:Rename…");
    type_text(&mut harness, "lighter");
    harness.keys("enter");
    harness.draw();
    harness.find("preset:lighter");
    harness.right_click("preset:lighter");
    harness.click("menu:Delete…");
    harness.click("button:accept");
    assert_eq!(harness.lookup("preset:lighter"), None);
    assert_eq!(harness.exposure(), brighter);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn the_photos_information_is_listed_and_searched() {
    let Some((mut harness, ..)) = opened_in(size(px(1440.0), px(1900.0)), "info") else {
        return;
    };
    harness.find("exposure-summary");
    assert_eq!(harness.lookup("info:Camera"), None, "it starts collapsed");
    harness.click("section:Info");
    harness.find("info:Camera");
    harness.find("info:File size");
    harness.capture("info");
    harness.keys("ctrl-f");
    type_text(&mut harness, "shutter");
    harness.find("info:Shutter");
    assert_eq!(harness.lookup("info:Camera"), None);
}
