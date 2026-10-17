//! A reference game as a job (design/phase4-helper.md P4.4.1): one game of citar-sim's
//! statistical baseline, played with the baseline's own seats and summed up by its own line
//! (`citar_sim::baseline::summary`), so `scripts/refcheck/summarize.py` reads a reference run's
//! lines as it reads a baseline's.
//!
//! [`ReferenceResult`] is the baseline's line ([`citar_sim::BaselineGame`]) without what is no
//! game's to decide: `seconds` and `cpu_s` (measurements, added outside the hash by whoever writes
//! the line) and `checks` (a job never runs the engine's invariants: they watch a game and never
//! change it, and the jobs build records whether a build has them). A panic is the outcome
//! `{crash: message}`, the message only (`T<turn> P<player>: panic: ...`, normalised), never a
//! crash line with its trace, and never played again: a replica must reproduce it.

use std::sync::Arc;

use citar_bot::{Tuning, VersionId};
use citar_engine::game::{EventBatch, Game};
use citar_engine::rules::Ruleset;
use citar_sim::Seats;
use citar_sim::baseline::{
    BaselineCiv, Context, GameSpec, Tally, bot_seats, code_of_params, summary,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::lab::normalise_message;
use crate::spec::{ReferenceSpec, SpecError};

/// A finished reference game: the baseline's line without its times (P4.4.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceResult {
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
    /// The build id that played it.
    pub engine: String,
    /// The bot: `basic-1`, with its overrides' fingerprint after a `+` when the spec has any.
    pub bot: String,
}

/// A reference game that panicked, as the wire carries it: `{crash: message}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceCrashed {
    pub crash: String,
}

/// How a reference game came out.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ReferenceOutcome {
    Finished(Box<ReferenceResult>),
    Crashed(ReferenceCrashed),
}

impl ReferenceOutcome {
    /// Reads an outcome as the wire carries it, refusing an unknown key anywhere: a crash if it
    /// has the key `crash`, else a finished game's line.
    ///
    /// # Errors
    /// What does not read.
    pub fn read(v: &Value) -> Result<Self, String> {
        if v.get("crash").is_some() {
            ReferenceCrashed::deserialize(v).map(Self::Crashed).map_err(|e| e.to_string())
        } else {
            ReferenceResult::deserialize(v)
                .map(|r| Self::Finished(Box::new(r)))
                .map_err(|e| e.to_string())
        }
    }
}

/// What a reference spec decides of its line before a turn is played, as the line writes it
/// (design/phase4-helper.md P4.4.4: a result is checked against its spec, and the `jobs` golden
/// set against a build whose id has moved): `i`, `seed`, `size`, `map_type`, `barbarians`,
/// `speed`, `turn_limit`, `bot_errors` (none in a finished game), `engine` (the build id) and
/// `bot` (`basic-1`, with its overrides' fingerprint under this build after a `+`), and each
/// civilization's aggression, which its seat and the seed spread.
#[derive(Clone, Debug, PartialEq)]
pub struct ReferenceFacts {
    /// Keys of the line's top.
    pub top: serde_json::Map<String, Value>,
    /// The game's seed.
    pub seed: u64,
}

/// The keys of a reference line that name the build that played it: they move whenever the
/// build id does, whether or not a game changes.
pub const BUILD_KEYS: [&str; 2] = ["engine", "bot"];

impl ReferenceFacts {
    /// What `spec` decides of its line under `rules`' build.
    ///
    /// # Errors
    /// Overrides `check_spec` should have refused.
    pub fn of(rules: &Ruleset, spec: &ReferenceSpec) -> Result<Self, SpecError> {
        let (code, _) = code_of_params(rules, &spec.params_value()).map_err(SpecError)?;
        let pairs = [
            ("i", Value::from(spec.i)),
            ("seed", Value::from(spec.seed)),
            ("size", Value::from(spec.size.clone())),
            ("map_type", Value::from(spec.map_type.clone())),
            ("barbarians", Value::from(spec.barbarians.clone())),
            ("speed", Value::from(spec.speed.clone())),
            ("turn_limit", serde_json::json!(spec.turn_limit)),
            ("bot_errors", Value::from(0)),
            ("engine", Value::from(code.engine)),
            ("bot", Value::from(code.bot)),
        ];
        let top = pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
        Ok(Self { top, seed: spec.seed })
    }

