//! The `state_echo` group (DESIGN.md 9.2, package 1a-10): the fixture's own state, read back
//! from the loaded game (`Game::from_python`, package 1b-01).
//!
//! The Python side is a projection of the recorded `GameState.to_dict()`: its settings, tiles,
//! players, units, cities, diplomacy, world and history, in Python's vocabulary (names, tile
//! indices, player ids). The Rust side builds the same projection from the loaded game's
//! [`State`] and [`Chronicle`] through their public reads and the ruleset's names, never through
//! the converter's code, so a field the converter misreads, misplaces or misnames, or the settle
//! on load changes, shows as a difference at its place.
//!
//! The projection is of everything the Rust state keeps from Python, in the form the design gives
//! it (DESIGN.md 4.4-4.7): the settings (host keys included), the map and each tile's continent,
//! the tiles, the players with their seats, flags, explored tiles and each major's memory of
//! every tile, units, cities, relations, opinions, deals, negotiations, the world, the id counter
//! and the history. Lists that became sets compare as multisets, flags are read where they moved,
//! and what the converter drops by design (its `ConvertReport`) is left out on both sides: the
//! barbarians' explored tiles, the non-majors' memories, the explorers of units that are gone,
//! the dead fields. Python's reads that created state are read as Python's first read left them:
//! a city whose pressures nothing had read yet is seeded (`religion.py:105-109`).
//!
//! [`State`]: citar_engine::state::State
//! [`Chronicle`]: citar_engine::state::chronicle::Chronicle

use std::borrow::Cow;

use citar_engine::base::codec::b64_decode;
use citar_engine::base::ids::{
    CityStateTypeId, FeatureId, Id, PlayerId, QuestKindId, ReligionId, RuinId, TextId, TileIdx,
};
use citar_engine::base::sets::IdSet;
use citar_engine::game::Game;
use citar_engine::rules::defs::Route;
use citar_engine::rules::{Named, Ruleset};
use citar_engine::state::State;
use citar_engine::state::chronicle::{Event, EventData, EventType, RefKind, StatsRow};
use citar_engine::state::cities::Constructible;
use citar_engine::state::config::{MapSource, ResourceKindOptions, ResourceOptions, ResourceRule};
use citar_engine::state::diplo::{Deal, Negotiation, OpinionKey, Relation, Terms, side};
use citar_engine::state::map::WATER;
use citar_engine::state::memory::TileMemoryLayer;
use citar_engine::state::players::{AutoDecision, Player, QuestTarget};
use citar_engine::state::world::{ReligionName, UnResult};
use serde_json::{Map, Value, json};

use super::{AnswerError, AnswerModule, Ctx};
use crate::Group;

/// The `state_echo` answer module.
#[derive(Debug, Clone, Copy, Default)]
pub struct StateEcho;

impl AnswerModule for StateEcho {
    fn group(&self) -> Group {
        Group::StateEcho
    }

    fn expected<'a>(&self, cx: &Ctx<'a>) -> Result<Cow<'a, Value>, AnswerError> {
        let fixture = cx.fixture.ok_or_else(|| AnswerError::new("state_echo needs a fixture"))?;
        let state: Value = serde_json::from_str(fixture.state.get())
            .map_err(|e| AnswerError::new(format!("the fixture's state is not JSON: {e}")))?;
        Ok(Cow::Owned(python(&state)))
    }

    fn answer(&self, cx: &Ctx<'_>, _expected: &Value) -> Result<Value, AnswerError> {
        let g = cx.game.ok_or_else(|| AnswerError::new("no loaded game"))?;
        Ok(rust(Ruleset::shared(), g))
    }
}

// ---- The Python side ----------------------------------------------------------------------------

/// The projection of Python's state.
pub fn python(s: &Value) -> Value {
    json!({
        "clock": pick(s, &["turn", "current", "turn_started", "phase", "winner", "victory", "next_id"]),
        "map": py_map(s),
        "settings": py_settings(&s["config"]),
        "tiles": py_tiles(s),
        "players": py_players(s),
        "units": py_units(s),
        "cities": py_cities(s),
        "diplomacy": py_diplomacy(s),
        "world": py_world(s),
        "history": py_history(s),
    })
}

/// These fields of an object, as they are.
fn pick(v: &Value, keys: &[&str]) -> Value {
    let mut m = Map::new();
    for &k in keys {
        m.insert(k.to_owned(), v.get(k).cloned().unwrap_or(Value::Null));
    }
    Value::Object(m)
}

fn arr(v: &Value) -> &[Value] {
    v.as_array().map_or(&[], Vec::as_slice)
}

fn obj(v: &Value) -> impl Iterator<Item = (&String, &Value)> {
    v.as_object().into_iter().flatten()
}

fn py_map(s: &Value) -> Value {
    json!({
        "width": s["width"], "height": s["height"],
        "wrap_x": s["config"]["wrap_x"], "wrap_y": s["config"]["wrap_y"],
        "continents": s["continents"],
    })
}

/// The settings keys the engine reads (`game.py:32-60`, and what `Game.new` adds); the rest are
/// the host's.
const ENGINE_KEYS: [&str; 27] = [
    "map_size",
    "map_type",
    "map",
    "width",
    "height",
    "wrap_x",
    "wrap_y",
    "seed",
    "speed",
    "difficulty",
    "barbarian_difficulty",
    "ai_base_values",
    "starting_era",
    "barbarians",
    "barbarian_aggression",
    "turn_limit",
    "victories",
    "city_states",
    "religion",
    "espionage",
    "nuclear_weapons",
    "tech_trading",
    "ruins",
    "map_edges",
    "river_density",
    "resources",
    "diplomacy",
];

/// Python's `_num` (`mapgen.py:43-48`): a number clamped to a range, or the default.
fn num(v: Option<&Value>, default: f64, lo: f64, hi: f64) -> Value {
    json!(v.and_then(Value::as_f64).map_or(default, |x| x.clamp(lo, hi)))
}

/// The lobby's resource options as `mapgen.MapOptions` read them (`mapgen.py:73-93`): each
/// kind's density, and each resource's rule other than `normal`, as `[mode, value]`.
fn py_resources(v: &Value) -> Value {
    let none = Value::Null;
    let r = if v.is_object() { v } else { &none };
    let mut out = json!({"density": num(r.get("density"), 1.0, 0.0, 5.0)});
    for kind in ["strategic", "luxury", "bonus"] {
        let sub = r.get(kind).filter(|s| s.is_object()).unwrap_or(&none);
        let each: Map<String, Value> = obj(sub.get("each").unwrap_or(&none))
            .filter_map(|(name, rule)| {
                let mode = rule.get("mode").and_then(Value::as_str)?;
                let value = match mode {
                    "off" => json!(0.0),
                    "cap" | "share" => num(rule.get("value"), 0.0, 0.0, 10_000.0),
                    _ => return None,
                };
                Some((name.clone(), json!([mode, value])))
            })
            .collect();
        out[kind] = json!({"density": num(sub.get("density"), 1.0, 0.0, 5.0), "each": each});
    }
    out
}

fn py_settings(c: &Value) -> Value {
    let mut out = pick(
        c,
        &[
            "seed",
            "map_size",
            "speed",
            "difficulty",
            "barbarian_difficulty",
            "ai_base_values",
            "starting_era",
            "barbarians",
            "barbarian_aggression",
            "turn_limit",
            "victories",
            "city_states",
            "religion",
            "espionage",
            "nuclear_weapons",
            "tech_trading",
            "ruins",
            "river_density",
        ],
    );
    let (ty, map) = match c.get("map").and_then(Value::as_str) {
        Some(id) => ("custom".to_owned(), json!(id)),
        None => (c["map_type"].as_str().unwrap_or_default().to_owned(), Value::Null),
    };
    // An editor map's wraps are its own, and it has no edges setting (game.py:185-191).
    out["map_edges"] = match (&map, c.get("map_edges")) {
        (Value::String(_), _) => Value::Null,
        (_, Some(Value::String(e))) => json!(e),
        _ => json!("ice_caps"),
    };
    out["map_type"] = json!(ty);
    out["map"] = map;
    out["resources"] = py_resources(&c["resources"]);
    // A game's own chat limit, at least two (diplomacy.py:712-717).
    let chat = c["diplomacy"]["max_chat_messages"]
        .as_f64()
        .filter(|&m| m != 0.0)
        .map(|m| (m.trunc() as i64).max(2));
    out["diplomacy"] = json!({"max_chat_messages": chat});
    out["host"] = Value::Object(
        obj(c)
            .filter(|(k, _)| !ENGINE_KEYS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    );
    out
}

fn py_tiles(s: &Value) -> Value {
    let rows = arr(&s["tiles"]);
    let tiles: Vec<Value> = rows
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut features: Vec<Value> = arr(&t[1]).to_vec();
            if t[12] == json!(true) && !features.contains(&json!("Fallout")) {
                features.push(json!("Fallout"));
            }
            let build = match arr(&t[13]) {
                [] => Value::Null,
                steps => Value::Array(steps.to_vec()),
            };
            json!({
                "i": i, "terrain": t[0], "features": features, "wonder": t[2], "river": t[3],
                "resource": t[4], "resource_amount": t[5], "improvement": t[6],
                "pillaged": t[7], "route": t[8], "route_pillaged": t[9], "owner": t[10],
                "city": t[11], "build": build,
            })
        })
        .collect();
    Value::Array(tiles)
}

