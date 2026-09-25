//! Barbarians (`barbarians.py`; UnCiv's `BarbarianManager`, `BarbarianEncampment` and
//! `BarbarianAutomation`): encampments that appear in the fog, units they spawn on a countdown,
//! cities sacked instead of taken, and the raiders' own AI.
//!
//! - **Aggression.** One number from 0 to 100 (the game's `barbarian_aggression`, else its
//!   level's) sets every knob: how far the raiders look, what odds they take, whether they would
//!   rather loot or fight, how many gather before a healthy city, how fast camps breed, and how
//!   hard a sack hits (`barbarians.py:32-95`).
//! - **Camps** ([`place_camps`], [`update_camps`]) appear out of everyone's sight, away from
//!   capitals and other camps, and linger 15 turns once destroyed to keep new ones away. A camp
//!   spawns when its countdown runs out, and the unit's type is drawn weighted by UnCiv's force
//!   evaluation among what the barbarians' techs allow, which follow the techs every living
//!   civilization shares.
//! - **Sacking** ([`sack_city`]): a city the barbarians beat keeps its owner; they take gold, may
//!   kill a citizen and burn a building (never a wonder or the palace), and leave it alone for a
//!   while.
//! - **The AI** ([`take_turn`]): ranged units first, then melee, then the captured civilians,
//!   which walk to the nearest camp. A military unit heals by pillaging when wounded, attacks or
//!   pillages what is in reach (whichever is worth more), else heads for the best target within
//!   its search radius, else wanders.
//!
//! What differs from Python, on purpose (DESIGN.md 6.11, 7.2):
//! - the attack search asks the combat preview from each tile the unit could attack from
//!   (`resolve::preview_of_from`) rather than moving the unit there and back
//!   (`barbarians.py:596-604`);
//! - `_seek` builds one [`PathTree`] and reads every candidate's path from it, where Python
//!   searched once per candidate, and the unit follows the path it found;
//! - the random draws are keyed by ids and the turn: the camp placement by the next camp id
//!   (Python counted the camps), a spawned unit's type by the camp (Python counted the units), a
//!   sack by the city (Python counted the events);
//! - the barbarians know every tile without an explored set to fill (`_know_the_land`): the
//!   searches read them as knowing the map (DESIGN.md 6.10).

use serde_json::{Map, Value, json};
use smallvec::SmallVec;

use super::cities::founding::remove_building;
use super::cities::lifecycle::add_population;
use super::cities::stats::max_health;
use super::combat::resolve;
use super::derive::rev::{CityTouch, PlayerTouch, UnitTouch, WorldTouch};
use super::path::{Mover, PathTree};
use super::units::{self, place_unit_near, unit_has};
use super::{Game, movement, tiles, workers};
use crate::base::ids::{BaseUnitId, BuildingId, CampId, CityId, PlayerId, TileIdx, UnitId};
use crate::base::num;
use crate::base::rng::{KeyPart, Purpose, Rng};
use crate::base::sets::{BitSet, PlayerSet};
use crate::base::stats::Stat;
use crate::rules::constants::BarbarianSetting;
use crate::rules::defs::{Domain, ResourceType};
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::units::Activity;
use crate::state::world::Camp;
use crate::unique::{CondData, UniqueData, UniqueType};

// ---- The setting (barbarians.py:20-95) ------------------------------------------------------------

/// The game's barbarian setting, or `None` when they are off (`level`).
#[must_use]
pub fn level(g: &Game) -> Option<&'static BarbarianSetting> {
    let r = g.rules();
    r.constants().barbarian_levels.get(g.state().config().barbarians).and_then(|l| l.level.as_ref())
}

/// Whether the barbarians rage: they breed faster (`raging`).
#[must_use]
pub fn raging(g: &Game) -> bool {
    level(g).is_some_and(|l| l.raging)
}

/// How aggressive the barbarians are, 0 to 100 (`aggression`, `barbarians.py:32-50`): the game's
/// setting, else its level's; 0 when they are off.
#[must_use]
pub fn aggression(g: &Game) -> i32 {
    let Some(lv) = level(g) else { return 0 };
    let v = g.state().config().barbarian_aggression.map_or_else(|| lv.aggression(), i32::from);
    v.clamp(0, 100)
}

/// Aggression as a fraction (`_aggr`).
fn aggr(g: &Game) -> f64 {
    f64::from(aggression(g)) / 100.0
}

/// Turns a sacked city is left alone: 10 at aggression 0, 5 at 100 (`sack_cooldown`).
#[must_use]
pub fn sack_cooldown(g: &Game) -> i32 {
    num::round_half_even_i32(10.0 - 5.0 * aggr(g))
}

/// Whether the barbarians sacked this city too recently to bother with it (`recently_sacked`).
#[must_use]
pub fn recently_sacked(g: &Game, c: CityId) -> bool {
    g.city(c).is_some_and(|x| g.turn() - x.sacked_turn < sack_cooldown(g))
}

/// How far the barbarians look for targets (`seek_radius`).
#[must_use]
pub fn seek_radius(g: &Game) -> u32 {
    u32::try_from(num::round_half_even_i32(10.0 * aggr(g))).unwrap_or(0)
}

/// The lowest attack score the barbarians accept: 0 at aggression 0, lower as they grow bolder
/// (`attack_bar`).
#[must_use]
pub fn attack_bar(g: &Game) -> f64 {
    -35.0 * aggr(g)
}

/// How much they mind the damage they take against the damage they deal (`_loss_weight`).
fn loss_weight(g: &Game) -> f64 {
    1.3 - 0.6 * aggr(g)
}

/// How much they value looting against fighting (`_pillage_weight`).
fn pillage_weight(g: &Game) -> f64 {
    1.4 - 0.6 * aggr(g)
}

/// How many gather round a healthy city before they storm it at bad odds (`_siege_size`).
#[must_use]
pub fn siege_size(g: &Game) -> usize {
    usize::try_from(num::round_half_even_i32(3.0 - 3.0 * aggr(g)).max(1)).unwrap_or(1)
}

/// How many may already be round a camp for it to spawn another (`_max_near_camp`).
#[must_use]
pub fn max_near_camp(g: &Game) -> usize {
    usize::try_from(num::round_half_even_i32(4.0 * aggr(g)).max(1)).unwrap_or(1)
}

/// The barbarian camp improvement, if the ruleset has one.
fn camp_improvement(g: &Game) -> Option<crate::base::ids::ImprovementId> {
    g.rules().derived().known.barbarian_camp
}

/// Whether a tile carries the barbarian camp improvement.
#[must_use]
pub fn is_camp_tile(g: &Game, t: TileIdx) -> bool {
    let camp = camp_improvement(g);
    camp.is_some() && g.tile(t).and_then(crate::state::map::Tile::improvement) == camp
}

