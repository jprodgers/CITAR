//! The baseline writer (package 2-04, gates 1 and 4): a smoke run plays two duels whose lines
//! read back as `BaselineLine`s; a run killed midway resumes, keeping its finished lines and
//! playing the rest again; a file from another build, or from other options, is refused.
//!
//! The runs write under the target's temporary folder, never under `refcheck/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use citar_bot::build_id;
use citar_engine::rules::Ruleset;
use citar_sim::baseline::{self, Options, Outcome};
use citar_sim::{BaselineGame, BaselineLine};
use serde_json::Value;

/// A fresh, empty folder for one test.
fn folder(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("baseline-{test}"));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    }
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    dir
}

/// Short duels: games `0..games` on continents and pangaea in turn, `turns` turns each.
fn duels(dir: &Path, name: &str, games: u32, turns: u32) -> Options {
    Options {
        games,
        sizes: vec!["duel".into()],
        maps: vec!["continents".into(), "pangaea".into()],
        turn_limit: turns,
        name: Some(name.into()),
        workers: 2,
        dir: dir.to_owned(),
        ..Options::default()
    }
}

/// Runs a baseline, keeping what it says.
fn run(o: &Options) -> (Outcome, Vec<String>) {
    let mut said = Vec::new();
    let outcome = baseline::run(o, &mut |l| said.push(l.to_owned()));
    (outcome, said)
}

fn lines(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.lines().map(str::to_owned).collect()
}

/// Each game's finished line, by index (the last one, as `summarize.py` counts a game).
fn finished(path: &Path) -> BTreeMap<u32, BaselineGame> {
    let mut out = BTreeMap::new();
    for l in lines(path) {
        if let Ok(BaselineLine::Game(g)) = serde_json::from_str::<BaselineLine>(&l) {
            out.insert(g.i, g);
        }
    }
    out
}

/// A finished line with its timings left out: what a game played again must reproduce.
fn untimed(g: &BaselineGame) -> BaselineGame {
    BaselineGame { seconds: 0.0, cpu_s: 0.0, ..g.clone() }
}

/// Gates 1 and 4: `--smoke` plays two duels of 40 turns into `<name>-smoke`, and each line reads
/// as a `BaselineLine` and writes back to the same JSON value.
#[test]
fn a_smoke_run_plays_two_duels_whose_lines_read_back() {
    let dir = folder("smoke");
    let o = Options {
        smoke: true,
        name: Some("rust".into()),
        workers: 2,
        dir: dir.clone(),
        ..Options::default()
    };
    let (outcome, said) = run(&o);
    assert_eq!(outcome, Outcome::Done, "{said:#?}");
    let path = dir.join("rust-smoke.jsonl");
    let text = lines(&path);
    assert_eq!(text.len(), 2, "{text:#?}");
    let engine = build_id(Ruleset::shared());
    let mut seen = Vec::new();
    for l in &text {
        let value: Value = serde_json::from_str(l).expect("JSON");
        let typed: BaselineLine = serde_json::from_str(l).expect("a baseline line");
        assert_eq!(serde_json::to_value(&typed).expect("serialises"), value, "{l}");
        let BaselineLine::Game(g) = typed else { panic!("a crash: {l}") };
        assert_eq!((g.size.as_str(), g.speed.as_str(), g.turn_limit), ("duel", "Quick", Some(40)));
        assert_eq!((g.turns, g.players, g.bot_errors), (40, 2, 0));
        assert_eq!((g.engine.as_str(), g.bot.as_str()), (engine.as_str(), "basic-1"));
        assert_eq!(g.victory.as_deref(), Some("Time"));
        assert_eq!(g.civs.len(), 2);
        for c in &g.civs {
            let keys: Vec<&str> = c.at.keys().map(String::as_str).collect();
            assert_eq!(keys, ["10", "20", "30", "end"]);
            assert_eq!(c.at["end"].turn, Some(40));
            assert!(c.at["10"].alive && c.at["10"].cities.is_some_and(|n| n >= 1));
            assert!((0.25..=0.75).contains(&c.aggression));
        }
        seen.push((g.i, g.seed, g.map_type));
    }
    seen.sort();
    assert_eq!(seen, [(0, 5000, "continents".into()), (1, 5001, "pangaea".into())]);
    assert!(said.iter().any(|l| l.starts_with("Done in ")), "{said:#?}");

    // A smoke run starts afresh: run again, still two lines.
    assert_eq!(run(&o).0, Outcome::Done);
    assert_eq!(lines(&path).len(), 2);
}

