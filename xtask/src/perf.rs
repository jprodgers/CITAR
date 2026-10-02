//! `cargo xtask perf`: perfgate (DESIGN.md 9.7, 10; package 1e-03).
//!
//! Runs the criterion suites of citar-bench (`kernels`, `turns`, `io`), each pinned to one core,
//! then checks what they wrote to `<target>/perf/<suite>.json` against
//! `crates/citar-bench/thresholds.toml`. The `games` suite (whole bot games, DESIGN.md P2.4.2)
//! runs only when named (`--suite games`): it takes minutes, and is timed with the other lane
//! paused. Its file is read with the others whenever a run left one.
//!
//! - every budget holds a measure, at or under `hard` (1.5) times the budget; a budget that needs
//!   the corpus is skipped when a suite ran without it, and a report-only budget (the `games`
//!   rows until package 2-07) is printed against its budget and fails nothing;
//! - every pass round is at least `ratio` (20) times faster than Python's
//!   (`refcheck/perf/python-turns.json`), within the backstops, and within the target budgets
//!   times `hard`.
//!
//! It prints each measure against its budget, and each pass round with its Python ratio.
//! `--check` skips the run and checks the files a run left; `--suite <name>` runs one suite (the
//! check still reads every file, and says which an earlier run left); arguments after `--` go to
//! the benches (one criterion filter, which names a part or starts a measure's id, and options
//! such as `--quick`). Exit 0 all within, 1 over, 2 could not run: `cargo bench` failed with no
//! measure over its hard limit to say why, or a suite it ran did not write its file in this run
//! (each suite stamps its file with the run's id, `CITAR_PERF_RUN`), so nothing is checked
//! against numbers an earlier build left.
//!
//! The file is read with a copy of citar-bench's reader: xtask builds without the engine.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde::Deserialize;

/// Every suite perfgate knows: the files it reads, and what `--suite` may name.
const SUITES: [&str; 4] = ["kernels", "turns", "io", "games"];

/// The suites a run without `--suite` runs: not `games`, which plays whole bot games for minutes.
const DEFAULT_SUITES: [&str; 3] = ["kernels", "turns", "io"];

#[derive(Debug, Deserialize)]
struct Budget {
    id: String,
    suite: String,
    budget: String,
    row: String,
    #[serde(default)]
    corpus: bool,
    #[serde(default)]
    report_only: bool,
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
    /// The perfgate run that wrote it, if one did.
    #[serde(default)]
    run: Option<String>,
    /// Whether a filtered run kept an earlier run's measures of the parts it did not run.
    #[serde(default)]
    merged: bool,
    measures: BTreeMap<String, Measure>,
}

/// The environment variable a suite reads its run's id from (citar-bench's `RUN_ENV`).
const RUN_ENV: &str = "CITAR_PERF_RUN";

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

/// The arguments of `extra` criterion takes as its filter: those that are no option, nor an
/// option's value (citar-bench's `filter_args`).
fn filters_of(extra: &[String]) -> Vec<&str> {
    const WITH_VALUE: [&str; 14] = [
        "--save-baseline",
        "--baseline",
        "--baseline-lenient",
        "--load-baseline",
        "--sample-size",
        "--warm-up-time",
        "--measurement-time",
        "--nresamples",
        "--noise-threshold",
        "--confidence-level",
        "--significance-level",
        "--profile-time",
        "--color",
        "--format",
    ];
    let mut out = Vec::new();
    let mut skip = false;
    for a in extra {
        if skip {
            skip = false;
        } else if WITH_VALUE.contains(&a.as_str()) {
            skip = true;
        } else if !a.starts_with('-') {
            out.push(a.as_str());
        }
    }
    out
}

/// An id for this run, which each suite writes into its file.
fn run_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{now}-{}", std::process::id())
}

/// Runs the suites through `cargo bench`, the bench profile (release: fat LTO, one codegen unit),
/// every one of them whatever another does (`--no-fail-fast`); whether `cargo bench` succeeded.
fn run_suites(root: &Path, suites: &[&str], extra: &[String], run: &str) -> Result<bool, String> {
    let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.args(["bench", "--locked", "--no-fail-fast", "-p", "citar-bench"])
        .current_dir(root)
        .env(RUN_ENV, run);
    for s in suites {
        cmd.args(["--bench", s]);
    }
    if !extra.is_empty() {
        cmd.arg("--").args(extra);
    }
    println!("perfgate: running {cmd:?}");
    let status = cmd.status().map_err(|e| format!("cargo bench: {e}"))?;
    // A suite that finds a measure over its hard limit fails its run after writing its file; the
    // check below says which.
    if !status.success() {
        println!("perfgate: cargo bench exited with {status}");
    }
    Ok(status.success())
}

