//! `cargo golden`: checks and blesses the golden sets (DESIGN.md 9.6).
//!
//! ```text
//! cargo golden check [--out FILE]   compare this build's answers with the committed files;
//!                                   --out writes a report for the cross-target comparison
//! cargo golden bless [SET]          rewrite rng.json, libm.json, ruleset.json, uniques.json,
//!                                   filters.json, gen.json, states.json, convert.json and
//!                                   turns.json from this build, or only SET's file; a set that
//!                                   depends on a stage still pending is refused
//! cargo golden states               rewrite the checked-in states of testdata/states/ from the
//!                                   generator (then bless); only when the save format changes
//! cargo golden diff A B             compare two --out reports
//! ```
//!
//! Exit codes: 0 all match, 1 something differs or a set named to bless is refused, 2 a usage
//! or I/O error.
//!
//! A set that depends on the engine's stages (`turns`) is blessed only when none of them is
//! pending (DESIGN.md 9.6): until then `check` computes it without comparing it, and `bless`
//! leaves it out and says why, or refuses it by name.
//!
//! `pyfmt.json` holds Python's answers and is written only by `scripts/refcheck/pyfmt_vectors.py`;
//! `bless` leaves it alone. Bless only after a deliberate change (a new `Purpose`, a `libm` or
//! toolchain bump, a change to the ruleset data), and say why in the commit.

#![forbid(unsafe_code)]
// A command-line tool: it reports on the console and reads and writes the golden files.
#![allow(
    clippy::disallowed_macros,
    clippy::disallowed_methods,
    reason = "a command-line tool that prints its report and reads and writes the golden files"
)]

use std::process::ExitCode;

use citar_testkit::golden;
use serde_json::Value;

const USAGE: &str =
    "usage: golden check [--out FILE] | golden bless [SET] | golden states | golden diff A B";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["check"] => check(None),
        ["check", "--out", path] => check(Some(path)),
        ["bless"] => bless(None),
        ["bless", set] => bless(Some(set)),
        ["states"] => write_states(),
        ["diff", a, b] => diff(a, b),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn check(out: Option<&str>) -> ExitCode {
    let reports = golden::check_all();
    let mut failed = false;
    for r in &reports {
        if !r.waiting.is_empty() && r.problems.is_empty() {
            println!(
                "{:<6} waiting   {}  ({} stages it depends on are pending: not compared)",
                r.name,
                r.computed,
                r.waiting.len()
            );
        } else if r.problems.is_empty() {
            println!("{:<6} ok        {}", r.name, r.computed);
        } else {
            failed = true;
            println!("{:<6} DIFFERS   {}", r.name, r.computed);
            for p in &r.problems {
                println!("    {p}");
            }
        }
    }
    if let Some(path) = out {
        let report = golden::report_json(&reports);
        let text = serde_json::to_string_pretty(&report).unwrap_or_default() + "\n";
        if let Err(e) = std::fs::write(path, text) {
            eprintln!("golden: cannot write {path}: {e}");
            return ExitCode::from(2);
        }
    }
    if failed {
        println!(
            "golden: a set differs from its committed file. If this build is right, `cargo golden \
             bless` (rng, libm, ruleset, uniques, filters, gen, states, convert, turns) or \
             scripts/refcheck/pyfmt_vectors.py (pyfmt), and say why."
        );
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn bless(only: Option<&str>) -> ExitCode {
    let wanted = |file: &str| only.is_none_or(|set| file.strip_suffix(".json") == Some(set));
    let refused: Vec<(&str, String)> =
        golden::bless_refusals().into_iter().filter(|(file, _)| wanted(file)).collect();
    for (file, why) in &refused {
        println!("golden: not blessing {file}: {why}");
    }
    let files: Vec<(&str, String)> =
        golden::blessed_files().into_iter().filter(|(file, _)| wanted(file)).collect();
    if only.is_some() && files.is_empty() {
        if refused.is_empty() {
            eprintln!("golden: no set {} to bless", only.unwrap_or_default());
            return ExitCode::from(2);
        }
        return ExitCode::from(1);
    }
    let dir = golden::golden_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("golden: cannot create {}: {e}", dir.display());
        return ExitCode::from(2);
    }
    for (file, text) in files {
        let path = dir.join(file);
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("golden: cannot write {}: {e}", path.display());
            return ExitCode::from(2);
        }
        println!("golden: wrote {}", path.display());
    }
    // Blessing cannot fix a chi-square bound or Python's answers, so check what was written.
    check(None)
}

fn write_states() -> ExitCode {
    let dir = golden::states::states_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("golden: cannot create {}: {e}", dir.display());
        return ExitCode::from(2);
    }
    for (file, bytes) in golden::states::generated() {
        let path = dir.join(file);
        if let Err(e) = std::fs::write(&path, bytes) {
            eprintln!("golden: cannot write {}: {e}", path.display());
            return ExitCode::from(2);
        }
        println!("golden: wrote {}", path.display());
    }
    ExitCode::SUCCESS
}

fn read_report(path: &str) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{path} is not valid JSON: {e}"))
}

fn diff(a: &str, b: &str) -> ExitCode {
    let (ra, rb) = match (read_report(a), read_report(b)) {
        (Ok(ra), Ok(rb)) => (ra, rb),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("golden: {e}");
            return ExitCode::from(2);
        }
    };
    let empty = serde_json::Map::new();
    let sets_a = ra.get("sets").and_then(Value::as_object).unwrap_or(&empty);
    let sets_b = rb.get("sets").and_then(Value::as_object).unwrap_or(&empty);
    let mut names: Vec<&String> = sets_a.keys().chain(sets_b.keys()).collect();
    names.sort();
    names.dedup();
    let mut differ = false;
    for name in names {
        let ca = sets_a.get(name).and_then(|s| s.get("computed"));
        let cb = sets_b.get(name).and_then(|s| s.get("computed"));
        if ca == cb {
            println!("{name:<6} same");
        } else {
            differ = true;
            println!(
                "{name:<6} DIFFERS: {} vs {}",
                ca.unwrap_or(&Value::Null),
                cb.unwrap_or(&Value::Null)
            );
        }
    }
    if differ { ExitCode::from(1) } else { ExitCode::SUCCESS }
}
