//! Buying things in a city with gold or faith (`cities.py:1363-1568`, UnCiv's
//! `INonPerpetualConstruction` purchase rules): whether an item may be bought with a stat at all
//! ([`can_purchase_with`]), what it costs ([`buy_cost`]), whether a purchase can go ahead now
//! ([`purchase_check`]), and buying it, which the tool `buy` runs (`tools.py:665-676`).
//!
//! The kitchen sink's purchase uniques are read here: `May buy [] units with [] []`, `May buy []
//! units for [] [] []`, `May buy [] buildings with [] for [] times their normal Production cost`,
//! `[] cost of purchasing [] buildings []%`, `May buy [] buildings for [] [] [] at an increasing
//! price ([])` and `Can be purchased for [] [] []`.

use serde_json::{Value, json};
use smallvec::SmallVec;

use super::super::Game;
use super::super::action::{OutcomeSpec, Rule};
use super::super::derive::rev::{CityTouch, PlayerTouch};
use super::super::error::{ActionError, ErrCode};
use super::citizens::own_city;
use super::construction::{
    RejectionKind, complete_construction, item_name, rejection_kinds, rejection_reasons,
    unit_placement, validate_queue,
};
use super::queue::resolve_item;
use super::stats::production_cost;
use crate::base::ids::{CityId, PlayerId, SetRef};
use crate::base::num;
use crate::base::py;
use crate::base::stats::Stat;
use crate::rules::defs::Domain;
use crate::state::cities::Constructible;
use crate::unique::{Ctx, SourceUniques, UniqueData, UniqueType, uq};

/// The speed's modifier of a price in a stat (`cities._stat_cost_mod`, `cities.py:1366-1368`).
fn stat_cost_mod(g: &Game, stat: Stat) -> f64 {
    let sp = g.speed();
    match stat {
        Stat::Gold => sp.gold_cost_modifier,
        Stat::Production => sp.production_cost_modifier,
        Stat::Science => sp.science_cost_modifier,
        Stat::Culture => sp.culture_cost_modifier,
        Stat::Faith => sp.faith_cost_modifier,
        Stat::Food | Stat::Happiness => sp.modifier,
    }
}

/// An item's own uniques: a building's, or a unit's and then its unit type's.
fn own_uniques(g: &Game, item: Constructible) -> SmallVec<[&'static SourceUniques; 2]> {
    let r = g.rules();
    let mut out = SmallVec::new();
    match item {
        Constructible::Building(b) => out.push(&r.buildings()[b].uniques),
        Constructible::Unit(u) => {
            let d = &r.base_units()[u];
            out.push(&d.uniques);
            out.push(&r.unit_types()[d.unit_type].uniques);
        }
        Constructible::Perpetual(_) => {}
    }
    out
}

/// Whether one of an item's own uniques of type `ty` holds in `ctx`.
fn own_has(g: &Game, item: Constructible, ty: UniqueType, ctx: &Ctx) -> bool {
    let v = g.view();
    own_uniques(g, item).into_iter().any(|s| uq::any(uq::object(&v, s, ty, ctx)))
}

/// Whether an item passes a static filter of units or buildings (`base_unit_matches`,
/// `building_matches`).
fn names(g: &Game, set: SetRef, item: Constructible) -> bool {
    let t = g.rules().uniques();
    match item {
        Constructible::Unit(u) => t.in_set(set, u),
        Constructible::Building(b) => t.in_set(set, b),
        Constructible::Perpetual(_) => false,
    }
}

/// The standard gold price of an item, from its production cost (`cities.base_gold_cost`,
/// `cities.py:1371-1379`): `(30 * cost)^0.75`, times its hurry modifier.
#[must_use]
pub fn base_gold_cost(g: &Game, p: PlayerId, item: Constructible, city: Option<CityId>) -> f64 {
    let r = g.rules();
    let hurry = match item {
        Constructible::Unit(u) => r.base_units()[u].hurry_cost_modifier,
        Constructible::Building(b) => r.buildings()[b].hurry_cost_modifier,
        Constructible::Perpetual(_) => None,
    }
    .unwrap_or(0);
    num::pow(30.0 * f64::from(production_cost(g, p, item, city)), 0.75)
        * (1.0 + f64::from(hurry) / 100.0)
}

/// The `n`th price of what costs more each time it is bought (`cities._increasing`,
/// `cities.py:1382-1384`).
fn increasing(base: i32, inc: i32, n: i32) -> f64 {
    let n = f64::from(n);
    (f64::from(base) + f64::from(inc) / 2.0 * (n * n + n)).trunc()
}

