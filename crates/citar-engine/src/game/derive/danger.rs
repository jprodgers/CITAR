//! Where a civilization's civilians should not go (DESIGN.md 6.5, `DangerMap`): the tiles within
//! striking reach of the hostile military units it sees (`automation._danger_tiles`,
//! `automation.py:149-157`), which automated workers and explorers keep out of.
//!
//! Python rebuilt the set for every worker and every explorer, walking every unit of the world. It
//! is a memo per civilization here, recomputed when a unit moves, is made, lost or changed, when
//! war or peace is made, when what the civilization sees changes (`CivRevs::sight`), when a tile
//! changes (an embarked unit moves less), or when the conditionals of the movement uniques that
//! decide an enemy's reach may have: those its last computation read (`unique::record`), asked for
//! each civilization it is at war with.

use core::cell::{Cell, Ref};

use super::civ::cond;
use super::rev::{Memo, Rev};
use crate::base::ids::{PlayerId, TileIdx, UnitId};
use crate::base::sets::{BitSet, PlayerVec};
use crate::game::Game;
use crate::state::State;
use crate::state::units::Unit;
use crate::unique::{CondDeps, Ctx, record};

/// One civilization's danger tiles, with the classes its last computation recorded.
#[derive(Clone, Debug)]
struct DangerMemo {
    tiles: Memo<BitSet>,
    deps: Cell<CondDeps>,
}

impl Default for DangerMemo {
    fn default() -> Self {
        Self { tiles: Memo::new(), deps: Cell::new(CondDeps::empty()) }
    }
}

/// The memos of this module: part of `Derived`.
#[derive(Clone, Debug)]
pub(crate) struct DangerCaches {
    maps: PlayerVec<DangerMemo>,
}

impl DangerCaches {
    /// The memos of `st`'s players, none computed yet.
    pub(crate) fn new(st: &State) -> Self {
        Self { maps: st.players().ids().map(|_| DangerMemo::default()).collect() }
    }
}

/// How far an enemy unit could strike from where it stands (`automation._threat_reach`,
/// `automation.py:65-68`): its movement points, at least two.
#[must_use]
pub fn threat_reach(g: &Game, enemy: UnitId) -> u32 {
    let sc = g.rules().constants().move_scale.max(1);
    let points = crate::game::units::health::max_moves(g, enemy) / sc;
    u32::try_from(points.max(2)).unwrap_or(2)
}

/// Whether a unit is a military threat to civilization `p` (`automation._hostile_military`,
/// `automation.py:71-73`): a military unit of a civilization it is at war with.
#[must_use]
pub fn hostile_military(g: &Game, p: PlayerId, other: &Unit) -> bool {
    g.at_war(p, other.owner()) && g.rules().base_units()[other.base].military
}

/// Civilization `p`'s danger tiles, worked out afresh: within the reach of each hostile military
/// unit on a tile it sees.
#[must_use]
pub fn compute_danger(g: &Game, p: PlayerId) -> BitSet {
    let mut out = BitSet::new();
    let vis = g.dv.vis.visible(p);
    for q in g.state().players().ids() {
        if q == p || !g.at_war(p, q) {
            continue;
        }
        for other in g.player_units(q) {
            let t = other.tile();
            if !vis.is_some_and(|v| v.contains(t.0)) || !hostile_military(g, p, other) {
                continue;
            }
            for n in g.grid().within(t, threat_reach(g, other.id())) {
                out.insert(n.0);
            }
        }
    }
    out
}

/// Civilization `p`'s danger tiles as of now (the memo `DangerMap`); `None` for a player the game
/// does not have.
pub fn danger(g: &Game, p: PlayerId) -> Option<Ref<'_, BitSet>> {
    let m = g.dv.danger.maps.get(p)?;
    let revs = &g.dv.revs;
    let inputs = || {
        let mut r: Rev = revs
            .unit_pos
            .max(revs.units_core)
            .max(revs.diplo)
            .max(revs.tile_log.rev())
            .max(revs.civ(p).sight)
            .max(revs.config);
        let deps = m.deps.get();
        if !deps.is_empty() {
            let civ_level = deps.difference(CondDeps::LOCAL);
            for q in g.state().players().ids() {
                if q != p && g.at_war(p, q) {
                    r = r.max(cond(g, civ_level, &Ctx::civ(q)));
                }
            }
            if deps.intersects(CondDeps::LOCAL) {
                r = r.max(revs.cities).max(revs.city_core).max(revs.owners).max(revs.worked);
            }
        }
        r
    };
    let compute = || {
        let (tiles, d) = record::recorded(|| compute_danger(g, p));
        m.deps.set(d);
        tiles
    };
    Some(m.tiles.get(revs.now(), inputs, compute))
}

/// Whether tile `t` is one civilization `p`'s civilians keep out of.
#[must_use]
pub fn is_dangerous(g: &Game, p: PlayerId, t: TileIdx) -> bool {
    danger(g, p).is_some_and(|d| d.contains(t.0))
}

/// The cache oracle for this module's memos: each, validated, against a fresh look.
pub(crate) fn verify(g: &Game) -> Vec<String> {
    let mut out = Vec::new();
    for p in g.state().players().ids() {
        let fresh = compute_danger(g, p);
        if danger(g, p).is_none_or(|d| *d != fresh) {
            out.push(format!("player {}: its danger tiles differ from a fresh look", p.0));
        }
    }
    out
}
