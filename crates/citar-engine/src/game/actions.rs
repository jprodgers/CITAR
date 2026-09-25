//! Special unit actions, listed and carried out by id (`actions.py`, UnCiv's `UnitActions` and
//! `UnitActionsFromUniques`), and the tools that reach them: `unit_action` and `found_city`
//! (`tools.py:485-525`).
//!
//! [`unit_actions`] lists what a unit can do now: founding a city, founding and enhancing a
//! religion, spreading religion and removing heresy, a great person's hurry, trade mission or
//! treatise, an instant improvement (a great person's or a work boat's), a paradrop, a spaceship
//! part, and the one-time effects a unit carries as actions (a great artist's golden age), each
//! with Python's id (`trigger:<n>` numbering the unit's uniques as Python's unit map listed them:
//! its own, its type's, then its promotions'). Movement, combat, worker builds and simple orders
//! have their own tools. Every action checks on `&Game` before anything is written; the rules they
//! run are their systems' (`cities::founding`, `religion::{found, spread}`, `great_people`,
//! `workers`, `triggers`), and a unit spent by one fires `upon expending a [unit]` once
//! (`expending-a-unit-fires-once`: Python fired it again after a great person's action).
//!
//! A one-time effect a unit carries is tried on a copy of the game before it is taken: one that
//! would do nothing is refused, as Python refused it, and the pipeline refuses before it writes.
//! Adding a spaceship part is refused as not ported until package 1c-08 ports victory. The results
//! of founding a city and of a paradrop give their tiles as `{x, y}`, where Python listed the keys
//! of the coordinates (`unit-results-give-tiles`).

use serde_json::{Map, Value, json};

use super::Game;
use super::action::{OutcomeSpec, Rule};
use super::cities::construction::{buildable_items, item_name};
use super::cities::founding::{found_check, found_city_by};
use super::cities::stats::current_construction;
use super::derive::rev::{PlayerTouch, UnitTouch};
use super::error::{ActionError, ErrCode};
use super::religion::{found, spread};
use super::units::abilities::{consume_action, usable_action};
use super::units::actions::{own_unit, tile_at};
use super::units::type_has;
use super::workers::{self, InstantOption};
use super::{great_people, policies, research, triggers};
use crate::base::fmt::PyFloat;
use crate::base::ids::{CityId, ImprovementId, PlayerId, TileIdx, UniqueId, UnitId};
use crate::base::py;
use crate::rules::defs::ReligionProgress;
use crate::state::cities::Constructible;
use crate::state::map::Tile;
use crate::state::units::Activity;
use crate::unique::table::Role;
use crate::unique::trigger::TriggerSite;
use crate::unique::{UniqueData, UniqueType};

/// The action types the list reads itself, which `trigger:<n>` leaves to it
/// (`actions.HANDLED`).
const HANDLED: [UniqueType; 14] = [
    UniqueType::FoundCity,
    UniqueType::MayFoundReligion,
    UniqueType::MayEnhanceReligion,
    UniqueType::CanSpreadReligion,
    UniqueType::CanRemoveHeresy,
    UniqueType::CanHurryResearch,
    UniqueType::CanSpeedupConstruction,
    UniqueType::CanSpeedupWonderConstruction,
    UniqueType::CanHurryPolicy,
    UniqueType::CanTradeWithCityStateForGoldAndInfluence,
    UniqueType::ConstructImprovementInstantly,
    UniqueType::CreateWaterImprovements,
    UniqueType::AddInCapital,
    UniqueType::MayParadrop,
];

/// What an action does, with what the list found.
#[derive(Clone, Debug, PartialEq)]
pub enum ActionKind {
    FoundCity,
    FoundReligion(UniqueId),
    EnhanceReligion(UniqueId),
    SpreadReligion,
    RemoveHeresy,
    HurryResearch,
    HurryConstruction,
    TradeMission,
    PoliticalTreatise(UniqueId),
    Create(InstantOption),
    Paradrop(UniqueId),
    AddToSpaceship,
    Trigger(UniqueId),
}

