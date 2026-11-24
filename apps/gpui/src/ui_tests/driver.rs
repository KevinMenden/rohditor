//! A scripted driver: explore the UI from a text file, without recompiling.
//!
//! ```text
//! ROHDITOR_RAW=photo.ARW ROHDITOR_SCRIPT=script.txt \
//!     cargo test driver -- --ignored --nocapture
//! ```
//!
//! One command per line, `#` starts a comment. `SEL` is a test id such as `section:Defringe`
//! (see the `.txt` list written next to each screenshot). Offsets are logical pixels.
//!
//! ```text
//! window 1440 1800        # first line only; the default is 1440 900
//! open A.ARW              # A.ARW or B.ARW: copies of ROHDITOR_RAW in a scratch folder
//! click SEL               # also: rightclick
//! hover SEL               # moves the pointer to the center of SEL
//! drag SEL DX DY          # press at the center of SEL, move by (DX, DY), release
//! press SEL               # press without releasing, for shots in the middle of a drag
//! move DX DY              # move the pointer; drags while a press is held
//! release
//! scroll SEL DX DY        # wheel over SEL; positive DY scrolls up
//! keys ctrl-f m o i r e   # keystrokes, as in the compiled tests
//! shot NAME               # NAME.png and NAME.txt, after the preview has settled
//! ```
//!
//! Output goes to `ROHDITOR_SHOTS`, or `rohditor-gpui-shots` in the temp directory. A failing
//! step also writes `error.png` and `error.txt`.

use std::fmt::Write as _;

use super::*;

#[derive(Debug, PartialEq)]
enum Step {
    Window(Size<Pixels>),
    Open(String),
    Click(String),
    RightClick(String),
    Hover(String),
    Drag(String, Point<Pixels>),
    Press(String),
    Move(Point<Pixels>),
    Release,
    Scroll(String, Point<Pixels>),
    Keys(String),
    Shot(String),
}

/// The steps with the line they came from.
type Script = Vec<(usize, Step)>;

fn parse(text: &str) -> Result<Script, String> {
    let mut script = Script::new();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let step = parse_line(line, script.is_empty())
            .map_err(|problem| format!("line {number}: {problem}"))?;
        script.push((number, step));
    }
    Ok(script)
}

