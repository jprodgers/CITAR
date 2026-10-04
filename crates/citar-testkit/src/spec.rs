//! Tool calls described by a few numbers and bound to a game only when they are made (DESIGN.md
//! 9.5): what the properties generate, the chaos driver mixes into its games, and the fuzz
//! target reads from its bytes.
//!
//! An [`ActionSpec`] names no unit, city or player of any game. Its numbers are indices taken
//! modulo whatever the game offers when the call is made ([`ActionSpec::bind`]): the tool among
//! the registry's 61, the caller among the players (most often the one whose turn it is), each
//! argument among the candidates the game has for it (the caller's units and cities, the techs
//! it could research, the items the named city could build, the tiles around the named unit).
//! So a sequence of specs means something in any game, and a shrunk one still does. Its
//! [`Shape`] says how the arguments are made:
//! - [`Shape::Valid`] (70%): every parameter a value of its type drawn from what the game holds
//!   for the caller, the optional ones now and then left out;
//! - [`Shape::Confused`] (20%): the valid arguments with some sent as the wrong type (a number
//!   as text, text as a number, a flag as a word, a list as one value), or a required one left
//!   out;
//! - [`Shape::Random`] (10%): JSON drawn at random, keyed by the spec's seed, often not an
//!   object at all, with nulls at any depth.
//!
//! No text this sends contains what the text rules take for Rust debug output (`None`, `::`),
//! so a refusal that quotes it back is still checked fairly (property P5). A null inside a value
//! may come back as Python's `None`, which P5 reads as the caller's
//! (`stability::refusal_rule_broken`).

use citar_engine::api::tools::SchemaType;
use citar_engine::api::tools::registry::{TOOLS, ToolSpec};
use citar_engine::base::ids::{CityId, PlayerId, TileIdx, UnitId};
use citar_engine::base::rng::{Purpose, Rng};
use citar_engine::game::Game;
use citar_engine::game::actions::unit_actions;
use citar_engine::game::cities::construction::buildable_items;
use citar_engine::game::cities::stats::max_specialists;
use citar_engine::game::espionage;
use citar_engine::game::great_people::great_people_types;
use citar_engine::game::policies::adoptable_policies;
use citar_engine::game::religion::beliefs_available;
use citar_engine::game::research::available_techs;
use citar_engine::game::units::promotions;
use citar_engine::game::workers::{self, Builder};
use citar_engine::rules::defs::{BeliefKind, BeliefType};
use citar_engine::state::cities::{CityFocus, Constructible};
use citar_engine::state::diplo::NegStatus;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// How an [`ActionSpec`]'s arguments are made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    /// Every parameter a value of its type, drawn from what the game holds.
    Valid,
    /// The valid arguments, with those this mask picks sent as the wrong type.
    Confused(u8),
    /// JSON drawn at random from this seed.
    Random(u32),
}

impl Shape {
    /// The shape a draw of `r` (0 to 99) and `arg` give, in the proportions of DESIGN.md 9.5:
    /// 70 valid, 20 confused, 10 random.
    #[must_use]
    pub const fn weighted(r: u8, arg: u32) -> Self {
        match r % 100 {
            0..70 => Self::Valid,
            // The mask's low byte: which parameters go wrong.
            70..90 => Self::Confused(arg.to_le_bytes()[0]),
            _ => Self::Random(arg),
        }
    }
}

/// A tool call described by indices, bound to a game when it is made (DESIGN.md 9.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActionSpec {
    /// The tool, modulo the registry's.
    pub tool: u8,
    /// Who calls: five times in eight the player whose turn it is, then another living major
    /// civilization, any player of the game, or one it does not have.
    pub actor: u8,
    /// Which of the candidates each argument takes, modulo their number.
    pub target: u16,
    /// Where a tile argument lands: an offset from the named unit or city, or from the tile the
    /// target names; one `x` in sixteen puts it off the map instead ([`off_map`]).
    pub coords: (i16, i16),
    /// How the arguments are made.
    pub shape: Shape,
}

/// A call as it is made: who, which tool, and the arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub pid: PlayerId,
    pub tool: &'static str,
    pub args: Value,
}

