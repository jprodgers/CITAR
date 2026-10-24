//! Gold (`manage_gold` and `_spare_units`, basic.py:1591-1643, 1739-1766): a defender bought
//! for a city in danger with nobody in it; spare units disbanded and the biggest city on the gold
//! focus in a deficit that would empty the treasury, the focus released once gold flows again;
//! units upgraded with what is left over a reserve; the cheapest-to-finish production bought
//! where it pays; gifts to city-states, with city-states the bot's
//! (`diplomacy::city_states`, 1640-1641). Faith follows (`faith.rs`).
//!
//! A fix: Python's danger purchase bought the defender `manage_cities` had just queued and
//! nothing refilled the emptied queue that turn (a purchase of the queue's head refilled it only
//! further down, basic.py:1638-1639), so the city's production went nowhere until the bot's next
//! turn. Both purchases refill the queue here.

use citar_engine::base::ids::{CityId, PlayerId, UnitId};
use citar_engine::base::num;
use citar_engine::base::stats::Stat;
use citar_engine::game::advisor::{Advisor, Role};
use citar_engine::game::cities::purchase::purchase_check;
use citar_engine::game::cities::stats::current_construction;
use citar_engine::game::diplomacy::category::Category;
use citar_engine::game::units::actions::{UnitOrder, UpgradeUnit};
use citar_engine::game::units::upgrades::check_upgrade;
use citar_engine::game::{Action, Game, advisor, query};
use citar_engine::state::cities::{CityFocus, Constructible};

use super::Seat;
use super::cities::{by_threat, manage_cities, set_focus};
use super::context::{self, Context, breaks_space_reserve, in_danger};
use super::diplomacy::city_states::court_city_states;
use super::faith::{buy, spend_faith};
use crate::driver::Turn;

/// `manage_gold` (basic.py:1591-1643).
pub(crate) fn manage_gold(t: &mut Turn<'_>, s: &mut Seat<'_>, ctx: &Context) {
    let pid = t.pid();
    let p = s.params;
    // A defender for each city in danger with nobody in it, the most threatened first.
    for c in by_threat(ctx) {
        let g = t.game();
        if !in_danger(g, ctx, c, &s.advisor) {
            break;
        }
        let Some(at) = g.city(c).map(citar_engine::state::cities::City::tile) else { continue };
        if g.military_at(at).is_some() {
            continue;
        }
        let role = Role { prefer_ranged: true, ..Role::default() };
        if let Some(u) = Advisor::best_military(g, c, role, &s.advisor)
            && buy(t, c, Constructible::Unit(u), false)
        {
            refill(t, s);
        }
    }
    let gold = |g: &Game| g.player(pid).map_or(0.0, |x| x.econ.gold);
    if ctx.gpt < 0.0 && gold(t.game()) + ctx.gpt * f64::from(p.deficit_horizon) < 0.0 {
        let n = usize::try_from(num::trunc_i64(-ctx.gpt).max(1)).unwrap_or(1);
        for u in spare_units(t.game(), pid, s, ctx).into_iter().take(n) {
            t.act(Action::UnitOrder(UnitOrder {
                unit_id: i64::from(u.get()),
                order: "disband".to_owned(),
            }));
        }
        if ctx.gpt < -f64::from(p.gold_focus_deficit) {
            // The biggest, the first of equals (`max(cities, key=pop)`).
            let g = t.game();
            let mut big: Option<(CityId, u16)> = None;
            for &c in &ctx.cities {
                let Some(pop) = g.city(c).map(|x| x.pop) else { continue };
                if big.is_none_or(|(_, b)| pop > b) {
                    big = Some((c, pop));
                }
            }
            if let Some((c, _)) = big
                && g.city(c).is_some_and(|x| x.focus != CityFocus::Gold)
            {
                set_focus(t, c, CityFocus::Gold);
            }
        }
    } else if ctx.gpt > f64::from(p.gold_focus_release) {
        for &c in &ctx.cities {
            if t.game().city(c).is_some_and(|x| x.focus == CityFocus::Gold) {
                set_focus(t, c, CityFocus::Balanced);
            }
        }
    }
    let era = i32::try_from(ctx.era).unwrap_or(i32::MAX);
    let reserve =
        f64::from(p.gold_reserve.saturating_add(p.gold_reserve_per_era.saturating_mul(era)));
    for &u in &ctx.military {
        let g = t.game();
        let check = check_upgrade(g, u);
        let Some(target) = check.target.filter(|_| check.refusal.is_none()) else { continue };
        if gold(g) - f64::from(check.cost) > reserve
            && !breaks_space_reserve(g, pid, Constructible::Unit(target), ctx, &s.advisor)
        {
            t.act(Action::UpgradeUnit(UpgradeUnit { unit_id: i64::from(u.get()) }));
        }
    }
    // What the slowest cities build, bought where it pays: a settler, a worker or a building,
    // anything at war, never a wonder.
    let mut slowest: Vec<(CityId, f64)> =
        ctx.cities.iter().map(|&c| (c, query::city_stats(t.game(), c).production())).collect();
    slowest.sort_by(|a, b| a.1.total_cmp(&b.1));
    for (c, _) in slowest {
        let g = t.game();
        let Some(city) = g.city(c) else { continue };
        let Some(head) = current_construction(city) else { continue };
        if matches!(head, Constructible::Perpetual(_)) || city.puppet {
            continue;
        }
        let (reason, cost) = purchase_check(g, c, head, Stat::Gold);
        let Some(cost) = cost.filter(|&x| reason.is_none() && x != 0) else { continue };
        let a = &g.rules().derived().advisor;
        let (settler, worker, building, wonder) = match head {
            Constructible::Unit(u) => (a.founders.contains(u), a.workers.contains(u), false, false),
            Constructible::Building(b) => (false, false, true, g.rules().buildings()[b].is_wonder),
            Constructible::Perpetual(_) => (false, false, false, false),
        };
        let spare = gold(g) - reserve;
        let flat = p
            .buy_cap
            .saturating_add(p.buy_cap_per_era.saturating_mul(era))
            .saturating_add(if settler { p.buy_cap_settler } else { 0 });
        let cap = f64::from(flat).max(spare * p.buy_cap_spare_share);
        let cost = f64::from(cost);
        if cost <= spare
            && cost <= cap
            && (settler || worker || building || !ctx.wars.is_empty())
            && !wonder
            && buy(t, c, head, false)
        {
            refill(t, s);
        }
    }
    if !s.spec.owners.llm(Category::CityStates) {
        court_city_states(t, s, ctx);
    }
    if t.game().religion_enabled() {
        spend_faith(t, s, ctx);
    }
}

