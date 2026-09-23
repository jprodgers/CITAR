//! The path grammar shared by reports, `intended.toml`, `enforced.toml` and rule scripts
//! (DESIGN.md 9.2, "Path grammar").
//!
//! A path names a place in a group's answer, from its root. Segments:
//!
//! | Segment | Matches |
//! |---|---|
//! | `.key`, or `key` first | the object key `key` (letters, digits, `_` and `-`) |
//! | `["any key"]` | any object key, written as a JSON string |
//! | `[3]` | index 3 of a list compared in order, or of a multiset |
//! | `[pid=0]` | the element of a list keyed by field `pid` whose key is 0 |
//! | `[#0=12]` | the element of a list keyed by tuple position 0 whose key is 12 |
//! | `[*]` | any element of any list |
//! | `.*` | any object key |
//! | `.**` | any number of segments, none included |
//!
//! A selector value is a JSON literal, or a bare word for a string: `[type=Warrior]` is
//! `[type="Warrior"]`. Numbers compare by value, so `[pid=0]` and `[pid=0.0]` are the same.
//!
//! Reports print *concrete* paths ([`Path`]): only keys, indexes and key selectors, such as
//! `civs[pid=0].happiness.breakdown.Religion`. Which selector a list gets comes from the group's
//! [`CompareSpec`](super::CompareSpec): a keyed list prints `[pid=0]`, any other list `[3]`. So
//! `[pid=0]` in a pattern matches only a list the spec keys by `pid`, and `[3]` only a list it
//! does not key.

use std::fmt;

use serde_json::{Number, Value};

use super::value::{json_string, number_text, same_number};
use crate::{Error, Result};

/// A JSON scalar in a selector.
#[derive(Debug, Clone)]
pub enum Scalar {
    Null,
    Bool(bool),
    Num(Number),
    Str(String),
}

impl Scalar {
    /// The scalar a JSON value is, if it is one.
    pub fn from_value(v: &Value) -> Option<Scalar> {
        match v {
            Value::Null => Some(Scalar::Null),
            Value::Bool(b) => Some(Scalar::Bool(*b)),
            Value::Number(n) => Some(Scalar::Num(n.clone())),
            Value::String(s) => Some(Scalar::Str(s.clone())),
            Value::Array(_) | Value::Object(_) => None,
        }
    }

    /// Text that is equal for equal scalars, and different for different ones.
    pub fn canonical(&self) -> String {
        match self {
            Scalar::Null => "null".into(),
            Scalar::Bool(b) => b.to_string(),
            Scalar::Num(n) => number_text(n),
            Scalar::Str(s) => json_string(s),
        }
    }

    /// An unquoted selector value: a JSON literal, or else the text itself as a string.
    fn from_token(token: &str) -> Scalar {
        match token {
            "null" => return Scalar::Null,
            "true" => return Scalar::Bool(true),
            "false" => return Scalar::Bool(false),
            _ => {}
        }
        if let Some(n) = number_token(token) {
            return Scalar::Num(n);
        }
        Scalar::Str(token.to_string())
    }
}

/// A token that is a JSON number. Rust's float parser also takes `inf` and `NaN`, which are
/// strings here.
fn number_token(token: &str) -> Option<Number> {
    let numeric = token.starts_with(|c: char| c.is_ascii_digit() || c == '-')
        && token.chars().all(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'));
    if !numeric {
        return None;
    }
    if let Ok(i) = token.parse::<i64>() {
        return Some(Number::from(i));
    }
    if let Ok(u) = token.parse::<u64>() {
        return Some(Number::from(u));
    }
    token.parse::<f64>().ok().and_then(Number::from_f64)
}

impl PartialEq for Scalar {
    fn eq(&self, other: &Scalar) -> bool {
        match (self, other) {
            (Scalar::Null, Scalar::Null) => true,
            (Scalar::Bool(a), Scalar::Bool(b)) => a == b,
            (Scalar::Num(a), Scalar::Num(b)) => same_number(a, b),
            (Scalar::Str(a), Scalar::Str(b)) => a == b,
            _ => false,
        }
    }
}

impl fmt::Display for Scalar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scalar::Str(s) if is_bare(s) && matches!(Scalar::from_token(s), Scalar::Str(_)) => {
                f.write_str(s)
            }
            other => f.write_str(&other.canonical()),
        }
    }
}