impl ActionSpec {
    /// A spec drawn from `rng` with the shapes in DESIGN.md 9.5's proportions.
    #[must_use]
    pub fn draw(rng: &mut Rng) -> Self {
        let word = rng.next_u64().to_le_bytes();
        let coords =
            (i16::from_le_bytes([word[4], word[5]]), i16::from_le_bytes([word[6], word[7]]));
        let shape = Shape::weighted(
            u8::try_from(rng.below(100)).unwrap_or(0),
            u32::try_from(rng.next_u64() >> 32).unwrap_or(0),
        );
        Self {
            tool: word[0],
            actor: word[1],
            target: u16::from_le_bytes([word[2], word[3]]),
            coords,
            shape,
        }
    }

    /// The tool this spec calls.
    #[must_use]
    pub fn tool_spec(&self) -> &'static ToolSpec {
        &TOOLS[usize::from(self.tool) % TOOLS.len()]
    }

    /// The call this spec makes in `g` as it stands.
    #[must_use]
    pub fn bind(&self, g: &Game) -> Call {
        let spec = self.tool_spec();
        let pid = self.actor(g);
        let args = match self.shape {
            Shape::Valid => Value::Object(Binder::new(g, pid, self).valid(spec)),
            Shape::Confused(mask) => confuse(Binder::new(g, pid, self).valid(spec), spec, mask),
            Shape::Random(seed) => random_args(spec, seed),
        };
        Call { pid, tool: spec.name(), args }
    }

    /// Who calls.
    fn actor(&self, g: &Game) -> PlayerId {
        let n = g.state().players().len();
        let current = g.current();
        match self.actor % 8 {
            0..=4 => current,
            5 => {
                let others: Vec<PlayerId> =
                    g.majors(true).map(|p| p.id()).filter(|&p| p != current).collect();
                pick(&others, self.target).unwrap_or(current)
            }
            6 => PlayerId(u8::try_from(usize::from(self.target) % n.max(1)).unwrap_or(0)),
            // The first id past the game's players.
            _ => PlayerId(u8::try_from(n).unwrap_or(u8::MAX)),
        }
    }
}

/// Whether a spec's tile arguments land off the map: one `x` offset in sixteen, those that are
/// 15 modulo 16.
#[must_use]
pub const fn off_map(coords: (i16, i16)) -> bool {
    coords.0.rem_euclid(16) == 15
}

/// The `k`th of `v`, modulo its length.
fn pick<T: Copy>(v: &[T], k: u16) -> Option<T> {
    if v.is_empty() { None } else { Some(v[usize::from(k) % v.len()]) }
}

/// A different index for each parameter, so that two arguments of one call do not always take
/// the same place in their lists.
fn mix(k: u16, i: usize) -> u16 {
    let i = u16::try_from(i % 64).unwrap_or(0);
    k.wrapping_mul(31).wrapping_add(i.wrapping_mul(7919)) ^ (k >> 3)
}

/// The words the game's tools take as orders, actions, statuses and the like.
const ORDERS: [&str; 11] = [
    "fortify", "sleep", "heal", "skip", "wake", "explore", "automate", "pillage", "setup",
    "cancel", "disband",
];
const RESPONSES: [&str; 5] = ["accept", "reject", "counter", "reply", "withdraw"];
const CITY_STATE_ACTIONS: [&str; 8] = [
    "gift_gold",
    "gift_unit",
    "pledge",
    "withdraw",
    "tribute_gold",
    "tribute_worker",
    "make_peace",
    "marry",
];
const TOPICS: [&str; 8] =
    ["units", "buildings", "techs", "policies", "beliefs", "promotions", "combat", "overview"];

/// The deal items a proposal draws from, some of which no side can give.
fn deal_items(k: u16) -> Value {
    let pool = [
        json!([]),
        json!([{"type": "gold", "amount": 10 + k % 90}]),
        json!([{"type": "gold_per_turn", "amount": 1 + k % 4, "turns": 10}]),
        json!([{"type": "share_map"}]),
        json!([{"type": "embassy"}]),
        json!([{"type": "open_borders", "turns": 10 + k % 20}]),
        json!([{"type": "declaration_of_friendship"}]),
        json!([{"type": "peace_treaty"}]),
        json!([{"type": "research_agreement"}]),
        json!([{"type": "resource", "resource": "Iron", "amount": 1}]),
    ];
    pool[usize::from(k) % pool.len()].clone()
}

/// What a valid call's arguments are drawn from: the caller, and the unit, city and tile the
/// spec names in `g`.
struct Binder<'a> {
    g: &'a Game,
    pid: PlayerId,
    spec: &'a ActionSpec,
    unit: Option<UnitId>,
    city: Option<CityId>,
}