/// The fields every player projects as Python wrote them.
const PLAYER_FIELDS: [&str; 38] = [
    "id",
    "name",
    "kind",
    "nation",
    "leader",
    "color",
    "controller",
    "handicap",
    "auto",
    "difficulty",
    "alive",
    "eliminated_turn",
    "capital",
    "original_capital",
    "founded_city",
    "city_counter",
    "gold",
    "culture",
    "faith",
    "golden_age_points",
    "golden_age_turns",
    "golden_ages",
    "techs",
    "research_queue",
    "research_goal",
    "research_progress",
    "overflow_science",
    "free_techs",
    "future_techs",
    "policies",
    "policies_adopted_count",
    "free_policies",
    "religion_state",
    "religion",
    "gp_points",
    "gg_points",
    "gg_threshold",
    "free_great_people",
];

/// The scalar flags, each projected as its value, with 0, false and absent all read as nothing:
/// Python's readers defaulted a missing flag to 0 or false (`flags.get(k, 0)`).
const FLAG_FIELDS: [&str; 13] = [
    "start",
    "total_culture",
    "total_faith",
    "last_gold_rate",
    "ra_science",
    "maya_limited",
    "revolt_in",
    "election_in",
    "barb_help_cd",
    "recently_bullied",
    "cs_attacks",
    "cs_gp_gift",
    "choose_pantheon_belief",
];

/// A value with 0 and false read as nothing, for the scalar flags.
fn zn(v: Value) -> Value {
    match &v {
        Value::Bool(false) => Value::Null,
        Value::Number(n) if n.as_f64() == Some(0.0) => Value::Null,
        _ => v,
    }
}

/// A dict's entries whose values are not 0 or false.
fn nonzero_entries(v: &Value, keep: &[&str]) -> Value {
    Value::Object(
        obj(v)
            .filter(|(k, x)| keep.contains(&k.as_str()) || !zn((*x).clone()).is_null())
            .map(|(k, x)| (k.clone(), x.clone()))
            .collect(),
    )
}

fn py_players(s: &Value) -> Value {
    let players = arr(&s["players"]);
    let out: Vec<Value> = players
        .iter()
        .map(|p| {
            let mut o = pick(p, &PLAYER_FIELDS);
            let kind = p["kind"].as_str().unwrap_or_default();
            let flags = &p["flags"];
            let flag_or = |k: &str, d: Value| flags.get(k).cloned().unwrap_or(d);
            o["great_people_earned"] = p["great_people_earned"].clone();
            o["great_prophets_earned"] = p["great_prophets_earned"].clone();
            o["natural_wonders"] = p["natural_wonders"].clone();
            o["met"] = p["met"].clone();
            o["free_stat_buildings"] = p["free_stat_buildings"].clone();
            o["free_specific_buildings"] = p["free_specific_buildings"].clone();
            o["built_increasing"] = p["built_increasing"].clone();
            o["bought_increasing"] = p["bought_increasing"].clone();
            o["temp_uniques"] = p["temp_uniques"].clone();
            let ov = &p["overrides"];
            o["overrides"] = json!({
                "handicap": ov["handicap"].as_str().filter(|h| !h.is_empty()),
                "auto": ov.get("auto").filter(|a| !a.is_null()).cloned().unwrap_or(json!({})),
            });
            // Each tile explored, by index.
            o["explored"] = if kind == "barbarian" {
                Value::Null
            } else {
                let bytes =
                    b64_decode(p["explored"].as_str().unwrap_or_default()).unwrap_or_default();
                let tiles: Vec<usize> =
                    bytes.iter().enumerate().filter(|(_, b)| **b != 0).map(|(i, _)| i).collect();
                json!(tiles)
            };
            for k in FLAG_FIELDS {
                o[k] = zn(flag_or(k, Value::Null));
            }
            o["culture_last8"] = flag_or("culture_last8", json!([0, 0, 0, 0, 0, 0, 0, 0]));
            o["science_last8"] = flag_or("science_last8", json!([0, 0, 0, 0, 0, 0, 0, 0]));
            o["gp_threshold"] = flag_or("gp_threshold", json!({}));
            o["long_count_pool"] = flag_or("long_count_pool", json!([]));
            o["free_beliefs"] = nonzero_entries(&flag_or("free_beliefs", json!({})), &[]);
            o["last_ruins"] = flag_or("last_ruins", json!(["", ""]));
            o["skip_explore"] = flag_or("skip_explore", json!([]));
            o["eras_spy_earned"] = flag_or("eras_spy_earned", json!([]));
            o["gained"] = Value::Array(
                obj(flags)
                    .filter(|(k, v)| k.starts_with("gained_") && v.as_bool() == Some(true))
                    .map(|(k, _)| json!(k.trim_start_matches("gained_")))
                    .collect(),
            );
            if kind == "major" {
                o["spies"] = p["spies"].clone();
                o["notes"] = p["notes"].clone();
                // The last sight of each tile out of view (visibility.py:116-125).
                o["memory"] = Value::Object(
                    obj(&p["memory"])
                        .map(|(k, m)| {
                            let f = m.get("f").filter(|f| !f.is_null()).cloned();
                            let at = |key: &str| m.get(key).cloned().unwrap_or(Value::Null);
                            let seen = json!({
                                "f": f.unwrap_or(json!([])), "i": at("i"), "r": at("r"),
                                "o": at("o"), "p": m.get("p").cloned().unwrap_or(json!(false)),
                            });
                            (k.clone(), seen)
                        })
                        .collect(),
                );
                o["remembered_cities"] = Value::Array(
                    obj(&p["memory"])
                        .filter_map(|(k, m)| {
                            m.get("c")
                                .filter(|c| !c.is_null())
                                .map(|c| json!([k.parse::<u64>().ok(), c]))
                        })
                        .collect(),
                );
            }
            if kind == "city_state" {
                for k in ["cs_type", "cs_personality", "cs_resource", "cs_unique_unit", "ally"] {
                    o[k] = p[k].clone();
                }
                o["influence"] = Value::Object(
                    obj(&p["influence"])
                        .filter(|(_, x)| x.as_f64().is_some_and(|x| x.to_bits() != 0))
                        .map(|(k, x)| (k.clone(), x.clone()))
                        .collect(),
                );
                o["protectors"] = p["protectors"].clone();
                o["quests"] = Value::Array(
                    arr(&p["quests"])
                        .iter()
                        .map(|q| {
                            pick(
                                q,
                                &[
                                    "name",
                                    "assignee",
                                    "turn",
                                    "kind",
                                    "data1",
                                    "influence",
                                    "duration",
                                ],
                            )
                        })
                        .collect(),
                );
                o["pairs"] = Value::Object(
                    obj(&flags["pairs"])
                        .map(|(k, v)| (k.clone(), nonzero_entries(v, &["unit_timer"])))
                        .filter(|(_, v)| v.as_object().is_some_and(|m| !m.is_empty()))
                        .collect(),
                );
                let qs = flags.get("quest_state");
                o["quest_global"] = qs.map_or(json!(-1), |q| q["global"].clone());
                o["quest_individual"] = Value::Object(
                    qs.map(|q| &q["individual"])
                        .into_iter()
                        .flat_map(obj)
                        .filter(|(_, v)| v.as_i64() != Some(-1))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                );
                o["war_quests"] = flag_or("war_quests", json!({}));
            }
            o
        })
        .collect();
    // Spaceship parts moved from the game to the majors.
    let mut out = out;
    for (pid, parts) in obj(&s["spaceship"]) {
        if let Some(p) = pid.parse::<usize>().ok().and_then(|i| out.get_mut(i))
            && parts.as_object().is_some_and(|m| !m.is_empty())
        {
            p["spaceship"] = parts.clone();
        }
    }
    Value::Array(out)
}