/// How many barbarian military units stand within `radius` of a tile (`_barbs_near`).
fn barbs_near(g: &Game, t: TileIdx, radius: u32) -> usize {
    let Some(bid) = g.barbarian_id() else { return 0 };
    g.grid()
        .within(t, radius)
        .into_iter()
        .filter(|&i| g.military_at(i).is_some_and(|m| m.owner() == bid))
        .count()
}

// ---- Camps (barbarians.py:117-204, 342-407) --------------------------------------------------------

/// Every tile a living civilization other than the barbarians sees now (`_viewable_by_anyone`):
/// no camp appears before anyone's eyes.
fn viewable_by_anyone(g: &Game) -> BitSet {
    let mut out = BitSet::new();
    let vis = g.derived().vis();
    for (p, pl) in g.state().players().iter() {
        if pl.is_barbarian() || !pl.alive() {
            continue;
        }
        if let Some(seen) = vis.visible(p) {
            out.union_with(seen);
        }
    }
    out
}

/// Whether a feature on the tile keeps improvements off it (`RestrictedBuildableImprovements`).
fn restricted_features(g: &Game, t: TileIdx) -> bool {
    let r = g.rules();
    let Some(tile) = g.tile(t) else { return false };
    tile.features().iter().any(|f| {
        r.derived().features.get(f).is_some_and(|&ft| {
            super::core::has_type(
                r,
                &r.terrains()[ft].uniques,
                UniqueType::RestrictedBuildableImprovements,
            )
        })
    })
}

/// New camps in the fog (`place_camps`, UnCiv's `placeBarbarianEncampment`,
/// `barbarians.py:134-184`): at the game's start a third of what the fog holds room for, later one
/// at a time on half the turns; out of sight, on free land beside land, away from capitals and
/// other camps (and near the coast one time in six).
pub fn place_camps(g: &mut Game, first: bool) {
    let next = u64::from(g.state().ids().camp);
    let mut rng = Rng::keyed(g.state().seed(), Purpose::BarbPlace, &[g.turn().key(), next]);
    if !first && rng.unit() < 0.5 {
        return;
    }
    let viewable = viewable_by_anyone(g);
    let size = g.grid().size();
    let fog: Vec<TileIdx> =
        g.grid().tiles().filter(|&t| g.is_land(t) && !viewable.contains(t.0)).collect();
    let per_camp = num::trunc_i64(num::pow(f64::from(size), 0.4)).max(1);
    let standing = i64::try_from(g.state().world().camps.values().filter(|c| !c.destroyed).count())
        .unwrap_or(i64::MAX);
    let mut to_add = i64::try_from(fog.len()).unwrap_or(0) / per_camp - standing;
    if first {
        to_add = (to_add / 3).max(1);
    } else if to_add > 0 {
        to_add = 1;
    }
    if to_add <= 0 {
        return;
    }
    let mut near = BitSet::new();
    for p in g.majors(true) {
        if let Some(cap) = p.capital.and_then(|c| g.city(c)) {
            for t in g.grid().within(cap.tile(), 4) {
                near.insert(t.0);
            }
        }
    }
    for c in g.state().world().camps.values() {
        for t in g.grid().within(c.tile, if c.destroyed { 4 } else { 7 }) {
            near.insert(t.0);
        }
    }
    let mut viable: Vec<TileIdx> = fog
        .into_iter()
        .filter(|&t| {
            let Some(tile) = g.tile(t) else { return false };
            !tiles::is_impassable(g, t)
                && tile.resource().is_none()
                && tile.improvement().is_none()
                && g.city_at(t).is_none()
                && tile.owner().is_none()
                && g.units_at(t).next().is_none()
                && !restricted_features(g, t)
                && g.grid().neighbors(t).any(|n| g.is_land(n))
                && !near.contains(t.0)
        })
        .collect();
    let mut added = 0;
    let mut bias_coast = rng.below(6) == 0;
    while added < to_add && !viable.is_empty() {
        let coast: Vec<TileIdx> = if bias_coast {
            viable.iter().copied().filter(|&t| tiles::adjacent_to_coast(g, t)).collect()
        } else {
            Vec::new()
        };
        let pool = if coast.is_empty() { &viable } else { &coast };
        let Some(&t) = rng.pick(pool) else { break };
        create_camp(g, t);
        let told: Vec<PlayerId> = g
            .majors(true)
            .filter(|q| q.explored.contains(t.0))
            .map(crate::state::players::Player::id)
            .filter(|&q| {
                super::diplomacy::relations::civ_has(
                    g,
                    q,
                    UniqueType::NotifiedOfBarbarianEncampments,
                )
            })
            .collect();
        for q in told {
            g.emit(
                EngineEvent::CampSpawned,
                "A new barbarian encampment has spawned!",
                Some(PlayerSet::single(q)),
                Some(t),
                EventData::default(),
                &[],
            );
        }
        added += 1;
        let blocked = g.grid().within(t, 7);
        viable.retain(|v| !blocked.contains(v));
        bias_coast = rng.below(6) == 0;
    }
}

/// Puts a barbarian camp on a tile (`create_camp`, `barbarians.py:187-195`). `None` if the ruleset
/// has no camp or the ids are spent.
pub fn create_camp(g: &mut Game, t: TileIdx) -> Option<CampId> {
    let camp = camp_improvement(g)?;
    let id = g.st.ids_mut().next_camp()?;
    g.edit_world(WorldTouch::CAMPS).camps.insert(id, Camp::new(t));
    let route = g.tile(t).is_some_and(crate::state::map::Tile::route_pillaged);
    let refused = |e: crate::state::StateError| {
        debug_assert!(false, "a tile of the map takes a camp: {e}");
    };
    g.set_improvement(t, Some(camp)).unwrap_or_else(refused);
    g.set_pillaged(t, route, false).unwrap_or_else(refused);
    Some(id)
}

/// Removes every camp and its improvement, as a bare game for the rule scripts has none; the
/// tiles the camps stood on, in camp order.
#[cfg(feature = "test-ops")]
#[doc(hidden)]
pub fn clear_camps(g: &mut Game) -> Vec<TileIdx> {
    let tiles: Vec<TileIdx> = g.state().world().camps.values().map(|c| c.tile).collect();
    for &t in &tiles {
        if is_camp_tile(g, t) {
            let set = g.set_improvement(t, None);
            debug_assert!(set.is_ok(), "a tile of the map loses its camp: {set:?}");
        }
    }
    g.edit_world(WorldTouch::CAMPS).camps.clear();
    tiles
}

