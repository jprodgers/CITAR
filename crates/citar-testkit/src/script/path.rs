//! Paths into a value: the concrete half of the path grammar refcheck, `intended.toml` and rule
//! scripts share (DESIGN.md 9.2).
//!
//! | Segment | Selects |
//! |---|---|
//! | `key` first, or `.key` | the object key `key` (letters, digits, `_` and `-`) |
//! | `["any key"]` | any object key, written as a JSON string |
//! | `[3]` | item 3 of a list |
//! | `[id=4]` | the first item of a list that is an object whose `id` is 4 |
//! | `[#0=12]` | the first item of a list that is a list whose item 0 is 12 |
//!
//! A selector value is a JSON literal, or a bare word for a string: `[type=Warrior]`.

use serde_json::Value;

use super::matchers::same;

/// One segment.
#[derive(Clone, Debug, PartialEq)]
pub enum Seg {
    Key(String),
    Index(usize),
    Field(String, Value),
    Pos(usize, Value),
}

/// Parses a whole path.
pub fn parse(text: &str) -> Result<Vec<Seg>, String> {
    let mut p = Parser { s: text, i: 0 };
    let segs = p.segments(true)?;
    if p.i < text.len() {
        return Err(format!("path `{text}`: unexpected `{}`", &text[p.i..]));
    }
    Ok(segs)
}

/// Parses the segments that follow a name inside an expression (`r.techs[0]`), from byte `at`;
/// returns them and where they end.
pub fn parse_tail(text: &str, at: usize) -> Result<(Vec<Seg>, usize), String> {
    let mut p = Parser { s: text, i: at };
    let segs = p.segments(false)?;
    Ok((segs, p.i))
}

/// The value at `segs`, or `None` where the path leads nowhere.
#[must_use]
pub fn get<'a>(v: &'a Value, segs: &[Seg]) -> Option<&'a Value> {
    let mut cur = v;
    for s in segs {
        cur = match (s, cur) {
            (Seg::Key(k), Value::Object(m)) => m.get(k)?,
            (Seg::Index(n), Value::Array(a)) => a.get(*n)?,
            (Seg::Field(f, want), Value::Array(a)) => {
                a.iter().find(|x| x.get(f).is_some_and(|got| same(got, want)))?
            }
            (Seg::Pos(n, want), Value::Array(a)) => a.iter().find(|x| {
                x.as_array().and_then(|t| t.get(*n)).is_some_and(|got| same(got, want))
            })?,
            _ => return None,
        };
    }
    Some(cur)
}

fn bare_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

struct Parser<'a> {
    s: &'a str,
    i: usize,
}

impl Parser<'_> {
    fn rest(&self) -> &str {
        &self.s[self.i..]
    }

    fn segments(&mut self, whole: bool) -> Result<Vec<Seg>, String> {
        let mut out = Vec::new();
        if whole && self.rest().starts_with(bare_char) {
            out.push(Seg::Key(self.bare()));
        }
        loop {
            if self.rest().starts_with('.') && self.rest()[1..].starts_with(bare_char) {
                self.i += 1;
                out.push(Seg::Key(self.bare()));
            } else if self.rest().starts_with('[') {
                out.push(self.bracket()?);
            } else {
                break;
            }
        }
        if whole && out.is_empty() && !self.s.is_empty() {
            return Err(format!("path `{}`: no segment", self.s));
        }
        Ok(out)
    }

    fn bare(&mut self) -> String {
        let len = self.rest().find(|c: char| !bare_char(c)).unwrap_or(self.rest().len());
        let out = self.rest()[..len].to_owned();
        self.i += len;
        out
    }

    fn bracket(&mut self) -> Result<Seg, String> {
        let body_start = self.i + 1;
        let close = self.find_close(body_start)?;
        let body = &self.s[body_start..close];
        self.i = close + 1;
        let err = || format!("path `{}`: bad segment `[{body}]`", self.s);
        if body.starts_with('"') {
            let key: String = serde_json::from_str(body).map_err(|_| err())?;
            return Ok(Seg::Key(key));
        }
        if let Ok(n) = body.parse::<usize>() {
            return Ok(Seg::Index(n));
        }
        let (name, value) = body.split_once('=').ok_or_else(err)?;
        let value = literal(value);
        if let Some(pos) = name.strip_prefix('#') {
            let n = pos.parse::<usize>().map_err(|_| err())?;
            return Ok(Seg::Pos(n, value));
        }
        if name.is_empty() || !name.chars().all(bare_char) {
            return Err(err());
        }
        Ok(Seg::Field(name.to_owned(), value))
    }

    /// The `]` closing the bracket whose body starts at `from`, skipping one inside a string.
    fn find_close(&self, from: usize) -> Result<usize, String> {
        let bytes = self.s.as_bytes();
        let mut in_str = false;
        let mut k = from;
        while k < bytes.len() {
            match bytes[k] {
                b'\\' if in_str => k += 1,
                b'"' => in_str = !in_str,
                b']' if !in_str => return Ok(k),
                _ => {}
            }
            k += 1;
        }
        Err(format!("path `{}`: `[` is never closed", self.s))
    }
}

/// A selector value: a JSON literal, or else the text as a string.
fn literal(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn segments_select_as_documented() -> Result<(), String> {
        let v = json!({"a": {"b c": [10, 20]}, "units": [{"id": 3, "t": "x"}, {"id": 4, "t": "y"}],
                       "pairs": [[1, "one"], [2, "two"]], "0": "zero"});
        let at = |p: &str| -> Result<Option<Value>, String> { Ok(get(&v, &parse(p)?).cloned()) };
        assert_eq!(at(r#"a["b c"][1]"#)?, Some(json!(20)));
        assert_eq!(at("units[id=4].t")?, Some(json!("y")));
        assert_eq!(at("units[t=x].id")?, Some(json!(3)));
        assert_eq!(at("pairs[#0=2][1]")?, Some(json!("two")));
        assert_eq!(at("0")?, Some(json!("zero")));
        assert_eq!(at("units[5]")?, None);
        assert_eq!(at("a.missing")?, None);
        assert!(parse("a[").is_err());
        assert!(parse("a b").is_err());
        let (segs, end) = parse_tail("r.x[0] + 1", 1)?;
        assert_eq!((segs.len(), end), (2, 6));
        Ok(())
    }
}