fn py_units(s: &Value) -> Value {
    let players = arr(&s["players"]);
    let units: Vec<Value> = obj(&s["units"])
        .map(|(k, u)| {
            let mut o = pick(
                u,
                &[
                    "id",
                    "type",
                    "owner",
                    "idx",
                    "hp",
                    "moves",
                    "xp",
                    "promotions",
                    "promotion_count",
                    "pending_promotions",
                    "fortify",
                    "activity",
                    "goto",
                    "order_wait",
                    "attacks",
                    "interceptions",
                    "acted",
                    "name",
                    "camp",
                    "created_turn",
                    "religion",
                    "religious_strength",
                    "religious_strength_lost",
                    "abilities_used",
                    "carried_by",
                    "origin_city",
                    "original_owner",
                    "return_offer",
                ],
            );
            o["path"] = if u["path"].is_null() { json!([]) } else { u["path"].clone() };
            o["set_up"] = json!(arr(&u["status"]).contains(&json!("Set Up")));
            let owner = u["owner"].as_u64().and_then(|i| players.get(usize::try_from(i).ok()?));
            let flags = owner.map(|p| &p["flags"]);
            o["explore_target"] =
                flags.and_then(|f| f["explore_targets"].get(k)).cloned().unwrap_or(Value::Null);
            o["explore_recent"] =
                flags.and_then(|f| f["explore_hist"].get(k)).cloned().unwrap_or(json!([]));
            o
        })
        .collect();
    Value::Array(units)
}

fn py_cities(s: &Value) -> Value {
    let players = arr(&s["players"]);
    let cities: Vec<Value> = obj(&s["cities"])
        .map(|(k, c)| {
            let mut o = pick(
                c,
                &[
                    "id",
                    "name",
                    "owner",
                    "idx",
                    "pop",
                    "food",
                    "culture",
                    "tiles_claimed",
                    "tiles_bought",
                    "buildings",
                    "queue",
                    "progress",
                    "overflow",
                    "health",
                    "worked",
                    "locked",
                    "manual_specialists",
                    "focus",
                    "founded_turn",
                    "founder",
                    "previous_owner",
                    "original_capital",
                    "puppet",
                    "resistance",
                    "attacked",
                    "sacked_turn",
                    "razing",
                    "damaged_turn",
                    "auto_production",
                    "religions_adopted",
                    "holy_city_of",
                    "wltkd",
                    "demanded_resource",
                    "demand_countdown",
                    "bought_this_turn",
                    "avoid_growth",
                    "turn_acquired",
                ],
            );
            o["specialists"] = Value::Object(
                obj(&c["specialists"])
                    .filter(|(_, n)| n.as_u64() != Some(0))
                    .map(|(k, n)| (k.clone(), n.clone()))
                    .collect(),
            );
            // Seeded by the first read of the city's religion (religion.py:105-109).
            o["pressures"] = match c["pressures"].as_object() {
                Some(m) if !m.is_empty() => c["pressures"].clone(),
                _ => json!({"None": 100}),
            };
            // The owner's free buildings of this city join the city's own (cities.py:285-293).
            let mut free: Vec<Value> = arr(&c["free_buildings"]).to_vec();
            let owner = c["owner"].as_u64().and_then(|i| players.get(usize::try_from(i).ok()?));
            for b in owner.map_or(&[][..], |p| arr(&p["free_buildings"][k])) {
                if !free.contains(b) {
                    free.push(b.clone());
                }
            }
            o["free_buildings"] = Value::Array(free);
            o
        })
        .collect();
    Value::Array(cities)
}

/// A relation's projection, as the Rust relation holds it: the pair, contact, war and the
/// turns; two-sided fields by player.
fn py_diplomacy(s: &Value) -> Value {
    let players = arr(&s["players"]);
    let mut pairs: Map<String, Value> = Map::new();
    let mut opinions: Vec<Value> = Vec::new();
    let fresh = |a: usize, b: usize| {
        json!({"pair": format!("{a},{b}"), "met": false, "war": false, "war_declared_by": null,
               "since": 0, "treaty_until": 0, "friendship_until": 0, "pact_until": 0, "ra_until": 0,
               "ra_science": {}, "embassy": [], "denounced_until": {}, "open_borders_until": {}})
    };
    for (a, p) in players.iter().enumerate() {
        for b in arr(&p["met"]).iter().filter_map(Value::as_u64) {
            let b = usize::try_from(b).unwrap_or(usize::MAX);
            if a < b {
                let e = pairs.entry(format!("{a},{b}")).or_insert_with(|| fresh(a, b));
                e["met"] = json!(true);
            }
        }
    }
    for (key, r) in obj(&s["relations"]) {
        let (a, b) = key.split_once(',').unwrap_or_default();
        let (a, b): (usize, usize) = (a.parse().unwrap_or(0), b.parse().unwrap_or(0));
        let e = pairs.entry(key.clone()).or_insert_with(|| fresh(a, b));
        for k in [
            "war",
            "war_declared_by",
            "since",
            "treaty_until",
            "friendship_until",
            "pact_until",
            "ra_until",
        ] {
            e[k] = r[k].clone();
        }
        e["ra_science"] = Value::Object(
            obj(&r["ra_science"])
                .filter(|(_, v)| v.as_i64() != Some(0))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        );
        e["embassy"] = Value::Array(
            obj(&r["embassy"])
                .filter(|(_, v)| v.as_bool() == Some(true))
                .map(|(k, _)| json!(k))
                .collect(),
        );
        e["denounced_until"] = Value::Object(
            obj(&r["denounced_by"])
                .filter(|(_, v)| v.as_i64() != Some(0))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        );
        for (holder, values) in obj(&r["opinion"]) {
            let (h, about) = match holder.split_once('>') {
                Some((h, x)) => (h.parse().unwrap_or(0), x.parse().unwrap_or(0)),
                None => {
                    let h: usize = holder.parse().unwrap_or(0);
                    (h, if h == a { b } else { a })
                }
            };
            let nonzero: Map<String, Value> = obj(values)
                .filter(|(_, v)| v.as_f64().is_some_and(|x| x.to_bits() != 0))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if !nonzero.is_empty() {
                opinions.push(json!({"holder": h, "about": about, "values": nonzero}));
            }
        }
    }
    for (key, until) in obj(&s["open_borders"]) {
        let (x, y) = key.split_once('>').unwrap_or_default();
        let (x, y): (usize, usize) = (x.parse().unwrap_or(0), y.parse().unwrap_or(0));
        let pair = format!("{},{}", x.min(y), x.max(y));
        let e = pairs.entry(pair).or_insert_with(|| fresh(x.min(y), x.max(y)));
        if until.as_i64() != Some(0) {
            e["open_borders_until"][x.to_string()] = until.clone();
        }
    }
    let default = |a: &str, b: &str| fresh(a.parse().unwrap_or(0), b.parse().unwrap_or(0));
    let relations: Vec<Value> = pairs
        .into_iter()
        .filter(|(k, v)| {
            let (a, b) = k.split_once(',').unwrap_or_default();
            *v != default(a, b)
        })
        .map(|(_, v)| v)
        .collect();
    let deals: Vec<Value> = arr(&s["deals"])
        .iter()
        .map(|d| pick(d, &["id", "turn", "parties", "terms", "ongoing", "active", "summary"]))
        .collect();
    let negotiations: Vec<Value> = arr(&s["negotiations"])
        .iter()
        .map(|n| {
            let mut o = pick(
                n,
                &[
                    "id",
                    "initiator",
                    "responder",
                    "turn",
                    "status",
                    "awaiting",
                    "proposal",
                    "proposal_by",
                    "deal_id",
                ],
            );
            o["history"] = Value::Array(
                arr(&n["history"])
                    .iter()
                    .map(|h| {
                        let mut e =
                            pick(h, &["seq", "by", "action", "message", "proposal", "turn"]);
                        e["note"] = h.get("note").cloned().unwrap_or(Value::Null);
                        e
                    })
                    .collect(),
            );
            o
        })
        .collect();
    json!({"relations": relations, "opinions": opinions, "deals": deals, "negotiations": negotiations})
}

