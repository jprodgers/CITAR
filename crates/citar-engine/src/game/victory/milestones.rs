//! What winning takes: the spaceship (`victory.py:58-91`), the milestones of each victory and a
//! civilization's progress toward them (`victory.py:227-302`).
//!
//! A victory is won when every one of its milestones is done, in the ruleset's order of
//! victories; a civilization holding `Triggers victory` wins the neutral victory, which has no
//! victory of the ruleset ([`Won::Neutral`]).

use serde_json::{Map, Value, json};

use super::Won;
use crate::base::ids::{BaseUnitId, BuildingId, PlayerId, UnitId, VictoryId};
use crate::game::core::has_type;
use crate::game::derive::rev::PlayerTouch;
use crate::game::diplomacy::relations::civ_has;
use crate::game::{Game, units};
use crate::rules::defs::PolicyKind;
use crate::rules::gen_tables::Milestone;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::players::Player;
use crate::unique::UniqueType;

// ---- The spaceship ---------------------------------------------------------------------------------

/// How far along a civilization's spaceship is (`spaceship_status`, `victory.py:63-71`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceshipStatus {
    /// One of its cities has a building that `Enables construction of Spaceship parts`.
    pub apollo_program: bool,
    /// Each part the Scientific victory needs, in the order the ruleset first names it: how
    /// many were added (at most as many as needed) and how many are needed.
    pub parts: Vec<(BaseUnitId, u16, u16)>,
    /// Every part is added.
    pub complete: bool,
}

impl SpaceshipStatus {
    /// Python's dict, the parts by name.
    #[must_use]
    pub fn to_json(&self, g: &Game) -> Value {
        let r = g.rules();
        let parts: Map<String, Value> = self
            .parts
            .iter()
            .map(|&(u, added, needed)| {
                (r.base_units()[u].name.to_string(), json!({"added": added, "needed": needed}))
            })
            .collect();
        json!({"apollo_program": self.apollo_program, "parts": parts, "complete": self.complete})
    }
}

/// The spaceship parts the Scientific victory needs, with how many of each, in the order the
/// ruleset first names each (`required_parts`, `victory.py:58-60`); none without a Scientific
/// victory.
#[must_use]
pub fn required_parts(g: &Game) -> Vec<(BaseUnitId, u16)> {
    let r = g.rules();
    let mut out: Vec<(BaseUnitId, u16)> = Vec::new();
    let Some(v) = r.derived().known.victories.scientific else { return out };
    for &u in r.victories()[v].required_spaceship_parts.iter() {
        match out.iter_mut().find(|(x, _)| *x == u) {
            Some((_, n)) => *n = n.saturating_add(1),
            None => out.push((u, 1)),
        }
    }
    out
}

/// How far along a civilization's spaceship is (`spaceship_status`, `victory.py:63-71`).
#[must_use]
pub fn spaceship_status(g: &Game, p: PlayerId) -> SpaceshipStatus {
    let r = g.rules();
    let have = g.player(p).and_then(|x| x.major.as_deref()).map(|m| &m.spaceship);
    let parts: Vec<(BaseUnitId, u16, u16)> = required_parts(g)
        .into_iter()
        .map(|(u, needed)| {
            let added = have.and_then(|h| h.get(&u)).copied().unwrap_or(0);
            (u, added.min(needed), needed)
        })
        .collect();
    let apollo = g.player_cities(p).any(|c| {
        c.buildings.iter().any(|b| {
            has_type(r, &r.buildings()[b].uniques, UniqueType::EnablesConstructionOfSpaceshipParts)
        })
    });
    let complete = parts.iter().all(|&(_, added, needed)| added >= needed);
    SpaceshipStatus { apollo_program: apollo, parts, complete }
}

/// A spaceship part in its owner's capital is added to the spaceship
/// (`add_to_spaceship`, `victory.py:74-91`), which `actions::plan_action` has allowed: the part
/// is counted and leaves the game, everyone is told, and its owner may have won. What the tool
/// reports: the part and the spaceship as it stands.
pub(crate) fn add_to_spaceship(g: &mut Game, u: UnitId) -> Value {
    let Some((p, base)) = g.unit(u).map(|x| (x.owner(), x.base)) else { return Value::Null };
    if let Some(m) = g.player_mut(p, PlayerTouch::OTHER).and_then(|x| x.major.as_deref_mut()) {
        let n = m.spaceship.entry(base).or_insert(0);
        *n = n.saturating_add(1);
    }
    units::remove_unit(g, u);
    let (name, part) = (
        g.player(p).map(|x| x.name.to_string()).unwrap_or_default(),
        g.rules().base_units()[base].name.to_string(),
    );
    let data = EventData { player: Some(p), ..EventData::default() };
    g.emit(
        EngineEvent::Spaceship,
        &format!("{name} added a {part} to its spaceship."),
        None,
        None,
        data,
        &[],
    );
    super::check_victory(g, Some(p));
    json!({"added": part, "spaceship": spaceship_status(g, p).to_json(g)})
}

