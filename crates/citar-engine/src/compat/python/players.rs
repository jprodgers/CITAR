//! Players, and the 28 keys of `Player.flags` as typed fields (`state.py:193-312`,
//! DESIGN.md 4.5).
//!
//! The flags move as the state area's table has them: `start` to `start_tile`, the last-eight
//! histories and totals to `econ`, `ra_science` to `tech.ra_bonus`, the great-person pools and
//! the Maya calendar to `gp`, the free beliefs to `religion`, `revolt_in`, `last_ruins` and
//! `skip_explore` to `civ`, the explorers' targets and recent tiles to their units, the
//! city-states' pairs, quest timers, war quests, election and cooldowns to `city_state`, and
//! `eras_spy_earned`, `cs_attacks` and `cs_gp_gift` to `major`. `gained_<unit>` becomes
//! `civ.units_gained`, and `last_stats` and `unreachable_explore` are dropped.
//! `Player.free_buildings` moves to the cities, `GameState.spaceship` to the majors, and `met` to
//! the relations.

use std::collections::BTreeMap;

use serde_json::Value;

use super::entities::PendingUnit;
use super::read::{Obj, Path, Res, dict, flag, int, key_int, list, real, text};
use super::{Cx, Dropped, dead};
use crate::base::codec::b64_decode;
use crate::base::ids::{
    BaseUnitId, BuildingId, CityId, EraId, NationId, PlayerId, QuestKindId, RuinId, TextId, TileIdx,
};
use crate::base::sets::{BitSet, BuildingSet, PlayerSet, PlayerVec};
use crate::base::stats::Stat;
use crate::rules::defs::{
    BeliefKind, CityStatePersonality, QuestScope, QuestTargetKind, ReligionProgress,
};
use crate::state::TileClaim;
use crate::state::cities::City;
use crate::state::map::{RouteBits, Tile};
use crate::state::memory::{CityMemory, TileMemory, TileMemoryLayer};
use crate::state::players::{
    AutoDecision, AutoOverrides, CityStateData, Controller, CsPair, Handicap, Player, PlayerKind,
    Quest, QuestTarget, QuestTimers, Rgb, Seat, SeatOverrides, Spy, SpyAction, TempUnique,
    WarQuest,
};

/// A converted player, and whom it had met, which goes to the relations.
pub(super) struct PlayerOut {
    pub(super) player: Player,
    pub(super) met: PlayerSet,
}

/// Every player, in id order, with their explorers' memory given to the units and their free
/// buildings to the cities.
pub(super) fn players(
    cx: &mut Cx<'_>,
    top: &Obj<'_>,
    units: &mut [PendingUnit],
    cities: &mut [City],
) -> Res<Vec<PlayerOut>> {
    let at = top.at("players");
    let rows = list(top.req("players")?, &at)?;
    let mut out = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        out.push(player(cx, i, row, &at.index(i), units, cities)?);
    }
    spaceship(cx, top, &mut out)?;
    Ok(out)
}

/// `GameState.spaceship`, `{pid: {part: count}}`, moved to the majors.
fn spaceship(cx: &mut Cx<'_>, top: &Obj<'_>, out: &mut [PlayerOut]) -> Res<()> {
    let at = top.at("spaceship");
    dict(top.req("spaceship")?, &at, |k, v, p| {
        let pid = cx.player_key(k, p)?;
        let parts = dict(v, p, |name, n, pp| Ok((cx.id_of::<BaseUnitId>(name, pp)?, int(n, pp)?)))?;
        let major = out
            .get_mut(usize::from(pid.0))
            .and_then(|o| o.player.major.as_mut())
            .ok_or_else(|| p.err(format!("player {pid} builds a spaceship but is no major")))?;
        major.spaceship.extend(parts);
        Ok(())
    })?;
    Ok(())
}