/// Setup's camps (`place_initial_camps`, `barbarians.py:198-204`): the barbarians' techs set, then
/// the first camps, in a game with barbarians.
pub fn place_initial_camps(g: &mut Game) {
    if level(g).is_none() || g.barbarian_id().is_none() {
        return;
    }
    update_barbarian_techs(g);
    place_camps(g, true);
}

/// Keeps the barbarians' techs those every living civilization knows (`_update_barbarian_techs`,
/// `barbarians.py:210-228`): without it they would stay in the ancient era.
fn update_barbarian_techs(g: &mut Game) {
    let Some(bid) = g.barbarian_id() else { return };
    let mut common: Option<crate::base::sets::TechSet> = None;
    for (_, p) in g.state().players().iter() {
        if p.is_barbarian() || !p.alive() {
            continue;
        }
        common = Some(common.map_or(p.tech.known, |c| c & p.tech.known));
    }
    let techs = common.unwrap_or_default();
    if g.player(bid).is_some_and(|p| p.tech.known != techs)
        && let Some(p) = g.player_mut(bid, PlayerTouch::INDEX)
    {
        p.tech.known = techs;
    }
}

/// UnCiv's `BaseUnit.getForceEvaluation` (`force_evaluation`, `barbarians.py:231-261`): how
/// strong a unit type is, for weighting which a camp spawns.
#[must_use]
pub fn force_evaluation(g: &Game, base: BaseUnitId) -> i32 {
    let r = g.rules();
    let d = &r.base_units()[base];
    if d.strength == 0 && d.ranged_strength == 0 {
        return 0;
    }
    let mut power = num::pow(f64::from(d.strength), 1.5);
    let mut rp = num::pow(f64::from(d.ranged_strength), 1.45);
    if d.domain == Domain::Water {
        rp /= 2.0;
    }
    if rp > 0.0 {
        power = rp;
    }
    power *= num::pow(f64::from(d.movement), 0.3);
    if units::type_has(g, base, UniqueType::SelfDestructs) {
        power /= 2.0;
    }
    if units::type_has(g, base, UniqueType::NuclearWeapon) {
        power += 4000.0;
    }
    let t = r.uniques();
    let mut best = 1.0;
    for id in d.uniques.ids() {
        let u = t.get(id);
        let UniqueData::Strength(x) = u.data else { continue };
        let v = f64::from(x.percent);
        if v <= 0.0 {
            continue;
        }
        let conds = t.conds(u);
        let vs_units = conds.iter().any(|c| matches!(c.data, CondData::ConditionalVsUnits(_)));
        let halved = conds.iter().any(|c| {
            matches!(
                c.data,
                CondData::ConditionalVsCity
                    | CondData::ConditionalAttacking
                    | CondData::ConditionalDefending
                    | CondData::ConditionalFightingInTiles(_)
            )
        });
        best = if vs_units {
            1.0 + v / 4.0 / 100.0
        } else if halved {
            1.0 + v / 2.0 / 100.0
        } else {
            1.0 + v / 100.0
        };
    }
    num::trunc_i32(power * best)
}

/// The units a camp could spawn now, land or naval (`_barbarian_unit_options`,
/// `barbarians.py:264-283`): military units that may attack and be barbarians, of no nation, not
/// great people, nuclear weapons or unbuildable, that the barbarians' techs allow and have not
/// made obsolete; in ruleset order.
#[must_use]
pub fn unit_options(g: &Game, naval: bool) -> Vec<BaseUnitId> {
    let Some(bid) = g.barbarian_id() else { return Vec::new() };
    let r = g.rules();
    let domain = if naval { Domain::Water } else { Domain::Land };
    r.base_units()
        .iter()
        .filter(|&(u, d)| {
            d.military
                && d.domain == domain
                && d.unique_to.is_none()
                && !d.great_person
                && g.has_tech(bid, d.required_tech)
                && !d.obsolete_tech.is_some_and(|t| g.has_tech(bid, Some(t)))
                && ![
                    UniqueType::CannotAttack,
                    UniqueType::CannotBeBarbarian,
                    UniqueType::Unbuildable,
                    UniqueType::NuclearWeapon,
                    UniqueType::OnlyAvailable,
                ]
                .into_iter()
                .any(|ty| units::type_has(g, u, ty))
        })
        .map(|(u, _)| u)
        .collect()
}

/// A unit type for a camp to spawn, weighted by its force (`_choose_unit`), drawn from
/// `Purpose::BarbUnit` keyed by the turn and the camp (Python counted the units).
fn choose_unit(g: &Game, naval: bool, key: u64) -> Option<BaseUnitId> {
    let opts = unit_options(g, naval);
    let weights: Vec<f64> =
        opts.iter().map(|&u| f64::from(force_evaluation(g, u).max(1))).collect();
    let mut rng = Rng::keyed(g.state().seed(), Purpose::BarbUnit, &[g.turn().key(), key]);
    rng.weighted(&weights).and_then(|i| opts.get(i).copied())
}

/// A camp spawns a unit, for the barbarians or, when a civilization clears it and recruits, for
/// that civilization (`spawn_barbarian`, UnCiv's `spawnBarbarian`, `barbarians.py:295-316`): on
/// the camp if it is free, else (after turn 10, and while few barbarians are about) beside it,
/// at sea after turn 30.
pub fn spawn_barbarian(
    g: &mut Game,
    camp: Option<CampId>,
    t: TileIdx,
    allegiance: Option<PlayerId>,
) -> Option<UnitId> {
    let bid = g.barbarian_id();
    let owner = allegiance.or(bid)?;
    let key = camp.map_or(u64::from(t.0), |c| u64::from(c.get()));
    if Some(owner) == bid {
        if g.military_at(t).is_none() {
            return spawn(g, t, false, owner, key);
        }
        if g.turn() < 10 || barbs_near(g, t, 4) > max_near_camp(g) {
            return None;
        }
    }
    let can_naval = g.turn() > 30;
    let valid: Vec<TileIdx> = g
        .grid()
        .neighbors(t)
        .filter(|&n| {
            let water = g.is_water(n);
            !(tiles::is_impassable(g, n)
                || g.city_at(n).is_some()
                || g.units_at(n).next().is_some()
                || (water && !can_naval)
                || (water && workers::fresh_water(g, n)))
        })
        .collect();
    let mut rng = Rng::keyed(g.state().seed(), Purpose::BarbSpawn, &[g.turn().key(), t.key()]);
    let &side = rng.pick(&valid)?;
    let naval = g.is_water(side);
    spawn(g, t, naval, owner, key)
}

