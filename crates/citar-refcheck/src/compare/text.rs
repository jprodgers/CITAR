//! Line diffs for long texts: briefings, turn progress, tool errors that run to paragraphs.
//!
//! A briefing is a few kilobytes; printing both copies side by side hides the one changed number.
//! Over [`LONG_TEXT`] characters, or with a line break, a text difference carries a unified diff.

use similar::TextDiff;

/// Texts longer than this many characters get a line diff.
pub const LONG_TEXT: usize = 200;

pub fn wants_line_diff(python: &str, rust: &str) -> bool {
    python.contains('\n')
        || rust.contains('\n')
        || python.chars().count() > LONG_TEXT
        || rust.chars().count() > LONG_TEXT
}

/// A unified diff of two texts by lines, Python's as the old side.
pub fn line_diff(python: &str, rust: &str) -> String {
    TextDiff::from_lines(python, rust)
        .unified_diff()
        .context_radius(2)
        .header("python", "rust")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_or_multiline_texts_get_a_line_diff() {
        assert!(!wants_line_diff("short", "shorter"));
        assert!(wants_line_diff("a\nb", "a\nc"));
        assert!(wants_line_diff(&"x".repeat(201), "y"));
        let diff = line_diff("one\ntwo\nthree\n", "one\n2\nthree\n");
        assert!(diff.contains("--- python"), "{diff}");
        assert!(diff.contains("-two") && diff.contains("+2"), "{diff}");
    }
}
