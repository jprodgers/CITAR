//! Spies (`_spies`, basic.py:1043-1064), at the end of the empire's choices with espionage the
//! bot's (`Owners`) and in a game with espionage: each idle spy goes to the capital of the
//! civilization furthest ahead in technology among those met whose capital the seat has
//! explored and no spy of its own watches, the lead broken by a draw from the `Spies` stream
//! keyed by the spy and the civilization (DESIGN.md P2.3.5); with none, it guards the seat's own
//! capital if no spy does.

use citar_engine::base::ids::CityId;
use citar_engine::base::rng::KeyPart;
use citar_engine::game::Action;
use citar_engine::game::espionage::{MoveSpy, spies as spies_of};
use citar_engine::rules::defs::SpyAction;
use serde_json::json;

use crate::driver::Turn;
use crate::stream::Stream;

/// Sends the seat's idle spies (`_spies`, basic.py:1043-1064).
pub(crate) fn spies(t: &mut Turn<'_>) {
    let pid = t.pid();
    let mut taken: Vec<CityId> = spies_of(t.game(), pid).iter().filter_map(|s| s.city).collect();
    let n = spies_of(t.game(), pid).len();
    for i in 0..n {
        let g = t.game();
        let Some(spy) = spies_of(g, pid).get(i) else { break };
        if spy.action != SpyAction::None {
            continue;
        }
        let name = spy.name.to_string();
        let Some(me) = g.player(pid) else { return };
        let mut best: Option<(f64, CityId)> = None;
        for q in g.majors(true) {
            let qid = q.id();
            if qid == pid || !g.has_met(pid, qid) {
                continue;
            }
            let Some(cap) = q.capital.and_then(|c| g.city(c)) else { continue };
            if !me.explored.contains(cap.tile().0) || taken.contains(&cap.id()) {
                continue;
            }
            let lead = i64::try_from(q.tech.known.len()).unwrap_or(i64::MAX)
                - i64::try_from(me.tech.known.len()).unwrap_or(i64::MAX);
            #[allow(clippy::cast_precision_loss, reason = "a count of technologies")]
            let score = lead as f64
                + Stream::Spies
                    .rng(g, pid, &[u64::try_from(i).unwrap_or(u64::MAX), qid.key()])
                    .unit();
            // The highest score, then the highest city id, as Python's `max` of the pairs.
            let better = best
                .is_none_or(|(b, c)| score.total_cmp(&b).then_with(|| cap.id().cmp(&c)).is_gt());
            if better {
                best = Some((score, cap.id()));
            }
        }
        let target = best.map(|(_, c)| c).or_else(|| {
            me.capital.and_then(|c| g.city(c)).map(|c| c.id()).filter(|c| !taken.contains(c))
        });
        let Some(city) = target else { continue };
        let moved = t
            .act(Action::MoveSpy(MoveSpy { spy: json!(name), city_id: json!(city.get()) }))
            .is_some();
        if moved {
            taken.push(city);
        }
    }
}