/// One player, at position `pos`.
fn player(
    cx: &mut Cx<'_>,
    pos: usize,
    v: &Value,
    p: &Path<'_>,
    units: &mut [PendingUnit],
    cities: &mut [City],
) -> Res<PlayerOut> {
    let o = Obj::new(v, *p)?;
    let id: u8 = o.int_req("id")?;
    if usize::from(id) != pos {
        return Err(o.at("id").err(format!("player {id} is at position {pos}")));
    }
    let id = PlayerId(id);
    let kind = match o.text("kind", "major")? {
        "major" => PlayerKind::Major,
        "city_state" => PlayerKind::CityState,
        "barbarian" => PlayerKind::Barbarian,
        other => return Err(o.at("kind").err(format!("no player kind {other:?}"))),
    };
    let nation: NationId = cx.named(o.req("nation")?, &o.at("nation"))?;
    let color = o.text_req("color")?;
    let color =
        Rgb::from_hex(color).ok_or_else(|| o.at("color").err(format!("{color:?} is no colour")))?;
    let seat = seat(cx, &o)?;
    let mut pl = Player::new(id, kind, o.text_req("name")?.into(), nation, color, seat, cx.size);
    let alive = o.flag("alive", true)?;
    let eliminated: Option<i32> = o.opt_int("eliminated_turn")?;
    pl = pl.restore(alive, eliminated);
    pl.leader = o.text("leader", "")?.into();
    pl.capital = cx.opt_city(o.get("capital"), &o.at("capital"))?;
    pl.original_capital = cx.opt_city(o.get("original_capital"), &o.at("original_capital"))?;
    pl.founded_city = o.flag("founded_city", false)?;
    pl.city_counter = o.int("city_counter", 0)?;

    // Stocks and golden ages.
    let e = &mut pl.econ;
    e.gold = o.real("gold", 0.0)?;
    e.culture = o.real("culture", 0.0)?;
    e.faith = o.real("faith", 0.0)?;
    e.golden_age_points = o.real("golden_age_points", 0.0)?;
    e.golden_age_turns = o.int("golden_age_turns", 0)?;
    e.golden_ages = o.int("golden_ages", 0)?;

    // Research.
    if let Some(v) = o.get("techs") {
        pl.tech.known = cx.id_set(list(v, &o.at("techs"))?, &o.at("techs"))?;
    }
    pl.tech.queue = o.each("research_queue", |v, p| cx.named(v, p))?;
    pl.tech.goal = cx.opt_named(o.get("research_goal"), &o.at("research_goal"))?;
    pl.tech.progress = o
        .entries("research_progress", |k, v, p| Ok((cx.id_of(k, p)?, real(v, p)?)))?
        .into_iter()
        .collect();
    pl.tech.overflow = o.real("overflow_science", 0.0)?;
    pl.tech.free_techs = o.int("free_techs", 0)?;
    pl.tech.future_techs = o.int("future_techs", 0)?;

    // Policies.
    if let Some(v) = o.get("policies") {
        pl.policy.adopted = cx.id_set(list(v, &o.at("policies"))?, &o.at("policies"))?;
    }
    pl.policy.adopted_count = o.int("policies_adopted_count", 0)?;
    pl.policy.free_policies = o.int("free_policies", 0)?;

    // Religion.
    let state = o.text("religion_state", "none")?;
    pl.religion.progress = ReligionProgress::from_name(state)
        .ok_or_else(|| o.at("religion_state").err(format!("no religion state {state:?}")))?;
    pl.religion.founded = cx.opt_religion(o.get("religion"), &o.at("religion"))?;

    // Great people.
    let gp = &mut pl.gp;
    gp.prophets_earned = o.int("great_prophets_earned", 0)?;
    gp.points =
        o.entries("gp_points", |k, v, p| Ok((cx.id_of(k, p)?, real(v, p)?)))?.into_iter().collect();
    gp.combat_points =
        o.entries("gg_points", |k, v, p| Ok((cx.id_of(k, p)?, real(v, p)?)))?.into_iter().collect();
    gp.combat_threshold = o
        .entries("gg_threshold", |k, v, p| Ok((cx.id_of(k, p)?, int(v, p)?)))?
        .into_iter()
        .collect();
    gp.free = o.int("free_great_people", 0)?;
    gp.earned = o.int("great_people_earned", 0)?;
    if o.real("gp_threshold", 100.0)?.to_bits() != 100.0f64.to_bits() {
        cx.report.note(Dropped::GpThreshold);
    }

    // The rest of the civilization's standing data.
    pl.civ.temp_uniques = o.each("temp_uniques", |v, p| temp_unique(cx, v, p))?;
    pl.civ.built_increasing = increasing(cx, &o, "built_increasing")?;
    pl.civ.bought_increasing = increasing(cx, &o, "bought_increasing")?;
    pl.civ.free_stat_buildings = sorted_pairs(cx, &o, "free_stat_buildings", |_, v, p| {
        let name = text(v, p)?;
        Stat::from_name(name).ok_or_else(|| p.err(format!("no stat {name:?}")))
    })?;
    pl.civ.free_specific_buildings =
        sorted_pairs(cx, &o, "free_specific_buildings", |cx, v, p| cx.named::<BuildingId>(v, p))?;
    if let Some(v) = o.get("natural_wonders") {
        pl.civ.natural_wonders =
            cx.id_set(list(v, &o.at("natural_wonders"))?, &o.at("natural_wonders"))?;
    }
    free_buildings(cx, &o, id, cities)?;

    // What the tiles look like to it.
    pl.explored = explored(cx, &o, kind)?;
    let memory = memory(cx, &o, kind)?;
    let met = match o.get("met") {
        None => PlayerSet::EMPTY,
        Some(v) => cx.player_set(list(v, &o.at("met"))?, &o.at("met"))?,
    };
    if met.contains(id) {
        return Err(o.at("met").err("a player has met itself"));
    }

    // What only majors have.
    let spies = o.each("spies", |v, p| spy(cx, v, p))?;
    let notes = o.text("notes", "")?;
    match pl.major.as_deref_mut() {
        Some(m) => {
            if let Some(layer) = memory {
                m.memory = layer;
            }
            m.spies = spies;
            m.notes = notes.into();
        }
        None if !spies.is_empty() || !notes.is_empty() => {
            return Err(o.path().err("only a major civilization has spies and notes"));
        }
        None => {}
    }

    // What only city-states have.
    city_state_fields(cx, &o, &mut pl)?;

    for (key, drop) in [
        ("cs_unit_timer", Dropped::CsUnitTimer),
        ("tribute_turn", Dropped::TributeTurn),
        ("ruins_rewards", Dropped::RuinsRewards),
        ("spy_eras", Dropped::SpyEras),
        ("faith_buys", Dropped::FaithBuys),
    ] {
        dead(cx, &o, key, drop)?;
    }
    if let Some(v) = o.get("flags") {
        flags(cx, v, &o.at("flags"), &mut pl, units)?;
    }
    o.finish()?;
    Ok(PlayerOut { player: pl, met })
}

