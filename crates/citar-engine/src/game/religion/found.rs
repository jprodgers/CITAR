//! Pantheons, founding and enhancing religions, and the beliefs taken with them
//! (`religion.py:451-706, 778-796`, UnCiv's `ReligionManager`).
//!
//! Each act is a check that only reads and an apply that cannot fail, as the action pipeline runs
//! them (DESIGN.md 8.3): [`plan_pantheon`] and [`apply_pantheon`] (the tool `found_pantheon`,
//! [`FoundPantheon`]), [`plan_religion`] and [`apply_religion`], [`plan_enhance`] and
//! [`apply_enhance`]. A great prophet founds and enhances religions as a unit action (package
//! 1c-04): the action checks the unit may act, then passes the tile it stands on and a closure
//! that spends it, which the apply calls where Python consumed the unit, before the triggers.
//!
//! What differs from Python, on purpose:
//! - the checks write nothing: Python set a civilization's `choose_pantheon_belief` before it
//!   checked the beliefs chosen, so a refused founding still owed it a pantheon belief;
//! - a belief listed twice is refused ([`validate_choice`]), where Python founded the religion
//!   with one belief fewer than it owed;
//! - `Adopt [belief]` adds the belief to the civilization's religion ([`adopt_belief`]) when it
//!   fits the religion's progress, where Python did nothing;
//! - an AI breaks a tie between beliefs it weighs the same by the ruleset's order
//!   ([`ai_choose_beliefs`]), where Python broke it by name.

use serde_json::{Value, json};
use smallvec::SmallVec;

use super::super::action::{OutcomeSpec, Rule};
use super::super::derive::rev::{PlayerTouch, UnitTouch, WorldTouch};
use super::super::error::ActionError;
use super::super::{Game, triggers};
use super::prophets::{faith_for_pantheon, max_religions, remaining_foundable};
use super::{
    add_pressure, all_beliefs, beliefs_available, beliefs_taken, display_name, is_holy_city,
    key_name,
};
use crate::base::ids::{BeliefId, CityId, PlayerId, ReligionId, RulesReligionId, TileIdx, UnitId};
use crate::base::num;
use crate::base::py;
use crate::base::sets::BeliefSet;
use crate::base::text::echo;
use crate::game::lookup::with_list;
use crate::rules::defs::{BeliefKind, BeliefType, ReligionProgress};
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::world::{Religion, ReligionName};
use crate::unique::params::{FoundingOrEnhancing, PolicyOrBelief};
use crate::unique::trigger::{TriggerEvent, TriggerSite};
use crate::unique::{Ctx, UniqueData, UniqueType, uq};

/// How many beliefs of each kind a civilization must choose, in the order they were counted.
pub type BeliefCounts = SmallVec<[(BeliefKind, i32); 5]>;

/// A civilization's religious progress.
fn progress(g: &Game, p: PlayerId) -> ReligionProgress {
    g.player(p).map_or(ReligionProgress::None, |x| x.religion.progress)
}

/// A belief's kind.
fn kind_of(g: &Game, b: BeliefId) -> BeliefType {
    g.rules().beliefs()[b].kind
}

/// A belief's name.
fn belief_name(g: &Game, b: BeliefId) -> &str {
    &g.rules().beliefs()[b].name
}

/// The names of `beliefs`, for a refusal to list.
fn names(g: &Game, beliefs: &[BeliefId]) -> Vec<String> {
    beliefs.iter().map(|&b| belief_name(g, b).to_string()).collect()
}

// ---- Pantheons ---------------------------------------------------------------------------------