/// Makes a camp's unit near its tile (`_spawn`).
fn spawn(g: &mut Game, t: TileIdx, naval: bool, owner: PlayerId, key: u64) -> Option<UnitId> {
    update_barbarian_techs(g);
    let base = choose_unit(g, naval, key)?;
    place_unit_near(g, owner, base, t)
}

/// Sets how long until a camp spawns again (`_reset_countdown`, `barbarians.py:329-339`): 8 to
/// 12 turns, halved when raging, plus the barbarian difficulty's delay, less what the camp has
/// spawned (3 at most), scaled by aggression and the speed. Drawn from `Purpose::BarbCountdown`
/// keyed by the turn and the camp.
fn reset_countdown(g: &mut Game, id: CampId) {
    let Some(camp) = g.state().world().camps.get(&id).copied() else { return };
    let mut rng = Rng::keyed(g.state().seed(), Purpose::BarbCountdown, &[g.turn().key(), id.key()]);
    let mut cd = 8 + i32::try_from(rng.below(5)).unwrap_or(0);
    if raging(g) {
        cd /= 2;
    }
    let cfg = g.state().config();
    cd += g.rules().difficulties()[cfg.barbarian_difficulty].barbarian_spawn_delay;
    cd -= i32::from(camp.spawned).min(3);
    let scaled = f64::from(cd) * (1.3 - 0.6 * aggr(g));
    let countdown = num::trunc_i32(scaled * g.speed().barbarian_modifier).max(0);
    if let Some(c) = g.edit_world(WorldTouch::CAMPS).camps.get_mut(&id) {
        c.countdown = i16::try_from(countdown).unwrap_or(i16::MAX);
    }
}

/// The camps' turn (`update_camps`, UnCiv's `updateEncampments`, `barbarians.py:342-356`): a camp
/// whose improvement is gone is destroyed and lingers 15 turns, then is forgotten; new camps may
/// appear; each standing camp counts down, and spawns when it reaches 0.
pub fn update_camps(g: &mut Game) {
    let camps: Vec<(CampId, Camp)> =
        g.state().world().camps.iter().map(|(&id, &c)| (id, c)).collect();
    for (id, c) in camps {
        let gone = !c.destroyed && !is_camp_tile(g, c.tile);
        if gone || (c.destroyed && c.countdown == 0) {
            let w = g.edit_world(WorldTouch::CAMPS);
            if c.destroyed {
                w.camps.remove(&id);
            } else if let Some(x) = w.camps.get_mut(&id) {
                x.destroyed = true;
                x.countdown = 15;
            }
        }
    }
    place_camps(g, false);
    let ids: Vec<CampId> = g.state().world().camps.keys().copied().collect();
    for id in ids {
        let Some(c) = g.state().world().camps.get(&id).copied() else { continue };
        if c.countdown > 0 {
            if let Some(x) = g.edit_world(WorldTouch::CAMPS).camps.get_mut(&id) {
                x.countdown -= 1;
            }
        } else if !c.destroyed && spawn_barbarian(g, Some(id), c.tile, None).is_some() {
            if let Some(x) = g.edit_world(WorldTouch::CAMPS).camps.get_mut(&id) {
                x.spawned = x.spawned.saturating_add(1);
            }
            reset_countdown(g, id);
        }
    }
}

/// A camp attacked spawns sooner: its countdown halves (`camp_attacked`, `barbarians.py:359-363`).
pub fn camp_attacked(g: &mut Game, t: TileIdx) {
    let Some(id) = g.state().world().camp_at(t) else { return };
    if let Some(c) = g.edit_world(WorldTouch::CAMPS).camps.get_mut(&id) {
        c.countdown /= 2;
    }
}

/// Marks the camp on a tile destroyed, to be forgotten in 15 turns.
fn destroy_camp(g: &mut Game, t: TileIdx) -> Option<CampId> {
    let id = g.state().world().camp_at(t)?;
    if let Some(c) = g.edit_world(WorldTouch::CAMPS).camps.get_mut(&id) {
        c.destroyed = true;
        c.countdown = 15;
    }
    Some(id)
}

/// A camp goes without a unit clearing it, as when a city's borders take its tile (`remove_camp`,
/// `barbarians.py:366-377`): the improvement and the camp go, and the city-states' quests for it
/// with them.
pub fn remove_camp(g: &mut Game, t: TileIdx) {
    if is_camp_tile(g, t) {
        let set = g.set_improvement(t, None);
        debug_assert!(set.is_ok(), "a tile of the map loses its camp: {set:?}");
    }
    destroy_camp(g, t);
    super::city_states::quests::camp_removed(g, t);
}

/// A civilization's military unit clears a camp (`clear_camp`, UnCiv's `MapUnit.clearEncampment`,
/// `barbarians.py:380-407`): the city-state that asked for it rewards it, the camp goes, and the
/// civilization loots its difficulty's reward (with `When conquering an encampment, earn [n]
/// Gold and recruit a Barbarian unit`, which also recruits one, and `Receive [n]% Gold from
/// Barbarian encampments ...`), scaled by the speed. Returns the gold.
pub fn clear_camp(g: &mut Game, t: TileIdx, p: PlayerId) -> i32 {
    super::city_states::quests::camp_cleared(g, t, p);
    let refused = |e: crate::state::StateError| {
        debug_assert!(false, "a tile of the map loses its camp: {e}");
    };
    g.set_improvement(t, None).unwrap_or_else(refused);
    let camp = destroy_camp(g, t);
    let r = g.rules();
    let mut gold = f64::from(r.difficulties()[g.difficulty(Some(p))].clear_barbarian_camp_reward);
    let (recruits, percents): (SmallVec<[i32; 2]>, SmallVec<[i32; 2]>) = {
        let v = g.view();
        let ctx = crate::unique::Ctx::civ(p);
        let recruits = crate::unique::uq::civ(&v, p, UniqueType::GainFromEncampment, &ctx)
            .flat_map(|h| match *h.data() {
                UniqueData::GainFromEncampment(x) => core::iter::repeat_n(x.gold, usize::from(h.n)),
                _ => core::iter::repeat_n(0, 0),
            })
            .collect();
        let percents =
            crate::unique::uq::civ(&v, p, UniqueType::GoldFromEncampmentsAndCities, &ctx)
                .flat_map(|h| match *h.data() {
                    UniqueData::GoldFromEncampmentsAndCities(x) => {
                        core::iter::repeat_n(x.percent, usize::from(h.n))
                    }
                    _ => core::iter::repeat_n(0, 0),
                })
                .collect();
        (recruits, percents)
    };
    for extra in recruits {
        gold += f64::from(extra);
        if let Some(u) = spawn_barbarian(g, camp, t, Some(p)) {
            if let Some(x) = g.unit_mut(u, UnitTouch::CORE | UnitTouch::MOVES) {
                x.hp = 100;
                x.moves = 0;
            }
            let (base, at) = g.unit(u).map(|x| (x.base, x.tile())).unwrap_or((BaseUnitId(0), t));
            let text = format!("An enemy {} has joined us!", g.rules().base_units()[base].name);
            let data = EventData { unit: Some(u), ..EventData::default() };
            g.emit(
                EngineEvent::CampRecruit,
                &text,
                Some(PlayerSet::single(p)),
                Some(at),
                data,
                &[],
            );
        }
    }
    gold *= g.speed().gold_cost_modifier;
    for pct in percents {
        gold *= 1.0 + f64::from(pct) / 100.0;
    }
    let gold = num::trunc_i32(gold);
    g.add_stat(p, Stat::Gold, f64::from(gold));
    let name = g.player(p).map(|x| x.name.to_string()).unwrap_or_default();
    let text = format!("{name} destroyed a barbarian encampment and looted {gold} gold.");
    let data = EventData { gold: Some(gold), ..EventData::default() };
    g.emit(EngineEvent::CampCleared, &text, Some(PlayerSet::single(p)), Some(t), data, &[]);
    gold
}

