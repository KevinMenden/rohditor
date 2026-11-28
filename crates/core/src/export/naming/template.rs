//! File name templates such as `{date}_{name}_{seq:4}`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::NamingError;
use super::sanitize::sanitize_component;

/// Largest zero padding a `{seq:N}` token accepts.
const MAX_SEQUENCE_PADDING: usize = 12;
const MISSING_DATE: &str = "no-date";
const MISSING_CAMERA: &str = "no-camera";
/// Used when the source path has no usable file name.
const FALLBACK_NAME: &str = "developed";

/// What a template can say about one source photo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhotoNameInfo {
    /// The RAW file. Its name feeds `{name}`; its folder is the "next to the
    /// source" destination.
    pub source: PathBuf,
    /// EXIF capture time such as `2024:05:06 07:08:09`; `None` when unknown.
    pub capture_date: Option<String>,
    pub camera_model: Option<String>,
}

impl PhotoNameInfo {
    /// A photo known only by its path, for callers without metadata.
    #[must_use]
    pub fn from_path(source: &Path) -> Self {
        Self {
            source: source.to_owned(),
            ..Self::default()
        }
    }

    fn name(&self) -> &str {
        // Only the stem is used, so a private directory name never leaks in.
        self.source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .unwrap_or(FALLBACK_NAME)
    }

    /// `YYYY-MM-DD`, or `None` when the date is absent or not recognisable.
    fn date(&self) -> Option<String> {
        let text = self.capture_date.as_deref()?.trim();
        let date = text.get(..10)?;
        let bytes = date.as_bytes();
        let separators_ok = matches!(bytes[4], b':' | b'-') && matches!(bytes[7], b':' | b'-');
        let digits_ok = bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit());
        if !separators_ok || !digits_ok {
            return None;
        }
        let month: u8 = date[5..7].parse().ok()?;
        let day: u8 = date[8..10].parse().ok()?;
        ((1..=12).contains(&month) && (1..=31).contains(&day))
            .then(|| format!("{}-{}-{}", &date[..4], &date[5..7], &date[8..10]))
    }

    fn camera(&self) -> Option<&str> {
        self.camera_model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Literal(String),
    /// Original file name without extension.
    Name,
    /// Position in the batch, zero padded to at least `padding` digits.
    Sequence {
        padding: usize,
    },
    /// Capture date as `YYYY-MM-DD`.
    Date,
    Camera,
}

/// A parsed file name template. Unknown tokens are rejected when parsing, so
/// a typo is reported instead of leaking into file names.
///
/// Tokens: `{name}`, `{seq}`, `{seq:4}` (zero padded), `{date}` and
/// `{camera}`. Write `{{` and `}}` for literal braces. Missing metadata renders
/// as `no-date` or `no-camera`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileNameTemplate {
    parts: Vec<Part>,
}

impl Default for FileNameTemplate {
    /// `{name}-edited`: what the single-file export dialog suggests.
    fn default() -> Self {
        Self {
            parts: vec![Part::Name, Part::Literal("-edited".to_owned())],
        }
    }
}

impl FileNameTemplate {
    pub fn parse(text: &str) -> Result<Self, NamingError> {
        let mut parts = Vec::new();
        let mut literal = String::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    literal.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    literal.push('}');
                }
                '}' => return Err(NamingError::UnmatchedClosingBrace),
                '{' => {
                    let mut token = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some(c) => token.push(c),
                            None => return Err(NamingError::UnclosedToken),
                        }
                    }
                    if !literal.is_empty() {
                        parts.push(Part::Literal(std::mem::take(&mut literal)));
                    }
                    parts.push(parse_token(&token)?);
                }
                c => literal.push(c),
            }
        }
        if !literal.is_empty() {
            parts.push(Part::Literal(literal));
        }
        if parts.is_empty() {
            return Err(NamingError::EmptyTemplate);
        }
        Ok(Self { parts })
    }

    /// The sanitised file stem for `photo` as number `sequence` of its batch.
    #[must_use]
    pub fn render(&self, photo: &PhotoNameInfo, sequence: u64) -> String {
        let mut stem = String::new();
        for part in &self.parts {
            match part {
                Part::Literal(text) => stem.push_str(text),
                Part::Name => stem.push_str(photo.name()),
                Part::Sequence { padding } => {
                    // Writing to a String cannot fail.
                    let _ = write!(stem, "{sequence:0padding$}");
                }
                Part::Date => {
                    stem.push_str(&photo.date().unwrap_or_else(|| MISSING_DATE.to_owned()))
                }
                Part::Camera => stem.push_str(photo.camera().unwrap_or(MISSING_CAMERA)),
            }
        }
        sanitize_component(&stem)
    }

    /// The name with the given extension, as a single path component.
    #[must_use]
    pub fn file_name(&self, photo: &PhotoNameInfo, sequence: u64, extension: &str) -> String {
        format!("{}.{extension}", self.render(photo, sequence))
    }
}