fn py_world(s: &Value) -> Value {
    let religions: Vec<Value> = obj(&s["religions"])
        .map(|(_, r)| {
            pick(
                r,
                &[
                    "name",
                    "display",
                    "founder",
                    "founder_beliefs",
                    "follower_beliefs",
                    "blocked_holy",
                ],
            )
        })
        .collect();
    let un = &s["un"];
    let results = match un.get("results") {
        Some(r) if !r.is_null() => py_result(r),
        _ => Value::Null,
    };
    let camps: Vec<Value> = obj(&s["camps"])
        .map(|(k, c)| {
            json!({"id": k.parse::<u64>().ok(), "idx": c["idx"], "countdown": c["countdown"],
                   "spawned": c["spawned"], "destroyed": c["destroyed"]})
        })
        .collect();
    json!({
        "religions": religions,
        "wonders_built": s["wonders_built"],
        "un": {"next_vote": un.get("next_vote").cloned().unwrap_or(Value::Null),
               "votes": un.get("votes").cloned().unwrap_or(json!({})),
               "results": results,
               "won": un.get("won").cloned().unwrap_or(json!([])),
               "processed_turn": un.get("processed_turn").cloned().unwrap_or(Value::Null)},
        "camps": camps,
    })
}

fn py_result(r: &Value) -> Value {
    let tally: Vec<Value> = obj(&r["tally"]).map(|(name, n)| json!([name, n])).collect();
    json!({"turn": r["turn"], "tally": tally, "votes_needed": r["votes_needed"], "winner": r["winner"]})
}

fn py_history(s: &Value) -> Value {
    let events: Vec<Value> = arr(&s["events"])
        .iter()
        .map(|e| {
            let mut data: Map<String, Value> = obj(&e["data"])
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if let Some(r) = data.get("results").cloned() {
                data.insert("results".to_owned(), py_result(&r));
            }
            json!({
                "id": e["id"], "turn": e["turn"], "type": e["type"], "text": e["text"],
                "players": e["players"], "idx": e["idx"],
                "refs": e.get("refs").cloned().unwrap_or(json!([])),
                "data": data,
            })
        })
        .collect();
    let stats: Vec<Value> = arr(&s["stats"])
        .iter()
        .map(|row| {
            let players: Map<String, Value> = obj(&row["players"])
                .map(|(k, p)| {
                    let mut o = p.clone();
                    if o["alive"] == json!(false) {
                        o = json!({"alive": false, "score": 0});
                    }
                    (k.clone(), o)
                })
                .collect();
            json!({"turn": row["turn"], "players": players})
        })
        .collect();
    json!({
        "events": events,
        "messages": s["messages"],
        "thoughts": s["thoughts"],
        "stats": stats,
    })
}

// ---- The Rust side ------------------------------------------------------------------------------

/// The projection of the loaded game.
pub fn rust(r: &Ruleset, g: &Game) -> Value {
    let st = g.state();
    let n = Names { r, st };
    let clock = st.clock();
    // Python's one counter for units, cities and camps; three counters that part show apart.
    let ids = st.ids();
    let next_id = if ids.unit == ids.city && ids.unit == ids.camp {
        json!(ids.unit)
    } else {
        json!([ids.unit, ids.city, ids.camp])
    };
    json!({
        "clock": {"turn": clock.turn, "current": clock.current.0, "turn_started": clock.turn_started,
                  "phase": match clock.phase { citar_engine::state::Phase::Playing => "playing", citar_engine::state::Phase::Over => "over" },
                  "winner": clock.winner.map(|p| p.0), "victory": clock.victory.map(|v| n.of(v)),
                  "next_id": next_id},
        "map": rs_map(st),
        "settings": rs_settings(&n),
        "tiles": rs_tiles(&n),
        "players": rs_players(&n),
        "units": rs_units(&n),
        "cities": rs_cities(&n),
        "diplomacy": rs_diplomacy(&n),
        "world": rs_world(&n),
        "history": rs_history(&n, g.chronicle()),
    })
}

/// Names of rule objects, for the Rust side.
struct Names<'a> {
    r: &'a Ruleset,
    st: &'a State,
}

impl Names<'_> {
    fn of<I: Named>(&self, id: I) -> Value {
        self.r.name(id).map_or(Value::Null, Value::from)
    }

    fn set<I: Named, const W: usize>(&self, set: &IdSet<I, W>) -> Value {
        Value::Array(set.iter().map(|id| self.of(id)).collect())
    }

    fn feature(&self, f: FeatureId) -> Value {
        self.r.derived().features.get(f).map_or(Value::Null, |&t| self.of(t))
    }

    fn item(&self, c: Constructible) -> Value {
        match c {
            Constructible::Building(b) => self.of(b),
            Constructible::Unit(u) => self.of(u),
            Constructible::Perpetual(p) => json!(p.name()),
        }
    }

    fn religion(&self, id: ReligionId) -> Value {
        match self.st.world().religion(id).map(|x| x.name) {
            Some(ReligionName::Pantheon(b)) => self.of(b),
            Some(ReligionName::Religion(x)) => {
                self.r.religions().get(x).map_or(Value::Null, |n| json!(&**n))
            }
            None => Value::Null,
        }
    }

    fn quest(&self, q: QuestKindId) -> Value {
        self.r.quests().get(q).map_or(Value::Null, |d| json!(&*d.name))
    }

    fn ruin(&self, x: RuinId) -> Value {
        self.r.ruins().get(x).map_or(Value::Null, |d| json!(&*d.name))
    }

    fn cs_type(&self, x: CityStateTypeId) -> Value {
        self.r.city_state_types().get(x).map_or(Value::Null, |d| json!(&*d.name))
    }

    fn text(&self, t: TextId) -> Value {
        json!(self.r.uniques().text(t))
    }

    fn player_name(&self, p: PlayerId) -> Value {
        self.st.player(p).map_or(Value::Null, |x| json!(&*x.name))
    }
}

fn opt<T>(x: Option<T>, f: impl FnOnce(T) -> Value) -> Value {
    x.map_or(Value::Null, f)
}

fn rs_map(st: &State) -> Value {
    let m = st.map();
    // Water is -1 in Python.
    let continents: Vec<i64> =
        m.continents.iter().map(|&c| if c == WATER { -1 } else { i64::from(c) }).collect();
    json!({"width": m.width, "height": m.height, "wrap_x": m.wrap_x, "wrap_y": m.wrap_y,
           "continents": continents})
}

fn rs_resources(n: &Names<'_>, o: &ResourceOptions) -> Value {
    let kind = |k: &ResourceKindOptions| {
        let each: Map<String, Value> = k
            .each
            .iter()
            .map(|&(res, rule)| {
                let rule = match rule {
                    ResourceRule::Off => json!(["off", 0.0]),
                    ResourceRule::Cap(v) => json!(["cap", v]),
                    ResourceRule::Share(v) => json!(["share", v]),
                };
                (n.of(res).as_str().unwrap_or_default().to_owned(), rule)
            })
            .collect();
        json!({"density": k.density, "each": each})
    };
    json!({"density": o.density, "strategic": kind(&o.strategic), "luxury": kind(&o.luxury),
           "bonus": kind(&o.bonus)})
}

