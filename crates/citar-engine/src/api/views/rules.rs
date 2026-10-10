//! The rules as a model looks them up (`views.rules_lookup`, `views.py:628-675`): a whole topic
//! or one entry by name, the deal items, and the rules and combat in brief.
//!
//! The tables are the client JSON's (`Ruleset::client_value`, Python's `to_client`), which are
//! the files' objects without their private fields and with the defaults Python set at load, so
//! an entry reads as Python's `_public(table[key])` did. A name is looked up loosely, as Python's
//! `Rules.resolve` did ("bronze working" is Bronze Working).

use serde_json::{Map, Value, json};

use crate::api::text::{RULES_COMBAT, RULES_OVERVIEW};
use crate::base::{py, text};
use crate::game::Game;
use crate::game::error::{ActionError, ErrCode};
use crate::rules::names::NameKind;
use crate::state::diplo::DealItemKind;

/// The topics `get_rules` answers, in the order its refusal lists them (`views.py:633-648`).
pub const TOPICS: [&str; 19] = [
    "units",
    "buildings",
    "techs",
    "improvements",
    "resources",
    "promotions",
    "terrains",
    "terrain",
    "policies",
    "beliefs",
    "specialists",
    "eras",
    "nations",
    "city_state_types",
    "speeds",
    "difficulties",
    "deal_items",
    "combat",
    "overview",
];

/// How to write each kind of deal item, for a model (`diplomacy.ITEM_TYPES`,
/// `diplomacy.py:17-31`).
const fn item_help(k: DealItemKind) -> &'static str {
    match k {
        DealItemKind::Gold => r#"Lump sum of gold: {"type": "gold", "amount": 100}"#,
        DealItemKind::GoldPerTurn => {
            r#"Gold every turn: {"type": "gold_per_turn", "amount": 5, "turns": 30}"#
        }
        DealItemKind::Resource => {
            r#"Strategic or luxury resource per turn: {"type": "resource", "resource": "Iron", "amount": 1, "turns": 30}"#
        }
        DealItemKind::OpenBorders => {
            r#"Let the other side's units enter your territory: {"type": "open_borders", "turns": 30}"#
        }
        DealItemKind::Embassy => {
            r#"Let the other side establish an embassy in your capital: {"type": "embassy"}"#
        }
        DealItemKind::PeaceTreaty => r#"End the war between you: {"type": "peace_treaty"}"#,
        DealItemKind::DeclarationOfFriendship => {
            r#"Declare friendship (both sides; 30 turns): {"type": "declaration_of_friendship"}"#
        }
        DealItemKind::ResearchAgreement => {
            r#"Research agreement (both sides pay; needs friendship): {"type": "research_agreement"}"#
        }
        DealItemKind::DefensivePact => {
            r#"Defensive pact (both sides; needs friendship): {"type": "defensive_pact"}"#
        }
        DealItemKind::DeclareWar => {
            r#"The giver declares war on a third civ: {"type": "declare_war", "target": 2}"#
        }
        DealItemKind::City => {
            r#"Give one of your cities (not the capital): {"type": "city", "city_id": 12}"#
        }
        DealItemKind::ShareMap => r#"Give your explored map: {"type": "share_map"}"#,
        DealItemKind::Tech => {
            r#"Give a technology (if tech trading is on): {"type": "tech", "tech": "Bronze Working"}"#
        }
    }
}

/// Where a table topic's rows are in the client JSON, and the kind its names resolve as
/// (`views.py:634-641`); `None` for the city-state types, looked up by their exact name.
fn table_of(topic: &str) -> Option<(&'static [&'static str], Option<NameKind>)> {
    Some(match topic {
        "units" => (&["units"], Some(NameKind::Unit)),
        "buildings" => (&["buildings"], Some(NameKind::Building)),
        "techs" => (&["techs"], Some(NameKind::Tech)),
        "improvements" => (&["improvements"], Some(NameKind::Improvement)),
        "resources" => (&["resources"], Some(NameKind::Resource)),
        "promotions" => (&["promotions"], Some(NameKind::Promotion)),
        "terrains" | "terrain" => (&["terrains"], Some(NameKind::Terrain)),
        "policies" => (&["policy_branches", "policies"], Some(NameKind::Policy)),
        "beliefs" => (&["beliefs"], Some(NameKind::Belief)),
        "specialists" => (&["specialists"], Some(NameKind::Specialist)),
        "eras" => (&["eras"], Some(NameKind::Era)),
        "nations" => (&["nations"], Some(NameKind::Nation)),
        "city_state_types" => (&["city_state_types"], None),
        "speeds" => (&["speeds"], Some(NameKind::Speed)),
        "difficulties" => (&["difficulties"], Some(NameKind::Difficulty)),
        _ => return None,
    })
}

