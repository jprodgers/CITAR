//! A baseline run's options, and the game each index stands for (`baseline.py:39-48`,
//! `common.py:121-144, 292-305`).
//!
//! Game `i` uses seed `seed + i` and the `i`-th combination of sizes × map types × barbarian
//! settings (the first varying slowest, as `itertools.product` gives them), so a run can stop and
//! resume, and two runs of the same options play the same games. What makes game `i` the game it
//! is, [`IDENTITY`], is written on every line and checked when a run resumes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use citar_engine::base::ids::Turn;
use citar_engine::rules::Ruleset;
use serde_json::{Map, Value, json};

/// The checkpoints of a run: the war and capture totals are taken at the end of these turns.
pub const CHECKPOINTS: [Turn; 3] = [100, 200, 300];

/// The checkpoints of a smoke run, whose games last 40 turns.
pub const SMOKE_CHECKPOINTS: [Turn; 3] = [10, 20, 30];

/// The map types a run rotates through by default (`common.MAP_TYPES`).
pub const MAP_TYPES: [&str; 5] = ["continents", "pangaea", "archipelago", "inland_sea", "fractal"];

/// What makes game `i` the game it is: a file may only hold games whose fields match this run's
/// for the same `i` (`baseline.IDENTITY`).
pub const IDENTITY: [&str; 7] =
    ["i", "seed", "size", "map_type", "barbarians", "speed", "turn_limit"];

/// A game on a small map may run this many minutes (`common.BUDGET_MINUTES_SMALL`); others scale
/// by area. The budget catches a hang, not a slow game: a small game takes seconds.
const BUDGET_MINUTES_SMALL: f64 = 60.0;

/// The least budget of any game, in minutes.
const BUDGET_MINUTES_LEAST: f64 = 20.0;

/// Seconds past the longest budget before the run calls itself stuck: Python's watchdog grace
/// plus five minutes (`common.stall_limit`). A Rust game cannot be interrupted inside a turn, so
/// the stall limit is what catches one stuck there.
const STALL_GRACE_S: f64 = 300.0 + 300.0;

/// A run's options (`baseline.py`'s command line, its defaults included).
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// Two short duel games (40 turns, checkpoints 10, 20 and 30) into `<name>-smoke`, made
    /// afresh every time.
    pub smoke: bool,
    /// Games in the run.
    pub games: u32,
    /// Game 0's seed; game `i` uses `seed + i`.
    pub seed: u64,
    /// Map sizes, rotated.
    pub sizes: Vec<String>,
    /// Map types, rotated.
    pub maps: Vec<String>,
    /// Barbarian settings, rotated.
    pub barbarians: Vec<String>,
    pub speed: String,
    /// The last turn; 0 for the speed's own (330 on Quick).
    pub turn_limit: u32,
    /// The most minutes a game may take; 0 for 60 on a small map, scaled by the map's area.
    pub max_minutes: f64,
    /// The output file's name, without `.jsonl`; `None` for `rust-<build id>-<bot>`.
    pub name: Option<String>,
    /// Games played at once, each on its own thread.
    pub workers: usize,
    /// Where the file goes: the repository's `refcheck/baseline` by default.
    pub dir: PathBuf,
    /// Run the engine's invariants at every settle, and record a game that breaks one as a
    /// crash (needs a build with the checks).
    pub checks: bool,
    /// Parameter overrides for every seat's bot, as a profile gives them; `None` for the
    /// defaults.
    pub params: Option<Value>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            smoke: false,
            games: 100,
            seed: 5000,
            sizes: vec!["small".to_owned()],
            maps: MAP_TYPES.iter().map(|&m| m.to_owned()).collect(),
            barbarians: vec!["normal".to_owned()],
            speed: "Quick".to_owned(),
            turn_limit: 0,
            max_minutes: 0.0,
            name: None,
            workers: default_workers(),
            dir: default_dir(),
            checks: false,
            params: None,
        }
    }
}

impl Options {
    /// The options a run plays with: a smoke run's games, sizes, maps and turn limit replace the
    /// options' (`baseline.py:204-206`).
    #[must_use]
    pub fn effective(&self) -> Self {
        let mut o = self.clone();
        if o.smoke {
            o.games = 2;
            o.sizes = vec!["duel".to_owned()];
            o.maps = vec!["continents".to_owned(), "pangaea".to_owned()];
            o.turn_limit = 40;
        }
        o
    }