/// Why a civilization may not found a pantheon now, or `None` (`religion.can_found_pantheon`,
/// `religion.py:451-470`): religion in play, a major without a religion, a pantheon belief left, a
/// game with room for one more, and the faith it costs or a free pantheon belief.
#[must_use]
pub fn can_found_pantheon(g: &Game, p: PlayerId) -> Option<String> {
    let Some(pl) = g.player(p) else { return Some("No such player.".into()) };
    if !g.religion_enabled() {
        return Some("Religion is disabled in this game.".into());
    }
    if !pl.is_major() {
        return Some("Only major civilizations may found pantheons.".into());
    }
    if pl.religion.progress > ReligionProgress::Pantheon {
        return Some("You have already founded a religion.".into());
    }
    if beliefs_available(g, BeliefKind::Type(BeliefType::Pantheon)).is_empty() {
        return Some("No pantheon beliefs remain.".into());
    }
    let enhanced = g.majors(true).any(|q| q.religion.progress == ReligionProgress::Enhanced);
    let started = g.majors(true).filter(|q| q.religion.progress != ReligionProgress::None).count();
    let first = pl.religion.progress == ReligionProgress::None;
    if (enhanced && i32::try_from(started).unwrap_or(i32::MAX) >= max_religions(g))
        || (first && !has_room(g))
    {
        return Some("No more pantheons can be founded.".into());
    }
    let cost = faith_for_pantheon(g, 0);
    if pl.religion.progress == ReligionProgress::None && pl.econ.faith >= f64::from(cost) {
        return None;
    }
    if pl.religion.free(BeliefKind::Type(BeliefType::Pantheon)) > 0 {
        return None;
    }
    Some(format!("A pantheon costs {cost} faith; you have {}.", num::trunc_i64(pl.econ.faith)))
}

/// How a pantheon belief is paid for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PantheonPayment {
    /// With faith: a first pantheon the civilization can afford.
    Faith(i32),
    /// With a free pantheon belief.
    Free,
}

/// Whether a civilization may found a pantheon with belief `text` now, and how it pays
/// (`religion.found_pantheon`'s checks, `religion.py:494-501`). Reads only.
///
/// # Errors
/// [`can_found_pantheon`]'s reason, a belief that is not a pantheon belief, or one taken.
pub fn plan_pantheon(
    g: &Game,
    p: PlayerId,
    text: &str,
) -> Result<(BeliefId, PantheonPayment), ActionError> {
    if let Some(reason) = can_found_pantheon(g, p) {
        return Err(ActionError::rule(reason));
    }
    let available = beliefs_available(g, BeliefKind::Type(BeliefType::Pantheon));
    let b = g
        .rules()
        .resolve::<BeliefId>(text)
        .filter(|&b| kind_of(g, b) == BeliefType::Pantheon)
        .ok_or_else(|| {
            // refcheck: refusals-end-as-sentences
            let head = format!("'{}' is not a pantheon belief. Available: ", echo(text));
            ActionError::rule(with_list(&head, &names(g, &available), ".", "get_religion"))
        })?;
    if !available.contains(&b) {
        return Err(ActionError::rule(format!(
            "{} has already been chosen by another civilization.",
            belief_name(g, b)
        )));
    }
    let Some(pl) = g.player(p) else { return Err(ActionError::rule("No such player.")) };
    let cost = faith_for_pantheon(g, 0);
    let affords =
        pl.religion.progress == ReligionProgress::None && pl.econ.faith >= f64::from(cost);
    let free = pl.religion.free(BeliefKind::Type(BeliefType::Pantheon)) > 0;
    Ok((b, if free && !affords { PantheonPayment::Free } else { PantheonPayment::Faith(cost) }))
}

