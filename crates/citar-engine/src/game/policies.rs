//! Social policies (`policies.py`, UnCiv's `PolicyManager`): what the next policy costs in
//! culture, which policies and branches a civilization may adopt, adopting one, and the culture a
//! turn brings (package 1b-07).
//!
//! - [`culture_cost`]: `25 + (3n)^2.01` for the `n`th policy bought, times the cities beyond the
//!   first, the uniques, a humanlike seat's difficulty and the speed, rounded down to a five
//!   (`policies.py:35-52`);
//! - [`adoptable`] and [`adoptable_policies`] (`policies.py:55-91`), [`can_adopt_any`]
//!   (`policies.py:94-101`);
//! - adopting (`policies.adopt`, `policies.py:110-152`): with a free policy or the culture it
//!   costs; a branch's last policy adopts its finisher for free, announced to everyone. What the
//!   policy gives at once and `upon adopting [policy]` are fired by package 1b-08;
//! - stage E3's culture (`policies.end_turn`, `policies.py:155-163`);
//! - the tool `adopt_policy` (`tools.py:862-869`) and the scenario operation's adoption without
//!   culture (`scenario._adopt_counted`, `scenario.py:222-237`).
//!
//! What differs from Python, on purpose: the scenario operation adopts a policy whose branch's era
//! the civilization has not reached, as its reference says it does, where Python's second attempt
//! checked the era again and failed (`scenario-adopt-policy-skips-the-era`).

use serde_json::{Value, json};

use super::action::{OutcomeSpec, Rule};
use super::derive::rev::PlayerTouch;
use super::error::{ActionError, ErrCode};
use super::{Game, Porting, pending};
use crate::base::ids::{EraId, PlayerId, PolicyId};
use crate::base::num;
use crate::base::py;
use crate::base::sets::PlayerSet;
use crate::rules::defs::PolicyKind;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::unique::params::PolicyOrBelief;
use crate::unique::{CondData, Ctx, UniqueData, UniqueType, uq};

/// The branch a policy belongs to; a branch is its own (`policies.branch_of`,
/// `policies.py:30-32`).
#[must_use]
pub fn branch_of(g: &Game, policy: PolicyId) -> PolicyId {
    match g.rules().policies()[policy].kind {
        PolicyKind::Member { branch, .. } => branch,
        PolicyKind::Branch { .. } => policy,
    }
}

/// The era a branch unlocks in.
fn branch_era(g: &Game, branch: PolicyId) -> EraId {
    match g.rules().policies()[branch].kind {
        PolicyKind::Branch { era, .. } => era,
        PolicyKind::Member { .. } => EraId(0),
    }
}

/// The policy that completes a branch, adopted for free once the rest of it is.
#[must_use]
pub fn finisher(g: &Game, branch: PolicyId) -> Option<PolicyId> {
    g.rules().policies().iter().find_map(|(id, d)| match d.kind {
        PolicyKind::Member { branch: b, finisher: true, .. } if b == branch => Some(id),
        _ => None,
    })
}