/// How many of an item a civilization has bought at an increasing price.
fn bought_increasing(g: &Game, p: PlayerId, item: Constructible) -> i32 {
    g.player(p).and_then(|x| x.civ.bought_increasing.get(&item)).map_or(0, |&n| i32::from(n))
}

/// The purchase uniques of a city that name an item for a stat, of each kind, with the price
/// each gives (`buy_cost`'s `specific`, `cities.py:1460-1472`); or, with `None` for the price
/// asked, only whether any names it (`can_purchase_with`, `cities.py:1404-1433`).
fn specific_prices(g: &Game, c: CityId, item: Constructible, stat: Stat) -> SmallVec<[f64; 4]> {
    let Some(city) = g.city(c) else { return SmallVec::new() };
    let p = city.owner();
    let t = g.rules().uniques();
    let filters = t.filters();
    let v = g.view();
    let ctx = Ctx::city(&v, c);
    let unit = matches!(item, Constructible::Unit(_));
    let city_ok = |f| filters.city_matches(f, &v, c, None);
    let mut out = SmallVec::new();
    let (increasing_ty, production_ty, with_ty, amount_ty) = if unit {
        (
            UniqueType::BuyUnitsIncreasingCost,
            UniqueType::BuyUnitsByProductionCost,
            UniqueType::BuyUnitsWithStat,
            UniqueType::BuyUnitsForAmountStat,
        )
    } else {
        (
            UniqueType::BuyBuildingsIncreasingCost,
            UniqueType::BuyBuildingsByProductionCost,
            UniqueType::BuyBuildingsWithStat,
            UniqueType::BuyBuildingsForAmountStat,
        )
    };
    let n = bought_increasing(g, p, item);
    for h in uq::city(&v, c, increasing_ty, &ctx) {
        let x = match *h.data() {
            UniqueData::BuyUnitsIncreasingCost(x) => {
                (x.units, x.cost, x.stat, x.cities, x.increase)
            }
            UniqueData::BuyBuildingsIncreasingCost(x) => {
                (x.buildings, x.cost, x.stat, x.cities, x.increase)
            }
            _ => continue,
        };
        if x.2 == stat && names(g, x.0, item) && city_ok(x.3) {
            for _ in 0..h.n {
                out.push(increasing(x.1, i32::from(x.4), n) * stat_cost_mod(g, stat));
            }
        }
    }
    for h in uq::city(&v, c, production_ty, &ctx) {
        let x = match *h.data() {
            UniqueData::BuyUnitsByProductionCost(x) => (x.units, x.stat, x.times),
            UniqueData::BuyBuildingsByProductionCost(x) => (x.buildings, x.stat, x.times),
            _ => continue,
        };
        if x.1 == stat && names(g, x.0, item) {
            for _ in 0..h.n {
                out.push(f64::from(production_cost(g, p, item, Some(c))) * f64::from(x.2));
            }
        }
    }
    let with = uq::city(&v, c, with_ty, &ctx).any(|h| {
        let x = match *h.data() {
            UniqueData::BuyUnitsWithStat(x) => (x.units, x.stat, x.cities),
            UniqueData::BuyBuildingsWithStat(x) => (x.buildings, x.stat, x.cities),
            _ => return false,
        };
        x.1 == stat && names(g, x.0, item) && city_ok(x.2)
    });
    if with {
        out.push(era_buy_cost(g, p) * stat_cost_mod(g, stat));
    }
    for h in uq::city(&v, c, amount_ty, &ctx) {
        let x = match *h.data() {
            UniqueData::BuyUnitsForAmountStat(x) => (x.units, x.cost, x.stat, x.cities),
            UniqueData::BuyBuildingsForAmountStat(x) => (x.buildings, x.cost, x.stat, x.cities),
            _ => continue,
        };
        if x.2 == stat && names(g, x.0, item) && city_ok(x.3) {
            for _ in 0..h.n {
                out.push(f64::from(x.1) * stat_cost_mod(g, stat));
            }
        }
    }
    out
}

/// The era's base price of buying with a stat (`baseUnitBuyCost` of the civilization's era).
fn era_buy_cost(g: &Game, p: PlayerId) -> f64 {
    let era = super::super::derive::civ::era(g, p);
    f64::from(g.rules().eras()[era].base_unit_buy_cost)
}

