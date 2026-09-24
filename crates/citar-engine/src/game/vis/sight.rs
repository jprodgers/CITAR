//! What each source sees (DESIGN.md 6.9): a unit's sight radius (`units.sight`, `units.py:304-310`)
//! and footprint (`visibility.unit_viewable`, `visibility.py:74-93`), a city's
//! (`visibility.py:100-104`), a city-state's cities as its ally sees them
//! (`visibility.py:107-111`) and a set-up spy's (`espionage.visible_tiles`,
//! `espionage.py:444-451`); and what a civilization can make out of a unit it sees
//! (`visibility.unit_visible_to`, `visibility.py:213-229`) and line of sight for an attack
//! (`visibility.has_los`, `visibility.py:69-71`).
//!
//! Only the living see, and never the barbarians: Python's `refresh` gave them no sight.

use std::sync::Arc;

use super::visibility::{Sight, SourceKey, VisSource, Visibility};
use crate::base::ids::{CityId, PlayerId, TileIdx, UniqueId, UnitId};
use crate::game::Game;
use crate::game::economy::city_tiles;
use crate::unique::filter::UnitScope;
use crate::unique::{Ctx, EvalWorld, IndexLayer, TileFacts, UniqueData, UniqueType, applies, uq};

/// Whether a player has sight: alive, and not the barbarians (`visibility.py:139-141`).
#[must_use]
pub fn has_sight(g: &Game, p: PlayerId) -> bool {
    g.player(p).is_some_and(|x| x.alive() && !x.is_barbarian())
}

/// How far a unit sees (`units.sight`, `units.py:304-310`): 2, with its own and its
/// civilization's `[n] Sight` and those of the terrains it stands on, and never below 1.
#[must_use]
pub fn sight(g: &Game, u: UnitId) -> i32 {
    let v = g.view();
    let ctx = Ctx::unit(&v, u);
    sight_in(g, u, &ctx)
}

fn sight_in(g: &Game, u: UnitId, ctx: &Ctx) -> i32 {
    let v = g.view();
    let Some(t) = g.unit(u).map(crate::state::units::Unit::tile) else { return 2 };
    let amount = |d: &UniqueData| match d {
        UniqueData::Sight(x) => Some(x.sight),
        _ => None,
    };
    let own = uq::sum_i32(uq::unit_and_civ(&v, u, UniqueType::Sight, ctx), amount);
    let ground = uq::sum_i32(uq::terrains(&v, t, UniqueType::Sight, ctx), amount);
    2i32.saturating_add(own).saturating_add(ground).max(1)
}

/// How a unit sees now (`visibility._unit_viewable`, `visibility.py:85-93`): `No Sight` sees its
/// own tile, `Can see over obstacles` the whole disc, and the rest the elevation walk. `None` for
/// a unit the game does not have.
#[must_use]
pub fn sight_of(g: &Game, u: UnitId) -> Option<Sight> {
    g.unit(u)?;
    let v = g.view();
    let ctx = Ctx::unit(&v, u);
    if uq::any(uq::unit(&v, u, UniqueType::NoSight, &ctx)) {
        return Some(Sight::Blind);
    }
    // The walk reaches no further than the map: a larger radius changes nothing.
    let reach = u32::from(g.grid().width()) + u32::from(g.grid().height());
    let r = u32::try_from(sight_in(g, u, &ctx)).unwrap_or(1).min(reach);
    Some(if uq::any(uq::unit(&v, u, UniqueType::CanSeeOverObstacles, &ctx)) {
        Sight::Clear(r)
    } else {
        Sight::Walk(r)
    })
}

/// The tiles a unit with this sight sees from `at`, sorted.
pub(crate) fn footprint(g: &Game, vis: &Visibility, at: TileIdx, sight: Sight) -> Arc<[TileIdx]> {
    match sight {
        Sight::Blind => Arc::from([at].as_slice()),
        Sight::Clear(r) => {
            let mut v = g.grid().within(at, r);
            v.sort();
            Arc::from(v)
        }
        Sight::Walk(r) => vis.line_of_sight(g.grid(), at, r, false),
    }
}