fn parse_token(token: &str) -> Result<Part, NamingError> {
    match token {
        "name" => Ok(Part::Name),
        "seq" => Ok(Part::Sequence { padding: 0 }),
        "date" => Ok(Part::Date),
        "camera" => Ok(Part::Camera),
        _ => match token.strip_prefix("seq:") {
            Some(digits) => digits
                .parse::<usize>()
                .ok()
                .filter(|padding| (1..=MAX_SEQUENCE_PADDING).contains(padding))
                .map(|padding| Part::Sequence { padding })
                .ok_or_else(|| NamingError::InvalidPadding {
                    token: token.to_owned(),
                    max: MAX_SEQUENCE_PADDING,
                }),
            None => Err(NamingError::UnknownToken {
                token: token.to_owned(),
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo() -> PhotoNameInfo {
        PhotoNameInfo {
            source: PathBuf::from("/private/trip/DSC00042.ARW"),
            capture_date: Some("2024:05:06 07:08:09".to_owned()),
            camera_model: Some("ILCE-6400".to_owned()),
        }
    }

    fn render(template: &str, photo: &PhotoNameInfo, sequence: u64) -> String {
        FileNameTemplate::parse(template)
            .expect("valid template")
            .render(photo, sequence)
    }

    #[test]
    fn every_token_renders() {
        assert_eq!(
            render("{date}_{camera}_{name}_{seq}", &photo(), 7),
            "2024-05-06_ILCE-6400_DSC00042_7"
        );
    }

    #[test]
    fn sequence_is_zero_padded_but_never_truncated() {
        assert_eq!(render("{seq:4}", &photo(), 7), "0007");
        assert_eq!(render("{seq:2}", &photo(), 12345), "12345");
        assert_eq!(render("{seq}", &photo(), 7), "7");
    }

    #[test]
    fn default_template_matches_the_dialog_suggestion() {
        let name = FileNameTemplate::default().file_name(&photo(), 1, "jpg");
        assert_eq!(name, "DSC00042-edited.jpg");
    }

    #[test]
    fn braces_can_be_escaped() {
        assert_eq!(render("{{{name}}}", &photo(), 1), "{DSC00042}");
    }

    #[test]
    fn unknown_tokens_are_errors() {
        assert!(matches!(
            FileNameTemplate::parse("{name}-{lens}"),
            Err(NamingError::UnknownToken { token }) if token == "lens"
        ));
        assert!(matches!(
            FileNameTemplate::parse("{NAME}"),
            Err(NamingError::UnknownToken { .. })
        ));
    }

    #[test]
    fn malformed_templates_are_errors() {
        assert!(matches!(
            FileNameTemplate::parse(""),
            Err(NamingError::EmptyTemplate)
        ));
        assert!(matches!(
            FileNameTemplate::parse("{name"),
            Err(NamingError::UnclosedToken)
        ));
        assert!(matches!(
            FileNameTemplate::parse("name}"),
            Err(NamingError::UnmatchedClosingBrace)
        ));
        for bad in ["{seq:0}", "{seq:13}", "{seq:}", "{seq:x}", "{seq:-1}"] {
            assert!(
                matches!(
                    FileNameTemplate::parse(bad),
                    Err(NamingError::InvalidPadding { .. })
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn missing_metadata_renders_a_placeholder() {
        let bare = PhotoNameInfo::from_path(Path::new("/x/IMG_1.ARW"));
        assert_eq!(
            render("{date}_{camera}_{name}", &bare, 1),
            "no-date_no-camera_IMG_1"
        );
        let blank = PhotoNameInfo {
            camera_model: Some("  ".to_owned()),
            ..bare
        };
        assert_eq!(render("{camera}", &blank, 1), "no-camera");
    }

    #[test]
    fn dates_accept_exif_and_iso_forms_and_reject_garbage() {
        let with = |date: &str| PhotoNameInfo {
            capture_date: Some(date.to_owned()),
            ..photo()
        };
        assert_eq!(
            render("{date}", &with("2024-05-06T07:08:09"), 1),
            "2024-05-06"
        );
        assert_eq!(render("{date}", &with("2024:05:06"), 1), "2024-05-06");
        for garbage in [
            "",
            "yesterday",
            "2024:13:01 00:00:00",
            "2024:00:10",
            "20240506",
            "é024:05:06 x",
        ] {
            assert_eq!(
                render("{date}", &with(garbage), 1),
                "no-date",
                "{garbage:?}"
            );
        }
    }

    #[test]
    fn rendered_names_are_sanitised() {
        let tricky = PhotoNameInfo {
            camera_model: Some("A/B: C?".to_owned()),
            ..photo()
        };
        assert_eq!(render("{camera}", &tricky, 1), "A_B_ C_");
        // A template literal cannot smuggle in a directory either.
        assert_eq!(render("../{name}", &photo(), 1), ".._DSC00042");
    }

    #[test]
    fn sources_without_a_stem_use_the_fallback_name() {
        for path in ["", "/", ".."] {
            let info = PhotoNameInfo::from_path(Path::new(path));
            assert_eq!(render("{name}", &info, 1), "developed", "{path:?}");
        }
    }

    #[test]
    fn very_long_names_are_shortened() {
        let long = PhotoNameInfo::from_path(Path::new(&format!("/x/{}.ARW", "n".repeat(400))));
        assert!(render("{name}-{seq:4}", &long, 1).len() <= 200);
    }
}