// ---- Sacking cities (barbarians.py:410-470) --------------------------------------------------------

/// The buildings a sack may burn (`_sackable_buildings`): never a wonder of any kind, the palace
/// or a free building; in id order.
fn sackable_buildings(g: &Game, c: CityId) -> Vec<BuildingId> {
    let Some(city) = g.city(c) else { return Vec::new() };
    let r = g.rules();
    city.buildings
        .iter()
        .filter(|&b| {
            let d = &r.buildings()[b];
            !city.free_buildings.contains(b)
                && !d.is_wonder
                && !d.is_national_wonder
                && !d.any_wonder
                && !super::core::has_type(r, &d.uniques, UniqueType::IndicatesCapital)
        })
        .collect()
}

/// What a sack did (`sack_city`'s result).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sack {
    /// The city's name.
    pub city: Box<str>,
    /// It was sacked too recently to have anything left: the raiders were driven off.
    pub too_recent: bool,
    pub gold: i32,
    pub citizen_killed: bool,
    /// The building burned, with its name.
    pub building: Option<(BuildingId, Box<str>)>,
}

impl Sack {
    /// Adds what happened to an attack's result, with Python's keys.
    pub fn write(&self, out: &mut Map<String, Value>) {
        if self.too_recent {
            out.insert("sacked_city".into(), Value::Null);
            out.insert(
                "note".into(),
                json!(format!(
                    "{} was sacked too recently to have anything left to take.",
                    self.city
                )),
            );
            return;
        }
        out.insert("sacked_city".into(), json!(&*self.city));
        out.insert("gold_stolen".into(), json!(self.gold));
        out.insert("citizen_killed".into(), json!(self.citizen_killed));
        out.insert(
            "building_destroyed".into(),
            self.building.as_ref().map_or(Value::Null, |(_, n)| json!(&**n)),
        );
    }

    /// The result on its own.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        self.write(&mut out);
        Value::Object(out)
    }
}

/// The barbarians sack a city they beat instead of taking it (`sack_city`, `barbarians.py:430-470`,
/// Civilization V's rule): they carry off a share of its owner's gold, may kill a citizen and burn
/// a building (never a wonder or the palace), and leave it with a little health. How hard it hits
/// grows with aggression; a city sacked too recently is only driven clear. Drawn from
/// `Purpose::BarbSack` keyed by the turn and the city (Python counted the events).
pub fn sack_city(g: &mut Game, c: CityId) -> Sack {
    let Some((owner, name, at)) = g.city(c).map(|x| (x.owner(), x.name.clone(), x.tile())) else {
        return Sack {
            city: Box::default(),
            too_recent: true,
            gold: 0,
            citizen_killed: false,
            building: None,
        };
    };
    if recently_sacked(g, c) {
        if let Some(x) = g.city_mut(c, CityTouch::CORE) {
            x.health = x.health.max(2);
        }
        return Sack {
            city: name,
            too_recent: true,
            gold: 0,
            citizen_killed: false,
            building: None,
        };
    }
    let a = aggr(g);
    let mut rng = Rng::keyed(g.state().seed(), Purpose::BarbSack, &[g.turn().key(), c.key()]);
    let have = num::trunc_i64(g.player(owner).map_or(0.0, |p| p.econ.gold)).max(0);
    #[allow(clippy::cast_precision_loss, reason = "gold stocks far below 2^53")]
    let share = num::trunc_i64(have as f64 * (0.25 + 0.25 * a));
    let cap = num::trunc_i64((150.0 + 350.0 * a) * g.speed().gold_cost_modifier);
    let stolen = have.min(share.max(have.min(25))).min(cap);
    let stolen = i32::try_from(stolen).unwrap_or(i32::MAX);
    if stolen != 0 {
        g.add_stat(owner, Stat::Gold, -f64::from(stolen));
    }
    let pop = g.city(c).map_or(0, |x| x.pop);
    let killed = pop > 1 && rng.unit() < 0.3 + 0.4 * a;
    if killed {
        add_population(g, c, -1);
    }
    let opts = sackable_buildings(g, c);
    let mut burned = None;
    if !opts.is_empty() && rng.unit() < 0.25 + 0.35 * a {
        burned = rng.pick(&opts).copied();
        if let Some(b) = burned {
            remove_building(g, c, b);
        }
    }
    let health = (max_health(g, c) / 4).max(2);
    let turn = g.turn();
    if let Some(x) = g.city_mut(c, CityTouch::CORE) {
        x.health = health;
        x.sacked_turn = turn;
    }
    let r = g.rules();
    let building = burned.map(|b| (b, r.buildings()[b].name.clone()));
    let mut parts: Vec<String> = Vec::new();
    if stolen != 0 {
        parts.push(format!("stole {stolen} gold"));
    }
    if killed {
        parts.push("killed a citizen".to_owned());
    }
    if let Some((_, n)) = &building {
        parts.push(format!("burned the {n}"));
    }
    let text = match parts.split_last() {
        None => format!("Barbarians sacked {name}, but found nothing worth carrying off."),
        Some((last, [])) => format!("Barbarians sacked {name}: they {last}!"),
        Some((last, rest)) => {
            format!("Barbarians sacked {name}: they {} and {last}!", rest.join(", "))
        }
    };
    let data = EventData {
        gold: Some(stolen),
        citizen_killed: Some(killed),
        building: burned,
        ..EventData::default()
    };
    g.emit(EngineEvent::CitySacked, &text, Some(PlayerSet::single(owner)), Some(at), data, &[]);
    Sack { city: name, too_recent: false, gold: stolen, citizen_killed: killed, building }
}

