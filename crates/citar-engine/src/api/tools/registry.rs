//! The registry of the player tools (`tools.py:15-68, 135-137`): each tool's name, what a model
//! reads about it, its JSON schema, whether it is a query or an action, whether it may be used
//! when it is not the caller's turn, and its category (DESIGN.md 8.3, package 1d-01).
//!
//! [`TOOLS`] is the one table of the 61 tools, in the order `tools.py` registered them (21
//! queries, then 40 actions): [`schemas`] and [`schemas_json`] list them as `tool_list` did, and
//! [`crate::game::Game::execute`] dispatches on them. The descriptions are sent to language
//! models as the tool definitions and shown in the browser, so they are Python's words, fixed
//! where they were wrong about the rules: each fix cites its entry in `tests/rules/intended.toml`,
//! and `tests/rules/tool_list.json`, Python's own list, is what the rest must equal.
//!
//! A tool's arguments ([`ToolArgs`]) are its parameters in the order it declares them, which
//! is the order [`super::normalize()`] coerces them in; an action's are the fields of its
//! [`crate::game::Action`] variant, with the tool's argument names.

use std::sync::OnceLock;

use serde_json::{Map, Value, json};

use super::args::{
    Param, SchemaType, ToolArgs, X, Y, boolean, int, int_or_string, items, object, string, strings,
};

/// Whether a tool reads or changes the game (`Tool.kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolKind {
    /// Reads the game and changes nothing; asked freely, at any time.
    Query,
    /// Changes the game, is logged, and is refused when it is not the caller's turn unless the
    /// tool may be used at any time.
    Action,
}

impl ToolKind {
    /// The kind's name on the wire: `query` or `action`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Action => "action",
        }
    }

    /// The kind called `name`, if any.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "query" => Some(Self::Query),
            "action" => Some(Self::Action),
            _ => None,
        }
    }
}

/// Where the browser shows a tool (`Tool.category`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    /// What the player knows: the briefing, the map, units, cities, the empire.
    Info,
    /// Relations, negotiations, city-states and spies.
    Diplomacy,
    /// What a unit does.
    Unit,
    /// What a city does.
    City,
    /// Research, policies, religion, great people and the civilization's name.
    Empire,
    /// The player's notebook and reasoning.
    Meta,
    /// Ending the turn.
    Turn,
}

impl Category {
    /// The category's name on the wire.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Diplomacy => "diplomacy",
            Self::Unit => "unit",
            Self::City => "city",
            Self::Empire => "empire",
            Self::Meta => "meta",
            Self::Turn => "turn",
        }
    }
}

/// The query tools, one per `@tool(kind="query")` of `tools.py:200-406`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Query {
    /// `get_briefing`.
    Briefing,
    /// `get_map`.
    Map,
    /// `get_tile`.
    Tile,
    /// `get_unit`.
    Unit,
    /// `get_units`.
    Units,
    /// `get_city`.
    City,
    /// `get_cities`.
    Cities,
    /// `get_empire`.
    Empire,
    /// `get_players`.
    Players,
    /// `get_diplomacy`.
    Diplomacy,
    /// `get_city_states`.
    CityStates,
    /// `get_tech_tree`.
    TechTree,
    /// `get_policies`.
    Policies,
    /// `get_religion`.
    Religion,
    /// `get_great_people`.
    GreatPeople,
    /// `get_espionage`.
    Espionage,
    /// `get_rules`.
    Rules,
    /// `read_notes`.
    ReadNotes,
    /// `get_events`.
    Events,
    /// `preview_attack`.
    PreviewAttack,
    /// `get_victory_status`.
    VictoryStatus,
}

/// What a call to a tool runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    /// A query, answered from the game as it is.
    Query(Query),
    /// An action: the arguments are read into its [`crate::game::Action`] variant, which the
    /// tool's name tags, and taken through `Game::act`.
    Action,
}

/// One tool (`tools.Tool`, `tools.py:15-48`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolSpec {
    /// Its name, parameters and required parameters.
    pub args: ToolArgs,
    /// What a model reads about it.
    pub description: &'static str,
    /// What a call runs, which says the tool's kind.
    pub op: Op,
    /// Whether it may be used when it is not the caller's turn.
    pub any_time: bool,
    /// Where the browser shows it.
    pub category: Category,
}

impl ToolSpec {
    /// The tool's name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.args.tool
    }

    /// Whether it reads or changes the game.
    #[must_use]
    pub const fn kind(&self) -> ToolKind {
        match self.op {
            Op::Query(_) => ToolKind::Query,
            Op::Action => ToolKind::Action,
        }
    }

    /// The JSON Schema of its arguments (`Tool.schema`, `tools.py:37-43`). Unknown arguments
    /// are refused by the schema and dropped by the call: a model is told, and a caller that
    /// ignores the schema still gets its turn (DESIGN.md 8.3).
    #[must_use]
    pub fn schema(&self) -> Value {
        let props: Map<String, Value> =
            self.args.params.iter().map(|p| (p.name.to_owned(), param_schema(p))).collect();
        json!({
            "type": "object",
            "properties": props,
            "required": self.args.required,
            "additionalProperties": false,
        })
    }

    /// The tool as a model, an MCP client and the browser receive it (`Tool.to_dict`,
    /// `tools.py:45-48`).
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description,
            "input_schema": self.schema(),
            "kind": self.kind().name(),
            "any_time": self.any_time,
            "category": self.category.name(),
        })
    }
}