/// One action a unit could take (`actions._action`, `actions.py:24-32`).
#[derive(Clone, Debug, PartialEq)]
pub struct UnitActionEntry {
    /// What the `unit_action` tool calls it: `found_city`, `create:Academy`, `trigger:0`.
    pub id: String,
    pub name: String,
    /// Why it cannot be taken now; `None` when it can.
    pub reason: Option<String>,
    /// The tool's parameters it reads, and what each is.
    pub params: &'static [(&'static str, &'static str)],
    pub kind: ActionKind,
}

impl UnitActionEntry {
    /// Whether it can be taken now.
    #[must_use]
    pub const fn available(&self) -> bool {
        self.reason.is_none()
    }

    /// As Python's dict: `id`, `name`, `available`, and `reason` and `params` where there are.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), json!(self.id));
        m.insert("name".into(), json!(self.name));
        m.insert("available".into(), json!(self.available()));
        if let Some(r) = &self.reason {
            m.insert("reason".into(), json!(r));
        }
        if !self.params.is_empty() {
            let p: Map<String, Value> =
                self.params.iter().map(|&(k, v)| (k.to_owned(), json!(v))).collect();
            m.insert("params".into(), Value::Object(p));
        }
        Value::Object(m)
    }
}

/// Whether a unique happens once rather than standing (`triggers.is_triggerable`,
/// `triggers.py:28-30`): a one-time effect, a timed unique, or an effect that also happens when
/// its source is gained.
fn triggerable(g: &Game, id: UniqueId) -> bool {
    let meta = g.rules().uniques().meta(id);
    meta.role == Role::OneTime
        || meta.timed.is_some()
        || meta.ty.and_then(|t| t.info().support).is_some_and(|s| s.gain)
}

/// A unit's uniques in the order Python's unit map listed them (`units.unit_umap`,
/// `units.py:21-33`): its base unit's own, its type's, then each promotion's.
fn unit_map(g: &Game, u: UnitId) -> Vec<UniqueId> {
    let Some(x) = g.unit(u) else { return Vec::new() };
    let r = g.rules();
    let mut out: Vec<UniqueId> = super::units::type_uniques(g, x.base).collect();
    for pr in x.promotions.iter() {
        out.extend(r.promotions()[pr].uniques.ids());
    }
    out
}

