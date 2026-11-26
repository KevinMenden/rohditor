//! The export dialog.

use rohditor_engine::export_options::{ExportKind, SizeMode};

use super::*;

fn options(harness: &mut Harness) -> (ExportKind, SizeMode, bool) {
    harness.with_session(|session, _| {
        let options = session.export_options();
        (options.kind, options.size_mode, options.overwrite)
    })
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn the_export_dialog_offers_the_options_of_the_format() {
    let Some((mut harness, ..)) = opened("export-dialog") else {
        return;
    };
    harness.wait_until("the full-resolution frame", Session::export_ready);
    let defaults = options(&mut harness);
    harness.keys("ctrl-e");
    harness.find("export-dialog");

    // JPEG has a quality, PNG a bit depth.
    harness.find("choice:format:JPEG");
    assert_eq!(harness.lookup("choice:depth:16-bit"), None);
    harness.click("choice:format:PNG");
    harness.click("choice:depth:16-bit");
    harness.find("choice:depth:8-bit");

    // Sharpening strength only matters once sharpening is on.
    assert_eq!(harness.lookup("choice:sharpening-strength:High"), None);
    harness.click("choice:sharpening:Matte");
    harness.click("choice:sharpening-strength:High");
    harness.click("choice:size:Long edge");
    harness.click("toggle:overwrite");
    harness.capture("export-dialog");

    // Closing without exporting keeps the options of the last export.
    harness.keys("escape");
    harness.draw();
    assert_eq!(harness.lookup("export-dialog"), None);
    assert_eq!(options(&mut harness), defaults);
}