/// How a unit sees, as [`sight_of`] finds it, with its civilization's `[n] Sight` uniques read
/// from `mods`, the ones its index holds (kept by the sync's check of each civilization's sight
/// uniques), rather than from the index itself: a step then reads no civilization-wide memo.
/// The cache oracle checks it against [`sight_of`].
pub(crate) fn sight_with(g: &Game, mods: &[(UniqueId, u16)], u: UnitId) -> Option<Sight> {
    let unit = g.unit(u)?;
    let v = g.view();
    let ctx = Ctx::unit(&v, u);
    let t = g.rules().uniques();
    let profile = crate::game::derive::civ::unit_profile(g, u);
    let holds = |id: UniqueId| applies(id, &ctx, &v);
    if profile.get(UniqueType::NoSight).iter().any(|e| holds(e.id)) {
        return Some(Sight::Blind);
    }
    let amount = |id: UniqueId| match t.get(id).data {
        UniqueData::Sight(x) => x.sight,
        _ => 0,
    };
    let mut r = 2i32;
    for e in profile.get(UniqueType::Sight) {
        if holds(e.id) {
            r = r.saturating_add(amount(e.id).saturating_mul(i32::from(e.n)));
        }
    }
    for &(id, n) in mods {
        if holds(id) {
            r = r.saturating_add(amount(id).saturating_mul(i32::from(n)));
        }
    }
    let rules = g.derived().vis().sight_rules();
    if !rules.terrains.is_empty() && !v.tile_terrains(unit.tile()).is_disjoint(&rules.terrains) {
        let ground = uq::terrains(&v, unit.tile(), UniqueType::Sight, &ctx);
        r = r.saturating_add(uq::sum_i32(ground, |d| match d {
            UniqueData::Sight(x) => Some(x.sight),
            _ => None,
        }));
    }
    let reach = u32::from(g.grid().width()) + u32::from(g.grid().height());
    let r = u32::try_from(r.max(1)).unwrap_or(1).min(reach);
    Some(if profile.get(UniqueType::CanSeeOverObstacles).iter().any(|e| holds(e.id)) {
        Sight::Clear(r)
    } else {
        Sight::Walk(r)
    })
}

/// A unit's source as the state has it now: `None` when it is gone or its owner has no sight.
/// With `reuse`, a registered footprint from the same tile with the same sight is kept; without,
/// it is worked out again, as it must be once the heights it read have changed.
pub(crate) fn unit_source(g: &Game, vis: &Visibility, u: UnitId, reuse: bool) -> Option<VisSource> {
    let unit = g.unit(u)?;
    let owner = unit.owner();
    if !has_sight(g, owner) {
        return None;
    }
    let at = unit.tile();
    let sight = sight_with(g, vis.stamp(owner).1, u)?;
    // Nothing that decides it moved: keep the footprint as it is.
    if reuse
        && let Some(old) = vis.source(SourceKey::Unit(u))
        && old.owner == owner
        && old.at == at
        && old.sight == Some(sight)
    {
        return Some(old.clone());
    }
    Some(VisSource { owner, at, sight: Some(sight), footprint: footprint(g, vis, at, sight) })
}

/// A city's source: its tiles and every tile next to one (`visibility.py:100-104`).
pub(crate) fn city_source(g: &Game, c: CityId) -> Option<VisSource> {
    let city = g.city(c)?;
    let owner = city.owner();
    if !has_sight(g, owner) {
        return None;
    }
    let mut tiles = city_tiles(g, c);
    let own = tiles.len();
    for i in 0..own {
        tiles.extend(g.grid().neighbors(tiles[i]));
    }
    tiles.sort();
    tiles.dedup();
    Some(VisSource { owner, at: city.tile(), sight: None, footprint: Arc::from(tiles) })
}

/// Every city-state city a civilization sees as an ally, with what it sees of it: the city's own
/// tiles, not the ring beyond (`visibility.py:107-111`). A player sees the cities of each living
/// city-state allied with it, and a city-state those of the city-state it is allied with.
pub(crate) fn ally_sources(g: &Game) -> Vec<(SourceKey, VisSource)> {
    let mut out = Vec::new();
    for q in g.city_states(true) {
        let qid = q.id();
        let ally = q.city_state.as_deref().and_then(crate::state::players::CityStateData::ally);
        let mut viewers: Vec<PlayerId> = ally.into_iter().collect();
        for p in g.city_states(true) {
            if p.city_state.as_deref().and_then(crate::state::players::CityStateData::ally)
                == Some(qid)
            {
                viewers.push(p.id());
            }
        }
        viewers.sort();
        viewers.dedup();
        viewers.retain(|&v| has_sight(g, v));
        for &c in g.state().cities().of(qid) {
            let Some(city) = g.city(c) else { continue };
            let tiles = city_tiles(g, c);
            let mut sorted = tiles;
            sorted.sort();
            let fp: Arc<[TileIdx]> = Arc::from(sorted);
            for &v in &viewers {
                out.push((
                    SourceKey::AllyCity(v, c),
                    VisSource {
                        owner: v,
                        at: city.tile(),
                        sight: None,
                        footprint: Arc::clone(&fp),
                    },
                ));
            }
        }
    }
    out.sort_by_key(|e| e.0);
    out
}