/// Every action unit `u` could take now, with the ids to call them by (`actions.unit_actions`,
/// `actions.py:44-120`): what `get_unit` shows, and what the `unit_action` tool looks up.
#[must_use]
#[allow(clippy::too_many_lines, reason = "one entry per action kind, in Python's order")]
pub fn unit_actions(g: &Game, u: UnitId) -> Vec<UnitActionEntry> {
    let mut out = Vec::new();
    let Some(x) = g.unit(u) else { return out };
    let r = g.rules();
    let p = x.owner();
    let at = x.tile();
    let no_moves = (x.moves <= 0).then(|| "No movement left this turn.".to_owned());
    let entry = |id: String, name: String, reason: Option<String>, kind: ActionKind| {
        UnitActionEntry { id, name, reason, params: &[], kind }
    };
    if usable_action(g, u, UniqueType::FoundCity).is_some() {
        // A city-state keeps to one city, whatever settler it holds (its starting one, one given
        // or captured): it trains none either (`NoSettlerForOneCityPlayers`).
        // refcheck: city-states-found-one-city
        let one_city = (g.is_city_state(p) && g.player_cities(p).next().is_some())
            .then(|| "A city-state cannot found more cities.".to_owned());
        out.push(UnitActionEntry {
            params: &[("name", "optional city name")],
            ..entry(
                "found_city".into(),
                "Found a city here".into(),
                no_moves.clone().or(one_city).or_else(|| found_check(g, p, at)),
                ActionKind::FoundCity,
            )
        });
    }
    if g.religion_enabled() {
        if let Some(id) = usable_action(g, u, UniqueType::MayFoundReligion) {
            let mut reason = found::can_found_religion(g, p);
            if reason.is_none() && g.city_at(at).is_none_or(|c| c.owner() != p) {
                reason = Some("Move the Great Prophet into one of your cities.".into());
            }
            out.push(UnitActionEntry {
                params: &[
                    ("name", "religion name"),
                    ("beliefs", "list of belief names (see get_religion)"),
                ],
                ..entry(
                    "found_religion".into(),
                    "Found a religion".into(),
                    no_moves.clone().or(reason),
                    ActionKind::FoundReligion(id),
                )
            });
        }
        if let Some(id) = usable_action(g, u, UniqueType::MayEnhanceReligion) {
            let founded =
                g.player(p).is_some_and(|x| x.religion.progress == ReligionProgress::Religion);
            let mut reason = (!founded)
                .then(|| "You must have founded a (not yet enhanced) religion.".to_owned());
            if reason.is_none() && g.city_at(at).is_none() {
                reason =
                    Some("Move the Great Prophet into a city to enhance your religion.".into());
            }
            out.push(UnitActionEntry {
                params: &[("beliefs", "list of belief names (see get_religion)")],
                ..entry(
                    "enhance_religion".into(),
                    "Enhance your religion".into(),
                    no_moves.clone().or(reason),
                    ActionKind::EnhanceReligion(id),
                )
            });
        }
        if usable_action(g, u, UniqueType::CanSpreadReligion).is_some() {
            let mut reason =
                g.tile(at).and_then(Tile::city).is_none().then(|| {
                    "Move next to or into a city's territory to spread religion.".to_owned()
                });
            if x.religion.is_none() {
                reason = Some("This unit carries no religion.".into());
            }
            let name = x.religion.map_or_else(
                || "Spread religion".to_owned(),
                |rel| format!("Spread {}", super::religion::display_name(g, rel)),
            );
            out.push(entry(
                "spread_religion".into(),
                name,
                no_moves.clone().or(reason),
                ActionKind::SpreadReligion,
            ));
        }
        if usable_action(g, u, UniqueType::CanRemoveHeresy).is_some() {
            out.push(entry(
                "remove_heresy".into(),
                "Remove other religions from this city".into(),
                no_moves.clone(),
                ActionKind::RemoveHeresy,
            ));
        }
    }
    if usable_action(g, u, UniqueType::CanHurryResearch).is_some() {
        let science = research::science_from_great_scientist(g, p);
        let reason = research::current(g, p).is_none().then(|| "Choose a technology first.".into());
        out.push(entry(
            "hurry_research".into(),
            format!("Hurry research (+{science} science)"),
            no_moves.clone().or(reason),
            ActionKind::HurryResearch,
        ));
    }
    if usable_action(g, u, UniqueType::CanSpeedupConstruction).is_some()
        || usable_action(g, u, UniqueType::CanSpeedupWonderConstruction).is_some()
    {
        let ours = g.city_at(at).is_some_and(|c| c.owner() == p);
        out.push(entry(
            "hurry_construction".into(),
            "Hurry the building or wonder in production here".into(),
            no_moves.clone().or_else(|| (!ours).then(|| "Move into one of your cities.".into())),
            ActionKind::HurryConstruction,
        ));
    }
    if usable_action(g, u, UniqueType::CanTradeWithCityStateForGoldAndInfluence).is_some() {
        let ok =
            g.tile(at).and_then(Tile::owner).is_some_and(|o| g.is_city_state(o) && !g.at_war(o, p));
        let reason = (!ok)
            .then(|| "Move into the territory of a city-state you are at peace with.".to_owned());
        out.push(entry(
            "trade_mission".into(),
            "Trade mission (gold and influence)".into(),
            no_moves.clone().or(reason),
            ActionKind::TradeMission,
        ));
    }
    if let Some(id) = usable_action(g, u, UniqueType::CanHurryPolicy) {
        out.push(entry(
            "political_treatise".into(),
            "Generate a large amount of culture".into(),
            no_moves.clone(),
            ActionKind::PoliticalTreatise(id),
        ));
    }
    let mut instants = workers::great_options(g, u);
    instants.extend(workers::water_options(g, u));
    for o in instants {
        let name = r.improvements()[o.imp].name.to_string();
        let blocked = (!o.blocked.is_empty())
            .then(|| o.blocked.iter().map(|b| b.text(g, o.imp)).collect::<Vec<_>>().join("; "));
        out.push(entry(
            format!("create:{name}"),
            format!("Create {name} here"),
            no_moves.clone().or(blocked),
            ActionKind::Create(o),
        ));
    }
    if let Some(id) = usable_action(g, u, UniqueType::MayParadrop) {
        let t = r.uniques();
        let (filter, range) = match t.get(id).data {
            UniqueData::MayParadrop(d) => (t.tile_filter(d.tiles).to_owned(), d.range),
            _ => (String::new(), 0),
        };
        let full = x.moves >= super::movement::max_moves(g, u);
        // Python wrote the range as the float its unique held.
        let range = PyFloat(f64::from(range));
        out.push(UnitActionEntry {
            params: &[("x", "target column"), ("y", "target row")],
            ..entry(
                "paradrop".into(),
                format!("Paradrop to a [{filter}] tile up to {range} tiles away"),
                (!full).then(|| "Paradropping needs the unit's full movement.".into()),
                ActionKind::Paradrop(id),
            )
        });
    }
    if type_has(g, x.base, UniqueType::AddInCapital) {
        let cap = g.player(p).and_then(|pl| pl.capital).and_then(|c| g.city(c));
        let reason = (cap.map(crate::state::cities::City::tile) != Some(at))
            .then(|| "Move into your capital.".to_owned());
        out.push(entry(
            "add_to_spaceship".into(),
            "Add to the spaceship".into(),
            no_moves.clone().or(reason),
            ActionKind::AddToSpaceship,
        ));
    }
    let t = r.uniques();
    for (k, id) in unit_map(g, u).into_iter().enumerate() {
        let meta = t.meta(id);
        if meta.ty.is_some_and(|ty| HANDLED.contains(&ty))
            || !triggerable(g, id)
            || meta.trigger.is_some()
        {
            continue;
        }
        let a = meta.actions;
        if !(a.consume || a.times.is_some() || a.once || a.movement.is_some()) {
            continue;
        }
        if meta.ty.and_then(|ty| usable_action(g, u, ty)) != Some(id) {
            continue;
        }
        let text = t.text_of(id);
        let name = text.split(" <").next().unwrap_or(text).to_owned();
        out.push(entry(format!("trigger:{k}"), name, no_moves.clone(), ActionKind::Trigger(id)));
    }
    out
}