/// Founds the pantheon [`plan_pantheon`] allowed (`religion.found_pantheon`,
/// `religion.py:502-522`): paid for; a first pantheon is a religion of its own, named by its
/// belief, whose pressure every city of the civilization feels; the belief joins it, and its
/// triggers and `upon founding a Pantheon` fire; everyone is told.
pub fn apply_pantheon(g: &mut Game, p: PlayerId, b: BeliefId, pay: PantheonPayment) {
    let Some(first) = g.player(p).map(|x| x.religion.progress == ReligionProgress::None) else {
        return;
    };
    // The pantheon is made before it is paid for, so that nothing is spent on one the game has
    // no room for ([`can_found_pantheon`] refuses that).
    let r = if first {
        let Some(r) = new_religion(g, ReligionName::Pantheon(b), p, None) else { return };
        if let Some(x) = g.player_mut(p, PlayerTouch::RELIGION) {
            x.religion.founded = Some(r);
        }
        r
    } else {
        let Some(r) = g.player(p).and_then(|x| x.religion.founded) else { return };
        r
    };
    if let Some(x) = g.player_mut(p, PlayerTouch::STOCKS) {
        match pay {
            PantheonPayment::Free => {
                let k = BeliefKind::Type(BeliefType::Pantheon);
                let n = x.religion.free(k);
                x.religion.set_free(k, n.saturating_sub(1));
            }
            PantheonPayment::Faith(cost) => x.econ.faith -= f64::from(cost),
        }
    }
    if first {
        let cities: Vec<(CityId, u16)> = g.player_cities(p).map(|c| (c.id(), c.pop)).collect();
        for (c, pop) in cities {
            add_pressure(g, c, Some(r), 200 * i32::from(pop));
        }
    }
    add_beliefs(g, r, &[b]);
    if first {
        if let Some(x) = g.player_mut(p, PlayerTouch::RELIGION) {
            x.religion.progress = ReligionProgress::Pantheon;
        }
        triggers::fire(g, &TriggerSite::civ(p), &TriggerEvent::FoundingPantheon, true, None);
    }
    belief_triggers(g, p, &[b]);
    let who = g.player(p).map(|x| x.name.to_string()).unwrap_or_default();
    let data = EventData { player: Some(p), belief: Some(b), ..EventData::default() };
    let text = format!("{who} founded the {} pantheon.", belief_name(g, b));
    g.emit(EngineEvent::Pantheon, &text, None, None, data, &[]);
}

/// What founding a pantheon reports (`religion.py:522`).
#[must_use]
pub fn pantheon_result(g: &Game, p: PlayerId, b: BeliefId) -> Value {
    json!({
        "pantheon": belief_name(g, b),
        "faith_left": g.player(p).map_or(0, |x| num::trunc_i64(x.econ.faith)),
    })
}

/// `found_pantheon`: founds a pantheon with a pantheon belief (`tools.found_pantheon`,
/// `tools.py:871-876`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FoundPantheon {
    pub belief: Value,
}

impl Rule for FoundPantheon {
    type Plan = (BeliefId, PantheonPayment);

    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError> {
        plan_pantheon(g, pid, &py::str_of(&self.belief))
    }

    fn apply(self, g: &mut Game, pid: PlayerId, (b, pay): Self::Plan) -> OutcomeSpec {
        apply_pantheon(g, pid, b, pay);
        OutcomeSpec::render(move |g| pantheon_result(g, pid, b))
    }
}

// ---- Religions and their beliefs -----------------------------------------------------------------

/// Whether the game has room for one more religion or pantheon: a [`ReligionId`] holds 256.
fn has_room(g: &Game) -> bool {
    g.state().world().religions.len() <= usize::from(u8::MAX)
}

/// A new religion or pantheon (`religion._new_religion`, `religion.py:473-477`), with no belief
/// yet; `None` past the 256 a game can hold, which the checks refuse first ([`has_room`]).
fn new_religion(
    g: &mut Game,
    name: ReligionName,
    founder: PlayerId,
    display: Option<String>,
) -> Option<ReligionId> {
    let id = ReligionId(u8::try_from(g.state().world().religions.len()).ok()?);
    let display = display.unwrap_or_else(|| match name {
        ReligionName::Pantheon(b) => belief_name(g, b).to_owned(),
        ReligionName::Religion(x) => {
            g.rules().religions().get(x).map(ToString::to_string).unwrap_or_default()
        }
    });
    g.edit_world(WorldTouch::RELIGIONS).religions.push(Religion {
        name,
        display: display.into(),
        founder,
        founder_beliefs: BeliefSet::new(),
        follower_beliefs: BeliefSet::new(),
        blocked_holy: false,
    });
    Some(id)
}