fn parse_line(line: &str, first: bool) -> Result<Step, String> {
    let (command, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let words: Vec<&str> = rest.split_whitespace().collect();
    let number = |word: &str| {
        word.parse::<f32>()
            .map_err(|_| format!("`{word}` is not a number"))
    };
    let selector = |words: &[&str]| match words {
        [name] => Ok(name.to_string()),
        _ => Err(format!("{command} takes one selector")),
    };
    let offset = |words: &[&str]| match words {
        [dx, dy] => Ok(point(px(number(dx)?), px(number(dy)?))),
        _ => Err(format!("{command} takes DX DY")),
    };
    let with_offset = |words: &[&str]| match words {
        [name, dx, dy] => Ok((name.to_string(), offset(&[dx, dy])?)),
        _ => Err(format!("{command} takes SEL DX DY")),
    };
    Ok(match command {
        "window" if first => {
            let Point { x, y } = offset(&words)?;
            Step::Window(size(x, y))
        }
        "window" => return Err("window must be the first command".into()),
        "open" => Step::Open(selector(&words)?),
        "click" => Step::Click(selector(&words)?),
        "rightclick" => Step::RightClick(selector(&words)?),
        "hover" => Step::Hover(selector(&words)?),
        "press" => Step::Press(selector(&words)?),
        "move" => Step::Move(offset(&words)?),
        "release" if words.is_empty() => Step::Release,
        "release" => return Err("release takes nothing".into()),
        "drag" => {
            let (name, by) = with_offset(&words)?;
            Step::Drag(name, by)
        }
        "scroll" => {
            let (name, by) = with_offset(&words)?;
            Step::Scroll(name, by)
        }
        "keys" if !words.is_empty() => Step::Keys(words.join(" ")),
        "keys" => return Err("keys takes keystrokes".into()),
        "shot" => {
            let name = selector(&words)?;
            let plain = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
            if !name.chars().all(plain) {
                return Err(format!("`{name}` should be letters, digits, - and _"));
            }
            Step::Shot(name)
        }
        other => return Err(format!("unknown command `{other}`")),
    })
}

/// Runs steps against one window and tracks the pointer between them.
struct Driver {
    harness: Harness,
    output: PathBuf,
    /// The pointer, and whether the left button is held.
    pointer: Point<Pixels>,
    pressed: bool,
    photos: Option<(PathBuf, PathBuf)>,
}

impl Driver {
    fn new(window: Size<Pixels>) -> Self {
        let output = std::env::var_os("ROHDITOR_SHOTS")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("rohditor-gpui-shots"));
        std::fs::create_dir_all(&output).expect("the output folder");
        Self {
            harness: Harness::sized(window),
            output,
            pointer: point(px(0.0), px(0.0)),
            pressed: false,
            photos: None,
        }
    }

    fn target(&mut self, name: &str) -> Result<Point<Pixels>, String> {
        self.harness
            .lookup(name)
            .map(|bounds| bounds.center())
            .ok_or_else(|| {
                format!("{name} is not shown; `shot` lists what is (the .txt next to the png)")
            })
    }

    fn move_pointer(&mut self, to: Point<Pixels>) {
        if self.pressed {
            self.harness.drag_to(to);
        } else {
            self.harness.hover(to);
        }
        self.pointer = to;
    }

    fn release(&mut self) {
        self.harness.release(self.pointer);
        self.pressed = false;
    }

    fn open(&mut self, name: &str) -> Result<(), String> {
        if self.photos.is_none() {
            self.photos = Some(scratch_photos("script").ok_or("ROHDITOR_RAW is not set")?);
        }
        let (first, second) = self.photos.clone().expect("copied above");
        let path = match name {
            "A.ARW" => first,
            "B.ARW" => second,
            other => return Err(format!("open takes A.ARW or B.ARW, not `{other}`")),
        };
        self.harness.open(path);
        Ok(())
    }

    fn run(&mut self, step: &Step) -> Result<(), String> {
        match step {
            // Only matters before the window opens.
            Step::Window(_) => {}
            Step::Open(name) => self.open(name)?,
            Step::Click(name) => {
                let position = self.target(name)?;
                self.move_pointer(position);
                self.harness.press(position, 1);
                self.harness.release(position);
            }
            Step::RightClick(name) => {
                let position = self.target(name)?;
                self.move_pointer(position);
                self.harness.right_click(name);
            }
            Step::Hover(name) => {
                let position = self.target(name)?;
                self.move_pointer(position);
            }
            Step::Drag(name, by) => {
                let start = self.target(name)?;
                self.move_pointer(start);
                self.harness.press(start, 1);
                self.pressed = true;
                // Gradually, as a hand would, so that handlers see intermediate positions.
                for part in 1..=4 {
                    self.move_pointer(start + *by * (part as f32 / 4.0));
                }
                self.release();
            }
            Step::Press(name) => {
                let position = self.target(name)?;
                self.move_pointer(position);
                self.harness.press(position, 1);
                self.pressed = true;
            }
            Step::Move(by) => self.move_pointer(self.pointer + *by),
            Step::Release => self.release(),
            Step::Scroll(name, by) => {
                let position = self.target(name)?;
                self.move_pointer(position);
                self.harness.scroll(position, *by);
            }
            Step::Keys(keys) => self.harness.keys(keys),
            Step::Shot(name) => self.shot(name),
        }
        self.harness.draw();
        Ok(())
    }

    /// A screenshot and the list of what is on it, once the preview has caught up.
    fn shot(&mut self, name: &str) {
        let photo_open = self
            .harness
            .with_session(|session, _| session.document().is_some());
        if photo_open {
            self.harness.wait_until("the edited preview", settled);
        }
        let image = self.harness.screenshot();
        let png = self.output.join(format!("{name}.png"));
        image.save(&png).expect("screenshot is written");

        let mut list = String::new();
        for (selector, bounds) in self.harness.visible() {
            let (origin, size) = (bounds.origin, bounds.size);
            writeln!(
                list,
                "{selector} x={:.0} y={:.0} w={:.0} h={:.0}",
                origin.x.as_f32(),
                origin.y.as_f32(),
                size.width.as_f32(),
                size.height.as_f32(),
            )
            .expect("writing to a string");
        }
        std::fs::write(self.output.join(format!("{name}.txt")), list).expect("list is written");
        eprintln!("shot {}", png.display());
    }
}

#[test]
#[ignore = "requires a wgpu adapter, ROHDITOR_SCRIPT and ROHDITOR_RAW"]
fn run_the_script() {
    let Some(path) = std::env::var_os("ROHDITOR_SCRIPT") else {
        eprintln!("set ROHDITOR_SCRIPT to a script, see ui_tests/driver.rs");
        return;
    };
    let text = std::fs::read_to_string(&path).expect("the script is readable");
    let script = parse(&text).unwrap_or_else(|problem| panic!("{problem}"));
    let window = match script.first() {
        Some((_, Step::Window(window))) => *window,
        _ => WINDOW,
    };
    let mut driver = Driver::new(window);
    for (number, step) in &script {
        if let Err(problem) = driver.run(step) {
            driver.shot("error");
            panic!("line {number}: {problem}");
        }
    }
}

#[test]
fn scripts_parse() {
    let script = parse(
        "window 800 600\n# comment\n\nopen A.ARW  # trailing\ndrag adjustment:Exposure 100 -5\n\
         keys ctrl-f m o i r e\nshot after-drag\n",
    )
    .expect("a valid script");
    let steps: Vec<_> = script.iter().map(|(line, _)| *line).collect();
    assert_eq!(steps, [1, 4, 5, 6, 7]);
    assert_eq!(script[0].1, Step::Window(size(px(800.0), px(600.0))));
    assert_eq!(
        script[2].1,
        Step::Drag("adjustment:Exposure".into(), point(px(100.0), px(-5.0)))
    );
    assert_eq!(script[3].1, Step::Keys("ctrl-f m o i r e".into()));
}

#[test]
fn script_errors_name_the_line() {
    for (text, expected) in [
        ("open A.ARW\nclick", "line 2: click takes one selector"),
        ("zoom in", "line 1: unknown command `zoom`"),
        (
            "hover x\nwindow 1 1",
            "line 2: window must be the first command",
        ),
        ("drag a 1 x", "line 1: `x` is not a number"),
        (
            "shot ../x",
            "line 1: `../x` should be letters, digits, - and _",
        ),
    ] {
        assert_eq!(parse(text).unwrap_err(), expected);
    }
}