// ---- The unit_action tool (actions.do_unit_action, actions.py:123-185) ------------------------------

/// `unit_action`: uses a unit's special ability (`tools.unit_action`, `tools.py:485-505`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UnitAction {
    pub unit_id: i64,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beliefs: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<i64>,
}

/// A checked unit action: what it will do.
pub enum ActionPlan {
    FoundCity { unit: UnitId, tile: TileIdx, name: Option<String> },
    FoundReligion { unit: UnitId, action: UniqueId, plan: found::FoundPlan },
    EnhanceReligion { unit: UnitId, action: UniqueId, beliefs: Vec<crate::base::ids::BeliefId> },
    Spread(spread::SpreadPlan),
    Heresy(spread::HeresyPlan),
    HurryResearch { unit: UnitId, science: i32 },
    HurryConstruction { unit: UnitId, plan: (CityId, Constructible, i32) },
    TradeMission { unit: UnitId, plan: (PlayerId, i32, i32) },
    Treatise { unit: UnitId, action: UniqueId },
    Create { unit: UnitId, option: InstantOption },
    Paradrop { unit: UnitId, to: TileIdx },
    Trigger { unit: UnitId, id: UniqueId },
}

/// A string argument as Python read it: `None` stays absent, anything else its `str`.
fn text_arg(v: Option<&Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(v) => Some(py::str_of(v)),
    }
}