impl<'a> Binder<'a> {
    fn new(g: &'a Game, pid: PlayerId, spec: &'a ActionSpec) -> Self {
        let k = spec.target;
        let own_units: Vec<UnitId> = g.player_units(pid).map(|u| u.id()).collect();
        let any_units: Vec<UnitId> = g.state().units().iter().map(|u| u.id()).collect();
        let unit = pick(&own_units, k).or_else(|| pick(&any_units, k));
        let own_cities: Vec<CityId> = g.player_cities(pid).map(|c| c.id()).collect();
        let any_cities: Vec<CityId> = g.state().cities().iter().map(|c| c.id()).collect();
        let city = pick(&own_cities, k).or_else(|| pick(&any_cities, k));
        Self { g, pid, spec, unit, city }
    }

    /// Every parameter of `tool`: the required ones always, an optional one when the target's
    /// bit for it is set.
    fn valid(&self, tool: &ToolSpec) -> Map<String, Value> {
        let mut m = Map::new();
        for (i, p) in tool.args.params.iter().enumerate() {
            let required = tool.args.required.contains(&p.name);
            if !required && (self.spec.target >> (i % 16)) & 1 == 0 {
                continue;
            }
            m.insert(p.name.to_owned(), self.value(tool.name(), p.name, p.json, p.choices, i));
        }
        m
    }

    /// The tile a tile argument is taken around: the named unit's or city's, else the one the
    /// target names.
    fn base_tile(&self, tool: &ToolSpec) -> TileIdx {
        let named = |p: &str| tool.args.params.iter().any(|q| q.name == p);
        let g = self.g;
        let unit_tile =
            self.unit.and_then(|u| g.unit(u)).map(citar_engine::state::units::Unit::tile);
        let city_tile =
            self.city.and_then(|c| g.city(c)).map(citar_engine::state::cities::City::tile);
        let from_target = TileIdx(u32::from(self.spec.target) % g.grid().size().max(1));
        if named("unit_id") {
            unit_tile.unwrap_or(from_target)
        } else if named("city_id") {
            city_tile.unwrap_or(from_target)
        } else {
            from_target
        }
    }

    /// A coordinate: the base tile's, moved by the spec's offset (at most three either way) and
    /// kept on the map, wrapped across an edge the map wraps and held at one it does not; or,
    /// one spec in sixteen ([`off_map`]), off the map, past one of its four edges.
    fn coord(&self, tool: &ToolSpec, name: &str) -> Value {
        let g = self.g;
        let grid = g.grid();
        let (dx, dy) = self.spec.coords;
        let (bx, by) = g.xy(self.base_tile(tool));
        let (w, h) = (i32::from(grid.width()).max(1), i32::from(grid.height()).max(1));
        let (x, y) = if off_map(self.spec.coords) {
            // The tools wrap no coordinate: past any edge is off the map, wrapping or not.
            match dy.rem_euclid(4) {
                0 => (-1 - i32::from(dx.rem_euclid(3)), by),
                1 => (w + i32::from(dx.rem_euclid(3)), by),
                2 => (bx, -1),
                _ => (bx, h + 99_999),
            }
        } else {
            let (x, y) = (bx + i32::from(dx % 4), by + i32::from(dy % 4));
            let x = if grid.wrap_x() { x.rem_euclid(w) } else { x.clamp(0, w - 1) };
            let y = if grid.wrap_y() { y.rem_euclid(h) } else { y.clamp(0, h - 1) };
            (x, y)
        };
        json!(if name == "x" { x } else { y })
    }

