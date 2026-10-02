//! `cargo golden`: checks and blesses the golden sets (DESIGN.md 9.6).
//!
//! ```text
//! cargo golden check [--long] [--out FILE] [--states DIR]
//!                                   compare this build's answers with the committed files; with
//!                                   --long, the long set too (the nightly run's); --out writes a
//!                                   report for the cross-target comparison; --states writes, for
//!                                   each game that leaves its file, its state at the first round
//!                                   that differs and at the round before
//! cargo golden bless [SET]          rewrite rng.json, libm.json, ruleset.json, uniques.json,
//!                                   filters.json, gen.json, states.json, convert.json,
//!                                   turns.json, maps.json, newgame.json, load.json, pass.json
//!                                   and random.json from this build, or only SET's file; `bless
//!                                   long` writes long.json (about 80 s); a set that depends on a
//!                                   stage still pending is refused
//! cargo golden states               rewrite the checked-in states of testdata/states/ from the
//!                                   generator (then bless); only when the save format changes
//! cargo golden dump SET:GAME [TURN] [--out FILE]
//!                                   play a golden game on this machine to the end of round TURN
//!                                   (or as it starts) and write its state as save JSON
//! cargo golden dump --list          the games dump plays
//! cargo golden diff A B             compare two --out reports set by set, naming the first row
//!                                   of each list where they part; or two states, place by place
//! ```
//!
//! Exit codes: 0 all match, 1 something differs or a set named to bless is refused, 2 a usage
//! or I/O error.
//!
//! A set that depends on the engine's stages (`turns`, `pass`, `random`, `long`) is blessed only
//! when none of them is pending (DESIGN.md 9.6): until then `check` computes it without
//! comparing it, and `bless` leaves it out and says why, or refuses it by name.
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

use std::path::PathBuf;
use std::process::ExitCode;

use citar_engine::base::ids::Turn;
use citar_testkit::golden::{self, divergence, dump};
use serde_json::Value;

const USAGE: &str = "usage: golden check [--long] [--out FILE] [--states DIR] | golden bless [SET] \
                     | golden states | golden dump SET:GAME [TURN] [--out FILE] | golden dump \
                     --list | golden diff A B";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["check", rest @ ..] => match check_options(rest) {
            Ok((long, out, states)) => check(long, out, states),
            Err(e) => usage(&e),
        },
        ["bless"] => bless(None),
        ["bless", set] => bless(Some(set)),
        ["states"] => write_states(),
        ["dump", "--list"] => list_games(),
        ["dump", name, rest @ ..] => match dump_options(rest) {
            Ok((turn, out)) => write_dump(name, turn, out),
            Err(e) => usage(&e),
        },
        ["diff", a, b] => diff(a, b),
        _ => usage(""),
    }
}