/// A list of names as Python's `list(beliefs or [])` read one.
fn names_arg(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().map(py::str_of).collect(),
        Some(Value::String(s)) if !s.is_empty() => s.chars().map(String::from).collect(),
        _ => Vec::new(),
    }
}

/// Whether unit `u` may take `action` now, and what it will do (`actions.do_unit_action`'s
/// checks, with those of the rule each action runs).
///
/// # Errors
/// An action the unit does not have, one it cannot take now (with the list's reason), or the
/// refusal of the rule the action runs.
pub fn plan_action(
    g: &Game,
    u: UnitId,
    action: &str,
    name: Option<&str>,
    beliefs: &[String],
    target: Option<TileIdx>,
) -> Result<ActionPlan, ActionError> {
    let x = g.unit(u).ok_or_else(|| ActionError::new(ErrCode::NoSuchUnit, "No such unit."))?;
    let (p, at) = (x.owner(), x.tile());
    let list = unit_actions(g, u);
    let Some(a) = list.iter().rev().find(|a| a.id == action) else {
        let ids: Vec<&str> = list.iter().map(|a| a.id.as_str()).collect();
        let names = if ids.is_empty() { "none".to_owned() } else { ids.join(", ") };
        let unit = &g.rules().base_units()[x.base].name;
        return Err(ActionError::rule(format!(
            "{unit} has no action '{action}'. Its actions: {names}."
        )));
    };
    if let Some(reason) = &a.reason {
        return Err(ActionError::rule(if reason.is_empty() {
            "Not possible right now.".to_owned()
        } else {
            reason.clone()
        }));
    }
    Ok(match &a.kind {
        ActionKind::FoundCity => {
            ActionPlan::FoundCity { unit: u, tile: at, name: name.map(str::to_owned) }
        }
        ActionKind::FoundReligion(id) => {
            let name = name
                .filter(|n| !n.is_empty())
                .ok_or_else(|| ActionError::rule("Give the religion a name."))?;
            let plan = found::plan_religion(g, p, at, name, beliefs, None)?;
            ActionPlan::FoundReligion { unit: u, action: *id, plan }
        }
        ActionKind::EnhanceReligion(id) => {
            let beliefs = found::plan_enhance(g, p, at, beliefs)?;
            ActionPlan::EnhanceReligion { unit: u, action: *id, beliefs }
        }
        ActionKind::SpreadReligion => ActionPlan::Spread(spread::plan_spread(g, u)?),
        ActionKind::RemoveHeresy => ActionPlan::Heresy(spread::plan_remove_heresy(g, u)?),
        ActionKind::HurryResearch => {
            ActionPlan::HurryResearch { unit: u, science: great_people::plan_hurry_research(g, u)? }
        }
        ActionKind::HurryConstruction => ActionPlan::HurryConstruction {
            unit: u,
            plan: great_people::plan_hurry_construction(g, u)?,
        },
        ActionKind::TradeMission => {
            ActionPlan::TradeMission { unit: u, plan: great_people::plan_trade_mission(g, u)? }
        }
        ActionKind::PoliticalTreatise(id) => ActionPlan::Treatise { unit: u, action: *id },
        ActionKind::Create(o) => {
            workers::check_instant(g, u, o)?;
            ActionPlan::Create { unit: u, option: o.clone() }
        }
        ActionKind::Paradrop(_) => {
            let to = target.ok_or_else(|| {
                ActionError::rule("Give the target tile (x, y) for the paradrop.")
            })?;
            if let Some(why) = paradrop_problem(g, u, to) {
                return Err(ActionError::rule(why));
            }
            ActionPlan::Paradrop { unit: u, to }
        }
        // victory.add_to_spaceship (victory.py).
        ActionKind::AddToSpaceship => return Err(not_ported("game::victory")),
        ActionKind::Trigger(id) => {
            // Tried on a copy first: an effect that does nothing is refused unwritten.
            let mut probe = g.clone();
            let site = TriggerSite { civ: p, city: None, unit: Some(u), tile: Some(at) };
            let note = trigger_note(g, u);
            if !triggers::apply(&mut probe, *id, &site, Some(&note)) {
                return Err(ActionError::rule(
                    "That had no effect right now; the unit was not used.",
                ));
            }
            ActionPlan::Trigger { unit: u, id: *id }
        }
    })
}

