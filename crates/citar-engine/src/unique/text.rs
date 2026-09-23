//! Splitting a unique's text into its parts, as UnCiv writes them: `uniques.py:28-90`.
//!
//! A unique such as `[+20]% Strength <for [Mounted] units> <when attacking>` has a main text and
//! modifiers (`<...>`), and the main text has a *placeholder* (`[]% Strength`), which names its
//! type, and *parameters* (`+20`). These functions do the splitting and nothing else: they never
//! fail, exactly like Python's, and leave judging the pieces to the compiler (`unique::compile`),
//! which calls them for every text and modifier, and `parse_stats` for every `[stats]`
//! parameter.
//!
//! The brackets and angle brackets are ASCII, so scanning bytes finds the same places Python's
//! scan of code points did, and every slice falls on a character boundary.

use crate::base::stats::{Stat, Stats};
use crate::base::text::is_space;

/// Splits `text` into its main text and its modifiers, both trimmed: `X <a> <b>` gives
/// `("X", ["a", "b"])` (`uniques.py:28-55`).
///
/// A `<` inside square brackets belongs to a parameter. A modifier runs to the first `>` that is
/// not inside square brackets, or to the end of the text if none closes it.
#[must_use]
pub fn split_modifiers(text: &str) -> (String, Vec<&str>) {
    let bytes = text.as_bytes();
    let n = bytes.len();
    let mut main = String::with_capacity(n);
    let mut mods = Vec::new();
    let mut depth_sq = 0usize;
    let mut i = 0;
    // The start of the run of main text not yet copied.
    let mut run = 0;
    while i < n {
        match bytes[i] {
            b'[' => depth_sq += 1,
            b']' => depth_sq = depth_sq.saturating_sub(1),
            _ => {}
        }
        if bytes[i] == b'<' && depth_sq == 0 {
            main.push_str(&text[run..i]);
            let mut j = i + 1;
            // Python's inner depth may go negative, and a `>` then closes at any depth <= 0.
            let mut d: i64 = 0;
            while j < n {
                match bytes[j] {
                    b'[' => d += 1,
                    b']' => d -= 1,
                    b'>' if d <= 0 => break,
                    _ => {}
                }
                j += 1;
            }
            mods.push(trim(&text[i + 1..j]));
            i = j + 1;
            run = i.min(n);
            continue;
        }
        i += 1;
    }
    if run < n {
        main.push_str(&text[run..]);
    }
    let trimmed = trim(&main);
    let main = if trimmed.len() == main.len() { main } else { trimmed.to_owned() };
    (main, mods)
}

/// The placeholder and the parameters of a main text: the top-level `[...]` groups become `[]`
/// and their contents the parameters, `[+20]% Strength` giving `("[]% Strength", ["+20"])`
/// (`uniques.py:58-76`).
///
/// Brackets nest; only the outermost pair makes a parameter, whose text keeps any inner
/// brackets. A `]` with no `[` open is ordinary text. An unclosed `[` swallows the rest of the
/// text and makes no parameter.
#[must_use]
pub fn placeholder(text: &str) -> (String, Vec<&str>) {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut params = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    // The start of the run of placeholder text not yet copied.
    let mut run = 0;
    for (i, &c) in bytes.iter().enumerate() {
        if c == b'[' {
            if depth == 0 {
                out.push_str(&text[run..i]);
                out.push_str("[]");
                start = i + 1;
            }
            depth += 1;
        } else if c == b']' && depth > 0 {
            depth -= 1;
            if depth == 0 {
                params.push(&text[start..i]);
                run = i + 1;
            }
        }
    }
    if depth == 0 {
        out.push_str(&text[run..]);
    }
    (out, params)
}

/// The placeholder, the parameters and the modifiers of a whole unique text: what Python's
/// `Unique.__init__` computed first (`uniques.py:120-122`).
#[must_use]
pub fn parts(text: &str) -> Parts {
    let (main, mods) = split_modifiers(text);
    let (placeholder, params) = placeholder(&main);
    let params = params.into_iter().map(str::to_owned).collect();
    Parts { placeholder, params, modifiers: mods.into_iter().map(str::to_owned).collect() }
}

/// A unique text taken apart by [`parts`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parts {
    /// The main text with each top-level parameter written `[]`: the unique's type.
    pub placeholder: String,
    /// The parameters of the main text, in order.
    pub params: Vec<String>,
    /// The modifiers, the text inside each `<...>`, in order.
    pub modifiers: Vec<String>,
}