/// The seat: controller, handicap, automatic decisions, explicit overrides and difficulty, as
/// `Player.__post_init__` settled them (`state.py:274-280`).
fn seat(cx: &Cx<'_>, o: &Obj<'_>) -> Res<Seat> {
    let c = o.text("controller", "human")?;
    let controller = Controller::from_name(c)
        .ok_or_else(|| o.at("controller").err(format!("no controller {c:?}")))?;
    let handicap = match o.text("handicap", "")? {
        "" => None,
        h => Some(
            Handicap::from_name(h)
                .ok_or_else(|| o.at("handicap").err(format!("no handicap {h:?}")))?,
        ),
    };
    let mut saved = AutoOverrides::default();
    for (d, on) in o.entries("auto", |k, v, p| {
        let d = AutoDecision::from_name(k)
            .ok_or_else(|| p.err("no automatic decision of this name"))?;
        Ok((d, flag(v, p)?))
    })? {
        saved.set(d, on);
    }
    let overrides = match o.get("overrides") {
        None | Some(Value::Null) => SeatOverrides::default(),
        Some(v) => {
            let ov = Obj::new(v, o.at("overrides"))?;
            let parsed = SeatOverrides::parse(ov.get("handicap"), ov.get("auto"))
                .map_err(|e| ov.path().err(e.0))?;
            ov.finish()?;
            parsed
        }
    };
    let difficulty = cx.opt_named(o.get("difficulty"), &o.at("difficulty"))?;
    Ok(Seat::restore(controller, handicap, saved, overrides, difficulty, None))
}

/// A unique held for some turns: the variant without the timer of the timed unique whose text
/// Python kept (`triggers.py:88-91`).
fn temp_unique(cx: &Cx<'_>, v: &Value, p: &Path<'_>) -> Res<TempUnique> {
    let o = Obj::new(v, *p)?;
    let text = o.text_req("text")?;
    let turns: i16 = o.int_req("turns")?;
    o.finish()?;
    let table = cx.r.uniques();
    let unique = table
        .meta
        .iter()
        .find(|(id, m)| m.timed.is_some() && table.text_of(*id) == text)
        .and_then(|(_, m)| m.temp_variant)
        .ok_or_else(|| o.at("text").err(format!("no timed unique reads {text:?}")))?;
    Ok(TempUnique { unique, turns })
}

/// Times each item was built or bought with increasing cost.
fn increasing(
    cx: &Cx<'_>,
    o: &Obj<'_>,
    key: &str,
) -> Res<BTreeMap<crate::state::cities::Constructible, u16>> {
    Ok(o.entries(key, |k, v, p| Ok((cx.constructible(k, p)?, int(v, p)?)))?.into_iter().collect())
}