/// The refusal of an action whose system is not ported yet (DESIGN.md 3.4, rule 4).
fn not_ported(path: &str) -> ActionError {
    ActionError::new(
        ErrCode::NotPorted,
        format!("This action is not ported to the new engine yet ({path})."),
    )
}

/// What a unit's one-time effect says caused it (`note=f"by a {u.type}"`).
fn trigger_note(g: &Game, u: UnitId) -> String {
    g.unit(u).map_or_else(String::new, |x| format!("by a {}", g.rules().base_units()[x.base].name))
}

/// Carries out what [`plan_action`] allowed, and says how to report it.
#[must_use]
pub fn apply_action(g: &mut Game, plan: ActionPlan) -> OutcomeSpec {
    match plan {
        ActionPlan::FoundCity { unit, tile, name } => {
            let Some(p) = g.unit(unit).map(crate::state::units::Unit::owner) else {
                return OutcomeSpec::value(Value::Null);
            };
            match found_city_by(g, p, tile, name.as_deref(), Some(unit)) {
                Ok(c) => OutcomeSpec::render(move |g| founded(g, c)),
                Err(e) => {
                    debug_assert!(false, "a founding the check allowed was refused: {e}");
                    OutcomeSpec::value(Value::Null)
                }
            }
        }
        ActionPlan::FoundReligion { unit, action, plan } => {
            let Some(p) = g.unit(unit).map(crate::state::units::Unit::owner) else {
                return OutcomeSpec::value(Value::Null);
            };
            found::apply_religion(g, p, &plan, |g| consume_action(g, unit, action));
            OutcomeSpec::render(move |g| found::religion_result(g, p, &plan))
        }
        ActionPlan::EnhanceReligion { unit, action, beliefs } => {
            let Some(p) = g.unit(unit).map(crate::state::units::Unit::owner) else {
                return OutcomeSpec::value(Value::Null);
            };
            found::apply_enhance(g, p, &beliefs, |g| consume_action(g, unit, action));
            OutcomeSpec::render(move |g| found::enhance_result(g, p))
        }
        ActionPlan::Spread(plan) => OutcomeSpec::value(spread::apply_spread(g, plan)),
        ActionPlan::Heresy(plan) => OutcomeSpec::value(spread::apply_remove_heresy(g, plan)),
        ActionPlan::HurryResearch { unit, science } => {
            OutcomeSpec::value(great_people::apply_hurry_research(g, unit, science))
        }
        ActionPlan::HurryConstruction { unit, plan } => {
            OutcomeSpec::value(great_people::apply_hurry_construction(g, unit, plan))
        }
        ActionPlan::TradeMission { unit, plan } => {
            OutcomeSpec::value(great_people::apply_trade_mission(g, unit, plan))
        }
        ActionPlan::Treatise { unit, action } => {
            let Some(p) = g.unit(unit).map(crate::state::units::Unit::owner) else {
                return OutcomeSpec::value(Value::Null);
            };
            let culture = policies::culture_from_great_writer(g, p);
            if let Some(x) = g.player_mut(p, PlayerTouch::STOCKS) {
                x.econ.culture += f64::from(culture);
            }
            consume_action(g, unit, action);
            OutcomeSpec::value(json!({ "culture_added": culture }))
        }
        ActionPlan::Create { unit, option } => {
            OutcomeSpec::value(workers::apply_instant(g, unit, &option))
        }
        ActionPlan::Paradrop { unit, to } => OutcomeSpec::value(paradrop(g, unit, to)),
        ActionPlan::Trigger { unit, id } => {
            let Some((p, at)) = g.unit(unit).map(|x| (x.owner(), x.tile())) else {
                return OutcomeSpec::value(Value::Null);
            };
            let text = g.rules().uniques().text_of(id);
            let name = text.split(" <").next().unwrap_or(text).to_owned();
            let site = TriggerSite { civ: p, city: None, unit: Some(unit), tile: Some(at) };
            let note = trigger_note(g, unit);
            let done = triggers::apply(g, id, &site, Some(&note));
            debug_assert!(done, "the effect did something on the copy the check tried");
            if g.unit(unit).is_some() {
                consume_action(g, unit, id);
            }
            OutcomeSpec::value(json!({ "done": name }))
        }
    }
}

