//! The soak driver (DESIGN.md 9.5, 9.6): whole `RandomAgent` games on every map size, duel to
//! gargantuan, played to their turn limit with the invariants checked at every settle, each
//! under `catch_unwind`. It reports what a long run finds that a short one does not:
//! - **panics**, with the game that panicked, which `soak --game N` plays again alone;
//! - **violations**: what the checks reported at a settle, the invariants broken after a round,
//!   the caches against a cold rebuild (the cache oracle) every `oracle_every` rounds and at the
//!   end, and a save and load every `save_every` rounds and at the end that must give the state
//!   saved (property P6 at the scale of whole games);
//! - **turn-time outliers**: rounds that took more than `outlier_factor` times their game's
//!   median round, and more than `outlier_floor_ms`, so a round that is slow for its game shows
//!   whatever the map's size;
//! - **peak memory**: the most heap a game held, as the binary's allocator counts it.
//!
//! Games are drawn from the run's seed in a fixed order ([`plan`]): the sizes in turn, the map
//! types and edges rotating under them, and every fourth lap of the sizes the duel, small and
//! standard games on the kitchen-sink ruleset with the Kitchen Sink nation in the first seat. So
//! game `n` of a seed is the same game on every machine, and a run split over processes (`shard`)
//! plays the same games as one that is not: each shard takes whole laps of the sizes, so each
//! plays every size alike and the shards take about as long as each other.
//!
//! Time and memory come from the binary ([`Probe`]): the engine and this library read no clock.
//! Panics, violations and errors fail the run; outliers and memory are reported. A run its time
//! budget stopped before it played every game it was to play says so ([`Report::cut_short`]),
//! and the binary fails it on its own exit code: a slower runner must not quietly shrink a run.

use std::panic::{AssertUnwindSafe, catch_unwind};

use citar_engine::base::ids::Turn;
use citar_engine::base::rng::{Purpose, Rng};
use citar_engine::game::{DebugOptions, Game};
use citar_engine::save::canon;
use citar_engine::state::Phase;
use serde::Serialize;

use crate::games;

/// Every map size, smallest first, and the turn limit a game of each plays to by default: a Quick
/// game's 330 turns on every size, since late games on large maps are where a slow leak or a
/// slow round shows. In the ci profile on the laptop that is about 1 s for a duel, 15 s for a
/// huge map and 45 s for a gargantuan one; `max_rounds` shortens them all.
pub const SIZES: [(&str, u32); 6] = [
    ("duel", 330),
    ("small", 330),
    ("standard", 330),
    ("large", 330),
    ("huge", 330),
    ("gargantuan", 330),
];

const MAP_TYPES: [&str; 5] = ["continents", "pangaea", "fractal", "archipelago", "inland_sea"];
const EDGES: [&str; 5] = ["wrap_x", "boxed", "ice_caps", "wrap_y", "wrap_both"];

/// The sizes the kitchen-sink ruleset plays: its games stay small, since its point is the extra
/// unique types, not the map.
const KITCHEN_SINK_SIZES: [&str; 3] = ["duel", "small", "standard"];

/// One game of a soak.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GameSpec {
    /// Its number in the run, from 0.
    pub index: u32,
    pub size: String,
    pub map_type: String,
    pub edges: String,
    pub seed: u64,
    /// The game's turn limit: it ends there, by the Time victory if no other came first.
    pub turn_limit: u32,
    /// On the kitchen-sink ruleset, with the Kitchen Sink nation in the first seat (always on
    /// continents with the edges wrapped east to west, as `games::kitchen_sink_game` sets it).
    pub kitchen_sink: bool,
}

impl GameSpec {
    /// A short name for reports: `#12 large archipelago wrap_x s123`.
    #[must_use]
    pub fn label(&self) -> String {
        if self.kitchen_sink {
            format!("#{} {} kitchen-sink s{}", self.index, self.size, self.seed)
        } else {
            format!("#{} {} {} {} s{}", self.index, self.size, self.map_type, self.edges, self.seed)
        }
    }

    /// The game, set up with the invariants on at every settle.
    ///
    /// # Errors
    /// Settings the engine refuses.
    pub fn game(&self) -> Result<Game, String> {
        let spec = format!("soak:{}", self.label());
        let debug = DebugOptions { invariants: true, verify_caches: false };
        if self.kitchen_sink {
            games::kitchen_sink_game(&self.size, self.seed, self.turn_limit, spec.as_bytes(), debug)
        } else {
            let settings = games::random_settings(
                &self.size,
                &self.map_type,
                &self.edges,
                self.seed,
                self.turn_limit,
            );
            games::new_game(&settings, spec.as_bytes(), debug)
        }
    }
}

