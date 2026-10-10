//! The empire's choices (`_policy_order` and `empire_choices`, basic.py:1003-1041): culture spent
//! on policies, a free great person, faith on a pantheon, and then the spies, with espionage the
//! bot's (`diplomacy::spies`, 1040-1064).
//!
//! Each choice is a function of the game that writes nothing ([`policy_choice`],
//! [`great_person_choice`], [`pantheon_choice`]), which the turn acts on and the reference
//! checks ask (`crate::decisions`) with what Python's recorder patched: whether the civilization
//! could afford a policy, held a free great person or could found a pantheon.
//!
//! What differs from Python: names are compared once, when the parameters are resolved for the
//! ruleset (`Resolved::policy_rank`), where Python compared the strings at every pick; the order
//! is the same.

use citar_engine::base::ids::{BeliefId, PlayerId, PolicyId};
use citar_engine::game::diplomacy::category::Category;
use citar_engine::game::great_people::ChooseGreatPerson;
use citar_engine::game::policies::{AdoptPolicy, adoptable_policies, branch_of, can_adopt_any};
use citar_engine::game::religion::beliefs_available;
use citar_engine::game::religion::found::{FoundPantheon, can_found_pantheon};
use citar_engine::game::{Action, Game};
use citar_engine::rules::defs::{BeliefKind, BeliefType};
use serde_json::json;

use super::Seat;
use super::context::Context;
use super::diplomacy::spies::spies;
use crate::driver::Turn;
use crate::params::BeliefMode;

/// `empire_choices` (basic.py:1011-1041).
pub(crate) fn empire_choices(t: &mut Turn<'_>, s: &Seat<'_>, ctx: &Context) {
    let pid = t.pid();
    for _ in 0..s.params.policy_picks_per_turn.max(0) {
        if !can_adopt_any(t.game(), pid) {
            break;
        }
        let Some(choice) = policy_choice(t.game(), pid, s) else { break };
        let name = t.game().rules().name(choice).unwrap_or_default().to_owned();
        if t.act(Action::AdoptPolicy(AdoptPolicy { policy: json!(name) })).is_none() {
            break;
        }
    }
    if t.game().player(pid).is_some_and(|p| p.gp.free > 0)
        && let (name, Some(_)) = great_person_choice(s, ctx)
    {
        t.act(Action::ChooseGreatPerson(ChooseGreatPerson { great_person: json!(name) }));
    }
    if t.game().religion_enabled()
        && can_found_pantheon(t.game(), pid).is_none()
        && let Some(b) = pantheon_choice(t.game(), s)
    {
        let name = t.game().rules().name(b).unwrap_or_default().to_owned();
        t.act(Action::FoundPantheon(FoundPantheon { belief: json!(name) }));
    }
    if t.game().espionage_enabled() && !s.spec.owners.llm(Category::Espionage) {
        spies(t);
    }
}

/// The policy branch order of the bot's temperament (`_policy_order`, basic.py:1003-1009): the
/// aggressive one above `aggressive_above`.
fn policy_order<'a>(s: &'a Seat<'_>) -> &'a [PolicyId] {
    if s.spec.aggression > s.params.aggressive_above {
        &s.resolved.policy_order_aggressive
    } else {
        &s.resolved.policy_order_peaceful
    }
}

/// The policy it would adopt among those adoptable now, whatever its culture (basic.py:1024-1029):
/// one of a branch it has opened first, then by its branch's place in the order (50 for a branch
/// the order leaves out), then by name. `None` when nothing is adoptable.
pub(crate) fn policy_choice(g: &Game, pid: PlayerId, s: &Seat<'_>) -> Option<PolicyId> {
    let adopted = &g.player(pid)?.policy.adopted;
    let order = policy_order(s);
    let rank = |q: PolicyId| {
        let br = branch_of(g, q);
        let open = u8::from(!adopted.contains(br));
        let place = order.iter().position(|&x| x == br).unwrap_or(50);
        (open, place, s.resolved.policy_rank(q))
    };
    adoptable_policies(g, pid).into_iter().min_by_key(|&q| rank(q))
}

/// The policy it adopts now: [`policy_choice`] when it can afford one.
pub(crate) fn policy_now(g: &Game, pid: PlayerId, s: &Seat<'_>) -> Option<PolicyId> {
    if can_adopt_any(g, pid) { policy_choice(g, pid, s) } else { None }
}

/// The great person a free one is taken as (basic.py:1031-1033): `free_gp_early` before
/// `free_gp_switch_era`, `free_gp_late` from it; by the parameter's name, with its unit in the
/// ruleset (`None` for a great person the ruleset lacks, which Python asked for and was
/// refused).
pub(crate) fn great_person_choice(
    s: &Seat<'_>,
    ctx: &Context,
) -> (&'static str, Option<citar_engine::base::ids::BaseUnitId>) {
    let switch = usize::try_from(s.params.free_gp_switch_era).unwrap_or(0);
    if ctx.era < switch {
        (s.params.free_gp_early.name(), s.resolved.free_gp_early)
    } else {
        (s.params.free_gp_late.name(), s.resolved.free_gp_late)
    }
}

/// The belief it would found a pantheon with (basic.py:1035-1037): the first of its order
/// (`beliefs_pantheon` in the prefs mode, `pantheon_unciv` in the unciv one) that nobody has
/// taken, else the first nobody has; `None` when every pantheon belief is taken.
pub(crate) fn pantheon_choice(g: &Game, s: &Seat<'_>) -> Option<BeliefId> {
    let avail = beliefs_available(g, BeliefKind::Type(BeliefType::Pantheon));
    let prefs = match s.params.belief_mode {
        BeliefMode::Prefs => &s.resolved.beliefs_pantheon,
        BeliefMode::Unciv => &s.resolved.pantheon_unciv,
    };
    prefs.iter().copied().find(|b| avail.contains(b)).or_else(|| avail.first().copied())
}
