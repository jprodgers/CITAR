//! The stages of a turn, as tables (DESIGN.md 6.2): what happens when a player's turn starts
//! ([`PLAYER_START`], `turns.py:20-67`), when it ends ([`PLAYER_END`], `turns.py:70-118`), and
//! when a round ends ([`ROUND_END`], `turns.py:190-202`).
//!
//! Each row is one system's step, labelled with the stage of DESIGN.md 6.2 it belongs to (`S0`
//! to `S9`, `E0` to `E6`, and `R0` to `R6` for the round), and owned by one work package. A row
//! whose system is not ported yet is `Porting::Pending("<package>")` and runs as an explicit
//! no-op; `inspect` lists it, `cargo xtask check` fails once its package is done, and `golden
//! bless` refuses a set that depends on it. The package that ports a system fills in its rows'
//! steps and flips them to `Ported`.
//!
//! The control flow of Python's functions is in the rows too: a row runs for the kinds of player
//! it names, when its condition holds (`if g.player_cities(pid)`, `if g.religion_enabled`), and
//! the `Stop` rows end a table early, for a dead civilization and for the barbarians. A round
//! that ends the game at its eliminations skips to its close (`SkipIfOver`, then the rows
//! `EvenIfOver`), so that the round is still settled and its digest taken. The `Settle` rows are
//! the ◆ settle points, which replace Python's `g.invalidate()` and `visibility.refresh`
//! (`turns.py:36-61, 87, 108, 115`).
//!
//! What differs from Python, on purpose:
//! - happiness is committed at S1 and E1 and read as committed (DESIGN.md 6.6), and the gold
//!   rate is written at E2;
//! - the game ends at its turn limit with no winner until scores exist (package 1c-08 declares
//!   the Time victory).

use crate::base::ids::PlayerId;
use crate::base::stats::Stat;
use crate::game::cities::lifecycle;
use crate::game::derive::rev::UnitTouch;
use crate::game::triggers;
use crate::game::{
    Game, Porting, economy, great_people, pending, policies, religion, research, units,
};
use crate::state::Phase;
use crate::state::TurnClock;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::players::Player;
use crate::state::units::Unit;

bitflags::bitflags! {
    /// The kinds of player a row runs for.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct Who: u8 {
        const MAJOR = 1 << 0;
        const CITY_STATE = 1 << 1;
        const BARBARIAN = 1 << 2;
        /// Majors and city-states: every civilization with cities of its own.
        const CIVS = Self::MAJOR.bits() | Self::CITY_STATE.bits();
        const ALL = Self::CIVS.bits() | Self::BARBARIAN.bits();
    }
}

impl Who {
    /// The kind of `p`.
    #[must_use]
    pub fn of(p: &Player) -> Self {
        if p.is_major() {
            Self::MAJOR
        } else if p.is_city_state() {
            Self::CITY_STATE
        } else {
            Self::BARBARIAN
        }
    }
}

/// A row's condition on the player, besides its kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum When {
    Always,
    /// The civilization has a city (`if g.player_cities(pid)`).
    HasCities,
    /// Religion is in play (`if g.religion_enabled`).
    Religion,
    /// Both.
    HasCitiesAndReligion,
    /// The round's close: runs even in a round a game over cut short (`Step::SkipIfOver`).
    EvenIfOver,
}

impl When {
    fn holds(self, g: &Game, p: PlayerId) -> bool {
        let cities = || g.player_cities(p).next().is_some();
        match self {
            Self::Always | Self::EvenIfOver => true,
            Self::HasCities => cities(),
            Self::Religion => g.religion_enabled(),
            Self::HasCitiesAndReligion => cities() && g.religion_enabled(),
        }
    }
}

/// What a row does.
#[derive(Clone, Copy, Debug)]
pub enum Step {
    /// A step of a player's turn.
    Player(fn(&mut Game, PlayerId)),
    /// A step of the round's end.
    Round(fn(&mut Game)),
    /// A settle point (◆).
    Settle,
    /// A dead civilization's turn ends here.
    StopIfDead,
    /// The barbarians' turn ends here.
    StopIfBarbarian,
    /// Nothing more of the round happens in a game that is over, but its close: the rows
    /// [`When::EvenIfOver`].
    SkipIfOver,
    /// A system not ported yet: nothing happens.
    Pending,
}