/// What a soak is asked to do.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The seed every game is drawn from.
    pub seed: u64,
    /// How many games the run plays, at most.
    pub games: u32,
    /// The sizes it plays, from [`SIZES`]; all of them when empty.
    pub sizes: Vec<String>,
    /// A cap on every size's rounds, for a shorter soak.
    pub max_rounds: Option<u32>,
    /// The cache oracle every so many rounds (and always at the end); 0 for the end only.
    pub oracle_every: u32,
    /// A save and a load every so many rounds (and always at the end); 0 for the end only.
    pub save_every: u32,
    /// A round is an outlier above this many times its game's median round...
    pub outlier_factor: f64,
    /// ... and above this many milliseconds.
    pub outlier_floor_ms: f64,
    /// Play only the laps of the sizes whose number is `k` modulo `n`: `(k, n)`.
    pub shard: (u32, u32),
    /// Play only this game.
    pub only: Option<u32>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            seed: 1,
            games: 12,
            sizes: Vec::new(),
            max_rounds: None,
            oracle_every: 50,
            save_every: 25,
            outlier_factor: 8.0,
            outlier_floor_ms: 50.0,
            shard: (0, 1),
            only: None,
        }
    }
}

/// The binary's clock and memory, which the library has no access to.
pub trait Probe {
    /// Nanoseconds since some fixed moment.
    fn now_ns(&mut self) -> u64;
    /// Starts counting the most heap held afresh, from what is held now.
    fn reset_peak(&mut self);
    /// The most heap held since the last reset, in bytes, if the binary counts it.
    fn peak_bytes(&mut self) -> Option<u64>;
    /// Whether the run may start another game (a time budget).
    fn keep_going(&mut self) -> bool;
}

/// A round slow for its game.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Outlier {
    /// The round's turn.
    pub turn: Turn,
    pub ms: f64,
    /// Its game's median round.
    pub median_ms: f64,
}

/// One game's report.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GameReport {
    pub game: GameSpec,
    /// Rounds played, and the turn the game reached.
    pub rounds: u32,
    pub turn: Turn,
    /// Whether it ended (at its turn limit, or by a victory before it).
    pub over: bool,
    pub winner: Option<u8>,
    /// How long it took, setup and checks included.
    pub seconds: f64,
    pub median_round_ms: f64,
    pub max_round_ms: f64,
    pub outliers: Vec<Outlier>,
    /// The most heap the game held, in bytes, if the binary counts it: the game, its agents, and
    /// the journal chunks the soak keeps to load the game back from (`journal_bytes`), as a
    /// host that keeps its journal in memory would hold them.
    pub peak_bytes: Option<u64>,
    /// The canonical state's size at the end, and the journal chunks' total.
    pub state_bytes: usize,
    pub journal_bytes: usize,
    /// Cities and units at the end, the city-states' and the barbarians' among them.
    pub cities: usize,
    pub units: usize,
    /// What failed: violations, broken invariants, cache disagreements, a save that did not load
    /// as saved, an engine refusal, a game that outlived its turn limit.
    pub failures: Vec<String>,
    /// What a panic said, if the game panicked.
    pub panic: Option<String>,
}

impl GameReport {
    /// Whether the game failed.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.panic.is_some() || !self.failures.is_empty()
    }
}

/// A whole run's report.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Report {
    pub seed: u64,
    /// How many games the run (or its shard) was to play: fewer were played when the probe
    /// stopped it ([`Report::cut_short`]).
    pub planned: u32,
    pub games: Vec<GameReport>,
}

impl Report {
    /// The games that failed.
    pub fn failed(&self) -> impl Iterator<Item = &GameReport> {
        self.games.iter().filter(|g| g.failed())
    }

    /// Whether the probe stopped the run before it had played every game it was to play: a
    /// time budget that ran out, so the run covered less than it was asked to. Not a failure of
    /// the games played, but not the run that was asked for either.
    #[must_use]
    pub fn cut_short(&self) -> bool {
        self.games.len() < self.planned as usize
    }

