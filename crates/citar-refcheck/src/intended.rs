//! `refcheck/intended.toml` v2: the deliberate differences (DESIGN.md 9.2, "`intended.toml` v2").
//!
//! ```toml
//! [[differences]]
//! id = "civ-stats-gold"                        # kebab-case, unique; cited as `// refcheck: <id>`
//! reason = "Python never wrote civ_stats_gold; Rust does"
//! where = [{ group = "city_stats", path = "cities[*].stats.total.gold" }]
//! cases = ["duel-*", "small-*/t280"]            # optional globs on <case>/t<turn>
//! python = 0                                   # optional constraints, see below
//! rust = { min = 0, max = 50 }
//! rule = "rust_le_python"                      # optional
//! broad = true                                 # required when a path has `.*` or `.**`
//! ```
//!
//! An entry explains a difference when one of its `where` locations matches the difference's
//! group and whole path, its `cases` (if any) match the fixture, and its constraints hold. A
//! constraint on `python` or `rust` is one of:
//!
//! - a value: equal under refcheck's number rule (`python = 3`, `rust = [1, 2]`);
//! - `"re:<regex>"`: a string matching the regex, unanchored (a non-string is matched as JSON);
//! - `{ min = a, max = b }`, either optional: a number within the bounds, inclusive;
//! - `{ eq = <value> }`: a value, spelled out (for a string that starts with `re:`);
//! - `{ is = "null" }` or `{ is = "absent" }`.
//!
//! So an entry never masks a later, unrelated change at the same place: once the value moves
//! outside what the entry says, the difference is unexplained again.
//!
//! `rule = "rust_le_python"` holds when both values are numbers and Rust's is not above Python's.
//! A `better` difference (a route with fewer turns) needs it.
//!
//! An entry is **stale** when a run covered it (compared one of its groups on a fixture its
//! `cases` match) and it explained nothing: a warning, and an error with `--strict`.
//!
//! An `error` (an answer module that failed or panicked) is never explained: nothing was
//! compared, so there is only a module to fix.
//!
//! The v1 form, one inline `differences = [...]` array, is refused: there were never any v1
//! entries to carry over, so the file went straight to v2.
//!
//! [`ScriptIntended`] reads `tests/rules/intended.toml`: the differences only the rule scripts
//! show (DESIGN.md 9.3), each an `id` and a `reason` and nothing else, since no group compares
//! them. The changelog lists both files.

use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

use crate::compare::path::{accepted, walk};
use crate::compare::value::{self, values_equal};
use crate::compare::{CompareSpec, Diff, DiffKind, Pattern};
use crate::{Error, Group, Result};

/// A constraint on one side's value at the difference.
#[derive(Debug, Clone)]
pub enum Constraint {
    Exact(Value),
    Regex(Regex),
    Bounds { min: Option<f64>, max: Option<f64> },
    Null,
    Absent,
}

impl Constraint {
    fn from_toml(v: &toml::Value) -> Result<Constraint> {
        match v {
            toml::Value::String(s) => match s.strip_prefix("re:") {
                Some(re) => Regex::new(re)
                    .map(Constraint::Regex)
                    .map_err(|e| Error::new(format!("bad regex `{re}`: {e}"))),
                None => Ok(Constraint::Exact(Value::String(s.clone()))),
            },
            toml::Value::Table(t) => {
                let keys: Vec<&str> = t.keys().map(String::as_str).collect();
                match keys.as_slice() {
                    ["eq"] => Ok(Constraint::Exact(to_json(&t["eq"])?)),
                    ["is"] => match t["is"].as_str() {
                        Some("null") => Ok(Constraint::Null),
                        Some("absent") => Ok(Constraint::Absent),
                        _ => Err(Error::new("`is` must be \"null\" or \"absent\"")),
                    },
                    ["max"] | ["min"] | ["max", "min"] | ["min", "max"] => {
                        let bound = |k: &str| -> Result<Option<f64>> {
                            match t.get(k) {
                                None => Ok(None),
                                Some(toml::Value::Integer(i)) => Ok(Some(*i as f64)),
                                Some(toml::Value::Float(f)) => Ok(Some(*f)),
                                Some(_) => Err(Error::new(format!("`{k}` must be a number"))),
                            }
                        };
                        Ok(Constraint::Bounds { min: bound("min")?, max: bound("max")? })
                    }
                    _ => Err(Error::new(
                        "a table constraint is { eq = value }, { min = a, max = b } or { is = \"null\" | \"absent\" }",
                    )),
                }
            }
            other => Ok(Constraint::Exact(to_json(other)?)),
        }
    }

