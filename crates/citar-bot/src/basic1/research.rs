//! Research (`choose_research`, `_tech_value`, `_building_potential`, basic.py:876-998): a free
//! technology goes to the most expensive one available, and research to the first step of the
//! path to whichever technology is worth most for what the whole path costs. A technology's
//! worth is what it unlocks weighed by what the civilization needs now: buildings (by their flat
//! stats in the classic mode, by what they would add to the empire's yields in the potential
//! one), stronger units (more at war), improvements, luxuries and strategic resources it has in
//! its borders but cannot improve, and the spaceship.
//!
//! What differs from Python, on purpose:
//! - the noise on a technology's score is drawn from the bot's `Research` stream keyed by the
//!   technology (DESIGN.md P2.3.5), where Python drew from one sequential generator in the order
//!   it met them, so that what one technology draws never moves another's;
//! - a building's `[+N stat] per [M] population` is read from its compiled unique
//!   (`StatsPerPopulation` with one positive whole stat), where Python matched the text with a
//!   regular expression: the same uniques, without reading text;
//! - the costs of a decision's technologies are asked once each and kept for the decision, as
//!   Python kept the values.

use std::collections::BTreeMap;

use citar_engine::base::ids::{BuildingId, PlayerId, TechId};
use citar_engine::base::num;
use citar_engine::base::rng::KeyPart as _;
use citar_engine::base::stats::Stat;
use citar_engine::game::research::{
    ChooseFreeTech, SetResearch, available_techs, current, is_unresearchable, path_to, tech_cost,
};
use citar_engine::game::{Action, Game, advisor, query};
use citar_engine::rules::defs::{Domain, ResourceType};
use citar_engine::unique::UniqueData;
use serde_json::json;

use super::Seat;
use super::context::Context;
use crate::driver::Turn;
use crate::params::TechMode;
use crate::stream::Stream;

/// The stats a building's flat yields are weighed by, in Python's `STAT_KEYS` order.
const STATS: [Stat; 7] = [
    Stat::Food,
    Stat::Production,
    Stat::Gold,
    Stat::Science,
    Stat::Culture,
    Stat::Faith,
    Stat::Happiness,
];

/// `choose_research` (basic.py:876-909): a free technology first, then, with nothing being
/// researched, the best path's first step.
pub(crate) fn choose_research(t: &mut Turn<'_>, s: &Seat<'_>, ctx: &Context) {
    let pid = t.pid();
    if let Some(tech) = free_now(t.game(), pid) {
        let name = t.game().rules().name(tech).unwrap_or_default().to_owned();
        t.act(Action::ChooseFreeTech(ChooseFreeTech { tech: json!(name) }));
    }
    if current(t.game(), pid).is_some() {
        return;
    }
    if let Some(tech) = next(t.game(), pid, s, ctx) {
        let name = t.game().rules().name(tech).unwrap_or_default().to_owned();
        t.act(Action::SetResearch(SetResearch { tech: json!(name), append: None }));
    }
}

/// The free technology the civilization takes now: [`free_choice`] while it holds one.
pub(crate) fn free_now(g: &Game, pid: PlayerId) -> Option<TechId> {
    let holds = g.player(pid).is_some_and(|p| p.tech.free_techs > 0);
    if holds { free_choice(g, pid) } else { None }
}

/// The free technology it would take if it held one: the most expensive available, the first of
/// equals in `available_techs`' order (`max(avail, key=tech_cost)`).
pub(crate) fn free_choice(g: &Game, pid: PlayerId) -> Option<TechId> {
    let mut best: Option<(TechId, i32)> = None;
    for t in available_techs(g, pid) {
        let c = tech_cost(g, pid, t);
        if best.is_none_or(|(_, b)| c > b) {
            best = Some((t, c));
        }
    }
    best.map(|(t, _)| t)
}

