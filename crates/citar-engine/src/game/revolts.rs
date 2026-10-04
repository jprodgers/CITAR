//! Revolts: a deeply unhappy civilization's rebels (`turns._update_revolts` and `_spawn_revolt`,
//! `turns.py:134-187`; UnCiv's `TurnManager.updateRevolts` and `doRevoltSpawn`), stage S3 for
//! major civilizations (package 1c-08).
//!
//! While a civilization holds `Rebel units may spawn` (the shipped ruleset grants it under deep
//! unhappiness) and the game has barbarians, a countdown runs: set when it starts, from the
//! ruleset's `base_turns_until_revolt` plus up to two turns (`Purpose::RevoltDelay`, keyed by
//! the civilization and the turn), scaled by a slower speed; when it runs out, rebels appear by
//! one of its cities (`Purpose::Revolt`, keyed alike). The countdown is dropped whenever the
//! unique no longer holds.

use crate::base::ids::{BaseUnitId, CityId, PlayerId, TileIdx};
use crate::base::num;
use crate::base::rng::{KeyPart, Purpose, Rng};
use crate::base::sets::PlayerSet;
use crate::game::Game;
use crate::game::combat::strength::tile_defense_bonus;
use crate::game::derive::rev::PlayerTouch;
use crate::game::diplomacy::relations::civ_has;
use crate::game::economy::city_tiles;
use crate::game::tiles::is_impassable;
use crate::game::units::{place_unit_near, type_has};
use crate::rules::defs::Domain;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::unique::UniqueType;

/// Stage S3, revolts (`_update_revolts`, `turns.py:134-150`).
pub(crate) fn update_revolts(g: &mut Game, p: PlayerId) {
    let barbarians = g.barbarian_id();
    let revolt_in = g.player(p).and_then(|x| x.civ.revolt_in);
    let rebels = barbarians.filter(|_| civ_has(g, p, UniqueType::SpawnRebels));
    let Some(barb) = rebels else {
        if revolt_in.is_some()
            && let Some(x) = g.player_mut(p, PlayerTouch::OTHER)
        {
            x.civ.revolt_in = None;
        }
        return;
    };
    let next = match revolt_in {
        None => {
            let mut rng = Rng::keyed(g.state().seed(), Purpose::RevoltDelay, &keys(g, p));
            let base = g.rules().constants().formulas.base_turns_until_revolt;
            #[allow(clippy::cast_possible_wrap, reason = "a draw below 3")]
            let turns = base.saturating_add(rng.below(3) as i32);
            let scaled = num::trunc_i32(f64::from(turns) * g.speed().modifier.max(1.0));
            Some(i16::try_from(scaled.max(1)).unwrap_or(i16::MAX))
        }
        Some(n) => {
            // A loaded save may hold any count; one at 0 or below runs out now.
            let left = n.saturating_sub(1);
            (left > 0).then_some(left)
        }
    };
    if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
        x.civ.revolt_in = next;
    }
    if revolt_in.is_some() && next.is_none() {
        spawn_revolt(g, p, barb);
    }
}

/// The draw's keys: the civilization and the turn (`state_rng("revolt", pid, g.turn)`).
fn keys(g: &Game, p: PlayerId) -> [u64; 2] {
    [p.key(), g.turn().key()]
}

/// How well rebels stand on a tile of the city (`rate`, `turns.py:166-176`): -1 where they
/// cannot (water, a unit or a city there, impassable), else 10, more on unimproved ground (and
/// more again with a resource) and on ground that helps a defender.
fn rate(g: &Game, t: TileIdx) -> i32 {
    let Some(tile) = g.tile(t) else { return -1 };
    if g.is_water(t) || g.units_at(t).next().is_some() || g.city_at(t).is_some() {
        return -1;
    }
    if is_impassable(g, t) {
        return -1;
    }
    let mut s = 10;
    if tile.improvement().is_none() {
        s += 4 + if tile.resource().is_some() { 3 } else { 0 };
    }
    if tile_defense_bonus(g, t, None) > 0.0 {
        s += 4;
    }
    s
}

/// The land melee units rebels may be (`turns.py:179-181`), in the ruleset's order: no nation's
/// own, able to attack, and of the rebellious civilization's techs but not made obsolete by them.
fn rebel_units(g: &Game, p: PlayerId) -> Vec<BaseUnitId> {
    g.rules()
        .base_units()
        .iter()
        .filter(|(id, d)| {
            d.unique_to.is_none()
                && d.melee
                && d.domain == Domain::Land
                && !type_has(g, *id, UniqueType::CannotAttack)
                && g.has_tech(p, d.required_tech)
                && !d.obsolete_tech.is_some_and(|t| g.has_tech(p, Some(t)))
        })
        .map(|(id, _)| id)
        .collect()
}

/// Rebels appear in a deeply unhappy civilization (`_spawn_revolt`, `turns.py:153-187`): one,
/// or more the more cities it has, all of one land melee unit type drawn among those it could
/// field, beside a city drawn with its larger cities likelier, on the tile of that city where
/// rebels stand best (the first of the best, in the order of the city's tiles).
fn spawn_revolt(g: &mut Game, p: PlayerId, barb: PlayerId) {
    let mut rng = Rng::keyed(g.state().seed(), Purpose::Revolt, &keys(g, p));
    let cities: Vec<(CityId, u16)> = g.player_cities(p).map(|c| (c.id(), c.pop)).collect();
    if cities.is_empty() {
        return;
    }
    let n = u64::try_from(cities.len()).unwrap_or(u64::MAX);
    let count = 1 + rng.below(100 + 20 * (n - 1)) / 100;
    // Python's max: every city draws, and the first of the highest wins.
    let mut city: Option<(u64, CityId)> = None;
    for &(c, pop) in &cities {
        let d = rng.below(u64::from(pop) + 10);
        if city.is_none_or(|(best, _)| d > best) {
            city = Some((d, c));
        }
    }
    let Some((_, city)) = city else { return };
    let mut tile: Option<(i32, TileIdx)> = None;
    for t in city_tiles(g, city) {
        let r = rate(g, t);
        if tile.is_none_or(|(best, _)| r > best) {
            tile = Some((r, t));
        }
    }
    let Some((_, tile)) = tile else { return };
    let options = rebel_units(g, p);
    let mut unit: Option<(u64, BaseUnitId)> = None;
    for &u in &options {
        let d = rng.below(1000);
        if unit.is_none_or(|(best, _)| d > best) {
            unit = Some((d, u));
        }
    }
    let Some((_, unit)) = unit else { return };
    for _ in 0..count {
        let placed = place_unit_near(g, barb, unit, tile);
        if placed.is_none() {
            break;
        }
    }
    g.emit(
        EngineEvent::Revolt,
        "Your citizens are revolting due to very high unhappiness!",
        Some(PlayerSet::single(p)),
        Some(tile),
        EventData::default(),
        &[],
    );
}
