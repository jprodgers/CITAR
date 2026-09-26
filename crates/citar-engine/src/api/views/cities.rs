//! A city as a viewer sees it, and everything its owner could build or buy there
//! (`views.city_info`, `views.py:162-249`).

use serde_json::{Map, Value, json};

use super::{CITY_YIELDS, rounded, xy};
use crate::base::ids::{CityId, PlayerId};
use crate::base::num;
use crate::base::stats::Stat;
use crate::game::cities::{borders, construction, purchase, stats as cstats};
use crate::game::combat::city as ccity;
use crate::game::derive::stats as memo;
use crate::game::{Game, economy, religion};
use crate::state::cities::Constructible;

/// How many tiles for sale `get_city` lists, cheapest first (`views.py:244`).
const BUYABLE_SHOWN: usize = 24;

/// A city as `viewer` sees it (`views.city_info`): what anyone sees of it, and for its owner (or
/// a spectator) its yields, growth, production, citizens and culture; with `detail`, everything
/// it could build or buy, its tiles and its followers.
#[must_use]
pub fn city_info(g: &Game, c: CityId, viewer: Option<PlayerId>, detail: bool) -> Value {
    let Some(city) = g.city(c) else { return Value::Null };
    let r = g.rules();
    let owner = city.owner();
    let (x, y) = g.xy(city.tile());
    let capital = g.player(owner).is_some_and(|p| p.capital == Some(c));
    let mut m = Map::new();
    m.insert("id".into(), json!(c.get()));
    m.insert("name".into(), json!(&*city.name));
    m.insert("owner".into(), json!(owner.0));
    m.insert("x".into(), json!(x));
    m.insert("y".into(), json!(y));
    m.insert("pop".into(), json!(city.pop));
    m.insert("capital".into(), json!(capital));
    m.insert("hp".into(), json!(city.health));
    m.insert("max_hp".into(), json!(cstats::max_health(g, c)));
    m.insert(
        "strength".into(),
        json!(num::round_ndigits(f64::from(cstats::city_strength(g, c)), 1)),
    );
    m.insert("original_capital".into(), json!(city.original_capital));
    m.insert("puppet".into(), json!(city.puppet));
    m.insert("razing".into(), json!(city.razing));
    m.insert("resistance".into(), json!(city.resistance));
    if viewer == Some(owner) {
        m.insert("can_bombard".into(), json!(ccity::can_bombard(g, c).is_none()));
    }
    if let Some(rel) = religion::majority_religion(g, c) {
        m.insert("religion".into(), json!(religion::display_name(g, rel)));
    }
    if viewer.is_some_and(|v| v != owner) {
        return Value::Object(m);
    }
    let (total, breakdown) = {
        let st = memo::city_stats(g, c);
        let breakdown: Vec<(&'static str, Value)> = if detail {
            st.breakdown
                .iter()
                .map(|(src, y)| {
                    let keys: Vec<Stat> = y.keys.iter().collect();
                    (src.name(), rounded(&y.stats, &keys, 1))
                })
                .filter(|(_, v)| v.as_object().is_some_and(|o| !o.is_empty()))
                .collect()
        } else {
            Vec::new()
        };
        (st.total, breakdown)
    };
    let happiness: Vec<(&'static str, f64)> =
        memo::city_parts(g, c).happiness.iter().map(|&(k, v)| (k.name(), v)).collect();
    let surplus = total[Stat::Food];
    let need = cstats::food_to_next_pop(g, c);
    // Python's `-(-(need - food) // surplus)`: a ceiling, as a float.
    let turns_to_grow =
        (surplus > 0.0).then(|| ((f64::from(need) - city.food) / surplus).ceil() + 0.0);
    // The totals round as they are, which differs from Python where its sums carried rounding
    // error to a tenth's half or across zero.
    // refcheck: city-view-rounds-its-own-sums
    m.insert("yields".into(), rounded(&total, &CITY_YIELDS, 1));
    m.insert(
        "happiness".into(),
        json!(num::round_ndigits(num::py_sum(happiness.iter().map(|&(_, v)| v)), 1)),
    );
    m.insert("food_stored".into(), json!(num::round_ndigits(city.food, 1)));
    m.insert("food_to_grow".into(), json!(need));
    m.insert("turns_to_grow".into(), json!(turns_to_grow));
    m.insert("starving".into(), json!(surplus < 0.0));
    m.insert("avoid_growth".into(), json!(city.avoid_growth));
    let queue: Vec<Value> = city
        .queue
        .iter()
        .map(|&item| {
            let perpetual = matches!(item, Constructible::Perpetual(_));
            let progress = city.progress.get(&item).copied().unwrap_or(0.0);
            json!({
                "item": construction::item_name(r, item),
                "cost": (!perpetual).then(|| cstats::production_cost(g, owner, item, Some(c))),
                "progress": num::trunc_i64(progress),
                "turns": (!perpetual).then(|| cstats::turns_to_build(g, c, item)),
            })
        })
        .collect();
    m.insert("queue".into(), Value::Array(queue));
    let buildings: Vec<&str> = city.buildings.iter().filter_map(|b| r.name(b)).collect();
    m.insert("buildings".into(), json!(buildings));
    m.insert("focus".into(), json!(city.focus.name()));
    m.insert("auto_production".into(), json!(city.auto_production));
    m.insert("specialists".into(), specialists(g, &city.specialists));
    m.insert("culture_stored".into(), json!(num::round_ndigits(city.culture, 1)));
    m.insert("culture_for_next_tile".into(), json!(borders::culture_to_next_tile(g, c)));
    let tiles = |ts: &[crate::base::ids::TileIdx]| -> Value {
        Value::Array(ts.iter().map(|&t| xy(g, t)).collect())
    };
    m.insert("worked_tiles".into(), tiles(&city.worked));
    m.insert("locked_tiles".into(), tiles(&city.locked));
    let connected = memo::connectivity(g, owner).media(c).is_some();
    m.insert("connected_to_capital".into(), json!(connected));
    if city.wltkd > 0 {
        m.insert("we_love_the_king_day_turns".into(), json!(city.wltkd));
    } else if let Some(res) = city.demanded_resource {
        m.insert("demands_resource".into(), json!(r.name(res)));
    }
    if detail {
        let bd: Map<String, Value> =
            breakdown.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
        m.insert("yield_breakdown".into(), Value::Object(bd));
        let hb: Map<String, Value> = happiness
            .iter()
            .filter(|&&(_, v)| v != 0.0)
            .map(|&(k, v)| (k.to_owned(), json!(num::round_ndigits(v, 1))))
            .collect();
        m.insert("happiness_breakdown".into(), Value::Object(hb));
        city_detail(g, c, &mut m);
    }
    Value::Object(m)
}

/// A city's specialists by name, those it has (`dict(c.specialists)`).
fn specialists(g: &Game, counts: &[u8]) -> Value {
    let r = g.rules();
    let m: Map<String, Value> = counts
        .iter()
        .enumerate()
        .filter(|&(_, &n)| n > 0)
        .filter_map(|(i, &n)| {
            let s = crate::base::ids::SpecialistId(u8::try_from(i).ok()?);
            Some((r.specialists().get(s)?.name.to_string(), json!(n)))
        })
        .collect();
    Value::Object(m)
}

/// What the owner of a city needs to run it (`views.py:208-248`): its specialist slots,
/// everything it can build with the cost and turns and what buying it costs, the units only faith
/// buys, its tiles and what each yields, the tiles for sale and its followers.
fn city_detail(g: &Game, c: CityId, m: &mut Map<String, Value>) {
    let Some(city) = g.city(c) else { return };
    let r = g.rules();
    let owner = city.owner();
    let slots: Map<String, Value> = cstats::max_specialists(g, c)
        .into_iter()
        .filter_map(|(s, n)| Some((r.specialists().get(s)?.name.to_string(), json!(n))))
        .collect();
    m.insert("max_specialists".into(), Value::Object(slots));
    let items = construction::buildable_items(g, c);
    let mut other = Vec::new();
    if items.gold {
        other.push(Constructible::Perpetual(crate::state::cities::Perpetual::Gold));
    }
    if items.science {
        other.push(Constructible::Perpetual(crate::state::cities::Perpetual::Science));
    }
    let groups: [(&str, Vec<Constructible>); 4] = [
        ("units", items.units.iter().map(Constructible::Unit).collect()),
        ("buildings", items.buildings.iter().map(Constructible::Building).collect()),
        ("wonders", items.wonders.iter().map(Constructible::Building).collect()),
        ("other", other),
    ];
    let mut can = Map::new();
    for (kind, list) in &groups {
        if list.is_empty() {
            continue;
        }
        let rows: Vec<Value> = list.iter().map(|&item| build_row(g, c, owner, item)).collect();
        can.insert((*kind).to_owned(), Value::Array(rows));
    }
    m.insert("can_build".into(), Value::Object(can));
    let mut faith_only = Vec::new();
    for (u, _) in r.base_units().iter() {
        if items.units.contains(u) {
            continue;
        }
        let item = Constructible::Unit(u);
        if let (None, cost) = purchase::purchase_check(g, c, item, Stat::Faith) {
            faith_only.push(json!({"item": construction::item_name(r, item), "buy_faith": cost}));
        }
    }
    if !faith_only.is_empty() {
        m.insert("buy_with_faith".into(), Value::Array(faith_only));
    }
    let tiles: Vec<Value> = economy::city_tiles(g, c)
        .into_iter()
        .map(|t| {
            let (x, y) = g.xy(t);
            let ty = memo::tile_yield(g, t, Some(owner), Some(c));
            json!({
                "x": x,
                "y": y,
                "worked": city.worked.contains(&t) || t == city.tile(),
                "yields": rounded(&ty, &Stat::ALL, 1),
            })
        })
        .collect();
    m.insert("tiles".into(), Value::Array(tiles));
    let mut buy: Vec<(i32, Value)> = borders::choosable_tiles(g, c)
        .into_iter()
        .filter(|&t| borders::can_buy_tile(g, c, t).is_none())
        .map(|t| {
            let (x, y) = g.xy(t);
            let gold = borders::buy_tile_cost(g, c, t);
            (gold, json!({"x": x, "y": y, "gold": gold}))
        })
        .collect();
    buy.sort_by_key(|&(gold, _)| gold);
    let buy: Vec<Value> = buy.into_iter().take(BUYABLE_SHOWN).map(|(_, v)| v).collect();
    m.insert("buyable_tiles".into(), Value::Array(buy));
    if g.religion_enabled() {
        let f: Map<String, Value> = religion::followers(city)
            .into_iter()
            .filter(|&(_, n)| n != 0)
            .map(|(rel, n)| (religion::display_name(g, rel), json!(n)))
            .collect();
        m.insert("religious_followers".into(), Value::Object(f));
    }
}

/// One item a city can build (`views.py:212-222`): its cost and turns (none for gold or science),
/// and what buying it costs in gold and in faith where the city could buy it, or could but for
/// the gold or faith it lacks.
fn build_row(g: &Game, c: CityId, owner: PlayerId, item: Constructible) -> Value {
    let r = g.rules();
    let mut row = Map::new();
    row.insert("item".into(), json!(construction::item_name(r, item)));
    if !matches!(item, Constructible::Perpetual(_)) {
        row.insert("cost".into(), json!(cstats::production_cost(g, owner, item, Some(c))));
        row.insert("turns".into(), json!(cstats::turns_to_build(g, c, item)));
    }
    for (stat, key) in [(Stat::Gold, "buy_gold"), (Stat::Faith, "buy_faith")] {
        let (reason, cost) = purchase::purchase_check(g, c, item, stat);
        let short_of = reason.as_deref().is_none_or(|s| s.to_lowercase().contains("enough"));
        if let Some(cost) = cost.filter(|&n| n != 0)
            && short_of
        {
            row.insert(key.into(), json!(cost));
        }
    }
    Value::Object(row)
}
