//! `cargo xtask perf`: perfgate (DESIGN.md 9.7, 10; package 1e-03).
//!
//! Runs the criterion suites of citar-bench (`kernels`, `turns`, `io`), each pinned to one core,
//! then checks what they wrote to `<target>/perf/<suite>.json` against
//! `crates/citar-bench/thresholds.toml`:
//! - every budget holds a measure, at or under `hard` (1.5) times the budget; a budget that needs
//!   the corpus is skipped when a suite ran without it;
//! - every pass round is at least `ratio` (20) times faster than Python's
//!   (`refcheck/perf/python-turns.json`), within the backstops, and within the target budgets
//!   times `hard`.
//!
//! It prints each measure against its budget, and each pass round with its Python ratio.
//! `--check` skips the run and checks the files a run left; `--suite <name>` runs one suite (the
//! check still reads every file); arguments after `--` go to the benches (a criterion filter,
//! `--quick`). Exit 0 all within, 1 over, 2 could not run.
//!
//! The file is read with a copy of citar-bench's reader: xtask builds without the engine.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde::Deserialize;

const SUITES: [&str; 3] = ["kernels", "turns", "io"];

#[derive(Debug, Deserialize)]
struct Budget {
    id: String,
    suite: String,
    budget: String,
    row: String,
    #[serde(default)]
    corpus: bool,
}

#[derive(Debug, Deserialize)]
struct Backstop {
    case_prefix: String,
    turn: u32,
    budget: String,
}

#[derive(Debug, Deserialize)]
struct Target {
    case_prefix: String,
    points: Vec<(u32, f64)>,
}

#[derive(Debug, Deserialize)]
struct PassRound {
    ratio: f64,
    backstop: Vec<Backstop>,
    target: Vec<Target>,
}

#[derive(Debug, Deserialize)]
struct Thresholds {
    hard: f64,
    budget: Vec<Budget>,
    pass_round: PassRound,
}

#[derive(Debug, Deserialize)]
struct Measure {
    ns: f64,
}

#[derive(Debug, Deserialize)]
struct Written {
    corpus: bool,
    #[serde(default)]
    overflow_checks: bool,
    core: Option<usize>,
    measures: BTreeMap<String, Measure>,
}

/// A duration as the file writes it, in nanoseconds.
fn parse_ns(s: &str) -> Option<f64> {
    let s = s.trim();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (num, unit) = s.split_at(split);
    let x: f64 = num.parse().ok()?;
    let scale = match unit.trim() {
        "ns" => 1.0,
        "us" | "µs" => 1e3,
        "ms" => 1e6,
        "s" => 1e9,
        _ => return None,
    };
    Some(x * scale)
}

fn show(ns: f64) -> String {
    if ns < 1e3 {
        format!("{ns:.1} ns")
    } else if ns < 1e6 {
        format!("{:.2} µs", ns / 1e3)
    } else if ns < 1e9 {
        format!("{:.2} ms", ns / 1e6)
    } else {
        format!("{:.2} s", ns / 1e9)
    }
}

/// The target budget of a size's pass round at `turn`, in nanoseconds: the points interpolated,
/// held beyond the first and the last.
fn target_at(t: &Target, turn: u32) -> Option<f64> {
    let (first, last) = (t.points.first()?, t.points.last()?);
    if turn <= first.0 {
        return Some(first.1 * 1e6);
    }
    for w in t.points.windows(2) {
        let ((t0, m0), (t1, m1)) = (w[0], w[1]);
        if turn <= t1 {
            let f = f64::from(turn - t0) / f64::from((t1 - t0).max(1));
            return Some((m0 + (m1 - m0) * f) * 1e6);
        }
    }
    Some(last.1 * 1e6)
}