/// A key or string that needs no quotes.
fn is_bare(s: &str) -> bool {
    !s.is_empty() && s.chars().all(is_bare_char)
}

fn is_bare_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// One segment of a concrete path.
#[derive(Debug, Clone, PartialEq)]
pub enum Seg {
    Key(String),
    Index(usize),
    /// An element of a list keyed by a field.
    Field(String, Scalar),
    /// An element of a list keyed by a tuple position.
    Pos(usize, Scalar),
}

/// A concrete place in an answer, as reports print it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Path(pub Vec<Seg>);

impl Path {
    pub fn root() -> Path {
        Path(Vec::new())
    }

    pub fn segs(&self) -> &[Seg] {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("(root)");
        }
        for (i, seg) in self.0.iter().enumerate() {
            match seg {
                Seg::Key(k) => write_key(f, k, i == 0)?,
                Seg::Index(n) => write!(f, "[{n}]")?,
                Seg::Field(name, v) => write!(f, "[{name}={v}]")?,
                Seg::Pos(n, v) => write!(f, "[#{n}={v}]")?,
            }
        }
        Ok(())
    }
}

fn write_key(f: &mut fmt::Formatter<'_>, key: &str, first: bool) -> fmt::Result {
    if !is_bare(key) {
        return write!(f, "[{}]", json_string(key));
    }
    if !first {
        f.write_str(".")?;
    }
    f.write_str(key)
}

/// One segment of a pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum PSeg {
    Key(String),
    Index(usize),
    AnyIndex,
    Field(String, Scalar),
    Pos(usize, Scalar),
    AnyKey,
    /// `.**`: zero or more segments of any kind.
    AnyDepth,
}

impl PSeg {
    /// Whether this segment matches one concrete segment. [`PSeg::AnyDepth`] is handled by the
    /// matcher, which lets it absorb any number of them.
    fn matches(&self, seg: &Seg) -> bool {
        match (self, seg) {
            (PSeg::Key(a), Seg::Key(b)) => a == b,
            (PSeg::AnyKey, Seg::Key(_)) => true,
            (PSeg::Index(a), Seg::Index(b)) => a == b,
            (PSeg::AnyIndex, Seg::Index(_) | Seg::Field(..) | Seg::Pos(..)) => true,
            (PSeg::Field(a, x), Seg::Field(b, y)) => a == b && x == y,
            (PSeg::Pos(a, x), Seg::Pos(b, y)) => a == b && x == y,
            (PSeg::AnyDepth, _) => true,
            _ => false,
        }
    }
}

/// A path pattern, as written in `intended.toml`, `enforced.toml` or a compare spec. It matches a
/// concrete path when it matches all of it, not just a prefix: `a.b` does not match `a.b.c`, and
/// `a.b.**` matches both.
#[derive(Debug, Clone)]
pub struct Pattern {
    segs: Vec<PSeg>,
    text: String,
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Pattern) -> bool {
        self.segs == other.segs
    }
}

impl Pattern {
    pub fn parse(text: &str) -> Result<Pattern> {
        let segs =
            Parser { s: text, i: 0 }.parse().map_err(|e| e.context(format!("path `{text}`")))?;
        Ok(Pattern { segs, text: text.to_string() })
    }

    /// The pattern that matches exactly this concrete path.
    pub fn exact(path: &Path) -> Pattern {
        let segs = path
            .segs()
            .iter()
            .map(|s| match s {
                Seg::Key(k) => PSeg::Key(k.clone()),
                Seg::Index(n) => PSeg::Index(*n),
                Seg::Field(f, v) => PSeg::Field(f.clone(), v.clone()),
                Seg::Pos(n, v) => PSeg::Pos(*n, v.clone()),
            })
            .collect();
        Pattern::from_segs(segs)
    }

    /// The pattern that matches this concrete path with every list element as `[*]`: the form
    /// `suggest` writes, since the same difference usually recurs in every element.
    pub fn generalize(path: &Path) -> Pattern {
        let segs = path
            .segs()
            .iter()
            .map(|s| match s {
                Seg::Key(k) => PSeg::Key(k.clone()),
                Seg::Index(_) | Seg::Field(..) | Seg::Pos(..) => PSeg::AnyIndex,
            })
            .collect();
        Pattern::from_segs(segs)
    }

