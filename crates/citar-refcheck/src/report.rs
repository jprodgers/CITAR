//! Reports: the human summary, the JSON report and `explain` (DESIGN.md 9.2).
//!
//! Both reports list groups in dependency order, then fixtures by name, then differences in the
//! order the comparer met them. Neither holds a time, a thread count or an absolute path the run
//! did not already have, so two runs over the same inputs write the same bytes.

use serde_json::{Map, Value, json};

use crate::compare::value::short;
use crate::compare::{Pattern, Seg};
use crate::intended::Intended;
use crate::run::{Checked, Outcome, Run, Subject, Verdict};
use crate::{Error, Group, Result};

/// The width values are cut to on one line.
const VALUE_WIDTH: usize = 60;

fn verdict_name(v: &Verdict) -> &'static str {
    match v {
        Verdict::Unexplained => "unexplained",
        Verdict::Explained(_) => "explained",
        Verdict::Accepted => "accepted",
    }
}

fn side(v: Option<&Value>, width: usize) -> String {
    v.map_or_else(|| "(absent)".to_string(), |v| short(v, width))
}

fn indent(text: &str, by: &str) -> String {
    text.lines().map(|l| format!("{by}{l}\n")).collect()
}

/// One difference on one line (plus its detail and near misses, indented), for the reports.
fn diff_lines(sub: &Subject, c: &Checked, width: usize) -> String {
    let d = &c.diff;
    let mut line = format!("  {}  {}  {}", sub.case, d.path, d.kind.name());
    if d.python.is_some() || d.rust.is_some() {
        line.push_str(&format!(
            "  python {}  rust {}",
            side(d.python.as_ref(), width),
            side(d.rust.as_ref(), width)
        ));
    }
    match &c.verdict {
        Verdict::Explained(id) => line.push_str(&format!("  [intended: {id}]")),
        Verdict::Accepted => line.push_str("  [accepted]"),
        Verdict::Unexplained if c.enforced => line.push_str("  [enforced]"),
        Verdict::Unexplained => {}
    }
    line.push('\n');
    if let Some(detail) = &d.detail {
        line.push_str(&indent(detail.trim_end(), "      "));
    }
    for n in &c.near {
        line.push_str(&format!("      near miss {}: {}\n", n.id, n.why));
    }
    line
}

/// The human report. `limit` caps the unexplained differences printed per group (0: no cap).
pub fn human(run: &Run, limit: usize) -> String {
    let mut out = String::new();
    let sets: Vec<String> = run
        .options
        .sets
        .iter()
        .map(|s| {
            let n = run.states.iter().filter(|st| st.set == s.label).count();
            format!("{} ({n})", s.shown)
        })
        .collect();
    out.push_str(&format!("refcheck: {} states from {}\n", run.states.len(), sets.join(", ")));
    if !run.options.cases.is_empty() {
        out.push_str(&format!("  cases: {}\n", run.options.cases.join(", ")));
    }
    out.push('\n');

    out.push_str(&format!(
        "{:<16} {:>8} {:>12} {:>10} {:>9}  status\n",
        "group", "compared", "unexplained", "explained", "accepted"
    ));
    for (g, s) in &run.summaries {
        if !s.selected {
            continue;
        }
        let mut status = Vec::new();
        if !s.ported {
            status.push("not ported".to_string());
        }
        if s.enforced {
            status.push("enforced".to_string());
        }
        if s.failed > 0 {
            status.push(format!("{} failed", s.failed));
        }
        if s.python_crashed > 0 {
            status.push(format!("{} python-crashed", s.python_crashed));
        }
        if s.ported {
            out.push_str(&format!(
                "{:<16} {:>8} {:>12} {:>10} {:>9}  {}\n",
                g.name(),
                s.compared,
                s.unexplained,
                s.explained,
                s.accepted,
                status.join(", ")
            ));
        } else {
            out.push_str(&format!(
                "{:<16} {:>8} {:>12} {:>10} {:>9}  {}\n",
                g.name(),
                "-",
                "-",
                "-",
                "-",
                status.join(", ")
            ));
        }
    }

    for (g, s) in &run.summaries {
        if !s.selected || s.unexplained == 0 {
            continue;
        }
        out.push_str(&format!(
            "\n{g}: {} unexplained ({} enforced)\n",
            s.unexplained, s.unexplained_enforced
        ));
        let unexplained = run.findings().filter(|(sub, c)| sub.group == *g && c.is_unexplained());
        for (shown, (sub, c)) in unexplained.enumerate() {
            if limit > 0 && shown == limit {
                out.push_str(&format!(
                    "  ... and {} more (--limit 0, --json, or `explain {g}` shows them)\n",
                    s.unexplained - shown as u64
                ));
                break;
            }
            out.push_str(&diff_lines(sub, c, VALUE_WIDTH));
        }
    }

    let crashed: Vec<&Subject> =
        run.subjects.iter().filter(|s| matches!(s.outcome, Outcome::PythonCrashed(_))).collect();
    if !crashed.is_empty() {
        out.push_str("\npython-crashed (information, not differences):\n");
        for s in crashed {
            if let Outcome::PythonCrashed(line) = &s.outcome {
                out.push_str(&format!("  {} {}: {line}\n", s.group, s.case));
            }
        }
    }

    let entries = run.intended.len();
    let used = run.intended.iter().filter(|u| u.used > 0).count();
    let stale: Vec<&str> = run.stale().map(|u| u.id.as_str()).collect();
    out.push_str(&format!("\nintended: {entries} entries, {used} used"));
    if stale.is_empty() {
        out.push('\n');
    } else {
        let severity = if run.options.strict {
            "an error with --strict"
        } else {
            "a warning; an error with --strict"
        };
        out.push_str(&format!(", {} stale ({severity}): {}\n", stale.len(), stale.join(", ")));
    }

    if !run.load_failures.is_empty() {
        out.push_str("\nload failures:\n");
        for f in &run.load_failures {
            out.push_str(&format!("  {}: {}\n", f.name, f.error));
        }
    }

    out.push_str(&format!("\n{}\n", result_line(run)));
    out
}