/// One row of a stage table.
#[derive(Clone, Copy, Debug)]
pub struct Stage {
    /// The stage of DESIGN.md 6.2 it belongs to: `S2`, `E3`, `R0`.
    pub id: &'static str,
    /// What it does.
    pub name: &'static str,
    /// The kinds of player it runs for.
    pub who: Who,
    /// When it runs, besides.
    pub when: When,
    pub step: Step,
    /// Whether its system is ported, or the package that will port it.
    pub porting: Porting,
}

impl Stage {
    /// A ported row.
    const fn run(id: &'static str, name: &'static str, who: Who, when: When, step: Step) -> Self {
        Self { id, name, who, when, step, porting: Porting::Ported }
    }

    /// A row whose system waits for the package `porting` names.
    const fn later(
        id: &'static str,
        name: &'static str,
        who: Who,
        when: When,
        porting: Porting,
    ) -> Self {
        Self { id, name, who, when, step: Step::Pending, porting }
    }

    /// A settle point.
    const fn settle(id: &'static str, who: Who) -> Self {
        Self::run(id, "settle", who, When::Always, Step::Settle)
    }
}

use When::{Always, EvenIfOver, HasCities, HasCitiesAndReligion, Religion};

/// A player's turn begins (`turns.start_player_turn`, `turns.py:20-67`).
pub static PLAYER_START: [Stage; 23] = [
    Stage::run("S0", "a dead civilization plays no turn", Who::ALL, Always, Step::StopIfDead),
    Stage::run(
        "S0",
        "the barbarians' units start their turn",
        Who::BARBARIAN,
        Always,
        Step::Player(units::turn::start_units),
    ),
    Stage::later("S0", "the barbarians act", Who::BARBARIAN, Always, Porting::Pending("1c-06")),
    Stage::settle("S0", Who::BARBARIAN),
    Stage::run("S0", "the barbarians' turn ends here", Who::ALL, Always, Step::StopIfBarbarian),
    Stage::run(
        "S1",
        "commit the happiness conditionals see",
        Who::CIVS,
        Always,
        Step::Player(economy::commit_happiness_stage),
    ),
    Stage::settle("S1", Who::CIVS),
    Stage::run("S2", "research progress", Who::CIVS, HasCities, Step::Player(research::start_turn)),
    Stage::run("S2", "great people", Who::CIVS, HasCities, Step::Player(great_people::start_turn)),
    Stage::run(
        "S2",
        "religion",
        Who::CIVS,
        HasCitiesAndReligion,
        Step::Player(religion::start_turn_stage),
    ),
    Stage::run(
        "S2",
        "the Maya long count",
        Who::CIVS,
        HasCities,
        Step::Player(great_people::maya_long_count),
    ),
    Stage::later(
        "S3",
        "city-states' great-person gifts",
        Who::MAJOR,
        Always,
        Porting::Pending("1c-06"),
    ),
    Stage::later("S3", "revolts", Who::MAJOR, Always, Porting::Pending("1c-08")),
    Stage::run(
        "S4",
        "triggers upon turn start",
        Who::CIVS,
        Always,
        Step::Player(triggers::turn_start),
    ),
    Stage::run(
        "S5",
        "cities start their turn",
        Who::CIVS,
        Always,
        Step::Player(lifecycle::start_turn_stage),
    ),
    Stage::run(
        "S6",
        "units start their turn",
        Who::CIVS,
        Always,
        Step::Player(units::turn::start_units),
    ),
    Stage::settle("S7", Who::CIVS),
    Stage::later("S8", "the city-state's turn", Who::CITY_STATE, Always, Porting::Pending("1c-06")),
    Stage::later("S8", "standing unit orders", Who::MAJOR, Always, Porting::Pending("1c-04")),
    Stage::settle("S9", Who::CIVS),
    Stage::later("S9", "victory", Who::CIVS, Always, Porting::Pending("1c-08")),
    Stage::run("S9", "a research reminder", Who::MAJOR, Always, Step::Player(research::remind)),
    Stage::run("S9", "the turn's announcement", Who::MAJOR, Always, Step::Player(announce_start)),
];