fn usage(e: &str) -> ExitCode {
    if !e.is_empty() {
        eprintln!("golden: {e}");
    }
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn check_options<'a>(args: &[&'a str]) -> Result<(bool, Option<&'a str>, Option<&'a str>), String> {
    let (mut long, mut out, mut states) = (false, None, None);
    let mut it = args.iter();
    while let Some(&a) = it.next() {
        match a {
            "--long" => long = true,
            "--out" => out = Some(*it.next().ok_or("--out needs a file")?),
            "--states" => states = Some(*it.next().ok_or("--states needs a folder")?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok((long, out, states))
}

fn dump_options<'a>(args: &[&'a str]) -> Result<(Option<Turn>, Option<&'a str>), String> {
    let (mut turn, mut out) = (None, None);
    let mut it = args.iter();
    while let Some(&a) = it.next() {
        match a {
            "--out" => out = Some(*it.next().ok_or("--out needs a file")?),
            t if turn.is_none() => {
                turn = Some(t.parse::<Turn>().map_err(|_| format!("{t} is not a turn"))?);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok((turn, out))
}

fn check(long: bool, out: Option<&str>, states: Option<&str>) -> ExitCode {
    if let Err(e) = divergence::set_dir(states.map(PathBuf::from)) {
        eprintln!("golden: {e}");
        return ExitCode::from(2);
    }
    let mut reports = golden::check_all();
    if long {
        reports.extend(golden::check_long());
    }
    let mut failed = false;
    for r in &reports {
        if !r.waiting.is_empty() && r.problems.is_empty() {
            println!(
                "{:<7} waiting   {}  ({} stages it depends on are pending: not compared)",
                r.name,
                r.computed,
                r.waiting.len()
            );
        } else if r.problems.is_empty() {
            println!("{:<7} ok        {}", r.name, r.computed);
        } else {
            failed = true;
            println!("{:<7} DIFFERS   {}", r.name, r.computed);
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
        if let Some(dir) = states {
            println!("golden: the states where games first differ are in {dir} (divergence.jsonl)");
        }
        println!(
            "golden: a set differs from its committed file. If this build is right, `cargo golden \
             bless` (rng, libm, ruleset, uniques, filters, gen, states, convert, turns, maps, \
             newgame, load, pass, random), `cargo golden bless long` (long) or \
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
    let long = only == Some("long");
    let files: Vec<(&str, String)> = if long {
        golden::blessed_long()
    } else {
        golden::blessed_files().into_iter().filter(|(file, _)| wanted(file)).collect()
    };
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
    check(long, None, None)
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

fn list_games() -> ExitCode {
    match dump::names() {
        Ok(names) => {
            for n in names {
                println!("{n}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("golden: {e}");
            ExitCode::from(2)
        }
    }
}

fn write_dump(name: &str, turn: Option<Turn>, out: Option<&str>) -> ExitCode {
    let bytes = match dump::dump(name, turn) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("golden: {e}");
            return ExitCode::from(1);
        }
    };
    let (set, game) = name.split_once(':').unwrap_or((name, ""));
    let path = out.map_or_else(|| divergence::file_name(set, game, turn, ""), ToOwned::to_owned);
    if let Err(e) = std::fs::write(&path, bytes) {
        eprintln!("golden: cannot write {path}: {e}");
        return ExitCode::from(2);
    }
    println!("golden: wrote {path}");
    ExitCode::SUCCESS
}

fn read_json(path: &str) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{path} is not valid JSON: {e}"))
}

fn diff(a: &str, b: &str) -> ExitCode {
    let (ra, rb) = match (read_json(a), read_json(b)) {
        (Ok(ra), Ok(rb)) => (ra, rb),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("golden: {e}");
            return ExitCode::from(2);
        }
    };
    match (ra.get("sets"), rb.get("sets")) {
        (Some(_), Some(_)) => diff_reports(&ra, &rb),
        (None, None) => diff_states(&ra, &rb),
        _ => {
            eprintln!("golden: {a} and {b} are not both reports nor both states");
            ExitCode::from(2)
        }
    }
}

/// Two states, place by place.
fn diff_states(a: &Value, b: &Value) -> ExitCode {
    const SHOWN: usize = 200;
    let (lines, count) = divergence::diff_states(a, b, SHOWN);
    if count == 0 {
        println!("golden: the states are identical");
        return ExitCode::SUCCESS;
    }
    for l in &lines {
        println!("{l}");
    }
    if count > lines.len() {
        println!("... and {} more", count - lines.len());
    }
    println!("golden: {count} places differ");
    ExitCode::from(1)
}

/// Two reports, set by set, and in each set that differs the first row of each list.
fn diff_reports(ra: &Value, rb: &Value) -> ExitCode {
    let empty = serde_json::Map::new();
    let sets_a = ra.get("sets").and_then(Value::as_object).unwrap_or(&empty);
    let sets_b = rb.get("sets").and_then(Value::as_object).unwrap_or(&empty);
    let mut names: Vec<&String> = sets_a.keys().chain(sets_b.keys()).collect();
    names.sort();
    names.dedup();
    let mut differ = false;
    for name in names {
        let (sa, sb) = (sets_a.get(name), sets_b.get(name));
        let ca = sa.and_then(|s| s.get("computed"));
        let cb = sb.and_then(|s| s.get("computed"));
        if ca == cb {
            println!("{name:<7} same");
            continue;
        }
        differ = true;
        println!(
            "{name:<7} DIFFERS: {} vs {}",
            ca.unwrap_or(&Value::Null),
            cb.unwrap_or(&Value::Null)
        );
        for line in list_diffs(name, sa, sb) {
            println!("    {line}");
        }
    }
    if differ { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

/// For each list two reports of one set hold, where their rows first part, with the committed
/// file's row there, which names the game and turn for a whole-game set.
fn list_diffs(set: &str, a: Option<&Value>, b: Option<&Value>) -> Vec<String> {
    let rows =
        |s: Option<&Value>| s.and_then(|s| s.get("rows")).and_then(Value::as_object).cloned();
    let (Some(ra), Some(rb)) = (rows(a), rows(b)) else {
        return vec!["(a report without row hashes: from a build before package 1e-02)".to_owned()];
    };
    let committed = golden::committed(set);
    let mut out = Vec::new();
    for (key, ha) in &ra {
        let (Some(ha), Some(hb)) = (ha.as_array(), rb.get(key).and_then(Value::as_array)) else {
            continue;
        };
        let parted = ha.iter().zip(hb).position(|(x, y)| x != y);
        let differing = ha.iter().zip(hb).filter(|(x, y)| x != y).count();
        let first = parted.or_else(|| (ha.len() != hb.len()).then(|| ha.len().min(hb.len())));
        let Some(i) = first else { continue };
        let row = committed
            .as_ref()
            .and_then(|c| c.get(key))
            .and_then(Value::as_array)
            .and_then(|rows| rows.get(i))
            .map_or_else(|| "not in the committed file".to_owned(), ToString::to_string);
        out.push(format!(
            "{key}: {differing} of {} rows differ ({} against {} rows); the first is [{i}], committed {row}",
            ha.len().max(hb.len()),
            ha.len(),
            hb.len(),
        ));
    }
    if out.is_empty() {
        out.push("every list's rows agree: the set's other answers differ".to_owned());
    }
    out
}
