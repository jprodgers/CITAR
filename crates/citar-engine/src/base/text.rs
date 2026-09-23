//! Text rules the engine carried in regular expressions, as plain scanners
//! (DESIGN.md 8.4).
//!
//! - [`norm`]: loose name lookup, `rules.py:26-30`.
//! - [`possessive_s`]: "Aztecs's" to "Aztecs'", `game.py:17` as `emit` applied it
//!   (`game.py:870`).
//! - [`NameScanner`] and [`find_word`]: where event text names a civilization, leader or city,
//!   `game.py:837-838`, `847` and `854`: each name as a whole word, `(?<!\w)name(?!\w)`, longest
//!   first where names overlap.
//! - [`scrub_coords`]: coordinates such as "(12, 7)" hidden from a viewer who may not know
//!   them, `game.py:18` as `_scrub_event` applied it (`game.py:967`).
//!
//! **Word characters** are Python's `\w` for text: letters, digits and the underscore, in any
//! script, as [`is_word_char`] says. Rust's `char::is_alphanumeric` also counts the combining
//! vowel signs and the few symbols Unicode calls `Other_Alphabetic` (Devanagari "ा", circled
//! letters), which Python does not; no name in the game contains one.
//! **Digits** in coordinates are ASCII only, where Python's `\d` took any decimal digit; the
//! engine only ever writes coordinates in ASCII.
//!
//! Positions are byte offsets into UTF-8. Python's were code point indices; [`char_offset`]
//! converts where a view needs Python's numbers.

use std::borrow::Cow;

use aho_corasick::{AhoCorasick, MatchKind};

/// Whether `c` is a word character in Python's sense (`\w`): alphanumeric or `_`.
#[must_use]
#[inline]
pub fn is_word_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// Whether `c` is whitespace in Python's sense (`\s`, `str.isspace`), which adds the four
/// information separators U+001C to U+001F to Unicode's `White_Space`.
#[must_use]
#[inline]
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// A name for loose lookup: lower case, with everything but ASCII letters and digits dropped
/// (`rules.py:26-30`). "Great Wall of China" and "great_wall-of china" both give
/// "greatwallofchina".
#[must_use]
pub fn norm(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        // Lower-casing can turn a non-ASCII letter into an ASCII one (the Kelvin sign into k,
        // dotted capital I into i and a combining dot), so filter after it, as Python did.
        for l in c.to_lowercase() {
            if l.is_ascii_lowercase() || l.is_ascii_digit() {
                out.push(l);
            }
        }
    }
    out
}

/// Rewrites "s's" after a word character and before a non-word one as "s'" ("Aztecs's
/// Warrior" to "Aztecs' Warrior"), as `emit` did with `(?<=\w)s's\b` (`game.py:17`, `870`).
#[must_use]
pub fn possessive_s(text: &str) -> Cow<'_, str> {
    let mut out: Option<String> = None;
    let mut copied = 0;
    let mut from = 0;
    while let Some(off) = text[from..].find("s's") {
        let i = from + off;
        let after_word = text[..i].chars().next_back().is_some_and(is_word_char);
        let before_break = !text[i + 3..].chars().next().is_some_and(is_word_char);
        if after_word && before_break {
            let o = out.get_or_insert_with(|| String::with_capacity(text.len()));
            o.push_str(&text[copied..i]);
            o.push_str("s'");
            copied = i + 3;
            from = i + 3;
        } else {
            // 's' is one byte, so the next char starts right after it.
            from = i + 1;
        }
    }
    match out {
        None => Cow::Borrowed(text),
        Some(mut o) => {
            o.push_str(&text[copied..]);
            Cow::Owned(o)
        }
    }
}

/// Whether `text[start..end]` stands as a whole word: no word character just before or just
/// after it (`(?<!\w)` and `(?!\w)`).
fn is_whole_word(text: &str, start: usize, end: usize) -> bool {
    !text[..start].chars().next_back().is_some_and(is_word_char)
        && !text[end..].chars().next().is_some_and(is_word_char)
}

/// Where one name occurs in a text as a whole word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WordMatch {
    /// Byte offset of the first character.
    pub start: usize,
    /// Byte offset just past the last character.
    pub end: usize,
    /// The name's position in the list the scanner was built from.
    pub name: usize,
}

