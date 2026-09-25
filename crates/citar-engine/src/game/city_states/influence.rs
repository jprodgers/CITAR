//! Influence and allies, and the relationship they make (`city_states.py:65-247`).
//!
//! A major's influence with a city-state is stored per pair; at war it reads as the floor, so
//! influence built before a war does not quietly last through it. The ally is the met major with
//! the most influence, if that is at least [`ALLY_INFLUENCE`]: an alliance is a contest, not a
//! shared benefit. Every write of influence ends in [`update_ally`], and goes through
//! `Game::set_influence`, which moves a major's unique index when its friend level flips.
//!
//! A write the state refuses (a player that is no city-state, or no major) is an engine bug; the
//! rules return it rather than stop quietly halfway.
//!
//! Package 1c-06 adds the named relationship ([`relationship`], and [`friendship`] where only
//! friends and allies matter), the resting point influence
//! drifts toward ([`resting_point`]) and how fast it drifts ([`degrade`], [`recovery`]), and
//! whether a civilization has attacked city-states ([`is_aggressor`], [`is_warmonger`]).

use crate::base::ids::PlayerId;
use crate::base::sets::PlayerSet;
use crate::game::derive::rev::PlayerTouch;
use crate::game::diplomacy::relations::{WarReason, civ_has, set_war};
use crate::game::{Game, religion};
use crate::rules::defs::CityStatePersonality;
use crate::state::StateError;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::players::{CityStateData, CsPair};
use crate::unique::{Ctx, UniqueData, UniqueType, uq};

/// The lowest influence a major can have with a city-state (`city_states.py:14`).
pub const MIN_INFLUENCE: f64 = -60.0;

/// The influence at which a city-state may take a major as its ally (`city_states.py:15`).
pub const ALLY_INFLUENCE: f64 = 60.0;

/// The influence at which a city-state counts a major a friend (`city_states.py:16`).
pub const FRIEND_INFLUENCE: f64 = 30.0;

/// A city-state's data, if `cs` is one.
#[must_use]
pub fn data(g: &Game, cs: PlayerId) -> Option<&CityStateData> {
    g.player(cs).and_then(|p| p.city_state.as_deref())
}

/// A city-state's standing with a major (`_pair`, `city_states.py:25-31`): every pair is kept.
#[must_use]
pub fn pair(g: &Game, cs: PlayerId, major: PlayerId) -> CsPair {
    data(g, cs).map(|d| d.pair(major)).unwrap_or_default()
}

/// Edits a city-state's standing with a major. Nothing a cache reads.
pub(crate) fn pair_mut(g: &mut Game, cs: PlayerId, major: PlayerId) -> Option<&mut CsPair> {
    g.player_mut(cs, PlayerTouch::CITY_STATE)?.city_state.as_deref_mut()?.pairs.get_mut(major)
}

/// A city-state's data to edit. Nothing a cache reads but its influence and ally, which change
/// through [`set_influence`] and [`update_ally`].
pub(crate) fn data_mut(g: &mut Game, cs: PlayerId) -> Option<&mut CityStateData> {
    g.player_mut(cs, PlayerTouch::CITY_STATE)?.city_state.as_deref_mut()
}

/// The named relationship between a city-state and a major (`relationship`,
/// `city_states.py:96-110`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Relationship {
    /// Influence at -30 or below.
    Unforgivable,
    /// Influence below 0.
    Enemy,
    /// Its ally.
    Ally,
    /// Influence at the friend level.
    Friend,
    /// It would pay the major tribute.
    Afraid,
    Neutral,
}

impl Relationship {
    /// Python's name: `Friend`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unforgivable => "Unforgivable",
            Self::Enemy => "Enemy",
            Self::Ally => "Ally",
            Self::Friend => "Friend",
            Self::Afraid => "Afraid",
            Self::Neutral => "Neutral",
        }
    }

    /// A friend or an ally.
    #[must_use]
    pub const fn friendly(self) -> bool {
        matches!(self, Self::Friend | Self::Ally)
    }
}

/// The relationship at the current influence (`relationship`, `city_states.py:96-110`): at -30 or
/// below unforgivable, below 0 an enemy, its ally at the ally level, a friend at the friend
/// level, afraid when it would pay tribute, else neutral.
#[must_use]
pub fn relationship(g: &Game, cs: PlayerId, major: PlayerId) -> Relationship {
    let inf = influence(g, cs, major);
    if inf <= -30.0 {
        return Relationship::Unforgivable;
    }
    if inf < 0.0 {
        return Relationship::Enemy;
    }
    if let Some(level) = friendship(g, cs, major) {
        return level;
    }
    if super::actions::tribute_willingness(g, cs, major, false) > 0 {
        return Relationship::Afraid;
    }
    Relationship::Neutral
}

/// A city-state's ally or friend, or neither: the part of [`relationship`] that reads influence
/// alone. The end of its turn, its unit gifts and its border tension ask only this, and skip the
/// tribute test (every major's military, the capital's strength) that tells the afraid from the
/// neutral.
#[must_use]
pub fn friendship(g: &Game, cs: PlayerId, major: PlayerId) -> Option<Relationship> {
    let inf = influence(g, cs, major);
    if inf >= ALLY_INFLUENCE && data(g, cs).and_then(CityStateData::ally) == Some(major) {
        return Some(Relationship::Ally);
    }
    (inf >= FRIEND_INFLUENCE).then_some(Relationship::Friend)
}

