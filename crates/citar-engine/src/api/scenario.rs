//! Scenario operations: small JSON edits of a game, which the scenario editor sends and probe
//! cases use as their setup (`scenario.py:36-487`).
//!
//! Each operation is `{"op": name, ...parameters}`, and [`OPS`] lists them all with their
//! parameters, the reference the editor shows ([`ops_help`]). Coordinates are `x` and `y`,
//! players are ids, and ruleset objects are named loosely ("warrior" is the Warrior).
//! [`Game::apply_ops`](crate::game::Game::apply_ops) applies a list of them all or nothing.
//!
//! Package 1b-02 ports the framework and the operations whose rules exist: `grant_era`,
//! `grant_tech`, `remove_tech`, `set_player`, `set_tile`, `meet`, `set_relation`,
//! `set_influence`, `reveal` and `set_research`. The others are listed with the package that
//! ports their system, and are refused as not ported until then.
//!
//! What differs from Python, on purpose, each listed in `tests/rules/intended.toml` under its id
//! and cited where the fix is made:
//! - a list of operations applies all or nothing (DESIGN.md 8.1; `atomic-apply-ops`);
//! - `set_tile` refuses a terrain, feature or natural wonder of the wrong kind, where Python
//!   stored any terrain name in any slot (`scenario-set-tile-checks-kinds`);
//! - numbers must be finite, and a whole number must fit its field (a resource amount fits a
//!   byte), where Python stored anything (`scenario-numbers-finite-and-in-range`);
//! - `set_relation` refuses a war with a friendship, a defensive pact or open borders in force,
//!   which Python set as asked and no rule of the game can reach (invariant DIPLO-1;
//!   `scenario-war-refuses-standing-treaties`);
//! - `set_relation`'s opinion is the holder's own, within the ±100 every reason keeps, where
//!   Python stored it where nothing read it (`scenario-opinion-counts`, in
//!   `game::diplomacy::relations`);
//! - `set_influence` takes only a major civilization as the one whose influence is set, where
//!   Python took a city-state too (`scenario-influence-majors-only`);
//! - `grant_tech` reads a `techs` string as one name, where Python took each of its letters
//!   (`scenario-techs-string-is-one-name`);
//! - a granted tech is announced "Rome was granted Pottery." where Python wrote "Rome scenario
//!   Pottery." (`scenario-tech-announcement-wording`, in `game::research`);
//! - a parameter of the wrong type is refused with a sentence, where Python quoted its own
//!   exception ("bad parameters (ValueError: ...)"; `scenario-errors-are-sentences`).

use serde_json::{Map, Value, json};

use crate::base::ids::{
    DifficultyId, EraId, ImprovementId, PlayerId, ResourceId, TechId, TerrainId, TileIdx,
};
use crate::base::py;
use crate::base::sets::{BitSet, FeatureSet};
use crate::game::city_states::influence::{add_influence, raw_influence};
use crate::game::derive::rev::PlayerTouch;
use crate::game::diplomacy::relations::{WarReason, make_peace, set_opinion, set_war};
use crate::game::error::{ActionError, ErrCode};
use crate::game::research::{self, TechSource};
use crate::game::{Game, Porting};
use crate::rules::Named;
use crate::rules::defs::{Route, TerrainType};
use crate::state::diplo::{OpinionKey, side};
use crate::state::{StateError, TileClaim};

/// An operation's parameters, as the editor sent them.
pub type Params = Map<String, Value>;

/// What an operation does: it edits the game and returns what the editor is told.
type Run = fn(&mut Game, &Params) -> Result<Value, ActionError>;

/// One scenario operation.
#[derive(Clone, Copy, Debug)]
pub struct OpSpec {
    /// Its name: `grant_tech`.
    pub name: &'static str,
    /// Its parameters, as the editor shows them.
    pub params: &'static str,
    /// Whether its rules are ported yet.
    pub porting: Porting,
    run: Run,
}