/// The rows of a table topic, as one object (the policies are the branches, then the policies).
fn rows(g: &Game, parts: &[&str]) -> Map<String, Value> {
    let client = g.rules().client_value();
    let mut out = Map::new();
    for part in parts {
        if let Some(Value::Object(t)) = client.get(*part) {
            for (k, v) in t {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    out
}

/// A field of a row, or `default` where it has none (Python's `v.get(k, default)`).
fn field(v: &Value, k: &str, default: Value) -> Value {
    v.get(k).cloned().unwrap_or(default)
}

/// Every object of the topic, or one of them by name (`views.rules_lookup`). The topic is
/// already lower-cased and checked (`query_tools::rules_topic`).
///
/// # Errors
/// [`ErrCode::BadParam`] for a topic that is none, or a name the topic has no entry for.
pub fn rules_lookup(g: &Game, topic: &str, name: Option<&Value>) -> Result<Value, ActionError> {
    match topic {
        "deal_items" => {
            let items: Map<String, Value> = DealItemKind::ALL
                .iter()
                .map(|&k| (k.name().to_owned(), json!(item_help(k))))
                .collect();
            return Ok(Value::Object(items));
        }
        "combat" => return Ok(json!(RULES_COMBAT)),
        "overview" => return Ok(json!(RULES_OVERVIEW)),
        _ => {}
    }
    let Some((parts, kind)) = table_of(topic) else {
        return Err(ActionError::new(
            ErrCode::BadParam,
            format!("Unknown topic '{}'. Topics: {}.", text::echo(topic), TOPICS.join(", ")),
        ));
    };
    let table = rows(g, parts);
    let r = g.rules();
    // A name that is not text is read as Python's `str()` of it, as `Rules.resolve` read it.
    let name = name.filter(|v| py::truthy(v));
    if let Some(v) = name {
        let text = match v {
            Value::String(s) => s.clone(),
            other => py::str_of(other),
        };
        let key = match kind {
            Some(k) => r.resolve_name(k, &text).map(str::to_owned),
            None => table.contains_key(&text).then_some(text.clone()),
        };
        let Some((key, row)) = key.and_then(|k| table.get(&k).map(|row| (k, row))) else {
            // refcheck: refusals-quote-at-most-60-characters
            let quoted = match v {
                Value::String(s) => text::echo(s).into_owned(),
                other => py::repr_echo(other),
            };
            return Err(ActionError::new(
                ErrCode::BadParam,
                format!("No {topic} entry '{quoted}'."),
            ));
        };
        return Ok(entry(g, topic, &key, row));
    }
    let summary = |f: &dyn Fn(&Value) -> Value| -> Value {
        Value::Object(table.iter().map(|(k, v)| (k.clone(), f(v))).collect())
    };
    Ok(match topic {
        "units" => summary(&|v| {
            json!({
                "cost": field(v, "cost", Value::Null),
                "strength": field(v, "strength", json!(0)),
                "ranged": field(v, "rangedStrength", json!(0)),
                "range": field(v, "range", json!(0)),
                "movement": field(v, "movement", Value::Null),
                "type": field(v, "unitType", Value::Null),
                "tech": field(v, "requiredTech", Value::Null),
                "resource": field(v, "requiredResource", Value::Null),
                "unique_to": field(v, "uniqueTo", Value::Null),
                "upgrades_to": field(v, "upgradesTo", Value::Null),
            })
        }),
        "buildings" => summary(&|v| {
            json!({
                "cost": field(v, "cost", Value::Null),
                "tech": field(v, "requiredTech", Value::Null),
                "maintenance": field(v, "maintenance", json!(0)),
                "wonder": py::truthy(&field(v, "isWonder", Value::Null)),
                "national_wonder": py::truthy(&field(v, "isNationalWonder", Value::Null)),
                "unique_to": field(v, "uniqueTo", Value::Null),
            })
        }),
        "techs" => summary(&|v| {
            json!({
                "era": field(v, "era", Value::Null),
                "cost": field(v, "cost", Value::Null),
                "prerequisites": field(v, "prerequisites", Value::Null),
            })
        }),
        "nations" => summary(&|v| {
            json!({
                "kind": field(v, "kind", Value::Null),
                "leader": field(v, "leaderName", Value::Null),
                "start_bias": field(v, "startBias", json!([])),
                "uniques": field(v, "uniques", json!([])),
            })
        }),
        "city_state_types" => Value::Array(table.keys().map(|k| json!(k)).collect()),
        _ => Value::Object(table),
    })
}

/// One entry of a topic, as `get_rules` shows it with a name (`views.py:650-660`): the object,
/// a tech with what it unlocks, a nation with its city names, a city-state type as the bonuses
/// it gives its friends and its ally.
fn entry(g: &Game, topic: &str, key: &str, row: &Value) -> Value {
    let r = g.rules();
    match topic {
        "techs" => {
            let mut out = row.clone();
            if let Some(m) = out.as_object_mut() {
                let unlocks = r.client_value().get("unlocks").and_then(|u| u.get(key));
                let kept: Map<String, Value> = unlocks
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                    .filter(|&(_, v)| py::truthy(v))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                m.insert("unlocks".into(), Value::Object(kept));
            }
            out
        }
        "nations" => match r.nation_source(key) {
            // The file's row, private fields aside, where the client JSON leaves out the cities.
            Some(Value::Object(src)) => Value::Object(
                src.iter()
                    .filter(|(k, _)| !k.starts_with('_'))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
            _ => row.clone(),
        },
        "city_state_types" => {
            let def = r.city_state_types().iter().find(|(_, d)| &*d.name == key).map(|(_, d)| d);
            let texts = |u: &crate::unique::table::SourceUniques| -> Vec<String> {
                u.ids().map(|x| r.uniques().text_of(x).to_owned()).collect()
            };
            json!({
                "friend_bonuses": def.map(|d| texts(&d.friend)).unwrap_or_default(),
                "ally_bonuses": def.map(|d| texts(&d.ally)).unwrap_or_default(),
            })
        }
        _ => row.clone(),
    }
}