/// Gate 4: a run that stopped midway (a finished game, a crashed one, a torn last line) resumes:
/// the finished line is kept and not played again, the crashed and the unwritten games are
/// played, and they come out as an uninterrupted run plays them.
#[test]
fn a_stopped_run_resumes_keeping_its_finished_lines() {
    let dir = folder("resume");
    let whole = duels(&dir, "whole", 3, 25);
    assert_eq!(run(&whole).0, Outcome::Done);
    let reference = finished(&dir.join("whole.jsonl"));
    assert_eq!(reference.len(), 3);

    // The same run, stopped: game 0 finished, game 1 crashed, game 2 torn mid-write.
    let path = dir.join("stopped.jsonl");
    let full = lines(&dir.join("whole.jsonl"));
    let by_i = |i: u32| {
        full.iter()
            .find(|l| serde_json::from_str::<Value>(l).is_ok_and(|v| v["i"] == i))
            .cloned()
            .expect("a line")
    };
    let mut crash: Value = serde_json::from_str(&by_i(1)).expect("JSON");
    let crash = serde_json::json!({
        "i": 1, "seed": crash["seed"].take(), "size": "duel", "map_type": "pangaea",
        "barbarians": "normal", "speed": "Quick", "turn_limit": 25,
        "engine": crash["engine"].take(), "bot": "basic-1",
        "crash": "GameTimeout: still playing at turn 7 after its 32.4-minute budget",
        "trace": "", "seconds": 1.5,
    });
    let game0 = by_i(0);
    let torn = &by_i(2)[..40];
    std::fs::write(&path, format!("{game0}\n{crash}\n{torn}")).expect("written");

    let stopped = duels(&dir, "stopped", 3, 25);
    let (outcome, said) = run(&stopped);
    assert_eq!(outcome, Outcome::Done, "{said:#?}");
    assert!(said[0].starts_with("2 game(s) to play into "), "{said:#?}");
    let after = lines(&path);
    assert_eq!(after[0], game0, "the finished line is kept as it was");
    assert_eq!(after[1], crash.to_string(), "the crash line is kept: the replay follows it");
    assert_eq!(after[2], torn, "the torn line is ended, not mended");
    assert_eq!(after.len(), 5);
    let games = finished(&path);
    assert_eq!(games.keys().copied().collect::<Vec<_>>(), [0, 1, 2]);
    let played_again = after[3..]
        .iter()
        .filter_map(|l| serde_json::from_str::<BaselineLine>(l).ok())
        .filter(|l| matches!(l, BaselineLine::Game(g) if g.i == 0))
        .count();
    assert_eq!(played_again, 0, "game 0 is not played again");
    for (i, g) in &games {
        assert_eq!(untimed(g), untimed(&reference[i]), "game {i} as the uninterrupted run");
    }

    // Nothing is left to play.
    let (outcome, said) = run(&stopped);
    assert_eq!(outcome, Outcome::Done);
    assert!(said[0].starts_with("0 game(s) to play into "), "{said:#?}");
    assert_eq!(lines(&path).len(), 5);
}