// ---- Milestones ------------------------------------------------------------------------------------

/// How many policy branches a civilization has completed: those whose finisher it has adopted
/// (`policies.completed_branches`, `policies.py:104-107`).
fn completed_branches(g: &Game, p: PlayerId) -> usize {
    let policies = g.rules().policies();
    g.player(p).map_or(0, |x| {
        x.policy
            .adopted
            .iter()
            .filter(|&q| matches!(policies[q].kind, PolicyKind::Member { finisher: true, .. }))
            .count()
    })
}

/// Whether a building stands in one of a civilization's cities (`_built_by`).
fn built_by(g: &Game, p: PlayerId, b: BuildingId) -> bool {
    g.player_cities(p).any(|c| c.buildings.contains(b))
}

/// Whether a building stands in any city (`_built_anywhere`).
fn built_anywhere(g: &Game, b: BuildingId) -> bool {
    g.state().cities().iter().any(|c| c.buildings.contains(b))
}

/// Whether a city's founder is a major civilization, dead or alive.
fn founded_by_a_major(g: &Game, founder: PlayerId) -> bool {
    g.player(founder).is_some_and(Player::is_major)
}

/// How many major civilizations' original capitals a civilization holds
/// (`_original_capitals_owned`, `victory.py:237-239`).
fn original_capitals_owned(g: &Game, p: PlayerId) -> usize {
    g.player_cities(p).filter(|c| c.original_capital && founded_by_a_major(g, c.founder)).count()
}

/// The major civilizations whose original capital stands, whoever holds it, and every living
/// one (`_civs_with_capitals`, `victory.py:242-246`).
fn civs_with_capitals(g: &Game) -> usize {
    let mut s = crate::base::sets::PlayerSet::default();
    for c in g.state().cities().iter() {
        if c.original_capital && founded_by_a_major(g, c.founder) {
            s.insert(c.founder);
        }
    }
    for pl in g.majors(true) {
        s.insert(pl.id());
    }
    s.len()
}

/// Whether a civilization has reached a milestone (`milestone_done`, `victory.py:249-270`).
#[must_use]
pub fn milestone_done(g: &Game, p: PlayerId, m: Milestone) -> bool {
    match m {
        Milestone::Build(b) => built_by(g, p, b),
        Milestone::AnyoneBuilds(b) => built_anywhere(g, b),
        Milestone::SpaceshipComplete => spaceship_status(g, p).complete,
        Milestone::CompletePolicyBranches(n) => completed_branches(g, p) >= usize::from(n),
        Milestone::CaptureAllCapitals => original_capitals_owned(g, p) == civs_with_capitals(g),
        Milestone::DestroyAllPlayers => {
            let mut alive = g.majors(true).map(Player::id);
            alive.next() == Some(p) && alive.next().is_none()
        }
        Milestone::WinDiplomaticVote => g.state().world().un.won.contains(p),
        Milestone::HighestScoreAfterMaxTurns => {
            g.turn() > g.total_turns() && super::score::best_score(g) == Some(p)
        }
    }
}

/// The victories enabled in this game, in the ruleset's order (`enabled_victories`).
pub fn enabled_victories(g: &Game) -> impl Iterator<Item = VictoryId> + '_ {
    g.rules().victories().ids().filter(|&v| g.victory_enabled(v))
}

/// A civilization's progress toward each enabled victory (`victory_progress`,
/// `victory.py:278-290`): its milestones in order up to the first not done, how many there are,
/// and how many are done.
#[must_use]
pub fn victory_progress(g: &Game, p: PlayerId) -> Value {
    let r = g.rules();
    let mut out = Map::new();
    for v in enabled_victories(g) {
        let def = &r.victories()[v];
        let mut done = Vec::new();
        let mut completed = 0;
        for m in def.milestones.iter() {
            let ok = milestone_done(g, p, m.milestone);
            done.push(json!({"milestone": &*m.text, "done": ok}));
            if !ok {
                break;
            }
            completed += 1;
        }
        out.insert(
            def.name.to_string(),
            json!({"milestones": done, "total": def.milestones.len(), "completed": completed}),
        );
    }
    Value::Object(out)
}

/// The victory a civilization has achieved, if any (`victory_achieved`, `victory.py:293-302`):
/// the first enabled victory whose milestones are all done, else the neutral victory of
/// `Triggers victory`. Only a living major civilization wins.
#[must_use]
pub fn victory_achieved(g: &Game, p: PlayerId) -> Option<Won> {
    if !g.player(p).is_some_and(|x| x.is_major() && x.alive()) {
        return None;
    }
    let r = g.rules();
    for v in enabled_victories(g) {
        if r.victories()[v].milestones.iter().all(|m| milestone_done(g, p, m.milestone)) {
            return Some(Won::Victory(v));
        }
    }
    civ_has(g, p, UniqueType::TriggersVictory).then_some(Won::Neutral)
}
