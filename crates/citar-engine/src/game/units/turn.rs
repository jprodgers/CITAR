//! A unit's start and end of turn (`units.start_turn` and `end_turn`, `units.py:679-751`,
//! UnCiv's `UnitTurnManager`), and the stages that run them for a player's units (DESIGN.md 6.2:
//! S0 and E0 for the barbarians, S6 and E6 for everyone else).

use super::health::{heal_amount_here, heal_by, max_moves, take_damage, terrain_damage};
use super::promotions::unit_label;
use super::{remove_unit, type_has, unit_has};
use crate::base::ids::{PlayerId, UnitId};
use crate::game::Game;
use crate::game::derive::rev::UnitTouch;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::map::Tile;
use crate::state::units::Activity;
use crate::unique::{UniqueData, UniqueType};

/// Every unit of `p` starts its turn, in id order (stages S0 and S6). A unit a turn's start
/// removed is skipped.
pub fn start_units(g: &mut Game, p: PlayerId) {
    let ids: Vec<UnitId> = g.state().units().of(p).to_vec();
    for u in ids {
        if g.unit(u).is_some() {
            start_turn(g, u);
        }
    }
}

/// Every unit of `p` ends its turn, in id order (stages E0 and E6).
pub fn end_units(g: &mut Game, p: PlayerId) {
    let ids: Vec<UnitId> = g.state().units().of(p).to_vec();
    for u in ids {
        if g.unit(u).is_some() {
            end_turn(g, u);
        }
    }
}

/// A unit starts its owner's turn (`units.start_turn`, `units.py:679-702`): full moves, no
/// attacks, interceptions or action yet; a sleeping unit wakes when it sees an enemy military
/// unit within three tiles, a healing one when it is whole; and a unit in land it may no longer
/// be in (a treaty ended) goes to the nearest tile it may be on.
pub fn start_turn(g: &mut Game, u: UnitId) {
    let full = max_moves(g, u);
    let Some(x) = g.unit_mut(u, UnitTouch::CORE | UnitTouch::MOVES) else { return };
    x.moves = full;
    x.attacks = 0;
    x.interceptions = 0;
    x.acted = false;
    let (owner, tile, activity, hp) = (x.owner(), x.tile(), x.activity, x.hp);
    let mut activity = activity;
    if activity == Some(Activity::Sleep) {
        // Python's visible_tiles brought sight up to date first.
        if g.pending.any_sight() {
            g.settle_sight();
        }
        let near = g.grid().within(tile, 3).into_iter().any(|n| {
            g.military_at(n)
                .is_some_and(|m| g.derived().vis().sees(owner, n) && g.at_war(owner, m.owner()))
        });
        if near {
            activity = None;
            set_activity(g, u, None);
            woke(g, u, owner, "woke up: enemy nearby.");
        }
    }
    let healing =
        matches!(activity, Some(Activity::Heal | Activity::FortifyHeal | Activity::SleepHeal));
    if healing && hp >= 100 {
        set_activity(g, u, None);
        woke(g, u, owner, "has fully healed.");
    }
    let Some(tile) = g.unit(u).map(crate::state::units::Unit::tile) else { return };
    if let Some(o) = g.tile(tile).and_then(Tile::owner)
        && o != owner
        && !g.can_enter_territory(owner, tile)
        && !unit_has(g, u, UniqueType::CanEnterForeignTiles, false)
        && !g.is_city_state(o)
    {
        crate::game::movement::teleport_to_closest(g, u);
    }
}

fn set_activity(g: &mut Game, u: UnitId, a: Option<Activity>) {
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
        x.activity = a;
    }
}

/// `unit_woke`, to the unit's owner: "Warrior #3 woke up: enemy nearby."
fn woke(g: &mut Game, u: UnitId, owner: PlayerId, what: &str) {
    let text = format!("{} {what}", unit_label(g, u));
    let tile = g.unit(u).map(crate::state::units::Unit::tile);
    let data = EventData { unit: Some(u), ..EventData::default() };
    g.emit(EngineEvent::UnitWoke, &text, Some(core::iter::once(owner).collect()), tile, data, &[]);
}

