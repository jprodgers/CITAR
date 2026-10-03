//! Faith (`choose_beliefs` and `_spend_faith`, basic.py:1696-1737): the beliefs a religion is
//! founded or enhanced with, and what faith buys once there is a religion: the buildings the
//! beliefs allow (a Pagoda for Pagodas) before missionaries, one purchase a turn.
//!
//! [`choose_beliefs`] is asked when a great prophet founds or enhances a religion, which package
//! 2-03 ports (`basic1/units/special.rs`); `belief_mode = "unciv"` leaves the choice to the
//! engine's AI (`religion::found::ai_choose_beliefs`).

use std::collections::BTreeMap;

use citar_engine::base::ids::{BeliefId, PlayerId};
use citar_engine::base::stats::Stat;
use citar_engine::game::cities::purchase::{Buy, purchase_check};
use citar_engine::game::religion::found::{BeliefCounts, ai_choose_beliefs};
use citar_engine::game::religion::{beliefs_available, beliefs_taken};
use citar_engine::game::units::type_has;
use citar_engine::game::{Action, Game};
use citar_engine::rules::defs::{BeliefKind, BeliefType, ReligionProgress};
use citar_engine::state::cities::Constructible;
use citar_engine::unique::UniqueType;
use serde_json::json;

use super::Seat;
use super::context::Context;
use crate::driver::Turn;
use crate::params::BeliefMode;
use crate::params::resolve::names_of;

/// The beliefs to found or enhance a religion with, `needed` of each kind (`choose_beliefs`,
/// basic.py:1696-1710). In the prefs mode, the untaken beliefs of each kind by their place in
/// the kind's order, unranked ones last, then by name; a kind whose order is written empty is
/// ranked by the four orders one after another (pantheon, founder, follower, enhancer), the
/// first place of a belief counting.
#[expect(dead_code, reason = "package 2-03's great prophets found and enhance religions with it")]
pub(crate) fn choose_beliefs(
    g: &Game,
    pid: PlayerId,
    s: &Seat<'_>,
    needed: &BeliefCounts,
) -> Vec<BeliefId> {
    if s.params.belief_mode != BeliefMode::Prefs {
        return ai_choose_beliefs(g, pid, needed);
    }
    let p = s.params;
    let kinds = [
        (BeliefType::Pantheon, "beliefs_pantheon", &p.beliefs_pantheon),
        (BeliefType::Founder, "beliefs_founder", &p.beliefs_founder),
        (BeliefType::Follower, "beliefs_follower", &p.beliefs_follower),
        (BeliefType::Enhancer, "beliefs_enhancer", &p.beliefs_enhancer),
    ];
    // Each order as written, names the ruleset lacks keeping their places (Python's `index`
    // counted them).
    let written: Vec<(BeliefType, Vec<String>)> =
        kinds.iter().map(|&(t, key, list)| (t, names_of(key, list))).collect();
    let rank_of = |names: &[String]| -> BTreeMap<BeliefId, usize> {
        let mut out = BTreeMap::new();
        for (i, n) in names.iter().enumerate() {
            if let Some(b) = g.rules().lookup::<BeliefId>(n) {
                out.entry(b).or_insert(i);
            }
        }
        out
    };
    let all: Vec<String> = written.iter().flat_map(|(_, n)| n.iter().cloned()).collect();
    let taken = beliefs_taken(g);
    let mut out: Vec<BeliefId> = Vec::new();
    for &(kind, n) in needed {
        let own = match kind {
            BeliefKind::Type(t) => written.iter().find(|(x, _)| *x == t).map(|(_, v)| v),
            BeliefKind::Any => None,
        };
        let rank = match own {
            Some(names) if !names.is_empty() => rank_of(names),
            _ => rank_of(&all),
        };
        let mut pool: Vec<BeliefId> = beliefs_available(g, kind)
            .into_iter()
            .filter(|b| !out.contains(b) && !taken.contains(*b))
            .collect();
        pool.sort_by_key(|b| (rank.get(b).copied().unwrap_or(99), s.resolved.belief_rank(*b)));
        out.extend(pool.into_iter().take(usize::try_from(n).unwrap_or(0)));
    }
    out
}

/// `_spend_faith` (basic.py:1712-1737): with a religion and at least `faith_min` faith, a
/// building only faith buys (with `faith_buildings`), in the biggest city first, keeping
/// `faith_building_keep`; else a missionary while there are fewer than `max_missionaries`,
/// keeping `faith_missionary_keep`. One purchase, or none.
pub(crate) fn spend_faith(t: &mut Turn<'_>, s: &Seat<'_>, ctx: &Context) {
    let pid = t.pid();
    let p = s.params;
    let Some(pl) = t.game().player(pid) else { return };
    if pl.religion.progress < ReligionProgress::Religion || pl.econ.faith < f64::from(p.faith_min) {
        return;
    }
    let faith = |g: &Game| g.player(pid).map_or(0.0, |x| x.econ.faith);
    if p.faith_buildings {
        let mut cities = ctx.cities.clone();
        cities.sort_by_key(|&c| std::cmp::Reverse(t.game().city(c).map_or(0, |x| x.pop)));
        for c in cities {
            let buildings: Vec<_> = t.game().rules().buildings().ids().collect();
            for b in buildings {
                let g = t.game();
                let bd = &g.rules().buildings()[b];
                let has = g.city(c).is_some_and(|x| x.buildings.contains(b));
                if bd.cost != 0 || has || bd.is_wonder {
                    continue;
                }
                let item = Constructible::Building(b);
                let (reason, cost) = purchase_check(g, c, item, Stat::Faith);
                let Some(cost) = cost.filter(|&x| reason.is_none() && x != 0) else { continue };
                if faith(g) - f64::from(cost) > f64::from(p.faith_building_keep)
                    && buy(t, c, item, true)
                {
                    return;
                }
            }
        }
    }
    for &c in &ctx.cities {
        let units: Vec<_> = t.game().rules().base_units().ids().collect();
        for u in units {
            let g = t.game();
            if !type_has(g, u, UniqueType::CanSpreadReligion)
                || type_has(g, u, UniqueType::MayFoundReligion)
            {
                continue;
            }
            let item = Constructible::Unit(u);
            let (reason, cost) = purchase_check(g, c, item, Stat::Faith);
            let Some(cost) = cost.filter(|&x| reason.is_none() && x != 0) else { continue };
            if faith(g) - f64::from(cost) > f64::from(p.faith_missionary_keep) {
                let have = ctx.units.iter().filter(|&&x| g.unit(x).is_some_and(|y| y.base == u));
                if i32::try_from(have.count()).unwrap_or(i32::MAX) < p.max_missionaries {
                    buy(t, c, item, true);
                }
                return;
            }
        }
    }
}

/// Buys `item` in city `c` with faith, or gold: whether it was bought.
pub(crate) fn buy(
    t: &mut Turn<'_>,
    c: citar_engine::base::ids::CityId,
    item: Constructible,
    faith: bool,
) -> bool {
    let name =
        citar_engine::game::cities::construction::item_name(t.game().rules(), item).to_owned();
    t.act(Action::Buy(Buy {
        city_id: i64::from(c.get()),
        item: json!(name),
        currency: faith.then(|| json!("Faith")),
    }))
    .is_some()
}