    /// The report as JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self)
            .map_or_else(|e| format!("{{\"error\": \"{e}\"}}"), |s| s + "\n")
    }

    /// One line per size: games, rounds, the slowest median round, the slowest round and the
    /// most heap any game held.
    #[must_use]
    pub fn by_size(&self) -> Vec<String> {
        SIZES
            .iter()
            .filter_map(|(size, _)| {
                let of: Vec<&GameReport> =
                    self.games.iter().filter(|g| g.game.size == *size).collect();
                if of.is_empty() {
                    return None;
                }
                let rounds: u32 = of.iter().map(|g| g.rounds).sum();
                let median = of.iter().map(|g| g.median_round_ms).fold(0.0, f64::max);
                let max = of.iter().map(|g| g.max_round_ms).fold(0.0, f64::max);
                let peak = of.iter().filter_map(|g| g.peak_bytes).max();
                let outliers: usize = of.iter().map(|g| g.outliers.len()).sum();
                Some(format!(
                    "{size:<10} {:>3} games {rounds:>6} rounds; median round up to {median:.1} ms, \
                     slowest {max:.1} ms, {outliers} outliers; peak heap {}",
                    of.len(),
                    peak.map_or_else(|| "not counted".to_owned(), mib)
                ))
            })
            .collect()
    }
}

/// Bytes as mebibytes, for reports.
#[must_use]
pub fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