/// The culture a civilization's next policy costs (`policies.culture_cost`, `policies.py:35-52`),
/// or its `n`th if given: `25 + (3n)^2.01`, times `[n]% Culture cost of adopting new Policies`
/// for each, a humanlike seat's difficulty and the speed, and more for each city beyond the first
/// that is not a puppet (less with `Each city founded increases culture cost of policies [n]%
/// less than normal`); rounded, then down to a multiple of five.
#[must_use]
pub fn culture_cost(g: &Game, p: PlayerId, n: Option<i32>) -> i32 {
    let r = g.rules();
    let Some(pl) = g.player(p) else { return 0 };
    let n = n.unwrap_or(pl.policy.adopted_count);
    let mut cost = 25.0 + num::pow(f64::from(n) * 3.0, 2.01);
    let map = g.state().map();
    let pre = r.constants().map_size_predefined(map.width, map.height);
    let cities = g.player_cities(p).filter(|c| !c.puppet).count();
    let cities = f64::from(u32::try_from(cities).unwrap_or(u32::MAX));
    let mut city_mod = pre.policy_cost_per_city * (cities - 1.0);
    let v = g.view();
    let ctx = Ctx::civ(p);
    for h in uq::civ(&v, p, UniqueType::LessPolicyCostFromCities, &ctx) {
        if let UniqueData::LessPolicyCostFromCities(x) = h.data() {
            for _ in 0..h.n {
                city_mod *= 1.0 - f64::from(x.percent) / 100.0;
            }
        }
    }
    for h in uq::civ(&v, p, UniqueType::LessPolicyCost, &ctx) {
        if let UniqueData::LessPolicyCost(x) = h.data() {
            for _ in 0..h.n {
                cost *= 1.0 + f64::from(x.percent) / 100.0;
            }
        }
    }
    if g.is_humanlike(p) {
        cost *= r.difficulties()[g.difficulty(Some(p))].policy_cost_modifier;
    }
    cost *= g.speed().culture_cost_modifier;
    let c = num::round_half_even_i32(cost * (1.0 + city_mod));
    c - c.rem_euclid(5)
}

/// Why a civilization cannot adopt a policy or open a branch now, whatever it costs; `None` if it
/// can (`policies.adoptable`, `policies.py:55-81`). `check_era` false skips the branch's era, as
/// the scenario operation does.
#[must_use]
pub fn adoptable(g: &Game, p: PlayerId, policy: PolicyId, check_era: bool) -> Option<String> {
    let r = g.rules();
    let d = &r.policies()[policy];
    let Some(pl) = g.player(p) else { return Some("No such player.".into()) };
    let adopted = &pl.policy.adopted;
    if adopted.contains(policy) {
        return Some(format!("{} is already adopted.", d.name));
    }
    let requires: &[PolicyId] = match &d.kind {
        PolicyKind::Member { finisher: true, .. } => {
            return Some(
                "Branch finishers are gained automatically when a branch is complete.".into(),
            );
        }
        PolicyKind::Member { requires, .. } => requires,
        PolicyKind::Branch { .. } => &[],
    };
    let missing: Vec<&str> =
        requires.iter().filter(|&&q| !adopted.contains(q)).filter_map(|&q| r.name(q)).collect();
    if !missing.is_empty() {
        return Some(format!("{} requires {}.", d.name, missing.join(", ")));
    }
    let branch = branch_of(g, policy);
    let era = branch_era(g, branch);
    if check_era && era > super::derive::civ::era(g, p) {
        return Some(format!(
            "The {} branch unlocks in the {}.",
            r.policies()[branch].name,
            r.eras()[era].name
        ));
    }
    let t = r.uniques();
    let v = g.view();
    let ctx = Ctx::civ(p);
    for id in d.uniques.ids() {
        if t.meta(id).ty != Some(UniqueType::OnlyAvailable) || crate::unique::applies(id, &ctx, &v)
        {
            continue;
        }
        let blocked: Vec<&str> = t
            .conds(t.get(id))
            .iter()
            .filter_map(|c| match c.data {
                CondData::ConditionalBeforePolicyOrBelief(x) => match x.adopted {
                    PolicyOrBelief::Policy(q) => r.name(q),
                    PolicyOrBelief::Belief(b) => r.name(b),
                },
                _ => None,
            })
            .collect();
        return Some(if blocked.is_empty() {
            format!("{} is not available.", d.name)
        } else {
            format!("{} is not available after adopting {}.", d.name, blocked.join(", "))
        });
    }
    if uq::any(uq::object(&v, &d.uniques, UniqueType::Unavailable, &ctx)) {
        return Some(format!("{} is unavailable.", d.name));
    }
    None
}

