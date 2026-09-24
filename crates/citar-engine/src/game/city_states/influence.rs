//! Influence and allies (`city_states.py:65-153`).
//!
//! A major's influence with a city-state is stored per pair; at war it reads as the floor, so
//! influence built before a war does not quietly last through it. The ally is the met major with
//! the most influence, if that is at least [`ALLY_INFLUENCE`]: an alliance is a contest, not a
//! shared benefit. Every write of influence ends in [`update_ally`], and goes through
//! `Game::set_influence`, which moves a major's unique index when its friend level flips.
//!
//! A write the state refuses (a player that is no city-state, or no major) is an engine bug; the
//! rules return it rather than stop quietly halfway.

use crate::base::ids::PlayerId;
use crate::base::sets::PlayerSet;
use crate::game::Game;
use crate::game::derive::rev::PlayerTouch;
use crate::game::diplomacy::relations::{WarReason, set_war};
use crate::state::StateError;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::unique::{Ctx, UniqueData, UniqueType, uq};

/// The lowest influence a major can have with a city-state (`city_states.py:14`).
pub const MIN_INFLUENCE: f64 = -60.0;

/// The influence at which a city-state may take a major as its ally (`city_states.py:15`).
pub const ALLY_INFLUENCE: f64 = 60.0;

/// A major's influence with a city-state: the stored value, or the floor while they are at war
/// (`city_states.influence`, `city_states.py:65-74`).
#[must_use]
pub fn influence(g: &Game, cs: PlayerId, major: PlayerId) -> f64 {
    if g.at_war(cs, major) { MIN_INFLUENCE } else { raw_influence(g, cs, major) }
}

/// The stored influence, war or not (`city_states.py:77-79`).
#[must_use]
pub fn raw_influence(g: &Game, cs: PlayerId, major: PlayerId) -> f64 {
    g.player(cs).and_then(|p| p.city_state.as_deref()).map_or(0.0, |d| d.influence_of(major))
}

/// Sets a major's influence, never below the floor, and settles who the city-state's ally is
/// (`city_states.set_influence`, `city_states.py:82-87`).
///
/// # Errors
/// A write the state refused: `cs` is no city-state or `major` no major.
pub fn set_influence(
    g: &mut Game,
    cs: PlayerId,
    major: PlayerId,
    amount: f64,
) -> Result<(), StateError> {
    g.set_influence(cs, major, amount.max(MIN_INFLUENCE))?;
    update_ally(g, cs)
}

/// Changes a major's influence by an amount (`city_states.add_influence`,
/// `city_states.py:90-92`).
///
/// # Errors
/// A write the state refused, as [`set_influence`]'s.
pub fn add_influence(
    g: &mut Game,
    cs: PlayerId,
    major: PlayerId,
    amount: f64,
) -> Result<(), StateError> {
    set_influence(g, cs, major, raw_influence(g, cs, major) + amount)
}

/// Recomputes a city-state's ally and announces a change (`city_states.update_ally`,
/// `city_states.py:118-152`): the met major with the most influence, the first by id among
/// equals, if that is at least [`ALLY_INFLUENCE`]. A new ally's enemies become the city-state's.
///
/// # Errors
/// A write the state refused: `cs` is no city-state, or a war it joins is refused.
pub fn update_ally(g: &mut Game, cs: PlayerId) -> Result<(), StateError> {
    let old = g
        .player(cs)
        .and_then(|p| p.city_state.as_deref())
        .map(|d| d.ally())
        .ok_or(StateError::NotACityState(cs))?;
    let mut best: Option<(PlayerId, f64)> = None;
    for q in g.majors(true).map(|p| p.id()).collect::<Vec<_>>() {
        if !g.has_met(cs, q) {
            continue;
        }
        let v = influence(g, cs, q);
        if best.is_none_or(|(_, b)| v > b) {
            best = Some((q, v));
        }
    }
    let new = best.filter(|&(_, v)| v >= ALLY_INFLUENCE).map(|(q, _)| q);
    if new == old {
        return Ok(());
    }
    g.set_ally(cs, new)?;
    let name = |g: &Game, p: PlayerId| g.player(p).map(|x| x.name.to_string()).unwrap_or_default();
    let data = EventData { player: Some(cs), ..EventData::default() };
    if let Some(ally) = new {
        let text = format!("{} is now allied with {}.", name(g, ally), name(g, cs));
        g.emit(EngineEvent::CsAlly, &text, Some(PlayerSet::single(ally)), None, data.clone(), &[]);
        let cooldown = {
            let view = g.view();
            uq::civ(&view, ally, UniqueType::CityStateCanBeBoughtForGold, &Ctx::civ(ally))
                .filter_map(|h| match h.data() {
                    UniqueData::CityStateCanBeBoughtForGold(x) => Some(x.turns),
                    _ => None,
                })
                .last()
        };
        if let Some(turns) = cooldown
            && let Some(d) =
                g.player_mut(cs, PlayerTouch::CITY_STATE).and_then(|p| p.city_state.as_deref_mut())
            && let Some(pair) = d.pairs.get_mut(ally)
        {
            pair.marriage_cooldown = i16::try_from(turns).unwrap_or(i16::MAX);
        }
        let others: Vec<PlayerId> =
            g.st.players()
                .iter()
                .filter(|(e, p)| !p.is_barbarian() && *e != cs && *e != ally)
                .map(|(e, _)| e)
                .collect();
        for e in others {
            let alive = g.player(e).is_some_and(crate::state::players::Player::alive);
            if alive && g.at_war(e, ally) && !g.at_war(cs, e) {
                g.make_contact(cs, e);
                set_war(g, cs, e, WarReason::CityStateAlliance)?;
            }
        }
    }
    if let Some(lost) = old
        && g.player(cs).is_some_and(crate::state::players::Player::alive)
    {
        let text = format!("{} lost its alliance with {}.", name(g, lost), name(g, cs));
        g.emit(EngineEvent::CsAllyLost, &text, Some(PlayerSet::single(lost)), None, data, &[]);
    }
    Ok(())
}