/// Beliefs join a religion: founder and enhancer beliefs as its founder's, pantheon and follower
/// beliefs as its followers' (`religion._add_beliefs`, `religion.py:480-489`).
fn add_beliefs(g: &mut Game, r: ReligionId, beliefs: &[BeliefId]) {
    let kinds: SmallVec<[(BeliefId, BeliefType); 4]> =
        beliefs.iter().map(|&b| (b, kind_of(g, b))).collect();
    let world = g.edit_world(WorldTouch::RELIGIONS);
    let Some(rel) = world.religions.get_mut(usize::from(r.0)) else { return };
    for (b, kind) in kinds {
        let set = match kind {
            BeliefType::Founder | BeliefType::Enhancer => &mut rel.founder_beliefs,
            BeliefType::Pantheon | BeliefType::Follower => &mut rel.follower_beliefs,
        };
        set.insert(b);
    }
}

/// What beliefs do when they are taken (`religion._belief_triggers`, `religion.py:525-534`):
/// their one-time effects, then `upon adopting [belief]`.
fn belief_triggers(g: &mut Game, p: PlayerId, beliefs: &[BeliefId]) {
    let r = g.rules();
    for &b in beliefs {
        let site = TriggerSite::civ(p);
        triggers::on_gain(g, &r.beliefs()[b].uniques, &site, None);
        let event = TriggerEvent::Adopting(PolicyOrBelief::Belief(b));
        triggers::fire(g, &site, &event, true, None);
    }
}

/// Whether civilization `p` may take belief `b` outside founding and enhancing: religion is in
/// play, nobody has taken the belief, and it fits how far the civilization has come. A pantheon
/// or follower belief joins a pantheon or a religion; a founder belief only a religion without
/// one, and an enhancer belief only an enhanced religion without one, so that a pantheon never
/// becomes a religion by a belief and no religion holds two beliefs of either kind.
pub(crate) fn may_adopt_belief(g: &Game, p: PlayerId, b: BeliefId) -> Option<ReligionId> {
    if !g.religion_enabled() || beliefs_taken(g).contains(b) {
        return None;
    }
    let pl = g.player(p)?;
    let r = pl.religion.founded?;
    let fits = match kind_of(g, b) {
        BeliefType::Pantheon | BeliefType::Follower => {
            pl.religion.progress >= ReligionProgress::Pantheon
        }
        BeliefType::Founder => {
            pl.religion.progress >= ReligionProgress::Religion && !super::is_major(g, r)
        }
        BeliefType::Enhancer => {
            pl.religion.progress >= ReligionProgress::Enhanced && !super::is_enhanced(g, r)
        }
    };
    fits.then_some(r)
}

/// `Adopt [belief]`: the belief joins the civilization's religion or pantheon if it may
/// (`may_adopt_belief`), and does what a belief does when taken. Python adopted policies alone.
/// Whether it was adopted.
pub fn adopt_belief(g: &mut Game, p: PlayerId, b: BeliefId) -> bool {
    let Some(r) = may_adopt_belief(g, p, b) else { return false };
    add_beliefs(g, r, &[b]);
    belief_triggers(g, p, &[b]);
    true
}