/// The branches, then the policies, a civilization could adopt now (`policies.adoptable_policies`,
/// `policies.py:84-91`).
#[must_use]
pub fn adoptable_policies(g: &Game, p: PlayerId) -> Vec<PolicyId> {
    let r = g.rules();
    let branches = r.policies().iter().filter(|(_, d)| d.is_branch());
    let members = r.policies().iter().filter(|(_, d)| !d.is_branch());
    branches
        .chain(members)
        .map(|(id, _)| id)
        .filter(|&q| adoptable(g, p, q, true).is_none())
        .collect()
}

/// Whether a major civilization could adopt anything now: a free policy or the culture for the
/// next, and something adoptable (`policies.can_adopt_any`, `policies.py:94-101`).
#[must_use]
pub fn can_adopt_any(g: &Game, p: PlayerId) -> bool {
    let Some(pl) = g.player(p).filter(|x| x.is_major()) else { return false };
    if pl.policy.free_policies == 0 && pl.econ.culture < f64::from(culture_cost(g, p, None)) {
        return false;
    }
    !adoptable_policies(g, p).is_empty()
}

/// How a policy is paid for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Payment {
    /// With one of the civilization's free policies.
    Free,
    /// With this much culture, and it counts toward the next policy's cost.
    Culture(i32),
    /// Given by a scenario, counted toward the next policy's cost.
    Counted,
}

/// Whether a civilization may adopt a policy now, and how it pays (`policies.adopt`'s checks,
/// `policies.py:115-130`). Reads only.
///
/// # Errors
/// [`adoptable`]'s reason, or not enough culture.
pub fn plan_adopt(g: &Game, p: PlayerId, policy: PolicyId) -> Result<Payment, ActionError> {
    if let Some(reason) = adoptable(g, p, policy, true) {
        return Err(ActionError::rule(reason));
    }
    let Some(pl) = g.player(p) else { return Err(ActionError::rule("No such player.")) };
    if pl.policy.free_policies > 0 {
        return Ok(Payment::Free);
    }
    let cost = culture_cost(g, p, None);
    if pl.econ.culture < f64::from(cost) {
        return Err(ActionError::rule(format!(
            "Adopting a policy costs {cost} culture; you have {}.",
            num::trunc_i64(pl.econ.culture)
        )));
    }
    Ok(Payment::Culture(cost))
}

/// Adopts a policy [`plan_adopt`] allowed: pays for it, then [`adopt_now`].
pub fn apply_adopt(g: &mut Game, p: PlayerId, policy: PolicyId, pay: Payment) {
    if let Some(pl) = g.player_mut(p, PlayerTouch::STOCKS | PlayerTouch::OTHER) {
        match pay {
            Payment::Free => pl.policy.free_policies -= 1,
            Payment::Culture(cost) => {
                pl.econ.culture -= f64::from(cost);
                pl.policy.adopted_count += 1;
            }
            Payment::Counted => pl.policy.adopted_count += 1,
        }
    }
    adopt_now(g, p, policy, false);
}

/// The civilization adopts a policy it has paid for (`policies.adopt`, `policies.py:131-152`):
/// a branch's last policy also adopts its finisher, free and announced to everyone; the free
/// buildings owed are handed out, and every city looks at its citizens again.
pub fn adopt_now(g: &mut Game, p: PlayerId, policy: PolicyId, completion: bool) {
    let r = g.rules();
    if let Some(pl) = g.player_mut(p, PlayerTouch::POLICIES) {
        pl.policy.adopted.insert(policy);
    }
    if !completion {
        let branch = branch_of(g, policy);
        let members: &[PolicyId] = match &r.policies()[branch].kind {
            PolicyKind::Branch { members, .. } => members,
            PolicyKind::Member { .. } => &[],
        };
        let adopted = g.player(p).map(|x| x.policy.adopted).unwrap_or_default();
        if let Some(fin) = finisher(g, branch)
            && members.iter().all(|&m| adopted.contains(m))
            && !adopted.contains(fin)
        {
            adopt_now(g, p, fin, true);
        }
    }
    // What the policy gives at once, and `upon adopting [policy]` (policies.py:139-145).
    pending(Porting::Pending("1b-08"));
    super::cities::free_buildings::try_add_free_buildings(g, p);
    g.flag_cities_of(p);
    let who = g.player(p).map(|x| x.name.clone()).unwrap_or_default();
    let audience = if completion { None } else { Some(PlayerSet::single(p)) };
    let data = EventData { policy: Some(policy), player: Some(p), ..EventData::default() };
    let text = format!("{who} adopted {}.", r.policies()[policy].name);
    g.emit(EngineEvent::Policy, &text, audience, None, data, &[]);
}

