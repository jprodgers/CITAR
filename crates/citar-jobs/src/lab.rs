//! The lab game's result in Rust (design/phase4-helper.md P4.4.2): `citar/lab.py`'s `play()`,
//! whose result building (`lab.py:314-406` at the package's base) this replaces. Python heard the
//! game's events as dicts through `engine_api.run_game`'s listener and summed up its result from
//! them and from `run_game`'s dict; here the same sums are taken from the typed events of each
//! step and from the game itself.
//!
//! **What a result holds** ([`LabResult`]): the experiment, the game's index, seed and map, the
//! turns played, the winner, its seat's label and the victory, `engine` (the build id that played
//! it, `citar_bot::build_id`), `errors`, and per major (by `str(pid)`, eliminated ones too): the
//! seat's label, bot, difficulty, levels, profile with its revision and name; what played (the
//! build, the version, the fingerprint, the cleaned overrides, the fixed and the played
//! aggression); the score and its share, the rank, techs, cities, policies, religion, great
//! people, spaceship parts, the share of rounds unhappy, the twelve stats at the checkpoints 50 to
//! 300, the turn each era began, the event counts and the 25 most built items.
//!
//! **Only what the game decides:** no time, no trace. `seconds`, `started_ts`, `cpu_s` and
//! `finished` are measurements the writer of the line adds outside the hash (`lab.play`), and a
//! panic is the crash outcome ([`LabCrash`]: the turn, the seat and the message, normalised), never
//! a result with a trace in `errors`. A Rust bot raises no error short of a panic, so `errors` is
//! empty; it stays in the result's shape, as the messages of a game's errors only (no traces, no
//! addresses), for the lab's readers.
//!
//! **`engine` is the build id, not the jobs build.** The jobs build covers the build's flavour
//! (P4.4.5), but a `ci` and a `release` build play one game and must give one result hash (4-07's
//! gate 2); the claim carries the jobs build beside the result (P4.4.4).
//!
//! Maps keyed by an integer are `BTreeMap`s, written with their keys as text as Python's `json`
//! wrote them, so the bytes are the same on every target.

use std::collections::BTreeMap;
use std::sync::Arc;

use citar_bot::{Bot, BotSpec, Tuning, VersionId, build_id, clean, fingerprint};
use citar_engine::base::ids::{PlayerId, Turn};
use citar_engine::base::num::round_ndigits;
use citar_engine::game::cities::construction::item_name;
use citar_engine::game::{EventBatch, Game, SeatDriver};
use citar_engine::rules::Ruleset;
use citar_engine::state::chronicle::{CivStats, EngineEvent, EventType, StatsRow};
use citar_sim::{RunResult, Seats};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::spec::{LabSpec, SpecError, version_of};

/// The turns whose stats a result keeps (`lab.CHECKPOINTS`).
pub const CHECKPOINTS: [Turn; 6] = [50, 100, 150, 200, 250, 300];

/// How many of a seat's most built items a result keeps.
pub const BUILT_TOP: usize = 25;

/// The most characters a crash's message keeps.
pub const MESSAGE_CHARS: usize = 1000;

/// A major's twelve stats at a checkpoint (`lab.STAT_KEYS`, in their order).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointStats {
    pub score: i32,
    pub cities: u32,
    pub population: u32,
    pub techs: u32,
    pub era: u8,
    pub military: i64,
    pub gold: i64,
    pub gold_per_turn: f64,
    pub science: f64,
    pub production: f64,
    pub happiness: i32,
    pub culture: f64,
}

impl CheckpointStats {
    fn of(c: &CivStats) -> Self {
        Self {
            score: c.score,
            cities: u32::from(c.cities),
            population: c.population,
            techs: u32::from(c.techs),
            era: c.era.0,
            military: c.military,
            gold: c.gold,
            gold_per_turn: c.gold_per_turn,
            science: c.science,
            production: c.production,
            happiness: c.happiness,
            culture: c.culture,
        }
    }
}

