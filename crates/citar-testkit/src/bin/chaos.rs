//! `cargo chaos`: whole games of `RandomAgent` turns with tool calls of every kind mixed in, every
//! step checked for the properties P1 to P7 (DESIGN.md 9.5; `citar_testkit::chaos`).
//!
//! ```text
//! cargo chaos [--seconds N] [--seed S] [--from-fixtures] [--rounds R] [--calls C]
//!             [--bug NAME] [--out DIR]
//!                          play games for N seconds (60), from seed S (1), on generated maps
//!                          or the fixtures (every city flagged for a citizen recheck), each
//!                          for at most R rounds (30) with up to C calls (12) before each
//!                          seat's turn; NAME plants a seeded bug (`game::seeded`); each
//!                          failure is written to DIR (target/chaos) as a replay file
//! cargo chaos --replay FILE
//!                          play a replay file again and say whether it fails the same way
//! ```
//!
//! Exit codes: 0 no failure (or, for a replay, the failure did not come back), 1 a failure (or
//! a replay that failed again, the same way or not), 2 a usage or I/O error.

#![forbid(unsafe_code)]
// A command-line tool: it reports on the console, reads the clock and writes the replay files.
#![allow(
    clippy::disallowed_macros,
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "a command-line tool that prints its report, keeps to a time budget and writes files"
)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use citar_engine::game::seeded::SeededBug;
use citar_testkit::chaos::{self, Settings};
use citar_testkit::fixtures;

const USAGE: &str = "usage: chaos [--seconds N] [--seed S] [--from-fixtures] [--rounds R] \
                     [--calls C] [--bug NAME] [--out DIR] | chaos --replay FILE";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse(&args) {
        Ok(Command::Run { settings, seconds, out }) => run(&settings, seconds, &out),
        Ok(Command::Replay(path)) => replay(&path),
        Err(e) => {
            eprintln!("chaos: {e}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

enum Command {
    Run { settings: Settings, seconds: u64, out: PathBuf },
    Replay(PathBuf),
}

fn parse(args: &[String]) -> Result<Command, String> {
    let mut settings = Settings::default();
    let mut seconds = 60;
    let mut out = default_out();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |what: &str| it.next().cloned().ok_or(format!("{a} needs {what}"));
        let number =
            |v: String| v.parse::<u64>().map_err(|_| format!("{a} takes a number, not {v}"));
        match a.as_str() {
            "--replay" => return Ok(Command::Replay(PathBuf::from(value("a file")?))),
            "--seconds" => seconds = number(value("a number")?)?,
            "--seed" => settings.seed = number(value("a number")?)?,
            "--rounds" => {
                settings.rounds =
                    u32::try_from(number(value("a number")?)?).map_err(|e| e.to_string())?;
            }
            "--calls" => {
                settings.calls_per_turn =
                    u32::try_from(number(value("a number")?)?).map_err(|e| e.to_string())?;
            }
            "--from-fixtures" => settings.from_fixtures = true,
            "--bug" => {
                let name = value("a bug's name")?;
                let known: Vec<&str> = SeededBug::ALL.iter().map(|b| b.name()).collect();
                settings.bug = Some(
                    SeededBug::named(&name)
                        .ok_or(format!("no seeded bug {name}; there are {}", known.join(", ")))?,
                );
            }
            "--out" => out = PathBuf::from(value("a folder")?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Command::Run { settings, seconds, out })
}

/// `target/chaos` under the build's target folder.
fn default_out() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| fixtures::repo_root().join("target"), PathBuf::from)
        .join("chaos")
}

fn run(settings: &Settings, seconds: u64, out: &std::path::Path) -> ExitCode {
    let started = Instant::now();
    let budget = Duration::from_secs(seconds);
    let mut keep_going = || started.elapsed() < budget;
    let report = match chaos::run(settings, &mut keep_going) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("chaos: {e}");
            return ExitCode::from(2);
        }
    };
    let from = if settings.from_fixtures { "fixtures" } else { "generated maps" };
    println!(
        "chaos: seed {}, {from}{}: {} games, {} rounds, {} steps, {} calls ({} refused) in {:.0} s",
        settings.seed,
        settings.bug.map(|b| format!(", bug {}", b.name())).unwrap_or_default(),
        report.games,
        report.rounds,
        report.steps,
        report.calls,
        report.refused,
        started.elapsed().as_secs_f64()
    );
    if report.failures.is_empty() {
        println!("chaos: 0 failures");
        return ExitCode::SUCCESS;
    }
    if let Err(e) = std::fs::create_dir_all(out) {
        eprintln!("chaos: cannot create {}: {e}", out.display());
        return ExitCode::from(2);
    }
    println!("chaos: {} failures", report.failures.len());
    for (i, r) in report.failures.iter().enumerate() {
        let path = out.join(format!("chaos-s{}-{i}.json", settings.seed));
        let written =
            chaos::to_json(r).and_then(|t| std::fs::write(&path, t).map_err(|e| e.to_string()));
        let f = &r.failure;
        println!(
            "  {:?} at step {} of {:?}: {}\n    replay: {}",
            f.property,
            f.step,
            r.start,
            f.what,
            match written {
                Ok(()) => path.display().to_string(),
                Err(e) => format!("not written: {e}"),
            }
        );
    }
    ExitCode::from(1)
}

fn replay(path: &std::path::Path) -> ExitCode {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("chaos: cannot read {}: {e}", path.display());
            return ExitCode::from(2);
        }
    };
    let r = match chaos::from_json(&text) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("chaos: {}: {e}", path.display());
            return ExitCode::from(2);
        }
    };
    match chaos::replay(&r) {
        Err(e) => {
            eprintln!("chaos: {e}");
            ExitCode::from(2)
        }
        Ok(None) => {
            println!("chaos: the replay played its {} steps without failing", r.steps.len());
            ExitCode::SUCCESS
        }
        Ok(Some(f)) => {
            let same = f == r.failure;
            println!(
                "chaos: {} at step {}: {:?}: {}",
                if same { "reproduced" } else { "failed differently" },
                f.step,
                f.property,
                f.what
            );
            if !same {
                println!(
                    "  recorded: {:?} at step {}: {}",
                    r.failure.property, r.failure.step, r.failure.what
                );
            }
            ExitCode::from(1)
        }
    }
}