/// Gate 4: the CLI killed while it plays (after its first line is written), then run again,
/// finishes the run: every finished line is kept, the rest are played.
#[test]
fn a_run_killed_midway_resumes_through_the_cli() {
    let dir = folder("killed");
    let args = |extra: &[&str]| {
        let mut a: Vec<String> = [
            "baseline",
            "--games",
            "6",
            "--sizes",
            "duel",
            "--maps",
            "continents,pangaea",
            "--turn-limit",
            "40",
            "--workers",
            "1",
            "--name",
            "killed",
        ]
        .iter()
        .map(|&s| s.to_owned())
        .collect();
        a.push("--dir".into());
        a.push(dir.display().to_string());
        a.extend(extra.iter().map(|&s| s.to_owned()));
        a
    };
    let exe = env!("CARGO_BIN_EXE_citar-sim");
    let path = dir.join("killed.jsonl");
    let mut child = Command::new(exe)
        .args(args(&[]))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("citar-sim starts");
    let deadline = Instant::now() + Duration::from_secs(600);
    let first = loop {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if text.contains('\n') {
            break text;
        }
        assert!(Instant::now() < deadline, "no line within ten minutes");
        assert!(child.try_wait().expect("polls").is_none(), "it stopped before its first line");
        std::thread::sleep(Duration::from_millis(1));
    };
    child.kill().expect("killed");
    child.wait().expect("reaped");
    let at_kill = finished(&path);
    assert!(!at_kill.is_empty() && at_kill.len() < 6, "killed midway: {} done", at_kill.len());
    // The lines complete when it was read: a later one may have been torn by the kill.
    let complete = &first[..=first.rfind('\n').expect("a line")];
    let kept: Vec<String> = complete.lines().map(str::to_owned).collect();

    let status =
        Command::new(exe).args(args(&[])).stdout(Stdio::null()).status().expect("citar-sim runs");
    assert_eq!(status.code(), Some(0));
    let after = lines(&path);
    assert_eq!(after[..kept.len()], kept[..], "the lines written before the kill are kept");
    let games = finished(&path);
    assert_eq!(games.keys().copied().collect::<Vec<_>>(), (0..6).collect::<Vec<_>>());
    for (i, g) in &at_kill {
        assert_eq!(untimed(&games[i]), untimed(g), "game {i} finished before the kill");
        let n = after.iter().filter(|l| l.contains(&format!("\"i\":{i},"))).count();
        assert_eq!(n, 1, "game {i} was not played again");
    }
}

/// Gate 4: a file holding a game another build played is refused, and left as it was; so is one
/// whose game `i` had other options, and a line that is no game.
#[test]
fn a_file_from_another_build_or_other_options_is_refused() {
    let dir = folder("refused");
    let o = duels(&dir, "theirs", 2, 12);
    assert_eq!(run(&o).0, Outcome::Done);
    let path = dir.join("theirs.jsonl");
    // Game 0's line: the two workers finish in either order.
    let game0 =
        lines(&path).into_iter().find(|l| l.starts_with("{\"i\":0,")).expect("game 0 finished");

    let foreign = game0.replacen(&build_id(Ruleset::shared()), "000000000000", 1);
    std::fs::write(&path, format!("{foreign}\n")).expect("written");
    let (outcome, said) = run(&o);
    assert_eq!(outcome, Outcome::Refused);
    let all = said.join("\n");
    assert!(all.contains("one file must be one sample"), "{all}");
    assert!(all.contains("(game 0) was played by engine 000000000000, bot basic-1;"), "{all}");
    assert_eq!(lines(&path), [foreign], "nothing written");

    let other_seed = duels(&dir, "theirs", 2, 12);
    std::fs::write(&path, format!("{game0}\n")).expect("written");
    let o2 = Options { seed: 7000, ..other_seed };
    let (outcome, said) = run(&o2);
    assert_eq!(outcome, Outcome::Refused);
    let all = said.join("\n");
    assert!(all.contains("line 1 (game 0) has seed 5000 (this run: 7000)"), "{all}");

    std::fs::write(&path, "{\"i\": \"zero\"}\n").expect("written");
    let (outcome, said) = run(&o);
    assert_eq!(outcome, Outcome::Refused);
    assert!(said.join("\n").contains("line 1 is not a game of a baseline run"), "{said:#?}");
}