/// How many beliefs of each kind a civilization must choose to found (or enhance) a religion
/// (`religion.beliefs_to_choose`, `religion.py:537-575`): a founder (an enhancer), a pantheon
/// belief still owed, a follower belief, those its uniques add, and its free beliefs, each as
/// many as remain.
#[must_use]
pub fn beliefs_to_choose(g: &Game, p: PlayerId, enhancing: bool) -> BeliefCounts {
    let mut avail: [i32; BeliefKind::COUNT] = [0; BeliefKind::COUNT];
    for k in BeliefKind::ALL {
        avail[k.index()] = i32::try_from(beliefs_available(g, k).len()).unwrap_or(i32::MAX);
    }
    let mut out = BeliefCounts::new();
    let mut take = |k: BeliefKind, n: i32| {
        let n = n.min(avail[k.index()]);
        if n <= 0 {
            return;
        }
        match out.iter_mut().find(|(x, _)| *x == k) {
            Some((_, m)) => *m += n,
            None => out.push((k, n)),
        }
        avail[k.index()] -= n;
        if k != BeliefKind::Any {
            avail[BeliefKind::Any.index()] -= n;
        }
    };
    let Some(pl) = g.player(p) else { return out };
    let when =
        if enhancing { FoundingOrEnhancing::Enhancing } else { FoundingOrEnhancing::Founding };
    if enhancing {
        take(BeliefKind::Type(BeliefType::Enhancer), 1);
    } else {
        take(BeliefKind::Type(BeliefType::Founder), 1);
        // A civilization with no pantheon owes itself one: Python set the flag first.
        if pl.religion.choose_pantheon_belief || pl.religion.progress == ReligionProgress::None {
            take(BeliefKind::Type(BeliefType::Pantheon), 1);
        }
    }
    take(BeliefKind::Type(BeliefType::Follower), 1);
    let v = g.view();
    let ctx = Ctx::civ(p);
    for h in uq::civ(&v, p, UniqueType::FreeExtraBeliefs, &ctx) {
        if let UniqueData::FreeExtraBeliefs(x) = *h.data()
            && x.when == when
        {
            for _ in 0..h.n {
                take(x.belief, x.count);
            }
        }
    }
    for h in uq::civ(&v, p, UniqueType::FreeExtraAnyBeliefs, &ctx) {
        if let UniqueData::FreeExtraAnyBeliefs(x) = *h.data()
            && x.when == when
        {
            for _ in 0..h.n {
                take(BeliefKind::Any, x.count);
            }
        }
    }
    for k in BeliefKind::ALL {
        let n = i32::from(pl.religion.free(k));
        if n > 0 {
            take(k, n);
        }
    }
    out
}

/// The beliefs asked for, if they meet what is owed (`religion._validate_choice`,
/// `religion.py:578-605`): each one known and untaken, as many of each kind as owed, and the rest
/// filling the slots of any kind exactly.
///
/// # Errors
/// An unknown or taken belief, or the wrong number of some kind.
pub fn validate_choice(
    g: &Game,
    asked: &[String],
    needed: &BeliefCounts,
) -> Result<Vec<BeliefId>, ActionError> {
    let mut resolved = Vec::with_capacity(asked.len());
    for text in asked {
        let b = g
            .rules()
            .resolve::<BeliefId>(text)
            .ok_or_else(|| ActionError::rule(format!("Unknown belief '{}'.", echo(text))))?;
        // A belief is taken once: listed twice, it would fill two slots and join the religion
        // once.
        // refcheck: belief-listed-twice-refused
        if resolved.contains(&b) {
            return Err(ActionError::rule(format!("{} is listed twice.", belief_name(g, b))));
        }
        resolved.push(b);
    }
    let taken = beliefs_taken(g);
    if let Some(&b) = resolved.iter().find(|&&b| taken.contains(b)) {
        return Err(ActionError::rule(format!(
            "{} has already been chosen by another religion.",
            belief_name(g, b)
        )));
    }
    let count = |t: BeliefType| resolved.iter().filter(|&&b| kind_of(g, b) == t).count();
    let any_slots = needed.iter().find(|(k, _)| *k == BeliefKind::Any).map_or(0, |&(_, n)| n);
    let mut owed = 0i32;
    for &(k, n) in needed {
        let BeliefKind::Type(t) = k else { continue };
        owed += n;
        if i32::try_from(count(t)).unwrap_or(i32::MAX) < n {
            let available = beliefs_available(g, k);
            let first: Vec<BeliefId> = available.into_iter().take(30).collect();
            // refcheck: refusals-end-as-sentences
            let head = format!("Choose {n} {0} belief(s). Available {0} beliefs: ", t.name());
            let list = with_list(&head, &names(g, &first), ".", "get_religion");
            return Err(ActionError::rule(list));
        }
    }
    let extra = i32::try_from(resolved.len()).unwrap_or(i32::MAX) - owed;
    if extra > any_slots {
        let exactly: Vec<String> =
            needed.iter().map(|&(k, n)| format!("{n} {}", k.name())).collect();
        return Err(ActionError::rule(format!(
            "Too many beliefs: choose exactly {}.",
            exactly.join(", ")
        )));
    }
    if extra < any_slots {
        return Err(ActionError::rule(format!("Choose {any_slots} more belief(s) of any type.")));
    }
    Ok(resolved)
}

