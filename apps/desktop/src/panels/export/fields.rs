//! The typed numbers of the export dialog: JPEG quality and the pixel sizes.

use std::ops::RangeInclusive;

use gpui::{App, AppContext as _, Context, Entity};
use rohditor_engine::export_options::{ExportKind, ExportOptions, SizeMode};

use crate::ui::text_input::{Changed, TextInput};

const QUALITY: RangeInclusive<u32> = 1..=100;
/// The largest side an image format can hold.
const PIXELS: RangeInclusive<u32> = 1..=65_535;

/// The number in `text`, or what is wrong with it.
pub fn parse_number(name: &str, text: &str, allowed: RangeInclusive<u32>) -> Result<u32, String> {
    let range = format!("{} to {}", allowed.start(), allowed.end());
    let digits = text.trim().trim_end_matches("px").trim();
    match digits.parse::<u32>() {
        Ok(value) if allowed.contains(&value) => Ok(value),
        _ => Err(format!("{name} must be a whole number from {range}.")),
    }
}

pub struct Fields {
    pub quality: Entity<TextInput>,
    pub long_edge: Entity<TextInput>,
    pub fit_width: Entity<TextInput>,
    pub fit_height: Entity<TextInput>,
}

impl Fields {
    /// Fields that start with the values of `options`. `on_change` runs after every edit.
    pub fn new<V: 'static>(
        options: &ExportOptions,
        on_change: fn(&mut V, &mut Context<V>),
        cx: &mut Context<V>,
    ) -> Self {
        let mut field = |label: &'static str, value: u32| {
            let input = cx.new(|cx| TextInput::new(label, label, value.to_string(), cx));
            cx.subscribe(&input, move |this, _, _: &Changed, cx| on_change(this, cx))
                .detach();
            input
        };
        Self {
            quality: field("JPEG quality", u32::from(options.jpeg_quality)),
            long_edge: field("Long edge", options.long_edge),
            fit_width: field("Width", options.fit_width),
            fit_height: field("Height", options.fit_height),
        }
    }

    /// Copy the numbers that matter for the chosen format and size into `options`.
    pub fn read_into(&self, options: &mut ExportOptions, cx: &App) -> Result<(), String> {
        let text = |input: &Entity<TextInput>| input.read(cx).text().to_owned();
        apply(
            options,
            &Texts {
                quality: &text(&self.quality),
                long_edge: &text(&self.long_edge),
                fit_width: &text(&self.fit_width),
                fit_height: &text(&self.fit_height),
            },
        )
    }
}

/// What was typed into each field.
pub struct Texts<'a> {
    pub quality: &'a str,
    pub long_edge: &'a str,
    pub fit_width: &'a str,
    pub fit_height: &'a str,
}

/// Copy the typed numbers that matter for the chosen format and size into `options`. Fields that
/// are not shown are not checked. `options` stays as it was if any number is wrong.
pub fn apply(options: &mut ExportOptions, texts: &Texts) -> Result<(), String> {
    let mut parsed = options.clone();
    if parsed.kind == ExportKind::Jpeg {
        let quality = parse_number("JPEG quality", texts.quality, QUALITY)?;
        // Parsed within 1 to 100.
        parsed.jpeg_quality = quality as u8;
    }
    match parsed.size_mode {
        SizeMode::Original => {}
        SizeMode::LongEdge => {
            parsed.long_edge = parse_number("Long edge", texts.long_edge, PIXELS)?;
        }
        SizeMode::Fit => {
            parsed.fit_width = parse_number("Width", texts.fit_width, PIXELS)?;
            parsed.fit_height = parse_number("Height", texts.fit_height, PIXELS)?;
        }
    }
    *options = parsed;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_whole_and_in_range() {
        assert_eq!(parse_number("Size", " 2048 px", PIXELS), Ok(2048));
        assert_eq!(parse_number("Quality", "100", QUALITY), Ok(100));
        for text in ["", "0", "101", "-5", "9.5", "high"] {
            let error = parse_number("Quality", text, QUALITY).expect_err(text);
            assert_eq!(error, "Quality must be a whole number from 1 to 100.");
        }
        assert!(parse_number("Size", "65536", PIXELS).is_err());
    }

    fn texts<'a>(
        quality: &'a str,
        long_edge: &'a str,
        width: &'a str,
        height: &'a str,
    ) -> Texts<'a> {
        Texts {
            quality,
            long_edge,
            fit_width: width,
            fit_height: height,
        }
    }

    #[test]
    fn fields_that_are_not_shown_are_not_checked() {
        // A PNG has no quality and the original size has no pixel fields.
        let mut options = ExportOptions::with_jpeg_quality(90);
        options.kind = ExportKind::Png;
        apply(&mut options, &texts("high", "wide", "-1", "")).expect("nothing is shown to check");
        assert_eq!(options.jpeg_quality, 90);
        assert_eq!(
            options.long_edge,
            ExportOptions::with_jpeg_quality(90).long_edge
        );
    }

    #[test]
    fn a_wrong_number_blocks_the_export_and_changes_nothing() {
        let mut options = ExportOptions::with_jpeg_quality(90);
        options.size_mode = SizeMode::LongEdge;
        let error = apply(&mut options, &texts("101", "1600", "", "")).expect_err("quality");
        assert!(error.starts_with("JPEG quality"), "{error}");
        let error = apply(&mut options, &texts("80", "0", "", "")).expect_err("long edge");
        assert!(error.starts_with("Long edge"), "{error}");
        assert_eq!((options.jpeg_quality, options.long_edge), (90, 2048));
    }

    #[test]
    fn the_numbers_of_the_chosen_size_are_taken() {
        let mut options = ExportOptions::with_jpeg_quality(90);
        options.size_mode = SizeMode::Fit;
        apply(&mut options, &texts(" 75", "999", "1200 px", "800")).expect("valid");
        assert_eq!(options.jpeg_quality, 75);
        assert_eq!((options.fit_width, options.fit_height), (1200, 800));
        // Not the long edge of another size mode.
        assert_eq!(options.long_edge, 2048);
    }
}
