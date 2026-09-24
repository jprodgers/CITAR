//! Founding a city and giving it buildings (`cities.py:1837-1869, 2073-2206`), as far as a city's
//! stats and citizens need one to exist: the scenario operations `found_city` and `set_city`
//! found and build them, and the rule scripts of package 1b-06 play on them.
//!
//! What waits for the packages that port the rest, each marked where it happens: the free
//! buildings a civilization's uniques give, the settler buildings of a later era (their
//! rejection rules) and a science building's research boost (1b-07), the one-time triggers of
//! founding and of a building (1b-08), and clearing a barbarian camp in the new borders (1c-06).
//! The unit action that founds a city is 1c-04's.

use super::super::Game;
use super::super::derive::rev::{CityTouch, PlayerTouch};
use super::super::error::{ActionError, ErrCode};
use super::super::{Porting, pending};
use super::stats::max_health;
use crate::base::ids::{BuildingId, CityId, PlayerId, TileIdx};
use crate::game::core::has_type;
use crate::rules::defs::NationKind;
use crate::rules::defs::ReligionProgress;
use crate::state::TileClaim;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::cities::City;
use crate::unique::{Ctx, UniqueType, uq};

/// Why a city cannot be founded on a tile, if it cannot (`cities.found_check`,
/// `cities.py:2073-2088`).
#[must_use]
pub fn found_check(g: &Game, p: PlayerId, t: TileIdx) -> Option<String> {
    let tile = g.tile(t)?;
    if g.is_water(t) || crate::game::tiles::is_impassable(g, t) {
        return Some(
            "Cities must be founded on land (not mountains, ice or natural wonders).".into(),
        );
    }
    if tile.owner().is_some_and(|o| o != p) {
        return Some("That tile belongs to another civilization.".into());
    }
    let k = &g.rules().constants().formulas;
    for c in g.state().cities().iter() {
        let same = g.continent(c.tile()) == g.continent(t);
        let md =
            if same { k.minimal_city_distance } else { k.minimal_city_distance_other_continents };
        let d = i64::from(g.grid().distance(c.tile(), t));
        if d <= i64::from(md) {
            return Some(format!(
                "Too close to {} (at least {md} tiles must lie between cities).",
                c.name
            ));
        }
    }
    let camp = g.rules().derived().known.barbarian_camp;
    if camp.is_some() && tile.improvement() == camp {
        return Some("Clear the barbarian camp first.".into());
    }
    None
}

/// A player's name for something, as plain text of a sane length (`cities.clean_name`,
/// `cities.py:2091-2098`): up to the first of `<>{}` or a line break, spaces collapsed, at most
/// `limit` characters, quotes and spaces trimmed from the ends.
#[must_use]
pub fn clean_name(name: &str, limit: usize) -> String {
    let first = name.split(['<', '>', '\n', '\r', '{', '}']).next().unwrap_or("");
    let joined = first.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut: String = joined.chars().take(limit).collect();
    cut.trim_matches([' ', '"', '\'']).to_owned()
}

/// Whether a name is taken by a city of the game, whatever its case.
fn taken(g: &Game, name: &str) -> bool {
    let lower = name.to_lowercase();
    g.state().cities().iter().any(|c| c.name.to_lowercase() == lower)
}

/// The name of a new city (`cities.new_city_name`, `cities.py:2101-2128`): the one asked for,
/// numbered if it is taken; else the next of the nation's list, another nation's for a
/// civilization that borrows them, a prefixed one; else `"<civilization> City <n>"`. The second
/// value says whether the civilization's city counter was used.
#[must_use]
pub fn new_city_name(g: &Game, p: PlayerId, asked: Option<&str>) -> (String, bool) {
    if let Some(asked) = asked {
        let name = clean_name(asked, 40);
        if !name.is_empty() {
            if !taken(g, &name) {
                return (name, false);
            }
            for i in 2..100 {
                let n = format!("{name} {i}");
                if !taken(g, &n) {
                    return (n, false);
                }
            }
        }
    }
    let r = g.rules();
    let Some(player) = g.player(p) else { return ("City".into(), false) };
    let own = &r.nations()[player.nation].cities;
    if let Some(n) = own.iter().find(|n| !taken(g, n)) {
        return (n.to_string(), false);
    }
    let v = g.view();
    if uq::any(uq::civ(&v, p, UniqueType::BorrowsCityNames, &Ctx::civ(p))) {
        for (_, q) in g.state().players().iter() {
            if let Some(n) = r.nations()[q.nation].cities.iter().find(|n| !taken(g, n)) {
                return (n.to_string(), false);
            }
        }
    }
    for prefix in ["New", "Neo", "Nova", "Altera"] {
        for n in own.iter() {
            let cand = format!("{prefix} {n}");
            if !taken(g, &cand) {
                return (cand, false);
            }
        }
    }
    (format!("{} City {}", player.name, player.city_counter + 1), true)
}