    /// Whether the value satisfies the constraint; if not, how it fails, for `explain`.
    fn check(&self, v: Option<&Value>) -> std::result::Result<(), String> {
        let shown = || v.map_or_else(|| "absent".to_string(), |v| value::short(v, 60));
        let ok = match (self, v) {
            (Constraint::Exact(want), Some(got)) => values_equal(want, got),
            (Constraint::Regex(re), Some(Value::String(s))) => re.is_match(s),
            (Constraint::Regex(re), Some(other)) => re.is_match(&other.to_string()),
            (Constraint::Bounds { min, max }, Some(Value::Number(n))) => {
                let x = n.as_f64().unwrap_or(f64::NAN);
                min.is_none_or(|m| x >= m) && max.is_none_or(|m| x <= m)
            }
            (Constraint::Null, Some(Value::Null)) | (Constraint::Absent, None) => true,
            _ => false,
        };
        if ok { Ok(()) } else { Err(format!("is {}, which is not {}", shown(), self)) }
    }
}

impl std::fmt::Display for Constraint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Constraint::Exact(v) => write!(f, "{}", value::short(v, 60)),
            Constraint::Regex(re) => write!(f, "re:{}", re.as_str()),
            Constraint::Bounds { min, max } => {
                let side = |b: &Option<f64>| b.map_or_else(|| "..".to_string(), |x| x.to_string());
                write!(f, "within [{}, {}]", side(min), side(max))
            }
            Constraint::Null => f.write_str("null"),
            Constraint::Absent => f.write_str("absent"),
        }
    }
}

fn to_json(v: &toml::Value) -> Result<Value> {
    Ok(match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::from(*i),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .ok_or_else(|| Error::new("a constraint cannot be NaN or infinite"))?,
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Array(a) => Value::Array(a.iter().map(to_json).collect::<Result<_>>()?),
        toml::Value::Table(t) => Value::Object(
            t.iter().map(|(k, v)| Ok((k.clone(), to_json(v)?))).collect::<Result<_>>()?,
        ),
        toml::Value::Datetime(_) => return Err(Error::new("a constraint cannot be a date")),
    })
}

/// A rule over both values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueRule {
    /// Both are numbers, and Rust's is not above Python's.
    RustLePython,
}

impl ValueRule {
    pub fn name(self) -> &'static str {
        match self {
            ValueRule::RustLePython => "rust_le_python",
        }
    }

    fn parse(s: &str) -> Result<ValueRule> {
        match s {
            "rust_le_python" => Ok(ValueRule::RustLePython),
            other => {
                Err(Error::new(format!("unknown rule `{other}` (the rule is rust_le_python)")))
            }
        }
    }

    fn check(self, diff: &Diff) -> std::result::Result<(), String> {
        match self {
            ValueRule::RustLePython => {
                let num = |v: &Option<Value>| v.as_ref().and_then(Value::as_f64);
                match (num(&diff.python), num(&diff.rust)) {
                    (Some(p), Some(r)) if r <= p || value::floats_equal(r, p) => Ok(()),
                    (Some(p), Some(r)) => {
                        Err(format!("rule rust_le_python: Rust's {r} is above Python's {p}"))
                    }
                    _ => Err("rule rust_le_python needs two numbers".into()),
                }
            }
        }
    }
}

/// One location an entry covers.
#[derive(Debug, Clone)]
pub struct Where {
    pub group: Group,
    pub path: Pattern,
}

/// One accepted difference.
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub reason: String,
    pub wheres: Vec<Where>,
    /// The `cases` globs as written; empty means every fixture.
    pub cases: Vec<String>,
    case_set: Option<GlobSet>,
    pub python: Option<Constraint>,
    pub rust: Option<Constraint>,
    pub rule: Option<ValueRule>,
    pub broad: bool,
}