/// One major of a finished lab game, in the order `lab.play` wrote its keys.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabPlayer {
    pub label: Option<String>,
    pub bot: Option<String>,
    pub difficulty: String,
    pub alive: bool,
    pub levels: Option<BTreeMap<String, Value>>,
    pub profile: Option<String>,
    pub profile_rev: Option<u32>,
    pub profile_name: Option<String>,
    /// The build id that played the seat.
    pub build: String,
    /// The version that played: `basic-1`, `idle`.
    pub version: String,
    /// What played, hashed (`citar_bot::fingerprint`).
    pub fingerprint: String,
    /// The overrides as cleaned against the version's schema.
    pub params: BTreeMap<String, Value>,
    pub fixed_aggression: Option<f64>,
    /// The aggression it played with, three decimals.
    pub aggression: f64,
    pub score: i32,
    /// Its score over every major's, four decimals.
    pub score_share: f64,
    /// 0 for the highest score; equal scores rank by seat.
    pub rank: u32,
    pub techs: u32,
    pub cities: u32,
    pub policies: u32,
    pub religion: String,
    pub great_people: i32,
    /// Spaceship parts added, all kinds together.
    pub spaceship: i64,
    /// The share of rounds it was alive and unhappy, three decimals.
    pub unhappy_share: f64,
    pub checkpoints: BTreeMap<u32, CheckpointStats>,
    /// The turn each era (by its number) began.
    pub era_turn: BTreeMap<u8, Turn>,
    /// Counts of what happened to it, and `eliminated_turn`.
    pub events: BTreeMap<String, i64>,
    /// Its most built items and how many of each.
    pub built_top: BTreeMap<String, u32>,
}

/// A finished lab game: the deterministic part of the lab's result line (P4.4.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabResult {
    pub exp: String,
    pub i: u32,
    pub seed: u64,
    pub map: String,
    pub turns: Turn,
    pub winner: Option<u8>,
    pub winner_label: Option<String>,
    pub victory: Option<String>,
    pub players: BTreeMap<u8, LabPlayer>,
    /// The messages of the game's errors: none from Rust bots, whose only failure is a panic,
    /// the crash outcome.
    pub errors: Vec<String>,
    /// The build id that played it.
    pub engine: String,
}

/// A lab game that panicked: final, never retried, and what a replica must reproduce (P4.4.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabCrash {
    /// The turn it happened on (0 before the game existed).
    pub turn: Turn,
    /// The seat whose bot was playing or answering, if one was.
    pub player: Option<u8>,
    /// That seat's label.
    pub label: Option<String>,
    /// The panic's message only, normalised ([`normalise_message`]).
    pub message: String,
}

/// A crash outcome as the wire carries it: `{crash: {...}}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabCrashed {
    pub crash: LabCrash,
}

/// How a lab or ranking game came out.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum LabOutcome {
    Finished(Box<LabResult>),
    Crashed(LabCrashed),
}

impl LabOutcome {
    /// Reads an outcome as the wire carries it, refusing an unknown key anywhere: a crash if it
    /// has the key `crash`, else a finished game's result.
    ///
    /// # Errors
    /// What does not read.
    pub fn read(v: &Value) -> Result<Self, String> {
        if v.get("crash").is_some() {
            LabCrashed::deserialize(v).map(Self::Crashed).map_err(|e| e.to_string())
        } else {
            LabResult::deserialize(v)
                .map(|r| Self::Finished(Box::new(r)))
                .map_err(|e| e.to_string())
        }
    }
}