/// An item's own `Can be purchased with [stat] [cities]` that holds for `stat` here.
fn own_purchasable_with(g: &Game, c: CityId, item: Constructible, stat: Stat) -> bool {
    let t = g.rules().uniques();
    let v = g.view();
    let ctx = Ctx::city(&v, c);
    own_uniques(g, item).into_iter().any(|s| {
        uq::object(&v, s, UniqueType::CanBePurchasedWithStat, &ctx).any(|h| {
            matches!(*h.data(), UniqueData::CanBePurchasedWithStat(x)
                if x.stat == stat && t.filters().city_matches(x.cities, &v, c, None))
        })
    })
}

/// An item's own `Can be purchased for [amount] [stat] [cities]` that hold for `stat` here, with
/// their amounts.
fn own_amounts(g: &Game, c: CityId, item: Constructible, stat: Stat) -> SmallVec<[i32; 2]> {
    let t = g.rules().uniques();
    let v = g.view();
    let ctx = Ctx::city(&v, c);
    let mut out = SmallVec::new();
    for s in own_uniques(g, item) {
        for h in uq::object(&v, s, UniqueType::CanBePurchasedForAmountStat, &ctx) {
            if let UniqueData::CanBePurchasedForAmountStat(x) = *h.data()
                && x.stat == stat
                && t.filters().city_matches(x.cities, &v, c, None)
            {
                out.push(x.cost);
            }
        }
    }
    out
}

/// Whether a city may buy an item with a stat at all, whatever it costs
/// (`cities.can_purchase_with`, `cities.py:1387-1439`): its uniques and the city's say so, and
/// gold buys anything buildable that is not a wonder. A unit rejected only as unbuildable, as the
/// faith-bought religious units are, may still be bought.
#[must_use]
pub fn can_purchase_with(g: &Game, c: CityId, item: Constructible, stat: Stat) -> bool {
    purchasable(g, c, item, stat, || {
        rejection_kinds(g, c, item).iter().any(|&k| k != RejectionKind::Unbuildable)
    })
}

/// [`can_purchase_with`], told by `blocked` whether a unit has a reason it cannot be built other
/// than being unbuildable: [`purchase_check`] has read its reasons already.
fn purchasable(
    g: &Game,
    c: CityId,
    item: Constructible,
    stat: Stat,
    blocked: impl FnOnce() -> bool,
) -> bool {
    if matches!(item, Constructible::Perpetual(_))
        || matches!(stat, Stat::Production | Stat::Happiness)
    {
        return false;
    }
    let v = g.view();
    let ctx = Ctx::city(&v, c);
    if own_has(g, item, UniqueType::CannotBePurchased, &ctx) {
        return false;
    }
    if let Constructible::Unit(_) = item {
        if blocked() {
            return false;
        }
    } else if let Constructible::Building(b) = item
        && !own_purchasable_with(g, c, item, stat)
        && stat == Stat::Gold
        && g.rules().buildings()[b].any_wonder
    {
        return false;
    }
    if !specific_prices(g, c, item, stat).is_empty() {
        return true;
    }
    if own_purchasable_with(g, c, item, stat) || !own_amounts(g, c, item, stat).is_empty() {
        return true;
    }
    stat == Stat::Gold && !own_has(g, item, UniqueType::Unbuildable, &ctx)
}