/// Every scenario operation, sorted by name (`scenario.OPS`).
pub static OPS: &[OpSpec] = &[
    OpSpec {
        name: "add_unit",
        params: "player, unit, x, y; optional count, promotions: [...], xp, hp",
        porting: Porting::Pending("1c-02"),
        run: add_unit,
    },
    OpSpec {
        name: "adopt_policy",
        params: "player, policy (or policies: [...]); branches open automatically",
        porting: Porting::Pending("1b-07"),
        run: adopt_policy,
    },
    OpSpec {
        name: "found_city",
        params: "player, x, y; optional name, pop, buildings: [...], capital (bool)",
        porting: Porting::Pending("1b-07"),
        run: found_city,
    },
    OpSpec {
        name: "grant_era",
        params: "player (id or 'all'), era. Grants every tech of earlier eras, so the civ is in \
                 that era (like UnCiv's starting era). include=true also grants that era's own \
                 techs (which moves it to the next era).",
        porting: Porting::Ported,
        run: grant_era,
    },
    OpSpec {
        name: "grant_tech",
        params: "player (id or 'all'), tech (or techs: [...]). Also grants missing prerequisites.",
        porting: Porting::Ported,
        run: grant_tech,
    },
    OpSpec {
        name: "meet",
        params: "a, b (player ids; b may be 'all'): the civilizations know each other",
        porting: Porting::Ported,
        run: meet,
    },
    OpSpec {
        name: "remove_city",
        params: "city (id) or x, y",
        porting: Porting::Pending("1b-07"),
        run: remove_city,
    },
    OpSpec {
        name: "remove_tech",
        params: "player, tech. Removes a tech (and every tech that needs it).",
        porting: Porting::Ported,
        run: remove_tech,
    },
    OpSpec {
        name: "remove_units",
        params: "x, y (every unit there), or unit (one id); optional player (only theirs)",
        porting: Porting::Pending("1c-02"),
        run: remove_units,
    },
    OpSpec {
        name: "reveal",
        params: "player (id or 'all'): explore the whole map; meet (bool) also meets every \
                 civilization",
        porting: Porting::Ported,
        run: reveal,
    },
    OpSpec {
        name: "set_city",
        params: "city (id) or x, y; any of pop, add_buildings, remove_buildings, name, \
                 claim_radius (border radius), production",
        porting: Porting::Pending("1b-07"),
        run: set_city,
    },
    OpSpec {
        name: "set_influence",
        params: "city_state, player, amount: the civ's influence with a city-state",
        porting: Porting::Ported,
        run: set_influence,
    },
    OpSpec {
        name: "set_player",
        params: "player (id or 'all'); any of gold, faith, culture, golden_age_turns, name, \
                 difficulty, notes, free_policies, free_techs",
        porting: Porting::Ported,
        run: set_player,
    },
    OpSpec {
        name: "set_relation",
        params: "a, b; any of state ('war' | 'peace'), embassies (bool), friends (bool), \
                 defensive_pact (bool), open_borders (bool), turns (length of agreements, default \
                 30), opinion (number: a's opinion of b)",
        porting: Porting::Ported,
        run: set_relation,
    },
    OpSpec {
        name: "set_research",
        params: "player, tech: what the civ is researching",
        porting: Porting::Ported,
        run: set_research,
    },
    OpSpec {
        name: "set_tile",
        params: "x, y; any of terrain, features: [...], resource (null to clear), amount, \
                 improvement, route, wonder, river (bitmask), owner (player id or null, for \
                 unclaimed tiles)",
        porting: Porting::Ported,
        run: set_tile,
    },
];

/// The operation called `name`, exactly.
#[must_use]
pub fn op(name: &str) -> Option<&'static OpSpec> {
    OPS.binary_search_by(|o| o.name.cmp(name)).ok().map(|i| &OPS[i])
}

/// Every operation with its parameters, sorted by name: the reference the editor shows
/// (`scenario.ops_help`).
#[must_use]
pub fn ops_help() -> Value {
    Value::Array(OPS.iter().map(|o| json!({"op": o.name, "params": o.params})).collect())
}