/// The suites of `ran` whose file this run did not write: they failed before writing, or did not
/// build.
fn not_written<'a>(
    results: &BTreeMap<String, Written>,
    ran: &[&'a str],
    run: &str,
) -> Vec<&'a str> {
    ran.iter()
        .copied()
        .filter(|s| results.get(*s).is_none_or(|w| w.run.as_deref() != Some(run)))
        .collect()
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
                let s = if b.report_only {
                    if x > 1.0 { "report-only: over the budget" } else { "report-only: ok" }
                } else if x > t.hard {
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
            (_, None) if b.report_only => "report-only: not measured",
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
    // (name, time, target, times the target) of each round over its target
    let mut over_target: Vec<(&str, f64, f64, f64)> = Vec::new();
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
                    over_target.push((name, ns, budget, x));
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
        over_target.len(),
        t.hard
    );
    for (name, ns, budget, x) in &over_target {
        println!("  over its target: {name}: {} against {} ({x:.2}x)", show(*ns), show(*budget));
    }
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
    let mut suites: Vec<&str> = DEFAULT_SUITES.to_vec();
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
    let filters = filters_of(&extra);
    if filters.len() > 1 {
        eprintln!("perfgate: criterion takes one filter, not {filters:?}");
        return ExitCode::from(2);
    }
    let run = run_id();
    let bench_ok = if check_only {
        true
    } else {
        match run_suites(root, &suites, &extra, &run) {
            Ok(ok) => ok,
            Err(e) => {
                eprintln!("perfgate: {e}");
                return ExitCode::from(2);
            }
        }
    };
    let results = read_results(&dir);
    for (s, w) in &results {
        let earlier = !check_only && w.run.as_deref() != Some(run.as_str());
        println!(
            "perfgate: {s}: {} measures, {}, corpus {}, overflow checks {}{}{}",
            w.measures.len(),
            w.core.map_or_else(|| "not pinned".to_owned(), |c| format!("core {c}")),
            if w.corpus { "on" } else { "off" },
            if w.overflow_checks { "on" } else { "off" },
            if w.merged {
                ", with an earlier run's measures of the parts it did not run"
            } else {
                ""
            },
            if earlier { " (left by an earlier run)" } else { "" }
        );
    }
    if !check_only {
        let missing = not_written(&results, &suites, &run);
        if !missing.is_empty() {
            eprintln!(
                "perfgate: {} did not write its results in this run (see cargo bench's output); \
                 nothing is checked against an earlier run's numbers",
                missing.join(", ")
            );
            return ExitCode::from(2);
        }
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
    if problems.is_empty() && !bench_ok {
        eprintln!(
            "\nperfgate: cargo bench failed, and no measure is over its hard limit to say why \
             (see its output)"
        );
        ExitCode::from(2)
    } else if problems.is_empty() {
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
        assert!(DEFAULT_SUITES.iter().all(|s| SUITES.contains(s)));
        assert!(!DEFAULT_SUITES.contains(&"games"), "the games suite runs only when named");
    }

    #[test]
    fn a_report_only_budget_fails_nothing() {
        let t: Thresholds = toml::from_str(
            r#"hard = 1.5
               [[budget]]
               id = "game/bot_small_330"
               suite = "games"
               budget = "16 s"
               row = "a small bot game"
               report_only = true
               [[budget]]
               id = "game/hard"
               suite = "games"
               budget = "1 s"
               row = "a hard one"
               [pass_round]
               ratio = 20.0
               backstop = []
               target = []"#,
        )
        .expect("it parses");
        let mut measures = BTreeMap::new();
        measures.insert("game/bot_small_330".to_owned(), Measure { ns: 40e9 });
        measures.insert("game/hard".to_owned(), Measure { ns: 2e9 });
        let w = Written {
            corpus: false,
            overflow_checks: false,
            core: Some(0),
            run: None,
            merged: false,
            measures,
        };
        let results = BTreeMap::from([("games".to_owned(), w)]);
        let problems = check_budgets(&t, &results).expect("checked");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].starts_with("game/hard"), "{problems:?}");
        let none = check_budgets(&t, &BTreeMap::new()).expect("checked");
        assert_eq!(none.len(), 1, "a missing report-only measure fails nothing: {none:?}");
    }

    #[test]
    fn only_a_file_this_run_wrote_counts() {
        let w = |run: Option<&str>| Written {
            corpus: true,
            overflow_checks: false,
            core: Some(0),
            run: run.map(str::to_owned),
            merged: false,
            measures: BTreeMap::new(),
        };
        let mut results = BTreeMap::new();
        results.insert("kernels".to_owned(), w(Some("now")));
        results.insert("turns".to_owned(), w(Some("before")));
        results.insert("io".to_owned(), w(None));
        assert_eq!(not_written(&results, &DEFAULT_SUITES, "now"), vec!["turns", "io"]);
        assert_eq!(not_written(&results, &["games"], "now"), vec!["games"]);
        results.remove("kernels");
        assert_eq!(not_written(&results, &["kernels"], "now"), vec!["kernels"]);
    }

    #[test]
    fn the_filters_are_what_is_no_option() {
        let args: Vec<String> =
            ["--quick", "path", "--sample-size", "10", "combat"].map(str::to_owned).to_vec();
        assert_eq!(filters_of(&args), vec!["path", "combat"]);
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