/// The workspace's target directory: `CARGO_TARGET_DIR`, else what `cargo metadata` says.
fn target_dir(root: &Path) -> Result<PathBuf, String> {
    if let Some(d) = std::env::var_os("CARGO_TARGET_DIR") {
        let d = PathBuf::from(d);
        return Ok(if d.is_absolute() { d } else { root.join(d) });
    }
    #[derive(Deserialize)]
    struct Meta {
        target_directory: PathBuf,
    }
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    let m: Meta =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: {e}"))?;
    Ok(m.target_directory)
}

/// Runs the suites through `cargo bench`, the bench profile (release: fat LTO, overflow checks).
fn run_suites(root: &Path, suites: &[&str], extra: &[String]) -> Result<(), String> {
    let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.args(["bench", "--locked", "-p", "citar-bench"]).current_dir(root);
    for s in suites {
        cmd.args(["--bench", s]);
    }
    if !extra.is_empty() {
        cmd.arg("--").args(extra);
    }
    println!("perfgate: running {cmd:?}");
    let status = cmd.status().map_err(|e| format!("cargo bench: {e}"))?;
    // A suite that finds a measure over its hard limit fails its run; the check below says which.
    if !status.success() {
        println!("perfgate: cargo bench exited with {status}");
    }
    Ok(())
}

/// Everything the suites wrote, by suite.
fn read_results(dir: &Path) -> BTreeMap<String, Written> {
    let mut out = BTreeMap::new();
    for s in SUITES {
        let p = dir.join(format!("{s}.json"));
        if let Some(w) =
            std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Written>(&b).ok())
        {
            out.insert(s.to_owned(), w);
        }
    }
    out
}

/// Python's pass rounds in milliseconds, by state.
fn python_rounds(root: &Path) -> BTreeMap<String, f64> {
    let p = root.join("refcheck/perf/python-turns.json");
    let Some(doc) =
        std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
    else {
        return BTreeMap::new();
    };
    doc.get("rounds")
        .and_then(serde_json::Value::as_object)
        .map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), v.get("ms")?.as_f64()?))).collect())
        .unwrap_or_default()
}

/// A state's case and turn from its name, `<case>/t<turn>`.
fn case_turn(name: &str) -> Option<(&str, u32)> {
    let (case, t) = name.rsplit_once("/t")?;
    Some((case, t.parse().ok()?))
}

/// Checks the budgets; returns the problems.
fn check_budgets(
    t: &Thresholds,
    results: &BTreeMap<String, Written>,
) -> Result<Vec<String>, String> {
    let mut problems = Vec::new();
    println!("\n{:<48} {:>11} {:>11} {:>7}  status", "measure", "median", "budget", "x");
    for b in &t.budget {
        let budget =
            parse_ns(&b.budget).ok_or_else(|| format!("{}: bad budget {}", b.id, b.budget))?;
        let written = results.get(&b.suite);
        let got = written.and_then(|w| w.measures.get(&b.id));
        let status = match (written, got) {
            (_, Some(m)) => {
                let x = m.ns / budget;
                let s = if x > t.hard {
                    problems.push(format!(
                        "{}: {} is {x:.2} times its budget {} ({})",
                        b.id,
                        show(m.ns),
                        b.budget,
                        b.row
                    ));
                    "OVER THE HARD LIMIT"
                } else if x > 1.0 {
                    "over the budget, within the hard limit"
                } else {
                    "ok"
                };
                println!("{:<48} {:>11} {:>11} {:>7.2}  {s}", b.id, show(m.ns), b.budget, x);
                continue;
            }
            (Some(w), None) if b.corpus && !w.corpus => "skipped: needs the corpus",
            (None, None) => {
                problems.push(format!("{}: the {} suite wrote nothing", b.id, b.suite));
                "MISSING (the suite did not run)"
            }
            (Some(_), None) => {
                problems.push(format!("{}: not measured by the {} suite", b.id, b.suite));
                "MISSING"
            }
        };
        println!("{:<48} {:>11} {:>11} {:>7}  {status}", b.id, "-", b.budget, "-");
    }
    Ok(problems)
}