/// A unit ends its owner's turn (`units.end_turn`, `units.py:705-751`): a fortified unit that
/// stayed put fortifies further, up to two turns; a unit that neither moved nor attacked (or heals
/// even after acting) heals; a religious unit in land it has no right to be in loses strength and
/// may lose its faith; an enemy's citadel next to it, and its tile's terrain, damage it.
pub fn end_turn(g: &mut Game, u: UnitId) {
    let full = max_moves(g, u);
    let heals_anyway = unit_has(g, u, UniqueType::HealsEvenAfterAction, false);
    let Some(x) = g.unit_mut(u, UnitTouch::CORE) else { return };
    let moved = x.moves < full || x.acted;
    let fortifying = matches!(x.activity, Some(Activity::Fortify | Activity::FortifyHeal));
    if !moved && fortifying && x.fortify < 2 {
        x.fortify += 1;
    }
    if !fortifying {
        x.fortify = 0;
    }
    let (owner, base, tile, attacks) = (x.owner(), x.base, x.tile(), x.attacks);
    if (!moved && attacks == 0) || heals_anyway {
        let amount = heal_amount_here(g, u);
        if amount > 0 {
            heal_by(g, u, amount);
        }
    }
    let r = g.rules();
    let tile_owner = g.tile(tile).and_then(Tile::owner);
    if type_has(g, base, UniqueType::ReligiousUnit)
        && let Some(o) = tile_owner
        && !g.is_city_state(o)
        && !g.can_enter_territory(owner, tile)
    {
        let lost = super::unit_uniques(
            g,
            u,
            UniqueType::CanEnterForeignTilesButLosesReligiousStrength,
            false,
        )
        .into_iter()
        .filter_map(|(id, _)| match r.uniques().get(id).data {
            UniqueData::CanEnterForeignTilesButLosesReligiousStrength(y) => Some(y.loss),
            _ => None,
        })
        .min();
        let strength = r.base_units()[base].religious_strength;
        let mut gone = false;
        if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
            if let Some(l) = lost {
                x.religious_strength_lost =
                    x.religious_strength_lost.saturating_add(i16::try_from(l).unwrap_or(i16::MAX));
            }
            gone = strength > 0 && i32::from(x.religious_strength_lost) >= strength;
        }
        if gone {
            let what = r.name(base).unwrap_or("");
            let text =
                format!("Your {what} lost its faith after spending too long in foreign territory.");
            remove_unit(g, u);
            let to = core::iter::once(owner).collect();
            g.emit(EngineEvent::UnitKilled, &text, Some(to), Some(tile), EventData::default(), &[]);
            return;
        }
    }
    // An enemy's citadel next to it: the one that does the most damage.
    let mut best: Option<(crate::base::ids::TileIdx, i32)> = None;
    for n in g.grid().neighbors(tile) {
        let Some(nt) = g.tile(n) else { continue };
        let imp = nt.improvement().filter(|_| !nt.improvement_pillaged());
        let (Some(o), Some(imp)) = (nt.owner(), imp) else { continue };
        if !g.at_war(owner, o) {
            continue;
        }
        let t = r.uniques();
        let d: i32 = r.improvements()[imp]
            .uniques
            .ids()
            .filter_map(|id| match t.get(id).data {
                UniqueData::DamagesAdjacentEnemyUnits(y) => Some(y.damage),
                _ => None,
            })
            .sum();
        if d > best.map_or(0, |b| b.1) {
            best = Some((n, d));
        }
    }
    if let Some((at, dmg)) = best
        && take_damage(g, u, dmg, None)
    {
        let imp = g.tile(at).and_then(|t| t.improvement()).and_then(|i| r.name(i)).unwrap_or("");
        let who = g.player(owner).map(|p| p.name.to_string()).unwrap_or_default();
        let what = r.name(base).unwrap_or("");
        let text = format!("An enemy {imp} destroyed {who}'s {what}.");
        let mut to: crate::base::sets::PlayerSet = core::iter::once(owner).collect();
        to.extend(g.tile(at).and_then(Tile::owner));
        g.emit(EngineEvent::UnitKilled, &text, Some(to), Some(tile), EventData::default(), &[]);
        return;
    }
    let td = terrain_damage(g, tile);
    if td != 0
        && g.unit(u).is_some()
        && !unit_has(g, u, UniqueType::CanPassImpassable, false)
        && take_damage(g, u, td, None)
    {
        let who = g.player(owner).map(|p| p.name.to_string()).unwrap_or_default();
        let what = r.name(base).unwrap_or("");
        let text = format!("{who}'s {what} took {td} terrain damage and was destroyed.");
        let to = core::iter::once(owner).collect();
        g.emit(EngineEvent::UnitKilled, &text, Some(to), Some(tile), EventData::default(), &[]);
    }
}
