//! War and peace, pacts, embassies and opinions between two players (`diplomacy.py:60-296`).
//!
//! Package 1b-02 ports what a scenario's `set_relation` needs: the reads (`diplomacy.py:84-125`),
//! opinions (`diplomacy.py:76-96`), and war and peace with the consequences that belong to the
//! relation itself (`set_war`, `diplomacy.py:171-237`; `make_peace`, `diplomacy.py:240-264`):
//! the treaty terms, lapsed deals, open borders and pacts, betrayal and warmonger opinions,
//! defensive pacts and city-state allies drawn in, and the peace treaty. The consequences that
//! belong to other systems are marked where Python had them: open negotiations are cancelled
//! (package 1c-05), a city-state attacked or protected reacts (1c-06), units in a new friend's
//! land go home (1c-02), and the war and peace triggers fire (1b-08).
//!
//! A write the state refuses (two ids that are no pair, a city-state that is none) is an engine
//! bug. The rules return it rather than stop quietly halfway, so a scenario operation reports it
//! and `Game::apply_ops` puts the game back.
//!
//! One fix: a scenario's opinion is the holder's own opinion of the other, within the ±100 every
//! reason keeps. Python kept it under a key (`"a>b"`) that `opinion()` never read, so a scenario
//! could not move what a bot thought (refcheck: scenario-opinion-counts).

use crate::base::ids::PlayerId;
use crate::game::city_states::influence::{add_influence, set_influence};
use crate::game::derive::rev::DiploTouch;
use crate::game::{Game, Porting, pending, triggers};
use crate::state::StateError;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::diplo::{OPINION_LIMIT, OpinionKey, PairError, side};
use crate::state::players::Player;
use crate::unique::trigger::{TriggerEvent, TriggerSite};

/// Why a war began, which decides what it drags in (`set_war`'s `reason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WarReason {
    /// Declared, by a player or a bot: the aggressor pays the diplomatic cost.
    Direct,
    /// A defensive pact called the partner in.
    DefensivePact,
    /// A city-state joined its ally's war.
    CityStateAlliance,
    /// A scenario set it.
    Scenario,
}

/// Whether a defensive pact between `a` and `b` is in force: not expired, and not at war with
/// each other (`diplomacy.py:119-122`).
#[must_use]
pub fn has_pact(g: &Game, a: PlayerId, b: PlayerId) -> bool {
    g.relation(a, b).is_some_and(|r| r.pact_until >= g.turn() && !r.war)
}

/// Whether a declaration of friendship between `a` and `b` is in force (`diplomacy.py:113-116`).
#[must_use]
pub fn is_friends(g: &Game, a: PlayerId, b: PlayerId) -> bool {
    g.relation(a, b).is_some_and(|r| r.friendship_until >= g.turn())
}

/// Whether `holder` has an embassy with `host` (`diplomacy.py:97-100`).
#[must_use]
pub fn has_embassy(g: &Game, holder: PlayerId, host: PlayerId) -> bool {
    g.relation(holder, host).is_some_and(|r| r.embassy[side(holder, host)])
}

/// What `holder` thinks of `about`, every reason summed (`diplomacy.py:85-94`).
#[must_use]
pub fn opinion(g: &Game, holder: PlayerId, about: PlayerId) -> f64 {
    g.state().diplo().opinions.total(holder, about)
}

/// Adds to what `holder` thinks of `about` for one reason, within ±100 (`diplomacy.py:76-82`).
pub fn add_opinion(g: &mut Game, holder: PlayerId, about: PlayerId, key: OpinionKey, amount: f64) {
    g.edit_diplo(DiploTouch::OPINIONS).opinions.add(holder, about, key, amount);
}

