//! The United Nations (`victory.py:97-221`, UnCiv's `handleDiplomaticVictoryFlags`): the vote
//! for world leader a civilization's `Triggers voting for the Diplomatic Victory` schedules, who
//! may vote and how many votes win, the `un_vote` tool, and the count at the end of the round
//! the vote falls on (stage R5).
//!
//! The state has its defaults from the start (`state::world::Un`), where Python made it on first
//! read, and the tally is kept by player id, where Python kept it by civilization name. What
//! differs from Python, on purpose:
//! - candidates with as many votes are listed by player id, where Python listed them in the
//!   order they were first voted for; who wins, and whether anyone does, is the same;
//! - a city-state votes for its ally only while its ally lives (Python counted a vote for a dead
//!   civilization, which then could not win);
//! - a vote for a player the game does not have is refused with the tool's own sentence, where
//!   Python raised an error the tool did not catch (a name that is not a number) or counted from
//!   the end of the list of players (a negative id);
//! - the vote is held only while the ruleset's Diplomatic victory is on; a ruleset without one
//!   has no vote, where Python held it as if one were on.

use serde_json::{Map, Value, json};

use crate::base::ids::{PlayerId, Turn};
use crate::base::num;
use crate::base::py;
use crate::base::rng::{KeyPart, Purpose, Rng};
use crate::game::Game;
use crate::game::action::{OutcomeSpec, Rule};
use crate::game::core::has_type;
use crate::game::derive::rev::WorldTouch;
use crate::game::diplomacy::relations::opinion;
use crate::game::error::{ActionError, ErrCode};
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::players::Player;
use crate::state::world::UnResult;
use crate::unique::UniqueType;

/// How often the world leader vote is held: fifteen turns, scaled by speed
/// (`turns_between_votes`, `victory.py:107-109`).
#[must_use]
pub fn turns_between_votes(g: &Game) -> Turn {
    num::trunc_i32(15.0 * g.speed().modifier)
}

/// The next world leader vote is held [`turns_between_votes`] from now, and the ballots cast so
/// far are thrown away (`schedule_vote`, `victory.py:112-117`): what `Triggers voting for the
/// Diplomatic Victory` does when the United Nations is built.
pub(crate) fn schedule_vote(g: &mut Game) {
    let turn = g.turn().saturating_add(turns_between_votes(g));
    let un = &mut g.edit_world(WorldTouch::UN).un;
    un.next_vote = Some(turn);
    un.votes.clear();
    g.emit(
        EngineEvent::UnVote,
        &format!("The United Nations will hold a vote for world leader on turn {turn}."),
        None,
        None,
        EventData::default(),
        &[],
    );
}

/// The civilization that built the United Nations, which casts two votes (`un_owner`,
/// `victory.py:120-127`): the owner of the first city, by id, holding a building with `Triggers
/// voting for the Diplomatic Victory`, if its owner lives and is not the barbarians.
#[must_use]
pub fn un_owner(g: &Game) -> Option<PlayerId> {
    let r = g.rules();
    g.state()
        .cities()
        .iter()
        .find(|c| {
            g.player(c.owner()).is_some_and(|p| p.alive() && !p.is_barbarian())
                && c.buildings.iter().any(|b| {
                    has_type(r, &r.buildings()[b].uniques, UniqueType::OneTimeTriggerVoting)
                })
        })
        .map(crate::state::cities::City::owner)
}

/// Everyone who may vote: every living civilization and city-state, by id (`_voters`).
fn voters(g: &Game) -> impl Iterator<Item = &Player> + '_ {
    g.state().players().iter().map(|(_, p)| p).filter(|p| p.alive() && !p.is_barbarian())
}

/// How many votes a diplomatic victory needs (`votes_needed`, `victory.py:135-140`), counting
/// the United Nations' owner's second vote.
#[must_use]
pub fn votes_needed(g: &Game) -> i64 {
    let n = i64::try_from(voters(g).count()).unwrap_or(0) + i64::from(un_owner(g).is_some());
    if n > 28 {
        return num::floor_div(n * 35, 100);
    }
    #[allow(clippy::cast_precision_loss, reason = "at most 64 voters")]
    let share = 67 - num::trunc_i64(1.1 * n as f64);
    num::floor_div(n * share, 100) + 1
}

/// Whether voting is open now (`vote_open`, `victory.py:143-149`): from the turn before the vote
/// until it is counted.
#[must_use]
pub fn vote_open(g: &Game) -> bool {
    let un = &g.state().world().un;
    un.next_vote.is_some_and(|v| g.turn() >= v.saturating_sub(1))
        && un.processed_turn != Some(g.turn())
}