/// A list of `[thing, city id]` pairs, kept sorted; its order is dropped, and counted if it was
/// not sorted.
fn sorted_pairs<T: Ord + Copy>(
    cx: &mut Cx<'_>,
    o: &Obj<'_>,
    key: &str,
    thing: impl Fn(&Cx<'_>, &Value, &Path<'_>) -> Res<T>,
) -> Res<Vec<(T, CityId)>> {
    let pairs = o.each(key, |v, p| {
        let [a, c] = list(v, p)? else {
            return Err(p.err("expected a [name, city id] pair"));
        };
        Ok((thing(cx, a, &p.index(0))?, cx.city(c, &p.index(1))?))
    })?;
    let mut sorted = pairs.clone();
    sorted.sort();
    if sorted.windows(2).any(|w| w[0] == w[1]) {
        return Err(o.at(key).err("a pair is listed twice"));
    }
    if sorted != pairs {
        cx.report.note(Dropped::ListOrder);
    }
    Ok(sorted)
}

/// `Player.free_buildings`, `{city id: [building]}`: each city's entry of the player that holds
/// it joins the city's free buildings, as Python read them there (`cities.py:285-293`); another
/// player's, or a gone city's, is dropped.
fn free_buildings(cx: &mut Cx<'_>, o: &Obj<'_>, id: PlayerId, cities: &mut [City]) -> Res<()> {
    let entries = o.entries("free_buildings", |k, v, p| {
        let c: u32 = key_int(k, p)?;
        let set: BuildingSet = cx.id_set(list(v, p)?, p)?;
        Ok((c, set))
    })?;
    for (c, set) in entries {
        if set.is_empty() {
            continue;
        }
        let city = cities
            .binary_search_by_key(&c, |x| x.id().get())
            .ok()
            .and_then(|i| cities.get_mut(i))
            .filter(|x| x.owner() == id);
        match city {
            Some(city) => city.free_buildings |= set,
            None => cx.report.note(Dropped::FreeBuildingsElsewhere),
        }
    }
    Ok(())
}

/// The tiles a player has seen: base64 of one byte per tile. The barbarians keep none.
fn explored(cx: &mut Cx<'_>, o: &Obj<'_>, kind: PlayerKind) -> Res<BitSet> {
    let at = o.at("explored");
    let Some(v) = o.get("explored") else { return Ok(BitSet::new()) };
    let bytes = b64_decode(text(v, &at)?).map_err(|e| at.err(e))?;
    if !bytes.is_empty() && bytes.len() != cx.size as usize {
        return Err(at.err(format!("{} tiles explored of a map of {}", bytes.len(), cx.size)));
    }
    let mut set = BitSet::with_capacity(cx.size);
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            0 => {}
            1 => {
                set.insert(u32::try_from(i).unwrap_or(u32::MAX));
            }
            other => return Err(at.err(format!("tile {i} is explored as {other}, not 0 or 1"))),
        }
    }
    if kind == PlayerKind::Barbarian {
        if !set.is_empty() {
            cx.report.note(Dropped::BarbarianExplored);
        }
        return Ok(BitSet::new());
    }
    Ok(set)
}

/// A major's memory of tiles out of sight (`visibility.py:116-125`); a city-state's or the
/// barbarians' is dropped, and `None` is returned for them.
fn memory(cx: &mut Cx<'_>, o: &Obj<'_>, kind: PlayerKind) -> Res<Option<TileMemoryLayer>> {
    let entries = o.entries("memory", |k, v, p| {
        let t = cx.tile_key(k, p)?;
        let m = Obj::new(v, *p)?;
        let features = match m.get("f") {
            None | Some(Value::Null) => Default::default(),
            Some(f) => super::config::features(cx, list(f, &m.at("f"))?, &m.at("f"))?,
        };
        let improvement = cx.opt_named(m.get("i"), &m.at("i"))?;
        let route = match m.opt_text("r")? {
            None => None,
            Some("Road") => Some(crate::rules::defs::Route::Road),
            Some("Railroad") => Some(crate::rules::defs::Route::Railroad),
            Some(other) => return Err(m.at("r").err(format!("no route {other:?}"))),
        };
        let owner = cx.opt_player(m.get("o"), &m.at("o"))?;
        let pillaged = m.flag("p", false)?;
        let city = match m.get("c") {
            None | Some(Value::Null) => None,
            Some(c) => {
                let at = m.at("c");
                let [name, pop, owner] = list(c, &at)? else {
                    return Err(at.err("a remembered city is [name, pop, owner]"));
                };
                Some(CityMemory {
                    name: text(name, &at.index(0))?.into(),
                    pop: int(pop, &at.index(1))?,
                    owner: cx.player(owner, &at.index(2))?,
                })
            }
        };
        m.finish()?;
        // The terrain is not remembered; any stands in for it.
        let seen = Tile::new(crate::base::ids::TerrainId(0))
            .with_features(features)
            .with_improvement(improvement)
            .with_route_bits(RouteBits::EMPTY.with_route(route).with_improvement_pillaged(pillaged))
            .with_claim(TileClaim { owner, city: None });
        Ok((t, TileMemory::of(&seen), city))
    })?;
    if kind != PlayerKind::Major {
        if !entries.is_empty() {
            cx.report.note(Dropped::MinorMemory);
        }
        return Ok(None);
    }
    let mut tiles = vec![TileMemory::NONE; cx.size as usize];
    let mut cities = BTreeMap::new();
    for (t, m, c) in entries {
        if let Some(slot) = tiles.get_mut(t.0 as usize) {
            *slot = m;
        }
        if let Some(c) = c {
            cities.insert(t, c);
        }
    }
    TileMemoryLayer::from_parts(tiles, cities)
        .map(Some)
        .ok_or_else(|| o.at("memory").err("a remembered city is off the map"))
}