/// Whether a major has attacked a city-state at all (`is_aggressor`, `city_states.py:240-242`).
#[must_use]
pub fn is_aggressor(g: &Game, major: PlayerId) -> bool {
    g.player(major).and_then(|p| p.major.as_deref()).is_some_and(|m| m.cs_attacks >= 1)
}

/// Whether it has done so often enough for every city-state to hold it against it
/// (`is_warmonger`, `city_states.py:245-247`).
#[must_use]
pub fn is_warmonger(g: &Game, major: PlayerId) -> bool {
    g.player(major).and_then(|p| p.major.as_deref()).is_some_and(|m| m.cs_attacks >= 3)
}

/// Whether a city-state's capital follows the religion `major` founded.
fn follows_major(g: &Game, cs: PlayerId, major: PlayerId) -> bool {
    let founded = g.player(major).and_then(|p| p.religion.founded);
    let cap = g.player(cs).and_then(|p| p.capital).filter(|&c| g.city(c).is_some());
    founded.is_some() && cap.is_some_and(|c| religion::majority_religion(g, c) == founded)
}

/// The influence a relationship drifts toward (`resting_point`, `city_states.py:187-207`): 0,
/// raised by the major's `Resting point for Influence with City-States is increased by [n]`,
/// and by `... following this religion [n]` where the capital follows its religion; 10 more for
/// a protector, 20 less where the city-state has grown wary of it.
#[must_use]
pub fn resting_point(g: &Game, cs: PlayerId, major: PlayerId) -> f64 {
    let v = g.view();
    let ctx = Ctx::civ(major);
    let mut rp =
        f64::from(uq::sum_i32(uq::civ(&v, major, UniqueType::CityStateRestingPoint, &ctx), |d| {
            match d {
                UniqueData::CityStateRestingPoint(x) => Some(x.influence),
                _ => None,
            }
        }));
    if follows_major(g, cs, major) {
        rp += f64::from(uq::sum_i32(
            uq::civ(&v, major, UniqueType::RestingPointOfCityStatesFollowingReligionChange, &ctx),
            |d| match d {
                UniqueData::RestingPointOfCityStatesFollowingReligionChange(x) => Some(x.influence),
                _ => None,
            },
        ));
    }
    if data(g, cs).is_some_and(|d| d.protectors.contains(major)) {
        rp += 10.0;
    }
    if pair(g, cs, major).wary {
        rp -= 20.0;
    }
    rp
}

/// The influence lost this turn above the resting point (`_degrade`, `city_states.py:210-224`):
/// 1, 1.5 for a hostile city-state, 2 for an aggressor; changed by the major's `[n]% City-State
/// Influence degradation`, 25% less where the capital follows its religion, and more for every
/// other major's `City-State Influence degrades [n]% faster ...`.
#[must_use]
pub fn degrade(g: &Game, cs: PlayerId, major: PlayerId) -> f64 {
    if influence(g, cs, major) <= resting_point(g, cs, major) {
        return 0.0;
    }
    let hostile = data(g, cs).and_then(|d| d.personality) == Some(CityStatePersonality::Hostile);
    let dec = if hostile {
        1.5
    } else if is_aggressor(g, major) {
        2.0
    } else {
        1.0
    };
    let v = g.view();
    let mut pct = f64::from(uq::sum_i32(
        uq::civ(&v, major, UniqueType::CityStateInfluenceDegradation, &Ctx::civ(major)),
        |d| match d {
            UniqueData::CityStateInfluenceDegradation(x) => Some(x.percent),
            _ => None,
        },
    ));
    if follows_major(g, cs, major) {
        pct -= 25.0;
    }
    for q in g.majors(true).map(crate::state::players::Player::id).filter(|&q| q != major) {
        pct += f64::from(uq::sum_i32(
            uq::civ(&v, q, UniqueType::OtherCivsCityStateRelationsDegradeFaster, &Ctx::civ(q)),
            |d| match d {
                UniqueData::OtherCivsCityStateRelationsDegradeFaster(x) => Some(x.percent),
                _ => None,
            },
        ));
    }
    f64::max(0.0, dec) * (1.0 + pct.max(-100.0) / 100.0)
}

/// The influence regained this turn below the resting point (`_recovery`,
/// `city_states.py:227-237`): 1, twice that with `City-State Influence recovers at twice the
/// normal rate`, and half again where the capital follows the major's religion.
#[must_use]
pub fn recovery(g: &Game, cs: PlayerId, major: PlayerId) -> f64 {
    if influence(g, cs, major) >= resting_point(g, cs, major) {
        return 0.0;
    }
    let mut pct = if civ_has(g, major, UniqueType::CityStateInfluenceRecoversTwiceNormalRate) {
        100.0
    } else {
        0.0
    };
    if follows_major(g, cs, major) {
        pct += 50.0;
    }
    1.0 + f64::max(0.0, pct) / 100.0
}

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