/// Applies operations in order and returns what each said; the first that fails stops the
/// list with an error naming it, "Operation 3 (set_tile): ...". What the earlier ones did is left
/// in place: [`Game::apply_ops`](crate::game::Game::apply_ops), the host's call, puts the game
/// back.
pub(crate) fn apply(g: &mut Game, ops: &Value) -> Result<Vec<Value>, ActionError> {
    let list: &[Value] = match ops {
        Value::Array(a) => a,
        v if !py::truthy(v) => &[],
        _ => return Err(bad("The operations must be a list of objects.")),
    };
    let mut out = Vec::with_capacity(list.len());
    for (i, o) in list.iter().enumerate() {
        let n = i + 1;
        let spec = o.get("op").and_then(Value::as_str).and_then(op);
        let (Value::Object(params), Some(spec)) = (o, spec) else {
            // Python quoted either with `!r`: the op's name, or the whole entry when it is no table.
            let what = py::repr(match o {
                Value::Object(m) => m.get("op").unwrap_or(&Value::Null),
                other => other,
            });
            let known: Vec<&str> = OPS.iter().map(|o| o.name).collect();
            return Err(bad(format!(
                "Operation {n}: unknown op {what}. Known: {}.",
                known.join(", ")
            )));
        };
        let done = (spec.run)(g, params).map_err(|e| {
            ActionError::new(e.code, format!("Operation {n} ({}): {}", spec.name, e.message))
        })?;
        out.push(done);
    }
    Ok(out)
}

// ---- Parameters (scenario.py:36-85) ---------------------------------------------------------

/// A refusal of a parameter.
fn bad(message: impl Into<String>) -> ActionError {
    ActionError::new(ErrCode::BadParam, message)
}

/// A state write refused: a bug, since the operations check what they write.
fn refused(e: &StateError) -> ActionError {
    ActionError::rule(format!("The game refused the edit ({e})."))
}

/// A player named by id (`_pid`, `scenario.py:36-46`): any player but the barbarians, or only a
/// major civilization.
pub(crate) fn pid(g: &Game, v: Option<&Value>, majors_only: bool) -> Result<PlayerId, ActionError> {
    let raw = v.unwrap_or(&Value::Null);
    let n =
        py::int_of(raw).ok_or_else(|| bad(format!("'{}' is not a player id.", py::str_of(raw))))?;
    let found = u8::try_from(n).ok().map(PlayerId).filter(|&p| g.player(p).is_some());
    let Some(p) = found.filter(|&p| !g.is_barbarian(p)) else {
        return Err(bad(format!("No player {n}.")));
    };
    if majors_only && !g.player(p).is_some_and(crate::state::players::Player::is_major) {
        return Err(bad(format!("Player {n} is not a major civilization.")));
    }
    Ok(p)
}

/// Players named by id, a list of ids, or `all` (`_players`, `scenario.py:49-55`): `all` (or
/// nothing, or `*`) is every living major civilization, or with `majors_only` false every player
/// but the barbarians, the eliminated included.
pub(crate) fn players(
    g: &Game,
    v: Option<&Value>,
    majors_only: bool,
) -> Result<Vec<PlayerId>, ActionError> {
    match v {
        None | Some(Value::Null) => Ok(everyone(g, majors_only)),
        Some(Value::String(s)) if s == "all" || s == "*" => Ok(everyone(g, majors_only)),
        Some(Value::Array(list)) => list.iter().map(|x| pid(g, Some(x), majors_only)).collect(),
        Some(one) => Ok(vec![pid(g, Some(one), majors_only)?]),
    }
}

fn everyone(g: &Game, majors_only: bool) -> Vec<PlayerId> {
    g.state()
        .players()
        .iter()
        .filter(|(_, p)| if majors_only { p.is_major() && p.alive() } else { !p.is_barbarian() })
        .map(|(id, _)| id)
        .collect()
}

/// The tile an operation names by `x` and `y` (`_idx`, `scenario.py:58-66`).
pub(crate) fn tile(g: &Game, o: &Params) -> Result<TileIdx, ActionError> {
    let coord = |k: &str| o.get(k).and_then(py::int_of);
    let (Some(x), Some(y)) = (coord("x"), coord("y")) else {
        return Err(bad("This operation needs integer x and y."));
    };
    let at = i32::try_from(x).ok().zip(i32::try_from(y).ok()).and_then(|(x, y)| g.grid().idx(x, y));
    at.ok_or_else(|| ActionError::new(ErrCode::OffMap, format!("({x}, {y}) is off the map.")))
}