/// Sets what `holder` thinks of `about` for one reason outright, as a scenario does, within the
/// ±100 [`add_opinion`] keeps each reason to: now that the scenario's reason counts, a value
/// beyond it would make an opinion no play can reach.
pub fn set_opinion(g: &mut Game, holder: PlayerId, about: PlayerId, key: OpinionKey, value: f64) {
    // refcheck: scenario-opinion-counts
    let value = value.clamp(-OPINION_LIMIT, OPINION_LIMIT);
    g.edit_diplo(DiploTouch::OPINIONS).opinions.set(holder, about, key, value);
}

/// Players by a test on them, collected so the game can change while they are visited.
fn players_where(g: &Game, f: impl Fn(&Player) -> bool) -> Vec<PlayerId> {
    g.state().players().iter().filter(|(_, p)| f(p)).map(|(id, _)| id).collect()
}

/// `a` goes to war with `b`, with what follows (`set_war`, `diplomacy.py:171-237`; UnCiv's
/// `DeclareWar.declareWar`). Nothing happens if they already are at war.
///
/// # Errors
/// A write the state refused: a bug, such as two ids that are not two players.
pub fn set_war(
    g: &mut Game,
    a: PlayerId,
    b: PlayerId,
    reason: WarReason,
) -> Result<(), StateError> {
    let rel = *g.relation(a, b).ok_or(PairError::NotAPair(a, b))?;
    if rel.war {
        return Ok(());
    }
    let turn = g.turn();
    let a_major = g.player(a).is_some_and(Player::is_major);
    if g.is_city_state(b) && reason == WarReason::Direct {
        // Attacking a city-state drops the attacker's influence to the floor, and its ally's far
        // below it (diplomacy.py:177-181).
        set_influence(g, b, a, -60.0)?;
        // city_states.on_attacked: the attacked city-state's friends and quests react.
        pending(Porting::Pending("1c-06"));
        let ally = g.player(b).and_then(|p| p.city_state.as_deref()).and_then(|d| d.ally());
        if ally == Some(a) {
            set_influence(g, b, a, -120.0)?;
        }
    }
    let betrayed_friend = rel.friendship_until >= turn;
    let betrayed_pact = rel.pact_until >= turn;
    g.update_relation(a, b, |r| {
        r.war = true;
        r.since = turn;
        r.war_declared_by = Some(a);
        r.open_borders_until = [0, 0];
        r.friendship_until = 0;
        r.pact_until = 0;
    })?;
    for d in &mut g.edit_diplo(DiploTouch::DEALS).deals {
        let between =
            (d.parties[0] == a && d.parties[1] == b) || (d.parties[0] == b && d.parties[1] == a);
        if d.active && between {
            d.active = false;
        }
    }
    if betrayed_friend || betrayed_pact {
        for q in players_where(g, |p| p.is_major() && p.alive() && p.id() != a) {
            if !g.has_met(q, a) {
                continue;
            }
            let wronged = q == b;
            let mut amount = 0.0;
            if betrayed_friend {
                amount += if wronged { -40.0 } else { -20.0 };
            }
            if betrayed_pact {
                amount += if wronged { -20.0 } else { -10.0 };
            }
            add_opinion(g, q, a, OpinionKey::Betrayal, amount);
        }
    }
    if reason == WarReason::Direct && a_major {
        // The aggressor's other defensive pacts lapse, and everyone who knows it thinks less of
        // it.
        let others = players_where(g, |p| p.is_major() && p.alive() && p.id() != a && p.id() != b);
        for &q in &others {
            if g.relation(a, q).is_some_and(|r| r.pact_until >= turn) {
                g.update_relation(a, q, |r| r.pact_until = 0)?;
            }
        }
        for &q in &others {
            if g.has_met(q, a) {
                add_opinion(g, q, a, OpinionKey::Warmonger, -5.0);
            }
        }
    }
    // Open negotiations between the two are cancelled (diplomacy.close_negotiation).
    pending(Porting::Pending("1c-05"));
    for (side_p, other) in [(a, b), (b, a)] {
        if !g.player(side_p).is_some_and(Player::is_major) {
            continue;
        }
        if side_p == b && reason != WarReason::DefensivePact && a_major {
            let partners =
                players_where(g, |p| p.is_major() && p.alive() && p.id() != a && p.id() != b);
            for q in partners {
                if has_pact(g, b, q) && !g.at_war(q, a) {
                    g.make_contact(a, q);
                    set_war(g, a, q, WarReason::DefensivePact)?;
                    let (qn, an) = (name(g, q), name(g, a));
                    let data =
                        EventData { attacker: Some(a), defender: Some(q), ..EventData::default() };
                    let text = format!("{qn} joins the war against {an} (defensive pact).");
                    g.emit(EngineEvent::WarDeclared, &text, None, None, data, &[]);
                }
            }
        }
        for cs in players_where(g, |p| p.is_city_state() && p.alive()) {
            let ally = g.player(cs).and_then(|p| p.city_state.as_deref()).and_then(|d| d.ally());
            if ally == Some(side_p) && !g.at_war(cs, other) && cs != other {
                g.make_contact(cs, other);
                set_war(g, cs, other, WarReason::CityStateAlliance)?;
            }
        }
    }
    // A protector that attacks its city-state withdraws its protection
    // (city_states.withdraw_protection).
    pending(Porting::Pending("1c-06"));
    // `upon declaring war`, `upon being declared war on` and `upon entering a war`
    // (diplomacy.py:234-237).
    let fire = |g: &mut Game, p: PlayerId, e: TriggerEvent| {
        triggers::fire(g, &TriggerSite::civ(p), &e, true, None);
    };
    fire(g, a, TriggerEvent::DeclaringWar { on: b });
    fire(g, b, TriggerEvent::BeingDeclaredWarUpon { by: a });
    fire(g, a, TriggerEvent::EnteringWar { with: b });
    fire(g, b, TriggerEvent::EnteringWar { with: a });
    Ok(())
}

