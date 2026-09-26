//! Tool calls as models make them, right and wrong: for every tool of the registry, argument
//! sets drawn from what the game holds (the caller's units, cities, tiles, spies and chats, the
//! other players, the ruleset's names) mixed with what it does not (ids no game hands out, tiles
//! off the map, names nobody knows, numbers sent as text and text sent as numbers, arguments
//! missing).
//!
//! [`battery`] is deterministic: the same game gives the same calls. The refusal tests run it
//! on every committed fixture and check each refusal's text against the rules a model reads
//! (property P5) and that it changed nothing (property P2); the chaos driver of package 1e-01
//! can draw from it too. No value it sends contains what the text rules take for Rust debug
//! output (`None`, `::`), so a refusal that quotes it back is still checked fairly.

use citar_engine::api::tools::registry::{TOOLS, ToolKind, ToolSpec};
use citar_engine::base::ids::PlayerId;
use citar_engine::game::Game;
use citar_engine::game::espionage;
use citar_engine::state::diplo::NegStatus;
use serde_json::{Value, json};

/// How many argument sets each tool is called with.
pub const VARIANTS: usize = 10;

/// A name far too long for any table, which a refusal may quote only in part.
fn long() -> Value {
    Value::from("Zanzibar ".repeat(80))
}

/// What the game offers a caller to name, and names it does not know.
struct Pools {
    units: Vec<Value>,
    cities: Vec<Value>,
    tiles: Vec<(i64, i64)>,
    players: Vec<Value>,
    spies: Vec<Value>,
    chats: Vec<Value>,
    techs: Vec<Value>,
    items: Vec<Value>,
    policies: Vec<Value>,
    beliefs: Vec<Value>,
    promotions: Vec<Value>,
    improvements: Vec<Value>,
    great_people: Vec<Value>,
}

/// Up to `n` entries of `v`, spread over it.
fn spread<T: Clone>(v: &[T], n: usize) -> Vec<T> {
    if v.len() <= n {
        return v.to_vec();
    }
    (0..n).map(|i| v[i * v.len() / n].clone()).collect()
}

/// Names of a table, spread over it.
fn names<'a>(all: impl Iterator<Item = &'a str>, n: usize) -> Vec<Value> {
    let v: Vec<&str> = all.collect();
    spread(&v, n).into_iter().map(Value::from).collect()
}