    /// The checkpoints of this run.
    #[must_use]
    pub const fn checkpoints(&self) -> [Turn; 3] {
        if self.smoke { SMOKE_CHECKPOINTS } else { CHECKPOINTS }
    }

    /// The combinations of size, map type and barbarian setting, in the order games take them.
    fn combos(&self) -> Vec<(&str, &str, &str)> {
        let mut out = Vec::new();
        for s in &self.sizes {
            for m in &self.maps {
                for b in &self.barbarians {
                    out.push((s.as_str(), m.as_str(), b.as_str()));
                }
            }
        }
        out
    }
}

/// Worker threads by default: all cores but four, which leaves the machine usable meanwhile
/// (`common.default_workers`).
#[must_use]
pub fn default_workers() -> usize {
    std::thread::available_parallelism().map_or(2, std::num::NonZero::get).saturating_sub(4).max(1)
}

/// The repository's `refcheck/baseline`, found from the working directory up (the folder that
/// holds both `Cargo.toml` and `refcheck/`); `refcheck/baseline` under the working directory
/// when there is none.
#[must_use]
pub fn default_dir() -> PathBuf {
    let here = std::env::current_dir().unwrap_or_default();
    here.ancestors()
        .find(|d| d.join("Cargo.toml").is_file() && d.join("refcheck").is_dir())
        .unwrap_or(&here)
        .join("refcheck")
        .join("baseline")
}

/// One game of a run.
#[derive(Clone, Debug, PartialEq)]
pub struct GameSpec {
    pub i: u32,
    pub seed: u64,
    pub size: String,
    pub map_type: String,
    pub barbarians: String,
    pub speed: String,
    /// `None` for the speed's own.
    pub turn_limit: Option<u32>,
    /// The turns whose totals the line records.
    pub checkpoints: [Turn; 3],
    /// The most the game may take.
    pub budget: Duration,
}

impl GameSpec {
    /// Its identity, as every line of it carries it: [`IDENTITY`]'s keys in order.
    #[must_use]
    pub fn identity(&self) -> Map<String, Value> {
        let values = [
            json!(self.i),
            json!(self.seed),
            json!(self.size),
            json!(self.map_type),
            json!(self.barbarians),
            json!(self.speed),
            json!(self.turn_limit),
        ];
        IDENTITY.iter().map(|&k| k.to_owned()).zip(values).collect()
    }

    /// The configuration of its all-bot game (`common.game_config`): the size's number of
    /// majors, each a bot with a nation drawn at random, on Prince.
    #[must_use]
    pub fn config(&self, rules: &Ruleset) -> Value {
        let n = rules.constants().map_size(&self.size).map_or(0, |s| s.players);
        let seat = json!({"controller": "bot", "nation": null});
        json!({
            "map_type": self.map_type, "map_size": self.size, "seed": self.seed,
            "barbarians": self.barbarians, "speed": self.speed, "difficulty": "Prince",
            "turn_limit": self.turn_limit,
            "players": vec![seat; usize::from(n)],
        })
    }
}

/// Game `i` of a run with options `o` (already [`Options::effective`]).
#[must_use]
pub fn game_spec(rules: &Ruleset, o: &Options, i: u32) -> GameSpec {
    let combos = o.combos();
    let (size, map_type, barbarians) =
        combos.get(i as usize % combos.len().max(1)).copied().unwrap_or(("small", "", ""));
    GameSpec {
        i,
        seed: o.seed + u64::from(i),
        size: size.to_owned(),
        map_type: map_type.to_owned(),
        barbarians: barbarians.to_owned(),
        speed: o.speed.clone(),
        turn_limit: (o.turn_limit > 0).then_some(o.turn_limit),
        checkpoints: o.checkpoints(),
        budget: time_budget(rules, size, o.max_minutes),
    }
}