/// A panic's message as a result keeps it: CRLF as LF, every `0x` followed by 8 or more hex
/// digits (an address, which differs between runs) as `0x?`, cut to [`MESSAGE_CHARS`]
/// characters. The backtrace is the host's to log, never hashed: frame addresses differ
/// between builds of one source.
#[must_use]
pub fn normalise_message(message: &str) -> String {
    let text = message.replace("\r\n", "\n");
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '0' && chars.get(i + 1).is_some_and(|&c| c == 'x' || c == 'X') {
            let digits = chars[i + 2..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
            if digits >= 8 {
                out.push_str("0x?");
                i += 2 + digits;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    match out.char_indices().nth(MESSAGE_CHARS) {
        Some((at, _)) => out[..at].to_owned(),
        None => out,
    }
}

/// What played a seat, taken when the game starts (`lab.play`'s `played`, and the handle's
/// aggression).
#[derive(Clone, Debug)]
struct Played {
    version: VersionId,
    fingerprint: String,
    params: BTreeMap<String, Value>,
    fixed_aggression: Option<f64>,
    aggression: f64,
}

/// What a lab game heard from its events (`lab.play`'s listener): items built per player in the
/// order first built, the turn each era began, and the event counts.
#[derive(Clone, Debug, Default)]
struct Heard {
    built: BTreeMap<PlayerId, Vec<(String, u32)>>,
    era_turn: BTreeMap<PlayerId, BTreeMap<u8, Turn>>,
    events: BTreeMap<PlayerId, BTreeMap<String, i64>>,
}

impl Heard {
    fn count(&mut self, p: PlayerId, what: &str) {
        *self.events.entry(p).or_default().entry(what.to_owned()).or_insert(0) += 1;
    }

    /// One step's events, as the listener took them: each event's first matching rule.
    fn hear(&mut self, g: &Game, events: &EventBatch) {
        let rules = g.rules();
        for ev in events.events() {
            // A headless game has no host to add events of its own.
            let EventType::Engine(kind) = &ev.kind else { continue };
            let d = ev.data.as_deref();
            match kind {
                // `t in ("unit_built", "building_built", "wonder_built") and d.get("item")`
                EngineEvent::UnitBuilt | EngineEvent::BuildingBuilt | EngineEvent::WonderBuilt
                    if d.and_then(|d| d.item).is_some_and(|i| !item_name(rules, i).is_empty()) =>
                {
                    let (Some(p), Some(item)) = (d.and_then(|d| d.player), d.and_then(|d| d.item))
                    else {
                        continue;
                    };
                    let name = item_name(rules, item);
                    let list = self.built.entry(p).or_default();
                    match list.iter_mut().find(|(n, _)| n == name) {
                        Some((_, k)) => *k += 1,
                        None => list.push((name.to_owned(), 1)),
                    }
                }
                EngineEvent::Era => {
                    if let (Some(p), Some(era)) = (d.and_then(|d| d.player), d.and_then(|d| d.era))
                    {
                        self.era_turn.entry(p).or_default().entry(era.0).or_insert(ev.turn);
                    }
                }
                EngineEvent::CityCaptured => {
                    if let Some(p) = d.and_then(|d| d.new_owner) {
                        self.count(p, "captured_city");
                    }
                    if let Some(p) = d.and_then(|d| d.old_owner) {
                        self.count(p, "lost_city");
                    }
                }
                EngineEvent::WarDeclared => {
                    if let Some(p) = d.and_then(|d| d.attacker) {
                        self.count(p, "declared_war");
                    }
                }
                EngineEvent::Eliminated => {
                    if let Some(p) = d.and_then(|d| d.player) {
                        self.events
                            .entry(p)
                            .or_default()
                            .insert("eliminated_turn".to_owned(), i64::from(ev.turn));
                    }
                }
                EngineEvent::Bankrupt
                | EngineEvent::CityStarving
                | EngineEvent::CampCleared
                | EngineEvent::CityFounded
                | EngineEvent::UnitCaptured
                | EngineEvent::ReligionFounded
                | EngineEvent::GoldenAge
                | EngineEvent::GreatPersonBorn
                | EngineEvent::Pillaged => {
                    // `events[ev["players"][0]][t] += 1`: the first of whoever heard of it.
                    if let Some(p) = ev.audience.and_then(|a| a.iter().next()) {
                        self.count(p, kind.name());
                    }
                }
                _ => {}
            }
        }
    }
}

/// A lab game in play: its spec, what plays each seat, and what it has heard.
#[derive(Debug)]
pub struct LabPlay {
    spec: LabSpec,
    build: String,
    played: Vec<Played>,
    heard: Heard,
}

impl LabPlay {
    /// The seats' bots for `spec` under `rules` (`lab.make_bot`): each seat's version with its
    /// cleaned overrides, its profile's fixed aggression if it has one, else its start
    /// position's (`0.25 + 0.5 * ((pid * 37 + seed) % 10) / 9`).
    ///
    /// # Errors
    /// A seat whose version or overrides `check_spec` should have refused.
    pub fn start(rules: &Ruleset, spec: &LabSpec) -> Result<(Self, Seats), SpecError> {
        let build = build_id(rules);
        let mut seats: Seats = Vec::with_capacity(spec.seats.len());
        let mut played = Vec::with_capacity(spec.seats.len());
        for (n, seat) in spec.seats.iter().enumerate() {
            let pid =
                u8::try_from(n).map(PlayerId).map_err(|_| SpecError("too many seats".into()))?;
            let version = version_of(seat.bot.as_deref())
                .ok_or_else(|| SpecError(format!("seat {n}: an unknown bot version")))?;
            let given = seat.params.as_ref().map_or(Value::Object(serde_json::Map::new()), |p| {
                Value::Object(p.clone().into_iter().collect())
            });
            let name = seat.bot.as_deref().unwrap_or("basic");
            let overrides =
                clean(name, &given).map_err(|e| SpecError(format!("seat {n}: {}", e.message)))?;
            let cleaned = clean(version.id(), &given)
                .map_err(|e| SpecError(format!("seat {n}: {}", e.message)))?;
            let params = match cleaned.to_json() {
                Value::Object(m) => m.into_iter().collect(),
                _ => BTreeMap::new(),
            };
            let seat_aggression = citar_sim::baseline::aggression(pid, spec.seed);
            let bot = BotSpec::new(
                version,
                Arc::new(Tuning::new(version, overrides)),
                seat.aggression,
                Some(seat_aggression),
            );
            played.push(Played {
                version,
                fingerprint: fingerprint(&bot, &build),
                params,
                fixed_aggression: bot.fixed_aggression,
                aggression: bot.aggression,
            });
            seats.push((pid, Box::new(Bot::new(Arc::new(bot))) as Box<dyn SeatDriver>));
        }
        Ok((Self { spec: spec.clone(), build, played, heard: Heard::default() }, seats))
    }

    /// The seats' labels, for a crash record.
    #[must_use]
    pub fn labels(&self) -> BTreeMap<PlayerId, String> {
        (0u8..)
            .zip(&self.spec.seats)
            .filter_map(|(p, s)| s.label.clone().map(|l| (PlayerId(p), l)))
            .collect()
    }

    /// Hears one step's events.
    pub fn hear(&mut self, g: &Game, events: &EventBatch) {
        self.heard.hear(g, events);
    }

    /// The crash outcome of a panic on `turn` in `player`'s seat.
    #[must_use]
    pub fn crash(&self, turn: Turn, player: Option<PlayerId>, message: &str) -> LabCrash {
        let label = player
            .and_then(|p| self.spec.seats.get(usize::from(p.0)))
            .and_then(|s| s.label.clone());
        LabCrash { turn, player: player.map(|p| p.0), label, message: normalise_message(message) }
    }

    /// The finished game's result (`lab.play`'s return, without the times).
    #[must_use]
    pub fn result(&self, g: &Game) -> LabResult {
        let run = RunResult::of(g, Vec::new());
        let stats: BTreeMap<Turn, &StatsRow> = g.stats(None).iter().map(|s| (s.turn, s)).collect();
        fn civ(row: &StatsRow, p: PlayerId) -> Option<&CivStats> {
            row.civs.iter().find(|c| c.player == p)
        }
        let majors: Vec<(PlayerId, &citar_sim::MajorRow, bool)> = run
            .players
            .iter()
            .filter_map(|p| p.major.as_ref().map(|m| (p.id, m, p.alive)))
            .collect();
        let total = majors.iter().map(|(_, m, _)| i64::from(m.score)).sum::<i64>();
        let total = if total == 0 { 1 } else { total };
        // `sorted(final, key=lambda q: -final[q])`: stable, so equal scores keep seat order.
        let mut ranking: Vec<PlayerId> = majors.iter().map(|(p, _, _)| *p).collect();
        let score_of =
            |p: PlayerId| majors.iter().find(|(q, _, _)| *q == p).map_or(0, |m| m.1.score);
        ranking.sort_by_key(|&p| std::cmp::Reverse(score_of(p)));
        let rounds = stats.len().max(1);
        let mut players = BTreeMap::new();
        for (pid, m, alive) in &majors {
            let n = usize::from(pid.0);
            let (Some(seat), Some(played)) = (self.spec.seats.get(n), self.played.get(n)) else {
                continue;
            };
            let mut checkpoints = BTreeMap::new();
            for cp in CHECKPOINTS {
                if let Some(c) = stats.get(&cp).and_then(|r| civ(r, *pid)).filter(|c| c.alive) {
                    checkpoints.insert(u32::try_from(cp).unwrap_or(0), CheckpointStats::of(c));
                }
            }
            let unhappy = stats
                .values()
                .filter(|r| civ(r, *pid).is_some_and(|c| c.alive && c.happiness < 0))
                .count();
            let spaceship = match &m.spaceship {
                Value::Object(parts) => parts.values().filter_map(Value::as_i64).sum(),
                _ => 0,
            };
            let mut built: Vec<(String, u32)> =
                self.heard.built.get(pid).cloned().unwrap_or_default();
            // `Counter.most_common(25)`: by count, the first built first among equals.
            built.sort_by_key(|(_, k)| std::cmp::Reverse(*k));
            built.truncate(BUILT_TOP);
            players.insert(
                pid.0,
                LabPlayer {
                    label: seat.label.clone(),
                    bot: seat.bot.clone(),
                    difficulty: m.difficulty.clone(),
                    alive: *alive,
                    levels: seat.levels.clone(),
                    profile: seat.profile.clone(),
                    profile_rev: seat.profile_rev,
                    profile_name: seat.profile_name.clone(),
                    build: self.build.clone(),
                    version: played.version.id().to_owned(),
                    fingerprint: played.fingerprint.clone(),
                    params: played.params.clone(),
                    fixed_aggression: played.fixed_aggression,
                    aggression: round_ndigits(played.aggression, 3),
                    score: m.score,
                    score_share: round_ndigits(f64::from(m.score) / total as f64, 4),
                    rank: ranking
                        .iter()
                        .position(|p| p == pid)
                        .and_then(|r| u32::try_from(r).ok())
                        .unwrap_or(0),
                    techs: m.techs,
                    cities: m.cities,
                    policies: m.policies,
                    religion: m.religion.to_owned(),
                    great_people: m.great_people,
                    spaceship,
                    unhappy_share: round_ndigits(unhappy as f64 / rounds as f64, 3),
                    checkpoints,
                    era_turn: self.heard.era_turn.get(pid).cloned().unwrap_or_default(),
                    events: self.heard.events.get(pid).cloned().unwrap_or_default(),
                    built_top: built.into_iter().collect(),
                },
            );
        }
        let winner = run.winner;
        let winner_label = winner
            .and_then(|w| self.spec.seats.get(usize::from(w.0)))
            .and_then(|s| s.label.clone());
        LabResult {
            exp: self.spec.exp.clone(),
            i: self.spec.i,
            seed: self.spec.seed,
            map: self.spec.map_type.clone(),
            turns: run.turns,
            winner: winner.map(|w| w.0),
            winner_label,
            victory: run.victory,
            players,
            errors: Vec::new(),
            engine: self.build.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The listener's rules name these event types; the engine's names for them must be those.
    #[test]
    fn the_events_heard_are_pythons_by_name() {
        let pairs = [
            (EngineEvent::UnitBuilt, "unit_built"),
            (EngineEvent::BuildingBuilt, "building_built"),
            (EngineEvent::WonderBuilt, "wonder_built"),
            (EngineEvent::Era, "era"),
            (EngineEvent::CityCaptured, "city_captured"),
            (EngineEvent::WarDeclared, "war_declared"),
            (EngineEvent::Eliminated, "eliminated"),
            (EngineEvent::Bankrupt, "bankrupt"),
            (EngineEvent::CityStarving, "city_starving"),
            (EngineEvent::CampCleared, "camp_cleared"),
            (EngineEvent::CityFounded, "city_founded"),
            (EngineEvent::UnitCaptured, "unit_captured"),
            (EngineEvent::ReligionFounded, "religion_founded"),
            (EngineEvent::GoldenAge, "golden_age"),
            (EngineEvent::GreatPersonBorn, "great_person_born"),
            (EngineEvent::Pillaged, "pillaged"),
        ];
        for (e, name) in pairs {
            assert_eq!(e.name(), name);
        }
    }

    #[test]
    fn a_message_keeps_no_address_and_no_carriage_return() {
        assert_eq!(normalise_message("a\r\nb"), "a\nb");
        assert_eq!(normalise_message("at 0x7ffd1234abcd now"), "at 0x? now");
        assert_eq!(normalise_message("index 0x10 of 3"), "index 0x10 of 3", "short hex stays");
        assert_eq!(normalise_message(&"é".repeat(2000)).chars().count(), MESSAGE_CHARS);
        assert_eq!(normalise_message("ends 0x"), "ends 0x");
    }

    #[test]
    fn an_outcome_reads_as_a_crash_or_a_result_and_refuses_unknown_keys() {
        let crash = serde_json::json!({"crash": {"turn": 3, "player": 1, "label": "b",
                                                 "message": "boom"}});
        assert!(matches!(LabOutcome::read(&crash), Ok(LabOutcome::Crashed(_))));
        let mut traced = crash.clone();
        traced["crash"]["trace"] = serde_json::json!("at x.rs:1");
        assert!(LabOutcome::read(&traced).expect_err("refused").contains("unknown field"));
        let mut both = crash;
        both["exp"] = serde_json::json!("x");
        assert!(LabOutcome::read(&both).is_err());
    }
}
