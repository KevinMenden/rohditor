//! Making generated names safe to create on common filesystems.
//!
//! The rules are deliberately the strictest common subset (Windows/FAT), so a
//! batch exported to an external drive is as valid as one exported at home.

/// Longest file stem in bytes. Leaves room for an extension and a rename
/// suffix inside the usual 255-byte file name limit.
pub(super) const MAX_STEM_BYTES: usize = 200;

/// Names Windows refuses regardless of extension.
const RESERVED_NAMES: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

const FALLBACK_STEM: &str = "export";

/// A file stem or folder name that is valid as a single path component.
pub(super) fn sanitize_component(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    // Cut first, then trim: Windows strips trailing dots and spaces, and a cut
    // that lands after one would otherwise leave it behind. A leading space is
    // only trouble.
    let mut name = truncate_to_bytes(&replaced, MAX_STEM_BYTES)
        .trim_start_matches(' ')
        .trim_end_matches([' ', '.'])
        .to_owned();
    // "." and ".." survive the character filter but are not file names.
    if name.is_empty() || name.chars().all(|c| c == '.') {
        name = FALLBACK_STEM.to_owned();
    }
    // "CON.txt" is as reserved as "CON": compare the part before the first dot.
    let first_part = name.split('.').next().unwrap_or_default();
    if RESERVED_NAMES.contains(&first_part.to_ascii_lowercase().as_str()) {
        name.insert(0, '_');
    }
    name
}

/// The longest prefix of `text` that fits in `max_bytes` without splitting a
/// character.
fn truncate_to_bytes(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn illegal_characters_and_controls_become_underscores() {
        assert_eq!(
            sanitize_component("a/b\\c:d*e?f\"g<h>i|j"),
            "a_b_c_d_e_f_g_h_i_j"
        );
        assert_eq!(sanitize_component("tab\there\nnewline"), "tab_here_newline");
    }

    #[test]
    fn trailing_dots_and_spaces_are_removed() {
        assert_eq!(sanitize_component("  shot. . "), "shot");
        assert_eq!(sanitize_component(".hidden"), ".hidden");
    }

    #[test]
    fn a_cut_that_lands_after_a_dot_or_space_is_trimmed_too() {
        for separator in ['.', ' '] {
            let raw = format!(
                "{}{separator}{}",
                "a".repeat(MAX_STEM_BYTES - 1),
                "b".repeat(50)
            );
            let name = sanitize_component(&raw);
            assert_eq!(name, "a".repeat(MAX_STEM_BYTES - 1), "{separator:?}");
        }
    }

    #[test]
    fn empty_and_dot_only_names_fall_back() {
        for raw in ["", "   ", ".", "..", "..."] {
            assert_eq!(sanitize_component(raw), "export", "{raw:?}");
        }
    }

    #[test]
    fn reserved_device_names_are_prefixed() {
        assert_eq!(sanitize_component("CON"), "_CON");
        assert_eq!(sanitize_component("lpt1.backup"), "_lpt1.backup");
        assert_eq!(sanitize_component("console"), "console");
    }

    #[test]
    fn long_names_are_cut_on_a_character_boundary() {
        // "é" is two bytes, so a 200-byte limit cannot fall inside the 101st one.
        let name = sanitize_component(&"é".repeat(150));
        assert_eq!(name.len(), MAX_STEM_BYTES);
        assert!(name.chars().all(|c| c == 'é'));

        let odd = sanitize_component(&format!("a{}", "é".repeat(150)));
        assert_eq!(odd.len(), MAX_STEM_BYTES - 1);
    }
}