// ---- The tool (tools.py:887-897) ---------------------------------------------------------------------

/// `un_vote`: a vote in the world leader election for a living major civilization, by player id,
/// or an abstention (`abstain`, `none` or an empty string) (`victory.cast_vote`,
/// `victory.py:152-167`). Usable out of turn: voting opens the turn before the vote is counted.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UnVote {
    pub candidate: Value,
}

/// Who a vote is for: a candidate, or nobody.
pub type Ballot = Option<PlayerId>;

/// Why a vote is refused now, if it is (`cast_vote`'s checks, `victory.py:154-158`).
fn voting_closed(g: &Game) -> Option<String> {
    match g.state().world().un.next_vote {
        None => Some(
            "No United Nations vote is scheduled (the United Nations wonder schedules it).".into(),
        ),
        Some(v) if g.turn() < v.saturating_sub(1) => {
            Some(format!("Voting opens on turn {}.", v.saturating_sub(1)))
        }
        Some(_) => None,
    }
}

/// Reads a candidate as Python did: `abstain`, `none`, an empty string or null for nobody, else a
/// player id, which must be a living major civilization (`victory.py:159-165`).
pub fn plan_vote(g: &Game, candidate: &Value) -> Result<Ballot, ActionError> {
    if let Some(why) = voting_closed(g) {
        return Err(ActionError::rule(why));
    }
    let text = py::str_of(candidate).to_lowercase();
    if candidate.is_null() || matches!(text.as_str(), "abstain" | "none" | "") {
        return Ok(None);
    }
    let refused = || {
        ActionError::new(ErrCode::BadParam, "You can only vote for a living major civilization.")
    };
    // refcheck: un-vote-for-no-player-refused
    let id = py::int_of(candidate)
        .and_then(|n| u8::try_from(n).ok())
        .map(PlayerId)
        .ok_or_else(refused)?;
    if !g.player(id).is_some_and(|p| p.is_major() && p.alive()) {
        return Err(refused());
    }
    Ok(Some(id))
}

/// Records a vote, replacing an earlier one (`victory.py:166-167`).
pub fn cast_vote(g: &mut Game, pid: PlayerId, ballot: Ballot) -> Value {
    g.edit_world(WorldTouch::UN).un.votes.insert(pid, ballot);
    let name = ballot.and_then(|c| g.player(c)).map_or("abstain", |p| &*p.name);
    json!({"voted_for": name})
}

impl Rule for UnVote {
    type Plan = Ballot;

    fn check(&self, g: &Game, _: PlayerId) -> Result<Ballot, ActionError> {
        plan_vote(g, &self.candidate)
    }

    fn apply(self, g: &mut Game, pid: PlayerId, plan: Ballot) -> OutcomeSpec {
        OutcomeSpec::value(cast_vote(g, pid, plan))
    }
}

// ---- The count (victory.py:170-221) ------------------------------------------------------------------

/// How a civilization or city-state votes when it has not (`_ai_vote`, `victory.py:170-184`): a
/// city-state for its ally, while it lives; a civilization whose player casts its own votes
/// abstains; one whose seat votes for it (`un_vote` among its automatic decisions) for the
/// civilization it thinks best of among those it has met, one of equals drawn
/// from `Purpose::UnVote` keyed by the voter and the turn, unless it thinks badly even of that
/// one, when it may abstain (always below -80, below -40 by a draw).
fn ai_vote(g: &Game, p: &Player) -> Ballot {
    if p.is_city_state() {
        // refcheck: un-city-state-votes-for-a-living-ally
        return p
            .city_state
            .as_deref()
            .and_then(|d| d.ally())
            .filter(|&a| g.player(a).is_some_and(|x| x.alive() && x.is_major()));
    }
    if p.is_major() && !p.seat().auto().un_vote {
        return None;
    }
    let me = p.id();
    let known: Vec<(PlayerId, f64)> = g
        .majors(true)
        .map(Player::id)
        .filter(|&q| q != me && g.has_met(me, q))
        .map(|q| (q, opinion(g, me, q)))
        .collect();
    if known.is_empty() {
        return None;
    }
    let best = known.iter().map(|&(_, o)| o).fold(f64::NEG_INFINITY, f64::max);
    let mut rng = Rng::keyed(g.state().seed(), Purpose::UnVote, &[me.key(), g.turn().key()]);
    let draw = |rng: &mut Rng| f64::from(u32::try_from(rng.below(40)).unwrap_or(0));
    if best < -80.0 || (best < -40.0 && best + draw(&mut rng) < -40.0) {
        return None;
    }
    let ties: Vec<PlayerId> =
        known.iter().filter(|&&(_, o)| o.total_cmp(&best).is_eq()).map(|&(q, _)| q).collect();
    rng.pick(&ties).copied()
}