/// Refills the queues a purchase emptied: `manage_cities` afresh, with a new context
/// (`self.manage_cities(g, pid)`, basic.py:1639).
fn refill(t: &mut Turn<'_>, s: &mut Seat<'_>) {
    let ctx = context::build(t.game(), t.pid(), s);
    manage_cities(t, s, &ctx);
}

/// The units not committed to defence or a war, the least valuable first (`_spare_units`,
/// basic.py:1739-1766): military units outside the threatened cities, scouts worth nothing, an
/// obsolete unit less, a city's garrison more; at least `keep_units_per_city` per city (one at
/// the least) of the others are kept back.
pub(crate) fn spare_units(g: &Game, pid: PlayerId, s: &Seat<'_>, ctx: &Context) -> Vec<UnitId> {
    let p = s.params;
    let r = g.rules();
    let mut out: Vec<(f64, UnitId, bool)> = Vec::new();
    for &u in &ctx.units {
        let Some(x) = g.unit(u) else { continue };
        let d = &r.base_units()[x.base];
        if !d.military {
            continue;
        }
        let ours = g.city_at(x.tile()).filter(|c| c.owner() == pid);
        if ours.is_some_and(|c| ctx.threat(c.id()) > 0.0) {
            continue;
        }
        let recon = !advisor::is_army(g, x.base);
        let mut value = if recon { 0.0 } else { advisor::power(d) };
        if d.obsolete_tech.is_some_and(|o| g.has_tech(pid, Some(o))) {
            value *= p.spare_obsolete_mult;
        }
        if ours.is_some() && g.military_at(x.tile()).is_some_and(|m| m.id() == u) {
            value += f64::from(p.spare_garrison_value);
        }
        out.push((value, u, recon));
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    let keep = i64::try_from(ctx.cities.len())
        .unwrap_or(i64::MAX)
        .saturating_mul(i64::from(p.keep_units_per_city))
        .max(1);
    let mut left = i64::try_from(out.iter().filter(|x| !x.2).count()).unwrap_or(i64::MAX);
    let mut result = Vec::new();
    for (_, u, recon) in out {
        if !recon {
            if left <= keep {
                break;
            }
            left -= 1;
        }
        result.push(u);
    }
    result
}