fn rs_settings(n: &Names<'_>) -> Value {
    let (r, c) = (n.r, n.st.config());
    let (size, ty, map, edges) = match &c.map {
        MapSource::Generated { size, map_type, edges, .. } => (
            *size,
            r.constants().map_types.get(*map_type).map_or(Value::Null, |t| json!(&*t.key)),
            Value::Null,
            json!(edges.name()),
        ),
        MapSource::Editor { id, size } => (*size, json!("custom"), json!(&**id), Value::Null),
    };
    let victories: Map<String, Value> = r
        .victories()
        .iter()
        .map(|(v, d)| (d.name.to_string(), json!(c.victory_enabled(v))))
        .collect();
    json!({
        "seed": c.seed,
        "map_size": r.map_sizes().get(size).map(|s| &*s.key),
        "map_type": ty,
        "map": map,
        "speed": n.of(c.speed),
        "difficulty": n.of(c.difficulty),
        "barbarian_difficulty": n.of(c.barbarian_difficulty),
        "ai_base_values": match c.ai_base_values {
            citar_engine::state::config::AiBaseValues::Unciv => "unciv",
            citar_engine::state::config::AiBaseValues::Monotonic => "monotonic",
        },
        "starting_era": n.of(c.starting_era),
        "barbarians": r.constants().barbarian_levels.get(c.barbarians).map(|b| &*b.key),
        "barbarian_aggression": c.barbarian_aggression,
        "turn_limit": c.turn_limit,
        "victories": victories,
        "city_states": c.city_states,
        "religion": c.religion,
        "espionage": c.espionage,
        "nuclear_weapons": c.nuclear_weapons,
        "tech_trading": c.tech_trading,
        "ruins": c.ruins,
        "river_density": c.river_density,
        "map_edges": edges,
        "resources": rs_resources(n, &c.resources),
        "diplomacy": {"max_chat_messages": c.diplomacy.max_chat_messages},
        "host": Value::Object(c.host.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
    })
}

fn rs_tiles(n: &Names<'_>) -> Value {
    let tiles = n.st.tiles();
    let out: Vec<Value> = tiles
        .iter()
        .map(|(i, t)| {
            let build: Vec<Value> = tiles
                .builds(i)
                .iter()
                .map(|s| json!([n.of(s.improvement), s.turns_left]))
                .collect();
            json!({
                "i": i.0,
                "terrain": n.of(t.terrain()),
                "features": t.features().iter().map(|f| n.feature(f)).collect::<Vec<_>>(),
                "wonder": opt(t.wonder(), |w| n.of(w)),
                "river": t.river_mask(),
                "resource": opt(t.resource(), |x| n.of(x)),
                "resource_amount": t.resource_amount(),
                "improvement": opt(t.improvement(), |x| n.of(x)),
                "pillaged": t.improvement_pillaged(),
                "route": opt(t.route(), |x| json!(route_name(x))),
                "route_pillaged": t.route_pillaged(),
                "owner": t.owner().map(|p| p.0),
                "city": t.city().map(|c| c.get()),
                "build": if build.is_empty() { Value::Null } else { Value::Array(build) },
            })
        })
        .collect();
    Value::Array(out)
}

fn rs_players(n: &Names<'_>) -> Value {
    let (r, st) = (n.r, n.st);
    let out: Vec<Value> = st.players().iter().map(|(id, p)| rs_player(n, r, st, id, p)).collect();
    Value::Array(out)
}

fn rs_player(n: &Names<'_>, r: &Ruleset, st: &State, id: PlayerId, p: &Player) -> Value {
    let seat = p.seat();
    let kind = if p.is_major() {
        "major"
    } else if p.is_city_state() {
        "city_state"
    } else {
        "barbarian"
    };
    let map_f =
        |m: &std::collections::BTreeMap<citar_engine::base::ids::BaseUnitId, f64>| -> Value {
            Value::Object(
                m.iter()
                    .map(|(&u, &x)| (r.name(u).unwrap_or_default().to_owned(), json!(x)))
                    .collect(),
            )
        };
    let met: Vec<u8> = st.diplo().met_mask(id).iter().map(|q| q.0).collect();
    let mut o = json!({
        "id": id.0,
        "name": &*p.name,
        "kind": kind,
        "nation": n.of(p.nation),
        "leader": &*p.leader,
        "color": p.color.to_hex(),
        "controller": seat.controller().name(),
        "handicap": seat.handicap().name(),
        "auto": {"un_vote": seat.auto().un_vote, "conquest": seat.auto().conquest, "free_picks": seat.auto().free_picks},
        "difficulty": opt(seat.difficulty(), |d| n.of(d)),
        "alive": p.alive(),
        "eliminated_turn": p.eliminated_turn(),
        "capital": p.capital.map(|c| c.get()),
        "original_capital": p.original_capital.map(|c| c.get()),
        "founded_city": p.founded_city,
        "city_counter": p.city_counter,
        "gold": p.econ.gold,
        "culture": p.econ.culture,
        "faith": p.econ.faith,
        "golden_age_points": p.econ.golden_age_points,
        "golden_age_turns": p.econ.golden_age_turns,
        "golden_ages": p.econ.golden_ages,
        "techs": n.set(&p.tech.known),
        "research_queue": p.tech.queue.iter().map(|&t| n.of(t)).collect::<Vec<_>>(),
        "research_goal": opt(p.tech.goal, |t| n.of(t)),
        "research_progress": Value::Object(p.tech.progress.iter().map(|(&t, &x)| (r.name(t).unwrap_or_default().to_owned(), json!(x))).collect()),
        "overflow_science": p.tech.overflow,
        "free_techs": p.tech.free_techs,
        "future_techs": p.tech.future_techs,
    });
    let more = json!({
        "policies": n.set(&p.policy.adopted),
        "policies_adopted_count": p.policy.adopted_count,
        "free_policies": p.policy.free_policies,
        "religion_state": p.religion.progress.name(),
        "religion": opt(p.religion.founded, |x| n.religion(x)),
        "gp_points": map_f(&p.gp.points),
        "gg_points": map_f(&p.gp.combat_points),
        "gg_threshold": Value::Object(p.gp.combat_threshold.iter().map(|(&u, &x)| (r.name(u).unwrap_or_default().to_owned(), json!(x))).collect()),
        "free_great_people": p.gp.free,
    });
    let rest = json!({
        "great_people_earned": p.gp.earned,
        "great_prophets_earned": p.gp.prophets_earned,
        "natural_wonders": n.set(&p.civ.natural_wonders),
        "met": met,
        "free_stat_buildings": p.civ.free_stat_buildings.iter().map(|(s, c)| json!([s.name(), c.get()])).collect::<Vec<_>>(),
        "free_specific_buildings": p.civ.free_specific_buildings.iter().map(|&(b, c)| json!([n.of(b), c.get()])).collect::<Vec<_>>(),
        "built_increasing": Value::Object(p.civ.built_increasing.iter().map(|(&c, &x)| (n.item(c).as_str().unwrap_or_default().to_owned(), json!(x))).collect()),
        "bought_increasing": Value::Object(p.civ.bought_increasing.iter().map(|(&c, &x)| (n.item(c).as_str().unwrap_or_default().to_owned(), json!(x))).collect()),
        "temp_uniques": p.civ.temp_uniques.iter().map(|t| json!({"text": r.uniques().text_of(t.unique), "turns": t.turns})).collect::<Vec<_>>(),
        "explored": if p.is_barbarian() { Value::Null } else { json!(p.explored.iter().collect::<Vec<u32>>()) },
        "overrides": {
            "handicap": seat.overrides().handicap.map(|h| h.name()),
            "auto": Value::Object(AutoDecision::ALL.into_iter().filter_map(|d| {
                seat.overrides().auto.get(d).map(|on| (d.name().to_owned(), json!(on)))
            }).collect()),
        },
        "start": zn(json!(p.start_tile.map(|t| t.0))),
        "total_culture": zn(json!(p.econ.total_culture)),
        "total_faith": zn(json!(p.econ.total_faith)),
        "last_gold_rate": zn(json!(p.econ.last_gold_rate)),
        "ra_science": zn(json!(p.tech.ra_bonus)),
        "maya_limited": zn(json!(p.gp.maya_limited)),
        "revolt_in": zn(json!(p.civ.revolt_in)),
        "choose_pantheon_belief": zn(json!(p.religion.choose_pantheon_belief)),
        "culture_last8": p.econ.culture_hist,
        "science_last8": p.econ.science_hist,
        "gp_threshold": Value::Object(p.gp.pool_threshold.iter().map(|(k, &x)| (k.map_or_else(String::new, |t| n.text(t).as_str().unwrap_or_default().to_owned()), json!(x))).collect()),
        "long_count_pool": p.gp.long_count_pool.iter().map(|&u| n.of(u)).collect::<Vec<_>>(),
        "free_beliefs": Value::Object(citar_engine::rules::defs::BeliefKind::ALL.into_iter().filter(|&k| p.religion.free(k) != 0).map(|k| (k.name().to_owned(), json!(p.religion.free(k)))).collect()),
        "last_ruins": p.civ.last_ruins.map(|x| x.map_or_else(|| json!(""), |x| n.ruin(x))),
        "skip_explore": p.civ.explore_skip.iter().map(|t| t.0).collect::<Vec<_>>(),
        "gained": p.civ.units_gained.iter().map(|u| n.of(u)).collect::<Vec<_>>(),
        "eras_spy_earned": p.major.as_ref().map_or_else(|| json!([]), |m| n.set(&m.spy_eras_earned)),
        "election_in": zn(json!(p.city_state.as_ref().and_then(|c| c.election_in))),
        "barb_help_cd": zn(json!(p.city_state.as_ref().map(|c| c.barb_help_cd))),
        "recently_bullied": zn(json!(p.city_state.as_ref().map(|c| c.recently_bullied))),
        "cs_attacks": zn(json!(p.major.as_ref().map(|m| m.cs_attacks))),
        "cs_gp_gift": zn(json!(p.major.as_ref().and_then(|m| m.cs_gp_gift))),
    });
    for part in [more, rest] {
        if let (Some(a), Value::Object(b)) = (o.as_object_mut(), part) {
            a.extend(b);
        }
    }
    if let Some(m) = &p.major {
        o["spies"] = Value::Array(
            m.spies
                .iter()
                .map(|s| {
                    json!({"name": &*s.name, "rank": s.rank, "city": s.city.map(|c| c.get()),
                           "action": s.action.name(), "turns": s.turns, "progress": s.progress})
                })
                .collect(),
        );
        o["notes"] = json!(&*m.notes);
        o["memory"] = rs_memory(n, &m.memory);
        o["remembered_cities"] = Value::Array(
            m.memory.cities().map(|(t, c)| json!([t.0, [&*c.name, c.pop, c.owner.0]])).collect(),
        );
        if !m.spaceship.is_empty() {
            o["spaceship"] = Value::Object(
                m.spaceship
                    .iter()
                    .map(|(&u, &x)| (r.name(u).unwrap_or_default().to_owned(), json!(x)))
                    .collect(),
            );
        }
    }
    if let Some(cs) = &p.city_state {
        o["cs_type"] = opt(cs.cs_type, |t| n.cs_type(t));
        o["cs_personality"] = opt(cs.personality, |x| json!(x.name()));
        o["cs_resource"] = opt(cs.resource, |x| n.of(x));
        o["cs_unique_unit"] = opt(cs.unique_unit, |x| n.of(x));
        o["ally"] = json!(cs.ally().map(|a| a.0));
        o["influence"] = Value::Object(
            cs.influence
                .iter()
                .filter(|(_, x)| x.to_bits() != 0)
                .map(|(q, &x)| (q.0.to_string(), json!(x)))
                .collect(),
        );
        o["protectors"] = json!(cs.protectors.iter().map(|q| q.0).collect::<Vec<_>>());
        o["quests"] = Value::Array(
            cs.quests
                .iter()
                .map(|q| {
                    let data1 = match q.target {
                        QuestTarget::None => json!(""),
                        QuestTarget::Tile(t) => json!(t.0),
                        QuestTarget::Resource(x) => n.of(x),
                        QuestTarget::Building(x) => n.of(x),
                        QuestTarget::UnitType(x) => n.of(x),
                        QuestTarget::Player(x) => json!(x.0),
                        QuestTarget::NaturalWonder(x) => n.of(x),
                        QuestTarget::Religion(x) => n.religion(x),
                        QuestTarget::Baseline(x) => json!(x),
                        QuestTarget::Percent(x) => json!(x),
                    };
                    let scope = match q.scope {
                        citar_engine::rules::defs::QuestScope::Individual => "individual",
                        citar_engine::rules::defs::QuestScope::Global => "global",
                    };
                    json!({"name": n.quest(q.kind), "assignee": q.assignee.0, "turn": q.turn, "kind": scope,
                           "data1": data1, "influence": q.influence, "duration": q.duration})
                })
                .collect(),
        );
        o["pairs"] = Value::Object(
            cs.pairs
                .iter()
                .filter(|(_, pair)| **pair != citar_engine::state::players::CsPair::default())
                .map(|(q, pair)| {
                    let mut m = Map::new();
                    for (k, x) in [
                        ("bullied", pair.bullied),
                        ("pledged", pair.pledged),
                        ("withdrew", pair.withdrew),
                        ("border_conflict", pair.border_conflict),
                        ("anger_free", pair.anger_free),
                        ("recently_attacked", pair.recently_attacked),
                        ("marriage_cooldown", pair.marriage_cooldown),
                    ] {
                        if x != 0 {
                            m.insert(k.to_owned(), json!(x));
                        }
                    }
                    if let Some(t) = pair.unit_timer {
                        m.insert("unit_timer".to_owned(), json!(t));
                    }
                    if pair.wary {
                        m.insert("wary".to_owned(), json!(true));
                    }
                    (q.0.to_string(), Value::Object(m))
                })
                .collect(),
        );
        o["quest_global"] = json!(cs.timers.global);
        o["quest_individual"] = Value::Object(
            cs.timers.individual.iter().map(|(q, &x)| (q.0.to_string(), json!(x))).collect(),
        );
        o["war_quests"] = Value::Object(
            cs.war_quests
                .iter()
                .map(|(q, w)| {
                    let kills: Map<String, Value> =
                        w.kills.iter().map(|(k, &x)| (k.0.to_string(), json!(x))).collect();
                    (q.0.to_string(), json!({"needed": w.needed, "kills": kills}))
                })
                .collect(),
        );
    }
    o
}

/// A major's memory in Python's form, `{tile: {f, i, r, o, p}}`, for each tile remembered. The
/// route's own pillage bit, which Python's memory never held, shows only when set.
fn rs_memory(n: &Names<'_>, layer: &TileMemoryLayer) -> Value {
    Value::Object(
        layer
            .tiles()
            .iter()
            .enumerate()
            .filter(|(_, m)| m.is_remembered())
            .map(|(i, m)| {
                let route = m.route();
                let mut seen = json!({
                    "f": m.features().iter().map(|f| n.feature(f)).collect::<Vec<_>>(),
                    "i": opt(m.improvement(), |x| n.of(x)),
                    "r": opt(route.route(), |x| json!(route_name(x))),
                    "o": m.owner().map(|p| p.0),
                    "p": route.improvement_pillaged(),
                });
                if route.route_pillaged() {
                    seen["rp"] = json!(true);
                }
                (i.to_string(), seen)
            })
            .collect(),
    )
}

fn route_name(r: Route) -> &'static str {
    match r {
        Route::Road => "Road",
        Route::Railroad => "Railroad",
    }
}

