//! Drives the real workspace with GPUI's headless renderer.
//!
//! Ignored by default: they need a wgpu adapter, and `ROHDITOR_RAW` for the ones that open a
//! photo. The photo is copied to a scratch folder first, so sidecars never end up next to the
//! original. `ROHDITOR_SHOTS=DIR` writes a PNG per scenario.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AppContext, Bounds, Entity, HeadlessAppContext, Keystroke, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, PlatformInput, Point, Size, WindowHandle,
    point, px, size,
};
use rohditor_engine::persistence;
use rohditor_engine::preview::WgpuContext;
use rohditor_gpu::{GpuPreviewProcessor, GpuSensorProcessor, GpuSpatialProcessor};

use crate::library::{Library, LibraryWorker};
use crate::session::{Display, Session, SessionWorkers};
use crate::ui::test_id::recorded;
use crate::workspace::Workspace;
use crate::{actions, assets, device};

const WINDOW: Size<Pixels> = size(px(1440.0), px(900.0));
const EXPOSURE: &str = "adjustment:Exposure";

struct Harness {
    cx: HeadlessAppContext,
    window: WindowHandle<Workspace>,
    session: Entity<Session>,
    library: Entity<Library>,
}

impl Harness {
    fn new() -> Self {
        let mut cx = HeadlessAppContext::with_platform(
            Arc::new(gpui_wgpu::CosmicTextSystem::new("Noto Sans")),
            Arc::new(assets::Assets),
            gpui_platform::current_headless_renderer,
        );
        // Worker threads run in real time.
        cx.allow_parking();
        cx.update(actions::bind_keys);
        let mut created = None;
        let window = cx
            .open_window(WINDOW, |window, cx| {
                let session = cx.new(|cx| {
                    let workers = SessionWorkers::start().expect("the engine starts");
                    let settings = rohditor_engine::settings::AppSettings::default();
                    Session::new(workers, settings, device::shared_context(), cx)
                });
                let library = cx.new(|cx| {
                    let worker = LibraryWorker::start().expect("the catalog starts");
                    Library::new(worker, None, &session, cx)
                });
                created = Some((session.clone(), library.clone()));
                cx.new(|cx| Workspace::new(session, library, window, cx))
            })
            .expect("headless window");
        let (session, library) = created.expect("the window was built");
        Self {
            cx,
            window,
            session,
            library,
        }
    }

    fn with_session<R>(
        &mut self,
        change: impl FnOnce(&mut Session, &mut gpui::Context<Session>) -> R,
    ) -> R {
        let session = self.session.clone();
        self.cx.update(|cx| session.update(cx, change))
    }

    /// Let workers and timers run until `done`.
    fn wait_until(&mut self, what: &str, done: impl Fn(&Session) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            self.with_session(|session, cx| session.tick(cx));
            self.cx.run_until_parked();
            if self.with_session(|session, _| done(session)) {
                return;
            }
            let problem = self.with_session(|session, _| {
                session
                    .document()
                    .and_then(|document| document.error.clone())
            });
            assert_eq!(problem, None, "while waiting for {what}");
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_library(&mut self, what: &str, done: impl Fn(&Library) -> bool) {
        let library = self.library.clone();
        let deadline = Instant::now() + Duration::from_secs(60);
        while !self.cx.update(|cx| done(library.read(cx))) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            self.cx.update(|cx| library.update(cx, Library::tick));
            self.cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn send(&mut self, event: PlatformInput) {
        self.cx
            .update_window(self.window.into(), |_, window, cx| {
                window.dispatch_event(event, cx);
            })
            .expect("window is open");
        self.cx.run_until_parked();
    }

    /// The bounds of the element with this test id, if it is shown.
    fn lookup(&mut self, name: &str) -> Option<Bounds<Pixels>> {
        // Cached panels record nothing, so lay out everything anew.
        recorded::clear();
        self.cx
            .update_window(self.window.into(), |_, window, cx| {
                window.refresh();
                drop(window.draw(cx));
            })
            .expect("window is open");
        recorded::get(name)
    }

    fn find(&mut self, name: &str) -> Bounds<Pixels> {
        self.lookup(name)
            .unwrap_or_else(|| panic!("{name} is not shown"))
    }

    fn click(&mut self, name: &str) {
        let position = self.find(name).center();
        self.press(position, 1);
        self.release(position);
    }

    /// How often the files and develop panels were rendered.
    fn panel_renders(&mut self) -> (usize, usize) {
        let window = self.window;
        self.cx
            .update(|cx| window.update(cx, |workspace, _, cx| workspace.panel_renders(cx)))
            .expect("window is open")
    }

    /// Draw a frame if something changed, as the window would.
    fn draw(&mut self) {
        self.cx
            .update_window(self.window.into(), |_, window, cx| drop(window.draw(cx)))
            .expect("window is open");
    }

    fn press(&mut self, position: Point<Pixels>, click_count: usize) {
        self.send(PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position,
            click_count,
            ..Default::default()
        }));
    }