/// A ruleset object named loosely: "warrior" is the Warrior (`_name`, `scenario.py:80-85`).
pub(crate) fn resolve<I: Named>(g: &Game, v: Option<&Value>) -> Result<I, ActionError> {
    let raw = v.unwrap_or(&Value::Null);
    let found = match raw {
        Value::Null => None,
        Value::String(s) => g.rules().resolve::<I>(s),
        other => g.rules().resolve::<I>(&py::str_of(other)),
    };
    found.ok_or_else(|| bad(format!("Unknown {} '{}'.", I::KIND.as_str(), py::str_of(raw))))
}

/// A number parameter (Python's `float()`), which must be finite.
fn number(v: &Value, key: &str) -> Result<f64, ActionError> {
    // refcheck: scenario-numbers-finite-and-in-range
    // refcheck: scenario-errors-are-sentences
    py::float_of(v)
        .filter(|x| x.is_finite())
        .ok_or_else(|| bad(format!("{key} must be a finite number, not {}.", py::repr(v))))
}

/// A whole-number parameter (Python's `int()`) that must fit `T`.
fn whole<T: TryFrom<i64>>(v: &Value, key: &str) -> Result<T, ActionError> {
    // refcheck: scenario-numbers-finite-and-in-range
    // refcheck: scenario-errors-are-sentences
    py::int_of(v)
        .and_then(|n| T::try_from(n).ok())
        .ok_or_else(|| bad(format!("{key} must be a whole number in range, not {}.", py::repr(v))))
}

/// The value of `key`, if present and not `null`.
fn given<'a>(o: &'a Params, key: &str) -> Option<&'a Value> {
    o.get(key).filter(|v| !v.is_null())
}

/// Python's `o.get(key) or default`: the value if it is true, else `default`.
fn or<'a>(o: &'a Params, key: &str, default: &'a Value) -> &'a Value {
    o.get(key).filter(|v| py::truthy(v)).unwrap_or(default)
}

/// A map from player ids, as Python's dicts keyed by player became in JSON.
fn by_player(entries: impl IntoIterator<Item = (PlayerId, Value)>) -> Value {
    Value::Object(entries.into_iter().map(|(p, v)| (p.0.to_string(), v)).collect())
}

// ---- Research (scenario.py:108-174, 463-468) --------------------------------------------------

/// Grants every tech of the eras before `era`, and with `include` that era's own
/// (`scenario.py:108-126`).
fn grant_era(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let era: EraId = resolve(g, o.get("era"))?;
    let include = o.get("include").is_some_and(py::truthy);
    let r = g.rules();
    let mut out = Vec::new();
    for p in players(g, o.get("player"), false)? {
        let mut added = 0u32;
        for &t in &r.derived().tech_order {
            let te = r.techs()[t].era;
            if (te < era || (include && te == era))
                && !g.has_tech(p, Some(t))
                && !research::is_repeatable(r, t)
            {
                research::add_tech(g, p, t, TechSource::Scenario);
                added += 1;
            }
        }
        out.push((p, Value::from(added)));
    }
    Ok(json!({"techs_added": by_player(out)}))
}

/// Grants techs with every prerequisite they are missing (`scenario.py:129-152`).
fn grant_tech(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    // refcheck: scenario-techs-string-is-one-name (Python iterated a string's letters)
    let names: Vec<&Value> = match o.get("techs").filter(|v| py::truthy(v)) {
        Some(Value::Array(list)) => list.iter().collect(),
        Some(one) => vec![one],
        None => vec![o.get("tech").unwrap_or(&Value::Null)],
    };
    let mut out = Vec::new();
    for p in players(g, o.get("player"), false)? {
        let mut added = Vec::new();
        for &n in &names {
            let t: TechId = resolve(g, Some(n))?;
            need(g, p, t, &mut added);
        }
        let r = g.rules();
        let names: Vec<&str> = added.iter().filter_map(|&t| r.name(t)).collect();
        out.push((p, json!(names)));
    }
    Ok(json!({"techs_added": by_player(out)}))
}

/// Grants `t` after everything it depends on, depth first in prerequisite order.
fn need(g: &mut Game, p: PlayerId, t: TechId, added: &mut Vec<TechId>) {
    if g.has_tech(p, Some(t)) || added.contains(&t) {
        return;
    }
    let pre: Vec<TechId> = g.rules().techs()[t].prerequisites.to_vec();
    for x in pre {
        need(g, p, x, added);
    }
    research::add_tech(g, p, t, TechSource::Scenario);
    added.push(t);
}