fn result_line(run: &Run) -> String {
    let code = run.exit_code();
    let why = match code {
        2 => format!("{} fixture(s) failed to load", run.load_failures.len()),
        1 if run.unexplained_enforced() > 0 => {
            format!(
                "{} unexplained difference(s) where enforced.toml covers them",
                run.unexplained_enforced()
            )
        }
        1 => {
            let missing: Vec<&str> = run.not_ported().into_iter().map(Group::name).collect();
            let mut parts = Vec::new();
            if run.unexplained() > 0 {
                parts.push(format!("{} unexplained difference(s)", run.unexplained()));
            }
            if !missing.is_empty() {
                parts.push(format!("groups not ported: {}", missing.join(", ")));
            }
            format!("--strict: {}", parts.join("; "))
        }
        3 => "--strict: stale intended entries".to_string(),
        _ if run.unexplained() > 0 => {
            format!(
                "clean where enforced ({} unexplained elsewhere, held by the ratchet)",
                run.unexplained()
            )
        }
        _ => "clean".to_string(),
    };
    format!("result: {why} (exit {code})")
}

fn diff_json(sub: &Subject, c: &Checked) -> Value {
    let d = &c.diff;
    let mut m = Map::new();
    m.insert("case".into(), json!(sub.case));
    m.insert("set".into(), json!(sub.set));
    m.insert("path".into(), json!(d.path.to_string()));
    m.insert("kind".into(), json!(d.kind.name()));
    // An absent side is left out, which is how "nothing there" differs from null.
    if let Some(p) = &d.python {
        m.insert("python".into(), p.clone());
    }
    if let Some(r) = &d.rust {
        m.insert("rust".into(), r.clone());
    }
    if let Some(detail) = &d.detail {
        m.insert("detail".into(), json!(detail));
    }
    m.insert("verdict".into(), json!(verdict_name(&c.verdict)));
    if let Verdict::Explained(id) = &c.verdict {
        m.insert("intended".into(), json!(id));
    }
    m.insert("enforced".into(), json!(c.enforced));
    if !c.near.is_empty() {
        let near: Vec<Value> = c.near.iter().map(|n| json!({"id": n.id, "why": n.why})).collect();
        m.insert("near".into(), Value::Array(near));
    }
    Value::Object(m)
}

/// The JSON report (`run --json`).
pub fn json(run: &Run) -> String {
    let groups: Vec<Value> = run
        .summaries
        .iter()
        .filter(|(_, s)| s.selected)
        .map(|(g, s)| {
            let crashed: Vec<Value> = run
                .subjects
                .iter()
                .filter(|x| x.group == *g)
                .filter_map(|x| match &x.outcome {
                    Outcome::PythonCrashed(line) => {
                        Some(json!({"case": x.case, "set": x.set, "error": line}))
                    }
                    _ => None,
                })
                .collect();
            let diffs: Vec<Value> = run
                .findings()
                .filter(|(sub, _)| sub.group == *g)
                .map(|(sub, c)| diff_json(sub, c))
                .collect();
            json!({
                "group": g.name(),
                "status": if s.ported { "compared" } else { "not_ported" },
                "enforced": s.enforced,
                "compared": s.compared,
                "failed": s.failed,
                "unexplained": s.unexplained,
                "unexplained_enforced": s.unexplained_enforced,
                "explained": s.explained,
                "accepted": s.accepted,
                "python_crashed": crashed,
                "diffs": diffs,
            })
        })
        .collect();
    let report = json!({
        "report": "citar-refcheck",
        "format": 1,
        "fixtures": run.options.sets.iter().map(|s| s.shown.clone()).collect::<Vec<_>>(),
        "states": run.states.len(),
        "options": {
            "groups": run.options.groups.iter().map(|g| g.name()).collect::<Vec<_>>(),
            "cases": run.options.cases,
            "strict": run.options.strict,
            "with_bot": run.options.with_bot,
        },
        "load_failures": run.load_failures.iter().map(|f| json!({"name": f.name, "error": f.error})).collect::<Vec<_>>(),
        "groups": groups,
        "intended": run.intended.iter().map(|u| json!({"id": u.id, "used": u.used, "covered": u.covered, "stale": u.stale()})).collect::<Vec<_>>(),
        "exit": run.exit_code(),
    });
    let mut text = serde_json::to_string_pretty(&report).unwrap_or_default();
    text.push('\n');
    text
}