/// The building that marks a capital (`cities.capital_indicator`, `cities.py:2188-2193`): the
/// first `Indicates the capital city` building no nation has to itself, or this civilization's
/// replacement of it.
#[must_use]
pub fn capital_indicator(g: &Game, p: PlayerId) -> Option<BuildingId> {
    let r = g.rules();
    let base = r.buildings().iter().find(|(_, b)| {
        b.unique_to.is_none() && has_type(r, &b.uniques, UniqueType::IndicatesCapital)
    })?;
    Some(equivalent_building(g, p, base.0))
}

/// The building a civilization builds in place of `b` (`cities.equivalent_building`,
/// `cities.py:1144-1156`): its nation's unique replacement of it, if it has one.
#[must_use]
pub fn equivalent_building(g: &Game, p: PlayerId, b: BuildingId) -> BuildingId {
    let r = g.rules();
    let base = r.buildings()[b].replaces.unwrap_or(b);
    let Some(nation) = g.player(p).map(|x| x.nation) else { return base };
    r.buildings()
        .iter()
        .find(|(_, x)| x.replaces == Some(base) && x.unique_to == Some(nation))
        .map_or(base, |(id, _)| id)
}

/// Founds a city (`cities.found_city`, `cities.py:2131-2185`): its tile and the free tiles
/// around it claimed, its first citizens, a palace for a civilization with no capital, and the
/// announcement. Its citizens are placed when the call settles.
///
/// # Errors
/// [`ErrCode::Rule`] with [`found_check`]'s reason, or a state write refused.
pub fn found_city(
    g: &mut Game,
    p: PlayerId,
    t: TileIdx,
    name: Option<&str>,
) -> Result<CityId, ActionError> {
    if let Some(reason) = found_check(g, p, t) {
        return Err(ActionError::new(ErrCode::Rule, reason));
    }
    let refused =
        |e: crate::state::StateError| ActionError::rule(format!("The game refused ({e})."));
    let Some(player) = g.player(p) else { return Err(ActionError::rule("No such player.")) };
    let first = !player.founded_city && player.is_major();
    let first_of_cs = player.kind == NationKind::CityState && g.state().cities().of(p).is_empty();
    let (city_name, counted) = new_city_name(g, p, name);
    let id =
        g.st.ids_mut()
            .next_city()
            .ok_or_else(|| ActionError::rule("The game has run out of city ids."))?;
    let turn = g.turn();
    let mut city = City::new(id, city_name.clone().into(), p, t, turn);
    city.original_capital = first || first_of_cs;
    g.add_city(city).map_err(refused)?;
    if counted && let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
        x.city_counter += 1;
    }
    let r = g.rules();
    // Features a worker could remove go (cities.py:2143-2146).
    if let Some(tile) = g.tile(t) {
        let mut f = tile.features();
        for x in tile.features().iter() {
            if r.derived().removal_of.get(x).copied().flatten().is_some() {
                f.remove(x);
            }
        }
        if f != tile.features() {
            g.set_features(t, f).map_err(refused)?;
        }
    }
    g.set_improvement(t, r.derived().known.city_center).map_err(refused)?;
    if g.tile(t).is_some_and(crate::state::map::Tile::improvement_pillaged) {
        let route = g.tile(t).is_some_and(crate::state::map::Tile::route_pillaged);
        g.set_pillaged(t, route, false).map_err(refused)?;
    }
    g.set_tile_owner(t, TileClaim::city(p, id)).map_err(refused)?;
    for n in g.grid().neighbors(t).collect::<Vec<_>>() {
        let Some(nt) = g.tile(n) else { continue };
        if nt.city().is_some() || nt.owner().is_some_and(|o| o != p) {
            continue;
        }
        // barbarians.remove_camp on a camp in the new borders (cities.py:2156-2158).
        pending(Porting::Pending("1c-06"));
        g.set_tile_owner(n, TileClaim::city(p, id)).map_err(refused)?;
    }
    let era = &r.eras()[g.state().config().starting_era];
    let pop = u16::try_from(era.settler_population.max(1)).unwrap_or(1);
    let pantheon = g
        .player(p)
        .filter(|x| x.religion.progress == ReligionProgress::Pantheon)
        .and_then(|x| x.religion.founded);
    if let Some(x) = g.city_mut(id, CityTouch::CORE) {
        x.pop = pop;
        x.health = 200;
    }
    if let Some(rel) = pantheon
        && let Some(x) = g.city_mut(id, CityTouch::RELIGION)
    {
        let add = 200 * i32::from(pop);
        match x.pressures.iter_mut().find(|(k, _)| *k == Some(rel)) {
            Some((_, v)) => *v += add,
            None => {
                x.pressures.push((Some(rel), add));
                x.pressures.sort_by_key(|&(k, _)| k);
            }
        }
    }
    let capital = g.player(p).and_then(|x| x.capital);
    let needs_capital =
        g.state().cities().of(p).len() == 1 || capital.and_then(|c| g.city(c)).is_none();
    if needs_capital {
        if let Some(b) = capital_indicator(g, p) {
            add_building(g, id, b, false);
        }
        if let Some(x) = g.player_mut(p, PlayerTouch::CAPITAL) {
            x.capital = Some(id);
            if first {
                x.original_capital = Some(id);
            }
        }
    }
    // The era's settler buildings, each unless the city could not build it (rejection_reasons).
    pending(Porting::Pending("1b-07"));
    if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
        x.founded_city = true;
    }
    // try_add_free_buildings (cities.py:2178).
    pending(Porting::Pending("1b-07"));
    // triggers.fire(UponFoundingCity) (cities.py:2179-2180).
    pending(Porting::Pending("1b-08"));
    let who = g.player(p).map(|x| x.name.clone()).unwrap_or_default();
    let at = g.fmt_xy(t);
    g.emit(
        EngineEvent::CityFounded,
        &format!("{who} founded {city_name} at {at}."),
        Some(crate::base::sets::PlayerSet::single(p)),
        Some(t),
        EventData { city: Some(id), ..EventData::default() },
        &[],
    );
    Ok(id)
}