/// What it would research next, were nothing being researched (basic.py:888-909): the first
/// step of the path to the technology of the best value for its cost, each score drawn a little
/// up or down by `tech_noise`; else the cheapest available; `None` when nothing is.
pub(crate) fn next(g: &Game, pid: PlayerId, s: &Seat<'_>, ctx: &Context) -> Option<TechId> {
    let avail = available_techs(g, pid);
    if avail.is_empty() {
        return None;
    }
    let p = s.params;
    let mut memo = Memo::default();
    let (lo, hi) = (1.0 - p.tech_noise, 1.0 + p.tech_noise);
    let mut best: Option<TechId> = None;
    let mut best_v = -1.0;
    for t in g.rules().techs().ids() {
        if g.has_tech(pid, Some(t)) || is_unresearchable(g, pid, t) {
            continue;
        }
        let mut v = value(g, pid, t, s, ctx, &mut memo);
        if v <= 0.0 {
            continue;
        }
        let path = path_to(g, pid, t);
        let Some(&first) = path.first() else { continue };
        let cost: i64 = path.iter().map(|&x| i64::from(memo.cost(g, pid, x))).sum();
        let before = &path[..path.len() - 1];
        let along: Vec<f64> = before.iter().map(|&x| value(g, pid, x, s, ctx, &mut memo)).collect();
        v += p.tech_path_value * num::py_sum(along);
        // A noise of 0 draws nothing: every factor would be 1.
        let noise = if hi > lo {
            lo + (hi - lo) * Stream::Research.rng(g, pid, &[t.key()]).unit()
        } else {
            1.0
        };
        #[allow(clippy::cast_precision_loss, reason = "a path's cost is far below 2^53")]
        let score = v / num::pow(cost as f64, p.tech_cost_exp) * noise;
        if score > best_v {
            best = Some(first);
            best_v = score;
        }
    }
    best.or_else(|| {
        // The cheapest available, the first of equals (`min(avail, key=tech_cost)`).
        let mut cheapest: Option<(TechId, i32)> = None;
        for &t in &avail {
            let c = memo.cost(g, pid, t);
            if cheapest.is_none_or(|(_, b)| c < b) {
                cheapest = Some((t, c));
            }
        }
        cheapest.map(|(t, _)| t)
    })
}

/// What one decision has worked out already: the technologies' values and costs, the strongest
/// land unit the civilization has now, and the empire's average yields.
#[derive(Default)]
pub(crate) struct Memo {
    values: BTreeMap<TechId, f64>,
    costs: BTreeMap<TechId, i32>,
    best_now: Option<i32>,
    empire: Option<Empire>,
}

impl Memo {
    fn cost(&mut self, g: &Game, pid: PlayerId, t: TechId) -> i32 {
        *self.costs.entry(t).or_insert_with(|| tech_cost(g, pid, t))
    }
}

/// The empire's figures `_building_potential` weighs a building by.
#[derive(Clone, Copy)]
struct Empire {
    /// Its cities, at least 1.
    n: f64,
    /// Their mean population.
    pop: f64,
    /// Its yields, none below 0, in `STATS`' order (happiness is not one of them: 0).
    yields: [f64; 7],
}

/// The value of technology `t` to the civilization, kept for the decision (`_tech_value`,
/// basic.py:911-917).
pub(crate) fn value(
    g: &Game,
    pid: PlayerId,
    t: TechId,
    s: &Seat<'_>,
    ctx: &Context,
    memo: &mut Memo,
) -> f64 {
    if let Some(&v) = memo.values.get(&t) {
        return v;
    }
    let v = value_uncached(g, pid, t, s, ctx, memo);
    memo.values.insert(t, v);
    v
}

/// A technology scored by what it unlocks and what the civilization needs now
/// (`_tech_value_uncached`, basic.py:947-998).
fn value_uncached(
    g: &Game,
    pid: PlayerId,
    t: TechId,
    s: &Seat<'_>,
    ctx: &Context,
    memo: &mut Memo,
) -> f64 {
    let r = g.rules();
    let p = s.params;
    let un = &r.derived().unlocks[t];
    let mut v = p.tech_base_value;
    for &b in &un.buildings {
        let bd = &r.buildings()[b];
        if p.tech_mode == TechMode::Potential {
            v += potential(g, pid, b, s, ctx, memo);
            continue;
        }
        let weights = [
            p.tw_food,
            p.tw_production,
            p.tw_gold,
            p.tw_science,
            p.tw_culture,
            p.tw_faith,
            p.tw_happiness,
        ];
        let flat = num::py_sum(STATS.iter().zip(weights).map(|(&k, w)| bd.stats[k] * w));
        v += f64::from(p.tech_building_base)
            + flat
            + f64::from(p.tech_building_unique) * count(bd.uniques.all.len());
        if bd.is_wonder {
            v += f64::from(p.tech_wonder);
        }
    }
    let best_now = *memo.best_now.get_or_insert_with(|| {
        r.base_units()
            .iter()
            .filter(|(_, d)| {
                d.military && d.domain == Domain::Land && g.has_tech(pid, d.required_tech)
            })
            .map(|(_, d)| d.strength.max(d.ranged_strength))
            .max()
            .unwrap_or(p.tech_default_power)
    });
    for &u in &un.units {
        let d = &r.base_units()[u];
        if !d.military {
            v += f64::from(p.tech_civilian_unit);
            continue;
        }
        let gain = advisor::power(d) / f64::from(best_now.max(1));
        if gain > p.tech_unit_min_gain {
            let war = if ctx.wars.is_empty() { 1.0 } else { p.tech_unit_war_mult };
            v += (f64::from(p.tech_unit_base) + f64::from(p.tech_unit_gain) * (gain - 1.0))
                * war
                * (p.tech_unit_aggr_base + s.spec.aggression);
        }
    }
    v += f64::from(p.tech_improvement) * count(un.improvements.len())
        + f64::from(p.tech_reveal) * count(un.reveals.len());
    let cities = i32::try_from(ctx.cities.len()).unwrap_or(i32::MAX);
    for res in ctx.pending_res.iter() {
        let improves = un
            .improvements
            .iter()
            .any(|&i| citar_engine::game::tiles::resource_improved_by(g, res, i));
        if !improves {
            continue;
        }
        match r.resources()[res].kind {
            ResourceType::Luxury if !ctx.lux_owned.contains(res) => {
                let needed = ctx.hap < cities.saturating_add(p.tech_lux_hap_margin);
                v += f64::from(if needed { p.tech_lux_needed } else { p.tech_lux });
            }
            ResourceType::Strategic => v += f64::from(p.tech_strategic),
            _ => {}
        }
    }
    v += f64::from(p.tech_unique) * count(r.techs()[t].uniques.all.len());
    let space_era = usize::try_from(p.tech_space_era).unwrap_or(usize::MAX);
    if ctx.era >= space_era && un.units.iter().any(|&u| r.derived().advisor.space.contains(u)) {
        v += f64::from(p.tech_space);
    }
    v
}