/// What an item costs in a stat in a city, or `None` if it cannot be bought with it
/// (`cities.buy_cost`, `cities.py:1442-1500`): the cheapest price a unique of the city gives, else
/// the item's own price, else gold's standard price (or the era's for a stat the item may be
/// bought with); then the discounts; rounded down to ten.
#[must_use]
pub fn buy_cost(g: &Game, c: CityId, item: Constructible, stat: Stat) -> Option<i32> {
    let city = g.city(c)?;
    let p = city.owner();
    let specific = specific_prices(g, c, item, stat);
    let mut cost = if let Some(&low) = specific.iter().min_by(|a, b| a.total_cmp(b)) {
        low
    } else {
        let lows = own_amounts(g, c, item, stat);
        if let Some(&low) = lows.iter().min() {
            f64::from(low) * stat_cost_mod(g, stat)
        } else if stat == Stat::Gold {
            base_gold_cost(g, p, item, Some(c))
        } else if own_purchasable_with(g, c, item, stat) {
            era_buy_cost(g, p) * stat_cost_mod(g, stat)
        } else {
            return None;
        }
    };
    let t = g.rules().uniques();
    let v = g.view();
    let ctx = Ctx::city(&v, c);
    let items_discount = |cost: &mut f64| {
        for h in uq::city(&v, c, UniqueType::BuyItemsDiscount, &ctx) {
            if let UniqueData::BuyItemsDiscount(x) = *h.data()
                && x.stat == stat
            {
                for _ in 0..h.n {
                    *cost *= 1.0 + f64::from(x.percent) / 100.0;
                }
            }
        }
    };
    match item {
        Constructible::Unit(u) => {
            for h in uq::city(&v, c, UniqueType::BuyUnitsDiscount, &ctx) {
                if let UniqueData::BuyUnitsDiscount(x) = *h.data()
                    && x.stat == stat
                    && t.in_set(x.units, u)
                {
                    for _ in 0..h.n {
                        cost *= 1.0 + f64::from(x.percent) / 100.0;
                    }
                }
            }
            items_discount(&mut cost);
        }
        Constructible::Building(b) => {
            items_discount(&mut cost);
            for h in uq::city(&v, c, UniqueType::BuyBuildingsDiscount, &ctx) {
                if let UniqueData::BuyBuildingsDiscount(x) = *h.data()
                    && x.stat == stat
                    && t.in_set(x.buildings, b)
                {
                    for _ in 0..h.n {
                        cost *= 1.0 + f64::from(x.percent) / 100.0;
                    }
                }
            }
        }
        Constructible::Perpetual(_) => return None,
    }
    Some(num::trunc_i32(cost / 10.0).saturating_mul(10))
}

/// A stat's name, as the tools and messages write it: `Gold`, `Faith`.
fn stat_name(stat: Stat) -> &'static str {
    stat.name()
}

/// What a civilization has of a stat to buy with: its treasury for gold, else its reserve.
fn have(g: &Game, p: PlayerId, stat: Stat) -> f64 {
    g.stat_reserve(p, stat)
}

/// Whether a purchase can go ahead, and its cost (`cities.purchase_check`, `cities.py:1503-1537`):
/// the refusal (or `None`) and the price (or `None` when there is none). A refusal for want of
/// the stat says what it costs.
#[must_use]
pub fn purchase_check(
    g: &Game,
    c: CityId,
    item: Constructible,
    stat: Stat,
) -> (Option<String>, Option<i32>) {
    let r = g.rules();
    let Some(city) = g.city(c) else { return (Some("No such city.".into()), None) };
    let name = item_name(r, item);
    if matches!(item, Constructible::Perpetual(_)) {
        return (Some(format!("Unknown item '{name}'.")), None);
    }
    if city.puppet {
        return (Some("Puppet cities cannot purchase.".into()), None);
    }
    if city.resistance > 0 {
        return (Some(format!("{} is in resistance.", city.name)), None);
    }
    let rr = rejection_reasons(g, c, item);
    if let Some(x) = rr.into_iter().find(|x| x.kind != RejectionKind::Unbuildable) {
        return (Some(x.text), None);
    }
    if let Constructible::Unit(u) = item {
        let d = &r.base_units()[u];
        if d.domain != Domain::Air {
            let occupied = if d.military {
                g.military_at(city.tile()).is_some()
            } else {
                g.civilian_at(city.tile()).is_some()
            };
            if occupied {
                return (
                    Some(format!(
                        "Move the unit out of {} first: a bought {name} appears in the city.",
                        city.name
                    )),
                    None,
                );
            }
        }
    }
    let refused = || Some(format!("{name} cannot be bought with {}.", stat_name(stat)));
    // Its reasons were read above: none but being unbuildable is left.
    if !purchasable(g, c, item, stat, || false) {
        return (refused(), None);
    }
    let Some(cost) = buy_cost(g, c, item, stat) else { return (refused(), None) };
    let have = have(g, city.owner(), stat);
    if have < f64::from(cost) {
        return (
            Some(format!(
                "{name} costs {cost} {}; you have {}.",
                stat_name(stat),
                num::trunc_i64(have)
            )),
            Some(cost),
        );
    }
    (None, Some(cost))
}

/// What buying an item did: the stat and its price, for the result.
#[derive(Clone, Copy, Debug)]
pub struct Purchase {
    pub city: CityId,
    pub item: Constructible,
    pub stat: Stat,
    pub cost: i32,
}