    fn from_segs(segs: Vec<PSeg>) -> Pattern {
        let text = render(&segs);
        Pattern { segs, text }
    }

    pub fn segs(&self) -> &[PSeg] {
        &self.segs
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Whether the pattern reaches across keys (`.*`) or depths (`.**`). An intended entry with
    /// such a path must say `broad = true`: it can hide more than the one difference it was
    /// written for.
    pub fn is_broad(&self) -> bool {
        self.segs.iter().any(|s| matches!(s, PSeg::AnyKey | PSeg::AnyDepth))
    }

    /// Whether every path this pattern matches lies under `prefix`, judged segment by segment:
    /// `deals[*].bot_value.**` and `deals[3].bot_value` start with `deals[*].bot_value`.
    pub fn starts_with(&self, prefix: &Pattern) -> bool {
        prefix.segs.len() <= self.segs.len()
            && !prefix.segs.contains(&PSeg::AnyDepth)
            && prefix.segs.iter().zip(&self.segs).all(|(p, s)| match (p, s) {
                (PSeg::AnyIndex, PSeg::Index(_) | PSeg::Field(..) | PSeg::Pos(..)) => true,
                (PSeg::AnyKey, PSeg::Key(_)) => true,
                _ => p == s,
            })
    }

    pub fn matches(&self, path: &Path) -> bool {
        let set = std::slice::from_ref(self);
        accepted(set, &walk(set, path)).next().is_some()
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

fn render(segs: &[PSeg]) -> String {
    let mut out = String::new();
    for (i, seg) in segs.iter().enumerate() {
        let dot = if i == 0 { "" } else { "." };
        out.push_str(&match seg {
            PSeg::Key(k) if is_bare(k) => format!("{dot}{k}"),
            PSeg::Key(k) => format!("[{}]", json_string(k)),
            PSeg::Index(n) => format!("[{n}]"),
            PSeg::AnyIndex => "[*]".to_string(),
            PSeg::Field(name, v) => format!("[{name}={v}]"),
            PSeg::Pos(n, v) => format!("[#{n}={v}]"),
            PSeg::AnyKey => format!("{dot}*"),
            PSeg::AnyDepth => format!("{dot}**"),
        });
    }
    out
}

/// Where the matcher stands in each pattern of a set: `(pattern, next segment)` pairs, sorted and
/// without duplicates. It is a small NFA, so `.**` costs nothing until it is used.
pub type Cursor = Vec<(u32, u32)>;

fn index(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

/// The cursor at the root of an answer.
pub fn start(patterns: &[Pattern]) -> Cursor {
    let mut states: Cursor = (0..patterns.len()).map(|p| (index(p), 0)).collect();
    close(patterns, &mut states);
    states
}

/// The cursor one segment further down.
pub fn step(patterns: &[Pattern], cursor: &Cursor, seg: &Seg) -> Cursor {
    step_where(patterns, cursor, |want| want.matches(seg))
}

/// The cursor one list element further down, when the element's selector is not known (an
/// index or a key): every element selector matches. It over-approximates, which is what
/// [`matches_inside`] needs.
pub fn step_element(patterns: &[Pattern], cursor: &Cursor) -> Cursor {
    step_where(patterns, cursor, |want| {
        matches!(want, PSeg::Index(_) | PSeg::AnyIndex | PSeg::Field(..) | PSeg::Pos(..))
    })
}

fn step_where(patterns: &[Pattern], cursor: &Cursor, hit: impl Fn(&PSeg) -> bool) -> Cursor {
    let mut next = Cursor::new();
    for &(p, i) in cursor {
        let Some(want) = patterns[p as usize].segs.get(i as usize) else { continue };
        match want {
            PSeg::AnyDepth => next.push((p, i)),
            other if hit(other) => next.push((p, i + 1)),
            _ => {}
        }
    }
    close(patterns, &mut next);
    next
}

/// The cursor at the end of a concrete path; empty as soon as no pattern can match any more.
pub fn walk(patterns: &[Pattern], path: &Path) -> Cursor {
    let mut cursor = start(patterns);
    for seg in path.segs() {
        if cursor.is_empty() {
            break;
        }
        cursor = step(patterns, &cursor, seg);
    }
    cursor
}

/// Whether some place strictly inside `value`, which stands where the cursor stands, matches a
/// pattern of the set. List elements are taken as any selector, so a pattern that names one
/// keyed element (`units[id=5]`) matches inside every list of units: it may say yes for a place
/// the value lacks, never no for one it has.
pub fn matches_inside(patterns: &[Pattern], cursor: &Cursor, value: &Value) -> bool {
    let below = |next: &Cursor, child: &Value| {
        !next.is_empty()
            && (accepted(patterns, next).next().is_some() || matches_inside(patterns, next, child))
    };
    match value {
        Value::Object(map) => {
            map.iter().any(|(k, child)| below(&step(patterns, cursor, &Seg::Key(k.clone())), child))
        }
        Value::Array(items) => {
            let next = step_element(patterns, cursor);
            items.iter().any(|child| below(&next, child))
        }
        _ => false,
    }
}

/// Adds the states a `.**` reaches by matching nothing, then sorts and dedups.
fn close(patterns: &[Pattern], states: &mut Cursor) {
    let mut k = 0;
    while k < states.len() {
        let (p, i) = states[k];
        if patterns[p as usize].segs.get(i as usize) == Some(&PSeg::AnyDepth) {
            states.push((p, i + 1));
        }
        k += 1;
    }
    states.sort_unstable();
    states.dedup();
}

/// The patterns that match the path the cursor stands at, in the set's order.
pub fn accepted<'a>(
    patterns: &'a [Pattern],
    cursor: &'a Cursor,
) -> impl Iterator<Item = usize> + 'a {
    cursor
        .iter()
        .filter(|&&(p, i)| i as usize == patterns[p as usize].segs.len())
        .map(|&(p, _)| p as usize)
}

struct Parser<'a> {
    s: &'a str,
    i: usize,
}

impl Parser<'_> {
    fn parse(mut self) -> Result<Vec<PSeg>> {
        let mut segs = Vec::new();
        if self.s.is_empty() {
            return Ok(segs);
        }
        // The first segment may leave out its dot.
        if !self.s.starts_with(['.', '[']) {
            segs.push(self.after_dot()?);
        }
        while let Some(c) = self.peek() {
            match c {
                '.' => {
                    self.i += 1;
                    segs.push(self.after_dot()?);
                }
                '[' => segs.push(self.bracket()?),
                c => return Err(self.error(&format!("expected `.` or `[`, found `{c}`"))),
            }
        }
        Ok(segs)
    }

