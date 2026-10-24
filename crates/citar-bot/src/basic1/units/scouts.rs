//! Scouts (`handle_scout`, basic.py:2054-2063): keep exploring; a scout with nothing left to
//! explore sleeps, or is disbanded at the unit supply or past `scout_disband_turn`.

use citar_engine::base::ids::UnitId;
use citar_engine::state::units::Activity;

use crate::basic1::Seat;
use crate::basic1::context::Context;
use crate::basic1::workers::order;
use crate::driver::Turn;

/// `handle_scout` (basic.py:2054-2063).
pub(crate) fn handle_scout(t: &mut Turn<'_>, s: &Seat<'_>, ctx: &Context, u: UnitId) {
    if t.game().unit(u).is_some_and(|x| x.activity == Some(Activity::Explore)) {
        return;
    }
    let exploring = order(t, u, "explore")
        .and_then(|r| r.get("exploring").and_then(serde_json::Value::as_bool))
        .unwrap_or(false);
    if exploring {
        return;
    }
    let at_supply = i64::try_from(ctx.units.len()).unwrap_or(i64::MAX) >= i64::from(ctx.supply);
    if at_supply || t.game().turn() > s.params.scout_disband_turn {
        order(t, u, "disband");
    } else {
        order(t, u, "sleep");
    }
}