/// Adds a building to a city (`cities.add_building`, `cities.py:1837-1862`): its health grows
/// with it, and a capital's marker makes it the capital. Its citizens are placed again when the
/// call settles.
pub fn add_building(g: &mut Game, c: CityId, b: BuildingId, try_free: bool) {
    let r = g.rules();
    let Some(city) = g.city(c) else { return };
    let owner = city.owner();
    let bd = &r.buildings()[b];
    if bd.city_health != 0 {
        let mx = max_health(g, c).max(1);
        let health = city.health;
        let grow = crate::base::num::trunc_i32(
            f64::from(bd.city_health) * f64::from(health) / f64::from(mx),
        );
        if let Some(x) = g.city_mut(c, CityTouch::CORE) {
            x.health += grow;
        }
    }
    if let Some(x) = g.city_mut(c, CityTouch::BUILDINGS) {
        x.buildings.insert(b);
    }
    if has_type(r, &bd.uniques, UniqueType::IndicatesCapital)
        && let Some(x) = g.player_mut(owner, PlayerTouch::CAPITAL)
    {
        x.capital = Some(c);
    }
    // The building's one-time triggers (cities.py:1849-1854).
    pending(Porting::Pending("1b-08"));
    // A science building in the capital with `TechBoostWhenScientificBuildingsBuiltInCapital`
    // (cities.py:1856-1858).
    pending(Porting::Pending("1b-07"));
    if try_free {
        // try_add_free_buildings (cities.py:1860-1861).
        pending(Porting::Pending("1b-07"));
    }
}

/// Takes a building out of a city (`cities.remove_building`, `cities.py:1865-1869`).
pub fn remove_building(g: &mut Game, c: CityId, b: BuildingId) {
    if g.city(c).is_some_and(|x| x.buildings.contains(b))
        && let Some(x) = g.city_mut(c, CityTouch::BUILDINGS)
    {
        x.buildings.remove(b);
    }
}

/// Renames a city (`cities.rename_city`, `cities.py:2196-2205`), after cleaning the name.
///
/// # Errors
/// A name with no letters, or one another city has.
pub fn rename_city(g: &mut Game, c: CityId, name: &str) -> Result<String, ActionError> {
    let name = clean_name(name, 40);
    if name.is_empty() {
        return Err(ActionError::new(ErrCode::BadParam, "City names must contain letters."));
    }
    let lower = name.to_lowercase();
    if g.state().cities().iter().any(|x| x.id() != c && x.name.to_lowercase() == lower) {
        return Err(ActionError::new(
            ErrCode::BadParam,
            format!("There is already a city called {name}."),
        ));
    }
    if let Some(x) = g.city_mut(c, CityTouch::NAME) {
        x.name = name.clone().into();
    }
    Ok(name)
}