/// Checks the pass rounds; returns the problems.
fn check_rounds(
    t: &Thresholds,
    turns: Option<&Written>,
    python: &BTreeMap<String, f64>,
) -> Result<Vec<String>, String> {
    let mut problems = Vec::new();
    let Some(w) = turns else {
        return Ok(vec!["pass rounds: the turns suite wrote nothing".to_owned()]);
    };
    let rounds: Vec<(&str, f64)> = w
        .measures
        .iter()
        .filter_map(|(k, m)| Some((k.strip_prefix("turn/pass_round/")?, m.ns)))
        .collect();
    if rounds.is_empty() {
        return Ok(vec!["pass rounds: none measured".to_owned()]);
    }
    let mut worst: Option<(f64, &str)> = None;
    let mut over_target = 0usize;
    let mut no_python = 0usize;
    for &(name, ns) in &rounds {
        let Some((case, turn)) = case_turn(name) else { continue };
        match python.get(name) {
            Some(&ms) => {
                let ratio = ms * 1e6 / ns;
                if worst.is_none_or(|(x, _)| ratio < x) {
                    worst = Some((ratio, name));
                }
                if ratio < t.pass_round.ratio {
                    problems.push(format!(
                        "turn/pass_round/{name}: {} is only {ratio:.1} times Python's {ms:.1} ms",
                        show(ns)
                    ));
                }
            }
            None => no_python += 1,
        }
        for b in &t.pass_round.backstop {
            let limit = parse_ns(&b.budget).ok_or_else(|| format!("bad backstop {}", b.budget))?;
            if case.starts_with(&b.case_prefix) && turn == b.turn && ns > limit {
                problems.push(format!(
                    "turn/pass_round/{name}: {} is over the {} backstop",
                    show(ns),
                    b.budget
                ));
            }
        }
        for tg in &t.pass_round.target {
            if !case.starts_with(&tg.case_prefix) {
                continue;
            }
            if let Some(budget) = target_at(tg, turn) {
                let x = ns / budget;
                if x > 1.0 {
                    over_target += 1;
                }
                if x > t.hard {
                    problems.push(format!(
                        "turn/pass_round/{name}: {} is {x:.2} times its target {}",
                        show(ns),
                        show(budget)
                    ));
                }
            }
        }
    }
    println!(
        "\npass rounds: {} states{}; the lowest ratio to Python {}; {} over a target budget \
         (within {}x)",
        rounds.len(),
        if w.corpus { " (the corpus)" } else { " (the committed fixtures; no corpus)" },
        worst.map_or_else(|| "(no Python timings)".to_owned(), |(x, n)| format!("{x:.0}x on {n}")),
        over_target,
        t.hard
    );
    if no_python > 0 {
        println!("pass rounds: {no_python} states have no Python timing");
    }
    // The ten slowest and the ten lowest ratios, for the report.
    let mut by_time: Vec<&(&str, f64)> = rounds.iter().collect();
    by_time.sort_by(|a, b| b.1.total_cmp(&a.1));
    println!("slowest rounds:");
    for (name, ns) in by_time.iter().take(10) {
        let ratio =
            python.get(*name).map_or_else(String::new, |ms| format!(" ({:.0}x)", ms * 1e6 / ns));
        println!("  {name}: {}{ratio}", show(*ns));
    }
    let mut by_ratio: Vec<(f64, &str, f64)> =
        rounds.iter().filter_map(|&(n, ns)| Some((python.get(n)? * 1e6 / ns, n, ns))).collect();
    by_ratio.sort_by(|a, b| a.0.total_cmp(&b.0));
    println!("lowest ratios to Python:");
    for (x, name, ns) in by_ratio.iter().take(10) {
        println!("  {name}: {} ({x:.0}x)", show(*ns));
    }
    Ok(problems)
}