/// Why a civilization may not found a religion now, or `None` (`religion.can_found_religion`,
/// `religion.py:608-619`).
#[must_use]
pub fn can_found_religion(g: &Game, p: PlayerId) -> Option<String> {
    let Some(pl) = g.player(p) else { return Some("No such player.".into()) };
    if !g.religion_enabled() {
        return Some("Religion is disabled in this game.".into());
    }
    if pl.religion.progress >= ReligionProgress::Religion {
        return Some("You have already founded a religion.".into());
    }
    if !pl.is_major() {
        return Some("Only major civilizations may found religions.".into());
    }
    if remaining_foundable(g) == 0 || !has_room(g) {
        return Some("No more religions can be founded.".into());
    }
    None
}

/// A religion to found, as [`plan_religion`] checked it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundPlan {
    pub city: CityId,
    pub name: RulesReligionId,
    pub display: String,
    pub beliefs: Vec<BeliefId>,
}

/// Whether a civilization may found a religion in the city on tile `at` now, with this name and
/// these beliefs (`religion.found_religion`'s checks, `religion.py:622-650`). A name the ruleset
/// knows is that religion; any other is shown as the name of the first religion left. Reads only;
/// whether the unit may found a religion is the unit action's to check.
///
/// # Errors
/// [`can_found_religion`]'s reason, a tile that is not one of its cities, a holy city, a religion
/// founded already, or the beliefs [`validate_choice`] refuses.
pub fn plan_religion(
    g: &Game,
    p: PlayerId,
    at: TileIdx,
    name: &str,
    beliefs: &[String],
    display: Option<&str>,
) -> Result<FoundPlan, ActionError> {
    if let Some(reason) = can_found_religion(g, p) {
        return Err(ActionError::rule(reason));
    }
    let city = g.city_at(at).filter(|c| c.owner() == p).ok_or_else(|| {
        ActionError::rule(
            "A religion must be founded in one of your cities (move the Great Prophet into one).",
        )
    })?;
    if is_holy_city(g, city.id()) {
        return Err(ActionError::rule(format!("{} is already a holy city.", city.name)));
    }
    let r = g.rules();
    let wanted = name.trim().to_lowercase();
    let known = r.religions().iter().find(|(_, n)| n.to_lowercase() == wanted).map(|(id, _)| id);
    let used =
        |x: RulesReligionId| g.state().world().religion_named(ReligionName::Religion(x)).is_some();
    let (rid, display) = match known {
        Some(x) if used(x) => {
            return Err(ActionError::rule(format!(
                "{} has already been founded.",
                r.religions()[x]
            )));
        }
        Some(x) => (x, display.map_or_else(|| r.religions()[x].to_string(), str::to_owned)),
        None => {
            let free = r.religions().iter().map(|(id, _)| id).find(|&x| !used(x));
            let Some(x) = free else {
                return Err(ActionError::rule("No more religions can be founded."));
            };
            let own: String = name.trim().chars().take(40).collect();
            let shown = display
                .map(str::to_owned)
                .or_else(|| (!own.is_empty()).then_some(own))
                .unwrap_or_else(|| r.religions()[x].to_string());
            (x, shown)
        }
    };
    // Python owed the pantheon belief by setting `choose_pantheon_belief` here, before the
    // beliefs were checked; the count reads it without writing.
    // refcheck: refused-founding-owes-nothing
    let needed = beliefs_to_choose(g, p, false);
    let chosen = validate_choice(g, beliefs, &needed)?;
    Ok(FoundPlan {
        city: city.id(),
        name: rid,
        display: display.chars().take(40).collect(),
        beliefs: chosen,
    })
}