/// `a` and `b` make peace, and a treaty keeps it for the speed's peace deal duration
/// (`make_peace`, `diplomacy.py:240-264`). City-states allied to either side end their war
/// with the other side too; the others at war with that side hold the peace against the maker.
///
/// # Errors
/// A write the state refused: a bug, such as two ids that are not two players.
pub fn make_peace(g: &mut Game, a: PlayerId, b: PlayerId) -> Result<(), StateError> {
    let turn = g.turn();
    let until = turn + g.speed().peace_deal_duration;
    let peace = |r: &mut crate::state::diplo::Relation| {
        r.war = false;
        r.since = turn;
        r.treaty_until = until;
    };
    g.update_relation(a, b, peace)?;
    for (side_p, other) in [(a, b), (b, a)] {
        // Units standing in the other side's land go to their nearest own tile
        // (movement.teleport_to_closest).
        pending(Porting::Pending("1c-02"));
        let side_major = g.player(side_p).is_some_and(Player::is_major);
        for cs in players_where(g, |p| p.is_city_state() && p.alive()) {
            let ally = g.player(cs).and_then(|p| p.city_state.as_deref()).and_then(|d| d.ally());
            if !g.at_war(cs, other) {
                continue;
            }
            if ally == Some(side_p) {
                g.update_relation(cs, other, |r| {
                    r.war = false;
                    r.treaty_until = until;
                })?;
            } else if side_major {
                add_influence(g, cs, side_p, -10.0)?;
            }
        }
    }
    let text =
        format!("{} and {} signed a peace treaty (until turn {until}).", name(g, a), name(g, b));
    let data = EventData { a: Some(a), b: Some(b), ..EventData::default() };
    g.emit(EngineEvent::Peace, &text, None, None, data, &[]);
    // `upon signing a peace treaty` (diplomacy.py:263-264).
    for (p, with) in [(a, b), (b, a)] {
        triggers::fire(g, &TriggerSite::civ(p), &TriggerEvent::SigningPeace { with }, true, None);
    }
    Ok(())
}

/// A player's name, for messages.
fn name(g: &Game, p: PlayerId) -> String {
    g.player(p).map(|x| x.name.to_string()).unwrap_or_default()
}
