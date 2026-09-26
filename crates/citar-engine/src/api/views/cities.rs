//! A city as a viewer sees it, and everything its owner could build or buy there
//! (`views.city_info`, `views.py:162-249`).

use serde::Serialize;
use serde_json::{Map, Value, json};

use super::{CITY_YIELDS, rounded};
use crate::base::ids::{CityId, PlayerId};
use crate::base::num;
use crate::base::stats::{Stat, Stats};
use crate::game::cities::{borders, construction, purchase, stats as cstats};
use crate::game::combat::city as ccity;
use crate::game::derive::stats as memo;
use crate::game::{Game, economy, religion};
use crate::state::cities::Constructible;

/// How many tiles for sale `get_city` lists, cheapest first (`views.py:244`).
const BUYABLE_SHOWN: usize = 24;

/// A city as a viewer sees it (`views.city_info` without the detail), typed so that the client
/// view writes it straight to JSON: what anyone sees of it, and for its owner (or a spectator)
/// [`OwnCity`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CityView<'a> {
    pub id: u32,
    pub name: &'a str,
    pub owner: u8,
    pub x: i32,
    pub y: i32,
    pub pop: u16,
    pub capital: bool,
    pub hp: i32,
    pub max_hp: i32,
    pub strength: f64,
    pub original_capital: bool,
    pub puppet: bool,
    pub razing: bool,
    pub resistance: i16,
    /// Only for its owner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_bombard: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub religion: Option<String>,
    #[serde(flatten)]
    pub own: Option<OwnCity<'a>>,
}

/// What a city's owner sees of it besides: its yields, growth, production, citizens, culture
/// and trade route.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OwnCity<'a> {
    pub yields: NameMap<'static, f64>,
    pub happiness: f64,
    pub food_stored: f64,
    pub food_to_grow: i32,
    pub turns_to_grow: Option<f64>,
    pub starving: bool,
    pub avoid_growth: bool,
    pub queue: Vec<QueueItem<'a>>,
    pub buildings: Vec<&'a str>,
    pub focus: &'static str,
    pub auto_production: bool,
    pub specialists: NameMap<'a, u8>,
    pub culture_stored: f64,
    pub culture_for_next_tile: i32,
    pub worked_tiles: Vec<[i32; 2]>,
    pub locked_tiles: Vec<[i32; 2]>,
    pub connected_to_capital: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub we_love_the_king_day_turns: Option<i16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub demands_resource: Option<&'a str>,
}

/// One item of a city's production queue: its cost, progress and turns (none for gold or
/// science).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct QueueItem<'a> {
    pub item: &'a str,
    pub cost: Option<i32>,
    pub progress: i64,
    pub turns: Option<i32>,
}

/// Entries by name, written as a JSON object in their order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NameMap<'a, V>(pub Vec<(&'a str, V)>);

impl<V: Serialize> Serialize for NameMap<'_, V> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

/// A city as `viewer` sees it, typed (`views.city_info` without the detail).
#[must_use]
pub fn city_view(g: &Game, c: CityId, viewer: Option<PlayerId>) -> Option<CityView<'_>> {
    let city = g.city(c)?;
    let r = g.rules();
    let owner = city.owner();
    let (x, y) = g.xy(city.tile());
    let xys = |ts: &[crate::base::ids::TileIdx]| -> Vec<[i32; 2]> {
        ts.iter()
            .map(|&t| {
                let (a, b) = g.xy(t);
                [a, b]
            })
            .collect()
    };
    let own = viewer.is_none_or(|v| v == owner).then(|| {
        let total = memo::city_stats(g, c).total;
        let happiness = num::py_sum(memo::city_parts(g, c).happiness.iter().map(|&(_, v)| v));
        let surplus = total[Stat::Food];
        let need = cstats::food_to_next_pop(g, c);
        // Python's `-(-(need - food) // surplus)`: a ceiling, as a float.
        let turns_to_grow =
            (surplus > 0.0).then(|| ((f64::from(need) - city.food) / surplus).ceil() + 0.0);
        let queue = city
            .queue
            .iter()
            .map(|&item| {
                let perpetual = matches!(item, Constructible::Perpetual(_));
                let progress = city.progress.get(&item).copied().unwrap_or(0.0);
                QueueItem {
                    item: construction::item_name(r, item),
                    cost: (!perpetual).then(|| cstats::production_cost(g, owner, item, Some(c))),
                    progress: num::trunc_i64(progress),
                    turns: (!perpetual).then(|| cstats::turns_to_build(g, c, item)),
                }
            })
            .collect();
        OwnCity {
            // The totals round as they are, which differs from Python where its sums carried
            // rounding error to a tenth's half or across zero.
            // refcheck: city-view-rounds-its-own-sums
            yields: rounded_map(&total, &CITY_YIELDS, 1),
            happiness: num::round_ndigits(happiness, 1),
            food_stored: num::round_ndigits(city.food, 1),
            food_to_grow: need,
            turns_to_grow,
            starving: surplus < 0.0,
            avoid_growth: city.avoid_growth,
            queue,
            buildings: city.buildings.iter().filter_map(|b| r.name(b)).collect(),
            focus: city.focus.name(),
            auto_production: city.auto_production,
            specialists: specialists(g, &city.specialists),
            culture_stored: num::round_ndigits(city.culture, 1),
            culture_for_next_tile: borders::culture_to_next_tile(g, c),
            worked_tiles: xys(&city.worked),
            locked_tiles: xys(&city.locked),
            connected_to_capital: memo::connectivity(g, owner).media(c).is_some(),
            we_love_the_king_day_turns: (city.wltkd > 0).then_some(city.wltkd),
            demands_resource: city
                .demanded_resource
                .filter(|_| city.wltkd <= 0)
                .and_then(|res| r.name(res)),
        }
    });
    Some(CityView {
        id: c.get(),
        name: &city.name,
        owner: owner.0,
        x,
        y,
        pop: city.pop,
        capital: g.player(owner).is_some_and(|p| p.capital == Some(c)),
        hp: city.health,
        max_hp: cstats::max_health(g, c),
        strength: num::round_ndigits(f64::from(cstats::city_strength(g, c)), 1),
        original_capital: city.original_capital,
        puppet: city.puppet,
        razing: city.razing,
        resistance: city.resistance,
        can_bombard: (viewer == Some(owner)).then(|| ccity::can_bombard(g, c).is_none()),
        religion: religion::majority_religion(g, c).map(|rel| religion::display_name(g, rel)),
        own,
    })
}