/// The spies of `p` that see a city, with what they see: a major's spy set up in a city sees it
/// and the ring round it, while espionage is on (`espionage.visible_tiles`,
/// `espionage.py:444-451`; `visibility.py:112-114`).
pub(crate) fn spy_sources(g: &Game, p: PlayerId) -> Vec<(SourceKey, VisSource)> {
    let mut out = Vec::new();
    let Some(pl) = g.player(p) else { return out };
    if !pl.is_major() || !has_sight(g, p) || !g.espionage_enabled() {
        return out;
    }
    let Some(major) = pl.major.as_deref() else { return out };
    for (i, s) in major.spies.iter().enumerate() {
        let Ok(i) = u8::try_from(i) else { break };
        let Some(city) = s.city.and_then(|c| g.city(c)) else { continue };
        if !s.action.is_set_up() {
            continue;
        }
        let mut tiles = g.grid().within(city.tile(), 1);
        tiles.sort();
        out.push((
            SourceKey::Spy(p, i),
            VisSource { owner: p, at: city.tile(), sight: None, footprint: Arc::from(tiles) },
        ));
    }
    out
}

/// What a civilization's index holds of the `[n] Sight` uniques its units' sight reads, with
/// their copies (`SightMods`, DESIGN.md 6.5): when it is unchanged, so is every unit's sight that
/// depends on nothing else of the civilization's.
pub(crate) fn sight_mods(g: &Game, p: PlayerId) -> Vec<(UniqueId, u16)> {
    let v = g.view();
    let ix = v.civ_index(p, IndexLayer::Full);
    ix.get(UniqueType::Sight).iter().map(|e| (e.id, e.n)).collect()
}

/// The tiles a unit sees (`visibility.unit_viewable`), sorted: its registered footprint, or
/// what it would see if its owner had sight.
#[must_use]
pub fn unit_viewable(g: &Game, u: UnitId) -> Arc<[TileIdx]> {
    let vis = g.derived().vis();
    if let Some(s) = vis.source(SourceKey::Unit(u)) {
        return Arc::clone(&s.footprint);
    }
    match (g.unit(u), sight_of(g, u)) {
        (Some(unit), Some(sight)) => footprint(g, vis, unit.tile(), sight),
        _ => Arc::from([].as_slice()),
    }
}

/// Whether a ranged attack from `from` can see `to` (`visibility.has_los`): the elevation walk
/// for an attack, with the distance between them as its radius.
#[must_use]
pub fn has_los(g: &Game, from: TileIdx, to: TileIdx) -> bool {
    let d = g.grid().distance(from, to);
    if d == u32::MAX {
        return false;
    }
    g.derived().vis().line_of_sight(g.grid(), from, d, true).binary_search(&to).is_ok()
}

/// Whether civilization `p` can make out unit `u` (`visibility.unit_visible_to`,
/// `visibility.py:213-229`, UnCiv's `MapUnit.isVisibleTo`): its own units always; others on a
/// tile it sees, unless they are `Invisible to others` and none of its units that
/// `Can see invisible [...] units` of theirs sees them, or `Invisible to non-adjacent units` and
/// none of its units stands next to them.
#[must_use]
pub fn unit_visible_to(g: &Game, p: PlayerId, u: UnitId) -> bool {
    let Some(unit) = g.unit(u) else { return false };
    if unit.owner() == p {
        return true;
    }
    let at = unit.tile();
    if !g.derived().vis().sees(p, at) {
        return false;
    }
    let v = g.view();
    let ctx = Ctx::unit(&v, u);
    if uq::any(uq::unit(&v, u, UniqueType::Invisible, &ctx)) {
        let filters = g.rules().uniques().filters();
        return g.player_units(p).any(|o| {
            let oid = o.id();
            let octx = Ctx::unit(&v, oid);
            uq::unit(&v, oid, UniqueType::CanSeeInvisibleUnits, &octx).any(|h| match h.data() {
                UniqueData::CanSeeInvisibleUnits(x) => {
                    unit_viewable(g, oid).binary_search(&at).is_ok()
                        && filters.unit_matches(x.units, &v, u, UnitScope::default())
                }
                _ => false,
            })
        });
    }
    if uq::any(uq::unit(&v, u, UniqueType::InvisibleToNonAdjacent, &ctx)) {
        return g.grid().within(at, 1).into_iter().any(|t| g.units_at(t).any(|o| o.owner() == p));
    }
    true
}

/// Whether a unit is a military unit of a player at war with `p` (`movement._visible_enemies`,
/// `movement.py:683-690`): what stops a move when it comes into view.
fn is_enemy(g: &Game, p: PlayerId, u: &crate::state::units::Unit) -> bool {
    u.owner() != p
        && g.at_war(p, u.owner())
        && g.rules().base_units().get(u.base).is_some_and(|b| b.military)
}

/// Whether one of `tiles`, which just came into `p`'s sight, holds an enemy military unit
/// (`movement.move_toward`'s "enemy spotted", `movement.py:628-640`). The barbarians never stop
/// for one.
#[must_use]
pub fn enemy_spotted(g: &Game, p: PlayerId, tiles: &[TileIdx]) -> bool {
    !g.is_barbarian(p) && tiles.iter().any(|&t| g.units_at(t).any(|u| is_enemy(g, p, u)))
}