pub fn run(root: &Path, args: &[String]) -> ExitCode {
    let mut check_only = false;
    let mut suites: Vec<&str> = SUITES.to_vec();
    let mut extra: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--check" => check_only = true,
            "--suite" => match it.next().map(String::as_str) {
                Some(s) if SUITES.contains(&s) => {
                    suites = vec![SUITES[SUITES.iter().position(|x| *x == s).unwrap_or(0)]]
                }
                other => {
                    eprintln!("perfgate: --suite takes one of {SUITES:?}, not {other:?}");
                    return ExitCode::from(2);
                }
            },
            "--" => extra.extend(it.by_ref().cloned()),
            other => {
                eprintln!("perfgate: unknown argument {other}");
                return ExitCode::from(2);
            }
        }
    }
    let text = match std::fs::read_to_string(root.join("crates/citar-bench/thresholds.toml")) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("perfgate: thresholds.toml: {e}");
            return ExitCode::from(2);
        }
    };
    let t: Thresholds = match toml::from_str(&text) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("perfgate: thresholds.toml: {e}");
            return ExitCode::from(2);
        }
    };
    let dir = match target_dir(root) {
        Ok(d) => d.join("perf"),
        Err(e) => {
            eprintln!("perfgate: {e}");
            return ExitCode::from(2);
        }
    };
    if !check_only && let Err(e) = run_suites(root, &suites, &extra) {
        eprintln!("perfgate: {e}");
        return ExitCode::from(2);
    }
    let results = read_results(&dir);
    for (s, w) in &results {
        println!(
            "perfgate: {s}: {} measures, {}, corpus {}, overflow checks {}",
            w.measures.len(),
            w.core.map_or_else(|| "not pinned".to_owned(), |c| format!("core {c}")),
            if w.corpus { "on" } else { "off" },
            if w.overflow_checks { "on" } else { "off" }
        );
    }
    let python = python_rounds(root);
    let mut problems = match check_budgets(&t, &results) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("perfgate: {e}");
            return ExitCode::from(2);
        }
    };
    match check_rounds(&t, results.get("turns"), &python) {
        Ok(p) => problems.extend(p),
        Err(e) => {
            eprintln!("perfgate: {e}");
            return ExitCode::from(2);
        }
    }
    if problems.is_empty() {
        println!("\nperfgate: every budget holds (hard limit {}x)", t.hard);
        ExitCode::SUCCESS
    } else {
        println!("\nperfgate: {} problem(s):", problems.len());
        for p in &problems {
            println!("  {p}");
        }
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_thresholds_read_and_name_every_suite() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let text = std::fs::read_to_string(root.join("crates/citar-bench/thresholds.toml"))
            .expect("thresholds.toml");
        let t: Thresholds = toml::from_str(&text).expect("it parses");
        assert!((t.hard - 1.5).abs() < 1e-9);
        for b in &t.budget {
            assert!(SUITES.contains(&b.suite.as_str()), "{}: suite {}", b.id, b.suite);
            assert!(parse_ns(&b.budget).is_some(), "{}: {}", b.id, b.budget);
        }
        assert!(t.pass_round.backstop.iter().all(|b| parse_ns(&b.budget).is_some()));
    }

    #[test]
    fn a_state_name_splits_into_case_and_turn() {
        assert_eq!(
            case_turn("small-continents-normal-s1025/t280"),
            Some(("small-continents-normal-s1025", 280))
        );
        assert_eq!(case_turn("nothing"), None);
    }

    #[test]
    fn targets_interpolate() {
        let t = Target {
            case_prefix: "small-".into(),
            points: vec![(50, 2.0), (150, 8.0), (300, 20.0)],
        };
        let at = |turn| target_at(&t, turn).map(|x| (x / 1e3).round() as i64);
        assert_eq!(at(1), Some(2000));
        assert_eq!(at(100), Some(5000));
        assert_eq!(at(280), Some(18400));
        assert_eq!(at(400), Some(20000));
    }
}