/// Removes a tech and every tech that needs it (`scenario.py:155-174`).
fn remove_tech(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let tech: TechId = resolve(g, o.get("tech"))?;
    let mut out = Vec::new();
    for p in players(g, o.get("player"), false)? {
        let dropped = research::remove_tech(g, p, tech);
        let r = g.rules();
        let names: Vec<&str> = dropped.iter().filter_map(|&t| r.name(t)).collect();
        out.push((p, json!(names)));
    }
    Ok(json!({"techs_removed": by_player(out)}))
}

/// Sets what a civilization researches (`scenario.py:463-468`); a city-state too, as Python's
/// `_pid` let it. The result is read before `apply_ops` settles, as Python's was.
fn set_research(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let p = pid(g, o.get("player"), false)?;
    let tech: TechId = resolve(g, o.get("tech"))?;
    let path = research::plan_research(g, p, tech, false)?;
    research::apply_research(g, p, &path);
    Ok(research::research_result(g, p, &path))
}

// ---- Players (scenario.py:177-196) ------------------------------------------------------------

/// Sets a civilization's stocks, counters, name, difficulty and notes (`scenario.py:177-196`).
fn set_player(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    for p in players(g, o.get("player"), false)? {
        for key in ["gold", "faith", "culture"] {
            if let Some(v) = given(o, key) {
                let x = number(v, key)?;
                if let Some(pl) = g.player_mut(p, PlayerTouch::STOCKS) {
                    match key {
                        "gold" => pl.econ.gold = x,
                        "faith" => pl.econ.faith = x,
                        _ => pl.econ.culture = x,
                    }
                }
            }
        }
        if let Some(v) = given(o, "golden_age_turns") {
            let n = whole(v, "golden_age_turns")?;
            if let Some(pl) = g.player_mut(p, PlayerTouch::STOCKS) {
                pl.econ.golden_age_turns = n;
            }
        }
        for key in ["free_policies", "free_techs"] {
            if let Some(v) = given(o, key) {
                let n = whole(v, key)?;
                if let Some(pl) = g.player_mut(p, PlayerTouch::OTHER) {
                    if key == "free_policies" {
                        pl.policy.free_policies = n;
                    } else {
                        pl.tech.free_techs = n;
                    }
                }
            }
        }
        if let Some(v) = o.get("name").filter(|v| py::truthy(v)) {
            let name: String = py::str_of(v).chars().take(40).collect();
            if let Some(pl) = g.player_mut(p, PlayerTouch::NAME) {
                pl.name = name.into();
            }
        }
        if let Some(v) = o.get("difficulty").filter(|v| py::truthy(v)) {
            let d: DifficultyId = resolve(g, Some(v))?;
            g.set_seat_difficulty(p, Some(d)).map_err(|e| refused(&e))?;
        }
        if let Some(v) = given(o, "notes") {
            let notes = py::str_of(v);
            if let Some(major) =
                g.player_mut(p, PlayerTouch::OTHER).and_then(|pl| pl.major.as_deref_mut())
            {
                major.notes = notes.into();
            }
        }
    }
    Ok(json!({}))
}

// ---- Tiles (scenario.py:353-381) --------------------------------------------------------------

