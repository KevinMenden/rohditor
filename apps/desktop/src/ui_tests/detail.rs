//! Noise reduction, sharpening, moiré and defringe.

use rohditor_edit::{EditRecipe, Parameter};
use rohditor_engine::open_document::NoiseSuggestion;

use super::*;

fn recipe(harness: &mut Harness) -> EditRecipe {
    harness.with_session(|session, _| {
        let document = session.document().expect("a photo is open");
        document.edits.recipe().clone()
    })
}

/// High enough to show all sections, with the photo's noise estimated.
fn opened_tall(name: &str) -> Option<Harness> {
    let (mut harness, ..) = opened_in(size(px(1440.0), px(1800.0)), name)?;
    harness.wait_until("the noise estimate", |session| {
        settled(session)
            && session.document().is_some_and(|document| {
                matches!(
                    document.noise_suggestion(),
                    NoiseSuggestion::Suggested { .. }
                )
            })
    });
    Some(harness)
}

fn drag(harness: &mut Harness, parameter: Parameter, x: f32) {
    let start = harness.find(&format!("adjustment:{parameter:?}")).center();
    harness.press(start, 1);
    harness.drag_to(start + point(px(x), px(0.0)));
    harness.release(start + point(px(x), px(0.0)));
    harness.draw();
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn auto_restores_the_suggested_noise_reduction() {
    let Some(mut harness) = opened_tall("noise") else {
        return;
    };
    let chroma = |harness: &mut Harness| recipe(harness).noise_reduction.chroma;
    let suggested = chroma(&mut harness);
    let edited = Parameter::ChromaNoiseReduction;
    drag(&mut harness, edited, 60.0);
    drag(&mut harness, Parameter::LuminanceNoiseReduction, 60.0);
    assert!(chroma(&mut harness) > suggested);
    let before = recipe(&mut harness);
    harness.click("button:auto-noise");
    assert_eq!(chroma(&mut harness), suggested);
    assert!(recipe(&mut harness).noise_reduction.luminance > 0.0);
    harness.keys("ctrl-z");
    assert_eq!(recipe(&mut harness), before, "it is one undo step");
    harness.wait_until("the edited preview", settled);
    harness.capture("noise");
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn detail_sliders_change_the_photo() {
    let Some(mut harness) = opened_tall("detail") else {
        return;
    };
    harness.click("section:FalseColor");
    harness.click("section:Defringe");
    let before = recipe(&mut harness);
    drag(&mut harness, Parameter::CaptureAmount, 100.0);
    drag(&mut harness, Parameter::CaptureRadius, 40.0);
    drag(&mut harness, Parameter::FalseColorSteps, 100.0);
    drag(&mut harness, Parameter::DefringePurpleAmount, 100.0);
    drag(&mut harness, Parameter::DefringePurpleHueStart, -100.0);
    let after = recipe(&mut harness);
    assert!(after.capture_sharpening.enabled);
    assert!(after.capture_sharpening.radius > before.capture_sharpening.radius);
    assert!((1..5).contains(&after.false_color.steps));
    let [start, end] = after.defringe.purple_hue;
    assert!(
        start < 270.0 && end < 330.0 && end > start,
        "the other end follows"
    );
    harness.find("changed:Defringe");
    harness.wait_until("the edited preview", settled);
    harness.capture("detail");

    // Whole steps by key; the first press only continues from the drag.
    drag(&mut harness, Parameter::FalseColorSteps, 0.0);
    harness.keys("left");
    assert_eq!(
        recipe(&mut harness).false_color.steps,
        after.false_color.steps - 1
    );
    harness.keys("ctrl-z");
    harness.click("button:reset-Defringe");
    assert_eq!(recipe(&mut harness).defringe, before.defringe);

    harness.keys("ctrl-f m o i r e");
    harness.find("adjustment:FalseColorSteps");
    assert_eq!(harness.lookup(EXPOSURE), None);
}