/// A player's turn ends (`turns.end_player_turn`, `turns.py:70-118`).
pub static PLAYER_END: [Stage; 25] = [
    Stage::later(
        "E0",
        "the player's negotiations expire",
        Who::MAJOR,
        Always,
        Porting::Pending("1c-05"),
    ),
    Stage::run(
        "E0",
        "unanswered offers to return a civilian lapse",
        Who::MAJOR,
        Always,
        Step::Player(lapse_return_offers),
    ),
    Stage::run("E0", "a dead civilization ends no turn", Who::ALL, Always, Step::StopIfDead),
    Stage::run(
        "E0",
        "the barbarians' units end their turn",
        Who::BARBARIAN,
        Always,
        Step::Player(units::turn::end_units),
    ),
    Stage::run("E0", "the barbarians' turn ends here", Who::ALL, Always, Step::StopIfBarbarian),
    Stage::run("E1", "triggers upon turn end", Who::CIVS, Always, Step::Player(triggers::turn_end)),
    Stage::run(
        "E1",
        "commit the happiness conditionals see",
        Who::CIVS,
        Always,
        Step::Player(economy::commit_happiness_stage),
    ),
    Stage::settle("E1", Who::CIVS),
    Stage::run(
        "E2",
        "the civilization's yields, its gold rate and totals",
        Who::CIVS,
        Always,
        Step::Player(economy::end_turn_rates),
    ),
    Stage::run("E3", "culture and policies", Who::CIVS, Always, Step::Player(culture_and_policies)),
    Stage::later(
        "E3",
        "the city-state's own end of turn",
        Who::CITY_STATE,
        Always,
        Porting::Pending("1c-06"),
    ),
    Stage::run(
        "E3",
        "gold and bankruptcy",
        Who::CIVS,
        Always,
        Step::Player(economy::end_turn_gold),
    ),
    Stage::run("E3", "science", Who::CIVS, HasCities, Step::Player(science)),
    Stage::run("E3", "faith", Who::CIVS, Religion, Step::Player(faith)),
    Stage::later("E3", "espionage", Who::MAJOR, Always, Porting::Pending("1c-05")),
    Stage::run(
        "E3",
        "great person points",
        Who::MAJOR,
        Always,
        Step::Player(great_people::end_turn),
    ),
    Stage::run(
        "E4",
        "cities end their turn, razing ones first",
        Who::CIVS,
        Always,
        Step::Player(cities_end),
    ),
    Stage::run(
        "E5",
        "temporary uniques expire",
        Who::CIVS,
        Always,
        Step::Player(economy::expire_temp_uniques),
    ),
    Stage::settle("E5", Who::CIVS),
    Stage::run(
        "E6",
        "golden-age progress",
        Who::MAJOR,
        Always,
        Step::Player(great_people::golden_age_stage),
    ),
    Stage::later("E6", "worker builds", Who::CIVS, Always, Porting::Pending("1c-04")),
    Stage::run(
        "E6",
        "units end their turn",
        Who::CIVS,
        Always,
        Step::Player(units::turn::end_units),
    ),
    Stage::settle("E6", Who::CIVS),
    Stage::later("E6", "victory", Who::CIVS, Always, Porting::Pending("1c-08")),
    Stage::run("E6", "the turn's close", Who::MAJOR, Always, Step::Player(announce_end)),
];

/// Every player has moved: the round ends (`turns.end_round`, `turns.py:190-202`).
pub static ROUND_END: [Stage; 11] = [
    Stage::later("R0", "eliminations", Who::ALL, Always, Porting::Pending("1c-08")),
    Stage::run("R0", "a game that is over skips to the close", Who::ALL, Always, Step::SkipIfOver),
    Stage::later("R1", "diplomacy's round", Who::ALL, Always, Porting::Pending("1c-05")),
    Stage::later("R2", "the round's statistics", Who::ALL, Always, Porting::Pending("1c-08")),
    Stage::later("R3", "the replay frame", Who::ALL, Always, Porting::Pending("1c-08")),
    Stage::run("R4", "the next turn", Who::ALL, Always, Step::Round(next_turn)),
    Stage::later("R5", "the world leader vote", Who::ALL, Always, Porting::Pending("1c-08")),
    Stage::later("R5", "victory", Who::ALL, Always, Porting::Pending("1c-08")),
    Stage::run("R5", "the turn limit", Who::ALL, Always, Step::Round(turn_limit)),
    Stage::run("R6", "settle", Who::ALL, EvenIfOver, Step::Settle),
    Stage::run(
        "R6",
        "the round's digest, if the game chains",
        Who::ALL,
        EvenIfOver,
        Step::Round(chain),
    ),
];