/// A spy (`espionage.py:1-5`).
fn spy(cx: &Cx<'_>, v: &Value, p: &Path<'_>) -> Res<Spy> {
    let o = Obj::new(v, *p)?;
    let action = o.text("action", "None")?;
    let spy = Spy {
        name: o.text_req("name")?.into(),
        rank: o.int("rank", 1)?,
        city: cx.opt_city(o.get("city"), &o.at("city"))?,
        action: SpyAction::from_name(action)
            .ok_or_else(|| o.at("action").err(format!("no spy action {action:?}")))?,
        turns: o.int("turns", 0)?,
        progress: o.int("progress", 0)?,
    };
    o.finish()?;
    Ok(spy)
}

/// The fields only a city-state may fill: its type, personality, luxury, unit, influence, ally,
/// protectors and quests.
fn city_state_fields(cx: &mut Cx<'_>, o: &Obj<'_>, pl: &mut Player) -> Res<()> {
    let cs_type = cx.opt_named(o.get("cs_type"), &o.at("cs_type"))?;
    let personality = match o.opt_text("cs_personality")? {
        None => None,
        Some(name) => Some(
            CityStatePersonality::from_name(name)
                .ok_or_else(|| o.at("cs_personality").err(format!("no personality {name:?}")))?,
        ),
    };
    let resource = cx.opt_named(o.get("cs_resource"), &o.at("cs_resource"))?;
    let unique_unit = cx.opt_named(o.get("cs_unique_unit"), &o.at("cs_unique_unit"))?;
    let influence: Vec<(PlayerId, f64)> =
        o.entries("influence", |k, v, p| Ok((cx.player_key(k, p)?, real(v, p)?)))?;
    let ally = cx.opt_player(o.get("ally"), &o.at("ally"))?;
    let protectors = match o.get("protectors") {
        None => PlayerSet::EMPTY,
        Some(v) => cx.player_set(list(v, &o.at("protectors"))?, &o.at("protectors"))?,
    };
    let quests = o.each("quests", |v, p| quest(cx, v, p))?;
    let Some(cs) = pl.city_state.take() else {
        let any = cs_type.is_some()
            || personality.is_some()
            || resource.is_some()
            || unique_unit.is_some()
            || !influence.is_empty()
            || ally.is_some()
            || !protectors.is_empty()
            || !quests.is_empty();
        if any {
            return Err(o.path().err("only a city-state has city-state data"));
        }
        return Ok(());
    };
    let mut cs: CityStateData = *cs;
    cs.cs_type = cs_type;
    cs.personality = personality;
    cs.resource = resource;
    cs.unique_unit = unique_unit;
    let mut cs = cs.with_ally(ally);
    // One entry per player, as every city-state keeps them (`CityStateData::influence`), however
    // many keys Python happened to hold.
    let mut by = PlayerVec::from_elem(0.0, cx.n);
    for (p, x) in influence {
        if let Some(slot) = by.get_mut(p) {
            *slot = x;
        }
    }
    cs.influence = by;
    // Until `flags["pairs"]` says more.
    cs.pairs = PlayerVec::from_elem(CsPair::default(), cx.n);
    cs.protectors = protectors;
    cs.quests = quests;
    pl.city_state = Some(Box::new(cs));
    Ok(())
}

