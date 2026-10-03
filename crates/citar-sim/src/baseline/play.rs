//! One baseline game, played and summed up as its line (`baseline.py:51-160`).
//!
//! Every major civilization is a `basic-1` bot whose aggression is spread by seat and seed (the
//! lab's formula, `common.make_bots`), so a baseline covers peaceful and warlike bots in every
//! start position. The war and capture totals are kept from the game's events ([`Tally`]), since
//! the engine records no such counters, with each checkpoint's totals at the end of its turn, as
//! the engine's stats row is. A crash, a refusal, a panic or a game past its budget is returned
//! as the game's crash line, never raised: the run goes on and a resumed run plays it again.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Instant;

use citar_bot::{Bot, BotSpec, Tuning, VersionId};
use citar_engine::base::ids::{PlayerId, Turn};
use citar_engine::base::num::round_ndigits;
use citar_engine::game::victory::{score, won_by};
use citar_engine::game::{DebugOptions, EventBatch, Game, SeatDriver};
use citar_engine::rules::Ruleset;
use citar_engine::state::chronicle::{CivStats, EngineEvent, EventType, StatsRow};
use cpu_time::ThreadTime;

use super::spec::GameSpec;
use super::{BaselineCiv, BaselineCrash, BaselineGame, BaselineLine, CheckpointRow};
use crate::panics;
use crate::runner::{RunSpec, Runner, Seats, SimError, TRACEBACK_LIMIT, panic_message};

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

