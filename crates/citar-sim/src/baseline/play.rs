//! One baseline game, played and summed up as its line (`baseline.py:51-160`).
//!
//! Every major civilization is a `basic-1` bot whose aggression is spread by seat and seed (the
//! lab's formula, `common.make_bots`), so a baseline covers peaceful and warlike bots in every
//! start position. The war and capture totals are kept from the game's events ([`Tally`]), since
//! the engine records no such counters, and taken at each checkpoint at the start of the next
//! round, when they are the totals at the end of the checkpoint's turn, as the engine's stats
//! row is. A crash, a refusal or a game past its budget is returned as the game's crash line,
//! never raised: the run goes on and a resumed run plays it again.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use citar_bot::{Bot, BotSpec, Tuning, VersionId};
use citar_engine::base::ids::{PlayerId, Turn};
use citar_engine::base::num::round_ndigits;
use citar_engine::game::victory::{score, won_by};
use citar_engine::game::{DebugOptions, EventBatch, Game, SeatDriver};
use citar_engine::rules::Ruleset;
use citar_engine::state::Phase;
use citar_engine::state::chronicle::{CivStats, EngineEvent, EventType, StatsRow};
use cpu_time::ThreadTime;

use super::spec::GameSpec;
use super::{BaselineCiv, BaselineCrash, BaselineGame, BaselineLine, CheckpointRow};
use crate::runner::{RunSpec, Runner, SimError};

/// The code that plays a run: written on every line, and a run refuses to add to a file another
/// code wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Code {
    /// `citar_bot::build_id` of the ruleset: the engine, the bot and the ruleset.
    pub engine: String,
    /// The bot version, `basic-1`, with its parameters' fingerprint after a `+` when the run
    /// overrides any.
    pub bot: String,
}

/// What every game of a run shares.
#[derive(Clone, Debug)]
pub struct Context {
    pub rules: &'static Ruleset,
    pub code: Code,
    /// The bot version every seat plays.
    pub version: VersionId,
    /// The bots' parameters, shared by every seat so its resolution is made once per ruleset.
    pub tuning: Arc<Tuning>,
    /// Whether the games run the engine's invariants.
    pub checks: bool,
}

/// The aggression of seat `pid` in a game of seed `seed`: 0.25 to 0.75 in ten steps
/// (`common.make_bots`).
#[must_use]
pub fn aggression(pid: PlayerId, seed: u64) -> f64 {
    let step = (u64::from(pid.0) * 37 + seed) % 10;
    // step < 10, exact in an f64.
    0.25 + 0.5 * (step as f64) / 9.0
}

/// One civilization's war and capture totals so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub wars_declared: u32,
    pub wars_declared_on_majors: u32,
    pub wars_declared_on_others: u32,
    pub cities_captured: u32,
    pub cities_lost: u32,
}

/// Running totals per civilization of wars declared and cities captured and lost, kept from the
/// game's events (`baseline.Tally`).
#[derive(Clone, Debug, Default)]
pub struct Tally {
    counts: BTreeMap<PlayerId, Totals>,
}

impl Tally {
    /// Counts the events of a step: a war declared, by its attacker and the defender's kind; a
    /// city captured, for its new owner and its old.
    pub fn count(&mut self, g: &Game, events: &EventBatch) {
        for ev in events.events() {
            let EventType::Engine(kind) = &ev.kind else { continue };
            let Some(d) = ev.data.as_deref() else { continue };
            match kind {
                EngineEvent::WarDeclared => {
                    let Some(attacker) = d.attacker else { continue };
                    let on_major =
                        d.defender.and_then(|p| g.player(p)).is_some_and(|p| p.is_major());
                    let t = self.counts.entry(attacker).or_default();
                    t.wars_declared += 1;
                    if on_major {
                        t.wars_declared_on_majors += 1;
                    } else {
                        t.wars_declared_on_others += 1;
                    }
                }
                EngineEvent::CityCaptured => {
                    if let Some(p) = d.new_owner {
                        self.counts.entry(p).or_default().cities_captured += 1;
                    }
                    if let Some(p) = d.old_owner {
                        self.counts.entry(p).or_default().cities_lost += 1;
                    }
                }
                _ => {}
            }
        }
    }