impl Entry {
    pub fn matches_case(&self, case: &str) -> bool {
        self.case_set.as_ref().is_none_or(|s| s.is_match(case))
    }

    /// Whether its constraints and rule hold for the difference; if not, why not.
    pub fn check(&self, diff: &Diff) -> std::result::Result<(), String> {
        if let Some(c) = &self.python {
            c.check(diff.python.as_ref()).map_err(|w| format!("Python's value {w}"))?;
        }
        if let Some(c) = &self.rust {
            c.check(diff.rust.as_ref()).map_err(|w| format!("Rust's value {w}"))?;
        }
        if let Some(rule) = self.rule {
            rule.check(diff)?;
        }
        if diff.kind == DiffKind::Better && self.rule != Some(ValueRule::RustLePython) {
            return Err("a better route needs rule = \"rust_le_python\"".into());
        }
        Ok(())
    }
}

/// A difference an entry points at but does not explain, because a constraint failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NearMiss {
    pub id: String,
    pub why: String,
}

/// Which entries explain a difference.
#[derive(Debug, Clone, Default)]
pub struct Explanation {
    /// Every entry that explains it, in file order; the first is credited in reports.
    pub by: Vec<usize>,
    pub near: Vec<NearMiss>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    differences: Vec<RawEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    id: String,
    reason: String,
    #[serde(rename = "where")]
    wheres: Vec<RawWhere>,
    #[serde(default)]
    cases: Vec<String>,
    python: Option<toml::Value>,
    rust: Option<toml::Value>,
    rule: Option<String>,
    #[serde(default)]
    broad: bool,
}

/// `tests/rules/intended.toml`: one entry per `[[differences]]` table, `id` and `reason` only.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScriptFile {
    #[serde(default)]
    differences: Vec<RawScriptEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScriptEntry {
    id: String,
    reason: String,
}

/// A deliberate difference only the rule scripts show: a check that expects the Rust engine's
/// value cites it with `intended = "<id>"`, and the Python runner skips that check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptEntry {
    pub id: String,
    pub reason: String,
}

/// `tests/rules/intended.toml`, the differences no refcheck group compares.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptIntended {
    entries: Vec<ScriptEntry>,
}

impl ScriptIntended {
    pub fn load(path: &Path) -> Result<ScriptIntended> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::new(format!("cannot read {}: {e}", path.display())))?;
        ScriptIntended::parse(&text).map_err(|e| e.context(path.display()))
    }

    /// Parses the list: kebab-case ids, each once, and one-line reasons, as `intended.toml`'s.
    pub fn parse(text: &str) -> Result<ScriptIntended> {
        reject_inline_form(text)?;
        let raw: RawScriptFile =
            toml::from_str(text).map_err(|e| Error::new(e.to_string().trim_end().to_string()))?;
        let mut entries: Vec<ScriptEntry> = Vec::with_capacity(raw.differences.len());
        for r in raw.differences {
            let context = format!("difference `{}`", r.id);
            if !is_kebab(&r.id) {
                return Err(Error::new(
                    "the id must be kebab-case: lower-case words joined by `-`",
                )
                .context(&context));
            }
            let reason = r.reason.trim();
            if reason.is_empty() || reason.contains('\n') {
                return Err(Error::new("the reason must be one non-empty line").context(&context));
            }
            if entries.iter().any(|e| e.id == r.id) {
                return Err(Error::new(format!("the id `{}` is used twice", r.id)));
            }
            entries.push(ScriptEntry { id: r.id, reason: reason.to_string() });
        }
        Ok(ScriptIntended { entries })
    }

    pub fn entries(&self) -> &[ScriptEntry] {
        &self.entries
    }
}

