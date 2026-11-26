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
        if options.kind == ExportKind::Jpeg {
            let quality = parse_number("JPEG quality", &text(&self.quality), QUALITY)?;
            // Parsed within 1 to 100.
            options.jpeg_quality = quality as u8;
        }
        match options.size_mode {
            SizeMode::Original => {}
            SizeMode::LongEdge => {
                options.long_edge = parse_number("Long edge", &text(&self.long_edge), PIXELS)?;
            }
            SizeMode::Fit => {
                options.fit_width = parse_number("Width", &text(&self.fit_width), PIXELS)?;
                options.fit_height = parse_number("Height", &text(&self.fit_height), PIXELS)?;
            }
        }
        Ok(())
    }
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
}