/// Edits a tile: its terrain, features, resource, improvement, route, natural wonder, river and
/// owner (`scenario.py:353-381`).
fn set_tile(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let t = tile(g, o)?;
    let r = g.rules();
    let before = *g.tile(t).ok_or_else(|| bad("No such tile."))?;
    let kind = |id: TerrainId| r.terrains()[id].kind;
    // refcheck: scenario-set-tile-checks-kinds (the terrain, the features and the wonder)
    if let Some(v) = o.get("terrain") {
        let id: TerrainId = resolve(g, Some(v))?;
        if !matches!(kind(id), TerrainType::Land | TerrainType::Water) {
            return Err(bad(format!("{} is not a base terrain.", r.terrains()[id].name)));
        }
        g.set_terrain(t, id).map_err(|e| refused(&e))?;
    }
    if let Some(v) = o.get("features") {
        let list: &[Value] = match v {
            Value::Array(a) => a,
            v if !py::truthy(v) => &[],
            _ => return Err(bad("features must be a list of terrain features.")),
        };
        let mut set = FeatureSet::EMPTY;
        for f in list {
            let id: TerrainId = resolve(g, Some(f))?;
            let Some(feature) = r.terrains()[id].feature else {
                return Err(bad(format!("{} is not a terrain feature.", r.terrains()[id].name)));
            };
            set.insert(feature);
        }
        g.set_features(t, set).map_err(|e| refused(&e))?;
    }
    let zero = Value::from(0);
    if let Some(v) = o.get("resource") {
        let resource: Option<ResourceId> =
            if py::truthy(v) { Some(resolve(g, Some(v))?) } else { None };
        let amount = whole(or(o, "amount", &zero), "amount")?;
        g.set_resource(t, resource, amount).map_err(|e| refused(&e))?;
    } else if o.contains_key("amount") {
        let amount = whole(or(o, "amount", &zero), "amount")?;
        g.set_resource(t, before.resource(), amount).map_err(|e| refused(&e))?;
    }
    if let Some(v) = o.get("improvement") {
        let improvement: Option<ImprovementId> =
            if py::truthy(v) { Some(resolve(g, Some(v))?) } else { None };
        g.set_improvement(t, improvement).map_err(|e| refused(&e))?;
        let route_pillaged = g.tile(t).is_some_and(crate::state::map::Tile::route_pillaged);
        g.set_pillaged(t, route_pillaged, false).map_err(|e| refused(&e))?;
    }
    if let Some(v) = o.get("route") {
        let route = match v.as_str() {
            Some("Road") => Some(Route::Road),
            Some("Railroad") => Some(Route::Railroad),
            _ => None,
        };
        g.set_route(t, route).map_err(|e| refused(&e))?;
    }
    if let Some(v) = o.get("wonder") {
        let wonder: Option<TerrainId> =
            if py::truthy(v) { Some(resolve(g, Some(v))?) } else { None };
        if let Some(w) = wonder
            && kind(w) != TerrainType::NaturalWonder
        {
            return Err(bad(format!("{} is not a natural wonder.", r.terrains()[w].name)));
        }
        g.set_wonder(t, wonder).map_err(|e| refused(&e))?;
    }
    if o.contains_key("river") {
        let mask: i64 = whole(or(o, "river", &zero), "river")?;
        // `& 63` keeps the six edges, of a negative number too, as Python's did.
        let mask = u8::try_from(mask & 63).unwrap_or(0);
        g.set_river(t, mask).map_err(|e| refused(&e))?;
    }
    if let Some(v) = o.get("owner")
        && g.tile(t).is_some_and(|x| x.city().is_none())
    {
        let owner = if v.is_null() { None } else { Some(pid(g, Some(v), false)?) };
        g.set_tile_owner(t, TileClaim { owner, city: None }).map_err(|e| refused(&e))?;
    }
    Ok(json!({}))
}

// ---- Relations (scenario.py:384-445) ----------------------------------------------------------

/// Makes civilizations known to each other (`scenario.py:384-391`).
fn meet(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let a = pid(g, o.get("a"), false)?;
    for b in players(g, o.get("b"), false)? {
        if b != a {
            g.make_contact(a, b);
        }
    }
    Ok(json!({}))
}