impl Pools {
    fn of(g: &Game, pid: PlayerId) -> Self {
        let r = g.rules();
        let own_units: Vec<i64> = g.player_units(pid).map(|u| i64::from(u.id().get())).collect();
        let foreign_unit =
            g.state().units().iter().find(|u| u.owner() != pid).map(|u| i64::from(u.id().get()));
        let mut units: Vec<Value> = spread(&own_units, 4).into_iter().map(Value::from).collect();
        units.extend(foreign_unit.map(Value::from));
        units.extend([json!(999_999), json!(-7), json!("abc"), json!(" 12 ")]);
        let own_cities: Vec<i64> = g.player_cities(pid).map(|c| i64::from(c.id().get())).collect();
        let foreign_city =
            g.state().cities().iter().find(|c| c.owner() != pid).map(|c| i64::from(c.id().get()));
        let mut cities: Vec<Value> = spread(&own_cities, 3).into_iter().map(Value::from).collect();
        cities.extend(foreign_city.map(Value::from));
        cities.extend([json!(999_999), json!("hideout"), json!("moon")]);
        let mut tiles: Vec<(i64, i64)> = Vec::new();
        for u in g.player_units(pid).take(4) {
            let (x, y) = g.xy(u.tile());
            let (x, y) = (i64::from(x), i64::from(y));
            tiles.extend([(x, y), (x + 1, y), (x, y + 1), (x - 2, y)]);
        }
        for c in g.player_cities(pid).take(2) {
            let (x, y) = g.xy(c.tile());
            tiles.extend([(i64::from(x) + 1, i64::from(y) + 1)]);
        }
        tiles.extend([(0, 0), (-3, 99_999)]);
        let mut players: Vec<Value> = g.state().players().ids().map(|p| Value::from(p.0)).collect();
        players.extend([json!(999), json!("all"), json!("abstain"), json!(-1)]);
        let mut spies: Vec<Value> =
            espionage::spies(g, pid).iter().map(|s| Value::from(&*s.name)).collect();
        spies.extend([json!("Agent Nobody"), long()]);
        let mut chats: Vec<Value> = g
            .negotiations()
            .iter()
            .filter(|n| n.status == NegStatus::Open)
            .map(|n| Value::from(n.id.get()))
            .collect();
        chats.extend([json!(99_999), json!("first")]);
        let mut techs = names(r.techs().iter().map(|(_, t)| &*t.name), 6);
        techs.extend([json!("Warp Drive"), json!(""), long()]);
        let mut items = names(r.buildings().iter().map(|(_, b)| &*b.name), 6);
        items.extend(names(r.base_units().iter().map(|(_, u)| &*u.name), 6));
        items.extend([json!("Death Star"), json!("Gold"), json!("Science"), long()]);
        let mut policies = names(r.policies().iter().map(|(_, p)| &*p.name), 6);
        policies.extend([json!("Tyranny"), long()]);
        let mut beliefs = names(r.beliefs().iter().map(|(_, b)| &*b.name), 6);
        beliefs.extend([json!("Astrology"), long()]);
        let mut promotions = names(r.promotions().iter().map(|(_, p)| &*p.name), 6);
        promotions.extend([json!("Wings of Icarus"), long()]);
        let mut improvements = names(r.improvements().iter().map(|(_, i)| &*i.name), 8);
        improvements.extend([json!("Castle in the Sky"), long()]);
        let mut great_people =
            names(r.base_units().iter().filter(|(_, u)| u.great_person).map(|(_, u)| &*u.name), 4);
        great_people.extend([json!("Great Nobody"), long()]);
        Self {
            units,
            cities,
            tiles,
            players,
            spies,
            chats,
            techs,
            items,
            policies,
            beliefs,
            promotions,
            improvements,
            great_people,
        }
    }

    /// What a parameter of `tool` called `param` may be sent.
    fn values(&self, tool: &str, param: &str) -> Vec<Value> {
        let words = |w: &[&str]| {
            let mut v: Vec<Value> = w.iter().map(|&s| Value::from(s)).collect();
            v.push(long());
            v
        };
        match (tool, param) {
            (_, "unit_id") => self.units.clone(),
            (_, "city_id") => self.cities.clone(),
            (_, "player_id" | "to" | "candidate") => self.players.clone(),
            (_, "negotiation_id") => self.chats.clone(),
            (_, "spy") => self.spies.clone(),
            (_, "tech") => self.techs.clone(),
            (_, "item") => self.items.clone(),
            (_, "policy") => self.policies.clone(),
            (_, "belief") => self.beliefs.clone(),
            (_, "beliefs") => {
                let mut v: Vec<Value> = self.beliefs.iter().map(|b| json!([b])).collect();
                v.push(json!("Astrology, Tithe"));
                v
            }
            (_, "promotion") => self.promotions.clone(),
            (_, "improvement") => self.improvements.clone(),
            (_, "great_person") => self.great_people.clone(),
            (_, "order") => words(&[
                "fortify", "sleep", "wake", "skip", "heal", "explore", "automate", "pillage",
                "setup", "disband", "cancel", "dance",
            ]),
            ("unit_action", "action") => words(&[
                "found_city",
                "found_religion",
                "enhance_religion",
                "spread_religion",
                "remove_heresy",
                "hurry_research",
                "hurry_construction",
                "trade_mission",
                "create:Academy",
                "add_to_spaceship",
                "paradrop",
                "trigger:0",
                "dance",
            ]),
            ("change_queue", "action") => {
                words(&["up", "down", "first", "last", "remove", "clear", "sideways"])
            }
            ("respond_negotiation", "action") => {
                words(&["accept", "reject", "counter", "reply", "shrug"])
            }
            ("city_state_action", "action") => words(&[
                "gift_gold",
                "gift_unit",
                "pledge",
                "withdraw",
                "tribute_gold",
                "tribute_worker",
                "make_peace",
                "marry",
                "hug",
            ]),
            (_, "status") => words(&["annex", "puppet", "raze", "stop_razing", "liberate", "burn"]),
            (_, "focus") => words(&["food", "production", "gold", "manual", "nonsense"]),
            (_, "currency") => words(&["Gold", "Faith", "faith", "Bitcoin"]),
            (_, "mode") => words(&["replace", "append", "sideways"]),
            (_, "message" | "text") => words(&["Hello there.", "", "   ", "Let us trade."]),
            ("get_rules", "name") => words(&["Warrior", "Nope"]),
            (_, "name" | "leader") => words(&["New Harbour", "", "Rome"]),
            (_, "give" | "receive") => vec![
                json!([{"type": "gold", "amount": 50}]),
                json!([{"type": "embassy"}]),
                json!([{"type": "tech"}]),
                json!([{"type": "city", "city_id": 999}]),
                json!([{"type": "flying_carpet"}]),
                json!("gold"),
                json!([{"type": "resource", "resource": "Iron"}]),
                json!([{"type": "declare_war", "target": 999}]),
                json!([{"type": "gold_per_turn", "amount": 5, "turns": 30}]),
            ],
            (_, "specialists") => vec![
                json!({"Scientist": 1}),
                json!({"Nothing": 2}),
                json!({"Scientist": 99}),
                json!("Scientist"),
                json!({}),
            ],
            (_, "amount") => vec![json!(50), json!(-5), json!(1_000_000), json!("lots")],
            (_, "index") => vec![json!(0), json!(3), json!(-1)],
            (_, "since_id") => vec![json!(0), json!(10), json!(-5)],
            (_, "message_limit") => vec![json!(5), json!(-1)],
            (_, "radius") => vec![json!(3), json!(50), json!(-2)],
            (_, "filter") => words(&["available", "all", "locked", "nonsense"]),
            (_, "topic") => words(&["units", "overview", "astrology", " Techs "]),
            (_, "append" | "enabled" | "locked" | "keep" | "avoid_growth" | "legend") => {
                vec![json!(true), json!(false), json!("yes"), json!(5)]
            }
            (_, "x" | "y") => vec![json!(0)],
            _ => vec![json!("?")],
        }
    }
}

