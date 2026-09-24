//! Units and cities, keyed by their ids as text (`state.py:110-190`).
//!
//! Units keep their ids. `build` and `due_heal`, which nothing read, are dropped; `status`, whose
//! one value was `"Set Up"`, becomes `set_up`; the limited-ability counts keep their keys, which
//! the compiler interned (`units.py:417`). Cities keep their ids too; their religious pressures,
//! which Python seeded on first read (`religion.py:105-109`), are seeded if Python had not read
//! them yet, and `spaceship_parts`, never read, is dropped.

use smallvec::SmallVec;

use super::read::{Obj, Path, Res, dict, int, key_int, list, real, text};
use super::{Cx, Drop, dead};
use crate::base::ids::{AbilityKey, CampId, CityId, ReligionId, SpecialistId, TileIdx, UnitId};
use crate::base::sets::MAX_SPECIALISTS;
use crate::state::cities::{City, CityFocus, NO_RELIGION_PRESSURE};
use crate::state::units::{Activity, Unit, Units};

/// A unit read, and the unit carrying it, linked once every unit is read.
pub(super) struct PendingUnit {
    pub(super) unit: Unit,
    pub(super) carried_by: Option<UnitId>,
}

/// Every unit, in id order.
pub(super) fn units(cx: &mut Cx<'_>, top: &Obj<'_>) -> Res<Vec<PendingUnit>> {
    let at = top.at("units");
    let mut out = dict(top.req("units")?, &at, |k, v, p| {
        let id: u32 = key_int(k, p)?;
        unit(cx, id, v, p)
    })?;
    out.sort_by_key(|u| u.unit.id());
    Ok(out)
}

/// One unit, whose key said `id`.
fn unit(cx: &mut Cx<'_>, id: u32, v: &serde_json::Value, p: &Path<'_>) -> Res<PendingUnit> {
    let o = Obj::new(v, *p)?;
    let own: u32 = o.int_req("id")?;
    if own != id {
        return Err(o.at("id").err(format!("unit {own} is filed under {id}")));
    }
    let id = UnitId::new(id).ok_or_else(|| o.at("id").err("0 is not a unit id"))?;
    let base = cx.named(o.req("type")?, &o.at("type"))?;
    let owner = cx.player(o.req("owner")?, &o.at("owner"))?;
    let tile = cx.tile(o.req("idx")?, &o.at("idx"))?;
    let created: i32 = o.int("created_turn", 0)?;
    let mut u = Unit::new(id, base, owner, tile, created);
    u.hp = o.int("hp", 100)?;
    u.moves = o.int("moves", 0)?;
    u.xp = o.int("xp", 0)?;
    if let Some(v) = o.get("promotions") {
        u.promotions = cx.id_set(list(v, &o.at("promotions"))?, &o.at("promotions"))?;
    }
    u.promotion_count = o.int("promotion_count", 0)?;
    u.pending_promotions = o.int("pending_promotions", 0)?;
    u.fortify = o.int("fortify", 0)?;
    u.activity = match o.opt_text("activity")? {
        None => None,
        Some(a) => Some(
            Activity::from_name(a)
                .ok_or_else(|| o.at("activity").err(format!("no activity {a:?}")))?,
        ),
    };
    u.goto = cx.opt_tile(o.get("goto"), &o.at("goto"))?;
    u.path = o.each("path", |v, p| cx.tile(v, p))?;
    u.order_wait = o.int("order_wait", 0)?;
    u.attacks = o.int("attacks", 0)?;
    u.interceptions = o.int("interceptions", 0)?;
    u.acted = o.flag("acted", false)?;
    for (i, s) in o.each("status", |v, p| text(v, p))?.into_iter().enumerate() {
        if s != "Set Up" || u.set_up {
            return Err(o.at("status").index(i).err(format!("no status {s:?}, or listed twice")));
        }
        u.set_up = true;
    }
    u.name = o.opt_text("name")?.map(Into::into);
    u.camp = match o.opt_int::<u32>("camp")? {
        None => None,
        Some(c) => Some(CampId::new(c).ok_or_else(|| o.at("camp").err("0 is not a camp id"))?),
    };
    u.religion = cx.opt_religion(o.get("religion"), &o.at("religion"))?;
    u.religious_strength = o.int("religious_strength", 0)?;
    u.religious_strength_lost = o.int("religious_strength_lost", 0)?;
    let mut used: Vec<(AbilityKey, u8)> =
        o.entries("abilities_used", |k, v, p| Ok((cx.id_of::<AbilityKey>(k, p)?, int(v, p)?)))?;
    let keyed: Vec<AbilityKey> = used.iter().map(|&(k, _)| k).collect();
    used.sort_by_key(|&(k, _)| k);
    if used.windows(2).any(|w| w[0].0 == w[1].0) {
        return Err(o.at("abilities_used").err("an ability is counted twice"));
    }
    if !keyed.is_sorted() {
        cx.report.note(Drop::ListOrder);
    }
    u.abilities_used = used.into_iter().collect();
    u.origin_city = cx.opt_city(o.get("origin_city"), &o.at("origin_city"))?;
    u.original_owner = cx.opt_player(o.get("original_owner"), &o.at("original_owner"))?;
    u.return_offer = cx.opt_player(o.get("return_offer"), &o.at("return_offer"))?;
    let carried_by = match o.opt_int::<u32>("carried_by")? {
        None => None,
        Some(c) => {
            Some(UnitId::new(c).ok_or_else(|| o.at("carried_by").err("0 is not a unit id"))?)
        }
    };
    if !super::read::is_none(o.get("build")) {
        cx.report.note(Drop::UnitBuild);
    }
    if o.flag("due_heal", false)? {
        cx.report.note(Drop::UnitDueHeal);
    }
    o.finish()?;
    Ok(PendingUnit { unit: u, carried_by })
}