/// The CHANGELOG's list of rule fixes: `intended`'s entries, then those only the scripts show,
/// one Markdown bullet each. An id in both lists is refused, since a script cites either by id.
pub fn changelog(intended: &Intended, scripts: &ScriptIntended) -> Result<String> {
    if let Some(e) = scripts.entries.iter().find(|e| intended.get(&e.id).is_some()) {
        return Err(Error::new(format!(
            "the id `{}` is in both refcheck/intended.toml and tests/rules/intended.toml",
            e.id
        )));
    }
    let mut out = intended.changelog();
    for e in &scripts.entries {
        out.push_str(&format!("- {} (`{}`)\n", e.reason, e.id));
    }
    Ok(out)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWhere {
    group: String,
    path: String,
}

/// One group's places: every `where` path given for the group, walked as one pattern set.
#[derive(Debug, Clone, Default)]
struct GroupIndex {
    patterns: Vec<Pattern>,
    /// The entry each pattern is from.
    owner: Vec<usize>,
    /// Whether only `--with-bot` compares what the pattern matches.
    bot_only: Vec<bool>,
    /// The entries with a place in the group, in file order, once each.
    entries: Vec<usize>,
}

impl GroupIndex {
    fn build(group: Group, entries: &[Entry]) -> GroupIndex {
        let spec = CompareSpec::for_group(group);
        let mut ix = GroupIndex::default();
        for (i, e) in entries.iter().enumerate() {
            for w in e.wheres.iter().filter(|w| w.group == group) {
                ix.patterns.push(w.path.clone());
                ix.owner.push(i);
                ix.bot_only.push(spec.is_bot_only(&w.path));
                if ix.entries.last() != Some(&i) {
                    ix.entries.push(i);
                }
            }
        }
        ix
    }
}

/// The loaded list.
#[derive(Debug, Clone)]
pub struct Intended {
    entries: Vec<Entry>,
    /// One per group, in [`Group::ALL`] order.
    index: Vec<GroupIndex>,
}

impl Default for Intended {
    fn default() -> Self {
        Intended::from_entries(Vec::new())
    }
}

/// The entries as they apply to one group on one fixture: what the verdicts of that subject
/// need. Each entry's `cases` are matched once here, not once per difference, and the group's
/// places are one pattern set, so a difference costs one walk down its path.
pub struct Scoped<'a> {
    entries: &'a [Entry],
    index: &'a GroupIndex,
    /// Per entry of the file: whether it has a place in the group and its `cases` match.
    applies: Vec<bool>,
}

impl Scoped<'_> {
    /// Which entries explain a difference.
    pub fn explain(&self, diff: &Diff) -> Explanation {
        let mut out = Explanation::default();
        if diff.kind == DiffKind::Error || self.index.patterns.is_empty() {
            return out;
        }
        let cursor = walk(&self.index.patterns, &diff.path);
        let mut hits: Vec<usize> = accepted(&self.index.patterns, &cursor)
            .map(|k| self.index.owner[k])
            .filter(|&i| self.applies[i])
            .collect();
        hits.sort_unstable();
        hits.dedup();
        for i in hits {
            let e = &self.entries[i];
            match e.check(diff) {
                Ok(()) => out.by.push(i),
                Err(why) => out.near.push(NearMiss { id: e.id.clone(), why }),
            }
        }
        out
    }

    /// The entries that comparing this group on this fixture covers, for stale detection: those
    /// with a place in the group and matching `cases`, leaving out places that only `--with-bot`
    /// compares when it is off. An entry may come more than once.
    pub fn covered(&self, with_bot: bool) -> impl Iterator<Item = usize> + '_ {
        let ix = self.index;
        (0..ix.patterns.len())
            .filter(move |&k| self.applies[ix.owner[k]] && (with_bot || !ix.bot_only[k]))
            .map(|k| ix.owner[k])
    }
}