/// Every tool called with [`VARIANTS`] argument sets for `pid`, in the registry's order: the
/// tool's name and the arguments.
#[must_use]
pub fn battery(g: &Game, pid: PlayerId) -> Vec<(&'static str, Value)> {
    let pools = Pools::of(g, pid);
    let mut out = Vec::with_capacity(TOOLS.len() * VARIANTS);
    for spec in &TOOLS {
        for k in 0..VARIANTS {
            out.push((spec.name(), args(&pools, spec, k)));
        }
    }
    out
}

/// The `k`th argument set of a tool: each parameter a value of its pool, some optional ones
/// left out, a tile named by both coordinates together, and in the last set the first required
/// parameter missing.
fn args(pools: &Pools, spec: &ToolSpec, k: usize) -> Value {
    let mut m = serde_json::Map::new();
    for (i, p) in spec.args.params.iter().enumerate() {
        let required = spec.args.required.contains(&p.name);
        if !required && (k + i) % 3 == 2 {
            continue;
        }
        let v = match p.name {
            "x" | "y" => {
                let (x, y) = pools.tiles[k % pools.tiles.len()];
                // Now and then a coordinate sent as text, or one that is no number.
                let n = if p.name == "x" { x } else { y };
                match k % 7 {
                    5 => json!(n.to_string()),
                    6 if p.name == "y" => json!("north"),
                    _ => json!(n),
                }
            }
            name => {
                let pool = pools.values(spec.name(), name);
                pool[(k + i * 2) % pool.len()].clone()
            }
        };
        m.insert(p.name.to_owned(), v);
    }
    if k == VARIANTS - 1
        && let Some(first) = spec.args.required.first()
    {
        m.shift_remove(*first);
    }
    // A query is asked as often as an action; an action's set with nothing at all is its own
    // variant too, which the tools that take nothing always are.
    if spec.kind() == ToolKind::Action && k == VARIANTS - 2 {
        m.clear();
    }
    Value::Object(m)
}
