//! Victory and defeat (package 1c-08): score and military strength, the United Nations' vote,
//! the milestones and the checks that end the game, eliminations, and what each round records
//! (DESIGN.md 6.2).
//!
//! Replaces `victory.py` and the round's end of `turns.py:190-202`:
//! - [`score`](mod@score): `score` and `military_strength` (`victory.py:23-52`);
//! - [`milestones`]: the spaceship, the milestones and a civilization's progress toward each
//!   victory (`victory.py:58-302`);
//! - [`un`]: the United Nations and the `un_vote` tool (`victory.py:97-221`);
//! - [`records`]: the statistics rows and the replay frames (`victory.py:424-485`);
//! - here: declaring a winner and the checks that do (`victory.py:305-366`), eliminations
//!   (`victory.py:371-406`), and the steps of the stage tables: the victory checks at the end of
//!   a turn's start and of its end (S9, E6), and the round's end (R0 eliminations, R2 the
//!   statistics, R3 the frame, R5 the vote, victory and the turn limit).
//!
//! What differs from Python, on purpose:
//! - **Eliminations** happen as soon as a civilization is defeated, where Python checked (a
//!   capture, a unit lost, a city destroyed, a marriage), except for the player whose turn it is,
//!   which is eliminated as the round ends (`eliminated-on-its-own-turn-at-the-round-end`):
//!   Python ended the turn of a player it had just removed, and a game must always have a living
//!   player whose turn it is (invariant TURN-1). The round's end checks everyone, as Python's
//!   did, and a player eliminated there whose turn it was hands the turn to the next one.
//! - **The neutral victory** of `Triggers victory` is no victory of the ruleset: the game is won
//!   with a winner and no victory type, which `inspect` names `Neutral` as Python did.
//! - **The victories Python named** (Scientific's spaceship, the Domination win of a civilization
//!   left alone, the Diplomatic vote and the Time victory) are the ruleset's own
//!   (`rules::derived::KnownVictories`, `victories-python-named-are-the-rulesets`): a ruleset
//!   without one has none of them, where Python acted as if they were on.

pub mod milestones;
pub mod records;
pub mod score;
pub mod un;

pub use self::milestones::{SpaceshipStatus, spaceship_status, victory_achieved, victory_progress};
pub use self::score::{Score, military_strength, score};
pub use self::un::UnVote;

use crate::base::ids::{PlayerId, VictoryId};
use crate::game::derive::rev::DiploTouch;
use crate::game::diplomacy::negotiation;
use crate::game::{Game, city_states, espionage};
use crate::rules::Ruleset;
use crate::rules::gen_tables::Milestone;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::players::Player;
use crate::state::{Phase, TurnClock};

/// A victory won: one of the ruleset's, or the neutral victory of `Triggers victory`, which no
/// victory of the ruleset stands for (Python's `"Neutral"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Won {
    Victory(VictoryId),
    Neutral,
}

impl Won {
    /// The victory of the ruleset, if it is one.
    #[must_use]
    pub const fn id(self) -> Option<VictoryId> {
        match self {
            Self::Victory(v) => Some(v),
            Self::Neutral => None,
        }
    }

    /// Its name, as Python wrote a game's `victory`.
    #[must_use]
    pub fn name(self, r: &Ruleset) -> &str {
        match self {
            Self::Victory(v) => &r.victories()[v].name,
            Self::Neutral => "Neutral",
        }
    }
}

/// The victory a game over was won by, as `inspect` and the views name it: the ruleset's, or
/// `Neutral` for a game won with no victory type (`Triggers victory`); `None` for a game with no
/// winner.
#[must_use]
pub fn won_by(g: &Game) -> Option<Won> {
    let c = g.state().clock();
    match (c.winner, c.victory) {
        (_, Some(v)) => Some(Won::Victory(v)),
        (Some(_), None) => Some(Won::Neutral),
        (None, None) => None,
    }
}