    fn peek(&self) -> Option<char> {
        self.s[self.i..].chars().next()
    }

    fn eat(&mut self, token: &str) -> bool {
        if self.s[self.i..].starts_with(token) {
            self.i += token.len();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, token: &str) -> Result<()> {
        if self.eat(token) { Ok(()) } else { Err(self.error(&format!("expected `{token}`"))) }
    }

    fn error(&self, what: &str) -> Error {
        Error::new(format!("{what} at column {}", self.i + 1))
    }

    fn after_dot(&mut self) -> Result<PSeg> {
        if self.eat("**") {
            return Ok(PSeg::AnyDepth);
        }
        if self.eat("*") {
            return Ok(PSeg::AnyKey);
        }
        Ok(PSeg::Key(self.bare()?))
    }

    fn bare(&mut self) -> Result<String> {
        let rest = &self.s[self.i..];
        let len = rest.find(|c: char| !is_bare_char(c)).unwrap_or(rest.len());
        if len == 0 {
            return Err(self.error("expected a key (letters, digits, `_` or `-`; quote others)"));
        }
        self.i += len;
        Ok(rest[..len].to_string())
    }

    fn bracket(&mut self) -> Result<PSeg> {
        self.expect("[")?;
        if self.peek() == Some('"') {
            let key = self.json_string()?;
            self.expect("]")?;
            return Ok(PSeg::Key(key));
        }
        if self.eat("*]") {
            return Ok(PSeg::AnyIndex);
        }
        if self.eat("#") {
            let pos = self.digits()?;
            self.expect("=")?;
            let value = self.value()?;
            self.expect("]")?;
            return Ok(PSeg::Pos(pos, value));
        }
        let name = self.bare()?;
        if self.eat("]") {
            return name.parse::<usize>().map(PSeg::Index).map_err(|_| {
                self.error(&format!("`[{name}]` is not an index; did you mean `.{name}`?"))
            });
        }
        self.expect("=")?;
        let value = self.value()?;
        self.expect("]")?;
        Ok(PSeg::Field(name, value))
    }

    fn digits(&mut self) -> Result<usize> {
        let rest = &self.s[self.i..];
        let len = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        let n = rest[..len].parse::<usize>().map_err(|_| self.error("expected a position"))?;
        self.i += len;
        Ok(n)
    }

    fn value(&mut self) -> Result<Scalar> {
        if self.peek() == Some('"') {
            return Ok(Scalar::Str(self.json_string()?));
        }
        let rest = &self.s[self.i..];
        let len = rest.find(']').ok_or_else(|| self.error("expected `]`"))?;
        if len == 0 {
            return Err(self.error("expected a value"));
        }
        self.i += len;
        Ok(Scalar::from_token(&rest[..len]))
    }

    fn json_string(&mut self) -> Result<String> {
        let rest = &self.s[self.i..];
        let mut escaped = false;
        let mut end = None;
        for (k, c) in rest.char_indices().skip(1) {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => {
                    end = Some(k);
                    break;
                }
                _ => {}
            }
        }
        let end = end.ok_or_else(|| self.error("unterminated string"))?;
        let text = serde_json::from_str::<String>(&rest[..=end])
            .map_err(|e| self.error(&format!("bad string: {e}")))?;
        self.i += end + 1;
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Pattern {
        Pattern::parse(s).unwrap_or_else(|e| panic!("{e}"))
    }

    fn key(k: &str) -> Seg {
        Seg::Key(k.into())
    }

    fn field(f: &str, v: i64) -> Seg {
        Seg::Field(f.into(), Scalar::Num(v.into()))
    }

    #[test]
    fn parses_every_segment_kind() {
        let pat = p(r#"civs[pid=0].x["Tile yields"][3][*][#0=12].*.**[type=Warrior]"#);
        assert_eq!(
            pat.segs(),
            [
                PSeg::Key("civs".into()),
                PSeg::Field("pid".into(), Scalar::Num(0.into())),
                PSeg::Key("x".into()),
                PSeg::Key("Tile yields".into()),
                PSeg::Index(3),
                PSeg::AnyIndex,
                PSeg::Pos(0, Scalar::Num(12.into())),
                PSeg::AnyKey,
                PSeg::AnyDepth,
                PSeg::Field("type".into(), Scalar::Str("Warrior".into())),
            ]
        );
        assert_eq!(p("**").segs(), [PSeg::AnyDepth]);
        assert_eq!(p(".a").segs(), [PSeg::Key("a".into())]);
        assert_eq!(p("[3]").segs(), [PSeg::Index(3)]);
        assert!(p("").segs().is_empty());
        assert_eq!(p("a[x=null][y=true][z=\"3\"][w=2.5]").segs().len(), 5);
        assert_eq!(p("a[z=\"3\"]").segs()[1], PSeg::Field("z".into(), Scalar::Str("3".into())));
    }

    #[test]
    fn rejects_malformed_paths() {
        for bad in ["a..b", "a[", "a[1", "a[x=]", "a b", "a[\"x]", "a[#x=1]", "a[x]", "a.", "[]"] {
            assert!(Pattern::parse(bad).is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn concrete_paths_print_in_the_grammar_and_parse_back() {
        let path = Path(vec![
            key("civs"),
            field("pid", 0),
            key("happiness"),
            key("breakdown"),
            key("Luxury resources"),
            Seg::Index(2),
            Seg::Pos(0, Scalar::Num(12.into())),
            Seg::Field("type".into(), Scalar::Str("Great General".into())),
            Seg::Field("name".into(), Scalar::Str("3".into())),
        ]);
        let text = path.to_string();
        assert_eq!(
            text,
            r#"civs[pid=0].happiness.breakdown["Luxury resources"][2][#0=12][type="Great General"][name="3"]"#
        );
        assert!(p(&text).matches(&path), "a printed path matches itself");
        assert!(Pattern::exact(&path).matches(&path));
        assert_eq!(Path::root().to_string(), "(root)");
    }

    #[test]
    fn matching_is_whole_path() {
        let path = Path(vec![key("a"), key("b"), key("c")]);
        assert!(p("a.b.c").matches(&path));
        assert!(!p("a.b").matches(&path), "a prefix is not a match");
        assert!(p("a.b.**").matches(&path));
        assert!(p("a.b.c.**").matches(&path), "** matches nothing too");
        assert!(p("**.c").matches(&path));
        assert!(p("a.*.c").matches(&path));
        assert!(!p("a.*").matches(&path));
        assert!(p("**").matches(&path));
        assert!(!p("a.b.c.d").matches(&path));
    }

    #[test]
    fn selectors_match_by_kind_and_value() {
        let keyed = Path(vec![key("civs"), field("pid", 0), key("x")]);
        let indexed = Path(vec![key("civs"), Seg::Index(0), key("x")]);
        assert!(p("civs[pid=0].x").matches(&keyed));
        assert!(p("civs[pid=0.0].x").matches(&keyed), "numbers compare by value");
        assert!(!p("civs[pid=1].x").matches(&keyed));
        assert!(!p("civs[pid=\"0\"].x").matches(&keyed), "a string is not a number");
        assert!(!p("civs[0].x").matches(&keyed), "an index does not match a keyed element");
        assert!(p("civs[0].x").matches(&indexed));
        assert!(!p("civs[pid=0].x").matches(&indexed));
        assert!(p("civs[*].x").matches(&keyed) && p("civs[*].x").matches(&indexed));
        assert!(!p("civs.*.x").matches(&indexed), ".* is an object key, not a list element");
        assert!(p("civs.**").matches(&indexed));
    }

    #[test]
    fn broadness_generalizing_and_prefixes() {
        assert!(!p("cities[*].stats.total.gold").is_broad());
        assert!(p("cities[*].stats.*").is_broad());
        assert!(p("cities.**").is_broad());
        let path = Path(vec![key("cities"), field("id", 9), key("workable"), Seg::Index(3)]);
        assert_eq!(Pattern::generalize(&path).as_str(), "cities[*].workable[*]");
        assert!(p("deals[*].bot_value.**").starts_with(&p("deals[*].bot_value")));
        assert!(p("deals[3].bot_value").starts_with(&p("deals[*].bot_value")));
        assert!(!p("deals.**").starts_with(&p("deals[*].bot_value")));
        assert!(!p("deals[*].valid").starts_with(&p("deals[*].bot_value")));
        assert!(!p("deals").starts_with(&p("deals[*].bot_value")));
    }

    #[test]
    fn a_set_of_patterns_runs_as_one_matcher() {
        let set = [p("a[*].b"), p("a.**"), p("x")];
        let root = start(&set);
        let a = step(&set, &root, &key("a"));
        assert_eq!(accepted(&set, &a).collect::<Vec<_>>(), [1]);
        let a0 = step(&set, &a, &Seg::Index(0));
        let a0b = step(&set, &a0, &key("b"));
        assert_eq!(accepted(&set, &a0b).collect::<Vec<_>>(), [0, 1]);
        let x = step(&set, &root, &key("x"));
        assert_eq!(accepted(&set, &x).collect::<Vec<_>>(), [2]);
        let dead = step(&set, &x, &key("y"));
        assert!(dead.is_empty());
        assert!(walk(&set, &Path(vec![key("x"), key("y"), key("z")])).is_empty());
        assert_eq!(accepted(&set, &walk(&set, &Path(vec![key("a"), key("q")]))).count(), 1);
    }

    #[test]
    fn a_value_can_hold_a_place_below_where_it_stands() {
        let set = [p("civs[*].units[id=5].hp"), p("world.**.gold")];
        let civs = walk(&set, &Path(vec![key("civs")]));
        let civ = serde_json::json!({"pid": 0, "units": [{"id": 7, "hp": 3}]});
        // Any element selector matches inside a value, so `[id=5]` meets the unit with id 7.
        assert!(matches_inside(&set, &step_element(&set, &civs), &civ));
        let bare = serde_json::json!({"pid": 0, "units": []});
        assert!(!matches_inside(&set, &step_element(&set, &civs), &bare), "no unit, no hp");
        let world = walk(&set, &Path(vec![key("world")]));
        let deep = serde_json::json!({"a": {"b": [{"gold": 1}]}});
        assert!(matches_inside(&set, &world, &deep));
        assert!(!matches_inside(&set, &world, &serde_json::json!({"a": {"b": [1, 2]}})));
        assert!(!matches_inside(&set, &world, &serde_json::json!(3)), "a scalar holds nothing");
    }
}