// ---- The barbarians' turn (barbarians.py:473-807) --------------------------------------------------

/// Stage S0: the barbarians move every unit, then their camps act (`take_turn`,
/// `barbarians.py:476-493`): ranged units first, then melee, then the rest, each in id order.
pub fn take_turn(g: &mut Game) {
    let Some(bid) = g.barbarian_id() else { return };
    if level(g).is_none() {
        return;
    }
    let r = g.rules();
    let mine: Vec<(UnitId, BaseUnitId)> = g.player_units(bid).map(|u| (u.id(), u.base)).collect();
    let ranged = mine.iter().filter(|&&(_, b)| r.base_units()[b].ranged);
    let melee = mine.iter().filter(|&&(_, b)| r.base_units()[b].melee);
    let rest = mine.iter().filter(|&&(_, b)| {
        let d = &r.base_units()[b];
        !d.ranged && !d.melee
    });
    let order: Vec<UnitId> = ranged.chain(melee).chain(rest).map(|&(u, _)| u).collect();
    for u in order {
        if g.unit(u).is_none() {
            continue;
        }
        if automate(g, u).is_err()
            && let Some(x) = g.unit_mut(u, UnitTouch::MOVES)
        {
            x.moves = 0;
        }
    }
    update_camps(g);
}

/// One barbarian unit's orders now, with the moves it has (`_automate`), for the test operation
/// `barbarian_act`.
#[cfg(feature = "test-ops")]
#[doc(hidden)]
pub fn act_for_test(g: &mut Game, u: UnitId) {
    if automate(g, u).is_err()
        && let Some(x) = g.unit_mut(u, UnitTouch::MOVES)
    {
        x.moves = 0;
    }
}

/// Stage S0's row: [`take_turn`], for the barbarians.
pub(crate) fn take_turn_stage(g: &mut Game, _: PlayerId) {
    take_turn(g);
}

/// A step the AI took was refused where Python raised out of the unit's turn: the unit's
/// movement is spent (`barbarians.py:489-492`).
#[derive(Clone, Copy, Debug)]
struct Refused;

/// The unit's moves left, 0 once it is gone.
fn moves(g: &Game, u: UnitId) -> i32 {
    g.unit(u).map_or(0, |x| x.moves)
}

/// Sets an activity.
fn set_activity(g: &mut Game, u: UnitId, a: Activity) {
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
        x.activity = Some(a);
    }
}

/// One barbarian unit's orders (`_automate`, `barbarians.py:496-525`).
fn automate(g: &mut Game, u: UnitId) -> Result<(), Refused> {
    let Some((base, tile, hp)) = g.unit(u).map(|x| (x.base, x.tile(), x.hp)) else { return Ok(()) };
    if !g.rules().base_units()[base].military {
        return automate_civilian(g, u);
    }
    if is_camp_tile(g, tile) {
        // The camp's guard stays home.
        if try_attack(g, u, true)? {
            return Ok(());
        }
        set_activity(g, u, Activity::Fortify);
        return Ok(());
    }
    let a = aggr(g);
    if f64::from(hp) < 50.0 - 20.0 * a {
        // Wounded: pillage, which heals, before anything else.
        if try_pillage(g, u, true)? && moves(g, u) <= 0 {
            return Ok(());
        }
        if moves(g, u) > 0 && try_pillage(g, u, false)? && moves(g, u) <= 0 {
            return Ok(());
        }
    }
    for _ in 0..3 {
        if moves(g, u) <= 0 || g.unit(u).is_none() {
            return Ok(());
        }
        if !act_here(g, u)? {
            break;
        }
    }
    if g.unit(u).is_none() || moves(g, u) <= 0 {
        return Ok(());
    }
    if seek(g, u) {
        if g.unit(u).is_some() && moves(g, u) > 0 {
            act_here(g, u)?;
        }
        return Ok(());
    }
    wander(g, u)
}