/// `explain TARGET`: an intended id, or `group` or `group:path-pattern`.
pub fn explain(run: &Run, intended: &Intended, target: &str) -> Result<String> {
    if let Some(entry) = intended.get(target) {
        return Ok(explain_entry(run, intended, entry.id.as_str()));
    }
    let (group, pattern) = match target.split_once(':') {
        Some((g, p)) => (Group::parse(g)?, Pattern::parse(p)?),
        None => {
            let group = Group::from_name(target).ok_or_else(|| {
                let names: Vec<&str> = Group::ALL.iter().map(|g| g.name()).collect();
                Error::new(format!(
                    "`{target}` is neither an intended id nor a group; the groups are {}",
                    names.join(", ")
                ))
            })?;
            (group, Pattern::parse("**")?)
        }
    };
    Ok(explain_place(run, group, &pattern))
}

fn explain_entry(run: &Run, intended: &Intended, id: &str) -> String {
    let mut out = String::new();
    let Some(e) = intended.get(id) else { return out };
    out.push_str(&format!("{id}: {}\n", e.reason));
    for w in &e.wheres {
        out.push_str(&format!("  where {} {}\n", w.group, w.path));
    }
    if !e.cases.is_empty() {
        out.push_str(&format!("  cases {}\n", e.cases.join(", ")));
    }
    if let Some(c) = &e.python {
        out.push_str(&format!("  python {c}\n"));
    }
    if let Some(c) = &e.rust {
        out.push_str(&format!("  rust {c}\n"));
    }
    if let Some(r) = e.rule {
        out.push_str(&format!("  rule {}\n", r.name()));
    }
    if let Some(u) = run.intended.iter().find(|u| u.id == id) {
        let state = if u.stale() {
            "stale: covered by this run, and explained nothing"
        } else if u.covered {
            "covered by this run"
        } else {
            "not covered by this run (none of its groups compared on a fixture its cases match)"
        };
        out.push_str(&format!("  explained {} difference(s); {state}\n", u.used));
    }
    let explained: Vec<(&Subject, &Checked)> =
        run.findings().filter(|(_, c)| c.verdict == Verdict::Explained(id.to_string())).collect();
    if !explained.is_empty() {
        out.push_str("\nfirst to explain:\n");
        for (sub, c) in &explained {
            out.push_str(&diff_lines(sub, c, 400));
        }
    }
    let near: Vec<(&Subject, &Checked)> =
        run.findings().filter(|(_, c)| c.near.iter().any(|n| n.id == id)).collect();
    if !near.is_empty() {
        out.push_str("\nat its places, but outside its constraints:\n");
        for (sub, c) in &near {
            out.push_str(&diff_lines(sub, c, 400));
        }
    }
    out
}

fn explain_place(run: &Run, group: Group, pattern: &Pattern) -> String {
    let mut out = String::new();
    let s = run.summary(group);
    out.push_str(&format!("{group} at {pattern}\n"));
    if !s.selected {
        out.push_str("  not selected in this run (--groups)\n");
        return out;
    }
    if !s.ported {
        out.push_str("  not ported: the group has no answer module yet\n");
    }
    let found: Vec<(&Subject, &Checked)> = run
        .findings()
        .filter(|(sub, c)| sub.group == group && pattern.matches(&c.diff.path))
        .collect();

    // The Python functions behind the answer: those named by a key on the path, or all of them.
    if let Some((_, Value::Object(fns))) = run.fns.iter().find(|(g, _)| *g == group) {
        let keys: Vec<&str> = found
            .iter()
            .flat_map(|(_, c)| c.diff.path.segs().iter())
            .filter_map(|s| if let Seg::Key(k) = s { Some(k.as_str()) } else { None })
            .collect();
        let named: Vec<(&String, &Value)> =
            fns.iter().filter(|(k, _)| keys.contains(&k.as_str())).collect();
        let shown: Vec<(&String, &Value)> =
            if named.is_empty() { fns.iter().collect() } else { named };
        out.push_str("  python:\n");
        for (k, v) in shown {
            out.push_str(&format!(
                "    {k}: {}\n",
                v.as_str().map_or_else(|| v.to_string(), str::to_string)
            ));
        }
    }
    if found.is_empty() {
        out.push_str("  no differences there\n");
        return out;
    }
    out.push_str(&format!("  {} difference(s):\n", found.len()));
    for (sub, c) in found {
        out.push_str(&diff_lines(sub, c, 400));
        if c.verdict == Verdict::Unexplained && c.near.is_empty() {
            out.push_str("      no intended entry points here (`suggest` writes a stub)\n");
        }
    }
    out
}