fn rs_units(n: &Names<'_>) -> Value {
    let r = n.r;
    let out: Vec<Value> = n
        .st
        .units()
        .iter()
        .map(|u| {
            let abilities: Map<String, Value> = u
                .abilities_used
                .iter()
                .map(|&(k, x)| (r.uniques().ability(k).to_owned(), json!(x)))
                .collect();
            json!({
                "id": u.id().get(), "type": n.of(u.base), "owner": u.owner().0, "idx": u.tile().0,
                "hp": u.hp, "moves": u.moves, "xp": u.xp, "promotions": n.set(&u.promotions),
                "promotion_count": u.promotion_count, "pending_promotions": u.pending_promotions,
                "fortify": u.fortify, "activity": u.activity.map(|a| a.name()),
                "goto": u.goto.map(|t| t.0), "path": u.path.iter().map(|t| t.0).collect::<Vec<_>>(),
                "order_wait": u.order_wait, "attacks": u.attacks, "interceptions": u.interceptions,
                "acted": u.acted, "set_up": u.set_up, "name": u.name.as_deref(),
                "camp": u.camp.map(|c| c.get()), "created_turn": u.created_turn,
                "religion": opt(u.religion, |x| n.religion(x)),
                "religious_strength": u.religious_strength,
                "religious_strength_lost": u.religious_strength_lost,
                "abilities_used": abilities, "carried_by": u.carried_by().map(|c| c.get()),
                "origin_city": u.origin_city.map(|c| c.get()),
                "original_owner": u.original_owner.map(|p| p.0),
                "return_offer": u.return_offer.map(|p| p.0),
                "explore_target": u.explore.target.map(|t| t.0),
                "explore_recent": u.explore.recent.iter().map(|t| t.0).collect::<Vec<_>>(),
            })
        })
        .collect();
    Value::Array(out)
}