/// A bot of `version` with `tuning` in every major's seat of `g`, its aggression spread by seat
/// and `seed` ([`aggression`]): the baseline's seats, and the game benchmarks'.
#[must_use]
pub fn bot_seats(g: &Game, seed: u64, version: VersionId, tuning: &Arc<Tuning>) -> Seats {
    g.majors(false)
        .map(|p| {
            let s = BotSpec::new(version, Arc::clone(tuning), None, Some(aggression(p.id(), seed)));
            (p.id(), Box::new(Bot::new(Arc::new(s))) as Box<dyn SeatDriver>)
        })
        .collect()
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
/// game's events (`baseline.Tally`), and the totals at the end of each checkpoint's turn.
///
/// Python took a checkpoint's totals in its round hook, once round T+1 had begun and before any
/// bot played or answered in it (`common.play`). The runner's steps do not fall there: the step
/// that begins round T+1 goes on until the drive's seat limit stops it, and a drive first puts
/// the chats that wait on drivers to them, so an answer that brings a war could land in that
/// step. So checkpoint T's totals are taken by the events' own turns: the totals just before
/// the first event of a later turn. Nothing declares a war or takes a city as a turn begins
/// (wars come from a seat's actions and answers, captures from its attacks), so these are the
/// totals Python's hook saw. A checkpoint that no later event has passed reads the totals so
/// far, which are then those at its end: that is Python's fallback for a game that ended in the
/// end-of-round checks right after the checkpoint (a turn limit, a vote), where the hook never
/// ran.
#[derive(Clone, Debug, Default)]
pub struct Tally {
    counts: BTreeMap<PlayerId, Totals>,
    /// The checkpoints no event has passed yet, the latest first.
    pending: Vec<Turn>,
    /// Each checkpoint passed, with every civilization's totals at the end of its turn.
    taken: BTreeMap<Turn, BTreeMap<PlayerId, Totals>>,
}

impl Tally {
    /// A tally that keeps the totals at the end of each of `checkpoints`' turns.
    #[must_use]
    pub fn new(checkpoints: &[Turn]) -> Self {
        let mut pending = checkpoints.to_vec();
        pending.sort_unstable_by(|a, b| b.cmp(a));
        pending.dedup();
        Self { counts: BTreeMap::new(), pending, taken: BTreeMap::new() }
    }

    /// Counts the events of a step: a war declared, by its attacker and the defender's kind; a
    /// city captured, for its new owner and its old. An event of a turn after a checkpoint's
    /// first takes the checkpoint's totals.
    pub fn count(&mut self, g: &Game, events: &EventBatch) {
        for ev in events.events() {
            self.pass(ev.turn);
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

    /// Takes the totals of every checkpoint before `turn`: an event of `turn` comes after the
    /// end of theirs.
    fn pass(&mut self, turn: Turn) {
        while let Some(&cp) = self.pending.last()
            && cp < turn
        {
            self.pending.pop();
            self.taken.insert(cp, self.counts.clone());
        }
    }

    /// One civilization's totals so far.
    #[must_use]
    pub fn of(&self, pid: PlayerId) -> Totals {
        self.counts.get(&pid).copied().unwrap_or_default()
    }

    /// One civilization's totals at the end of checkpoint `cp`'s turn: those taken when the
    /// first event of a later turn came, or the totals so far while none has. `None` for a turn
    /// that is not one of the checkpoints.
    #[must_use]
    pub fn at(&self, cp: Turn, pid: PlayerId) -> Option<Totals> {
        match self.taken.get(&cp) {
            Some(then) => Some(then.get(&pid).copied().unwrap_or_default()),
            None => self.pending.contains(&cp).then(|| self.of(pid)),
        }
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

/// Plays game `spec` with the run's bots in its seats and sums it up as its line; a crash, a
/// refusal, a broken invariant, a panic or a game past its budget is its crash line.
#[must_use]
pub fn play_one(cx: &Context, spec: &GameSpec) -> BaselineLine {
    play_seated(cx, spec, |g| bot_seats(g, spec.seed, cx.version, &cx.tuning))
}

/// [`play_one`] with the drivers `seats` makes for the new game.
///
/// The runner catches a panic inside a drive. One outside it, in making the game (its map), in
/// seating the bots or in summing the game up, is caught here, so its line says where it
/// happened, as Python's `play_one` caught every error of its game and wrote its traceback.
fn play_seated(cx: &Context, spec: &GameSpec, seats: impl FnOnce(&Game) -> Seats) -> BaselineLine {
    let t0 = Instant::now();
    let cpu0 = ThreadTime::try_now().ok();
    // A trace left by a panic someone caught earlier on this thread is not this game's.
    panics::forget();
    let played = catch_unwind(AssertUnwindSafe(|| play(cx, spec, seats, t0, cpu0)));
    let (crash, trace) = match played {
        Ok(Ok(game)) => return BaselineLine::Game(game),
        Ok(Err(f)) => (f.crash, f.trace),
        Err(payload) => {
            let trace = panics::take().map(|t| t.text(TRACEBACK_LIMIT)).unwrap_or_default();
            (format!("panic: {}", panic_message(payload.as_ref())), trace)
        }
    };
    BaselineLine::Crash(crash_line(cx, spec, crash, trace, Some(t0)))
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
    seats: impl FnOnce(&Game) -> Seats,
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
    let mut r = Runner::new_with(cx.rules, run, seats)?;
    let majors: Vec<PlayerId> = r.game().majors(false).map(|p| p.id()).collect();
    let mut tally = Tally::new(&spec.checkpoints);
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
    }
    let g = r.game();
    let stats: BTreeMap<Turn, &StatsRow> = g.stats(None).iter().map(|s| (s.turn, s)).collect();
    let last = stats.keys().next_back().copied();
    let row_of = |turn: Turn, pid: PlayerId| {
        stats.get(&turn).and_then(|s| s.civs.iter().find(|c| c.player == pid))
    };
    let mut civs = Vec::with_capacity(majors.len());
    for &pid in &majors {
        let Some(p) = g.player(pid) else { continue };
        let mut at = BTreeMap::new();
        for &cp in &spec.checkpoints {
            if let Some(row) = row_of(cp, pid) {
                let totals = tally.at(cp, pid).unwrap_or_default();
                at.insert(cp.to_string(), checkpoint_row(Some(row), totals));
            }
        }
        if let Some(last) = last {
            let mut end = checkpoint_row(row_of(last, pid), tally.of(pid));
            end.turn = u32::try_from(last).ok();
            at.insert("end".to_owned(), end);
        }
        // What its bot plays with: in 0..1 already, so the spec held it as it is.
        let a = aggression(pid, spec.seed);
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
    use crate::baseline::spec::SMOKE_CHECKPOINTS;
    use citar_bot::Overrides;
    use citar_engine::base::ids::{EventId, NegotiationId};
    use citar_engine::game::diplomacy::actions::DeclareWar;
    use citar_engine::game::setup::config_from_value;
    use citar_engine::game::{Action, DriverOutcome};
    use citar_engine::state::chronicle::{Event, EventData};
    use citar_engine::state::players::DriverMemory;
    use serde_json::json;
    use std::time::Duration;

    /// A duel with a city-state, for the defenders' kinds.
    fn duel_with_a_city_state() -> (Game, PlayerId) {
        let r = Ruleset::shared();
        let cfg = json!({"seed": 3, "map_size": "duel", "map_type": "pangaea",
                         "players": [{"controller": "bot"}, {"controller": "bot"}],
                         "city_states": 1, "barbarians": "off"});
        let (g, _) = Game::new(r, &config_from_value(r, cfg).expect("settings")).expect("a game");
        let cs = g.city_states(false).next().map(|p| p.id()).expect("a city-state");
        (g, cs)
    }

    fn event(id: u32, turn: Turn, kind: EngineEvent, data: EventData) -> Event {
        Event {
            id: EventId::new(id).expect("an id"),
            turn,
            kind: EventType::Engine(kind),
            text: "".into(),
            audience: None,
            tile: None,
            data: Some(Box::new(data)),
            refs: Default::default(),
        }
    }

    fn war(id: u32, turn: Turn, attacker: Option<PlayerId>, defender: Option<PlayerId>) -> Event {
        let data = EventData { attacker, defender, ..EventData::default() };
        event(id, turn, EngineEvent::WarDeclared, data)
    }

    fn capture(id: u32, turn: Turn, new: Option<PlayerId>, old: Option<PlayerId>) -> Event {
        let data = EventData { new_owner: new, old_owner: old, ..EventData::default() };
        event(id, turn, EngineEvent::CityCaptured, data)
    }

    const A: PlayerId = PlayerId(0);
    const B: PlayerId = PlayerId(1);

    #[test]
    fn the_tally_counts_wars_by_the_defenders_kind_and_captures_both_ways() {
        let (g, cs) = duel_with_a_city_state();
        let mut t = Tally::new(&[]);
        t.count(
            &g,
            &EventBatch::new(vec![
                war(1, 5, Some(A), Some(B)),
                war(2, 5, Some(A), Some(cs)),
                // No attacker: not counted (baseline.py skips it).
                war(3, 6, None, Some(B)),
                // No defender: on others, as Python's kind test reads it.
                war(4, 6, Some(B), None),
                capture(5, 7, Some(A), Some(B)),
                capture(6, 7, Some(cs), Some(A)),
                capture(7, 8, Some(B), None),
                event(8, 8, EngineEvent::Peace, EventData { a: Some(A), ..EventData::default() }),
            ]),
        );
        let totals = |w, m, o, c, l| Totals {
            wars_declared: w,
            wars_declared_on_majors: m,
            wars_declared_on_others: o,
            cities_captured: c,
            cities_lost: l,
        };
        assert_eq!(t.of(A), totals(2, 1, 1, 1, 1));
        assert_eq!(t.of(B), totals(1, 0, 1, 1, 1));
        assert_eq!(t.of(cs), totals(0, 0, 0, 1, 0));
        assert_eq!(t.at(10, A), None, "no checkpoint 10");
    }

    #[test]
    fn a_checkpoint_has_the_totals_at_the_end_of_its_turn() {
        let (g, cs) = duel_with_a_city_state();
        let mut t = Tally::new(&[30, 10, 20]);
        // One step that ends turn 10 and goes on into turn 11 (a drive puts the chats that wait
        // on drivers to them before its seat limit stops it): checkpoint 10 has the war of turn
        // 10 and not those of turn 11, as Python's round hook, before anyone acted in round 11.
        t.count(
            &g,
            &EventBatch::new(vec![
                war(1, 9, Some(A), Some(cs)),
                war(2, 10, Some(B), Some(cs)),
                war(3, 11, Some(A), Some(B)),
                capture(4, 11, Some(A), Some(B)),
            ]),
        );
        let at = |t: &Tally, cp, p| t.at(cp, p).expect("a checkpoint");
        assert_eq!(at(&t, 10, A).wars_declared, 1);
        assert_eq!(at(&t, 10, A).cities_captured, 0);
        assert_eq!(at(&t, 10, B).wars_declared, 1);
        assert_eq!(at(&t, 10, B).cities_lost, 0);
        // A checkpoint no event has passed reads the totals so far.
        assert_eq!(at(&t, 20, A), t.of(A));
        // Turns with no events between: the first event of turn 25 passes checkpoint 20.
        t.count(&g, &EventBatch::new(vec![capture(5, 25, Some(B), Some(A))]));
        assert_eq!(at(&t, 20, A).cities_lost, 0);
        assert_eq!(at(&t, 20, A).cities_captured, 1);
        assert_eq!(t.of(A).cities_lost, 1);
        // The game ended in the checks right after turn 30 was recorded: no later event came,
        // so checkpoint 30 is the end (Python's fallback).
        t.count(&g, &EventBatch::new(vec![war(6, 30, Some(B), Some(A))]));
        assert_eq!(at(&t, 30, B), t.of(B));
        assert_eq!(at(&t, 30, B).wars_declared, 2);
        assert_eq!(at(&t, 20, B).wars_declared, 1);
    }

    /// Who a [`Warmonger`] declares war on.
    #[derive(Clone, Copy)]
    enum Foe {
        Major(PlayerId),
        CityState,
    }

    /// The idle bot, declaring war on the turns it is told.
    struct Warmonger {
        idle: Bot,
        wars: Vec<(Turn, Foe)>,
    }

    impl Warmonger {
        fn seat(pid: PlayerId, wars: Vec<(Turn, Foe)>) -> (PlayerId, Box<dyn SeatDriver>) {
            let tuning = Arc::new(Tuning::new(VersionId::Idle, Overrides::default()));
            let idle = Bot::new(Arc::new(BotSpec::new(VersionId::Idle, tuning, None, None)));
            (pid, Box::new(Self { idle, wars }))
        }
    }

    impl SeatDriver for Warmonger {
        fn play_turn(
            &mut self,
            g: &mut Game,
            pid: PlayerId,
            mem: &mut DriverMemory,
        ) -> DriverOutcome {
            let out = self.idle.play_turn(g, pid, mem);
            let turn = g.turn();
            for &(t, foe) in &self.wars {
                if t != turn {
                    continue;
                }
                let target = match foe {
                    Foe::Major(p) => p,
                    Foe::CityState => g.city_states(true).next().map(|p| p.id()).expect("one"),
                };
                g.meet(pid, target).expect("they meet");
                let war = DeclareWar { player_id: i64::from(target.0), message: None };
                g.act(pid, Action::DeclareWar(war)).expect("war is declared");
            }
            out
        }

        fn respond(
            &mut self,
            g: &mut Game,
            pid: PlayerId,
            nid: NegotiationId,
            mem: &mut DriverMemory,
        ) -> DriverOutcome {
            self.idle.respond(g, pid, nid, mem)
        }
    }

    fn test_context() -> Context {
        let tuning = Arc::new(Tuning::new(VersionId::Idle, Overrides::default()));
        let code = Code { engine: "test".into(), bot: "idle".into() };
        Context { rules: Ruleset::shared(), code, version: VersionId::Idle, tuning, checks: false }
    }

    /// A 30-turn duel with the smoke checkpoints (10, 20, 30), the last the turn limit.
    fn short_duel() -> GameSpec {
        GameSpec {
            i: 0,
            seed: 11,
            size: "duel".into(),
            map_type: "pangaea".into(),
            barbarians: "off".into(),
            speed: "Quick".into(),
            turn_limit: Some(30),
            checkpoints: SMOKE_CHECKPOINTS,
            budget: Duration::from_secs(600),
        }
    }

    #[test]
    fn a_games_line_has_its_wars_at_the_checkpoints_and_the_end() {
        let cx = test_context();
        // Player 1 plays last in a round: its war on turn 10 is in checkpoint 10. Player 0's
        // on turn 11 is in the first step of round 11, and its war on turn 30 in the last
        // round, which the turn limit ends before round 31 begins.
        let line = play_seated(&cx, &short_duel(), |_| {
            vec![
                Warmonger::seat(A, vec![(11, Foe::Major(B)), (30, Foe::CityState)]),
                Warmonger::seat(B, vec![(10, Foe::CityState)]),
            ]
        });
        let BaselineLine::Game(g) = line else { panic!("a crash: {line:?}") };
        assert_eq!((g.turns, g.victory.as_deref()), (30, Some("Time")));
        let civ = |p: PlayerId| g.civs.iter().find(|c| c.pid == u32::from(p.0)).expect("a civ");
        let wars = |p: PlayerId, at: &str| {
            let r = &civ(p).at[at];
            assert_eq!((r.cities_captured, r.cities_lost), (0, 0), "no city changed hands");
            (r.wars_declared, r.wars_declared_on_majors, r.wars_declared_on_others)
        };
        assert_eq!(wars(A, "10"), (0, 0, 0));
        assert_eq!(wars(B, "10"), (1, 0, 1));
        assert_eq!(wars(A, "20"), (1, 1, 0));
        assert_eq!(wars(B, "20"), (1, 0, 1));
        assert_eq!(wars(A, "30"), (2, 1, 1));
        assert_eq!(wars(A, "end"), (2, 1, 1));
        assert_eq!(civ(A).at["end"].turn, Some(30));
        assert_eq!(wars(B, "30"), (1, 0, 1));
        assert!(civ(A).alive && civ(B).alive);
    }

    #[test]
    fn a_panic_outside_the_runner_is_the_games_crash_line_with_where_it_happened() {
        crate::panics::install();
        let cx = test_context();
        let line = play_seated(&cx, &short_duel(), |_| panic!("the seats panic"));
        let BaselineLine::Crash(c) = line else { panic!("a game: {line:?}") };
        assert_eq!(c.crash, "panic: the seats panic");
        assert!(c.trace.starts_with("at ") && c.trace.contains("play.rs"), "{}", c.trace);
        assert!(c.seconds.is_some(), "a game that was timed");
        assert_eq!((c.i, c.seed, c.engine.as_str()), (0, 11, "test"));
    }

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