/// Why a [`NameScanner`] could not be built.
#[derive(Clone, Debug, thiserror::Error)]
#[error("cannot build the name scanner: {0}")]
pub struct ScanError(String);

/// Finds many names at once in a text, each as a whole word.
///
/// The same answer as Python's `(?<!\w)(?:n1|n2|...)(?!\w)` with the names sorted longest first
/// (`game.py:837-838`): scanning left to right, at the first position where some name stands as
/// a whole word, the longest such name wins, and the scan resumes after it. Two identical names
/// resolve to the one listed first. Empty names never match.
#[derive(Clone, Debug)]
pub struct NameScanner {
    ac: Option<AhoCorasick>,
    /// The caller's index of each pattern the automaton holds.
    ids: Vec<usize>,
}

impl NameScanner {
    /// A scanner for these names. Match results refer to them by position.
    pub fn new<I, S>(names: I) -> Result<Self, ScanError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut ids = Vec::new();
        let mut patterns = Vec::new();
        for (i, name) in names.into_iter().enumerate() {
            let name = name.as_ref();
            if !name.is_empty() {
                ids.push(i);
                patterns.push(name.to_owned());
            }
        }
        if patterns.is_empty() {
            return Ok(Self { ac: None, ids });
        }
        let ac = AhoCorasick::builder()
            .match_kind(MatchKind::Standard)
            .build(&patterns)
            .map_err(|e| ScanError(e.to_string()))?;
        Ok(Self { ac: Some(ac), ids })
    }

    /// Every match in `text`, in order, none overlapping.
    #[must_use]
    pub fn find_all(&self, text: &str) -> Vec<WordMatch> {
        let Some(ac) = &self.ac else { return Vec::new() };
        // The non-panicking form: overlapping search needs MatchKind::Standard, which `new`
        // always builds, so the error arm is never taken.
        let Ok(matches) = ac.try_find_overlapping_iter(text) else { return Vec::new() };
        // A match of a whole UTF-8 pattern in UTF-8 text starts and ends on character boundaries,
        // so slicing at its offsets is safe.
        let mut found: Vec<WordMatch> = matches
            .filter(|m| is_whole_word(text, m.start(), m.end()))
            .filter_map(|m| {
                let name = *self.ids.get(m.pattern().as_usize())?;
                Some(WordMatch { start: m.start(), end: m.end(), name })
            })
            .collect();
        // Earliest first; at one position the longest; among identical names the first listed.
        found.sort_by(|a, b| {
            a.start.cmp(&b.start).then(b.end.cmp(&a.end)).then(a.name.cmp(&b.name))
        });
        let mut out = Vec::with_capacity(found.len());
        let mut pos = 0;
        for m in found {
            if m.start >= pos {
                pos = m.end;
                out.push(m);
            }
        }
        out
    }
}

