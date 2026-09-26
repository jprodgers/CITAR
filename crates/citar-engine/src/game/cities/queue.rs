//! A city's production queue (`cities.py:1571-1717`): reading an item a player named
//! ([`resolve_item`]), putting it in the queue ([`plan_production`]), editing the queue
//! ([`plan_queue_change`]), and the tools `set_production`, `change_queue`,
//! `set_auto_production` and `rename_city` (`tools.py:631-662, 782-795`).
//!
//! What a city builds when its queue runs empty and the advisor picks for it
//! ([`auto_pick_production`], `cities.py:1696-1717`) is the production advisor's
//! (`game::advisor`).

use serde_json::{Value, json};
use smallvec::SmallVec;

use super::super::Game;
use super::super::action::{OutcomeSpec, Rule};
use super::super::derive::rev::CityTouch;
use super::super::error::{ActionError, ErrCode};
use super::construction::{QUEUE_MAX, equivalent_unit, item_name, rejection_reasons, turns_for};
use super::founding::{self, equivalent_building};
use crate::base::ids::{BaseUnitId, BuildingId, CityId, PlayerId};
use crate::base::py;
use crate::base::sets::PlayerSet;
use crate::game::lookup::own_city;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::cities::{Constructible, Perpetual};

/// The exact item a player named (`cities.resolve_item`, `cities.py:1574-1593`): a conversion of
/// production by its name, a unit, then a building, named loosely; with a civilization, its own
/// version of it.
///
/// # Errors
/// A name that is none of them.
pub fn resolve_item(
    g: &Game,
    item: &str,
    p: Option<PlayerId>,
) -> Result<Constructible, ActionError> {
    if let Some(k) = Perpetual::from_name(item) {
        return Ok(Constructible::Perpetual(k));
    }
    let r = g.rules();
    if let Some(u) = r.resolve::<BaseUnitId>(item) {
        return Ok(Constructible::Unit(p.map_or(u, |p| equivalent_unit(g, p, u))));
    }
    if let Some(b) = r.resolve::<BuildingId>(item) {
        return Ok(Constructible::Building(p.map_or(b, |p| equivalent_building(g, p, b))));
    }
    let low = item.trim().to_lowercase();
    if let Some(k) = Perpetual::ALL.into_iter().find(|k| k.name().to_lowercase() == low) {
        return Ok(Constructible::Perpetual(k));
    }
    Err(ActionError::new(
        ErrCode::BadParam,
        format!("Unknown item '{item}'. Use unit or building names like 'Warrior' or 'Granary'."),
    ))
}

/// The queue that putting `item` at the front of a city's queue, or at its end with `append`,
/// makes (`cities.set_production`, `cities.py:1634-1661`): a building already queued moves to the
/// front, and a conversion of production stays last when an item is appended. Reads only.
///
/// # Errors
/// The first reason the city cannot build it, a building already queued when appending, or a
/// full queue.
pub fn plan_production(
    g: &Game,
    c: CityId,
    item: Constructible,
    append: bool,
) -> Result<SmallVec<[Constructible; 4]>, ActionError> {
    let Some(city) = g.city(c) else { return Err(ActionError::rule("No such city.")) };
    if let Some(first) = rejection_reasons(g, c, item).into_iter().next() {
        return Err(ActionError::rule(first.text));
    }
    let mut q = city.queue.clone();
    let building = matches!(item, Constructible::Building(_));
    if building && q.contains(&item) && !(!append && q.first() == Some(&item)) {
        if append {
            return Err(ActionError::rule(format!(
                "{} is already queued in {}.",
                item_name(g.rules(), item),
                city.name
            )));
        }
        q.retain(|x| *x != item);
    }
    if append {
        if q.len() >= QUEUE_MAX {
            return Err(ActionError::rule(format!(
                "The production queue is full ({QUEUE_MAX} items)."
            )));
        }
        if q.last().is_some_and(|x| matches!(x, Constructible::Perpetual(_))) {
            q.insert(q.len() - 1, item);
        } else {
            q.push(item);
        }
    } else {
        if q.first() != Some(&item) {
            q.insert(0, item);
        }
        q.truncate(QUEUE_MAX);
    }
    Ok(q)
}