/// What founding a city reports (`actions.py:137-138`): its id, its name and where it is. Python
/// listed the keys of its tile's coordinates (`list(g.xy(...))`, `["x", "y"]`).
// refcheck: unit-results-give-tiles
fn founded(g: &Game, c: CityId) -> Value {
    let Some(city) = g.city(c) else { return Value::Null };
    let (x, y) = g.xy(city.tile());
    json!({ "city_id": c.get(), "name": city.name.to_string(), "at": {"x": x, "y": y} })
}

impl Rule for UnitAction {
    type Plan = ActionPlan;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<ActionPlan, ActionError> {
        let u = own_unit(g, pid, self.unit_id)?;
        let target = match (self.x, self.y) {
            (Some(x), Some(y)) => tile_at(g, x, y).ok(),
            _ => None,
        };
        let name = text_arg(self.name.as_ref());
        let beliefs = names_arg(self.beliefs.as_ref());
        plan_action(g, u, &self.action, name.as_deref(), &beliefs, target)
    }

    fn apply(self, g: &mut Game, _: PlayerId, plan: ActionPlan) -> OutcomeSpec {
        apply_action(g, plan)
    }
}

// ---- The found_city tool (tools.found_city_tool, tools.py:508-525) ----------------------------------

/// `found_city`: a settler founds a city where it stands, and the result says what the city can
/// build (`tools.found_city_tool`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FoundCity {
    pub unit_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Value>,
}

impl Rule for FoundCity {
    type Plan = ActionPlan;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<ActionPlan, ActionError> {
        let u = own_unit(g, pid, self.unit_id)?;
        let name = text_arg(self.name.as_ref());
        plan_action(g, u, "found_city", name.as_deref(), &[], None)
    }

    fn apply(self, g: &mut Game, _: PlayerId, plan: ActionPlan) -> OutcomeSpec {
        let ActionPlan::FoundCity { unit, tile, name } = plan else {
            return apply_action(g, plan);
        };
        let Some(p) = g.unit(unit).map(crate::state::units::Unit::owner) else {
            return OutcomeSpec::value(Value::Null);
        };
        match found_city_by(g, p, tile, name.as_deref(), Some(unit)) {
            Ok(c) => OutcomeSpec::render(move |g| {
                let mut out = founded(g, c);
                let r = g.rules();
                let production = g.city(c).and_then(current_construction).map_or_else(
                    || "nothing (use set_production)".to_owned(),
                    |i| item_name(r, i).to_owned(),
                );
                let items = buildable_items(g, c);
                let mut can = Map::new();
                let units: Vec<&str> = items.units.iter().filter_map(|u| r.name(u)).collect();
                let buildings: Vec<&str> =
                    items.buildings.iter().filter_map(|b| r.name(b)).collect();
                let wonders: Vec<&str> = items.wonders.iter().filter_map(|b| r.name(b)).collect();
                let mut other: Vec<&str> = Vec::new();
                if items.gold {
                    other.push("Gold");
                }
                if items.science {
                    other.push("Science");
                }
                for (k, v) in [
                    ("units", units),
                    ("buildings", buildings),
                    ("wonders", wonders),
                    ("other", other),
                ] {
                    if !v.is_empty() {
                        can.insert(k.into(), json!(v));
                    }
                }
                if let Some(m) = out.as_object_mut() {
                    m.insert("production".into(), json!(production));
                    m.insert("can_build".into(), Value::Object(can));
                }
                out
            }),
            Err(e) => {
                debug_assert!(false, "a founding the check allowed was refused: {e}");
                OutcomeSpec::value(Value::Null)
            }
        }
    }
}