fn rs_cities(n: &Names<'_>) -> Value {
    let r = n.r;
    let out: Vec<Value> = n
        .st
        .cities()
        .iter()
        .map(|c| {
            let specialists: Map<String, Value> = r
                .specialists()
                .iter()
                .filter_map(|(s, d)| {
                    let k = c.specialists.get(s.index()).copied().unwrap_or(0);
                    (k != 0).then(|| (d.name.to_string(), json!(k)))
                })
                .collect();
            let pressures: Map<String, Value> = c
                .pressures
                .iter()
                .map(|&(x, v)| {
                    let k = x.map_or_else(|| json!("None"), |x| n.religion(x));
                    (k.as_str().unwrap_or_default().to_owned(), json!(v))
                })
                .collect();
            json!({
                "id": c.id().get(), "name": &*c.name, "owner": c.owner().0, "idx": c.tile().0,
                "pop": c.pop, "food": c.food, "culture": c.culture,
                "tiles_claimed": c.tiles_claimed, "tiles_bought": c.tiles_bought,
                "buildings": n.set(&c.buildings), "free_buildings": n.set(&c.free_buildings),
                "queue": c.queue.iter().map(|&x| n.item(x)).collect::<Vec<_>>(),
                "progress": Value::Object(c.progress.iter().map(|(&x, &v)| (n.item(x).as_str().unwrap_or_default().to_owned(), json!(v))).collect()),
                "overflow": c.overflow, "health": c.health,
                "worked": c.worked.iter().map(|t| t.0).collect::<Vec<_>>(),
                "locked": c.locked.iter().map(|t| t.0).collect::<Vec<_>>(),
                "specialists": specialists,
                "manual_specialists": c.manual_specialists, "focus": c.focus.name(),
                "founded_turn": c.founded_turn, "founder": c.founder.0,
                "previous_owner": c.previous_owner.map(|p| p.0),
                "original_capital": c.original_capital, "puppet": c.puppet,
                "resistance": c.resistance, "attacked": c.attacked, "sacked_turn": c.sacked_turn,
                "razing": c.razing, "damaged_turn": c.damaged_turn,
                "auto_production": c.auto_production, "pressures": pressures,
                "religions_adopted": c.religions_adopted.iter().map(|&x| n.religion(x)).collect::<Vec<_>>(),
                "holy_city_of": opt(c.holy_city_of, |x| n.religion(x)),
                "wltkd": c.wltkd, "demanded_resource": opt(c.demanded_resource, |x| n.of(x)),
                "demand_countdown": c.demand_countdown,
                "bought_this_turn": c.bought_this_turn.iter().map(|&x| n.item(x)).collect::<Vec<_>>(),
                "avoid_growth": c.avoid_growth, "turn_acquired": c.turn_acquired,
            })
        })
        .collect();
    Value::Array(out)
}

fn rs_relation(lo: PlayerId, hi: PlayerId, rel: &Relation) -> Value {
    let by = |xs: [i32; 2]| -> Value {
        let mut m = Map::new();
        for (who, x) in [(lo, xs[side(lo, hi)]), (hi, xs[side(hi, lo)])] {
            if x != 0 {
                m.insert(who.0.to_string(), json!(x));
            }
        }
        Value::Object(m)
    };
    let mut embassy = Vec::new();
    for (holder, host) in [(lo, hi), (hi, lo)] {
        if rel.embassy[side(holder, host)] {
            embassy.push(json!(format!("{}>{}", holder.0, host.0)));
        }
    }
    json!({
        "pair": format!("{},{}", lo.0, hi.0), "met": rel.met, "war": rel.war,
        "war_declared_by": rel.war_declared_by.map(|p| p.0), "since": rel.since,
        "treaty_until": rel.treaty_until, "friendship_until": rel.friendship_until,
        "pact_until": rel.pact_until, "ra_until": rel.ra_until,
        "ra_science": by(rel.ra_science), "embassy": embassy,
        "denounced_until": by(rel.denounced_until), "open_borders_until": by(rel.open_borders_until),
    })
}

fn rs_terms(r: &Ruleset, t: &Terms) -> Value {
    t.to_json(r).unwrap_or(Value::Null)
}

fn rs_deal(r: &Ruleset, d: &Deal) -> Value {
    let ongoing: Vec<Value> = d
        .ongoing
        .iter()
        .map(|o| {
            let mut v = o.item.to_json(r).unwrap_or(Value::Null);
            v["from"] = json!(o.from.0);
            v["to"] = json!(o.to.0);
            v["until"] = json!(o.until);
            v
        })
        .collect();
    json!({"id": d.id.get(), "turn": d.turn, "parties": [d.parties[0].0, d.parties[1].0],
           "terms": rs_terms(r, &d.terms), "ongoing": ongoing, "active": d.active,
           "summary": &*d.summary})
}

fn rs_negotiation(r: &Ruleset, x: &Negotiation) -> Value {
    let history: Vec<Value> = x
        .history
        .iter()
        .map(|h| {
            json!({"seq": h.seq, "by": h.by.map(|p| p.0), "action": h.action.name(),
                   "message": &*h.message, "proposal": h.proposal.as_ref().map(|t| rs_terms(r, t)),
                   "turn": h.turn, "note": h.note.as_deref()})
        })
        .collect();
    json!({"id": x.id.get(), "initiator": x.initiator.0, "responder": x.responder.0, "turn": x.turn,
           "status": x.status.name(), "awaiting": x.awaiting.map(|p| p.0),
           "proposal": x.proposal.as_ref().map(|t| rs_terms(r, t)),
           "proposal_by": x.proposal_by.map(|p| p.0), "deal_id": x.deal.map(|d| d.get()),
           "history": history})
}

fn rs_diplomacy(n: &Names<'_>) -> Value {
    let d = n.st.diplo();
    let fresh = Relation::default();
    let relations: Vec<Value> = d
        .relations()
        .pairs()
        .filter(|(_, _, rel)| **rel != fresh)
        .map(|(lo, hi, rel)| rs_relation(lo, hi, rel))
        .collect();
    let opinions: Vec<Value> = d
        .opinions
        .iter()
        .filter_map(|((h, a), values)| {
            let nonzero: Map<String, Value> = OpinionKey::ALL
                .iter()
                .zip(values)
                .filter(|(_, x)| x.to_bits() != 0)
                .map(|(k, &x)| (k.name().to_owned(), json!(x)))
                .collect();
            (!nonzero.is_empty()).then(|| json!({"holder": h.0, "about": a.0, "values": nonzero}))
        })
        .collect();
    json!({
        "relations": relations,
        "opinions": opinions,
        "deals": d.deals.iter().map(|x| rs_deal(n.r, x)).collect::<Vec<_>>(),
        "negotiations": d.negotiations.iter().map(|x| rs_negotiation(n.r, x)).collect::<Vec<_>>(),
    })
}

fn rs_result(n: &Names<'_>, x: &UnResult) -> Value {
    let tally: Vec<Value> = x.tally.iter().map(|&(p, v)| json!([n.player_name(p), v])).collect();
    json!({"turn": x.turn, "tally": tally, "votes_needed": x.votes_needed,
           "winner": x.winner.map(|p| p.0)})
}

fn rs_world(n: &Names<'_>) -> Value {
    let w = n.st.world();
    let religions: Vec<Value> = (0..w.religions.len())
        .filter_map(|i| {
            let id = ReligionId(u8::try_from(i).ok()?);
            let x = w.religion(id)?;
            Some(json!({"name": n.religion(id), "display": &*x.display, "founder": x.founder.0,
                        "founder_beliefs": n.set(&x.founder_beliefs),
                        "follower_beliefs": n.set(&x.follower_beliefs),
                        "blocked_holy": x.blocked_holy}))
        })
        .collect();
    let wonders: Map<String, Value> = w
        .wonders_built
        .iter()
        .map(|(&b, c)| (n.of(b).as_str().unwrap_or_default().to_owned(), json!(c.get())))
        .collect();
    let votes: Map<String, Value> =
        w.un.votes.iter().map(|(p, c)| (p.0.to_string(), json!(c.map(|c| c.0)))).collect();
    let camps: Vec<Value> = w
        .camps
        .iter()
        .map(|(id, c)| {
            json!({"id": id.get(), "idx": c.tile.0, "countdown": c.countdown, "spawned": c.spawned,
                   "destroyed": c.destroyed})
        })
        .collect();
    json!({
        "religions": religions,
        "wonders_built": wonders,
        "un": {"next_vote": w.un.next_vote, "votes": votes,
               "results": w.un.results.as_ref().map(|x| rs_result(n, x)),
               "won": w.un.won.iter().map(|p| p.0).collect::<Vec<_>>(),
               "processed_turn": w.un.processed_turn},
        "camps": camps,
    })
}