/// Every place `name` occurs in `text` as a whole word, as byte ranges in order
/// (`re.finditer(rf"(?<!\w){re.escape(name)}(?!\w)", text)`, `game.py:854`). An empty name
/// never matches.
#[must_use]
pub fn find_word(text: &str, name: &str) -> Vec<core::ops::Range<usize>> {
    let mut out = Vec::new();
    if name.is_empty() {
        return out;
    }
    let mut from = 0;
    while let Some(off) = text[from..].find(name) {
        let start = from + off;
        let end = start + name.len();
        if is_whole_word(text, start, end) {
            out.push(start..end);
            from = end;
        } else {
            // Step over one character and look again: a later start may overlap this one.
            from = start + text[start..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

/// What [`scrub_coords`] puts in place of coordinates.
pub const UNKNOWN_LOCATION: &str = "an unknown location";

/// The byte length of the coordinates `(x, y)` at the start of `s`, if there are some there:
/// `\(-?\d+,\s*-?\d+\)`.
fn coords_len(s: &str) -> Option<usize> {
    let mut chars = s.char_indices().peekable();
    chars.next().filter(|&(_, c)| c == '(')?;
    let number = |chars: &mut core::iter::Peekable<core::str::CharIndices<'_>>| {
        chars.next_if(|&(_, c)| c == '-');
        let mut digits = 0;
        while chars.next_if(|&(_, c)| c.is_ascii_digit()).is_some() {
            digits += 1;
        }
        (digits > 0).then_some(())
    };
    number(&mut chars)?;
    chars.next().filter(|&(_, c)| c == ',')?;
    while chars.next_if(|&(_, c)| is_space(c)).is_some() {}
    number(&mut chars)?;
    let (i, c) = chars.next()?;
    (c == ')').then_some(i + 1)
}

/// Replaces every "(x, y)" coordinate pair with [`UNKNOWN_LOCATION`] (`game.py:18`, `967`).
#[must_use]
pub fn scrub_coords(text: &str) -> Cow<'_, str> {
    let mut out: Option<String> = None;
    let mut copied = 0;
    let mut from = 0;
    while let Some(off) = text[from..].find('(') {
        let i = from + off;
        match coords_len(&text[i..]) {
            Some(len) => {
                let o = out.get_or_insert_with(|| String::with_capacity(text.len()));
                o.push_str(&text[copied..i]);
                o.push_str(UNKNOWN_LOCATION);
                copied = i + len;
                from = i + len;
            }
            // '(' is one byte.
            None => from = i + 1,
        }
    }
    match out {
        None => Cow::Borrowed(text),
        Some(mut o) => {
            o.push_str(&text[copied..]);
            Cow::Owned(o)
        }
    }
}

/// The code point index of byte offset `byte` in `text`: Python's string index for the same
/// place. An offset inside a character counts that character as before it.
#[must_use]
pub fn char_offset(text: &str, byte: usize) -> usize {
    text.char_indices().take_while(|&(i, _)| i < byte).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_matches_rules_py() {
        assert_eq!(norm("Great Wall of China"), "greatwallofchina");
        assert_eq!(norm("great_wall-of china"), "greatwallofchina");
        assert_eq!(norm("Kraków 2"), "krakw2");
        assert_eq!(norm("\u{212A}elvin \u{130}"), "kelvini");
        assert_eq!(norm(""), "");
    }

    #[test]
    fn possessives() {
        assert_eq!(possessive_s("The Aztecs's Warrior"), "The Aztecs' Warrior");
        assert_eq!(possessive_s("Aztecs's"), "Aztecs'");
        assert_eq!(possessive_s("s's at the start"), "s's at the start");
        assert_eq!(possessive_s("Aztecs'sword"), "Aztecs'sword");
        assert!(matches!(possessive_s("nothing here"), Cow::Borrowed(_)));
        assert_eq!(possessive_s("Bs's Cs's"), "Bs' Cs'");
    }

    #[test]
    fn scanner_takes_the_longest_whole_word() {
        let names = ["Rome", "Roman Empire", "Rom", "", "Athens"];
        let s = NameScanner::new(names).expect("builds");
        let text = "Roman Empire took Rome, not Romeo; Athens_x and (Athens).";
        let found: Vec<(&str, usize)> =
            s.find_all(text).iter().map(|m| (&text[m.start..m.end], m.name)).collect();
        assert_eq!(found, [("Roman Empire", 1), ("Rome", 0), ("Athens", 4)]);
        assert!(NameScanner::new([""]).expect("builds").find_all("x").is_empty());
    }

    #[test]
    fn single_word_search_steps_over_failures() {
        assert_eq!(find_word("aaa", "aa"), Vec::<core::ops::Range<usize>>::new());
        assert_eq!(find_word("Kraków and Kraków", "Kraków"), [0..7, 12..19]);
        let found = find_word("x.a.a", "a.a");
        assert_eq!((found.len(), found.first()), (1, Some(&(2..5))));
    }

    #[test]
    fn coordinates_are_scrubbed() {
        assert_eq!(
            scrub_coords("Spotted at (12, -7) and (3,4) but not (a, 1) or (1,)"),
            "Spotted at an unknown location and an unknown location but not (a, 1) or (1,)"
        );
        assert_eq!(scrub_coords("((1,\t 2))"), "(an unknown location)");
        assert_eq!(scrub_coords("(-, 1)"), "(-, 1)");
    }

    #[test]
    fn char_offsets_count_code_points() {
        let text = "Kraków is here";
        let byte = text.find("is").expect("present");
        assert_eq!(char_offset(text, byte), 7);
    }
}