/// The sizes a run plays, with their rounds.
fn sizes(settings: &Settings) -> Result<Vec<(&'static str, u32)>, String> {
    let all: Vec<(&'static str, u32)> = SIZES.to_vec();
    let mut out = if settings.sizes.is_empty() {
        all
    } else {
        let mut chosen = Vec::new();
        for s in &settings.sizes {
            let found = SIZES.iter().find(|(name, _)| name == s).ok_or_else(|| {
                let names: Vec<&str> = SIZES.iter().map(|(n, _)| *n).collect();
                format!("no map size {s}; there are {}", names.join(", "))
            })?;
            chosen.push(*found);
        }
        chosen
    };
    if let Some(cap) = settings.max_rounds {
        for (_, rounds) in &mut out {
            *rounds = (*rounds).min(cap.max(1));
        }
    }
    Ok(out)
}

/// Every game a run of `settings` plays, in order, before sharding.
///
/// # Errors
/// A size that does not exist.
pub fn plan(settings: &Settings) -> Result<Vec<GameSpec>, String> {
    let sizes = sizes(settings)?;
    let n = u32::try_from(sizes.len()).map_err(|e| e.to_string())?.max(1);
    Ok((0..settings.games)
        .map(|i| {
            let (size, rounds) = sizes[(i % n) as usize];
            let k = (i / n) as usize;
            let mut rng = Rng::keyed(settings.seed, Purpose::TestAgent, &[0x50a6, u64::from(i)]);
            // Every fourth lap of the sizes, the small ones on the kitchen-sink ruleset.
            let kitchen_sink = k % 4 == 3 && KITCHEN_SINK_SIZES.contains(&size);
            GameSpec {
                index: i,
                size: size.to_owned(),
                map_type: MAP_TYPES[k % MAP_TYPES.len()].to_owned(),
                // Shifted every lap of the types, so each type meets every edge mode.
                edges: EDGES[(k + k / MAP_TYPES.len()) % EDGES.len()].to_owned(),
                seed: rng.next_u64() >> 16,
                turn_limit: rounds,
                kitchen_sink,
            }
        })
        .collect())
}

/// What a panic said.
fn panic_text(payload: &(dyn core::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no message".to_owned())
}

/// The median of some round times.
fn median(times: &[f64]) -> f64 {
    let mut v = times.to_vec();
    v.sort_by(f64::total_cmp);
    match v.len() {
        0 => 0.0,
        n if n % 2 == 1 => v[n / 2],
        n => (v[n / 2 - 1] + v[n / 2]) / 2.0,
    }
}

/// What a game's play collects as it goes.
#[derive(Default)]
struct Tally {
    rounds: Vec<(Turn, f64)>,
    failures: Vec<String>,
    chunks: Vec<Vec<u8>>,
}

/// Plays one game to its end, checking it as it goes; a failure is recorded and play goes on
/// where it can, so one report holds everything the game found.
fn play(
    spec: &GameSpec,
    settings: &Settings,
    probe: &mut dyn Probe,
    t: &mut Tally,
) -> Option<Game> {
    let mut g = match spec.game() {
        Ok(g) => g,
        Err(e) => {
            t.failures.push(format!("does not set up: {e}"));
            return None;
        }
    };
    let mut agents = games::agents_for(&g);
    let mut last = probe.now_ns();
    let mut hook = |g: &mut Game, (turn, _): games::Round| -> Result<(), String> {
        let now = probe.now_ns();
        t.rounds.push((turn, now.saturating_sub(last) as f64 / 1e6));
        let n = u32::try_from(t.rounds.len()).unwrap_or(u32::MAX);
        // The invariants ran at every settle of the round (the game's debug options), and what
        // they found waits here.
        t.failures.extend(g.take_violations().into_iter().map(|v| format!("turn {turn}: {v}")));
        if settings.oracle_every > 0 && n % settings.oracle_every == 0 {
            t.failures.extend(
                crate::checks::caches(g).into_iter().map(|e| format!("turn {turn}: caches: {e}")),
            );
        }
        if settings.save_every > 0 && n % settings.save_every == 0 {
            games::save_and_load(g, &mut t.chunks)?;
        }
        // The checks are not the round's time.
        last = probe.now_ns();
        Ok(())
    };
    // A few rounds past the limit, so a game that outlives it is caught, not cut off.
    let cap = spec.turn_limit + 3;
    if let Err(e) = games::play_random(&mut g, &mut agents, cap, &mut hook) {
        t.failures.push(e);
    }
    if g.phase() == Phase::Playing {
        t.failures.push(format!(
            "turn {}: still playing after {} rounds, past its turn limit {}",
            g.turn(),
            t.rounds.len(),
            spec.turn_limit
        ));
    }
    t.failures.extend(games::problems(&mut g));
    t.failures.extend(crate::checks::caches(&g).into_iter().map(|e| format!("end: caches: {e}")));
    if let Err(e) = games::save_and_load(&mut g, &mut t.chunks) {
        t.failures.push(format!("end: {e}"));
    }
    Some(g)
}

/// Plays one game under `catch_unwind` and reports on it.
pub fn play_game(spec: &GameSpec, settings: &Settings, probe: &mut dyn Probe) -> GameReport {
    let started = probe.now_ns();
    probe.reset_peak();
    let mut t = Tally::default();
    let played = catch_unwind(AssertUnwindSafe(|| play(spec, settings, probe, &mut t)));
    let peak_bytes = probe.peak_bytes();
    let seconds = probe.now_ns().saturating_sub(started) as f64 / 1e9;
    let (g, panic) = match played {
        Ok(g) => (g, None),
        Err(p) => (None, Some(panic_text(&*p))),
    };
    let times: Vec<f64> = t.rounds.iter().map(|(_, ms)| *ms).collect();
    let median_round_ms = median(&times);
    let outliers = t
        .rounds
        .iter()
        .filter(|(_, ms)| {
            *ms > settings.outlier_factor * median_round_ms && *ms > settings.outlier_floor_ms
        })
        .map(|&(turn, ms)| Outlier { turn, ms, median_ms: median_round_ms })
        .collect();
    let (turn, over, winner, state_bytes, cities, units) =
        g.as_ref().map_or((0, false, None, 0, 0, 0), |g| {
            let st = g.state();
            (
                g.turn(),
                g.phase() == Phase::Over,
                st.clock().winner.map(|p| p.0),
                canon::state_bytes(st).map_or(0, |b| b.len()),
                st.cities().len(),
                st.units().len(),
            )
        });
    GameReport {
        game: spec.clone(),
        rounds: u32::try_from(t.rounds.len()).unwrap_or(u32::MAX),
        turn,
        over,
        winner,
        seconds,
        median_round_ms,
        max_round_ms: times.iter().copied().fold(0.0, f64::max),
        outliers,
        peak_bytes,
        state_bytes,
        journal_bytes: t.chunks.iter().map(Vec::len).sum(),
        cities,
        units,
        failures: t.failures,
        panic,
    }
}

/// Plays the run's games one after another, those of its shard, while the probe says to go on;
/// `each` sees every game's report as it ends. The report counts the games the run was to play
/// beside those it played.
///
/// # Errors
/// A size that does not exist, or a shard that is not one.
pub fn run(
    settings: &Settings,
    probe: &mut dyn Probe,
    each: &mut dyn FnMut(&GameReport),
) -> Result<Report, String> {
    let (k, n) = settings.shard;
    if n == 0 || k >= n {
        return Err(format!("no shard {k} of {n}"));
    }
    let laps = u32::try_from(sizes(settings)?.len()).map_err(|e| e.to_string())?.max(1);
    // Game `i` is the same game whatever the count, so asking for one past the count plays it.
    let count = settings.only.map_or(settings.games, |i| settings.games.max(i.saturating_add(1)));
    let mine: Vec<GameSpec> = plan(&Settings { games: count, ..settings.clone() })?
        .into_iter()
        .filter(|spec| match settings.only {
            Some(i) => spec.index == i,
            None => (spec.index / laps) % n == k,
        })
        .collect();
    let planned = u32::try_from(mine.len()).map_err(|e| e.to_string())?;
    let mut report = Report { seed: settings.seed, planned, games: Vec::new() };
    for spec in mine {
        if !probe.keep_going() {
            break;
        }
        let r = play_game(&spec, settings, probe);
        each(&r);
        report.games.push(r);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_covers_every_size_type_and_edge_and_the_kitchen_sink() {
        let games = plan(&Settings { games: 200, ..Settings::default() }).expect("a plan");
        assert_eq!(games.len(), 200);
        for (size, rounds) in SIZES {
            let of: Vec<&GameSpec> = games.iter().filter(|g| g.size == size).collect();
            assert!(of.len() >= 33, "{size}: {} games", of.len());
            assert!(of.iter().all(|g| g.turn_limit == rounds));
        }
        for t in MAP_TYPES {
            assert!(games.iter().any(|g| g.map_type == t && !g.kitchen_sink), "{t}");
        }
        for e in EDGES {
            assert!(games.iter().any(|g| g.edges == e && !g.kitchen_sink), "{e}");
        }
        let sink = games.iter().filter(|g| g.kitchen_sink).count();
        assert_eq!(sink, 24, "kitchen-sink games");
        for size in KITCHEN_SINK_SIZES {
            assert!(games.iter().any(|g| g.kitchen_sink && g.size == size), "{size}");
        }
        // The same plan twice: game n of a seed is one game.
        assert_eq!(plan(&Settings { games: 200, ..Settings::default() }).expect("a plan"), games);
    }

    /// A probe with no clock and no memory, which never stops a run.
    struct Still;

    impl Probe for Still {
        fn now_ns(&mut self) -> u64 {
            0
        }
        fn reset_peak(&mut self) {}
        fn peak_bytes(&mut self) -> Option<u64> {
            None
        }
        fn keep_going(&mut self) -> bool {
            false
        }
    }

    #[test]
    fn shards_take_whole_laps_of_the_sizes_and_cover_the_run_once() {
        let s = Settings { games: 30, ..Settings::default() };
        let all = plan(&s).expect("a plan");
        let mut seen = Vec::new();
        for k in 0..4 {
            let mine: Vec<u32> =
                all.iter().filter(|g| (g.index / 6) % 4 == k).map(|g| g.index).collect();
            // Every shard of a 30-game run but the last plays whole laps: each size alike.
            if k < 3 {
                assert_eq!(mine.len() % 6, 0, "shard {k}: {mine:?}");
            }
            seen.extend(mine);
        }
        seen.sort();
        assert_eq!(seen, (0..30).collect::<Vec<u32>>());
        // A run whose probe says stop plays nothing, and says it was cut short; a shard that is
        // not one is refused.
        let r =
            run(&Settings { shard: (1, 4), ..s.clone() }, &mut Still, &mut |_| {}).expect("a run");
        assert_eq!((r.planned, r.games.len(), r.cut_short()), (6, 0, true));
        let none = Report { seed: 1, planned: 0, games: Vec::new() };
        assert!(!none.cut_short(), "a run with nothing to play played it all");
        assert!(run(&Settings { shard: (4, 4), ..s }, &mut Still, &mut |_| {}).is_err());
    }

    #[test]
    fn a_shorter_soak_caps_the_rounds_and_keeps_the_games() {
        let s = Settings { games: 12, max_rounds: Some(40), ..Settings::default() };
        let short = plan(&s).expect("a plan");
        let long = plan(&Settings { games: 12, ..Settings::default() }).expect("a plan");
        assert!(short.iter().all(|g| g.turn_limit <= 40));
        for (a, b) in short.iter().zip(&long) {
            assert_eq!((a.index, &a.size, a.seed), (b.index, &b.size, b.seed));
        }
        assert!(plan(&Settings { sizes: vec!["tiny".into()], ..s }).is_err());
    }
}