    fn drag_to(&mut self, position: Point<Pixels>) {
        self.send(PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        }));
    }

    fn release(&mut self, position: Point<Pixels>) {
        self.send(PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position,
            click_count: 1,
            ..Default::default()
        }));
    }

    fn keys(&mut self, keys: &str) {
        for key in keys.split(' ') {
            let mut keystroke = Keystroke::parse(key).expect("valid keystroke");
            if keystroke.key.len() == 1 && !keystroke.modifiers.control {
                keystroke.key_char = Some(keystroke.key.clone());
            }
            self.cx
                .update_window(self.window.into(), |_, window, cx| {
                    window.dispatch_keystroke(keystroke, cx);
                })
                .expect("window is open");
            self.cx.run_until_parked();
        }
    }

    fn capture(&mut self, name: &str) -> image::RgbaImage {
        self.cx.run_until_parked();
        let shot = self
            .cx
            .capture_screenshot(self.window.into())
            .expect("screenshot");
        if let Some(directory) = std::env::var_os("ROHDITOR_SHOTS") {
            shot.save(Path::new(&directory).join(format!("{name}.png")))
                .expect("screenshot is written");
        }
        shot
    }

    fn exposure(&mut self) -> f32 {
        self.with_session(|session, _| {
            let document = session.document().expect("a photo is open");
            document.edits.recipe().light.exposure_ev
        })
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.with_session(|session, cx| session.shut_down(cx));
    }
}

/// A scratch folder holding copies of `ROHDITOR_RAW` under two names.
fn scratch_photos(name: &str) -> Option<(PathBuf, PathBuf)> {
    let raw = PathBuf::from(std::env::var_os("ROHDITOR_RAW")?);
    let folder = std::env::temp_dir().join(format!("rohditor-gpui-{name}"));
    drop(std::fs::remove_dir_all(&folder));
    std::fs::create_dir_all(&folder).expect("scratch folder");
    let copy = |name: &str| {
        let target = folder.join(name);
        std::fs::copy(&raw, &target).expect("photo is copied");
        target
    };
    Some((copy("A.ARW"), copy("B.ARW")))
}

fn opened(name: &str) -> Option<(Harness, PathBuf, PathBuf)> {
    let (first, second) = scratch_photos(name)?;
    let mut harness = Harness::new();
    let path = first.clone();
    harness.with_session(|session, cx| session.open_file(path, cx));
    // The camera look is fitted in the background and changes the picture once it is ready.
    harness.wait_until("the developed preview with its camera look", |session| {
        settled(session)
            && session
                .document()
                .is_some_and(|document| document.edits.recipe().rendering.camera_tone.is_some())
    });
    Some((harness, first, second))
}

/// The GPU frame and its histogram are current.
fn settled(session: &Session) -> bool {
    session.display().is_some_and(Display::is_zero_copy)
        && session.document().is_some_and(|document| {
            document.preview.status.is_none()
                && document.preview.histogram_revision == Some(document.edits.revision())
        })
}