/// A city-state quest, its `data1` typed by the quest's kind (DESIGN.md 4.5).
fn quest(cx: &mut Cx<'_>, v: &Value, p: &Path<'_>) -> Res<Quest> {
    let o = Obj::new(v, *p)?;
    let name = o.text_req("name")?;
    let kind: QuestKindId = cx.id_of(name, &o.at("name"))?;
    let target_kind = cx.r.quests().get(kind).map_or(QuestTargetKind::None, |q| q.target);
    let at = o.at("data1");
    let data = o.req("data1")?;
    let target = match target_kind {
        QuestTargetKind::None => match data {
            Value::String(s) if s.is_empty() => QuestTarget::None,
            other => return Err(at.err(format!("a {name} quest holds nothing, not {other}"))),
        },
        QuestTargetKind::Tile => QuestTarget::Tile(cx.tile(data, &at)?),
        QuestTargetKind::Resource => QuestTarget::Resource(cx.named(data, &at)?),
        QuestTargetKind::Building => QuestTarget::Building(cx.named(data, &at)?),
        QuestTargetKind::UnitType => QuestTarget::UnitType(cx.named(data, &at)?),
        QuestTargetKind::Player => QuestTarget::Player(cx.player(data, &at)?),
        QuestTargetKind::NaturalWonder => QuestTarget::NaturalWonder(cx.named(data, &at)?),
        QuestTargetKind::Religion => QuestTarget::Religion(cx.religion(data, &at)?),
        QuestTargetKind::Baseline => QuestTarget::Baseline(int(data, &at)?),
        QuestTargetKind::Percent => QuestTarget::Percent(int(data, &at)?),
    };
    let scope = match o.text("kind", "individual")? {
        "individual" => QuestScope::Individual,
        "global" => QuestScope::Global,
        other => return Err(o.at("kind").err(format!("no quest scope {other:?}"))),
    };
    if !o.text("data2", "")?.is_empty() {
        cx.report.note(Dropped::QuestData2);
    }
    let q = Quest {
        kind,
        assignee: cx.player(o.req("assignee")?, &o.at("assignee"))?,
        turn: o.int_req("turn")?,
        scope,
        target,
        influence: o.int("influence", 40)?,
        duration: o.int("duration", 0)?,
    };
    o.finish()?;
    Ok(q)
}

/// The kind of player a flag belongs to, for the flags that only one kind keeps.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Keeps {
    Major,
    CityState,
}