/// Options that name nothing are refused before anything is written.
#[test]
fn options_that_name_nothing_are_refused() {
    let dir = folder("names");
    let o = Options { sizes: vec!["enormous".into()], ..duels(&dir, "x", 1, 5) };
    let (outcome, said) = run(&o);
    assert_eq!(outcome, Outcome::Refused);
    assert!(said[0].contains("enormous"), "{said:#?}");
    let o = Options { maps: vec!["donut".into()], ..duels(&dir, "x", 1, 5) };
    assert_eq!(run(&o).0, Outcome::Refused);
    assert!(!dir.join("x.jsonl").exists());
}

/// A budget that is no number of minutes, and seeds that would pass the largest, are refused
/// before anything is written, in every build (a debug build once panicked on both, a release
/// build wrapped the seed to 0).
#[test]
fn budgets_and_seeds_that_cannot_be_played_are_refused() {
    let dir = folder("bounds");
    for minutes in [f64::INFINITY, f64::NAN, -1.0, 1e300] {
        let o = Options { max_minutes: minutes, ..duels(&dir, "x", 1, 5) };
        let (outcome, said) = run(&o);
        assert_eq!(outcome, Outcome::Refused, "{minutes}");
        assert!(said[0].starts_with("--max-minutes: "), "{said:#?}");
    }
    let o = Options { seed: u64::MAX, ..duels(&dir, "x", 2, 5) };
    let (outcome, said) = run(&o);
    assert_eq!(outcome, Outcome::Refused);
    assert!(said[0].contains("would pass the largest seed"), "{said:#?}");
    let o = Options { seed: u64::MAX, smoke: true, ..duels(&dir, "x", 1, 5) };
    assert_eq!(run(&o).0, Outcome::Refused, "a smoke run plays two seeds");
    assert!(!dir.join("x.jsonl").exists() && !dir.join("x-smoke.jsonl").exists());

    // The CLI says so and exits 2, without a panic.
    let exe = env!("CARGO_BIN_EXE_citar-sim");
    for args in [
        ["--max-minutes", "inf", "--games", "1"],
        ["--seed", "18446744073709551615", "--games", "2"],
    ] {
        let out = Command::new(exe)
            .arg("baseline")
            .args(args)
            .arg("--dir")
            .arg(&dir)
            .output()
            .expect("citar-sim runs");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(!stderr.contains("panicked"), "{args:?}: {stderr}");
    }
    assert_eq!(std::fs::read_dir(&dir).expect("the folder").count(), 0, "nothing written");
}

/// With the checks on (a debug or ci build has them), the games run the invariants at every
/// settle and finish clean, and every line says so (`"checks": true`, which
/// `summarize.py --checked` asks of each line). A file of checked games and one of unchecked
/// games are two samples: neither run adds to the other's file.
#[test]
fn a_run_with_the_checks_finishes_clean() {
    let dir = folder("checks");
    let o = Options { checks: true, ..duels(&dir, "checked", 2, 15) };
    let (outcome, said) = run(&o);
    if !baseline::CHECKS_BUILT {
        assert_eq!(outcome, Outcome::Refused);
        return;
    }
    assert_eq!(outcome, Outcome::Done, "{said:#?}");
    let path = dir.join("checked.jsonl");
    assert_eq!(finished(&path).len(), 2);
    for l in lines(&path) {
        let v: Value = serde_json::from_str(&l).expect("JSON");
        assert_eq!(v.get("checks"), Some(&Value::Bool(true)), "{l}");
    }
    let unchecked = Options { checks: false, ..o.clone() };
    let (outcome, said) = run(&unchecked);
    assert_eq!(outcome, Outcome::Refused);
    let all = said.join("\n");
    let on_off = "was played with the engine's invariants on; this run has them off";
    assert!(all.contains(on_off), "{all}");

    let plain = duels(&dir, "plain", 1, 6);
    assert_eq!(run(&plain).0, Outcome::Done);
    let path = dir.join("plain.jsonl");
    assert!(lines(&path).iter().all(|l| !l.contains("\"checks\"")), "no key when off");
    let (outcome, said) = run(&Options { checks: true, ..plain });
    assert_eq!(outcome, Outcome::Refused);
    let all = said.join("\n");
    let off_on = "was played with the engine's invariants off; this run has them on";
    assert!(all.contains(off_on), "{all}");
}