impl Intended {
    pub fn load(path: &Path) -> Result<Intended> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::new(format!("cannot read {}: {e}", path.display())))?;
        Intended::parse(&text).map_err(|e| e.context(path.display()))
    }

    pub fn parse(text: &str) -> Result<Intended> {
        reject_inline_form(text)?;
        let raw: RawFile =
            toml::from_str(text).map_err(|e| Error::new(e.to_string().trim_end().to_string()))?;
        let mut entries: Vec<Entry> = Vec::with_capacity(raw.differences.len());
        for r in raw.differences {
            let id = r.id.clone();
            let entry = build_entry(r).map_err(|e| e.context(format!("difference `{id}`")))?;
            if entries.iter().any(|e| e.id == entry.id) {
                return Err(Error::new(format!("the id `{}` is used twice", entry.id)));
            }
            entries.push(entry);
        }
        Ok(Intended::from_entries(entries))
    }

    fn from_entries(entries: Vec<Entry>) -> Intended {
        let index = Group::ALL.iter().map(|&g| GroupIndex::build(g, &entries)).collect();
        Intended { entries, index }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// The entries as they apply to one group on one fixture (or run-scope case).
    pub fn scoped(&self, group: Group, case: &str) -> Scoped<'_> {
        let index = &self.index[group.ordinal()];
        let mut applies = vec![false; self.entries.len()];
        for &i in &index.entries {
            applies[i] = self.entries[i].matches_case(case);
        }
        Scoped { entries: &self.entries, index, applies }
    }

    /// Which entries explain a difference in this group and case. A run explains many
    /// differences per subject through [`Intended::scoped`] instead.
    pub fn explain(&self, group: Group, case: &str, diff: &Diff) -> Explanation {
        self.scoped(group, case).explain(diff)
    }

    /// The entries as the CHANGELOG's list of rule fixes, one Markdown bullet each.
    pub fn changelog(&self) -> String {
        let mut out = String::new();
        for e in &self.entries {
            out.push_str(&format!("- {} (`{}`)\n", e.reason.trim(), e.id));
        }
        out
    }
}

fn build_entry(r: RawEntry) -> Result<Entry> {
    if !is_kebab(&r.id) {
        return Err(Error::new("the id must be kebab-case: lower-case words joined by `-`"));
    }
    let reason = r.reason.trim();
    if reason.is_empty() || reason.contains('\n') {
        return Err(Error::new("the reason must be one non-empty line"));
    }
    if r.wheres.is_empty() {
        return Err(Error::new("`where` needs at least one { group, path }"));
    }
    if r.wheres.iter().any(|w| w.path.is_empty()) {
        return Err(Error::new("a path cannot be empty: name the place in the answer"));
    }
    let wheres = r
        .wheres
        .iter()
        .map(|w| Ok(Where { group: Group::parse(&w.group)?, path: Pattern::parse(&w.path)? }))
        .collect::<Result<Vec<Where>>>()?;
    let broad = wheres.iter().any(|w| w.path.is_broad());
    if broad && !r.broad {
        return Err(Error::new("a path uses `.*` or `.**`, so the entry must say broad = true"));
    }
    if r.broad && !broad {
        return Err(Error::new("broad = true, but no path uses `.*` or `.**`"));
    }
    let case_set = if r.cases.is_empty() {
        None
    } else {
        let mut b = GlobSetBuilder::new();
        for c in &r.cases {
            b.add(Glob::new(c).map_err(|e| Error::new(format!("bad cases glob `{c}`: {e}")))?);
        }
        Some(b.build().map_err(|e| Error::new(e.to_string()))?)
    };
    let constraint = |v: &Option<toml::Value>, side: &str| -> Result<Option<Constraint>> {
        v.as_ref().map(|v| Constraint::from_toml(v).map_err(|e| e.context(side))).transpose()
    };
    Ok(Entry {
        python: constraint(&r.python, "python")?,
        rust: constraint(&r.rust, "rust")?,
        rule: r.rule.as_deref().map(ValueRule::parse).transpose()?,
        id: r.id,
        reason: reason.to_string(),
        wheres,
        cases: r.cases,
        case_set,
        broad: r.broad,
    })
}