    #[expect(clippy::too_many_lines, reason = "one arm per parameter a tool takes")]
    fn value(&self, tool: &str, name: &str, ty: SchemaType, choices: &[&str], i: usize) -> Value {
        let g = self.g;
        let r = g.rules();
        let k = mix(self.spec.target, i);
        let names = |v: Vec<String>| {
            pick(&v.iter().map(String::as_str).collect::<Vec<_>>(), k).map(Value::from)
        };
        let others: Vec<PlayerId> = g.state().players().ids().filter(|&p| p != self.pid).collect();
        let majors: Vec<PlayerId> =
            g.majors(true).map(|p| p.id()).filter(|&p| p != self.pid).collect();
        let tool_spec = citar_engine::api::tools::registry::tool(tool);
        if !choices.is_empty() {
            return json!(choices[usize::from(k) % choices.len()]);
        }
        let v = match name {
            "unit_id" => self.unit.map(|u| json!(u.get())),
            "city_id" if tool == "move_spy" => {
                if k.is_multiple_of(4) {
                    Some(json!("hideout"))
                } else {
                    let all: Vec<u32> = g.state().cities().iter().map(|c| c.id().get()).collect();
                    pick(&all, k).map(Value::from)
                }
            }
            "city_id" => self.city.map(|c| json!(c.get())),
            "x" | "y" => tool_spec.map(|t| self.coord(t, name)),
            "player_id" if tool == "city_state_action" => {
                let cs: Vec<PlayerId> = g.city_states(true).map(|p| p.id()).collect();
                pick(&cs, k).map(|p| json!(p.0))
            }
            "player_id" | "to" => pick(&majors, k).or_else(|| pick(&others, k)).map(|p| json!(p.0)),
            "candidate" => {
                if k.is_multiple_of(5) {
                    Some(json!("abstain"))
                } else {
                    let all: Vec<PlayerId> = g.majors(true).map(|p| p.id()).collect();
                    pick(&all, k).map(|p| json!(p.0))
                }
            }
            "negotiation_id" => {
                let open: Vec<u32> = g
                    .negotiations()
                    .iter()
                    .filter(|n| {
                        n.status == NegStatus::Open
                            && (n.initiator == self.pid || n.responder == self.pid)
                    })
                    .map(|n| n.id.get())
                    .collect();
                let any: Vec<u32> = g.negotiations().iter().map(|n| n.id.get()).collect();
                pick(&open, k).or_else(|| pick(&any, k)).map(Value::from)
            }
            "spy" => {
                names(espionage::spies(g, self.pid).iter().map(|s| s.name.to_string()).collect())
            }
            "tech" => {
                let queue: Vec<String> = g
                    .player(self.pid)
                    .map(|p| {
                        p.tech.queue.iter().filter_map(|&t| r.name(t)).map(str::to_owned).collect()
                    })
                    .unwrap_or_default();
                let now: Vec<String> = available_techs(g, self.pid)
                    .into_iter()
                    .filter_map(|t| r.name(t).map(str::to_owned))
                    .collect();
                if tool == "dequeue_research" && !queue.is_empty() {
                    names(queue)
                } else {
                    names(now)
                }
            }
            "item" => self.city.and_then(|c| {
                let items = buildable_items(g, c);
                let mut all: Vec<String> = Vec::new();
                if tool == "buy"
                    && let Some(first) = g.city(c).and_then(|x| x.queue.first().copied())
                    && !matches!(first, Constructible::Perpetual(_))
                {
                    all.push(
                        citar_engine::game::cities::construction::item_name(r, first).to_owned(),
                    );
                }
                all.extend(items.units.iter().filter_map(|u| r.name(u).map(str::to_owned)));
                all.extend(
                    items
                        .buildings
                        .iter()
                        .chain(items.wonders.iter())
                        .filter_map(|b| r.name(b).map(str::to_owned)),
                );
                names(all)
            }),
            "currency" => Some(json!(if k.is_multiple_of(3) { "Faith" } else { "Gold" })),
            "policy" => names(
                adoptable_policies(g, self.pid)
                    .into_iter()
                    .filter_map(|p| r.name(p).map(str::to_owned))
                    .collect(),
            ),
            "belief" | "beliefs" => {
                let open: Vec<String> =
                    beliefs_available(g, BeliefKind::Type(BeliefType::Pantheon))
                        .into_iter()
                        .chain(beliefs_available(g, BeliefKind::Type(BeliefType::Founder)))
                        .chain(beliefs_available(g, BeliefKind::Type(BeliefType::Follower)))
                        .filter_map(|b| r.name(b).map(str::to_owned))
                        .collect();
                let one = names(open);
                if name == "beliefs" { one.map(|b| json!([b])) } else { one }
            }
            "promotion" => self.unit.and_then(|u| {
                names(
                    promotions::available_promotions(g, u)
                        .into_iter()
                        .filter_map(|p| r.name(p).map(str::to_owned))
                        .collect(),
                )
            }),
            "improvement" => self.unit.and_then(|u| {
                let b = Builder::unit(g, u)?;
                let t = g.unit(u)?.tile();
                let mut all: Vec<String> = workers::build_options(g, &b, t, None)
                    .into_iter()
                    .filter_map(|o| r.name(o.imp).map(str::to_owned))
                    .collect();
                all.push("cancel".to_owned());
                names(all)
            }),
            "great_person" => names(
                great_people_types(g, self.pid)
                    .into_iter()
                    .filter_map(|u| r.name(u).map(str::to_owned))
                    .collect(),
            ),
            "order" => Some(json!(ORDERS[usize::from(k) % ORDERS.len()])),
            "action" => match tool {
                "unit_action" => self.unit.and_then(|u| {
                    names(
                        unit_actions(g, u)
                            .into_iter()
                            .filter(|a| a.available())
                            .map(|a| a.id)
                            .collect(),
                    )
                }),
                "respond_negotiation" => Some(json!(RESPONSES[usize::from(k) % RESPONSES.len()])),
                "city_state_action" => {
                    Some(json!(CITY_STATE_ACTIONS[usize::from(k) % CITY_STATE_ACTIONS.len()]))
                }
                _ => None,
            },
            "focus" => pick(&CityFocus::ALL, k).map(|f| json!(f.name())),
            "mode" => Some(json!(if k.is_multiple_of(2) { "append" } else { "replace" })),
            "topic" => Some(json!(TOPICS[usize::from(k) % TOPICS.len()])),
            "filter" => Some(json!(if k.is_multiple_of(2) { "available" } else { "all" })),
            "name" => Some(json!(match tool {
                "get_rules" => "Warrior".to_owned(),
                "set_civ_name" => format!("Realm {k}"),
                "unit_action" => format!("Faith {k}"),
                _ => format!("Spec Town {k}"),
            })),
            "leader" => Some(json!(format!("Leader {k}"))),
            "text" | "message" => Some(json!(format!("Message {k}."))),
            "give" | "receive" => Some(deal_items(k)),
            "specialists" => self.city.map(|c| {
                let slots = max_specialists(g, c);
                let mut m = Map::new();
                if let Some(&(s, n)) = slots.get(usize::from(k) % slots.len().max(1)) {
                    let name = r.specialists().get(s).map_or("Scientist", |d| &*d.name);
                    let most = u16::try_from(n).unwrap_or(0).saturating_add(1);
                    m.insert(name.to_owned(), json!(k % most.max(1)));
                }
                Value::Object(m)
            }),
            "amount" => Some(json!(1 + k % 300)),
            "index" => Some(json!(k % 4)),
            "since_id" => Some(json!(k)),
            "radius" => Some(json!(k % 5)),
            "message_limit" => Some(json!(k % 8)),
            _ => None,
        };
        v.unwrap_or_else(|| typed(ty, k))
    }
}