/// What the world is told when a victory is won (`VICTORY_TEXT`, `victory.py:305-312`).
fn victory_text(g: &Game, won: Won, name: &str) -> String {
    let k = g.rules().derived().known.victories;
    match won {
        Won::Neutral => format!("{name} has won the game!"),
        Won::Victory(v) if Some(v) == k.scientific => {
            format!("{name} launched its spaceship to Alpha Centauri — Scientific Victory!")
        }
        Won::Victory(v) if Some(v) == k.cultural => {
            format!("{name} completed the Utopia Project — Cultural Victory!")
        }
        Won::Victory(v) if Some(v) == k.domination => {
            format!("{name} controls every original capital — Domination Victory!")
        }
        Won::Victory(v) if Some(v) == k.diplomatic => {
            format!("{name} was elected world leader by the United Nations — Diplomatic Victory!")
        }
        Won::Victory(v) if Some(v) == k.time => {
            format!("The final turn has passed. {name} wins with the highest score — Time Victory!")
        }
        Won::Victory(_) => format!("{name} wins!"),
    }
}

/// Ends the game with a winner (`declare_winner`, `victory.py:315-323`), unless it is over.
pub(crate) fn declare_winner(g: &mut Game, p: PlayerId, won: Won, text: Option<String>) {
    if g.phase() != Phase::Playing {
        return;
    }
    let name = g.player(p).map(|x| x.name.to_string()).unwrap_or_default();
    let text = text.unwrap_or_else(|| victory_text(g, won, &name));
    let c = *g.state().clock();
    g.set_clock(TurnClock { phase: Phase::Over, winner: Some(p), victory: won.id(), ..c });
    let data = EventData { winner: Some(p), victory: won.id(), ..EventData::default() };
    g.emit(EngineEvent::Victory, &text, None, None, data, &[]);
}

/// Whether anybody has won, ending the game if so (`check_victory`, `victory.py:326-336`): `p`
/// alone, or every living major civilization in id order. True in a game that is over.
pub(crate) fn check_victory(g: &mut Game, p: Option<PlayerId>) -> bool {
    if g.phase() != Phase::Playing {
        return true;
    }
    let order: Vec<PlayerId> = match p {
        Some(p) => vec![p],
        None => g.majors(true).map(Player::id).collect(),
    };
    for q in order {
        if let Some(won) = victory_achieved(g, q) {
            declare_winner(g, q, won, None);
            return true;
        }
    }
    false
}

/// The Domination victory, when it is on (`check_domination`, `victory.py:339-349`): won by the
/// first living major civilization holding every original capital, or by the last one standing
/// of several.
pub(crate) fn check_domination(g: &mut Game) {
    // refcheck: victories-python-named-are-the-rulesets
    let Some(d) = g.rules().derived().known.victories.domination else { return };
    if !g.victory_enabled(d) {
        return;
    }
    let alive: Vec<PlayerId> = g.majors(true).map(Player::id).collect();
    for &p in &alive {
        if milestones::milestone_done(g, p, Milestone::CaptureAllCapitals) {
            declare_winner(g, p, Won::Victory(d), None);
            return;
        }
    }
    if let &[last] = alive.as_slice()
        && g.majors(false).nth(1).is_some()
    {
        let name = g.player(last).map(|x| x.name.to_string()).unwrap_or_default();
        let text = format!("{name} is the last civilization standing — Domination Victory!");
        declare_winner(g, last, Won::Victory(d), Some(text));
    }
}

