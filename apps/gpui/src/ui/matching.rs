//! How searches compare text: ignoring case and the accent of "Moiré".

fn folded(text: &str) -> String {
    text.to_lowercase().replace('é', "e")
}

/// Whether `text` contains every word of `query`.
pub fn words(text: &str, query: &str) -> bool {
    let text = folded(text);
    folded(query)
        .split_whitespace()
        .all(|word| text.contains(word))
}

/// Whether the letters of `query` appear in `text` in order, ignoring spaces.
pub fn letters(text: &str, query: &str) -> bool {
    let text = folded(text);
    let mut letters = text.chars();
    folded(query)
        .chars()
        .filter(|letter| !letter.is_whitespace())
        .all(|wanted| letters.any(|letter| letter == wanted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_word_of_a_search_must_match() {
        let text = "Light Tone curve Shadows";
        assert!(words(text, ""));
        assert!(words(text, "shad CURVE"));
        assert!(!words(text, "curve whites"));
        assert!(words("Moiré", "moire") && words("Moiré", "MOIRÉ"));
    }

    #[test]
    fn query_letters_match_in_order() {
        assert!(letters("view: zoom to fit", "zfit"));
        assert!(letters("view: zoom to fit", "Zoom Fit"));
        assert!(!letters("view: zoom to fit", "fitzoom"));
        assert!(letters("edit: undo", ""));
        assert!(letters("develop: reset moiré", "moire"));
    }
}