/// The three tables, by name, for listings.
pub static TABLES: [(&str, &[Stage]); 3] =
    [("player_start", &PLAYER_START), ("player_end", &PLAYER_END), ("round_end", &ROUND_END)];

/// Every row still waiting for its package: `(table, row, package)`, in table order.
pub fn waiting() -> impl Iterator<Item = (&'static str, &'static Stage, &'static str)> {
    TABLES.iter().flat_map(|&(table, rows)| {
        rows.iter().filter_map(move |s| match s.porting {
            Porting::Pending(pkg) => Some((table, s, pkg)),
            Porting::Ported => None,
        })
    })
}

/// Runs a player's table for `p`, row by row.
pub(super) fn run_player(g: &mut Game, table: &[Stage], p: PlayerId) {
    let Some(who) = g.player(p).map(Who::of) else { return };
    for s in table {
        if !s.who.intersects(who) {
            continue;
        }
        match s.step {
            Step::StopIfDead if !g.player(p).is_some_and(Player::alive) => return,
            Step::StopIfBarbarian if who == Who::BARBARIAN => return,
            Step::StopIfDead | Step::StopIfBarbarian | Step::Pending => {}
            Step::SkipIfOver => {
                debug_assert!(false, "a round's skip in a player's table: {}", s.name);
            }
            _ if !s.when.holds(g, p) => {}
            Step::Player(f) => f(g, p),
            Step::Settle => g.settle(),
            Step::Round(_) => {
                debug_assert!(false, "a round's step in a player's table: {}", s.name)
            }
        }
    }
}

/// Runs the round's table: after a [`Step::SkipIfOver`] in a game that is over, only its close.
pub(super) fn run_round(g: &mut Game, table: &[Stage]) {
    let mut over = false;
    for s in table {
        if over && s.when != When::EvenIfOver {
            continue;
        }
        match s.step {
            Step::SkipIfOver => over = g.phase() != Phase::Playing,
            Step::StopIfDead | Step::StopIfBarbarian | Step::Pending => {}
            Step::Round(f) => f(g),
            Step::Settle => g.settle(),
            Step::Player(_) => {
                debug_assert!(false, "a player's step in the round's table: {}", s.name)
            }
        }
    }
}

// ---- The steps this package ports --------------------------------------------------------------

/// `turn_start` (`turns.py:66-67`): "Turn 12 (3560 BC): Rome's turn."
fn announce_start(g: &mut Game, p: PlayerId) {
    let Some(name) = g.player(p).map(|x| x.name.clone()) else { return };
    let text = format!("Turn {} ({}): {name}'s turn.", g.turn(), g.year_text(None));
    let data = EventData { player: Some(p), ..EventData::default() };
    g.emit(EngineEvent::TurnStart, &text, None, None, data, &[]);
}

/// `turn_end` (`turns.py:117-118`): "Rome ended their turn."
fn announce_end(g: &mut Game, p: PlayerId) {
    let Some(name) = g.player(p).map(|x| x.name.clone()) else { return };
    let data = EventData { player: Some(p), ..EventData::default() };
    g.emit(EngineEvent::TurnEnd, &format!("{name} ended their turn."), None, None, data, &[]);
}

/// An offer to return a recaptured civilian that its owner left unanswered lapses at the end of
/// its turn, and the civilian is kept (`turns.py:77-78`).
fn lapse_return_offers(g: &mut Game, p: PlayerId) {
    let offered: Vec<_> =
        g.player_units(p).filter(|u| u.return_offer.is_some()).map(Unit::id).collect();
    for u in offered {
        if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
            x.return_offer = None;
        }
    }
}

/// The civilization's yield of `stat` this turn, as stage E2 read it for the end of the turn
/// (`turns.py:88-89`: Python read `civ_stats` once, before banking gold), or read now for a
/// civilization E2 has not read this turn.
fn turn_yield(g: &Game, p: PlayerId, stat: Stat) -> f64 {
    match g.turn_yields {
        Some((q, s)) if q == p => s[stat],
        _ => crate::game::derive::stats::civ_stats(g, p).total[stat],
    }
}