/// The unit store, with the carriers linked.
pub(super) fn build_units(pending: Vec<PendingUnit>, tiles: u32, p: &Path<'_>) -> Res<Units> {
    let units = pending.into_iter().map(|x| x.unit.with_carrier(x.carried_by));
    Units::from_units(units, tiles).map_err(|e| p.field("units").err(e.to_string()))
}

/// Every city, in id order.
pub(super) fn cities(cx: &mut Cx<'_>, top: &Obj<'_>) -> Res<Vec<City>> {
    let at = top.at("cities");
    let mut out = dict(top.req("cities")?, &at, |k, v, p| {
        let id: u32 = key_int(k, p)?;
        city(cx, id, v, p)
    })?;
    out.sort_by_key(City::id);
    Ok(out)
}

/// One city, whose key said `id`.
fn city(cx: &mut Cx<'_>, id: u32, v: &serde_json::Value, p: &Path<'_>) -> Res<City> {
    let o = Obj::new(v, *p)?;
    let own: u32 = o.int_req("id")?;
    if own != id {
        return Err(o.at("id").err(format!("city {own} is filed under {id}")));
    }
    let id = CityId::new(id).ok_or_else(|| o.at("id").err("0 is not a city id"))?;
    let owner = cx.player(o.req("owner")?, &o.at("owner"))?;
    let tile = cx.tile(o.req("idx")?, &o.at("idx"))?;
    let founded: i32 = o.int("founded_turn", 0)?;
    let mut c = City::new(id, o.text_req("name")?.into(), owner, tile, founded);
    c.founder = match o.get("founder") {
        None => owner,
        Some(v) => cx.player(v, &o.at("founder"))?,
    };
    c.previous_owner = cx.opt_player(o.get("previous_owner"), &o.at("previous_owner"))?;
    c.turn_acquired = o.int("turn_acquired", 0)?;
    c.original_capital = o.flag("original_capital", false)?;
    c.pop = o.int("pop", 1)?;
    c.food = o.real("food", 0.0)?;
    c.culture = o.real("culture", 0.0)?;
    c.tiles_claimed = o.int("tiles_claimed", 0)?;
    c.tiles_bought = o.int("tiles_bought", 0)?;
    if let Some(v) = o.get("buildings") {
        c.buildings = cx.id_set(list(v, &o.at("buildings"))?, &o.at("buildings"))?;
    }
    if let Some(v) = o.get("free_buildings") {
        c.free_buildings = cx.id_set(list(v, &o.at("free_buildings"))?, &o.at("free_buildings"))?;
    }
    c.queue = o
        .each("queue", |v, p| cx.constructible(text(v, p)?, p))?
        .into_iter()
        .collect::<SmallVec<_>>();
    let progress = o.entries("progress", |k, v, p| Ok((cx.constructible(k, p)?, real(v, p)?)))?;
    for (item, amount) in progress {
        if c.progress.insert(item, amount).is_some() {
            return Err(o.at("progress").err("an item is counted twice"));
        }
    }
    c.overflow = o.real("overflow", 0.0)?;
    c.bought_this_turn = o
        .each("bought_this_turn", |v, p| cx.constructible(text(v, p)?, p))?
        .into_iter()
        .collect::<SmallVec<_>>();
    c.auto_production = o.flag("auto_production", false)?;
    c.worked = tile_list(cx, &o, "worked")?;
    c.locked = tile_list(cx, &o, "locked")?;
    let specialists = o.entries("specialists", |k, v, p| {
        let s: SpecialistId = cx.id_of(k, p)?;
        if usize::from(s.0) >= MAX_SPECIALISTS {
            return Err(p.err("a specialist past the eight kept"));
        }
        Ok((usize::from(s.0), int::<u8>(v, p)?))
    })?;
    for (s, n) in specialists {
        c.specialists[s] = n;
    }
    c.manual_specialists = o.flag("manual_specialists", false)?;
    let focus = o.text("focus", "balanced")?;
    c.focus = CityFocus::from_name(focus)
        .ok_or_else(|| o.at("focus").err(format!("no city focus {focus:?}")))?;
    c.avoid_growth = o.flag("avoid_growth", false)?;
    c.health = o.int("health", 200)?;
    c.damaged_turn = o.int("damaged_turn", -1)?;
    c.attacked = o.flag("attacked", false)?;
    c.sacked_turn = o.int("sacked_turn", -1000)?;
    c.puppet = o.flag("puppet", false)?;
    c.resistance = o.int("resistance", 0)?;
    c.razing = o.flag("razing", false)?;
    let mut pressures: Vec<(Option<ReligionId>, i32)> = o.entries("pressures", |k, v, p| {
        let r = if k == "None" { None } else { Some(cx.religion_named(k, p)?) };
        Ok((r, int(v, p)?))
    })?;
    if pressures.is_empty() {
        // Python seeded this on the first read of the city's religion; nothing had read it yet.
        pressures.push((None, NO_RELIGION_PRESSURE));
    }
    pressures.sort_by_key(|&(r, _)| r);
    if pressures.windows(2).any(|w| w[0].0 == w[1].0) {
        return Err(o.at("pressures").err("a religion is listed twice"));
    }
    c.pressures = pressures.into_iter().collect();
    // In adoption order, which is not a set's: kept as Python had it.
    c.religions_adopted =
        o.each("religions_adopted", |v, p| cx.religion(v, p))?.into_iter().collect();
    c.holy_city_of = cx.opt_religion(o.get("holy_city_of"), &o.at("holy_city_of"))?;
    c.wltkd = o.int("wltkd", 0)?;
    c.demanded_resource = cx.opt_named(o.get("demanded_resource"), &o.at("demanded_resource"))?;
    c.demand_countdown = o.int("demand_countdown", 0)?;
    dead(cx, &o, "spaceship_parts", Drop::CitySpaceshipParts)?;
    o.finish()?;
    Ok(c)
}

/// A list of tiles kept sorted; its order is dropped, and counted if it was not ascending.
fn tile_list(cx: &mut Cx<'_>, o: &Obj<'_>, key: &str) -> Res<Vec<TileIdx>> {
    let tiles = o.each(key, |v, p| cx.tile(v, p))?;
    let mut sorted = tiles.clone();
    sorted.sort();
    if sorted.windows(2).any(|w| w[0] == w[1]) {
        return Err(o.at(key).err("a tile is listed twice"));
    }
    if sorted != tiles {
        cx.report.note(Drop::ListOrder);
    }
    Ok(sorted)
}
