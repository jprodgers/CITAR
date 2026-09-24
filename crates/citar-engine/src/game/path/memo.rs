//! The memos a [`Mover`](super::Mover) is built from (DESIGN.md 6.3, 6.10): each civilization's
//! movement rules with whose land it may enter ([`CivParts`]), each unit's profile
//! ([`Profile`]), and the tiles from which enemies exert a zone of control on each
//! civilization's units ([`Zoc`]).
//!
//! Python cached a unit's profile per unit, tile and count of promotions, techs and policies, and
//! threw its caches away on every write (`g._cache`, `movement.py:41-80`); a search here built all
//! three afresh for each mover, a microsecond or more before the first tile was looked at. Each
//! is now a memo checked against the revisions of exactly what it reads, so a mover costs a few
//! reads while nothing it depends on moved:
//! - a civilization's rules read its unique index with the resource layer (the memo's own
//!   stamp), what the conditionals of the movement uniques read in its context, the units it has
//!   (the units it gained, which Carthage's crossing reads, grow only as it gains units), and for
//!   whose land it may enter, the relations, the cities, the turn and the settings;
//! - a unit's profile reads its promotions and type, its owner's index, and what the
//!   conditionals of the profile's uniques read in its context, its place among them when they
//!   read where it stands;
//! - the zones of control read who is at war with whom, the cities, and where every unit stands,
//!   what it is and on what terrain (whether it is embarked).
//!
//! [`verify`] is their cache oracle.

use core::cell::{Ref, RefCell};

use super::class::{CivMove, Profile, Zoc};
use crate::base::collections::LookupMap;
use crate::base::ids::{PlayerId, UnitId};
use crate::base::sets::{PlayerSet, PlayerVec};
use crate::game::Game;
use crate::game::derive::civ::{cond, index_full_changed};
use crate::game::derive::rev::{BitEq, Memo, Rev};
use crate::state::State;
use crate::unique::{CondDeps, Ctx};

/// A civilization's movement rules and whose land its units may enter: what every mover of its
/// shares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CivParts {
    pub civ: CivMove,
    /// Whose territory it may enter (`Game::can_enter_owner`, by owner).
    pub enter: PlayerSet,
    pub city_states: PlayerSet,
}

impl CivParts {
    /// Civilization `p`'s, worked out afresh.
    fn of(g: &Game, p: PlayerId) -> Self {
        let players = g.state().players();
        Self {
            civ: CivMove::of(g, &g.rules().derived().moves, p),
            enter: players.ids().filter(|&q| g.can_enter_owner(p, q)).collect(),
            city_states: players
                .iter()
                .filter(|(_, x)| x.is_city_state())
                .map(|(q, _)| q)
                .collect(),
        }
    }
}

impl BitEq for CivParts {
    fn bit_eq(&self, other: &Self) -> bool {
        self == other
    }
}

impl BitEq for Zoc {
    fn bit_eq(&self, other: &Self) -> bool {
        self.none == other.none && *self.tiles == *other.tiles
    }
}

/// A unit's profile, and the revision it was last found valid at.
#[derive(Clone, Debug)]
struct ProfileEntry {
    at: Rev,
    profile: Profile,
}

/// The memos of this module: part of `Derived`.
#[derive(Clone, Debug)]
pub(crate) struct MoveCaches {
    civs: PlayerVec<Memo<CivParts>>,
    /// Per civilization, for its units that are not land units and for its land units.
    zoc: PlayerVec<[Memo<Zoc>; 2]>,
    /// Per unit, added when a mover first asks; emptied when it has grown to twice the units
    /// there are, which drops the entries of units gone (the table is never walked).
    profiles: RefCell<LookupMap<UnitId, ProfileEntry>>,
}

impl MoveCaches {
    /// The memos of `st`'s players, none computed yet.
    pub(crate) fn new(st: &State) -> Self {
        Self {
            civs: st.players().ids().map(|_| Memo::new()).collect(),
            zoc: st.players().ids().map(|_| [Memo::new(), Memo::new()]).collect(),
            profiles: RefCell::new(LookupMap::new()),
        }
    }
}