/// Stage E3, culture and policies (`policies.end_turn`, `turns.py:90`).
fn culture_and_policies(g: &mut Game, p: PlayerId) {
    let culture = turn_yield(g, p, Stat::Culture);
    policies::end_turn(g, p, culture);
}

/// Stage E3, faith (`religion.end_turn`, `turns.py:98-99`).
fn faith(g: &mut Game, p: PlayerId) {
    let faith = turn_yield(g, p, Stat::Faith);
    religion::end_turn(g, p, faith);
}

/// Stage E3, science (`research.end_turn`, `turns.py:96-97`).
fn science(g: &mut Game, p: PlayerId) {
    let science = turn_yield(g, p, Stat::Science);
    research::end_turn(g, p, science);
}

/// Stage E4: the cities end their turn, and the turn's yields read at E2 are done with.
fn cities_end(g: &mut Game, p: PlayerId) {
    lifecycle::end_turn_stage(g, p);
    g.turn_yields = None;
}

/// The turn number moves on (`turns.py:201`).
fn next_turn(g: &mut Game) {
    let c = *g.state().clock();
    g.set_clock(TurnClock { turn: c.turn.saturating_add(1), ..c });
}

/// The game ends once its last turn is past (`victory.check_turn_limit`, `victory.py:352-366`),
/// if a major civilization is still alive.
fn turn_limit(g: &mut Game) {
    if g.phase() != Phase::Playing || g.turn() <= g.total_turns() || g.majors(true).next().is_none()
    {
        return;
    }
    // With the Time victory on, the best score wins it (`victory.score`); until scores exist, the
    // game ends with no winner, as it does with the Time victory off.
    pending(Porting::Pending("1c-08"));
    let c = *g.state().clock();
    g.set_clock(TurnClock { phase: Phase::Over, ..c });
    g.emit(
        EngineEvent::GameOver,
        "The turn limit has been reached. The game ends with no winner.",
        None,
        None,
        EventData::default(),
        &[],
    );
}

/// The round's digest, folded into the chain of a game that keeps one (DESIGN.md 4.10).
fn chain(g: &mut Game) {
    g.chain_round();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_does_something_exactly_when_it_is_ported() {
        for (table, rows) in TABLES {
            for s in rows {
                assert_eq!(
                    matches!(s.step, Step::Pending),
                    matches!(s.porting, Porting::Pending(_)),
                    "{table} {} {}",
                    s.id,
                    s.name
                );
            }
        }
    }

    #[test]
    fn rows_keep_the_order_of_their_stages() {
        for (table, rows) in TABLES {
            let ids: Vec<(u8, u8)> = rows
                .iter()
                .map(|s| {
                    let b = s.id.as_bytes();
                    (b[0], b[1..].iter().fold(0, |n, d| n * 10 + (d - b'0')))
                })
                .collect();
            assert!(ids.windows(2).all(|w| w[0] <= w[1]), "{table}: {ids:?}");
        }
        let last = |rows: &[Stage]| rows.last().map(|s| s.id);
        assert_eq!(
            [last(&PLAYER_START), last(&PLAYER_END), last(&ROUND_END)],
            [Some("S9"), Some("E6"), Some("R6")]
        );
    }

    #[test]
    fn only_the_round_table_takes_round_steps() {
        let round = |s: &Stage| {
            matches!(s.step, Step::Round(_) | Step::SkipIfOver) || s.when == When::EvenIfOver
        };
        assert!(PLAYER_START.iter().chain(&PLAYER_END).all(|s| !round(s)));
        assert!(ROUND_END.iter().all(|s| !matches!(s.step, Step::Player(_))));
    }

    #[test]
    fn a_round_that_ends_the_game_keeps_its_close() {
        // The close is the last rows, R6: a settle, then the digest.
        let close: Vec<&str> =
            ROUND_END.iter().filter(|s| s.when == When::EvenIfOver).map(|s| s.id).collect();
        assert_eq!(close, ["R6", "R6"]);
        let first = ROUND_END.iter().position(|s| s.when == When::EvenIfOver);
        assert!(first.is_some_and(|i| ROUND_END[i..].iter().all(|s| s.when == When::EvenIfOver)));
        assert!(matches!(ROUND_END[ROUND_END.len() - 2].step, Step::Settle));
    }
}