/// Whether a city may buy an item now (`cities.purchase`' checks, `cities.py:1540-1554`): what
/// [`purchase_check`] says, and room for the unit.
///
/// # Errors
/// The refusal.
pub fn plan_purchase(
    g: &Game,
    c: CityId,
    item: Constructible,
    stat: Stat,
) -> Result<Purchase, ActionError> {
    let (reason, cost) = purchase_check(g, c, item, stat);
    if let Some(reason) = reason {
        return Err(ActionError::rule(reason));
    }
    let cost = cost.unwrap_or(0);
    if let Constructible::Unit(u) = item
        && unit_placement(g, c, u).is_none()
    {
        return Err(ActionError::rule("No room to place the unit."));
    }
    Ok(Purchase { city: c, item, stat, cost })
}

/// Buys what [`plan_purchase`] allowed (`cities.purchase`, `cities.py:1552-1567`): the item is
/// finished (a bought unit waits a turn to move), paid for, counted if it costs more each time,
/// taken off the queue if it is a building, and noted as bought this turn.
pub fn apply_purchase(g: &mut Game, x: Purchase) {
    let Purchase { city: c, item, stat, cost } = x;
    let Some(owner) = g.city(c).map(crate::state::cities::City::owner) else { return };
    let placed = complete_construction(g, c, item, Some(stat));
    debug_assert!(placed, "the check found room for the unit");
    if stat == Stat::Gold {
        if let Some(pl) = g.player_mut(owner, PlayerTouch::STOCKS) {
            pl.econ.gold -= f64::from(cost);
        }
    } else {
        g.add_stat(owner, stat, -f64::from(cost));
    }
    let counts = {
        let t = g.rules().uniques();
        let v = g.view();
        let ctx = Ctx::city(&v, c);
        let ty = match item {
            Constructible::Unit(_) => UniqueType::BuyUnitsIncreasingCost,
            _ => UniqueType::BuyBuildingsIncreasingCost,
        };
        uq::city(&v, c, ty, &ctx).any(|h| {
            let x = match *h.data() {
                UniqueData::BuyUnitsIncreasingCost(x) => (x.units, x.stat, x.cities),
                UniqueData::BuyBuildingsIncreasingCost(x) => (x.buildings, x.stat, x.cities),
                _ => return false,
            };
            names(g, x.0, item) && t.filters().city_matches(x.2, &v, c, None) && x.1 == stat
        })
    };
    if counts && let Some(pl) = g.player_mut(owner, PlayerTouch::OTHER) {
        let n = pl.civ.bought_increasing.entry(item).or_insert(0);
        *n = n.saturating_add(1);
    }
    if let Some(x) = g.city_mut(c, CityTouch::CORE) {
        if matches!(item, Constructible::Building(_))
            && let Some(i) = x.queue.iter().position(|&q| q == item)
        {
            x.queue.remove(i);
        }
        x.bought_this_turn.push(item);
    }
    validate_queue(g, c);
}

/// What buying reports (`cities.py:1568`), read from the settled game.
#[must_use]
pub fn purchase_result(g: &Game, x: &Purchase) -> Value {
    let owner = g.city(x.city).map(crate::state::cities::City::owner);
    let left = owner.map_or(0.0, |p| have(g, p, x.stat));
    json!({
        "bought": item_name(g.rules(), x.item),
        "cost": x.cost,
        "stat": stat_name(x.stat),
        "left": num::trunc_i64(left),
    })
}

/// `buy`: buys a unit or building in a city with gold or faith (`tools.buy`, `tools.py:665-676`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Buy {
    pub city_id: i64,
    pub item: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<Value>,
}

impl Rule for Buy {
    type Plan = Purchase;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError> {
        let raw = self.currency.as_ref().map_or_else(|| "Gold".to_owned(), py::str_of);
        let stat = match raw.to_lowercase().as_str() {
            "gold" => Stat::Gold,
            "faith" => Stat::Faith,
            _ => {
                return Err(ActionError::new(ErrCode::BadParam, "currency must be Gold or Faith."));
            }
        };
        let c = own_city(g, pid, self.city_id)?;
        let item = resolve_item(g, &py::str_of(&self.item), Some(pid))?;
        plan_purchase(g, c, item, stat)
    }

    fn apply(self, g: &mut Game, _: PlayerId, plan: Self::Plan) -> OutcomeSpec {
        apply_purchase(g, plan);
        OutcomeSpec::render(move |g| purchase_result(g, &plan))
    }
}