/// What adopting reports (`policies.py:151-152`), read from the settled game.
#[must_use]
pub fn adopt_result(g: &Game, p: PlayerId, policy: PolicyId) -> Value {
    let pl = g.player(p);
    json!({
        "adopted": g.rules().name(policy),
        "culture_left": pl.map_or(0, |x| num::trunc_i64(x.econ.culture)),
        "next_cost": culture_cost(g, p, None),
        "free_policies": pl.map_or(0, |x| x.policy.free_policies),
    })
}

/// A scenario adopts a policy without culture, whatever the era, and counts it toward the next
/// policy's cost (`scenario._adopt_counted`, `scenario.py:222-237`).
///
/// # Errors
/// [`adoptable`]'s reason, the era aside.
pub fn adopt_counted(g: &mut Game, p: PlayerId, policy: PolicyId) -> Result<(), ActionError> {
    if let Some(reason) = adoptable(g, p, policy, false) {
        return Err(ActionError::rule(reason));
    }
    // refcheck: scenario-adopt-policy-skips-the-era
    apply_adopt(g, p, policy, Payment::Counted);
    Ok(())
}

/// Stage E3, culture and policies (`policies.end_turn`, `policies.py:155-163`): the turn's culture
/// is banked and kept in the history of the last eight turns; a civilization that could not adopt
/// a policy before and can now is told.
pub(crate) fn end_turn(g: &mut Game, p: PlayerId, culture: f64) {
    let could = can_adopt_any(g, p);
    let slot = usize::try_from(g.turn().rem_euclid(8)).unwrap_or(0);
    if let Some(pl) = g.player_mut(p, PlayerTouch::STOCKS) {
        pl.econ.culture += culture.trunc();
        pl.econ.culture_hist[slot] = num::trunc_i32(culture);
    }
    if !could && can_adopt_any(g, p) {
        let who = g.player(p).map(|x| x.name.clone()).unwrap_or_default();
        g.emit(
            EngineEvent::PolicyAvailable,
            &format!("{who} can adopt a new social policy."),
            Some(PlayerSet::single(p)),
            None,
            EventData::default(),
            &[],
        );
    }
}

/// The culture a great writer is worth now: the civilization's culture of the last eight turns,
/// times the speed (`policies.culture_from_great_writer`, `policies.py:166-169`).
#[must_use]
pub fn culture_from_great_writer(g: &Game, p: PlayerId) -> i32 {
    let sum: i32 = g.player(p).map_or(0, |x| x.econ.culture_hist.iter().sum());
    num::trunc_i32(f64::from(sum) * g.speed().culture_cost_modifier)
}

/// `adopt_policy`: adopts a policy or opens a branch with culture or a free policy
/// (`tools.adopt_policy`, `tools.py:862-869`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdoptPolicy {
    pub policy: Value,
}

impl Rule for AdoptPolicy {
    type Plan = (PolicyId, Payment);

    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError> {
        let text = py::str_of(&self.policy);
        let policy = g.rules().resolve::<PolicyId>(&text).ok_or_else(|| {
            ActionError::new(
                ErrCode::BadParam,
                format!("Unknown policy '{text}'. Use names like 'Tradition' or 'Aristocracy'."),
            )
        })?;
        Ok((policy, plan_adopt(g, pid, policy)?))
    }

    fn apply(self, g: &mut Game, pid: PlayerId, (policy, pay): Self::Plan) -> OutcomeSpec {
        apply_adopt(g, pid, policy, pay);
        OutcomeSpec::render(move |g| adopt_result(g, pid, policy))
    }
}