/// Founds the religion [`plan_religion`] allowed (`religion.found_religion`,
/// `religion.py:651-671`): it takes the civilization's pantheon's beliefs and those chosen; the
/// city becomes its holy city with five hundred pressure a citizen; the civilization's religious
/// units that take its religion carry it; `spend` spends the great prophet; then `upon founding a
/// Religion` and the beliefs' triggers fire, and everyone is told.
pub fn apply_religion(g: &mut Game, p: PlayerId, plan: &FoundPlan, spend: impl FnOnce(&mut Game)) {
    let old = g.player(p).and_then(|x| x.religion.founded);
    let Some(r) = new_religion(g, ReligionName::Religion(plan.name), p, Some(plan.display.clone()))
    else {
        return;
    };
    if let Some(old) = old.and_then(|o| g.state().world().religion(o)).cloned() {
        let beliefs: Vec<BeliefId> =
            old.founder_beliefs.iter().chain(old.follower_beliefs.iter()).collect();
        add_beliefs(g, r, &beliefs);
    }
    add_beliefs(g, r, &plan.beliefs);
    if let Some(x) = g.player_mut(p, PlayerTouch::RELIGION) {
        x.religion.founded = Some(r);
        x.religion.progress = ReligionProgress::Religion;
        x.religion.free_beliefs = [0; BeliefKind::COUNT];
        x.religion.choose_pantheon_belief = false;
    }
    if let Some(x) = g.city_mut(plan.city, super::super::derive::rev::CityTouch::RELIGION) {
        x.holy_city_of = Some(r);
    }
    let pop = g.city(plan.city).map_or(0, |x| i32::from(x.pop));
    add_pressure(g, plan.city, Some(r), pop * 500);
    let rules = g.rules();
    let takers: Vec<UnitId> = g
        .player_units(p)
        .filter(|u| {
            let has = |ty| super::super::cities::construction::unit_has_type(rules, u.base, ty);
            has(UniqueType::ReligiousUnit) && has(UniqueType::TakeReligionOverBirthCity)
        })
        .map(crate::state::units::Unit::id)
        .collect();
    for u in takers {
        if let Some(x) = g.unit_mut(u, UnitTouch::CORE) {
            x.religion = Some(r);
        }
    }
    spend(g);
    triggers::fire(g, &TriggerSite::civ(p), &TriggerEvent::FoundingReligion, true, None);
    belief_triggers(g, p, &plan.beliefs);
    let who = g.player(p).map(|x| x.name.to_string()).unwrap_or_default();
    let (city_name, at) = g.city(plan.city).map(|x| (x.name.to_string(), x.tile())).unzip();
    let city_name = city_name.unwrap_or_default();
    let data = EventData { player: Some(p), religion: Some(r), ..EventData::default() };
    let text = format!("{who} founded {} in {city_name}!", display_name(g, r));
    g.emit(EngineEvent::ReligionFounded, &text, None, at, data, &[]);
}

/// What founding a religion reports (`religion.py:671`).
#[must_use]
pub fn religion_result(g: &Game, p: PlayerId, plan: &FoundPlan) -> Value {
    let r = g.player(p).and_then(|x| x.religion.founded);
    json!({
        "founded": r.map(|r| display_name(g, r)),
        "religion": r.map(|r| key_name(g, r)),
        "beliefs": r.map(|r| all_beliefs(g, r)).unwrap_or_default().iter().map(|&b| belief_name(g, b)).collect::<Vec<_>>(),
        "holy_city": g.city(plan.city).map(|x| x.name.to_string()),
    })
}