/// One parameter's schema, its keys in Python's order: the type, the items of an array, the
/// values allowed, and the description.
fn param_schema(p: &Param) -> Value {
    let mut m = Map::new();
    let ty = match p.json {
        SchemaType::Integer => json!("integer"),
        SchemaType::Number => json!("number"),
        SchemaType::String => json!("string"),
        SchemaType::Boolean => json!("boolean"),
        SchemaType::Strings | SchemaType::Objects => json!("array"),
        SchemaType::Object => json!("object"),
        SchemaType::IntegerOrString => json!(["integer", "string"]),
        SchemaType::Any => Value::Null,
    };
    if !ty.is_null() {
        m.insert("type".into(), ty);
    }
    match p.json {
        SchemaType::Strings => {
            m.insert("items".into(), json!({"type": "string"}));
        }
        SchemaType::Objects => {
            m.insert("items".into(), json!({"type": "object"}));
        }
        _ => {}
    }
    if !p.choices.is_empty() {
        m.insert("enum".into(), json!(p.choices));
    }
    if let Some(d) = p.description {
        m.insert("description".into(), json!(d));
    }
    Value::Object(m)
}

/// The tool called `name`, exactly.
#[must_use]
pub fn tool(name: &str) -> Option<&'static ToolSpec> {
    // The registry's positions by name, sorted once: a name is looked up on every call.
    static BY_NAME: OnceLock<Vec<(&'static str, u8)>> = OnceLock::new();
    let index = BY_NAME.get_or_init(|| {
        let mut v: Vec<(&'static str, u8)> = TOOLS
            .iter()
            .enumerate()
            .map(|(i, t)| (t.name(), u8::try_from(i).unwrap_or(u8::MAX)))
            .collect();
        v.sort();
        v
    });
    let at = index.binary_search_by(|&(n, _)| n.cmp(name)).ok()?;
    TOOLS.get(usize::from(index[at].1))
}

/// Whether the tool called `name` is a query or an action; `None` for a name that is no tool
/// (`engine_api.tool_kind`).
#[must_use]
pub fn kind(name: &str) -> Option<ToolKind> {
    tool(name).map(ToolSpec::kind)
}

/// Every tool as a model receives it, in the registry's order, or only the queries or the
/// actions (`tools.tool_list`, `tools.py:135-137`).
#[must_use]
pub fn schemas(kind: Option<ToolKind>) -> Vec<Value> {
    TOOLS.iter().filter(|t| kind.is_none_or(|k| t.kind() == k)).map(ToolSpec::to_json).collect()
}

/// [`schemas`] of every tool as JSON text, built once: hosts send it to every model and
/// browser, and it never changes while the engine runs.
#[must_use]
pub fn schemas_json() -> &'static str {
    static JSON: OnceLock<String> = OnceLock::new();
    JSON.get_or_init(|| Value::Array(schemas(None)).to_string())
}