/// Counts the vote (`hold_vote`, `victory.py:187-221`): who has not voted votes as its seat
/// decides (a civilization whose player casts its own votes and cast none abstains), the United
/// Nations'
/// owner's vote counts twice, and the one candidate with the most votes, if they are enough,
/// is elected world leader, which the Diplomatic victory's `Win diplomatic vote` reads. The next
/// vote is set, and the ballots are thrown away.
pub(crate) fn hold_vote(g: &mut Game) {
    let mut votes = g.state().world().un.votes.clone();
    for p in voters(g) {
        votes.entry(p.id()).or_insert_with(|| ai_vote(g, p));
    }
    let owner = un_owner(g);
    let mut tally: Vec<(PlayerId, u16)> = Vec::new();
    for (&voter, &ballot) in &votes {
        let Some(c) = ballot else { continue };
        let n = if Some(voter) == owner { 2 } else { 1 };
        match tally.iter_mut().find(|(x, _)| *x == c) {
            Some((_, v)) => *v = v.saturating_add(n),
            None => tally.push((c, n)),
        }
    }
    // Most votes first, equals by player id.
    tally.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let needed = votes_needed(g);
    let mut text = "No valid votes were cast.".to_owned();
    let mut winner = None;
    if let Some(&(first, top)) = tally.first() {
        let leaders = tally.iter().filter(|&&(_, v)| v == top).count();
        if i64::from(top) < needed {
            text = format!("No world leader was elected (minimum {needed} votes; best {top}).");
        } else if leaders > 1 {
            text = "No world leader was elected (tie for first place).".into();
        } else {
            winner = Some(first);
            let name = g.player(first).map_or("", |p| &*p.name);
            text = format!("{name} has been elected world leader with {top} votes!");
        }
    }
    let turn = g.turn();
    let results =
        UnResult { turn, tally, votes_needed: u16::try_from(needed).unwrap_or(u16::MAX), winner };
    let next = turn.saturating_add(turns_between_votes(g));
    let un = &mut g.edit_world(WorldTouch::UN).un;
    if let Some(w) = winner {
        un.won.insert(w);
    }
    un.results = Some(results.clone());
    un.processed_turn = Some(turn);
    un.votes.clear();
    un.next_vote = Some(next);
    let data = EventData { results: Some(Box::new(results)), ..EventData::default() };
    g.emit(EngineEvent::UnVote, &format!("United Nations vote: {text}"), None, None, data, &[]);
}

/// Stage R5, the vote (`victory.end_round`, `victory.py:414-416`): counted on the turn it was
/// set for, or the first one after, while the Diplomatic victory is on.
pub(crate) fn vote_stage(g: &mut Game) {
    let due = g.state().world().un.next_vote.is_some_and(|v| g.turn() >= v);
    let on = g.rules().derived().known.victories.diplomatic.is_some_and(|v| g.victory_enabled(v));
    if due && on {
        hold_vote(g);
    }
}

/// The United Nations as `inspect` gives it: the turn of the next vote, the ballots cast (by
/// voter id; null for an abstention), the last result, who has ever won, the turn the last vote
/// was counted, whether voting is open, how many votes win and who owns the United Nations.
#[must_use]
pub fn un_json(g: &Game) -> Value {
    let un = &g.state().world().un;
    let votes: Map<String, Value> =
        un.votes.iter().map(|(v, c)| (v.0.to_string(), json!(c.map(|c| c.0)))).collect();
    let results = un.results.as_ref().map(|r| {
        let tally: Vec<Value> = r.tally.iter().map(|&(p, n)| json!([p.0, n])).collect();
        json!({
            "turn": r.turn,
            "tally": tally,
            "votes_needed": r.votes_needed,
            "winner": r.winner.map(|p| p.0),
        })
    });
    let won: Vec<u8> = un.won.iter().map(|p| p.0).collect();
    json!({
        "next_vote": un.next_vote,
        "votes": votes,
        "results": results,
        "won": won,
        "processed_turn": un.processed_turn,
        "open": vote_open(g),
        "votes_needed": votes_needed(g),
        "owner": un_owner(g).map(|p| p.0),
    })
}