/// A count as a float.
fn count(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// What having building `b` available is worth to the empire, roughly (`_building_potential`,
/// basic.py:919-945): its flat stats, its percentages of the empire's yields per city, its
/// stats per population and its specialist slots, in every city, or once for a wonder.
fn potential(
    g: &Game,
    pid: PlayerId,
    b: BuildingId,
    s: &Seat<'_>,
    ctx: &Context,
    memo: &mut Memo,
) -> f64 {
    let r = g.rules();
    let p = s.params;
    let bd = &r.buildings()[b];
    let emp = *memo.empire.get_or_insert_with(|| {
        let n = ctx.cities.len().max(1);
        let pop: i64 = ctx.cities.iter().filter_map(|&c| g.city(c)).map(|c| i64::from(c.pop)).sum();
        let st = query::civ_stats(g, pid).total;
        let mut yields = [0.0; 7];
        for (y, k) in yields.iter_mut().zip(STATS).take(6) {
            *y = st[k].max(0.0);
        }
        #[allow(clippy::cast_precision_loss, reason = "populations and city counts are small")]
        Empire { n: count(n), pop: pop as f64 / count(n), yields }
    });
    let w =
        [p.u_food, p.u_production, p.u_gold, p.u_science, p.u_culture, p.u_faith, p.u_happiness];
    let mut per_city = num::py_sum(STATS.iter().zip(w).map(|(&k, wk)| bd.stats[k] * wk));
    per_city -= f64::from(bd.maintenance) * p.u_gold;
    for (i, &k) in STATS.iter().enumerate() {
        let pct = bd.percent_stat_bonus[k];
        if pct != 0.0 {
            per_city += pct / 100.0 * emp.yields[i] / emp.n * w[i];
        }
    }
    let table = r.uniques();
    for id in bd.uniques.ids() {
        if let UniqueData::StatsPerPopulation(x) = table.get(id).data
            && let Some((i, amount)) = one_whole_stat(table.stats(x.stats))
            && x.per > 0
        {
            per_city += amount * emp.pop / f64::from(x.per) * w[i];
        }
    }
    let slots: i32 = bd.specialist_slots.iter().map(|&(_, n)| n).sum();
    per_city += f64::from(slots) * p.pot_specialist;
    let mult = if bd.is_wonder || bd.is_national_wonder { 1.0 } else { emp.n };
    f64::from(p.pot_base) + per_city.max(0.0) * mult
}

/// The one stat a `[+N stat]` gives, with N, when the stats are exactly one positive whole
/// number of one stat: what Python's `\[\+(\d+) (\w+)\]` matched.
fn one_whole_stat(st: &citar_engine::base::stats::Stats) -> Option<(usize, f64)> {
    let mut found = None;
    for (i, &k) in STATS.iter().enumerate() {
        let n = st[k];
        if n == 0.0 {
            continue;
        }
        if found.is_some() || n < 0.0 || n.fract() != 0.0 {
            return None;
        }
        found = Some((i, n));
    }
    found
}