/// Attacks or pillages within reach this turn, whichever is worth more (`_act_here`,
/// `barbarians.py:528-541`). Whether the unit did something.
fn act_here(g: &mut Game, u: UnitId) -> Result<bool, Refused> {
    let atk = best_attack(g, u, false);
    let pil = best_pillage(g, u, false);
    let pv = pil.map(|(v, _)| f64::from(v) * 15.0 * pillage_weight(g));
    let pillage_first = matches!((atk, pv), (Some(t), Some(pv)) if pv > t.score);
    let tries = if pillage_first { [false, true] } else { [true, false] };
    for attack in tries {
        if g.unit(u).is_none() || moves(g, u) <= 0 {
            continue;
        }
        if attack {
            if let Some(t) = atk
                && do_attack(g, u, t)?
            {
                return Ok(true);
            }
        } else if let Some((_, t)) = pil
            && do_pillage(g, u, t)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A captured civilian heads for the nearest camp it can reach, else wanders
/// (`_automate_civilian`, `barbarians.py:544-556`).
fn automate_civilian(g: &mut Game, u: UnitId) -> Result<(), Refused> {
    let Some(at) = g.unit(u).map(crate::state::units::Unit::tile) else { return Ok(()) };
    if is_camp_tile(g, at) {
        set_activity(g, u, Activity::Sleep);
        return Ok(());
    }
    let mut camps: Vec<TileIdx> =
        g.state().world().camps.values().filter(|c| !c.destroyed).map(|c| c.tile).collect();
    camps.sort_by_key(|&t| (g.grid().distance(at, t), t));
    for t in camps {
        if g.civilian_at(t).is_some() {
            continue;
        }
        if let Some(path) = movement::find_path(g, u, t, 40) {
            movement::follow(g, u, t, path, false, false);
            return Ok(());
        }
    }
    wander(g, u)
}

/// A possible attack: its score in hit points, the target, and the tile to attack from.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Target {
    score: f64,
    tile: TileIdx,
    from: TileIdx,
}

/// Every attack the unit could make this turn, scored in hit points (`_attack_targets`,
/// `barbarians.py:565-627`): the damage dealt less the damage taken, weighted by how much the
/// barbarians mind it; a civilian a free capture (150), a city with its defences down a sack
/// (1000, for a melee unit), a city under siege worth more for each other barbarian round it, a
/// kill twice its damage. Melee units may move next to a target first; a certain death for
/// nothing is left out unless they are bold.
fn attack_targets(g: &Game, u: UnitId) -> Vec<Target> {
    let Some(x) = g.unit(u) else { return Vec::new() };
    if resolve::can_attack_now(g, u).is_some() {
        return Vec::new();
    }
    let (here, hp, base) = (x.tile(), x.hp, x.base);
    let ranged = g.rules().base_units()[base].ranged;
    let range =
        if ranged { u32::try_from(units::health::attack_range(g, u)).unwrap_or(0) } else { 1 };
    let Some(m) = Mover::unit(g, u) else { return Vec::new() };
    let mut reach = m.reachable();
    if !reach.iter().any(|&(t, _)| t == here) {
        reach.push((here, x.moves));
        reach.sort_by_key(|&(t, _)| t);
    }
    let loss = loss_weight(g);
    let bold = aggr(g) >= 0.9;
    let a = crate::unique::Combatant::Unit(u);
    let mut out = Vec::new();
    for (from, left) in reach {
        if from != here && (left <= 0 || !m.can_stand(from)) {
            continue;
        }
        for t in g.grid().within(from, range) {
            if t == from || resolve::contains_attackable_enemy_from(g, t, a, from).is_some() {
                continue;
            }
            let Ok(pv) = resolve::preview_of_from(g, u, from, t) else { continue };
            let dd = f64::from(pv.damage_to_defender[0] + pv.damage_to_defender[1]) / 2.0;
            let da = f64::from(pv.damage_to_attacker[0] + pv.damage_to_attacker[1]) / 2.0;
            let score = if let Some(city) = g.city_at(t) {
                if recently_sacked(g, city.id()) {
                    // Picked clean: nothing left worth taking yet.
                    continue;
                }
                if pv.city_down {
                    if ranged {
                        continue;
                    }
                    1000.0
                } else {
                    #[allow(clippy::cast_precision_loss, reason = "a handful of units")]
                    let others = barbs_near(g, t, 2).saturating_sub(1) as f64;
                    dd - da * loss + 12.0 * others
                }
            } else {
                let civilian = g.military_at(t).is_none() && g.civilian_at(t).is_some();
                if civilian && !ranged {
                    150.0
                } else {
                    let kills = dd >= f64::from(pv.defender_hp);
                    if !kills && !bold && pv.damage_to_attacker[0] >= i32::from(hp) {
                        continue;
                    }
                    dd * if kills { 2.0 } else { 1.0 } - da * loss
                }
            };
            out.push(Target { score, tile: t, from });
        }
    }
    out
}

/// The attack the unit should make now, if any is worth it at this aggression (`_best_attack`,
/// `barbarians.py:630-652`): a sack or any ranged shot; otherwise better than the bar, and not a
/// healthy city before enough others have gathered round it. The best score, then the lowest
/// tile, then the lowest tile to attack from.
fn best_attack(g: &Game, u: UnitId, stay: bool) -> Option<Target> {
    let here = g.unit(u)?.tile();
    let mut targets = attack_targets(g, u);
    if stay {
        targets.retain(|t| t.from == here);
    }
    let ranged = g.unit(u).is_some_and(|x| g.rules().base_units()[x.base].ranged);
    let bar = attack_bar(g);
    let need = siege_size(g);
    targets
        .into_iter()
        .filter(|t| {
            if t.score >= 1000.0 || ranged {
                return true;
            }
            if t.score < 0.0 && g.city_at(t.tile).is_some() && barbs_near(g, t.tile, 2) < need {
                return false;
            }
            t.score > bar
        })
        .max_by(|a, b| {
            a.score.total_cmp(&b.score).then(b.tile.cmp(&a.tile)).then(b.from.cmp(&a.from))
        })
}

/// Moves to the tile to attack from if needed, then attacks (`_do_attack`). A move that finds no
/// path is refused, as Python's raised.
fn do_attack(g: &mut Game, u: UnitId, t: Target) -> Result<bool, Refused> {
    let here = g.unit(u).map(crate::state::units::Unit::tile);
    if here != Some(t.from) {
        movement::move_toward(g, u, t.from, false, false).map_err(|_| Refused)?;
        if g.unit(u).map(crate::state::units::Unit::tile) != Some(t.from) {
            return Ok(false);
        }
    }
    let Ok(d) = resolve::validate_attack(g, u, t.tile) else { return Ok(false) };
    resolve::attack(g, u, d);
    Ok(true)
}

/// Attacks something in reach if it is worth attacking (`_try_attack`).
fn try_attack(g: &mut Game, u: UnitId, stay: bool) -> Result<bool, Refused> {
    match best_attack(g, u, stay) {
        Some(t) => do_attack(g, u, t),
        None => Ok(false),
    }
}

/// How much the barbarians want to pillage a tile (`pillage_value`, `barbarians.py:677-690`): 3
/// for an improvement on a luxury or strategic resource, 2 for any other improvement, 1 for a
/// route, 0 for nothing they may pillage (their own land, a city, a camp, or nobody's).
#[must_use]
pub fn pillage_value(g: &Game, p: PlayerId, t: TileIdx) -> i32 {
    let Some(tile) = g.tile(t) else { return 0 };
    if tile.owner().is_none_or(|o| o == p) || g.city_at(t).is_some() || is_camp_tile(g, t) {
        return 0;
    }
    let Some(what) = workers::improvement_to_pillage(g, t) else { return 0 };
    if tile.improvement() == Some(what) {
        let kind = tile.resource().map(|res| g.rules().resources()[res].kind);
        return if matches!(kind, Some(ResourceType::Luxury | ResourceType::Strategic)) {
            3
        } else {
            2
        };
    }
    1
}

/// The most valuable thing to pillage this turn, nearest first among equals, then the lowest
/// tile (`_best_pillage`, `barbarians.py:693-713`): `(value, tile)`. Only a land unit that may
/// pillage and has moves; only where it stands with `only_here`.
fn best_pillage(g: &Game, u: UnitId, only_here: bool) -> Option<(i32, TileIdx)> {
    let x = g.unit(u)?;
    if g.rules().base_units()[x.base].domain != Domain::Land || x.moves <= 0 {
        return None;
    }
    if unit_has(g, u, UniqueType::CannotPillage, true) {
        return None;
    }
    let here = x.tile();
    let mut tiles: SmallVec<[TileIdx; 32]> = SmallVec::new();
    if !only_here {
        tiles.extend(
            movement::reachable_this_turn(g, u).into_iter().filter(|&(_, l)| l > 0).map(|(t, _)| t),
        );
    }
    tiles.push(here);
    let mut best: Option<((i32, i64, i64), TileIdx)> = None;
    for t in tiles {
        if t != here && g.military_at(t).is_some() {
            continue;
        }
        let v = pillage_value(g, x.owner(), t);
        if v <= 0 {
            continue;
        }
        let key = (v, -i64::from(g.grid().distance(here, t)), -i64::from(t.0));
        if best.is_none_or(|(k, _)| key > k) {
            best = Some((key, t));
        }
    }
    best.map(|((v, _, _), t)| (v, t))
}

/// Moves to a tile and pillages it, which heals the unit (`_do_pillage`).
fn do_pillage(g: &mut Game, u: UnitId, t: TileIdx) -> Result<bool, Refused> {
    if g.unit(u).map(crate::state::units::Unit::tile) != Some(t) {
        movement::move_toward(g, u, t, false, false).map_err(|_| Refused)?;
        if g.unit(u).map(crate::state::units::Unit::tile) != Some(t) {
            return Ok(false);
        }
    }
    if workers::can_pillage(g, u).is_some() {
        return Ok(false);
    }
    workers::pillage(g, u);
    Ok(true)
}

/// Pillages the most valuable thing here or within reach (`_try_pillage`).
fn try_pillage(g: &mut Game, u: UnitId, only_here: bool) -> Result<bool, Refused> {
    match best_pillage(g, u, only_here) {
        Some((_, t)) => do_pillage(g, u, t),
        None => Ok(false),
    }
}

/// How a target is approached: next to it, or onto its tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Approach {
    Adjacent,
    Onto,
}