/// Stats rounded for display, the zero ones dropped, typed ([`super::rounded`]).
fn rounded_map(s: &Stats, keys: &[Stat], nd: i32) -> NameMap<'static, f64> {
    NameMap(
        keys.iter()
            .filter(|&&k| s[k] != 0.0)
            .map(|&k| (k.key(), num::round_ndigits(s[k], nd)))
            .collect(),
    )
}

/// A city as `viewer` sees it (`views.city_info`): [`city_view`], and with `detail` everything
/// it could build or buy, its tiles and its followers.
#[must_use]
pub fn city_info(g: &Game, c: CityId, viewer: Option<PlayerId>, detail: bool) -> Value {
    let Some(view) = city_view(g, c, viewer) else { return Value::Null };
    let own = view.own.is_some();
    // A view is text, numbers, lists and maps with text keys, which always convert.
    let mut v = serde_json::to_value(view).unwrap_or(Value::Null);
    if detail
        && own
        && let Some(m) = v.as_object_mut()
    {
        let st = memo::city_stats(g, c).clone();
        let bd: Map<String, Value> = st
            .breakdown
            .iter()
            .map(|(src, y)| {
                let keys: Vec<Stat> = y.keys.iter().collect();
                (src.name(), rounded(&y.stats, &keys, 1))
            })
            .filter(|(_, v)| v.as_object().is_some_and(|o| !o.is_empty()))
            .map(|(k, v)| (k.to_owned(), v))
            .collect();
        m.insert("yield_breakdown".into(), Value::Object(bd));
        let hb: Map<String, Value> = memo::city_parts(g, c)
            .happiness
            .iter()
            .filter(|&&(_, v)| v != 0.0)
            .map(|&(k, v)| (k.name().to_owned(), json!(num::round_ndigits(v, 1))))
            .collect();
        m.insert("happiness_breakdown".into(), Value::Object(hb));
        city_detail(g, c, m);
    }
    v
}

/// A city's specialists by name, those it has (`dict(c.specialists)`).
fn specialists<'a>(g: &'a Game, counts: &[u8]) -> NameMap<'a, u8> {
    let r = g.rules();
    NameMap(
        counts
            .iter()
            .enumerate()
            .filter(|&(_, &n)| n > 0)
            .filter_map(|(i, &n)| {
                let s = crate::base::ids::SpecialistId(u8::try_from(i).ok()?);
                Some((&*r.specialists().get(s)?.name, n))
            })
            .collect(),
    )
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
    // Python listed a city's tiles, and chose among equally dear ones for sale, in its `within`
    // order.
    let in_order = |mut ts: Vec<crate::base::ids::TileIdx>| {
        ts.sort_by_key(|&t| borders::within_order(g, city.tile(), t));
        ts
    };
    let tiles: Vec<Value> = in_order(economy::city_tiles(g, c))
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
    let mut buy: Vec<(i32, Value)> = in_order(borders::choosable_tiles(g, c))
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