/// A value of type `ty` when the game has nothing to offer for a parameter.
fn typed(ty: SchemaType, k: u16) -> Value {
    match ty {
        SchemaType::Integer | SchemaType::Number | SchemaType::IntegerOrString => json!(k),
        SchemaType::String | SchemaType::Any => json!("Spec"),
        SchemaType::Boolean => json!(k.is_multiple_of(2)),
        SchemaType::Strings => json!(["Spec"]),
        SchemaType::Objects => json!([{"type": "embassy"}]),
        SchemaType::Object => json!({}),
    }
}

/// The valid arguments with those `mask` picks sent as the wrong type, or, one time in eight, a
/// required one left out.
fn confuse(mut m: Map<String, Value>, spec: &ToolSpec, mask: u8) -> Value {
    if mask % 8 == 7
        && let Some(first) = spec.args.required.first()
    {
        m.shift_remove(*first);
        return Value::Object(m);
    }
    let mut any = false;
    for (i, p) in spec.args.params.iter().enumerate() {
        let hit = (mask >> (i % 8)) & 1 == 1;
        let last = i + 1 == spec.args.params.len();
        if !(hit || (last && !any)) {
            continue;
        }
        let Some(v) = m.get(p.name).cloned() else { continue };
        any = true;
        m.insert(
            p.name.to_owned(),
            wrong(&v, p.json, mask.wrapping_add(u8::try_from(i % 256).unwrap_or(0))),
        );
    }
    if !any {
        // A tool that takes nothing gets what it does not take, which it must ignore.
        m.insert("why".to_owned(), json!(mask));
    }
    Value::Object(m)
}