/// Whether a civilization may enhance its religion from tile `at` now with these beliefs
/// (`religion.enhance_religion`'s checks, `religion.py:674-688`). Reads only; whether the unit
/// may enhance a religion is the unit action's to check.
///
/// # Errors
/// No founded religion to enhance, no beliefs left, a tile with no city, or the beliefs
/// [`validate_choice`] refuses.
pub fn plan_enhance(
    g: &Game,
    p: PlayerId,
    at: TileIdx,
    beliefs: &[String],
) -> Result<Vec<BeliefId>, ActionError> {
    if !g.religion_enabled() || progress(g, p) != ReligionProgress::Religion {
        return Err(ActionError::rule(
            "You can only enhance a founded (not yet enhanced) religion.",
        ));
    }
    if beliefs_available(g, BeliefKind::Type(BeliefType::Follower)).is_empty()
        || beliefs_available(g, BeliefKind::Type(BeliefType::Enhancer)).is_empty()
    {
        return Err(ActionError::rule("No beliefs remain to enhance a religion."));
    }
    if g.city_at(at).is_none() {
        return Err(ActionError::rule("Enhance your religion from a city tile."));
    }
    let needed = beliefs_to_choose(g, p, true);
    validate_choice(g, beliefs, &needed)
}

/// Enhances the civilization's religion with the beliefs [`plan_enhance`] allowed
/// (`religion.enhance_religion`, `religion.py:689-698`): `spend` spends the great prophet, then
/// `upon enhancing a Religion` and the beliefs' triggers fire, and everyone is told.
pub fn apply_enhance(
    g: &mut Game,
    p: PlayerId,
    beliefs: &[BeliefId],
    spend: impl FnOnce(&mut Game),
) {
    let Some(r) = g.player(p).and_then(|x| x.religion.founded) else { return };
    add_beliefs(g, r, beliefs);
    if let Some(x) = g.player_mut(p, PlayerTouch::RELIGION) {
        x.religion.progress = ReligionProgress::Enhanced;
        x.religion.free_beliefs = [0; BeliefKind::COUNT];
    }
    spend(g);
    triggers::fire(g, &TriggerSite::civ(p), &TriggerEvent::EnhancingReligion, true, None);
    belief_triggers(g, p, beliefs);
    let who = g.player(p).map(|x| x.name.to_string()).unwrap_or_default();
    let data = EventData { player: Some(p), religion: Some(r), ..EventData::default() };
    let text = format!("{who} enhanced {}.", display_name(g, r));
    g.emit(EngineEvent::ReligionEnhanced, &text, None, None, data, &[]);
}

/// What enhancing a religion reports (`religion.py:698`).
#[must_use]
pub fn enhance_result(g: &Game, p: PlayerId) -> Value {
    let r = g.player(p).and_then(|x| x.religion.founded);
    json!({
        "enhanced": r.map(|r| display_name(g, r)),
        "beliefs": r.map(|r| all_beliefs(g, r)).unwrap_or_default().iter().map(|&b| belief_name(g, b)).collect::<Vec<_>>(),
    })
}

/// The beliefs an AI picks to meet what is owed (`religion.ai_choose_beliefs`,
/// `religion.py:778-786`): of each kind, the untaken ones it weighs most, then in the ruleset's
/// order, where Python took them by name.
#[must_use]
pub fn ai_choose_beliefs(g: &Game, p: PlayerId, needed: &BeliefCounts) -> Vec<BeliefId> {
    let mut out: Vec<BeliefId> = Vec::new();
    for &(k, n) in needed {
        let mut pool: Vec<(f64, BeliefId)> = beliefs_available(g, k)
            .into_iter()
            .filter(|b| !out.contains(b))
            .map(|b| (belief_weight(g, p, b), b))
            .collect();
        // refcheck: ai-beliefs-tie-by-ruleset-order
        pool.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        out.extend(pool.into_iter().take(usize::try_from(n).unwrap_or(0)).map(|(_, b)| b));
    }
    out
}

/// How much an AI wants a belief (`religion._belief_weight`, `religion.py:789-796`): each of its
/// `[n]% weight to this choice for AI decisions` that holds for the civilization.
fn belief_weight(g: &Game, p: PlayerId, b: BeliefId) -> f64 {
    let v = g.view();
    let ctx = Ctx::civ(p);
    let mut w = 1.0;
    for h in uq::object(&v, &g.rules().beliefs()[b].uniques, UniqueType::AiChoiceWeight, &ctx) {
        if let UniqueData::AiChoiceWeight(x) = *h.data() {
            w *= 1.0 + f64::from(x.percent) / 100.0;
        }
    }
    w
}