fn is_kebab(id: &str) -> bool {
    !id.is_empty()
        && id.split('-').all(|w| {
            !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// Refuses `differences = [...]`, the v1 form. Both forms parse to the same array, so the
/// difference is only visible in the source: a v2 file names `differences` in `[[ ]]` headers.
fn reject_inline_form(text: &str) -> Result<()> {
    let doc = toml::de::DeTable::parse(text)
        .map_err(|e| Error::new(e.to_string().trim_end().to_string()))?;
    for (key, _) in doc.get_ref().iter() {
        if key.get_ref() == "differences" && !text[..key.span().start].trim_end().ends_with("[[") {
            return Err(Error::new(
                "`differences = [...]` is the v1 inline form; v2 writes each entry as a [[differences]] table",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::{Path, Seg};
    use serde_json::json;

    fn diff(path: &[&str], kind: DiffKind, python: Option<Value>, rust: Option<Value>) -> Diff {
        Diff {
            path: Path(path.iter().map(|k| Seg::Key((*k).into())).collect()),
            kind,
            python,
            rust,
            detail: None,
        }
    }

    const ONE: &str = r#"
[[differences]]
id = "gold-fix"
reason = "Python never wrote the gold total"
where = [{ group = "city_stats", path = "stats.gold" }]
python = 0
rust = { min = 1, max = 10 }
"#;

    #[test]
    fn a_located_entry_explains_only_within_its_constraints() {
        let list = Intended::parse(ONE).unwrap();
        let good = diff(&["stats", "gold"], DiffKind::Number, Some(json!(0)), Some(json!(4)));
        assert_eq!(list.explain(Group::CityStats, "duel/t1", &good).by, [0]);
        // A later, unrelated change at the same place is not masked.
        let moved = diff(&["stats", "gold"], DiffKind::Number, Some(json!(0)), Some(json!(40)));
        let e = list.explain(Group::CityStats, "duel/t1", &moved);
        assert!(e.by.is_empty(), "a constraint mismatch stays unexplained");
        assert_eq!(e.near.len(), 1);
        assert!(e.near[0].why.contains("Rust's value is 40"), "{}", e.near[0].why);
        let python_moved =
            diff(&["stats", "gold"], DiffKind::Number, Some(json!(2)), Some(json!(4)));
        assert!(list.explain(Group::CityStats, "duel/t1", &python_moved).by.is_empty());
        // Another group or path is not located at all.
        assert!(list.explain(Group::Civs, "duel/t1", &good).near.is_empty());
        let other = diff(&["stats", "food"], DiffKind::Number, Some(json!(0)), Some(json!(4)));
        assert!(list.explain(Group::CityStats, "duel/t1", &other).near.is_empty());
    }

    #[test]
    fn constraint_forms() {
        let c = |t: &str| {
            Constraint::from_toml(&toml::from_str::<toml::Table>(&format!("v = {t}")).unwrap()["v"])
                .unwrap()
        };
        assert!(c("3").check(Some(&json!(3.0))).is_ok());
        assert!(c("3").check(Some(&json!(4))).is_err());
        assert!(c("\"x\"").check(Some(&json!("x"))).is_ok());
        assert!(c("\"re:^Unknown tool\"").check(Some(&json!("Unknown tool 'x'."))).is_ok());
        assert!(c("\"re:^Unknown tool\"").check(Some(&json!("No such tool"))).is_err());
        assert!(c("{ eq = \"re:x\" }").check(Some(&json!("re:x"))).is_ok());
        assert!(c("{ min = 1.5 }").check(Some(&json!(2))).is_ok());
        assert!(c("{ min = 1.5 }").check(Some(&json!(1))).is_err());
        assert!(c("{ max = 1 }").check(Some(&json!("1"))).is_err());
        assert!(c("{ is = \"null\" }").check(Some(&json!(null))).is_ok());
        assert!(c("{ is = \"absent\" }").check(None).is_ok());
        assert!(c("{ is = \"absent\" }").check(Some(&json!(null))).is_err());
        assert!(c("[1, 2]").check(Some(&json!([1, 2]))).is_ok());
        for bad in ["{ min = \"x\" }", "{ is = \"zero\" }", "{ foo = 1 }", "\"re:(\""] {
            let v = toml::from_str::<toml::Table>(&format!("v = {bad}")).unwrap();
            assert!(Constraint::from_toml(&v["v"]).is_err(), "{bad}");
        }
    }

    #[test]
    fn cases_limit_where_an_entry_applies() {
        let text = r#"
[[differences]]
id = "late-only"
reason = "r"
where = [{ group = "civs", path = "a" }]
cases = ["small-*/t280"]
"#;
        let list = Intended::parse(text).unwrap();
        let d = diff(&["a"], DiffKind::Number, Some(json!(1)), Some(json!(2)));
        assert_eq!(list.explain(Group::Civs, "small-continents-normal-s1025/t280", &d).by, [0]);
        assert!(list.explain(Group::Civs, "small-continents-normal-s1025/t200", &d).by.is_empty());
        assert!(list.entries()[0].matches_case("small-x/t280"));
        assert!(!list.entries()[0].matches_case("duel-x/t280"));
    }

    #[test]
    fn a_better_route_needs_the_rule() {
        let without = r#"
[[differences]]
id = "astar"
reason = "r"
where = [{ group = "movement", path = "turns" }]
"#;
        let with = format!("{without}rule = \"rust_le_python\"\n");
        let better = diff(&["turns"], DiffKind::Better, Some(json!(3)), Some(json!(2)));
        assert!(
            Intended::parse(without)
                .unwrap()
                .explain(Group::Movement, "c/t1", &better)
                .by
                .is_empty()
        );
        let list = Intended::parse(&with).unwrap();
        assert_eq!(list.explain(Group::Movement, "c/t1", &better).by, [0]);
        let worse = diff(&["turns"], DiffKind::Number, Some(json!(3)), Some(json!(4)));
        assert!(list.explain(Group::Movement, "c/t1", &worse).by.is_empty());
    }

    #[test]
    fn duplicate_ids_unknown_fields_and_the_v1_form_are_refused() {
        let twice = format!("{ONE}{}", ONE.replace("python = 0", "python = 1"));
        let e = Intended::parse(&twice).unwrap_err();
        assert!(e.message().contains("used twice"), "{e}");

        let unknown = ONE.replace("python = 0", "pyhton = 0");
        let e = Intended::parse(&unknown).unwrap_err();
        assert!(e.message().contains("pyhton"), "{e}");
        let top = format!("version = 2\n{ONE}");
        assert!(Intended::parse(&top).is_err(), "unknown top-level keys are refused too");
        let unknown_where =
            ONE.replace("path = \"stats.gold\"", "path = \"stats.gold\", note = \"x\"");
        assert!(Intended::parse(&unknown_where).is_err());

        let v1 = "differences = [\n]\n";
        let e = Intended::parse(v1).unwrap_err();
        assert!(e.message().contains("v1 inline form"), "{e}");
        let v1_entry = r#"differences = [
  { group = "city_stats", path = "cities[*].stats.total.gold", reason = "r" },
]
"#;
        assert!(Intended::parse(v1_entry).unwrap_err().message().contains("v1 inline form"));
        assert!(Intended::parse("# nothing yet\n").unwrap().entries().is_empty());
    }

    #[test]
    fn entries_are_checked_when_loaded() {
        let bad = [
            ("id = \"gold-fix\"", "id = \"Gold_Fix\""),
            ("reason = \"Python never wrote the gold total\"", "reason = \" \""),
            ("group = \"city_stats\"", "group = \"cities\""),
            ("path = \"stats.gold\"", "path = \"stats..gold\""),
            ("path = \"stats.gold\"", "path = \"\""),
            ("path = \"stats.gold\"", "path = \"stats.**\""),
            ("python = 0", "python = 0\nbroad = true"),
            ("python = 0", "python = 0\nrule = \"rust_lt_python\""),
            ("python = 0", "python = 0\ncases = [\"[\"]"),
            ("where = [{ group = \"city_stats\", path = \"stats.gold\" }]", "where = []"),
        ];
        for (from, to) in bad {
            let text = ONE.replace(from, to);
            assert!(Intended::parse(&text).is_err(), "should refuse: {to}");
        }
        let broad = ONE
            .replace("path = \"stats.gold\"", "path = \"stats.**\"")
            .replace("python = 0", "python = 0\nbroad = true");
        assert!(Intended::parse(&broad).is_ok());
    }

    #[test]
    fn places_are_indexed_per_group_and_errors_are_never_explained() {
        let text = r#"
[[differences]]
id = "two-places"
reason = "r"
where = [{ group = "civs", path = "a" }, { group = "views", path = "b.**" }, { group = "civs", path = "c[*]" }]
broad = true

[[differences]]
id = "everywhere"
reason = "r"
where = [{ group = "civs", path = "**" }]
broad = true
cases = ["duel-*"]

[[differences]]
id = "bot"
reason = "r"
where = [{ group = "deal_checks", path = "deals[*].bot_value" }]
"#;
        let list = Intended::parse(text).unwrap();
        let a = diff(&["a"], DiffKind::Number, Some(json!(1)), Some(json!(2)));
        assert_eq!(list.explain(Group::Civs, "duel-x/t1", &a).by, [0, 1]);
        assert_eq!(list.explain(Group::Civs, "small-x/t1", &a).by, [0]);
        assert!(list.explain(Group::Views, "duel-x/t1", &a).by.is_empty());
        let b = diff(&["b", "x", "y"], DiffKind::Number, Some(json!(1)), Some(json!(2)));
        assert_eq!(list.explain(Group::Views, "duel-x/t1", &b).by, [0]);
        let mut c = diff(&["c"], DiffKind::Text, Some(json!("x")), Some(json!("y")));
        c.path.0.push(Seg::Index(4));
        assert_eq!(list.explain(Group::Civs, "small-x/t1", &c).by, [0]);
        // `**` matches the root too, yet a failed answer module is never explained.
        assert!(list.explain(Group::Civs, "duel-x/t1", &Diff::error("panicked")).by.is_empty());
        // Coverage: every entry with a place in the group and matching cases, bot-only aside.
        let covered = |g, case, with_bot| {
            let mut c: Vec<usize> = list.scoped(g, case).covered(with_bot).collect();
            c.dedup();
            c
        };
        assert_eq!(covered(Group::Civs, "duel-x/t1", false), [0, 1]);
        assert_eq!(covered(Group::Civs, "small-x/t1", false), [0]);
        assert!(covered(Group::DealChecks, "duel-x/t1", false).is_empty());
        assert_eq!(covered(Group::DealChecks, "duel-x/t1", true), [2]);
    }

    #[test]
    fn the_changelog_lists_every_entry() {
        let list = Intended::parse(ONE).unwrap();
        assert_eq!(list.changelog(), "- Python never wrote the gold total (`gold-fix`)\n");
    }

    const SCRIPTS: &str = r#"
[[differences]]
id = "atomic-ops"
reason = "Python left half a list applied; Rust applies all or nothing"
"#;

    #[test]
    fn the_script_list_joins_the_changelog() {
        let list = Intended::parse(ONE).unwrap();
        let scripts = ScriptIntended::parse(SCRIPTS).unwrap();
        assert_eq!(scripts.entries().len(), 1);
        assert_eq!(
            changelog(&list, &scripts).unwrap(),
            "- Python never wrote the gold total (`gold-fix`)\n\
             - Python left half a list applied; Rust applies all or nothing (`atomic-ops`)\n"
        );
        let both = ScriptIntended::parse(
            "[[differences]]\nid = \"gold-fix\"\nreason = \"the same id twice\"\n",
        )
        .unwrap();
        assert!(changelog(&list, &both).is_err());
    }

    #[test]
    fn a_script_entry_is_an_id_and_a_reason() {
        let bad = [
            "[[differences]]\nid = \"Not_Kebab\"\nreason = \"x\"\n",
            "[[differences]]\nid = \"a\"\nreason = \"  \"\n",
            "[[differences]]\nid = \"a\"\nreason = \"x\"\nwhere = []\n",
            "[[differences]]\nid = \"a\"\nreason = \"x\"\n[[differences]]\nid = \"a\"\nreason = \"y\"\n",
            "differences = [{ id = \"a\", reason = \"x\" }]\n",
        ];
        for text in bad {
            assert!(ScriptIntended::parse(text).is_err(), "{text}");
        }
    }

    /// The repository's own list: it loads, shares no id with `refcheck/intended.toml`, and every
    /// entry is cited where its fix is made (`// refcheck: <id>`), as `intended.toml`'s are.
    #[test]
    fn the_repository_script_list_is_well_formed_and_cited() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let intended = Intended::load(&root.join(crate::run::INTENDED)).unwrap();
        let scripts = ScriptIntended::load(&root.join(crate::run::SCRIPT_INTENDED)).unwrap();
        changelog(&intended, &scripts).unwrap();
        let mut sources = String::new();
        let mut stack = vec![root.join("crates/citar-engine/src")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|x| x == "rs") {
                    sources.push_str(&std::fs::read_to_string(&path).unwrap());
                }
            }
        }
        let uncited: Vec<&str> = scripts
            .entries()
            .iter()
            .map(|e| e.id.as_str())
            .filter(|id| !sources.contains(&format!("refcheck: {id}")))
            .collect();
        assert!(uncited.is_empty(), "not cited in the engine: {uncited:?}");
    }
}