/// The game ends once its last turn is past (`check_turn_limit`, `victory.py:352-366`), if a
/// major civilization lives: the Time victory, when it is on, to the best score (the first of
/// equals), else with no winner.
pub(crate) fn check_turn_limit(g: &mut Game) {
    if g.phase() != Phase::Playing || g.turn() <= g.total_turns() {
        return;
    }
    let Some(best) = score::best_score(g) else { return };
    // refcheck: victories-python-named-are-the-rulesets
    let time = g.rules().derived().known.victories.time.filter(|&t| g.victory_enabled(t));
    if let Some(t) = time {
        let name = g.player(best).map(|x| x.name.to_string()).unwrap_or_default();
        let total = score(g, best).total;
        let text = format!("{} (score {total})", victory_text(g, Won::Victory(t), &name));
        declare_winner(g, best, Won::Victory(t), Some(text));
        return;
    }
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

// ---- Eliminations (victory.py:371-406) ---------------------------------------------------------------

/// Whether a civilization has been defeated (`is_defeated`, `victory.py:371-378`): it has no
/// city, and it had founded one, or it has no unit either. The barbarians never are.
#[must_use]
pub fn is_defeated(g: &Game, p: PlayerId) -> bool {
    let Some(pl) = g.player(p) else { return false };
    if pl.is_barbarian() || g.player_cities(p).next().is_some() {
        return false;
    }
    if pl.founded_city {
        return true;
    }
    g.player_units(p).next().is_none() && g.turn() > 0
}

/// Eliminates a civilization that has been defeated (`check_elimination`,
/// `victory.py:381-406`): its units go, its open negotiations are cancelled, its deals end, its
/// spies come home; a city-state's destroyer (`by`) answers for it to its protectors and the
/// city-states that wanted it gone, and it has no ally any more; everyone is told; and a major
/// civilization's fall may leave another the winner (`check_domination`). Whether it was.
///
/// Only stage R0 calls it directly: it would eliminate the player whose turn it is, so every
/// other site goes through [`eliminate_if_defeated`].
pub(crate) fn check_elimination(g: &mut Game, p: PlayerId, by: Option<PlayerId>) -> bool {
    if !g.player(p).is_some_and(Player::alive) || !is_defeated(g, p) {
        return false;
    }
    let (name, major, city_state) = match g.player(p) {
        Some(x) => (x.name.to_string(), x.is_major(), x.is_city_state()),
        None => return false,
    };
    let killed = g.kill_player(p);
    debug_assert!(killed.is_ok(), "a defeated player has no city: {killed:?}");
    negotiation::cancel_for(g, p, &format!("{name} has been eliminated."));
    let ends = g.state().diplo().deals.iter().any(|d| d.active && d.parties.contains(&p));
    if ends {
        for d in &mut g.edit_diplo(DiploTouch::DEALS).deals {
            if d.active && d.parties.contains(&p) {
                d.active = false;
            }
        }
    }
    espionage::remove_all_spies(g, p);
    if city_state {
        if let Some(by) = by {
            city_states::turn::on_destroyed(g, p, by);
        }
        let cleared = g.set_ally(p, None);
        debug_assert!(cleared.is_ok(), "a city-state loses its ally: {cleared:?}");
    }
    let data = EventData { player: Some(p), ..EventData::default() };
    g.emit(EngineEvent::Eliminated, &format!("{name} has been destroyed!"), None, None, data, &[]);
    if major {
        check_domination(g);
    }
    true
}

/// A civilization that has just lost a city or a unit is eliminated at once if that left it
/// defeated (`victory.check_elimination` at `conquest.py:149`, `combat.py:610`, `cities.py:2377`
/// and `city_states.py:800`), unless it is the player whose turn it is, which the round's end
/// eliminates: a game always has a living player whose turn it is (invariant TURN-1).
pub(crate) fn eliminate_if_defeated(g: &mut Game, p: PlayerId, by: Option<PlayerId>) {
    // refcheck: eliminated-on-its-own-turn-at-the-round-end
    if g.current() == p && g.phase() == Phase::Playing {
        return;
    }
    check_elimination(g, p, by);
}

// ---- The stages --------------------------------------------------------------------------------------

/// Stages S9 and E6: whether the player whose turn it is has won (`turns.py:62, 116`).
pub(crate) fn victory_stage(g: &mut Game, p: PlayerId) {
    check_victory(g, Some(p));
}

/// Stage R0: every living civilization and city-state that has been defeated is eliminated, in
/// id order (`turns.py:193-195`). A player whose turn it was and who is gone hands the turn to
/// the next living one, whose turn has not begun, so the game always has a living player whose
/// turn it is; ending the round moves the turn on from there as it would have.
pub(crate) fn eliminations_stage(g: &mut Game) {
    let players: Vec<PlayerId> = g
        .state()
        .players()
        .iter()
        .filter(|(_, p)| p.alive() && !p.is_barbarian())
        .map(|(id, _)| id)
        .collect();
    for p in players {
        check_elimination(g, p, None);
    }
    let c = *g.state().clock();
    if c.phase != Phase::Playing || g.player(c.current).is_some_and(Player::alive) {
        return;
    }
    let n = g.state().players().len();
    let next = (1..=n)
        .map(|k| (usize::from(c.current.0) + k) % n)
        .filter_map(|i| u8::try_from(i).ok().map(PlayerId))
        .find(|&q| g.player(q).is_some_and(Player::alive));
    if let Some(q) = next {
        g.set_clock(TurnClock { current: q, turn_started: false, ..c });
    }
}

/// Stage R5: whether anybody has won once the round is over (`victory.end_round`,
/// `victory.py:417`).
pub(crate) fn round_victory_stage(g: &mut Game) {
    check_victory(g, None);
}
