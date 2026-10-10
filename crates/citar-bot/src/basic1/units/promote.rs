//! Promotions (`_promote`, basic.py:1806-1822): a unit that has earned a promotion takes one,
//! up to five in a turn. In one of its own cities it takes the first of `promo_in_city` it may,
//! elsewhere the first of `promo_lines`, else the first by name; Python matched the names by
//! prefix, which `Resolved` worked out once per ruleset as two promotion sets.
//!
//! A fix: Python offered a unit whose only pick was a free promotion (`This Promotion is free`)
//! any promotion, and the engine refuses a paid one it has no experience for (the
//! `promotion-needs-its-experience` fix of package 1c-02), so such a unit never took the free
//! one. With neither the experience nor a free pick, only the free promotions are offered.

use citar_engine::base::ids::{PromotionId, UnitId};
use citar_engine::game::Action;
use citar_engine::game::units::actions::PromoteUnit;
use citar_engine::game::units::promotions::{
    available_promotions, can_promote, is_free, xp_for_next,
};

use crate::basic1::Seat;
use crate::driver::Turn;

/// The most promotions a unit takes in one turn (Python's `guard`).
const MOST: usize = 5;

/// `_promote` (basic.py:1806-1822).
pub(crate) fn promote(t: &mut Turn<'_>, s: &Seat<'_>, u: UnitId) {
    let pid = t.pid();
    for _ in 0..MOST {
        let g = t.game();
        if !can_promote(g, u) {
            return;
        }
        let Some(x) = g.unit(u) else { return };
        let r = g.rules();
        let mut opts: Vec<PromotionId> = available_promotions(g, u);
        if x.xp < xp_for_next(g, u) && x.pending_promotions == 0 {
            opts.retain(|&p| is_free(g, p));
        }
        // Python sorted the names (`sorted`, by code point, which is UTF-8's byte order).
        opts.sort_by(|&a, &b| r.name(a).cmp(&r.name(b)));
        let Some(&first) = opts.first() else { return };
        let in_city = g.city_at(x.tile()).is_some_and(|c| c.owner() == pid);
        let res = s.resolved;
        let pick = opts
            .iter()
            .copied()
            .find(|&p| in_city && res.promo_in_city.contains(p))
            .or_else(|| opts.iter().copied().find(|&p| res.promo_lines.contains(p)))
            .unwrap_or(first);
        let promotion = r.name(pick).unwrap_or_default().to_owned();
        if t.act(Action::PromoteUnit(PromoteUnit { unit_id: i64::from(u.get()), promotion }))
            .is_none()
        {
            return;
        }
    }
}
