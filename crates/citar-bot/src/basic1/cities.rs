//! The cities (`manage_cities`, `_manage_city_tiles` and `city_bombard`, basic.py:1152-1202,
//! 1577-1587): production through the engine's advisor, a city's focus and its growth while the
//! empire is unhappy, and every city's free shot.
//!
//! Production is the advisor's (`game::advisor`, DESIGN.md P2.3.7), asked with what the bot
//! remembers ([`super::facts`]): one `Advisor` for the turn's cities, told what each started so
//! the next sees it, as `manage_cities` counted them. The advisor reads only, so the bot keeps
//! its own records: the turn a city queued a work boat (`memory.boat_turns`), and the waiting
//! settler's escort, cleared once the city it waits in picks a military unit (Python's
//! production popped `_need_escort` itself, basic.py:1249-1251, 1415-1416).

use citar_engine::api::views::alerts::bombard_targets;
use citar_engine::base::ids::CityId;
use citar_engine::base::num;
use citar_engine::game::cities::citizens::SetCityFocus;
use citar_engine::game::cities::construction::item_name;
use citar_engine::game::cities::queue::SetProduction;
use citar_engine::game::cities::stats::is_capital;
use citar_engine::game::combat::actions::CityAttack;
use citar_engine::game::{Action, Game};
use citar_engine::state::cities::{CityFocus, Constructible};
use serde_json::json;

use super::Seat;
use super::context::{Context, in_danger};
use crate::driver::Turn;
use crate::params::SmallCityFocus;

/// `manage_cities` (basic.py:1152-1180): the cities most threatened first, puppets aside; a city
/// with something queued keeps it, unless it is in danger and that is no military unit; the
/// others start what the advisor picks, then set their focus.
pub(crate) fn manage_cities(t: &mut Turn<'_>, s: &mut Seat<'_>, ctx: &Context) {
    let pid = t.pid();
    let mut adv = super::advisor(t.game(), pid, s);
    for c in by_threat(ctx) {
        let g = t.game();
        let Some(city) = g.city(c) else { continue };
        if city.puppet {
            continue;
        }
        let at = city.tile();
        let danger = in_danger(g, ctx, c, &s.advisor);
        if let Some(&head) = city.queue.first() {
            let military = is_military(g, head);
            if !(danger && !military) {
                continue;
            }
        }
        if let Some(item) = adv.advise(g, c) {
            if s.memory.need_escort == Some(at) && is_military(g, item) {
                s.memory.need_escort = None;
            }
            if set_production(t, c, item) {
                let g = t.game();
                adv.started(g, item);
                if let Constructible::Unit(u) = item
                    && g.rules().derived().advisor.boats.contains(u)
                {
                    s.memory.boat_turns.insert(c, g.turn());
                }
            }
        }
        manage_city_tiles(t, s, ctx, c);
    }
}

/// The cities, the most threatened first, equals in the context's order (Python's stable sort
/// on `-threat`).
pub(crate) fn by_threat(ctx: &Context) -> Vec<CityId> {
    let mut order: Vec<(CityId, f64)> =
        ctx.cities.iter().copied().zip(ctx.threat.iter().copied()).collect();
    order.sort_by(|a, b| b.1.total_cmp(&a.1));
    order.into_iter().map(|(c, _)| c).collect()
}

/// Whether an item is a military unit.
fn is_military(g: &Game, item: Constructible) -> bool {
    matches!(item, Constructible::Unit(u) if g.rules().base_units()[u].military)
}

/// Sets city `c` to build `item`: whether the rules took it.
fn set_production(t: &mut Turn<'_>, c: CityId, item: Constructible) -> bool {
    let name = item_name(t.game().rules(), item).to_owned();
    t.act(Action::SetProduction(SetProduction {
        city_id: i64::from(c.get()),
        item: json!(name),
        append: None,
    }))
    .is_some()
}

/// A city's focus and its growth (`_manage_city_tiles`, basic.py:1182-1202): while the empire is
/// unhappy, with `unhappy_avoid_growth`, its biggest cities stop growing until happiness is back
/// to `avoid_growth_release`; a small city other than the capital takes `small_city_focus`, any
/// other the balanced one; a city on the gold focus keeps it, since `manage_gold` sets and
/// clears that.
fn manage_city_tiles(t: &mut Turn<'_>, s: &Seat<'_>, ctx: &Context, c: CityId) {
    let p = s.params;
    let g = t.game();
    let Some(city) = g.city(c) else { return };
    if city.razing || city.puppet {
        return;
    }
    let pop = i32::from(city.pop);
    if p.unhappy_avoid_growth {
        let mut big: Vec<(CityId, u16)> =
            ctx.cities.iter().filter_map(|&x| g.city(x).map(|y| (x, y.pop))).collect();
        big.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
        let n = i32::try_from(ctx.cities.len()).unwrap_or(i32::MAX);
        let k = usize::try_from(num::floor_div(n, p.avoid_growth_div.max(1)).max(1)).unwrap_or(1);
        let stop = ctx.hap < 0
            && big.iter().take(k).any(|&(x, _)| x == c)
            && pop >= p.avoid_growth_min_pop;
        if stop != city.avoid_growth && (stop || ctx.hap >= p.avoid_growth_release) {
            t.act(Action::SetCityFocus(SetCityFocus {
                city_id: i64::from(c.get()),
                focus: None,
                avoid_growth: Some(json!(stop)),
            }));
        }
    }
    let g = t.game();
    let Some(city) = g.city(c) else { return };
    let small = p.small_city_focus.filter(|_| pop < p.small_city_pop && !is_capital(g, c));
    let want = small.map_or(CityFocus::Balanced, |f| match f {
        SmallCityFocus::Food => CityFocus::Food,
        SmallCityFocus::Production => CityFocus::Production,
        SmallCityFocus::Gold => CityFocus::Gold,
        SmallCityFocus::Science => CityFocus::Science,
    });
    if city.focus == CityFocus::Gold || city.focus == want {
        return;
    }
    set_focus(t, c, want);
}

/// Sets city `c`'s focus.
pub(crate) fn set_focus(t: &mut Turn<'_>, c: CityId, focus: CityFocus) {
    t.act(Action::SetCityFocus(SetCityFocus {
        city_id: i64::from(c.get()),
        focus: Some(json!(focus.name())),
        avoid_growth: None,
    }));
}

/// `city_bombard` (basic.py:1577-1587): every city that can still shoot fires at its best
/// target, the one it would kill, else the one it would hurt most (`briefing.bombard_targets`).
pub(crate) fn city_bombard(t: &mut Turn<'_>) {
    let shots = bombard_targets(t.game(), t.pid());
    for b in shots {
        let (x, y) = t.game().xy(b.target);
        t.act(Action::CityAttack(CityAttack {
            city_id: i64::from(b.city.get()),
            x: i64::from(x),
            y: i64::from(y),
        }));
    }
}