// ---- Paradrop (actions.py:188-229) --------------------------------------------------------------

/// Why unit `u` cannot paradrop onto tile `to`, or `None` (`actions.paradrop_problem`,
/// `actions.py:188-211`): a tile explored, of the filter its unique names, within its range,
/// that is no foreign city and that it may stand on.
#[must_use]
pub fn paradrop_problem(g: &Game, u: UnitId, to: TileIdx) -> Option<String> {
    let x = g.unit(u)?;
    let Some(id) = usable_action(g, u, UniqueType::MayParadrop) else {
        return Some("This unit cannot paradrop from here.".into());
    };
    let t = g.rules().uniques();
    let UniqueData::MayParadrop(d) = t.get(id).data else {
        return Some("This unit cannot paradrop from here.".into());
    };
    if !g.grid().contains(to) {
        return Some("That tile is off the map.".into());
    }
    if to == x.tile() {
        return Some("The unit is already there.".into());
    }
    if i64::from(g.grid().distance(x.tile(), to)) > i64::from(d.range) {
        return Some(format!("Paradrops reach at most {} tiles.", PyFloat(f64::from(d.range))));
    }
    if !g.player(x.owner()).is_some_and(|p| p.explored.contains(to.0)) {
        return Some("You cannot paradrop into unexplored territory.".into());
    }
    if !t.filters().tile_matches(d.tiles, &g.view(), to, Some(x.owner())) {
        return Some(format!("The target must be a [{}] tile.", t.tile_filter(d.tiles)));
    }
    if g.city_at(to).is_some_and(|c| c.owner() != x.owner()) {
        return Some("You cannot paradrop into a foreign city.".into());
    }
    if !super::movement::can_stand(g, x.owner(), x.base, to, Some(u)) {
        return Some(
            "The unit cannot enter that tile (occupied, impassable or closed borders).".into(),
        );
    }
    None
}

/// Drops unit `u` onto tile `to`, which [`paradrop_problem`] allowed (`actions.paradrop`,
/// `actions.py:214-229`): it lands with no movement, having acted, out of its fortification, sleep
/// or move order, and enters the tile. The result gives where it came from and went, where Python
/// listed the keys of each tile's coordinates (`unit-results-give-tiles`).
pub fn paradrop(g: &mut Game, u: UnitId, to: TileIdx) -> Value {
    let Some(from) = g.unit(u).map(crate::state::units::Unit::tile) else { return Value::Null };
    if g.relocate_unit(u, to).is_err() {
        debug_assert!(false, "a paradrop the check allowed was refused");
        return Value::Null;
    }
    if let Some(x) = g.unit_mut(u, UnitTouch::CORE | UnitTouch::MOVES) {
        x.moves = 0;
        x.acted = true;
        if matches!(
            x.activity,
            Some(
                Activity::Fortify
                    | Activity::FortifyHeal
                    | Activity::Sleep
                    | Activity::SleepHeal
                    | Activity::Goto
            )
        ) {
            x.activity = None;
            x.goto = None;
            x.path.clear();
            x.order_wait = 0;
        }
    }
    super::movement::on_enter_tile(g, u, to);
    let (fx, fy) = g.xy(from);
    let (tx, ty) = g.xy(to);
    json!({ "from": {"x": fx, "y": fy}, "to": {"x": tx, "y": ty} })
}

/// An improvement's id by name, for the `create:<name>` actions.
#[must_use]
pub fn create_id(g: &Game, imp: ImprovementId) -> String {
    format!("create:{}", g.rules().improvements()[imp].name)
}
