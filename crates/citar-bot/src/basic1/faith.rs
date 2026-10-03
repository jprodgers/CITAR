//! Faith (`choose_beliefs` and `_spend_faith`, basic.py:1696-1737): the beliefs a religion is
//! founded or enhanced with, and what faith buys once there is a religion: the buildings the
//! beliefs allow (a Pagoda for Pagodas) before missionaries, one purchase a turn.
//!
//! [`choose_beliefs`] is asked when a great prophet founds or enhances a religion, which package
//! 2-03 ports (`basic1/units/special.rs`); `belief_mode = "unciv"` leaves the choice to the
//! engine's AI (`religion::found::ai_choose_beliefs`).

use citar_engine::base::ids::{BeliefId, PlayerId};
use citar_engine::base::stats::Stat;
use citar_engine::game::cities::purchase::{Buy, purchase_check};
use citar_engine::game::religion::found::{BeliefCounts, ai_choose_beliefs};
use citar_engine::game::religion::{beliefs_available, beliefs_taken};
use citar_engine::game::units::type_has;
use citar_engine::game::{Action, Game};
use citar_engine::rules::defs::ReligionProgress;
use citar_engine::state::cities::Constructible;
use citar_engine::unique::UniqueType;
use serde_json::json;

use super::Seat;
use super::context::Context;
use crate::driver::Turn;
use crate::params::BeliefMode;

/// The beliefs to found or enhance a religion with, `needed` of each kind (`choose_beliefs`,
/// basic.py:1696-1710). In the prefs mode, the untaken beliefs of each kind by their place in
/// the kind's order, unranked ones last, then by name; a kind whose order is written empty is
/// ranked by the four orders one after another (pantheon, founder, follower, enhancer), the
/// first place of a belief counting. The places are worked out once per ruleset
/// ([`Resolved::belief_place`](crate::params::Resolved::belief_place)), so the choice compares
/// ids only.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "package 2-03's great prophets found and enhance religions with it"
    )
)]
pub(crate) fn choose_beliefs(
    g: &Game,
    pid: PlayerId,
    s: &Seat<'_>,
    needed: &BeliefCounts,
) -> Vec<BeliefId> {
    if s.params.belief_mode != BeliefMode::Prefs {
        return ai_choose_beliefs(g, pid, needed);
    }
    let r = s.resolved;
    let taken = beliefs_taken(g);
    let mut out: Vec<BeliefId> = Vec::new();
    for &(kind, n) in needed {
        let mut pool: Vec<BeliefId> = beliefs_available(g, kind)
            .into_iter()
            .filter(|b| !out.contains(b) && !taken.contains(*b))
            .collect();
        // Python's `rank.index(b) if b in rank else 99`, then the name.
        pool.sort_by_key(|&b| (r.belief_place(kind, b).unwrap_or(99), r.belief_rank(b)));
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use citar_engine::base::ids::PlayerId;
    use citar_engine::game::setup::config_from_value;
    use citar_engine::rules::Ruleset;
    use citar_engine::rules::defs::{BeliefKind, BeliefType};
    use serde_json::{Value, json};

    use super::*;
    use crate::memory::Memory;
    use crate::params::{Overrides, Tuning};
    use crate::versions::VersionId;
    use crate::{BotSpec, clean};

    fn game() -> Game {
        let r = Ruleset::shared();
        let cfg = json!({"seed": 3, "map_size": "duel", "players": [{}, {}]});
        Game::new(r, &config_from_value(r, cfg).expect("settings")).expect("a game").0
    }

    fn spec(params: &Value) -> BotSpec {
        let o = if params.is_null() {
            Overrides::default()
        } else {
            clean("basic-1", params).expect("parameters")
        };
        BotSpec::new(VersionId::Basic1, Arc::new(Tuning::new(VersionId::Basic1, o)), None, None)
    }

    /// The beliefs `params`' bot chooses for one founder and one follower belief.
    fn choose(g: &Game, params: &Value) -> Vec<BeliefId> {
        let spec = spec(params);
        let resolved = spec.tuning.resolved(g.rules());
        let mut memory = Memory::default();
        let seat = Seat::new(&spec, &resolved, &mut memory);
        let needed: BeliefCounts = [
            (BeliefKind::Type(BeliefType::Founder), 1),
            (BeliefKind::Type(BeliefType::Follower), 1),
        ]
        .into_iter()
        .collect();
        choose_beliefs(g, PlayerId(0), &seat, &needed)
    }

    fn named(g: &Game, names: &[&str]) -> Vec<BeliefId> {
        names.iter().map(|n| g.rules().lookup::<BeliefId>(n).expect(n)).collect()
    }

    #[test]
    fn beliefs_are_chosen_by_their_order_else_by_the_four_orders_else_by_name() {
        let g = game();
        // The orders as given, the first of each.
        let mine = json!({"beliefs_founder": ["Tithe", "Pilgrimage"],
                          "beliefs_follower": ["Pagodas", "Mosques"]});
        assert_eq!(choose(&g, &mine), named(&g, &["Tithe", "Pagodas"]));
        // Names the order lacks come after it, by name; and a founder order written empty ranks
        // by the four orders one after another, which name no founder belief: all by name.
        let by_name = |g: &Game| {
            let mut founders: Vec<(String, BeliefId)> =
                beliefs_available(g, BeliefKind::Type(BeliefType::Founder))
                    .into_iter()
                    .map(|b| (g.rules().name(b).unwrap_or_default().to_owned(), b))
                    .collect();
            founders.sort();
            founders[0].1
        };
        let one = json!({"beliefs_founder": ["Tithe"], "beliefs_follower": ["Pagodas"]});
        assert_eq!(choose(&g, &one)[0], named(&g, &["Tithe"])[0]);
        assert_ne!(by_name(&g), named(&g, &["Tithe"])[0], "the order comes before the names");
        let empty = json!({"beliefs_founder": [], "beliefs_follower": ["Pagodas"]});
        assert_eq!(choose(&g, &empty), vec![by_name(&g), named(&g, &["Pagodas"])[0]]);
        // The unciv mode is the engine's AI.
        let unciv = json!({"belief_mode": "unciv"});
        let needed: BeliefCounts = [
            (BeliefKind::Type(BeliefType::Founder), 1),
            (BeliefKind::Type(BeliefType::Follower), 1),
        ]
        .into_iter()
        .collect();
        assert_eq!(choose(&g, &unciv), ai_choose_beliefs(&g, PlayerId(0), &needed));
    }
}