/// A city's queue by name.
fn queue_names(g: &Game, c: CityId) -> Vec<&str> {
    let r = g.rules();
    g.city(c).map(|x| x.queue.iter().map(|&i| item_name(r, i)).collect()).unwrap_or_default()
}

/// Writes a city's queue.
pub(crate) fn write_queue(g: &mut Game, c: CityId, q: SmallVec<[Constructible; 4]>) {
    if let Some(x) = g.city_mut(c, CityTouch::CORE) {
        x.queue = q;
    }
}

/// What setting production reports (`cities.py:1663-1664`), read from the settled game: the
/// queue and the turns its first item takes.
fn production_result(g: &Game, c: CityId) -> Value {
    let first = g.city(c).and_then(|x| x.queue.first().copied());
    json!({
        "city": g.city(c).map(|x| &*x.name),
        "queue": queue_names(g, c),
        "turns": first.and_then(|i| turns_for(g, c, i)),
    })
}

/// What editing a queue does (`cities.change_queue`, `cities.py:1667-1693`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueEdit {
    Up,
    Down,
    First,
    Last,
    Remove,
    Clear,
}

impl QueueEdit {
    /// Every edit, in Python's order.
    pub const ALL: [Self; 6] =
        [Self::Up, Self::Down, Self::First, Self::Last, Self::Remove, Self::Clear];

    /// The name the tool takes.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::First => "first",
            Self::Last => "last",
            Self::Remove => "remove",
            Self::Clear => "clear",
        }
    }
}

/// The queue an edit leaves (`cities.change_queue`, `cities.py:1670-1693`): the entry at `index`
/// moved up, down, first or last, or removed; or the queue cleared. Reads only.
///
/// # Errors
/// An edit it does not know, an empty queue, or a position outside it.
pub fn plan_queue_change(
    g: &Game,
    c: CityId,
    index: i64,
    action: &str,
) -> Result<SmallVec<[Constructible; 4]>, ActionError> {
    let Some(city) = g.city(c) else { return Err(ActionError::rule("No such city.")) };
    let low = action.to_lowercase();
    let Some(edit) = QueueEdit::ALL.into_iter().find(|e| e.name() == low) else {
        let all: Vec<&str> = QueueEdit::ALL.iter().map(|e| e.name()).collect();
        return Err(ActionError::new(
            ErrCode::BadParam,
            format!("Action must be one of {}.", all.join(", ")),
        ));
    };
    let mut q = city.queue.clone();
    if edit == QueueEdit::Clear {
        q.clear();
        return Ok(q);
    }
    if q.is_empty() {
        return Err(ActionError::rule(format!("{}'s production queue is empty.", city.name)));
    }
    let Some(i) = usize::try_from(index).ok().filter(|&i| i < q.len()) else {
        return Err(ActionError::new(
            ErrCode::BadParam,
            format!("Queue position must be 0-{} (0 is the item in production).", q.len() - 1),
        ));
    };
    let item = q.remove(i);
    match edit {
        QueueEdit::Up => q.insert(i.saturating_sub(1), item),
        QueueEdit::Down => q.insert((i + 1).min(q.len()), item),
        QueueEdit::First => q.insert(0, item),
        QueueEdit::Last => q.push(item),
        QueueEdit::Remove | QueueEdit::Clear => {}
    }
    Ok(q)
}

/// What a city starts when its queue runs empty and the advisor picks for it
/// (`cities.auto_pick_production`, `cities.py:1696-1717`): a puppet a building or Gold, any other
/// city what the advisor advises (`advisor::auto_pick`), put at the front of its queue; `None`
/// when it picks nothing, or what it picked cannot be built.
pub fn auto_pick_production(g: &mut Game, c: CityId) -> Option<Constructible> {
    let item = super::super::advisor::auto_pick(g, c)?;
    let q = plan_production(g, c, item, false).ok()?;
    write_queue(g, c, q);
    g.city(c).and_then(|x| x.queue.first().copied())
}

/// `set_production`: what a city builds, first or appended (`tools.set_production`,
/// `tools.py:631-637`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SetProduction {
    pub city_id: i64,
    pub item: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append: Option<Value>,
}