/// The brightness of the photo around `center`.
fn brightness(shot: &image::RgbaImage, center: Point<Pixels>) -> f64 {
    let scale = shot.width() as f32 / WINDOW.width.as_f32();
    let pixel = |logical: Pixels| (logical.as_f32() * scale) as u32;
    let (cx, cy) = (pixel(center.x), pixel(center.y));
    let mut sum = 0.0;
    for y in cy - 100..cy + 100 {
        for x in cx - 100..cx + 100 {
            let pixel = shot.get_pixel(x, y);
            sum += f64::from(pixel[0]) + f64::from(pixel[1]) + f64::from(pixel[2]);
        }
    }
    sum / (200.0 * 200.0 * 3.0)
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn the_engine_pipelines_build_on_gpuis_device() {
    let _harness = Harness::new();
    let WgpuContext {
        adapter,
        device,
        queue,
        target_format,
    } = device::shared_context().expect("GPUI published its device");
    GpuPreviewProcessor::new(&adapter, &device, &queue, target_format).expect("preview");
    GpuSpatialProcessor::new(&device, &queue).expect("spatial");
    GpuSensorProcessor::new(&adapter, &device, &queue).expect("sensor");
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn starts_without_a_photo() {
    let mut harness = Harness::new();
    let shot = harness.capture("welcome");
    let background = shot.get_pixel(shot.width() / 2, shot.height() / 4);
    let (left, right) = (shot.width() * 2 / 5, shot.width() * 3 / 5);
    let has_text = (left..right).any(|x| {
        let rows = shot.height() * 2 / 5..shot.height() * 3 / 5;
        rows.into_iter().any(|y| shot.get_pixel(x, y) != background)
    });
    assert!(has_text, "the welcome commands are drawn in the middle");
    assert!(harness.with_session(|session, _| session.display().is_none()));
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn dragging_a_slider_is_one_undo_step_and_is_saved() {
    let Some((mut harness, first, _)) = opened("slider") else {
        return;
    };
    let before = harness.capture("photo");
    let center = harness.find("viewport").center();
    let dark = brightness(&before, center);

    let start = harness.find(EXPOSURE).center();
    harness.press(start, 1);
    harness.drag_to(start + point(px(10.0), px(0.0)));
    harness.drag_to(start + point(px(30.0), px(0.0)));
    harness.release(start + point(px(30.0), px(0.0)));
    let exposure = harness.exposure();
    assert!((0.9..1.3).contains(&exposure), "exposure is {exposure}");
    harness.wait_until("the edited preview", settled);
    let after = harness.capture("photo-edited");
    assert!(
        brightness(&after, center) > dark * 1.2,
        "the frame follows the edit"
    );

    harness.wait_until("the autosave", |session| {
        session
            .document()
            .is_some_and(|document| !document.is_dirty())
    });
    let saved = persistence::load_recipe(&first).expect("sidecar is readable");
    assert_eq!(saved.expect("sidecar exists").light.exposure_ev, exposure);

    harness.keys("ctrl-z");
    assert_eq!(harness.exposure(), 0.0);
    harness.wait_until("the undone preview", settled);
    let undone = brightness(&harness.capture("photo-undone"), center);
    assert!((undone - dark).abs() < 1.0);
    harness.keys("ctrl-shift-z");
    assert_eq!(harness.exposure(), exposure);

    harness.click("button:undo");
    assert_eq!(harness.exposure(), 0.0);
    harness.click("button:redo");
    assert_eq!(harness.exposure(), exposure);

    harness.press(start, 2);
    assert_eq!(harness.exposure(), 0.0);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn the_palette_runs_commands_and_arrows_change_photos() {
    let Some((mut harness, first, second)) = opened("palette") else {
        return;
    };
    let open = |harness: &mut Harness| {
        harness.with_session(|session, _| session.document().map(|open| open.path.clone()))
    };
    harness.wait_for_library("the folder", |library| library.len() == 2);

    harness.keys("ctrl-shift-p");
    harness.keys("n e x t");
    harness.capture("palette");
    assert_eq!(
        open(&mut harness).as_ref(),
        Some(&first),
        "typing runs nothing"
    );
    harness.keys("enter");
    assert_eq!(open(&mut harness).as_ref(), Some(&second));
    harness.wait_until("the second photo", settled);
    harness.wait_for_library("the thumbnails", |library| {
        [&first, &second]
            .iter()
            .all(|path| library.thumbnail(path).is_some())
    });

    harness.keys("left");
    assert_eq!(open(&mut harness).as_ref(), Some(&first));
    harness.wait_until("the first photo again", settled);
    harness.capture("switched");
    harness.click("button:close-tab");
    assert_eq!(open(&mut harness), None);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn panels_redraw_only_for_their_own_changes() {
    let Some((mut harness, _, second)) = opened("redraw") else {
        return;
    };
    harness.wait_for_library("the folder", |library| library.len() == 2);
    let viewport = harness.find("viewport").center();
    let exposure = harness.find(EXPOSURE).center();
    harness.draw();

    // GPUI redraws the whole window for a press and a release, so only count what is between.
    harness.press(viewport, 1);
    harness.draw();
    let before = harness.panel_renders();
    for step in 1..=3 {
        harness.drag_to(viewport + point(px(step as f32 * 5.0), px(0.0)));
        harness.draw();
    }
    assert_eq!(harness.panel_renders(), before, "panning redraws no panel");
    harness.release(viewport);

    harness.press(exposure, 1);
    harness.draw();
    let before = harness.panel_renders();
    for step in 1..=3 {
        harness.drag_to(exposure + point(px(step as f32 * 5.0), px(0.0)));
        harness.draw();
    }
    let after = harness.panel_renders();
    assert_eq!(after.0, before.0, "a slider does not redraw the files");
    assert!(after.1 >= before.1 + 3, "a slider redraws its own panel");
    harness.release(exposure);

    let before = harness.panel_renders();
    harness.keys("right");
    harness.draw();
    assert!(harness.panel_renders().0 > before.0, "the highlight moves");
    harness.click("file:A.ARW");
    let open = harness.with_session(|session, _| session.document().map(|d| d.path.clone()));
    assert_ne!(open, Some(second));

    harness.wait_until("the photo again", settled);
    harness.click("section:Light");
    assert_eq!(harness.lookup(EXPOSURE), None, "the section collapsed");
    harness.click("button:toggle-files");
    assert_eq!(harness.lookup("file:A.ARW"), None, "the panel closed");
}

/// Prints what one step of a slider drag costs. The CPU time covers the edit, submitting the
/// engine's GPU work and building GPUI's scene; completion is when the GPU finished the preview.
#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn drag_step_cost() {
    let Some((mut harness, ..)) = opened("cost") else {
        return;
    };
    let start = harness.find(EXPOSURE).center();
    harness.press(start, 1);
    let (mut steps, mut completions) = (Vec::new(), Vec::new());
    for step in 1..=60 {
        let begun = Instant::now();
        harness.drag_to(start + point(px(step as f32 * 0.5), px(0.0)));
        harness
            .cx
            .update_window(harness.window.into(), |_, window, cx| {
                drop(window.draw(cx));
            })
            .expect("window is open");
        steps.push(begun.elapsed());
        let shown = |session: &Session| {
            let preview = &session.document()?.preview;
            preview.diagnostics?.gpu_queue_completion
        };
        harness.wait_until("the GPU", |session| shown(session).is_some());
        completions.extend(harness.with_session(|session, _| shown(session)));
    }
    harness.release(start + point(px(30.0), px(0.0)));
    steps.sort_unstable();
    completions.sort_unstable();
    println!(
        "drag step: CPU median {:?}, GPU completion median {:?}",
        steps[30], completions[30]
    );
}