/// An event's name references in code points, as Python kept them.
fn code_point_refs(e: &Event) -> Value {
    let text = &*e.text;
    let cp = |byte: u32| text.get(..byte as usize).map_or(0, |s| s.chars().count());
    Value::Array(
        e.refs
            .iter()
            .map(|r| {
                let code = match r.kind {
                    RefKind::Civ => "c",
                    RefKind::Leader => "l",
                    RefKind::City => "t",
                };
                json!([cp(r.start), cp(r.end), r.player.0, code])
            })
            .collect(),
    )
}

fn rs_event_data(n: &Names<'_>, d: Option<&EventData>) -> Value {
    let mut m = Map::new();
    let Some(d) = d else { return Value::Object(m) };
    for (k, p) in d.players() {
        m.insert(k.to_owned(), json!(p.0));
    }
    let mut put = |k: &str, v: Value| {
        if !v.is_null() {
            m.insert(k.to_owned(), v);
        }
    };
    put("unit", json!(d.unit.map(|x| x.get())));
    put("city", json!(d.city.map(|x| x.get())));
    put("deal", json!(d.deal.map(|x| x.get())));
    put("negotiation", json!(d.negotiation.map(|x| x.get())));
    put("message", json!(d.message.map(|x| x.get())));
    put("item", opt(d.item, |x| n.item(x)));
    put("unit_type", opt(d.unit_type, |x| n.of(x)));
    put("building", opt(d.building, |x| n.of(x)));
    put("improvement", opt(d.improvement, |x| n.of(x)));
    put("tech", opt(d.tech, |x| n.of(x)));
    put("policy", opt(d.policy, |x| n.of(x)));
    put("belief", opt(d.belief, |x| n.of(x)));
    put("era", json!(d.era.map(|x| x.0)));
    put("religion", opt(d.religion, |x| n.religion(x)));
    put("reward", opt(d.reward, |x| n.ruin(x)));
    put("victory", opt(d.victory, |x| n.of(x)));
    put("status", json!(d.status.map(|x| x.name())));
    put("gold", json!(d.gold));
    put("citizen_killed", json!(d.citizen_killed));
    put("results", d.results.as_deref().map_or(Value::Null, |x| rs_result(n, x)));
    Value::Object(m)
}

fn rs_stats(row: &StatsRow) -> Value {
    let players: Map<String, Value> = row
        .civs
        .iter()
        .map(|c| {
            let v = if c.alive {
                json!({"alive": true, "score": c.score, "cities": c.cities,
                       "population": c.population, "land": c.land, "techs": c.techs,
                       "policies": c.policies, "military": c.military, "gold": c.gold,
                       "gold_per_turn": c.gold_per_turn, "science": c.science,
                       "culture": c.culture, "faith": c.faith, "production": c.production,
                       "happiness": c.happiness, "era": c.era.0, "units": c.units,
                       "golden_age": c.golden_age})
            } else {
                json!({"alive": false, "score": c.score})
            };
            (c.player.0.to_string(), v)
        })
        .collect();
    json!({"turn": row.turn, "players": players})
}

fn rs_history(n: &Names<'_>, chron: &citar_engine::state::chronicle::Chronicle) -> Value {
    let events: Vec<Value> = chron
        .events()
        .iter()
        .map(|e| {
            let ty = match &e.kind {
                EventType::Engine(x) => x.name(),
                EventType::Host(x) => x,
            };
            json!({
                "id": e.id.get(), "turn": e.turn, "type": ty, "text": &*e.text,
                "players": e.audience.map(|a| a.iter().map(|p| p.0).collect::<Vec<_>>()),
                "idx": e.tile.map(|t: TileIdx| t.0), "refs": code_point_refs(e),
                "data": rs_event_data(n, e.data.as_deref()),
            })
        })
        .collect();
    let messages: Vec<Value> = chron
        .messages()
        .iter()
        .map(|m| {
            json!({"id": m.id.get(), "turn": m.turn, "from": m.from.0,
                   "to": m.to.iter().map(|p| p.0).collect::<Vec<_>>(), "text": &*m.text})
        })
        .collect();
    let thoughts: Vec<Value> = chron
        .thoughts()
        .iter()
        .map(|t| json!({"turn": t.turn, "player": t.player.0, "text": &*t.text, "kind": t.kind.as_deref()}))
        .collect();
    json!({
        "events": events,
        "messages": messages,
        "thoughts": thoughts,
        "stats": chron.stats().iter().map(rs_stats).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::spec::CompareSpec;
    use crate::compare::{Diff, Options, compare};

    fn fixture_state(name: &str) -> Value {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let path = root.join("refcheck/fixtures-mini").join(format!("{name}.json.gz"));
        let bytes = crate::fixture::read_gz(&path).expect("the fixture reads");
        let mut doc: Value = serde_json::from_slice(&bytes).expect("the fixture is JSON");
        doc["state"].take()
    }

    /// The differences between the projection of `original` and the loaded game of `changed`.
    fn echo(original: &Value, changed: &Value) -> Vec<Diff> {
        let r = Ruleset::shared();
        let (game, _) =
            Game::from_python(r, changed.to_string().as_bytes()).expect("the state loads");
        let spec = CompareSpec::for_group(Group::StateEcho);
        let opts = Options { with_bot: false, grid: None };
        compare(&spec, &python(original), &rust(r, &game), &opts)
    }

    fn paths(diffs: &[Diff]) -> Vec<String> {
        diffs.iter().map(|d| d.path.to_string()).collect()
    }

    #[test]
    fn a_fixture_echoes_itself() {
        let s = fixture_state("duel-continents-normal/t20");
        assert_eq!(paths(&echo(&s, &s)), Vec::<String>::new());
        let p = python(&s);
        assert_eq!(p["tiles"].as_array().map(Vec::len), Some(44 * 28));
        assert!(p["history"]["events"].as_array().is_some_and(|e| e.len() > 100));
        assert!(p["units"].as_array().is_some_and(|u| !u.is_empty()));
    }

    #[test]
    fn a_misread_field_shows_at_its_place() {
        let s = fixture_state("duel-continents-normal/t20");
        let mut c = s.clone();
        c["units"]["2"]["hp"] = json!(99);
        c["players"][0]["techs"] = json!(["Agriculture", "Pottery"]);
        c["events"][0]["refs"] = json!([[18, 23, 0, "c"]]);
        c["tiles"][5][3] = json!(1);
        c["cities"]["16"]["pop"] = json!(9);
        c["players"][0]["flags"]["start"] = json!(3);
        // A remembered tile's owner, one more tile explored, a tile's continent, the seat's
        // explicit handicap, the map's edges and a host key.
        let remembered = s["players"][0]["memory"]
            .as_object()
            .and_then(|m| m.keys().next().cloned())
            .expect("player 0 remembers a tile");
        c["players"][0]["memory"][&remembered]["o"] = json!(1);
        let mut explored =
            b64_decode(s["players"][0]["explored"].as_str().unwrap_or_default()).expect("base64");
        let unexplored = explored.iter().position(|&b| b == 0).expect("a tile not explored");
        explored[unexplored] = 1;
        c["players"][0]["explored"] = json!(citar_engine::base::codec::b64_encode(&explored));
        c["continents"][0] = json!(7);
        c["players"][0]["overrides"] = json!({"handicap": "human"});
        c["config"]["map_edges"] = json!("boxed");
        c["config"]["on_disconnect"] = json!("skip");
        let got = paths(&echo(&s, &c));
        let memory = format!("players[id=0].memory.{remembered}.o");
        for want in [
            "units[id=2].hp",
            "players[id=0].techs",
            "history.events[id=1].refs[0][1]",
            "tiles[i=5].river",
            "cities[id=16].pop",
            "players[id=0].start",
            &memory,
            "players[id=0].explored",
            "map.continents[0]",
            "players[id=0].overrides.handicap",
            "settings.map_edges",
            "settings.host.on_disconnect",
        ] {
            assert!(got.iter().any(|p| p.starts_with(want)), "{want} in {got:?}");
        }
    }

    #[test]
    fn a_reordered_set_is_no_difference() {
        let s = fixture_state("duel-continents-normal/t20");
        let mut c = s.clone();
        c["cities"]["16"]["worked"] = json!([804, 894, 935]);
        let techs: Vec<Value> = s["players"][0]["techs"].as_array().cloned().unwrap_or_default();
        c["players"][0]["techs"] = Value::Array(techs.into_iter().rev().collect());
        assert_eq!(paths(&echo(&s, &c)), Vec::<String>::new());
    }
}