/// `Player.flags`, typed (DESIGN.md 4.5).
fn flags(
    cx: &mut Cx<'_>,
    v: &Value,
    p: &Path<'_>,
    pl: &mut Player,
    units: &mut [PendingUnit],
) -> Res<()> {
    let f = Obj::new(v, *p)?;
    let id = pl.id();
    let only = |keeps: Keeps, key: &str, pl: &Player| -> Res<()> {
        let ok = match keeps {
            Keeps::Major => pl.major.is_some(),
            Keeps::CityState => pl.city_state.is_some(),
        };
        if ok { Ok(()) } else { Err(f.at(key).err("a flag this kind of player does not keep")) }
    };

    pl.start_tile = cx.opt_tile(f.get("start"), &f.at("start"))?;
    if let Some(v) = f.get("culture_last8") {
        pl.econ.culture_hist = last8(v, &f.at("culture_last8"))?;
    }
    if let Some(v) = f.get("science_last8") {
        pl.econ.science_hist = last8(v, &f.at("science_last8"))?;
    }
    pl.econ.total_culture = f.int("total_culture", 0)?;
    pl.econ.total_faith = f.int("total_faith", 0)?;
    pl.econ.last_gold_rate = f.real("last_gold_rate", 0.0)?;
    pl.tech.ra_bonus = f.int("ra_science", 0)?;
    pl.gp.pool_threshold = f
        .entries("gp_threshold", |k, v, p| {
            let pool = if k.is_empty() { None } else { Some(cx.id_of::<TextId>(k, p)?) };
            Ok((pool, int(v, p)?))
        })?
        .into_iter()
        .collect();
    pl.gp.maya_limited = f.int("maya_limited", 0)?;
    pl.gp.long_count_pool = f.each("long_count_pool", |v, p| cx.named(v, p))?;
    for (k, n) in f.entries("free_beliefs", |k, v, p| {
        let k = BeliefKind::from_name(k).ok_or_else(|| p.err("no belief type of this name"))?;
        Ok((k, int::<u8>(v, p)?))
    })? {
        pl.religion.set_free(k, n);
    }
    pl.religion.choose_pantheon_belief = f.flag("choose_pantheon_belief", false)?;
    pl.civ.revolt_in = f.opt_int("revolt_in")?;
    if let Some(v) = f.get("last_ruins") {
        pl.civ.last_ruins = last_ruins(cx, v, &f.at("last_ruins"))?;
    }
    if let Some(v) = f.get("skip_explore") {
        let at = f.at("skip_explore");
        let tiles: Vec<TileIdx> = list(v, &at)?
            .iter()
            .enumerate()
            .map(|(i, t)| cx.tile(t, &at.index(i)))
            .collect::<Res<_>>()?;
        let mut sorted = tiles.clone();
        sorted.sort();
        sorted.dedup();
        if sorted.len() > 300 {
            return Err(at.err(format!("{} tiles; the explorers keep at most 300", sorted.len())));
        }
        if sorted != tiles {
            cx.report.note(Dropped::ListOrder);
        }
        pl.civ.explore_skip = sorted;
    }
    explorers(cx, &f, id, units)?;
    if !super::read::is_none(f.get("last_stats")) {
        cx.report.note(Dropped::LastStats);
    }
    if f.get("unreachable_explore").is_some() {
        cx.report.note(Dropped::UnreachableExplore);
    }

    // A city-state's.
    if let Some(v) = f.get("pairs") {
        only(Keeps::CityState, "pairs", pl)?;
        let pairs = pairs(cx, v, &f.at("pairs"))?;
        if let Some(cs) = pl.city_state.as_mut() {
            cs.pairs = pairs;
        }
    }
    if let Some(v) = f.get("quest_state") {
        only(Keeps::CityState, "quest_state", pl)?;
        let timers = quest_timers(cx, v, &f.at("quest_state"))?;
        if let Some(cs) = pl.city_state.as_mut() {
            cs.timers = timers;
        }
    }
    if let Some(v) = f.get("war_quests") {
        only(Keeps::CityState, "war_quests", pl)?;
        let wars = war_quests(cx, v, &f.at("war_quests"))?;
        if let Some(cs) = pl.city_state.as_mut() {
            cs.war_quests = wars;
        }
    }
    for key in ["election_in", "barb_help_cd", "recently_bullied"] {
        if f.get(key).is_some() {
            only(Keeps::CityState, key, pl)?;
        }
    }
    if let Some(cs) = pl.city_state.as_mut() {
        cs.election_in = f.opt_int("election_in")?;
        cs.barb_help_cd = f.int("barb_help_cd", 0)?;
        cs.recently_bullied = f.int("recently_bullied", 0)?;
    }

    // A major's.
    for key in ["eras_spy_earned", "cs_attacks", "cs_gp_gift"] {
        if f.get(key).is_some() {
            only(Keeps::Major, key, pl)?;
        }
    }
    let eras = match f.get("eras_spy_earned") {
        None => Default::default(),
        Some(v) => {
            cx.id_set::<EraId, 1>(list(v, &f.at("eras_spy_earned"))?, &f.at("eras_spy_earned"))?
        }
    };
    let cs_attacks = f.int("cs_attacks", 0)?;
    let cs_gp_gift = f.opt_int("cs_gp_gift")?;
    if let Some(m) = pl.major.as_mut() {
        m.spy_eras_earned = eras;
        m.cs_attacks = cs_attacks;
        m.cs_gp_gift = cs_gp_gift;
    }

    // `gained_<unit>`: read but never written by Python (`movement.py:134`), kept now.
    for (k, v) in f.rest() {
        let at = f.at(k);
        let Some(unit) = k.strip_prefix("gained_") else {
            return Err(at.err("a flag the converter does not know"));
        };
        if truthy(v) {
            let u: BaseUnitId = cx.id_of(unit, &at)?;
            pl.civ.units_gained.insert(u);
        }
    }
    Ok(())
}

/// Python's truth value of a flag's value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// A yield per turn over the last eight turns (`policies.py:160-161`, `research.py:187-188`).
fn last8(v: &Value, p: &Path<'_>) -> Res<[i32; 8]> {
    let items = list(v, p)?;
    let mut out = [0; 8];
    if items.len() != out.len() {
        return Err(p.err(format!("{} turns; the history keeps 8", items.len())));
    }
    for (i, (slot, x)) in out.iter_mut().zip(items).enumerate() {
        *slot = int(x, &p.index(i))?;
    }
    Ok(out)
}

/// The last two ruin rewards, newest last, `""` for none (`ruins.py:45, 62`).
fn last_ruins(cx: &Cx<'_>, v: &Value, p: &Path<'_>) -> Res<[Option<RuinId>; 2]> {
    let items = list(v, p)?;
    let mut out = [None; 2];
    if items.len() != out.len() {
        return Err(p.err(format!("{} rewards; the history keeps 2", items.len())));
    }
    for (i, (slot, x)) in out.iter_mut().zip(items).enumerate() {
        let at = p.index(i);
        let name = text(x, &at)?;
        *slot = if name.is_empty() { None } else { Some(cx.id_of(name, &at)?) };
    }
    Ok(out)
}