/// How long a game on a `size` map may run: `minutes` if given, else an hour on a small map
/// scaled by the map's area, and never under 20 minutes (`common.time_budget`).
#[must_use]
pub fn time_budget(rules: &Ruleset, size: &str, minutes: f64) -> Duration {
    if minutes > 0.0 {
        return Duration::from_secs_f64(minutes * 60.0);
    }
    let k = rules.constants();
    let area = |s: &str| {
        k.map_size(s).map_or(0.0, |m| f64::from(u32::from(m.width) * u32::from(m.height)))
    };
    let small = area("small");
    let scaled = if small > 0.0 { BUDGET_MINUTES_SMALL * area(size) / small } else { 0.0 };
    Duration::from_secs_f64(scaled.max(BUDGET_MINUTES_LEAST) * 60.0)
}

/// How long the run waits for any game to finish before it calls itself stuck: the longest
/// budget and ten minutes (`common.stall_limit`).
#[must_use]
pub fn stall_limit(budgets: impl IntoIterator<Item = Duration>) -> Duration {
    let longest = budgets.into_iter().max().unwrap_or_default();
    longest + Duration::from_secs_f64(STALL_GRACE_S)
}

/// The output file of a run named `name` in `dir`.
#[must_use]
pub fn output_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_i_takes_the_ith_combination_and_seed_plus_i() {
        let r = Ruleset::shared();
        let o = Options {
            sizes: vec!["small".into(), "standard".into()],
            maps: vec!["continents".into(), "pangaea".into()],
            barbarians: vec!["off".into(), "normal".into()],
            ..Options::default()
        };
        let combo = |i| {
            let s = game_spec(r, &o, i);
            (s.seed, s.size, s.map_type, s.barbarians)
        };
        // itertools.product: the barbarians vary fastest, the sizes slowest.
        assert_eq!(combo(0), (5000, "small".into(), "continents".into(), "off".into()));
        assert_eq!(combo(1), (5001, "small".into(), "continents".into(), "normal".into()));
        assert_eq!(combo(2), (5002, "small".into(), "pangaea".into(), "off".into()));
        assert_eq!(combo(4), (5004, "standard".into(), "continents".into(), "off".into()));
        assert_eq!(combo(9), (5009, "small".into(), "continents".into(), "normal".into()));
        let s = game_spec(r, &o, 3);
        assert_eq!(s.turn_limit, None);
        assert_eq!(s.checkpoints, CHECKPOINTS);
        assert_eq!(
            Value::Object(s.identity()).to_string(),
            r#"{"i":3,"seed":5003,"size":"small","map_type":"pangaea","barbarians":"normal","speed":"Quick","turn_limit":null}"#
        );
    }

    #[test]
    fn a_smoke_run_is_two_short_duels() {
        let o = Options { smoke: true, games: 50, ..Options::default() }.effective();
        assert_eq!((o.games, o.turn_limit), (2, 40));
        let r = Ruleset::shared();
        let (a, b) = (game_spec(r, &o, 0), game_spec(r, &o, 1));
        assert_eq!((a.size.as_str(), a.map_type.as_str()), ("duel", "continents"));
        assert_eq!((b.size.as_str(), b.map_type.as_str()), ("duel", "pangaea"));
        assert_eq!(a.checkpoints, SMOKE_CHECKPOINTS);
        assert_eq!(a.turn_limit, Some(40));
        let players = a.config(r)["players"].as_array().map(Vec::len);
        assert_eq!(players, Some(2));
    }

    #[test]
    fn budgets_scale_with_the_map_and_have_a_floor() {
        let r = Ruleset::shared();
        assert_eq!(time_budget(r, "small", 0.0), Duration::from_secs(3600));
        let minutes = |size| time_budget(r, size, 0.0).as_secs_f64() / 60.0;
        let duel = minutes("duel");
        assert!((duel - 60.0 * (44.0 * 28.0) / (60.0 * 38.0)).abs() < 1e-6, "{duel}");
        let large = minutes("large");
        assert!((large - 60.0 * (92.0 * 58.0) / (60.0 * 38.0)).abs() < 1e-6, "{large}");
        // No map of the ruleset is small enough to meet the floor; an unknown one has no area.
        assert_eq!(time_budget(r, "nowhere", 0.0), Duration::from_secs(20 * 60), "the floor");
        assert_eq!(time_budget(r, "gargantuan", 2.5), Duration::from_secs(150));
        let stall = stall_limit([Duration::from_secs(60), Duration::from_secs(3600)]);
        assert_eq!(stall, Duration::from_secs(3600 + 600));
    }
}