impl Rule for SetProduction {
    type Plan = (CityId, SmallVec<[Constructible; 4]>);

    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError> {
        let c = own_city(g, pid, self.city_id)?;
        let item = resolve_item(g, &py::str_of(&self.item), Some(pid))?;
        let append = self.append.as_ref().is_some_and(py::truthy);
        Ok((c, plan_production(g, c, item, append)?))
    }

    fn apply(self, g: &mut Game, _: PlayerId, (c, q): Self::Plan) -> OutcomeSpec {
        write_queue(g, c, q);
        OutcomeSpec::render(move |g| production_result(g, c))
    }
}

/// `change_queue`: moves, removes or clears entries of a city's queue (`tools.change_queue`,
/// `tools.py:640-647`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChangeQueue {
    pub city_id: i64,
    pub action: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<i64>,
}

impl Rule for ChangeQueue {
    type Plan = (CityId, SmallVec<[Constructible; 4]>);

    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError> {
        let c = own_city(g, pid, self.city_id)?;
        let action = if self.action.is_null() { String::new() } else { py::str_of(&self.action) };
        Ok((c, plan_queue_change(g, c, self.index.unwrap_or(0), &action)?))
    }

    fn apply(self, g: &mut Game, _: PlayerId, (c, q): Self::Plan) -> OutcomeSpec {
        write_queue(g, c, q);
        OutcomeSpec::render(
            move |g| json!({"city": g.city(c).map(|x| &*x.name), "queue": queue_names(g, c)}),
        )
    }
}

/// `set_auto_production`: hands a city's choices to the advisor when its queue runs empty, and
/// picks at once if it is empty now (`tools.set_auto_production`, `tools.py:650-662`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SetAutoProduction {
    pub city_id: i64,
    pub enabled: Value,
}

impl Rule for SetAutoProduction {
    type Plan = CityId;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError> {
        own_city(g, pid, self.city_id)
    }

    fn apply(self, g: &mut Game, _: PlayerId, c: Self::Plan) -> OutcomeSpec {
        let on = py::truthy(&self.enabled);
        if let Some(x) = g.city_mut(c, CityTouch::CORE) {
            x.auto_production = on;
        }
        let empty = g.city(c).is_some_and(|x| x.queue.is_empty());
        let picked = if on && empty { auto_pick_production(g, c) } else { None };
        OutcomeSpec::render(move |g| {
            let mut out = json!({
                "city": g.city(c).map(|x| &*x.name),
                "auto_production": g.city(c).is_some_and(|x| x.auto_production),
            });
            if let (Some(item), Some(o)) = (picked, out.as_object_mut()) {
                o.insert("started".into(), json!(item_name(g.rules(), item)));
            }
            out
        })
    }
}

/// `rename_city`: renames one of the player's cities, at any time (`tools.rename_city`,
/// `tools.py:782-795`). A name that changes nothing is refused, since it is almost always a
/// confused caller repeating itself.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RenameCity {
    pub city_id: i64,
    pub name: Value,
}

impl Rule for RenameCity {
    type Plan = (CityId, String);

    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError> {
        let c = own_city(g, pid, self.city_id)?;
        let new = founding::plan_rename(g, c, &py::str_of(&self.name))?;
        let old = g.city(c).map(|x| x.name.to_string()).unwrap_or_default();
        if new == old {
            return Err(ActionError::rule(format!("The city is already named {old}.")));
        }
        Ok((c, new))
    }

    fn apply(self, g: &mut Game, pid: PlayerId, (c, new): Self::Plan) -> OutcomeSpec {
        let Some((old, at)) = g.city(c).map(|x| (x.name.to_string(), x.tile())) else {
            return OutcomeSpec::value(Value::Null);
        };
        founding::write_name(g, c, &new);
        g.emit(
            EngineEvent::CityRenamed,
            &format!("{old} is now called {new}."),
            Some(PlayerSet::single(pid)),
            Some(at),
            EventData::default(),
            &[],
        );
        OutcomeSpec::value(json!({"name": new}))
    }
}