/// The tools, in the order `tools.py` registered them (`tools.REGISTRY`): the 21 queries of
/// `tools.py:200-406`, then the 40 actions of `tools.py:412-1109`.
pub static TOOLS: [ToolSpec; 61] = [
    // `tools.get_briefing` (tools.py:200-210)
    ToolSpec {
        args: ToolArgs { tool: "get_briefing", params: &[], required: &[] },
        description: "Your turn briefing: empire status, cities, units (idle ones marked), events \
            since your last turn, diplomacy, and a to-do list. Start every turn with this.",
        op: Op::Query(Query::Briefing),
        any_time: true,
        category: Category::Info,
    },
    // `tools.get_map` (tools.py:213-229)
    ToolSpec {
        args: ToolArgs {
            tool: "get_map",
            params: &[
                int("x"),
                int("y"),
                int("radius").described("rows above/below center (default 8)"),
                boolean("legend").described("include the map legend (default false)"),
            ],
            required: &[],
        },
        description: "ASCII hex map of the area around (x,y) (defaults to your capital) showing \
            terrain, features, units, cities, camps, ruins and resources you know about. Radius \
            2-20.",
        op: Op::Query(Query::Map),
        any_time: true,
        category: Category::Info,
    },
    // `tools.get_tile` (tools.py:232-237)
    ToolSpec {
        args: ToolArgs { tool: "get_tile", params: &[X, Y], required: &["x", "y"] },
        description: "Details of one tile you have explored: terrain, yields, resource, \
            improvement, owner, units.",
        op: Op::Query(Query::Tile),
        any_time: true,
        category: Category::Info,
    },
    // `tools.get_unit` (tools.py:240-246)
    ToolSpec {
        args: ToolArgs { tool: "get_unit", params: &[int("unit_id")], required: &["unit_id"] },
        description: "Full details of one of your units: special actions (unit_action), build \
            options, reachable tiles, attack targets with predicted damage, upgrade and promotion \
            options.",
        op: Op::Query(Query::Unit),
        any_time: true,
        category: Category::Info,
    },
    // `tools.get_units` (tools.py:249-253)
    ToolSpec {
        args: ToolArgs { tool: "get_units", params: &[], required: &[] },
        description: "Compact list of all your units.",
        op: Op::Query(Query::Units),
        any_time: true,
        category: Category::Info,
    },
    // `tools.get_city` (tools.py:256-262)
    ToolSpec {
        args: ToolArgs { tool: "get_city", params: &[int("city_id")], required: &["city_id"] },
        description: "Full details of one of your cities: yields, growth, production queue, \
            everything it can build or buy (gold/faith), specialists, worked and buyable tiles, \
            religion.",
        op: Op::Query(Query::City),
        any_time: true,
        category: Category::Info,
    },
    // `tools.get_cities` (tools.py:265-269)
    ToolSpec {
        args: ToolArgs { tool: "get_cities", params: &[], required: &[] },
        description: "Summary of your cities.",
        op: Op::Query(Query::Cities),
        any_time: true,
        category: Category::Info,
    },
    // `tools.get_empire` (tools.py:272-277)
    ToolSpec {
        args: ToolArgs { tool: "get_empire", params: &[], required: &[] },
        description: "Empire overview: gold, science, culture and faith per turn with breakdowns, \
            happiness, golden age, resources, era, score and spaceship progress.",
        op: Op::Query(Query::Empire),
        any_time: true,
        category: Category::Info,
    },
    // `tools.get_players` (tools.py:280-285)
    ToolSpec {
        args: ToolArgs { tool: "get_players", params: &[], required: &[] },
        description: "All civilizations and city-states you know of (ids, names, war/peace status, \
            score).",
        op: Op::Query(Query::Players),
        any_time: true,
        category: Category::Info,
    },
    // `tools.get_diplomacy` (tools.py:288-298)
    ToolSpec {
        args: ToolArgs { tool: "get_diplomacy", params: &[int("message_limit")], required: &[] },
        description: "Diplomacy: relations and agreements with each civ, what could be traded, \
            open negotiations (with history and the current proposal from your perspective), \
            active deals and recent messages.",
        op: Op::Query(Query::Diplomacy),
        any_time: true,
        category: Category::Diplomacy,
    },
    // `tools.get_city_states` (tools.py:301-306)
    ToolSpec {
        args: ToolArgs { tool: "get_city_states", params: &[], required: &[] },
        description: "City-states you have met: type, personality, influence, ally, your bonuses, \
            quests, and what tribute they would pay.",
        op: Op::Query(Query::CityStates),
        any_time: true,
        category: Category::Diplomacy,
    },
    // `tools.get_tech_tree` (tools.py:309-321)
    ToolSpec {
        args: ToolArgs { tool: "get_tech_tree", params: &[string("filter")], required: &[] },
        description: "Technologies with status (known/available/locked), cost, prerequisites and \
            what each unlocks. filter: 'available' (default), 'all', or 'locked'.",
        op: Op::Query(Query::TechTree),
        any_time: true,
        category: Category::Info,
    },
    // `tools.get_policies` (tools.py:324-329)
    ToolSpec {
        args: ToolArgs { tool: "get_policies", params: &[], required: &[] },
        description: "Social policies: culture, cost of the next policy, branches \
            (open/locked/completed) with their policies and effects, and what you can adopt now.",
        op: Op::Query(Query::Policies),
        any_time: true,
        category: Category::Info,
    },
    // `tools.get_religion` (tools.py:332-337)
    ToolSpec {
        args: ToolArgs { tool: "get_religion", params: &[], required: &[] },
        description: "Religion: your faith, pantheon/religion, available beliefs, faith needed for \
            the next Great Prophet, religions in the world and what you can buy with faith.",
        op: Op::Query(Query::Religion),
        any_time: true,
        category: Category::Info,
    },
    // `tools.get_great_people` (tools.py:340-345)
    ToolSpec {
        args: ToolArgs { tool: "get_great_people", params: &[], required: &[] },
        description: "Great person points and progress per type, golden age progress and free \
            great people to choose.",
        op: Op::Query(Query::GreatPeople),
        any_time: true,
        category: Category::Info,
    },
    // `tools.get_espionage` (tools.py:348-353)
    ToolSpec {
        args: ToolArgs { tool: "get_espionage", params: &[], required: &[] },
        description: "Your spies, where they are and what they are doing.",
        op: Op::Query(Query::Espionage),
        any_time: true,
        category: Category::Info,
    },
    // `tools.get_rules` (tools.py:356-367)
    ToolSpec {
        args: ToolArgs {
            tool: "get_rules",
            params: &[string("topic"), string("name")],
            required: &["topic"],
        },
        description: "Look up game rules (UnCiv 'Civ V - Gods & Kings' data). topic: units | \
            buildings | techs | improvements | resources | promotions | terrains | policies | \
            beliefs | specialists | eras | nations | city_state_types | speeds | difficulties | \
            deal_items | combat | overview. Optionally name for one entry.",
        op: Op::Query(Query::Rules),
        any_time: true,
        category: Category::Info,
    },
    // `tools.read_notes` (tools.py:370-374)
    ToolSpec {
        args: ToolArgs { tool: "read_notes", params: &[], required: &[] },
        description: "Read your private strategy notebook (persists across turns and saves).",
        op: Op::Query(Query::ReadNotes),
        any_time: true,
        category: Category::Meta,
    },
    // `tools.get_events` (tools.py:377-386)
    ToolSpec {
        args: ToolArgs { tool: "get_events", params: &[int("since_id")], required: &[] },
        description: "Your notifications since a given event id (default: the last 40).",
        op: Op::Query(Query::Events),
        any_time: true,
        category: Category::Info,
    },
    // `tools.preview_attack` (tools.py:389-398)
    ToolSpec {
        args: ToolArgs {
            tool: "preview_attack",
            params: &[int("unit_id"), X, Y],
            required: &["unit_id", "x", "y"],
        },
        description: "Predict an attack (strengths, modifiers, damage range) without performing \
            it.",
        op: Op::Query(Query::PreviewAttack),
        any_time: true,
        category: Category::Unit,
    },
    // `tools.get_victory_status` (tools.py:401-406)
    ToolSpec {
        args: ToolArgs { tool: "get_victory_status", params: &[], required: &[] },
        description: "Victory conditions and progress: scores, milestones per victory type, \
            spaceship, United Nations vote and the turn limit.",
        op: Op::Query(Query::VictoryStatus),
        any_time: true,
        category: Category::Info,
    },
    // `tools.move_unit` (tools.py:412-449)
    ToolSpec {
        args: ToolArgs {
            tool: "move_unit",
            params: &[int("unit_id"), X, Y],
            required: &["unit_id", "x", "y"],
        },
        description: "Move a unit toward (x,y) using the best path. Moves as far as possible this \
            turn and keeps going on later turns automatically. Aircraft rebase to a city or \
            carrier instead. Moving a military unit onto an undefended enemy civilian captures it \
            (requires war); onto a barbarian camp clears it; onto ruins explores them.",
        op: Op::Action,
        any_time: false,
        category: Category::Unit,
    },
    // `tools.attack_tool` (tools.py:452-474)
    ToolSpec {
        args: ToolArgs {
            tool: "attack",
            params: &[int("unit_id"), X, Y],
            required: &["unit_id", "x", "y"],
        },
        description: "Attack the tile (x,y): melee units must be adjacent, ranged units within \
            range and line of sight, aircraft within their range (air strike), nuclear weapons \
            detonate on the target. Target may be an enemy unit or city (you must be at war; \
            nukes also declare war). A melee attack on a city at 0 HP captures it.",
        op: Op::Action,
        any_time: false,
        category: Category::Unit,
    },
    // `tools.air_sweep` (tools.py:477-482)
    ToolSpec {
        args: ToolArgs {
            tool: "air_sweep",
            params: &[int("unit_id"), X, Y],
            required: &["unit_id", "x", "y"],
        },
        description: "A fighter sweeps the tile (x,y), attacking enemy interceptors before your \
            bombers go in.",
        op: Op::Action,
        any_time: false,
        category: Category::Unit,
    },
    // `tools.unit_action` (tools.py:485-505)
    ToolSpec {
        args: ToolArgs {
            tool: "unit_action",
            params: &[
                int("unit_id"),
                string("action"),
                string("name"),
                strings("beliefs"),
                int("x"),
                int("y"),
            ],
            required: &["unit_id", "action"],
        },
        description: "Use a unit's special ability. get_unit lists its actions with ids, e.g. \
            found_city (Settler; optional name), found_religion (Great Prophet in your city; name \
            + beliefs), enhance_religion (beliefs), spread_religion, remove_heresy, \
            hurry_research, hurry_construction, trade_mission, create:<Improvement> (Academy, \
            Citadel, Holy site, Landmark, Manufactory, Customs house, Fishing Boats with Work \
            Boats...), add_to_spaceship, paradrop (Paratrooper; x, y of the target tile), and \
            one-time effects such as a Great Artist's golden age (trigger:<n>).",
        op: Op::Action,
        any_time: false,
        category: Category::Unit,
    },
    // `tools.found_city_tool` (tools.py:508-526)
    ToolSpec {
        args: ToolArgs {
            tool: "found_city",
            params: &[int("unit_id"), string("name")],
            required: &["unit_id"],
        },
        description: "Found a city with a Settler on its current tile (same as unit_action \
            found_city).",
        op: Op::Action,
        any_time: false,
        category: Category::Unit,
    },
    // `tools.build_improvement` (tools.py:529-537)
    ToolSpec {
        args: ToolArgs {
            tool: "build_improvement",
            params: &[int("unit_id"), string("improvement")],
            required: &["unit_id", "improvement"],
        },
        description: "Order a Worker to build on its current tile (farm, mine, pasture, \
            plantation, camp, quarry, lumber mill, trading post, road, railroad, fort, remove \
            forest/jungle/marsh, repair...). If a feature must be removed first, that is queued \
            automatically. get_unit lists the valid options with turns. Work progresses at the \
            end of each turn the worker spends on the tile with movement left.",
        op: Op::Action,
        any_time: false,
        category: Category::Unit,
    },
    // `tools.unit_order` (tools.py:540-602)
    ToolSpec {
        args: ToolArgs {
            tool: "unit_order",
            params: &[int("unit_id"), string("order")],
            required: &["unit_id", "order"],
        },
        description: "Give a standing order: fortify (military), sleep (until woken or enemies \
            near), wake, skip (end this unit's turn), heal (rest until healed), explore \
            (automatic), automate (automatic worker), pillage (enemy improvement here), setup \
            (siege units), disband (delete; refunds a little gold inside your borders), cancel \
            (clear orders).",
        op: Op::Action,
        any_time: false,
        category: Category::Unit,
    },
    // `tools.upgrade_unit` (tools.py:605-610)
    ToolSpec {
        args: ToolArgs { tool: "upgrade_unit", params: &[int("unit_id")], required: &["unit_id"] },
        description: "Upgrade an obsolete unit (costs gold; must be in your territory with moves \
            left).",
        op: Op::Action,
        any_time: false,
        category: Category::Unit,
    },
    // `tools.promote_unit` (tools.py:613-625)
    ToolSpec {
        args: ToolArgs {
            tool: "promote_unit",
            params: &[int("unit_id"), string("promotion")],
            required: &["unit_id", "promotion"],
        },
        description: "Choose a promotion for a unit that has enough XP (see get_unit for options).",
        op: Op::Action,
        any_time: false,
        category: Category::Unit,
    },
    // `tools.set_production` (tools.py:631-637)
    ToolSpec {
        args: ToolArgs {
            tool: "set_production",
            params: &[int("city_id"), string("item"), boolean("append")],
            required: &["city_id", "item"],
        },
        description: "Set what a city builds (unit, building, wonder, project, or Gold/Science \
            conversion). append=true adds to the end of the queue instead of replacing the \
            current item. Stored production carries over.",
        op: Op::Action,
        any_time: false,
        category: Category::City,
    },
    // `tools.change_queue` (tools.py:640-647)
    ToolSpec {
        args: ToolArgs {
            tool: "change_queue",
            params: &[
                int("city_id"),
                int("index"),
                string("action").one_of(&["up", "down", "first", "last", "remove", "clear"]),
            ],
            required: &["city_id", "action"],
        },
        description: "Edit a city's production queue: move the entry at position index (0 = in \
            production) up, down, first or last, remove it, or clear the whole queue.",
        op: Op::Action,
        any_time: false,
        category: Category::City,
    },
    // `tools.set_auto_production` (tools.py:650-662)
    ToolSpec {
        args: ToolArgs {
            tool: "set_auto_production",
            params: &[int("city_id"), boolean("enabled")],
            required: &["city_id", "enabled"],
        },
        description: "Turn automatic production on or off for a city: when its queue runs empty, \
            the built-in advisor picks the next item.",
        op: Op::Action,
        any_time: false,
        category: Category::City,
    },
    // `tools.buy` (tools.py:665-676)
    ToolSpec {
        args: ToolArgs {
            tool: "buy",
            params: &[int("city_id"), string("item"), string("currency")],
            required: &["city_id", "item"],
        },
        description: "Buy a unit or building in a city immediately. currency: Gold (default) or \
            Faith (religious units, some buildings and, with the right beliefs or policies, other \
            items). Puppets cannot buy.",
        op: Op::Action,
        any_time: false,
        category: Category::City,
    },
    // `tools.set_city_focus` (tools.py:679-700)
    ToolSpec {
        args: ToolArgs {
            tool: "set_city_focus",
            params: &[int("city_id"), string("focus"), boolean("avoid_growth")],
            required: &["city_id"],
        },
        description: "Citizen focus: balanced, food, production, gold, science, culture, faith, \
            happiness, gold_growth, production_growth, or manual (keep your tile and specialist \
            choices). avoid_growth=true stops the city from growing.",
        op: Op::Action,
        any_time: false,
        category: Category::City,
    },
    // `tools.work_tile` (tools.py:703-724)
    ToolSpec {
        args: ToolArgs {
            tool: "work_tile",
            params: &[int("city_id"), X, Y, boolean("locked")],
            required: &["city_id", "x", "y"],
        },
        description: "Lock (or unlock) a citizen onto a specific tile of a city.",
        op: Op::Action,
        any_time: false,
        category: Category::City,
    },
    // `tools.set_specialists` (tools.py:727-757)
    ToolSpec {
        args: ToolArgs {
            tool: "set_specialists",
            params: &[int("city_id"), object("specialists")],
            required: &["city_id", "specialists"],
        },
        description: "Assign specialists manually, e.g. {\"Scientist\": 2, \"Engineer\": 1} \
            (limited by the city's buildings; the remaining citizens work tiles). Pass {} to \
            return to automatic specialists.",
        op: Op::Action,
        any_time: false,
        category: Category::City,
    },
    // `tools.buy_tile` (tools.py:760-767)
    ToolSpec {
        args: ToolArgs {
            tool: "buy_tile",
            params: &[int("city_id"), X, Y],
            required: &["city_id", "x", "y"],
        },
        description: "Buy an unowned tile next to a city's borders (within 3 tiles of the city) \
            with gold.",
        op: Op::Action,
        any_time: false,
        category: Category::City,
    },
    // `tools.city_attack` (tools.py:770-779)
    ToolSpec {
        args: ToolArgs {
            tool: "city_attack",
            params: &[int("city_id"), X, Y],
            required: &["city_id", "x", "y"],
        },
        description: "A city bombards an enemy unit within range (once per turn).",
        op: Op::Action,
        any_time: false,
        category: Category::City,
    },
    // `tools.rename_city` (tools.py:782-797)
    ToolSpec {
        args: ToolArgs {
            tool: "rename_city",
            params: &[int("city_id"), string("name")],
            required: &["city_id", "name"],
        },
        description: "Rename one of your cities.",
        op: Op::Action,
        any_time: true,
        category: Category::City,
    },
    // `tools.return_civilian` (tools.py:800-808)
    ToolSpec {
        args: ToolArgs {
            tool: "return_civilian",
            params: &[int("unit_id"), boolean("keep")],
            required: &["unit_id"],
        },
        description: "A civilian you recaptured from barbarians that belonged to another \
            civilization: return it to them (goodwill; +45 influence with a city-state) or keep \
            it. Unanswered by the end of your turn, you keep it.",
        op: Op::Action,
        any_time: false,
        category: Category::Unit,
    },
    // `tools.city_status` (tools.py:811-827)
    ToolSpec {
        args: ToolArgs {
            tool: "city_status",
            params: &[
                int("city_id"),
                string("status").one_of(&["annex", "puppet", "raze", "stop_razing", "liberate"]),
            ],
            required: &["city_id", "status"],
        },
        description: "Decide what to do with a conquered city: annex (full control; unhappiness \
            until a Courthouse), puppet (keeps its own production, lower unhappiness), raze (burn \
            it down 1 population per turn; not original capitals or holy cities), stop_razing, or \
            liberate (return it to its original owner for their gratitude).",
        op: Op::Action,
        any_time: false,
        category: Category::City,
    },
    // `tools.set_research` (tools.py:833-843)
    ToolSpec {
        args: ToolArgs {
            tool: "set_research",
            params: &[string("tech"), boolean("append")],
            required: &["tech"],
        },
        description: "Research a technology. If it is not available yet, it becomes your goal and \
            prerequisites are researched automatically in order. append=true adds it to the end \
            of your research queue instead of replacing the queue.",
        op: Op::Action,
        any_time: false,
        category: Category::Empire,
    },
    // `tools.dequeue_research` (tools.py:846-851)
    ToolSpec {
        args: ToolArgs { tool: "dequeue_research", params: &[string("tech")], required: &["tech"] },
        description: "Remove a technology from your research queue, together with any queued \
            technology that needs it.",
        op: Op::Action,
        any_time: false,
        category: Category::Empire,
    },
    // `tools.choose_free_tech` (tools.py:854-859)
    ToolSpec {
        args: ToolArgs { tool: "choose_free_tech", params: &[string("tech")], required: &["tech"] },
        description: "Pick a free technology you have been granted (it must be researchable now).",
        op: Op::Action,
        any_time: false,
        category: Category::Empire,
    },
    // `tools.adopt_policy` (tools.py:862-868)
    ToolSpec {
        args: ToolArgs { tool: "adopt_policy", params: &[string("policy")], required: &["policy"] },
        description: "Adopt a social policy or open a policy branch with your culture (or a free \
            policy). get_policies lists the costs and what is adoptable.",
        op: Op::Action,
        any_time: false,
        category: Category::Empire,
    },
    // `tools.found_pantheon` (tools.py:871-876)
    ToolSpec {
        args: ToolArgs {
            tool: "found_pantheon",
            params: &[string("belief")],
            required: &["belief"],
        },
        description: "Found a pantheon with enough faith by choosing a pantheon belief (see \
            get_religion).",
        op: Op::Action,
        any_time: false,
        category: Category::Empire,
    },
    // `tools.choose_great_person` (tools.py:879-884)
    ToolSpec {
        args: ToolArgs {
            tool: "choose_great_person",
            params: &[string("great_person")],
            required: &["great_person"],
        },
        description: "Choose a free Great Person you have been granted (e.g. Great Scientist).",
        op: Op::Action,
        any_time: false,
        category: Category::Empire,
    },
    // `tools.un_vote` (tools.py:887-897)
    ToolSpec {
        args: ToolArgs {
            tool: "un_vote",
            params: &[int_or_string("candidate")],
            required: &["candidate"],
        },
        description: "Vote in the United Nations world leader election (player id of a \
            civilization, or 'abstain'). Voting opens the turn before the vote.",
        op: Op::Action,
        any_time: true,
        category: Category::Diplomacy,
    },
    // `tools.set_civ_name` (tools.py:900-925)
    ToolSpec {
        args: ToolArgs {
            tool: "set_civ_name",
            params: &[string("name"), string("leader")],
            required: &["name"],
        },
        description: "Name your civilization (and optionally your leader). Anything you like; your \
            civilization's bonuses do not change.",
        op: Op::Action,
        any_time: true,
        category: Category::Empire,
    },
    // `tools.send_message` (tools.py:931-937)
    ToolSpec {
        args: ToolArgs {
            tool: "send_message",
            params: &[int_or_string("to"), string("text")],
            required: &["to", "text"],
        },
        description: "Send a free-text message to a civilization you have met (player id) or 'all' \
            you have met. Nothing said is binding.",
        op: Op::Action,
        any_time: true,
        category: Category::Diplomacy,
    },
    // `tools.open_negotiation` (tools.py:940-956)
    ToolSpec {
        args: ToolArgs {
            tool: "open_negotiation",
            params: &[int("to"), string("message"), items("give"), items("receive")],
            required: &["to", "message"],
        },
        description: "Start a negotiation (on your turn) with a met civilization: a message \
            (required) plus an optional concrete proposal (give = what you give, receive = what \
            you want). Mutual agreements (peace, friendship, research agreement, defensive pact) \
            go on both sides automatically. The other side answers in its own time (accept, \
            reject, counter or reply) and you go back and forth, each with a message, until a \
            deal or a rejection. You cannot end your turn while the negotiation is open: wait for \
            the answer, or withdraw it with respond_negotiation action 'reject'. A negotiation \
            closes by itself at its message limit (get_diplomacy shows it).",
        op: Op::Action,
        any_time: false,
        category: Category::Diplomacy,
    },
    // `tools.respond_negotiation` (tools.py:959-976)
    ToolSpec {
        args: ToolArgs {
            tool: "respond_negotiation",
            params: &[
                int("negotiation_id"),
                string("action"),
                string("message"),
                items("give"),
                items("receive"),
            ],
            required: &["negotiation_id", "action"],
        },
        description: "Respond in a negotiation, with a message every time. action: accept (the \
            other side's current proposal), counter (a new proposal via give/receive from your \
            perspective, at least one item), reply (message only), or reject (end it). Accept, \
            counter and reply need it to be your move; reject works any time, which is how you \
            withdraw a negotiation the other side has not answered.",
        op: Op::Action,
        any_time: true,
        category: Category::Diplomacy,
    },
    // `tools.declare_war` (tools.py:979-987)
    ToolSpec {
        args: ToolArgs {
            tool: "declare_war",
            params: &[int("player_id"), string("message")],
            required: &["player_id"],
        },
        description: "Declare war on a civilization or city-state you have met (breaks deals and \
            agreements; allies, defensive pacts and city-state allies may join).",
        op: Op::Action,
        any_time: false,
        category: Category::Diplomacy,
    },
    // `tools.denounce` (tools.py:990-995)
    ToolSpec {
        args: ToolArgs { tool: "denounce", params: &[int("player_id")], required: &["player_id"] },
        description: "Publicly denounce a civilization (ends friendship; others take note).",
        op: Op::Action,
        any_time: false,
        category: Category::Diplomacy,
    },
    // `tools.city_state_action` (tools.py:998-1034)
    ToolSpec {
        args: ToolArgs {
            tool: "city_state_action",
            params: &[int("player_id"), string("action"), int("amount"), int("unit_id")],
            required: &["player_id", "action"],
        },
        // Python's text said a unit next to the territory could be gifted, which the rule
        // refused (city_states.py:516-521).
        // refcheck: gift-unit-described-as-ruled
        description: "Interact with a city-state: gift_gold (amount), gift_unit (unit_id; the unit \
            must stand in its territory with movement left), pledge (protection), withdraw \
            (protection), tribute_gold or tribute_worker (demand; they must fear you), \
            make_peace, or marry (annex a long-time ally with gold, if your civilization can).",
        op: Op::Action,
        any_time: false,
        category: Category::Diplomacy,
    },
    // `tools.move_spy` (tools.py:1037-1044)
    ToolSpec {
        args: ToolArgs {
            tool: "move_spy",
            params: &[string("spy"), int_or_string("city_id")],
            required: &["spy", "city_id"],
        },
        description: "Send a spy to a city you have explored (foreign city: steal technology; \
            city-state capital: rig elections; your own city: counter-intelligence), or to \
            'hideout'. It arrives next turn and needs a few turns to establish a network.",
        op: Op::Action,
        any_time: false,
        category: Category::Diplomacy,
    },
    // `tools.stage_coup` (tools.py:1047-1052)
    ToolSpec {
        args: ToolArgs { tool: "stage_coup", params: &[string("spy")], required: &["spy"] },
        description: "Order a spy in a city-state capital to stage a coup at the end of your turn \
            (on success you become their ally; on failure the spy dies).",
        op: Op::Action,
        any_time: false,
        category: Category::Diplomacy,
    },
    // `tools.write_notes` (tools.py:1058-1073)
    ToolSpec {
        args: ToolArgs {
            tool: "write_notes",
            params: &[string("text"), string("mode")],
            required: &["text"],
        },
        description: "Write to your private strategy notebook (kept across turns; shown in your \
            briefing). mode: replace (default) or append.",
        op: Op::Action,
        any_time: true,
        category: Category::Meta,
    },
    // `tools.log_thought` (tools.py:1076-1091)
    ToolSpec {
        args: ToolArgs { tool: "log_thought", params: &[string("text")], required: &["text"] },
        description: "Record your reasoning for this turn (shown to spectators and in the game \
            replay, never to other players).",
        op: Op::Action,
        any_time: true,
        category: Category::Meta,
    },
    // `tools.end_turn` (tools.py:1094-1109)
    ToolSpec {
        args: ToolArgs { tool: "end_turn", params: &[], required: &[] },
        description: "End your turn. Units with standing orders keep executing them. Refused while \
            a negotiation you are in is open: answer it, wait for the other side's reply, or \
            withdraw it (respond_negotiation action 'reject').",
        op: Op::Action,
        any_time: false,
        category: Category::Turn,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_holds_python_s_61_tools_once_each() {
        let queries = TOOLS.iter().filter(|t| t.kind() == ToolKind::Query).count();
        assert_eq!((TOOLS.len(), queries), (61, 21));
        // The queries come first, as tools.py registered them.
        assert!(TOOLS[..21].iter().all(|t| t.kind() == ToolKind::Query));
        let mut names: Vec<&str> = TOOLS.iter().map(ToolSpec::name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 61, "no name twice");
        for t in &TOOLS {
            assert_eq!(tool(t.name()).map(ToolSpec::name), Some(t.name()));
            // Python marked every query `any_time`; a query is never refused for the turn.
            assert!(t.kind() == ToolKind::Action || t.any_time, "{}", t.name());
            assert!(!t.description.is_empty() && t.description.ends_with(['.', ')']));
        }
        assert_eq!(tool("fly_to_the_moon"), None);
        assert_eq!(kind("end_turn"), Some(ToolKind::Action));
        assert_eq!(kind("get_map"), Some(ToolKind::Query));
        assert_eq!(ToolKind::from_name("query"), Some(ToolKind::Query));
    }

    #[test]
    fn the_schemas_list_every_tool_as_a_model_reads_it() {
        let all: Value = serde_json::from_str(schemas_json()).expect("JSON");
        assert_eq!(all, Value::Array(schemas(None)));
        assert_eq!(schemas(Some(ToolKind::Action)).len(), 40);
        let tile = tool("get_tile").map(ToolSpec::to_json).expect("get_tile");
        assert_eq!(
            tile["input_schema"],
            json!({
                "type": "object",
                "properties": {
                    "x": {"type": "integer", "description": "column"},
                    "y": {"type": "integer", "description": "row"},
                },
                "required": ["x", "y"],
                "additionalProperties": false,
            })
        );
        let queue = tool("change_queue").map(ToolSpec::schema).expect("change_queue");
        assert_eq!(
            queue["properties"]["action"],
            json!({"type": "string", "enum": ["up", "down", "first", "last", "remove", "clear"]})
        );
        let vote = tool("un_vote").map(ToolSpec::schema).expect("un_vote");
        assert_eq!(vote["properties"]["candidate"], json!({"type": ["integer", "string"]}));
    }
}