/// The explorers' targets and recent tiles (`automation.py:236-285`), moved to their units. A
/// unit that is gone, or no longer this player's, takes its entries with it.
fn explorers(cx: &mut Cx<'_>, f: &Obj<'_>, id: PlayerId, units: &mut [PendingUnit]) -> Res<()> {
    let find = |units: &[PendingUnit], uid: u32| {
        units
            .binary_search_by_key(&uid, |u| u.unit.id().get())
            .ok()
            .filter(|&i| units.get(i).is_some_and(|u| u.unit.owner() == id))
    };
    let targets = f.entries("explore_targets", |k, v, p| {
        Ok((key_int::<u32>(k, p)?, cx.opt_tile(Some(v), p)?))
    })?;
    for (uid, target) in targets {
        match find(units, uid).and_then(|i| units.get_mut(i)) {
            Some(u) => u.unit.explore.target = target,
            None => cx.report.note(Dropped::ExplorerGone),
        }
    }
    let hist = f.entries("explore_hist", |k, v, p| {
        let tiles: Vec<TileIdx> = list(v, p)?
            .iter()
            .enumerate()
            .map(|(i, t)| cx.tile(t, &p.index(i)))
            .collect::<Res<_>>()?;
        if tiles.len() > 4 {
            return Err(p.err(format!("{} recent tiles; an explorer keeps 4", tiles.len())));
        }
        Ok((key_int::<u32>(k, p)?, tiles))
    })?;
    for (uid, tiles) in hist {
        match find(units, uid).and_then(|i| units.get_mut(i)) {
            Some(u) => u.unit.explore.recent = tiles.into_iter().collect(),
            None => cx.report.note(Dropped::ExplorerGone),
        }
    }
    Ok(())
}

/// A city-state's standing with each major (`city_states.py:25-31`), one entry per player.
fn pairs(cx: &Cx<'_>, v: &Value, p: &Path<'_>) -> Res<PlayerVec<CsPair>> {
    let entries = dict(v, p, |k, x, pp| {
        let major = cx.player_key(k, pp)?;
        let o = Obj::new(x, *pp)?;
        let pair = CsPair {
            bullied: o.int("bullied", 0)?,
            pledged: o.int("pledged", 0)?,
            withdrew: o.int("withdrew", 0)?,
            border_conflict: o.int("border_conflict", 0)?,
            anger_free: o.int("anger_free", 0)?,
            recently_attacked: o.int("recently_attacked", 0)?,
            marriage_cooldown: o.int("marriage_cooldown", 0)?,
            unit_timer: o.opt_int("unit_timer")?,
            wary: o.flag("wary", false)?,
        };
        o.finish()?;
        Ok((major, pair))
    })?;
    let mut out = PlayerVec::from_elem(CsPair::default(), cx.n);
    for (m, pair) in entries {
        if let Some(slot) = out.get_mut(m) {
            *slot = pair;
        }
    }
    Ok(out)
}

/// When a city-state next gives quests (`city_states.py:983-997`). A countdown of -1 means
/// none is scheduled, as no entry does.
fn quest_timers(cx: &Cx<'_>, v: &Value, p: &Path<'_>) -> Res<QuestTimers> {
    let o = Obj::new(v, *p)?;
    let global = o.int("global", -1)?;
    let individual = o
        .entries("individual", |k, x, pp| Ok((cx.player_key(k, pp)?, int::<i16>(x, pp)?)))?
        .into_iter()
        .filter(|&(_, n)| n != -1)
        .collect();
    o.finish()?;
    Ok(QuestTimers { global, individual })
}

/// The "kill the attacker's units" pseudo-quests (`city_states.py:731-760`).
fn war_quests(cx: &Cx<'_>, v: &Value, p: &Path<'_>) -> Res<BTreeMap<PlayerId, WarQuest>> {
    Ok(dict(v, p, |k, x, pp| {
        let attacker = cx.player_key(k, pp)?;
        let o = Obj::new(x, *pp)?;
        let needed = o.int_req("needed")?;
        let kills = o
            .entries("kills", |kk, n, kp| Ok((cx.player_key(kk, kp)?, int::<u16>(n, kp)?)))?
            .into_iter()
            .collect();
        o.finish()?;
        Ok((attacker, WarQuest { needed, kills }))
    })?
    .into_iter()
    .collect())
}