/// Civilization `p`'s movement rules and whose land it may enter, as of now; `None` for a player
/// the game does not have.
pub(crate) fn civ_parts(g: &Game, p: PlayerId) -> Option<Ref<'_, CivParts>> {
    let m = g.dv.moves.civs.get(p)?;
    let revs = &g.dv.revs;
    let deps = g.rules().derived().moves.civ_deps;
    let inputs = || {
        let c = revs.civ(p);
        let mut r = index_full_changed(g, p)
            .max(c.index)
            .max(c.roster)
            .max(revs.diplo)
            .max(revs.turn)
            .max(revs.cities)
            .max(revs.config);
        if !deps.is_empty() {
            r = r.max(cond(g, deps, &Ctx::civ(p)));
        }
        r
    };
    Some(m.get(revs.now(), inputs, || CivParts::of(g, p)))
}

/// The tiles from which enemies exert a zone of control on player `p`'s units, land units when
/// `land`, as of now; `None` for a player the game does not have.
pub(crate) fn zoc(g: &Game, p: PlayerId, land: bool) -> Option<Ref<'_, Zoc>> {
    let m = &g.dv.moves.zoc.get(p)?[usize::from(land)];
    let revs = &g.dv.revs;
    let inputs = || {
        revs.diplo.max(revs.cities).max(revs.unit_pos).max(revs.units_core).max(revs.tile_log.rev())
    };
    Some(m.get(revs.now(), inputs, || Zoc::build(g, p, land)))
}

/// Unit `u`'s profile where it stands, as of now.
pub(crate) fn profile(g: &Game, u: UnitId) -> Profile {
    let now = g.dv.revs.now();
    let table = &g.dv.moves.profiles;
    if let Some(e) = table.borrow().get(&u).filter(|e| e.at == now) {
        return e.profile.clone();
    }
    let input = profile_inputs(g, u);
    if let Some(e) = table.borrow_mut().get_mut(&u).filter(|e| input <= e.at) {
        e.at = now;
        return e.profile.clone();
    }
    let fresh = Profile::of(g, &g.rules().derived().moves, u);
    let mut t = table.borrow_mut();
    if t.len() >= 2 * g.state().units().len() + 64 {
        t.clear();
    }
    t.insert(u, ProfileEntry { at: now, profile: fresh.clone() });
    fresh
}

/// The latest revision of what unit `u`'s profile reads.
fn profile_inputs(g: &Game, u: UnitId) -> Rev {
    let revs = &g.dv.revs;
    let Some(x) = g.unit(u) else { return revs.now() };
    let ur = revs.unit(u);
    let mut r = ur.core.max(index_full_changed(g, x.owner()));
    let deps = g.rules().derived().moves.profile_deps;
    if !deps.is_empty() {
        let v = g.view();
        r = r.max(cond(g, deps, &Ctx::unit(&v, u)));
        if deps.intersects(CondDeps::LOCAL) {
            // The conditionals ask where it stands: its place moving is enough to ask again.
            r = r.max(ur.place);
        }
    }
    r
}

/// The cache oracle for this module's memos: each, validated, against a fresh computation. One
/// line for each that disagrees.
pub(crate) fn verify(g: &Game) -> Vec<String> {
    let mut out = Vec::new();
    for p in g.state().players().ids() {
        if civ_parts(g, p).as_deref() != Some(&CivParts::of(g, p)) {
            out.push(format!("player {}: its movement rules differ from a fresh look", p.0));
        }
        for land in [false, true] {
            let fresh = Zoc::build(g, p, land);
            if !zoc(g, p, land).is_some_and(|z| z.bit_eq(&fresh)) {
                out.push(format!("player {}: its zones of control differ from a fresh look", p.0));
            }
        }
    }
    for x in g.state().units().iter() {
        let u = x.id();
        if profile(g, u) != Profile::of(g, &g.rules().derived().moves, u) {
            out.push(format!("unit {}: its movement profile differs from a fresh look", u.get()));
        }
    }
    out
}