    /// The aggression the civilization in seat `pid` played with, as the line writes it.
    #[must_use]
    pub fn aggression(&self, pid: u32) -> Option<f64> {
        let pid = u8::try_from(pid).ok()?;
        let a = citar_sim::baseline::aggression(citar_engine::base::ids::PlayerId(pid), self.seed);
        Some(citar_engine::base::num::round_ndigits(a, 3))
    }
}

/// A reference game in play: the baseline's context and game, and its war and capture tally.
#[derive(Debug)]
pub struct ReferencePlay {
    cx: Context,
    game: GameSpec,
    tally: Tally,
}

impl ReferencePlay {
    /// The baseline's seats for `spec` under `rules`: a `basic-1` bot with the spec's overrides
    /// in every major's seat, its aggression spread by seat and seed (`citar_sim::baseline`).
    /// The seats are made once the game exists: `seats` makes them from it.
    ///
    /// # Errors
    /// Overrides `check_spec` should have refused.
    pub fn start(rules: &'static Ruleset, spec: &ReferenceSpec) -> Result<Self, SpecError> {
        let (code, tuning) = code_of_params(rules, &spec.params_value()).map_err(SpecError)?;
        let cx = Context { rules, code, version: VersionId::Basic1, tuning, checks: false };
        let game = spec.game();
        let tally = Tally::new(&game.checkpoints);
        Ok(Self { cx, game, tally })
    }

    /// The game's settings (`GameSpec::config`).
    #[must_use]
    pub fn config(&self) -> Value {
        self.game.config(self.cx.rules)
    }

    /// The seats of `g`, once it exists.
    #[must_use]
    pub fn seats(&self, g: &Game) -> Seats {
        let tuning: &Arc<Tuning> = &self.cx.tuning;
        bot_seats(g, self.game.seed, self.cx.version, tuning)
    }

    /// Counts one step's wars and captures.
    pub fn hear(&mut self, g: &Game, events: &EventBatch) {
        self.tally.count(g, events);
    }

    /// The finished game's line, without its times.
    #[must_use]
    pub fn result(&self, g: &Game) -> ReferenceResult {
        let line = summary(&self.cx, &self.game, g, &self.tally);
        ReferenceResult {
            i: line.i,
            seed: line.seed,
            size: line.size,
            map_type: line.map_type,
            barbarians: line.barbarians,
            speed: line.speed,
            turn_limit: line.turn_limit,
            players: line.players,
            turns: line.turns,
            winner: line.winner,
            victory: line.victory,
            civs: line.civs,
            bot_errors: line.bot_errors,
            engine: line.engine,
            bot: line.bot,
        }
    }

    /// The crash outcome of a panic whose record reads `record` (the runner's
    /// `T<turn> P<player>: panic: <message>`).
    #[must_use]
    pub fn crash(record: &str) -> ReferenceCrashed {
        ReferenceCrashed { crash: normalise_message(record) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outcome_reads_as_a_crash_or_a_line_and_refuses_the_times() {
        let crash = serde_json::json!({"crash": "T3 P1: panic: boom"});
        assert!(matches!(ReferenceOutcome::read(&crash), Ok(ReferenceOutcome::Crashed(_))));
        let mut traced = crash;
        traced["trace"] = serde_json::json!("at x.rs:1");
        assert!(ReferenceOutcome::read(&traced).expect_err("refused").contains("unknown field"));
        let line = serde_json::json!({
            "i": 0, "seed": 1, "size": "duel", "map_type": "continents", "barbarians": "normal",
            "speed": "Quick", "turn_limit": 40, "players": 2, "turns": 40, "winner": 0,
            "victory": "Time", "civs": [], "bot_errors": 0, "engine": "x", "bot": "basic-1"
        });
        assert!(matches!(ReferenceOutcome::read(&line), Ok(ReferenceOutcome::Finished(_))));
        for (k, v) in [("seconds", 1.5), ("cpu_s", 1.0)] {
            let mut timed = line.clone();
            timed[k] = serde_json::json!(v);
            assert!(ReferenceOutcome::read(&timed).expect_err(k).contains("unknown field"));
        }
    }
}