/// Sets war or peace, embassies, friendship, a pact, open borders and an opinion between two
/// players, who meet first (`scenario.py:394-431`).
fn set_relation(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let a = pid(g, o.get("a"), false)?;
    let b = pid(g, o.get("b"), false)?;
    if a == b {
        return Err(bad("A relation needs two different players."));
    }
    let thirty = Value::from(30);
    let turns: i32 = whole(or(o, "turns", &thirty), "turns")?;
    g.make_contact(a, b);
    match o.get("state").and_then(Value::as_str) {
        Some("war") if !g.at_war(a, b) => {
            set_war(g, a, b, WarReason::Scenario).map_err(|e| refused(&e))?;
        }
        Some("peace") if g.relation(a, b).is_some_and(|r| r.war) => {
            make_peace(g, a, b).map_err(|e| refused(&e))?;
            g.update_relation(a, b, |r| r.treaty_until = 0).map_err(|e| refused(&e))?;
        }
        _ => {}
    }
    let until = g.turn().saturating_add(turns);
    let flag = |k: &str| given(o, k).map(py::truthy);
    let (embassies, friends, pact, open) =
        (flag("embassies"), flag("friends"), flag("defensive_pact"), flag("open_borders"));
    g.update_relation(a, b, |r| {
        if let Some(on) = embassies {
            r.embassy = [on, on];
        }
        if let Some(on) = friends {
            r.friendship_until = if on { until } else { 0 };
        }
        if let Some(on) = pact {
            r.pact_until = if on { until } else { 0 };
        }
        if let Some(on) = open {
            let v = if on { until } else { 0 };
            r.open_borders_until[side(a, b)] = v;
            r.open_borders_until[side(b, a)] = v;
        }
    })
    .map_err(|e| refused(&e))?;
    let turn = g.turn();
    // refcheck: scenario-war-refuses-standing-treaties
    if g.relation(a, b).is_some_and(|r| {
        r.war
            && (r.friendship_until >= turn
                || r.pact_until >= turn
                || r.open_borders_until.iter().any(|&u| u >= turn))
    }) {
        return Err(bad(
            "Two players at war can have no friendship, defensive pact or open borders.",
        ));
    }
    if let Some(v) = given(o, "opinion") {
        let x = number(v, "opinion")?;
        // refcheck: scenario-opinion-counts (within the ±100 every reason keeps)
        set_opinion(g, a, b, OpinionKey::Scenario, x);
    }
    Ok(json!({"war": g.relation(a, b).is_some_and(|r| r.war)}))
}

/// Sets a major's influence with a city-state, which meet first (`scenario.py:434-445`). Python
/// took any player but the barbarians as the major, a city-state included.
fn set_influence(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let cs = pid(g, o.get("city_state"), false)?;
    if !g.is_city_state(cs) {
        return Err(bad(format!("Player {} is not a city-state.", cs.0)));
    }
    // refcheck: scenario-influence-majors-only
    let p = pid(g, o.get("player"), true)?;
    g.make_contact(cs, p);
    let current = raw_influence(g, cs, p);
    let zero = Value::from(0);
    let amount = number(or(o, "amount", &zero), "amount")?;
    // Python added the difference to the stored value, which rounds as it does here.
    add_influence(g, cs, p, amount - current).map_err(|e| refused(&e))?;
    Ok(json!({"influence": raw_influence(g, cs, p)}))
}

// ---- Sight (scenario.py:448-460) --------------------------------------------------------------

/// Explores the whole map for civilizations, and with `meet` has them meet everyone
/// (`scenario.py:448-460`).
fn reveal(g: &mut Game, o: &Params) -> Result<Value, ActionError> {
    let meet_all = o.get("meet").is_some_and(py::truthy);
    let size = g.grid().size();
    for p in players(g, o.get("player"), false)? {
        let mut all = BitSet::with_capacity(size);
        for i in 0..size {
            all.insert(i);
        }
        if let Some(pl) = g.player_mut(p, PlayerTouch::OTHER) {
            pl.explored = all;
        }
        if meet_all {
            for q in everyone(g, false) {
                if q != p {
                    g.make_contact(p, q);
                }
            }
        }
    }
    Ok(json!({}))
}

// ---- Operations whose systems are not ported yet ---------------------------------------------

/// The refusal of an operation whose system is not ported yet: `path` names the system, and
/// `cargo xtask check` counts the calls (DESIGN.md 3.4, rule 4).
fn not_ported(path: &str) -> ActionError {
    ActionError::new(
        ErrCode::NotPorted,
        format!("This operation is not ported to the new engine yet ({path})."),
    )
}

fn found_city(_: &mut Game, _: &Params) -> Result<Value, ActionError> {
    Err(not_ported("game::cities::founding"))
}

fn set_city(_: &mut Game, _: &Params) -> Result<Value, ActionError> {
    Err(not_ported("game::cities::lifecycle"))
}

fn remove_city(_: &mut Game, _: &Params) -> Result<Value, ActionError> {
    Err(not_ported("game::cities::lifecycle"))
}

fn adopt_policy(_: &mut Game, _: &Params) -> Result<Value, ActionError> {
    Err(not_ported("game::policies"))
}

fn add_unit(_: &mut Game, _: &Params) -> Result<Value, ActionError> {
    Err(not_ported("game::units"))
}

fn remove_units(_: &mut Game, _: &Params) -> Result<Value, ActionError> {
    Err(not_ported("game::units"))
}
