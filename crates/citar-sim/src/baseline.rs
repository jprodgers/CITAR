//! The statistical baseline's line (DESIGN.md P2.4.1): one JSON line per game, in the shape
//! `scripts/refcheck/baseline.py` writes and `scripts/refcheck/summarize.py` reads, so the Rust
//! runs and the committed Python baselines (`refcheck/baseline/python/*.jsonl`) compare line for
//! line. The types refuse unknown keys, so the schema cannot drift: every committed Python line
//! reads back to the same JSON value.
//!
//! A finished game carries its identity (`IDENTITY`: `i`, `seed`, `size`, `map_type`,
//! `barbarians`, `speed`, `turn_limit`), how it ended, and each major's rows at the checkpoints
//! (`"100"`, `"200"`, `"300"`, or `"10"`... for a smoke run) and at `"end"`; a crashed game, its
//! identity and the crash.
//!
//! Package 2-00a wrote the line's types; package 2-04 writes the writer (the rotation, the tally,
//! resuming, refusing another build's file, the workers).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One civilization at one checkpoint: the engine's stats row (absent once it is dead, when the
/// score reads 0) and the war and capture tallies.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointRow {
    pub alive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cities: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub population: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub techs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub military: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub era: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policies: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub land: Option<i64>,
    pub wars_declared: u32,
    pub wars_declared_on_majors: u32,
    pub wars_declared_on_others: u32,
    pub cities_captured: u32,
    pub cities_lost: u32,
    /// The last recorded turn, on the `"end"` row only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
}

/// One major civilization of a finished game.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineCiv {
    pub pid: u32,
    pub nation: String,
    /// What its seat played with, three decimals.
    pub aggression: f64,
    pub alive: bool,
    pub eliminated_turn: Option<u32>,
    pub final_score: i64,
    /// By checkpoint (`"100"`, ...) and `"end"`.
    pub at: BTreeMap<String, CheckpointRow>,
}

/// A finished game's line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineGame {
    pub i: u32,
    pub seed: u64,
    pub size: String,
    pub map_type: String,
    pub barbarians: String,
    pub speed: String,
    pub turn_limit: Option<u32>,
    pub players: u32,
    pub turns: u32,
    pub winner: Option<u32>,
    pub victory: Option<String>,
    pub civs: Vec<BaselineCiv>,
    pub bot_errors: u32,
    /// The build that played it: `citar_bot::build_id` (Python wrote its engine's source hash).
    pub engine: String,
    /// The bot: `basic-1` (Python wrote its source hash).
    pub bot: String,
    pub seconds: f64,
    pub cpu_s: f64,
}

/// A crashed or timed-out game's line: played again when the run resumes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineCrash {
    pub i: u32,
    pub seed: u64,
    pub size: String,
    pub map_type: String,
    pub barbarians: String,
    pub speed: String,
    pub turn_limit: Option<u32>,
    pub engine: String,
    pub bot: String,
    pub crash: String,
    pub trace: String,
    /// How long the game ran before it crashed. Absent when the worker itself died or stalled
    /// (baseline.py:234 writes that line from the parent, which never timed the game).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
}

/// One line of a baseline file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BaselineLine {
    Game(BaselineGame),
    Crash(BaselineCrash),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Every line of the four committed Python baselines reads as a [`BaselineLine`] and writes
    /// back to the same JSON value: the type is the files' schema.
    #[test]
    fn every_committed_python_line_round_trips() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../refcheck/baseline/python");
        let mut lines = 0;
        for name in ["small", "std-large", "gargantuan", "smoke"] {
            let path = dir.join(format!("{name}.jsonl"));
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
                let value: serde_json::Value = serde_json::from_str(line).expect("JSON");
                let typed: BaselineLine = serde_json::from_str(line)
                    .unwrap_or_else(|e| panic!("{name}.jsonl line {}: {e}", n + 1));
                assert!(matches!(typed, BaselineLine::Game(_)), "{name}: finished games only");
                let back = serde_json::to_value(&typed).expect("serialises");
                assert_eq!(back, value, "{name}.jsonl line {}", n + 1);
                lines += 1;
            }
        }
        assert_eq!(lines, 60 + 24 + 2 + 2);
    }

    #[test]
    fn a_crash_line_reads_as_one() {
        let line = r#"{"i": 3, "seed": 5003, "size": "small", "map_type": "pangaea",
                       "barbarians": "normal", "speed": "Quick", "turn_limit": null,
                       "engine": "x", "bot": "y", "crash": "GameTimeout: stuck", "trace": "",
                       "seconds": 1.5}"#;
        let typed: BaselineLine = serde_json::from_str(line).expect("a crash line");
        assert!(matches!(typed, BaselineLine::Crash(ref c) if c.crash.starts_with("GameTimeout")));
        assert!(matches!(typed, BaselineLine::Crash(ref c) if c.seconds == Some(1.5)));
    }

    /// The line baseline.py writes for a worker that died or stalled (`{**_ident(spec), **code,
    /// "crash": why, "trace": ""}`, baseline.py:234) has no `seconds`, and reads as a crash that
    /// writes back to the same JSON value.
    #[test]
    fn a_dead_workers_crash_line_has_no_seconds() {
        let line = r#"{"i":7,"seed":5007,"size":"small","map_type":"fractal","barbarians":"off",
                       "speed":"Quick","turn_limit":null,"engine":"x","bot":"y",
                       "crash":"its worker process died (killed, or out of memory?)","trace":""}"#;
        let value: serde_json::Value = serde_json::from_str(line).expect("JSON");
        let typed: BaselineLine = serde_json::from_str(line).expect("a crash line");
        assert!(matches!(typed, BaselineLine::Crash(ref c) if c.seconds.is_none()));
        assert_eq!(serde_json::to_value(&typed).expect("serialises"), value);
    }
}