    /// One civilization's totals so far.
    #[must_use]
    pub fn of(&self, pid: PlayerId) -> Totals {
        self.counts.get(&pid).copied().unwrap_or_default()
    }
}

/// Why a game has a crash line in place of its results.
struct Failed {
    crash: String,
    trace: String,
}

impl From<SimError> for Failed {
    fn from(e: SimError) -> Self {
        match e {
            SimError::Crashed(c) => Self { crash: c.to_string(), trace: c.trace },
            SimError::TimedOut { turn, budget } => Self {
                crash: format!(
                    "GameTimeout: still playing at turn {turn} after its {}-minute budget",
                    general3(budget.as_secs_f64() / 60.0)
                ),
                trace: String::new(),
            },
            SimError::Config(e) => {
                Self { crash: format!("ConfigError: {e}"), trace: String::new() }
            }
            SimError::Refused(e) => {
                Self { crash: format!("ActionError: {e}"), trace: String::new() }
            }
        }
    }
}

/// Plays game `spec` and sums it up as its line; a crash, a refusal, a broken invariant or a game
/// past its budget is its crash line.
#[must_use]
pub fn play_one(cx: &Context, spec: &GameSpec) -> BaselineLine {
    let t0 = Instant::now();
    let cpu0 = ThreadTime::try_now().ok();
    match play(cx, spec, t0, cpu0) {
        Ok(game) => BaselineLine::Game(game),
        Err(f) => BaselineLine::Crash(crash_line(cx, spec, f.crash, f.trace, Some(t0))),
    }
}

/// A crash line for `spec`: `seconds` when the game was timed (`started`), none for a worker
/// that died or stalled, whose line the run writes without it (`baseline.py:234`).
#[must_use]
pub fn crash_line(
    cx: &Context,
    spec: &GameSpec,
    crash: String,
    trace: String,
    started: Option<Instant>,
) -> BaselineCrash {
    BaselineCrash {
        i: spec.i,
        seed: spec.seed,
        size: spec.size.clone(),
        map_type: spec.map_type.clone(),
        barbarians: spec.barbarians.clone(),
        speed: spec.speed.clone(),
        turn_limit: spec.turn_limit,
        engine: cx.code.engine.clone(),
        bot: cx.code.bot.clone(),
        crash,
        trace,
        seconds: started.map(|t| round1(t.elapsed().as_secs_f64())),
    }
}