/// Heads for the most tempting target within the search radius (`_seek`,
/// `barbarians.py:737-795`): a city to sack or besiege (more tempting with others round it), a
/// unit to fight (more so when wounded), a civilian to capture (for a melee unit), something to
/// pillage (for a land unit), each less 4 a tile. The five best are tried in turn, the best
/// first, then the lowest tile, and the unit follows the path found to the first it can reach
/// within `max(2, radius / 2 + 1)` turns. One [`PathTree`] answers every candidate. Whether it
/// moved.
fn seek(g: &mut Game, u: UnitId) -> bool {
    let radius = seek_radius(g);
    let Some((here, owner, base)) = g.unit(u).map(|x| (x.tile(), x.owner(), x.base)) else {
        return false;
    };
    if radius == 0 || moves(g, u) <= 0 {
        return false;
    }
    let a = aggr(g);
    let r = g.rules();
    let land = r.base_units()[base].domain == Domain::Land;
    let melee = r.base_units()[base].melee;
    let pw = pillage_weight(g);
    let mut cands: Vec<(f64, TileIdx, Approach)> = Vec::new();
    for t in g.grid().within(here, radius) {
        if t == here {
            continue;
        }
        let d = f64::from(g.grid().distance(here, t));
        if let Some(city) = g.city_at(t) {
            if city.owner() != owner && !recently_sacked(g, city.id()) {
                // Barbarians already round a city draw the others in: a siege.
                #[allow(clippy::cast_precision_loss, reason = "a handful of units")]
                let round = barbs_near(g, t, 2) as f64;
                cands.push((40.0 + 40.0 * a + 12.0 * round - 4.0 * d, t, Approach::Adjacent));
            }
            continue;
        }
        if let Some(mil) = g.military_at(t) {
            if mil.owner() != owner && r.base_units()[mil.base].domain != Domain::Air {
                let v = 15.0 + 30.0 * a + f64::from(100 - mil.hp) * 0.3;
                cands.push((v - 4.0 * d, t, Approach::Adjacent));
            }
            continue;
        }
        if let Some(civ) = g.civilian_at(t)
            && civ.owner() != owner
        {
            if melee {
                cands.push((70.0 + 20.0 * a - 4.0 * d, t, Approach::Onto));
            }
            continue;
        }
        if land {
            let pv = pillage_value(g, owner, t);
            if pv != 0 {
                cands.push((f64::from(pv) * 20.0 * pw - 4.0 * d, t, Approach::Onto));
            }
        }
    }
    if cands.is_empty() {
        return false;
    }
    cands.sort_by(|x, y| y.0.total_cmp(&x.0).then(x.1.cmp(&y.1)));
    let max_turns = (radius / 2 + 1).max(2);
    let chosen = {
        let Some(m) = Mover::unit(g, u) else { return false };
        let Some(tree) = PathTree::build(&m, max_turns) else { return false };
        let mut chosen = None;
        'cands: for &(_, t, how) in cands.iter().take(5) {
            let dests: SmallVec<[TileIdx; 2]> = match how {
                Approach::Onto => SmallVec::from_slice(&[t]),
                Approach::Adjacent => {
                    if g.grid().neighbors(t).any(|n| n == here) {
                        // Already there: nothing better to do than wait.
                        continue;
                    }
                    let mut ns: Vec<TileIdx> =
                        g.grid().neighbors(t).filter(|&n| m.can_stand(n)).collect();
                    ns.sort_by_key(|&n| (g.grid().distance(here, n), n));
                    ns.truncate(2);
                    ns.into_iter().collect()
                }
            };
            for dest in dests {
                if let Some(path) = tree.path_to(&m, dest) {
                    chosen = Some((dest, path));
                    break 'cands;
                }
            }
        }
        chosen
    };
    let Some((dest, path)) = chosen else { return false };
    movement::follow(g, u, dest, path, false, false);
    true
}

/// Moves to a random tile it can reach this turn (`_wander`, UnCiv's `UnitAutomation.wander`),
/// drawn from `Purpose::Wander` keyed by the unit and the turn.
fn wander(g: &mut Game, u: UnitId) -> Result<(), Refused> {
    let Some(here) = g.unit(u).map(crate::state::units::Unit::tile) else { return Ok(()) };
    let reach: Vec<TileIdx> = {
        let Some(m) = Mover::unit(g, u) else { return Ok(()) };
        m.reachable().into_iter().map(|(t, _)| t).filter(|&t| t != here && m.can_stand(t)).collect()
    };
    let mut rng = Rng::keyed(g.state().seed(), Purpose::Wander, &[u.key(), g.turn().key()]);
    let Some(&t) = rng.pick(&reach) else { return Ok(()) };
    movement::move_toward(g, u, t, false, false).map_err(|_| Refused)?;
    Ok(())
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests;

// ---- What the tools and scripts read -------------------------------------------------------------

/// The camps as scripts read them: `id`, `x`, `y`, `countdown`, `spawned` and `destroyed`, by id.
#[must_use]
pub fn camps_json(g: &Game) -> Value {
    Value::Array(
        g.state()
            .world()
            .camps
            .iter()
            .map(|(id, c)| {
                let (x, y) = g.xy(c.tile);
                json!({"id": id.get(), "x": x, "y": y, "countdown": c.countdown,
                       "spawned": c.spawned, "destroyed": c.destroyed})
            })
            .collect(),
    )
}