impl Parts {
    /// The first parameter that reads as stats, as Python's `Unique.stats` found it
    /// (`uniques.py:133-137`).
    #[must_use]
    pub fn stats(&self) -> Option<Stats> {
        self.params.iter().find_map(|p| parse_stats(p))
    }
}

/// Reads `+1 Food, +2 Gold` as stats; `None` unless every comma-separated part is a number, one
/// space, and a stat name as UnCiv writes it (`uniques.py:79-90`).
///
/// The number is read by Rust's float parser, which, like Python's `float`, takes a sign and an
/// exponent; unlike it, Rust refuses `1_000`, which no ruleset writes.
#[must_use]
pub fn parse_stats(text: &str) -> Option<Stats> {
    let mut out = Stats::ZERO;
    for part in text.split(',') {
        let (amount, name) = trim(part).split_once(' ')?;
        let stat = Stat::from_name(name)?;
        let amount: f64 = trim(amount).parse().ok()?;
        out[stat] += amount;
    }
    Some(out)
}

/// `str.strip()`: Python's whitespace, which includes four control characters Rust's `trim`
/// leaves alone.
fn trim(s: &str) -> &str {
    s.trim_matches(is_space)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_split_off_outside_brackets() {
        let (main, mods) =
            split_modifiers("[+20]% Strength <for [Mounted] units> <when attacking>");
        assert_eq!(main, "[+20]% Strength");
        assert_eq!(mods, ["for [Mounted] units", "when attacking"]);
        let (main, mods) = split_modifiers("[a <b>] c");
        assert_eq!(main, "[a <b>] c");
        assert!(mods.is_empty());
        let (main, mods) = split_modifiers("x <unclosed [y");
        assert_eq!(main, "x");
        assert_eq!(mods, ["unclosed [y"]);
        let (main, mods) = split_modifiers("<only>");
        assert_eq!(main, "");
        assert_eq!(mods, ["only"]);
        // A `>` inside brackets does not close the modifier.
        let (_, mods) = split_modifiers("a <b [c>d] e> f");
        assert_eq!(mods, ["b [c>d] e"]);
    }

    #[test]
    fn placeholders_and_parameters() {
        assert_eq!(placeholder("[+20]% Strength"), ("[]% Strength".to_owned(), vec!["+20"]));
        assert_eq!(
            placeholder("[+1 Food] from [Fresh water] tiles"),
            ("[] from [] tiles".to_owned(), vec!["+1 Food", "Fresh water"])
        );
        assert_eq!(placeholder("[a [b] c] d"), ("[] d".to_owned(), vec!["a [b] c"]));
        assert_eq!(placeholder("a ] b"), ("a ] b".to_owned(), vec![]));
        assert_eq!(placeholder("x [unclosed"), ("x []".to_owned(), vec![]));
        assert_eq!(placeholder("Spaceship part"), ("Spaceship part".to_owned(), vec![]));
        assert_eq!(placeholder("Grand Canal [é]"), ("Grand Canal []".to_owned(), vec!["é"]));
    }

    #[test]
    fn parts_of_a_whole_unique() {
        let p =
            parts("[+1 Production] from [Hill] tiles [in this city] <after discovering [Mining]>");
        assert_eq!(p.placeholder, "[] from [] tiles []");
        assert_eq!(p.params, ["+1 Production", "Hill", "in this city"]);
        assert_eq!(p.modifiers, ["after discovering [Mining]"]);
        assert_eq!(p.stats(), Some(Stats::single(Stat::Production, 1.0)));
    }

    #[test]
    fn stats_parse_as_python_parsed_them() {
        let s = parse_stats("+1 Food, +2 Gold, -1 Food").expect("stats");
        assert_eq!(s, {
            let mut x = Stats::ZERO;
            x[Stat::Gold] = 2.0;
            x
        });
        assert_eq!(parse_stats("+1.5 Culture"), Some(Stats::single(Stat::Culture, 1.5)));
        assert_eq!(parse_stats("+1 food"), None);
        assert_eq!(parse_stats("+1  Food"), None);
        assert_eq!(parse_stats("Food"), None);
        assert_eq!(parse_stats("x Food"), None);
        assert_eq!(parse_stats(""), None);
        assert_eq!(parse_stats("+1 Food,"), None);
    }
}