fn play(
    cx: &Context,
    spec: &GameSpec,
    t0: Instant,
    cpu0: Option<ThreadTime>,
) -> Result<BaselineGame, Failed> {
    let run = RunSpec {
        config: spec.config(cx.rules),
        raise_errors: true,
        budget: Some(spec.budget),
        debug: cx.checks.then_some(DebugOptions { invariants: true, verify_caches: false }),
        ..RunSpec::default()
    };
    let mut aggr = BTreeMap::new();
    let mut r = Runner::new_with(cx.rules, run, |g| {
        g.majors(false)
            .map(|p| {
                let a = aggression(p.id(), spec.seed);
                aggr.insert(p.id(), a);
                let s = BotSpec::new(cx.version, Arc::clone(&cx.tuning), None, Some(a));
                (p.id(), Box::new(Bot::new(Arc::new(s))) as Box<dyn SeatDriver>)
            })
            .collect()
    })?;
    let majors: Vec<PlayerId> = r.game().majors(false).map(|p| p.id()).collect();
    let mut tally = Tally::default();
    // Checkpoint turn -> the totals at the end of that turn.
    let mut snaps: BTreeMap<Turn, BTreeMap<PlayerId, Totals>> = BTreeMap::new();
    let mut seen = r.game().turn();
    while !r.is_over() {
        let step = r.step()?;
        tally.count(r.game(), &step.events);
        let broken = r.take_violations();
        if let Some(first) = broken.first() {
            let all: Vec<String> = broken.iter().take(20).map(ToString::to_string).collect();
            return Err(Failed {
                crash: format!(
                    "InvariantViolation: {} on turn {}; the first: {first}",
                    plural(broken.len(), "violation"),
                    r.game().turn()
                ),
                trace: all.join("\n"),
            });
        }
        // At the start of round T+1, the totals are those at the end of turn T, like the
        // engine's stats; a game that ended on the new turn never began it.
        if r.game().turn() != seen && r.game().phase() == Phase::Playing {
            seen = r.game().turn();
            let done = seen - 1;
            if spec.checkpoints.contains(&done) {
                snaps.insert(done, majors.iter().map(|&p| (p, tally.of(p))).collect());
            }
        }
    }
    let g = r.game();
    let stats: BTreeMap<Turn, &StatsRow> = g.stats(None).iter().map(|s| (s.turn, s)).collect();
    let last = stats.keys().next_back().copied();
    if let Some(last) = last
        && spec.checkpoints.contains(&last)
        && !snaps.contains_key(&last)
    {
        // The game ended in the end-of-round checks right after turn `last` was recorded (a turn
        // limit, a vote), so round last+1, where the snapshot is taken, never began; nothing
        // that declares a war or takes a city has run since, so the totals now are those then.
        snaps.insert(last, majors.iter().map(|&p| (p, tally.of(p))).collect());
    }
    let row_of = |turn: Turn, pid: PlayerId| {
        stats.get(&turn).and_then(|s| s.civs.iter().find(|c| c.player == pid))
    };
    let mut civs = Vec::with_capacity(majors.len());
    for &pid in &majors {
        let Some(p) = g.player(pid) else { continue };
        let mut at = BTreeMap::new();
        for &cp in &spec.checkpoints {
            if let Some(row) = row_of(cp, pid) {
                let totals = snaps.get(&cp).and_then(|s| s.get(&pid)).copied().unwrap_or_default();
                at.insert(cp.to_string(), checkpoint_row(Some(row), totals));
            }
        }
        if let Some(last) = last {
            let mut end = checkpoint_row(row_of(last, pid), tally.of(pid));
            end.turn = u32::try_from(last).ok();
            at.insert("end".to_owned(), end);
        }
        let a = aggr.get(&pid).copied().unwrap_or(BotSpec::DEFAULT_AGGRESSION);
        civs.push(BaselineCiv {
            pid: u32::from(pid.0),
            nation: g.rules().name(p.nation).unwrap_or("?").to_owned(),
            aggression: round_ndigits(a, 3),
            alive: p.alive(),
            eliminated_turn: p.eliminated_turn().and_then(|t| u32::try_from(t).ok()),
            final_score: if p.alive() { i64::from(score(g, pid).total) } else { 0 },
            at,
        });
    }
    let st = g.state();
    let cpu = cpu0.and_then(|c| c.try_elapsed().ok()).map_or(0.0, |d| d.as_secs_f64());
    Ok(BaselineGame {
        i: spec.i,
        seed: spec.seed,
        size: spec.size.clone(),
        map_type: spec.map_type.clone(),
        barbarians: spec.barbarians.clone(),
        speed: spec.speed.clone(),
        turn_limit: spec.turn_limit,
        players: u32::try_from(majors.len()).unwrap_or(u32::MAX),
        turns: u32::try_from(st.clock().turn - 1).unwrap_or(0),
        winner: st.clock().winner.map(|p| u32::from(p.0)),
        victory: won_by(g).map(|w| w.name(g.rules()).to_owned()),
        civs,
        // A bot that panics ends its game as a crash line: a finished game had none.
        bot_errors: 0,
        engine: cx.code.engine.clone(),
        bot: cx.code.bot.clone(),
        seconds: round1(t0.elapsed().as_secs_f64()),
        cpu_s: round1(cpu),
    })
}