/// `v` sent as something other than its type `ty`.
fn wrong(v: &Value, ty: SchemaType, k: u8) -> Value {
    let text = match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    match ty {
        SchemaType::Integer | SchemaType::Number => match k % 5 {
            0 => json!(text),
            1 => json!(format!(" {text} ")),
            2 => json!("twelve"),
            3 => json!(1.5),
            _ => json!(true),
        },
        SchemaType::String => match k % 5 {
            0 => json!(7),
            1 => json!(false),
            2 => json!([text]),
            3 => json!({"name": text}),
            _ => json!(""),
        },
        SchemaType::Boolean => match k % 4 {
            0 => json!("yes"),
            1 => json!("true"),
            2 => json!(1),
            _ => json!("maybe"),
        },
        SchemaType::Strings => match k % 3 {
            0 => json!(text),
            1 => json!([1, 2]),
            _ => json!({"a": text}),
        },
        SchemaType::Objects => match k % 3 {
            0 => json!("gold"),
            1 => json!(["gold", "embassy"]),
            _ => json!({"type": "gold", "amount": 5}),
        },
        SchemaType::Object => match k % 3 {
            0 => json!(text),
            1 => json!([text]),
            _ => json!(3),
        },
        SchemaType::IntegerOrString | SchemaType::Any => match k % 3 {
            0 => json!(true),
            1 => json!(2.5),
            _ => json!([text]),
        },
    }
}

/// Arguments drawn at random from `seed`: an object of some of the tool's parameters and other
/// keys, with values of any JSON type, or, one time in four, no object at all.
#[must_use]
pub fn random_args(spec: &ToolSpec, seed: u32) -> Value {
    let mut rng = Rng::keyed(u64::from(seed), Purpose::TestAgent, &[0x5eed, 0xa9]);
    if rng.below(4) == 0 {
        return random_json(&mut rng, 2);
    }
    let mut m = Map::new();
    for p in spec.args.params {
        if rng.chance(0.7) {
            m.insert(p.name.to_owned(), random_json(&mut rng, 2));
        }
    }
    for _ in 0..rng.below(3) {
        m.insert(random_text(&mut rng), random_json(&mut rng, 1));
    }
    Value::Object(m)
}

/// A JSON value of any type, nested at most `depth` deep, nulls included at any depth. A refusal
/// may quote a null inside a value back as Python's `None`, which property P5 reads as the
/// caller's (`stability::refusal_rule_broken`).
fn random_json(rng: &mut Rng, depth: u32) -> Value {
    let n = if depth == 0 { 6 } else { 8 };
    match rng.below(n) {
        0 => {
            if rng.chance(0.75) {
                Value::Null
            } else {
                json!("null")
            }
        }
        1 => json!(rng.chance(0.5)),
        2 => json!(rng.range(-5, 40)),
        3 => {
            let big = [i64::MIN, -1, 0, 999_999, i64::MAX, 1 << 40];
            json!(big[usize::try_from(rng.below(6)).unwrap_or(0)])
        }
        4 => {
            let f = [0.5, -2.25, 1e308, -0.0, 3.0];
            json!(f[usize::try_from(rng.below(5)).unwrap_or(0)])
        }
        5 => json!(random_text(rng)),
        6 => Value::Array((0..rng.below(4)).map(|_| random_json(rng, depth - 1)).collect()),
        _ => Value::Object(
            (0..rng.below(4)).map(|_| (random_text(rng), random_json(rng, depth - 1))).collect(),
        ),
    }
}

/// Text of a few characters, some of them quotes, brackets, backslashes and letters no ruleset
/// uses.
fn random_text(rng: &mut Rng) -> String {
    const CHARS: [&str; 16] =
        ["a", "Z", "7", " ", "'", "\"", "\\", "{", "]", "é", "日", "-", "_", ".", "\n", "0"];
    let n = rng.below(12);
    (0..n).map(|_| CHARS[usize::try_from(rng.below(16)).unwrap_or(0)]).collect()
}

impl<'a> arbitrary::Arbitrary<'a> for ActionSpec {
    fn arbitrary(u: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        let tool = u.arbitrary()?;
        let actor = u.arbitrary()?;
        let target = u.arbitrary()?;
        let coords = (u.arbitrary()?, u.arbitrary()?);
        let r: u8 = u.arbitrary()?;
        let arg: u32 = u.arbitrary()?;
        Ok(Self { tool, actor, target, coords, shape: Shape::weighted(r % 100, arg) })
    }
}