/// One civilization at one checkpoint (`baseline._row`): the engine's stats row (none, or not
/// alive, reads as dead with a score of 0) and the totals.
fn checkpoint_row(row: Option<&CivStats>, t: Totals) -> CheckpointRow {
    let alive = row.filter(|r| r.alive);
    CheckpointRow {
        alive: alive.is_some(),
        cities: alive.map(|r| i64::from(r.cities)),
        population: alive.map(|r| i64::from(r.population)),
        techs: alive.map(|r| i64::from(r.techs)),
        score: Some(alive.map_or(0, |r| i64::from(r.score))),
        military: alive.map(|r| r.military),
        era: alive.map(|r| i64::from(r.era.0)),
        policies: alive.map(|r| i64::from(r.policies)),
        land: alive.map(|r| i64::from(r.land)),
        wars_declared: t.wars_declared,
        wars_declared_on_majors: t.wars_declared_on_majors,
        wars_declared_on_others: t.wars_declared_on_others,
        cities_captured: t.cities_captured,
        cities_lost: t.cities_lost,
        turn: None,
    }
}

/// `x` rounded to a tenth, as Python's `round(x, 1)`.
#[must_use]
pub fn round1(x: f64) -> f64 {
    round_ndigits(x, 1)
}

/// `n thing` or `n things`.
fn plural(n: usize, thing: &str) -> String {
    if n == 1 { format!("1 {thing}") } else { format!("{n} {thing}s") }
}

/// `x` as Python's `format(x, ".3g")`: three significant digits, no trailing zeros, an exponent
/// only far from 1 (budgets are minutes, so it never has one in practice).
#[must_use]
pub fn general3(x: f64) -> String {
    if !x.is_finite() || x == 0.0 {
        return format!("{x}");
    }
    // The exponent of x once rounded to three significant digits, as %g decides it.
    let e = format!("{x:.2e}");
    let exp: i32 = e.split_once('e').and_then(|(_, p)| p.parse().ok()).unwrap_or(0);
    if !(-4..3).contains(&exp) {
        let (m, _) = e.split_once('e').unwrap_or((&e, ""));
        let m = m.trim_end_matches('0').trim_end_matches('.');
        return format!("{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    let decimals = usize::try_from(2 - exp).unwrap_or(0);
    let s = format!("{x:.decimals$}");
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_owned() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggression_spreads_by_seat_and_seed_as_the_lab_did() {
        // 0.25 + 0.5 * ((pid * 37 + seed) % 10) / 9, as in the committed smoke file.
        assert!((round_ndigits(aggression(PlayerId(0), 5001), 3) - 0.306).abs() < 1e-12);
        assert!((round_ndigits(aggression(PlayerId(1), 5001), 3) - 0.694).abs() < 1e-12);
        assert!((aggression(PlayerId(0), 5000) - 0.25).abs() < 1e-12);
        assert!((aggression(PlayerId(0), 5009) - 0.75).abs() < 1e-12);
    }

    #[test]
    fn minutes_print_as_python_prints_them() {
        assert_eq!(general3(60.0), "60");
        assert_eq!(general3(140.42), "140");
        assert_eq!(general3(20.0), "20");
        assert_eq!(general3(2.5), "2.5");
        assert_eq!(general3(0.016_666), "0.0167");
        assert_eq!(general3(1234.0), "1.23e+03");
        assert_eq!(general3(999.6), "1e+03");
    }

    #[test]
    fn a_dead_civilizations_row_has_a_score_of_0_and_its_totals() {
        let t = Totals { wars_declared: 2, cities_lost: 1, ..Totals::default() };
        let row = checkpoint_row(None, t);
        let v = serde_json::to_value(&row).expect("serialises");
        assert_eq!(
            v.to_string(),
            r#"{"alive":false,"score":0,"wars_declared":2,"wars_declared_on_majors":0,"wars_declared_on_others":0,"cities_captured":0,"cities_lost":1}"#
        );
    }
}
