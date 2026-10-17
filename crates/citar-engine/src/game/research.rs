//! Research: which techs a civilization knows, what it researches, what a tech costs, and what
//! learning one brings (`research.py`).
//!
//! Package 1b-02 ported what the scenario operations need: repeatable and unresearchable techs
//! (`research.py:14-17, 52-58`), the path to a goal and setting the research
//! (`research.py:81-135`), learning a tech (`research.py:284-331`) and forgetting one with every
//! tech that needs it (`scenario.py:155-174`). Setting research is split as the action pipeline
//! runs a tool: [`plan_research`] checks and only reads, [`apply_research`] writes and cannot
//! fail, and [`research_result`] reads the result from the settled game. Package 1b-03 added
//! `add_tech_silently` (`research.py:247-251`) for the starting techs of a new game, which
//! package 1c-06's city-state catch-up shares (`city_states.py:416`).
//!
//! Package 1b-07 ports the rest of `research.py`:
//! - costs: [`science_modifier`] and [`tech_cost`] (`research.py:19-49`), the techs a
//!   civilization may research ([`can_research`], [`available_techs`], `research.py:61-78`);
//! - progress: [`add_science`], [`update_research_progress`] and the overflow a completed tech
//!   carries ([`limit_overflow`]), the turns left ([`turns_left`]), and a turn's science with the
//!   research agreements' (`end_turn`, stage E3; `research.py:160-231`);
//! - taking a tech off the queue ([`plan_dequeue`]) and choosing a free one ([`plan_free_tech`]);
//! - eras: entering one ([`enter_era`], `research.py:357-376`) and the world's
//!   ([`world_era`]); the units a tech makes obsolete leave production queues
//!   (`research.py:333-354`);
//! - the Korean boost and a great scientist's science (`research.py:379-398`);
//! - the tools `set_research`, `dequeue_research` and `choose_free_tech` (`tools.py:833-859`).
//!
//! The uniques a tech or an era triggers, `upon discovering [tech]` and `upon entering the
//! [era]`, are fired by package 1b-08, which ports triggers; each is marked where it happens.
//!
//! What differs from Python, on purpose: a granted tech is announced "Rome was granted Pottery."
//! where Python wrote "Rome scenario Pottery." (refcheck: scenario-tech-announcement-wording).

use serde_json::{Value, json};

use super::Game;
use super::action::{OutcomeSpec, Rule};
use super::core::has_type;
use super::derive::rev::{CityTouch, PlayerTouch};
use super::error::{ActionError, ErrCode};
use crate::base::ids::{CityId, EraId, PlayerId, TechId};
use crate::base::num;
use crate::base::py;
use crate::base::sets::{PlayerSet, TechSet};
use crate::base::stats::Stat;
use crate::base::text::echo;
use crate::rules::Ruleset;
use crate::rules::defs::PolicyKind;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::cities::Constructible;
use crate::unique::trigger::{TriggerEvent, TriggerSite};
use crate::unique::{Ctx, UniqueData, UniqueType, uq};

/// How a civilization came by a tech, which the announcement says (`research.py:302-304`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TechSource {
    Research,
    Ruins,
    Trade,
    Free,
    GreatPerson,
    Espionage,
    ResearchAgreement,
    /// A scenario operation granted it.
    Scenario,
}

impl TechSource {
    /// The verb of the announcement: "Rome researched Pottery."
    #[must_use]
    pub const fn verb(self) -> &'static str {
        match self {
            Self::Research | Self::ResearchAgreement => "researched",
            Self::Ruins => "found in ruins",
            Self::Trade => "acquired through a deal",
            Self::Free => "chose as a free technology",
            Self::GreatPerson => "discovered with a Great Scientist",
            Self::Espionage => "stole",
            // refcheck: scenario-tech-announcement-wording
            Self::Scenario => "was granted",
        }
    }
}

/// Whether a tech can be researched again and again, as the future techs can
/// (`research.py:14-16`).
#[must_use]
pub fn is_repeatable(rules: &Ruleset, t: TechId) -> bool {
    rules
        .techs()
        .get(t)
        .is_some_and(|d| has_type(rules, &d.uniques, UniqueType::ResearchableMultipleTimes))
}

/// The era a civilization is in, from the techs it knows (`research.player_era`,
/// `research.py:254-275`, UnCiv's `TechManager.updateEra`): the era of its furthest column of
/// the tech tree, or, once it has every tech of the earlier columns, the era of the first column
/// it has not finished, if that is later. With no tech at all it is in the first era.
///
/// Package 1b-05 ports it for the unique index, which holds the era's uniques; within a column
/// the first tech by id stands for it, where Python took the first of the civilization's own
/// list (the techs of one column share an era).
#[must_use]
pub fn player_era(rules: &Ruleset, known: &TechSet) -> EraId {
    let techs = rules.techs();
    let mut furthest: Option<(u16, EraId)> = None;
    let mut first_missing: Option<(u16, EraId)> = None;
    for (t, def) in techs.iter() {
        let slot = if known.contains(t) { &mut furthest } else { &mut first_missing };
        let better = match (*slot, known.contains(t)) {
            (None, _) => true,
            (Some((col, _)), true) => def.column > col,
            (Some((col, _)), false) => def.column < col,
        };
        if better {
            *slot = Some((def.column, def.era));
        }
    }
    match (furthest, first_missing) {
        (None, _) => EraId(0),
        (Some((_, era)), None) => era,
        (Some((_, era)), Some((_, next))) => era.max(next),
    }
}

/// The era most major civilizations have reached: the median of the living majors' eras
/// (`research.world_era`, `research.py:278-281`), the first era with none.
#[must_use]
pub fn world_era(g: &Game) -> EraId {
    let mut eras: Vec<EraId> = g.majors(true).map(|p| super::derive::civ::era(g, p.id())).collect();
    eras.sort();
    eras.get(eras.len() / 2).copied().unwrap_or(EraId(0))
}

// ---- What can be researched, and what it costs (research.py:19-78) -------------------------------

/// Whether a civilization can never research a tech in this game (`research.py:52-58`): one of
/// its `Only available` requirements fails, or one of its `Unavailable` ones holds.
#[must_use]
pub fn is_unresearchable(g: &Game, p: PlayerId, t: TechId) -> bool {
    let Some(def) = g.rules.techs().get(t) else { return true };
    let table = g.rules.uniques();
    let view = g.view();
    let ctx = Ctx::civ(p);
    def.uniques.ids().any(|id| match table.meta(id).ty {
        Some(UniqueType::OnlyAvailable) => !crate::unique::applies(id, &ctx, &view),
        Some(UniqueType::Unavailable) => crate::unique::applies(id, &ctx, &view),
        _ => false,
    })
}

/// Whether a civilization could research a tech now (`research.can_research`,
/// `research.py:61-68`): it can be researched in this game, it is not known (or it repeats), and
/// every prerequisite is known.
#[must_use]
pub fn can_research(g: &Game, p: PlayerId, t: TechId) -> bool {
    let Some(def) = g.rules.techs().get(t) else { return false };
    if is_unresearchable(g, p, t) {
        return false;
    }
    if g.has_tech(p, Some(t)) && !is_repeatable(g.rules, t) {
        return false;
    }
    def.prerequisites.iter().all(|&pre| g.has_tech(p, Some(pre)))
}

/// The techs a civilization could research now, in the tech order (`research.available_techs`,
/// `research.py:71-73`).
#[must_use]
pub fn available_techs(g: &Game, p: PlayerId) -> Vec<TechId> {
    g.rules.derived().tech_order.iter().copied().filter(|&t| can_research(g, p, t)).collect()
}

/// Whether there is nothing left to research (`research.all_researched`, `research.py:76-78`).
#[must_use]
pub fn all_researched(g: &Game, p: PlayerId) -> bool {
    g.rules.techs().ids().all(|t| g.has_tech(p, Some(t)) || !can_research(g, p, t))
}

/// How much cheaper a tech is for the major civilizations a civilization has met that know it
/// (`research.science_modifier`, `research.py:19-23`, UnCiv's `getScienceModifier`): 30% of the
/// share of the living majors that do.
#[must_use]
pub fn science_modifier(g: &Game, p: PlayerId, t: TechId) -> f64 {
    let mut known = 0u32;
    let mut majors = 0u32;
    for q in g.majors(true) {
        majors += 1;
        if q.id() != p && g.has_met(p, q.id()) && q.tech.known.contains(t) {
            known += 1;
        }
    }
    1.0 + f64::from(known) / f64::from(majors.max(1)) * 0.3
}

/// What a tech costs a civilization (`research.tech_cost`, `research.py:26-49`): its cost, times
/// a humanlike seat's difficulty, the speed, less for the majors met who know it, times the map
/// size's multiplier; `[n]% Science cost of researching new Technologies` for each; and more for
/// each city beyond the first that is not a puppet, less with `Each city founded increases Science
/// cost of Technologies [n]% less than normal`.
///
/// At least 1: discounts a ruleset stacks to 100% or more would make a tech free, and a free
/// repeatable tech would be learned without end.
#[must_use]
pub fn tech_cost(g: &Game, p: PlayerId, t: TechId) -> i32 {
    let r = g.rules;
    let Some(def) = r.techs().get(t) else { return 0 };
    let mut cost = f64::from(def.cost);
    if g.is_humanlike(p) {
        cost *= r.difficulties()[g.difficulty(Some(p))].research_cost_modifier;
    }
    cost *= g.speed().science_cost_modifier;
    cost /= science_modifier(g, p, t);
    let map = g.state().map();
    let pre = r.constants().map_size_predefined(map.width, map.height);
    cost *= pre.tech_cost_multiplier;
    let cities = g.player_cities(p).filter(|c| !c.puppet).count();
    let cities = f64::from(u32::try_from(cities).unwrap_or(u32::MAX));
    let mut city_mod = (cities - 1.0) * pre.tech_cost_per_city;
    let v = g.view();
    let ctx = Ctx::civ(p);
    for h in uq::civ(&v, p, UniqueType::LessTechCostFromCities, &ctx) {
        if let UniqueData::LessTechCostFromCities(x) = h.data() {
            for _ in 0..h.n {
                city_mod *= 1.0 - f64::from(x.percent) / 100.0;
            }
        }
    }
    for h in uq::civ(&v, p, UniqueType::LessTechCost, &ctx) {
        if let UniqueData::LessTechCost(x) = h.data() {
            for _ in 0..h.n {
                cost *= 1.0 + f64::from(x.percent) / 100.0;
            }
        }
    }
    cost *= 1.0 + city_mod;
    num::trunc_i32(cost).max(1)
}

/// The median cost of what a civilization could research now (`research.median_available_cost`,
/// `research.py:379-385`), 0 with nothing to research.
#[must_use]
pub fn median_available_cost(g: &Game, p: PlayerId) -> f64 {
    let mut costs: Vec<i32> = g
        .rules
        .techs()
        .ids()
        .filter(|&t| can_research(g, p, t))
        .map(|t| tech_cost(g, p, t))
        .collect();
    costs.sort();
    let n = costs.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        f64::from(costs[n / 2])
    } else {
        (f64::from(costs[n / 2 - 1]) + f64::from(costs[n / 2])) / 2.0
    }
}

/// The science a great scientist is worth now: the civilization's science of the last eight
/// turns, times the speed (`research.science_from_great_scientist`, `research.py:395-398`).
#[must_use]
pub fn science_from_great_scientist(g: &Game, p: PlayerId) -> i32 {
    let sum: i32 = g.player(p).map_or(0, |x| x.econ.science_hist.iter().sum());
    num::trunc_i32(f64::from(sum) * g.speed().science_cost_modifier)
}

// ---- The research queue (research.py:81-157) -------------------------------------------------------

/// The techs to research, in order, to reach `goal`: its missing prerequisites and itself,
/// by tree column and then the tech order (`research.py:81-100`). Empty if the goal, or a tech
/// on the way, can never be researched.
#[must_use]
pub fn path_to(g: &Game, p: PlayerId, goal: TechId) -> Vec<TechId> {
    let r = g.rules;
    if is_unresearchable(g, p, goal) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut seen = Vec::new();
    let mut queue = std::collections::VecDeque::from([goal]);
    while let Some(t) = queue.pop_front() {
        if is_unresearchable(g, p, t) {
            return Vec::new();
        }
        if !is_repeatable(r, t) && (g.has_tech(p, Some(t)) || seen.contains(&t)) {
            continue;
        }
        seen.push(t);
        if let Some(d) = r.techs().get(t) {
            queue.extend(d.prerequisites.iter().copied());
        }
        out.push(t);
    }
    let order = &r.derived().tech_order;
    let place = |t: TechId| order.iter().position(|&x| x == t).unwrap_or(usize::MAX);
    out.sort_by_key(|&t| (r.techs().get(t).map_or(u16::MAX, |d| d.column), place(t)));
    out
}

/// What a civilization researches now (`research.current`, `research.py:103-106`).
#[must_use]
pub fn current(g: &Game, p: PlayerId) -> Option<TechId> {
    g.player(p).and_then(|x| x.tech.queue.first().copied())
}

/// The research queue that setting research to `tech` makes (`research.py:109-128`): `tech`, or
/// the path to it when it needs other techs first; with `append`, the path goes on the end of the
/// queue instead. Reads only, so a refusal changes nothing.
///
/// # Errors
/// The civilization knows `tech` (and it is not repeatable), cannot research it, or, with
/// `append`, has it queued already.
pub fn plan_research(
    g: &Game,
    p: PlayerId,
    tech: TechId,
    append: bool,
) -> Result<Vec<TechId>, ActionError> {
    let r = g.rules;
    let name = r.name(tech).unwrap_or("?");
    let repeatable = is_repeatable(r, tech);
    if g.has_tech(p, Some(tech)) && !repeatable {
        return Err(ActionError::rule(format!("You already know {name}.")));
    }
    let path = path_to(g, p, tech);
    if path.is_empty() {
        return Err(ActionError::rule(format!("{name} cannot be researched.")));
    }
    if !append {
        return Ok(path);
    }
    let mut queue = g.player(p).map(|pl| pl.tech.queue.clone()).unwrap_or_default();
    if queue.contains(&tech) && !repeatable {
        return Err(ActionError::rule(format!("{name} is already queued.")));
    }
    let rest: Vec<TechId> = path.into_iter().filter(|t| !queue.contains(t)).collect();
    queue.extend(rest);
    Ok(queue)
}

/// Writes the queue [`plan_research`] made, with its last tech as the goal when there is more
/// than one, then lets the science carried over complete what it can (`research.py:129-130`).
pub fn apply_research(g: &mut Game, p: PlayerId, path: &[TechId]) {
    let goal = (path.len() > 1).then(|| path[path.len() - 1]);
    if let Some(pl) = g.player_mut(p, PlayerTouch::RESEARCH) {
        pl.tech.queue = path.to_vec();
        pl.tech.goal = goal;
    }
    update_research_progress(g, p);
}

/// What setting the research reports (`research.py:131-135`), read after the write and the
/// settle, since overflow may already have completed the first tech: what is researched now,
/// the turns it takes and the queue; and, when `path` (what [`plan_research`] made) is more than
/// one tech, its goal and the path itself.
#[must_use]
pub fn research_result(g: &Game, p: PlayerId, path: &[TechId]) -> Value {
    let r = g.rules;
    let names = |ts: &[TechId]| -> Vec<&str> { ts.iter().filter_map(|&t| r.name(t)).collect() };
    let queue: &[TechId] = g.player(p).map_or(&[], |pl| &pl.tech.queue);
    let current = queue.first().and_then(|&t| r.name(t));
    let mut out =
        json!({"researching": current, "turns": turns_left(g, p, None), "queue": names(queue)});
    if let (Some(&goal), Some(o)) = (path.last(), out.as_object_mut())
        && path.len() > 1
    {
        o.insert("goal".into(), json!(r.name(goal)));
        o.insert("path".into(), json!(names(path)));
    }
    out
}

/// Whether tech `t` needs `target`, however indirectly.
fn needs(r: &Ruleset, t: TechId, target: TechId) -> bool {
    let mut seen: Vec<TechId> = Vec::new();
    let mut stack: Vec<TechId> =
        r.techs().get(t).map(|d| d.prerequisites.to_vec()).unwrap_or_default();
    while let Some(pre) = stack.pop() {
        if pre == target {
            return true;
        }
        if seen.contains(&pre) {
            continue;
        }
        seen.push(pre);
        if let Some(d) = r.techs().get(pre) {
            stack.extend(d.prerequisites.iter().copied());
        }
    }
    false
}

/// The queue left when `tech` and every queued tech that needs it come off it
/// (`research.dequeue_research`, `research.py:138-157`), and those taken off in queue order.
///
/// # Errors
/// `tech` is not in the queue.
pub fn plan_dequeue(
    g: &Game,
    p: PlayerId,
    tech: Option<TechId>,
    asked: &str,
) -> Result<(Vec<TechId>, Vec<TechId>), ActionError> {
    let queue: Vec<TechId> = g.player(p).map(|x| x.tech.queue.clone()).unwrap_or_default();
    let Some(tech) = tech.filter(|t| queue.contains(t)) else {
        return Err(ActionError::rule(format!("{} is not in your research queue.", echo(asked))));
    };
    let r = g.rules;
    let removed: Vec<TechId> =
        queue.iter().copied().filter(|&t| t == tech || needs(r, t, tech)).collect();
    let kept = queue.into_iter().filter(|t| !removed.contains(t)).collect();
    Ok((kept, removed))
}

/// Writes a queue [`plan_dequeue`] left: its last tech is the goal when there is more than one,
/// and the science carried over completes what it can.
pub fn apply_dequeue(g: &mut Game, p: PlayerId, kept: Vec<TechId>) {
    let goal = (kept.len() > 1).then(|| kept[kept.len() - 1]);
    if let Some(pl) = g.player_mut(p, PlayerTouch::RESEARCH) {
        pl.tech.queue = kept;
        pl.tech.goal = goal;
    }
    update_research_progress(g, p);
}

// ---- Progress (research.py:160-231) ----------------------------------------------------------------

/// The civilization's science for the next turn, from its stats.
fn science_rate(g: &Game, p: PlayerId) -> f64 {
    super::derive::stats::civ_stats(g, p).total[Stat::Science]
}

/// The turns until a tech (the current research by default) completes at the civilization's
/// science now (`research.turns_left`, `research.py:160-173`): 0 if it is paid for already,
/// `None` with no research or no science.
#[must_use]
pub fn turns_left(g: &Game, p: PlayerId, tech: Option<TechId>) -> Option<i32> {
    let tech = tech.or_else(|| current(g, p))?;
    let pl = g.player(p)?;
    let overflow = if can_research(g, p, tech) { pl.tech.overflow } else { 0.0 };
    let done = pl.tech.progress.get(&tech).copied().unwrap_or(0.0);
    let remaining = f64::from(tech_cost(g, p, tech)) - done - overflow;
    let sci = science_rate(g, p);
    if remaining <= 0.0 {
        return Some(0);
    }
    if sci <= 0.0 {
        return None;
    }
    // Python's -(-a // b): the ceiling of a whole division.
    let a = num::trunc_i64(remaining);
    let b = num::trunc_i64(sci).max(1);
    Some(i32::try_from(-num::floor_div(-a, b)).unwrap_or(i32::MAX).max(1))
}

/// The science a completed tech may carry over (`research.limit_overflow`,
/// `research.py:176-181`): at most five turns of the civilization's science, or the base cost of
/// what it researches, whichever is more.
#[must_use]
pub fn limit_overflow(g: &Game, p: PlayerId, overflow: f64) -> f64 {
    let cost = current(g, p).and_then(|t| g.rules.techs().get(t)).map_or(0, |d| d.cost);
    overflow.min((science_rate(g, p) * 5.0).max(f64::from(cost)))
}

/// Adds science to what a civilization researches, completing it when it is paid for, with the
/// overflow carried (`research.add_science`, `research.py:206-219`); with no research, it all
/// waits as overflow.
///
/// Python's `add_tech` called `update_research_progress`, which called `add_science` again: one
/// nested call per tech the overflow paid for. Here each tech is a turn of a loop, so a cheap
/// repeatable tech, which the overflow may pay for many times over, cannot exhaust the stack.
/// Each turn spends at least the tech's cost (at least 1) of the overflow, so the loop ends.
pub fn add_science(g: &mut Game, p: PlayerId, amount: f64) {
    let mut amount = amount;
    loop {
        let Some(cur) = current(g, p) else {
            if let Some(pl) = g.player_mut(p, PlayerTouch::RESEARCH) {
                pl.tech.overflow += amount;
            }
            return;
        };
        let Some(pl) = g.player_mut(p, PlayerTouch::RESEARCH) else { return };
        let done = pl.tech.progress.entry(cur).or_insert(0.0);
        *done += amount;
        let done = *done;
        let cost = f64::from(tech_cost(g, p, cur));
        if done < cost {
            return;
        }
        let extra = limit_overflow(g, p, done - cost);
        if let Some(pl) = g.player_mut(p, PlayerTouch::RESEARCH) {
            pl.tech.overflow += extra;
        }
        learn(g, p, cur, TechSource::Research);
        match take_carried(g, p) {
            Some(real) => amount = real,
            None => return,
        }
    }
}

/// The science carried over, taken from the overflow, if it pays for what the civilization
/// researches now (`research.py:224-229`).
fn take_carried(g: &mut Game, p: PlayerId) -> Option<f64> {
    let cur = current(g, p)?;
    let pl = g.player(p)?;
    let real = pl.tech.overflow;
    let done = pl.tech.progress.get(&cur).copied().unwrap_or(0.0);
    if done + real < f64::from(tech_cost(g, p, cur)) {
        return None;
    }
    if let Some(pl) = g.player_mut(p, PlayerTouch::RESEARCH) {
        pl.tech.overflow = 0.0;
    }
    Some(real)
}

/// Lets the science carried over complete what a civilization researches now, if it pays for
/// it (`research.update_research_progress`, `research.py:222-231`): after the research or its
/// cost changed, and at the start of the civilization's turn (stage S2).
pub fn update_research_progress(g: &mut Game, p: PlayerId) {
    if let Some(real) = take_carried(g, p) {
        add_science(g, p, real);
    }
}

/// Stage S2, research progress: `update_research_progress` for a civilization with cities
/// (`turns.py:39-40`).
pub(crate) fn start_turn(g: &mut Game, p: PlayerId) {
    update_research_progress(g, p);
}

/// Stage E3, science (`research.end_turn`, `research.py:184-203`): the turn's science goes into
/// the history of the last eight turns, then, with something researched, into it with what the
/// research agreements that concluded hold and the overflow waiting.
pub(crate) fn end_turn(g: &mut Game, p: PlayerId, science: f64) {
    let turn = g.turn();
    let slot = usize::try_from(turn.rem_euclid(8)).unwrap_or(0);
    if let Some(pl) = g.player_mut(p, PlayerTouch::OTHER) {
        pl.econ.science_hist[slot] = num::trunc_i32(science);
    }
    if current(g, p).is_none() {
        return;
    }
    let mut add = num::trunc_i64(science);
    let (ra, overflow, name) = match g.player(p) {
        Some(x) => (x.tech.ra_bonus, x.tech.overflow, x.name.clone()),
        None => return,
    };
    if ra != 0 {
        let m = {
            let v = g.view();
            let mut m = 0.5;
            for h in uq::civ(&v, p, UniqueType::ScienceFromResearchAgreements, &Ctx::civ(p)) {
                if let UniqueData::ScienceFromResearchAgreements(x) = h.data() {
                    for _ in 0..h.n {
                        m += f64::from(x.percent) / 200.0;
                    }
                }
            }
            m
        };
        let boost = num::trunc_i64(f64::from(ra) / 3.0 * m);
        add += boost;
        if let Some(pl) = g.player_mut(p, PlayerTouch::RESEARCH) {
            pl.tech.ra_bonus = 0;
        }
        g.emit(
            EngineEvent::Tech,
            &format!("{name} gained {boost} science from research agreements."),
            Some(PlayerSet::single(p)),
            None,
            EventData::default(),
            &[],
        );
    }
    if overflow != 0.0 {
        add += num::trunc_i64(overflow);
        if let Some(pl) = g.player_mut(p, PlayerTouch::RESEARCH) {
            pl.tech.overflow = 0.0;
        }
    }
    #[allow(clippy::cast_precision_loss, reason = "a turn's science is far below 2^52")]
    add_science(g, p, add as f64);
}

/// Stage S9, a research reminder (`turns.py:62-64`): a major with cities that researches nothing
/// and could is told to choose.
pub(crate) fn remind(g: &mut Game, p: PlayerId) {
    let idle = g.player(p).is_some_and(|x| x.alive() && x.tech.queue.is_empty());
    if !idle || g.player_cities(p).next().is_none() || available_techs(g, p).is_empty() {
        return;
    }
    g.emit(
        EngineEvent::ResearchNeeded,
        "Choose a technology to research.",
        Some(PlayerSet::single(p)),
        None,
        EventData::default(),
        &[],
    );
}

// ---- Learning a tech (research.py:234-354) ---------------------------------------------------------

/// A civilization learns a tech (`research.py:284-331`): it joins the known techs (a repeatable
/// one counts once more), leaves the research queue, and is announced to the civilization with
/// what it unlocks. The units it makes obsolete leave production queues, a new era is entered,
/// every city looks at its citizens again, and the science carried over may complete the next
/// research.
pub fn add_tech(g: &mut Game, p: PlayerId, tech: TechId, source: TechSource) {
    learn(g, p, tech, source);
    update_research_progress(g, p);
}

/// [`add_tech`] but for the science carried over, which its callers spend.
fn learn(g: &mut Game, p: PlayerId, tech: TechId, source: TechSource) {
    let r = g.rules;
    let Some(def) = r.techs().get(tech) else { return };
    let Some(pl) = g.player(p) else { return };
    let new = !pl.tech.known.contains(tech);
    let repeatable = is_repeatable(r, tech);
    let civ_name = pl.name.clone();
    let before = super::derive::civ::era(g, p);
    let Some(pl) = g.player_mut(p, PlayerTouch::INDEX | PlayerTouch::RESEARCH) else { return };
    pl.tech.known.insert(tech);
    if repeatable {
        pl.tech.future_techs += 1;
    } else {
        pl.tech.queue.retain(|&t| t != tech);
    }
    pl.tech.progress.remove(&tech);
    let future = pl.tech.future_techs;
    if new || repeatable {
        let label =
            if repeatable { format!("{} {future}", def.name) } else { def.name.to_string() };
        let mut text = format!("{civ_name} {} {label}.", source.verb());
        let bits = unlocks_text(r, tech);
        if !bits.is_empty() {
            text.push_str(&format!(" Unlocks {bits}."));
        }
        let data = EventData { tech: Some(tech), ..EventData::default() };
        g.emit(EngineEvent::Tech, &text, Some(PlayerSet::single(p)), None, data, &[]);
    }
    // The uniques the tech triggers, and `upon discovering [tech]` (research.py:315-321).
    let note = format!("due to researching {}", def.name);
    let site = TriggerSite::civ(p);
    super::triggers::on_gain(g, &def.uniques, &site, Some(&note));
    super::triggers::fire(g, &site, &TriggerEvent::Research(tech), true, Some(&note));
    obsolete_queue(g, p, tech);
    let after = super::derive::civ::era(g, p);
    if after > before {
        enter_era(g, p, before, after);
    }
    // Yields may have changed: every city of the civilization reassigns its citizens
    // (research.py:326-328).
    g.flag_cities_of(p);
}

/// Adds techs with no announcement and nothing a new tech triggers, as setup and the city-states'
/// catch-up do (`research.add_tech_silently`, `research.py:247-251`); techs already known are
/// skipped. Python's `g.invalidate()` is the `INDEX` touch.
pub(crate) fn add_tech_silently(g: &mut Game, p: PlayerId, techs: &[TechId]) {
    let missing: Vec<TechId> = techs.iter().copied().filter(|&t| !g.has_tech(p, Some(t))).collect();
    if missing.is_empty() {
        return;
    }
    if let Some(pl) = g.player_mut(p, PlayerTouch::INDEX) {
        for t in missing {
            pl.tech.known.insert(t);
        }
    }
}

/// Grants a free tech the civilization chooses among those it could research now
/// (`research.free_tech`, `research.py:234-244`); what it reports.
///
/// # Errors
/// No free tech to choose, or a tech it could not research now.
pub fn plan_free_tech(
    g: &Game,
    p: PlayerId,
    tech: Option<TechId>,
    asked: &str,
) -> Result<TechId, ActionError> {
    if g.player(p).is_none_or(|x| x.tech.free_techs <= 0) {
        return Err(ActionError::rule("You have no free technologies to choose."));
    }
    tech.filter(|&t| can_research(g, p, t)).ok_or_else(|| {
        ActionError::rule(format!("{asked} cannot be chosen now (it must be researchable next)."))
    })
}

/// What a tech unlocks, as the announcement lists it: "units: Warrior; reveals: Iron"
/// (`research.py:305-311`).
fn unlocks_text(r: &Ruleset, tech: TechId) -> String {
    let Some(u) = r.derived().unlocks.get(tech) else { return String::new() };
    let mut bits = Vec::new();
    let mut part = |label: &str, names: Vec<&str>| {
        if !names.is_empty() {
            bits.push(format!("{label}: {}", names.join(", ")));
        }
    };
    part("units", u.units.iter().filter_map(|&x| r.name(x)).collect());
    part("buildings", u.buildings.iter().filter_map(|&x| r.name(x)).collect());
    part("improvements", u.improvements.iter().filter_map(|&x| r.name(x)).collect());
    part("reveals", u.reveals.iter().filter_map(|&x| r.name(x)).collect());
    bits.join("; ")
}

/// The units a new tech makes obsolete leave the civilization's production queues, each replaced
/// by the unit it upgrades to (the civilization's own version of it), if it has one, with a word
/// to the civilization (`research._obsolete_queue`, `research.py:333-354`).
fn obsolete_queue(g: &mut Game, p: PlayerId, tech: TechId) {
    let r = g.rules;
    let cities: Vec<CityId> = g.state().cities().of(p).to_vec();
    for c in cities {
        let Some(city) = g.city(c) else { continue };
        let mut queue = city.queue.clone();
        let mut changed: Vec<(Constructible, Option<Constructible>)> = Vec::new();
        let mut kept = smallvec::SmallVec::<[Constructible; 4]>::new();
        for item in queue.drain(..) {
            match item {
                Constructible::Unit(u) if r.base_units()[u].obsolete_tech == Some(tech) => {
                    let up = r.base_units()[u].upgrades_to.map(|x| {
                        Constructible::Unit(super::cities::construction::equivalent_unit(g, p, x))
                    });
                    if let Some(x) = up {
                        kept.push(x);
                    }
                    changed.push((item, up));
                }
                other => kept.push(other),
            }
        }
        if changed.is_empty() {
            continue;
        }
        let (name, at) = (city.name.clone(), city.tile());
        if let Some(x) = g.city_mut(c, CityTouch::CORE) {
            x.queue = kept;
        }
        for (old, new) in changed {
            let old = super::cities::construction::item_name(r, old);
            let text = match new {
                Some(n) => format!(
                    "{name} changed production from {old} to {}.",
                    super::cities::construction::item_name(r, n)
                ),
                None => format!("{old} became obsolete and was removed from {name}'s queue."),
            };
            g.emit(
                EngineEvent::ProductionInvalid,
                &text,
                Some(PlayerSet::single(p)),
                Some(at),
                EventData::default(),
                &[],
            );
        }
    }
}

/// A civilization enters a new era (`research._enter_era`, `research.py:357-376`): a major's is
/// announced to everyone, with the policy branches it opens told to it.
pub fn enter_era(g: &mut Game, p: PlayerId, before: EraId, after: EraId) {
    let r = g.rules;
    let Some(pl) = g.player(p) else { return };
    let (major, name) = (pl.is_major(), pl.name.clone());
    if major {
        let era_name = &r.eras()[after].name;
        let data = EventData { player: Some(p), era: Some(after), ..EventData::default() };
        g.emit(
            EngineEvent::Era,
            &format!("{name} has entered the {era_name}."),
            None,
            None,
            data,
            &[],
        );
        for (_, def) in r.policies().iter() {
            if let PolicyKind::Branch { era, .. } = def.kind
                && era == after
            {
                g.emit(
                    EngineEvent::PolicyAvailable,
                    &format!("The {} policy branch is now available.", def.name),
                    Some(PlayerSet::single(p)),
                    None,
                    EventData::default(),
                    &[],
                );
            }
        }
    }
    // The era's uniques, and `upon entering the [era]` (research.py:368-376).
    for n in (before.0 + 1)..=after.0 {
        let era = EraId(n);
        let Some(def) = r.eras().get(era) else { continue };
        let note = format!("due to entering the {}", def.name);
        let site = TriggerSite::civ(p);
        super::triggers::on_gain(g, &def.uniques, &site, Some(&note));
        super::triggers::fire(g, &site, &TriggerEvent::EnteringEra(era), true, Some(&note));
    }
}

/// The Korean boost (`research.research_agreement_boost`, `research.py:388-392`): half the
/// median cost of what the civilization could research, as science.
pub fn research_agreement_boost(g: &mut Game, p: PlayerId) {
    let boost = num::round_half_even(0.5 * median_available_cost(g, p));
    if boost != 0.0 {
        add_science(g, p, boost);
    }
}

// ---- Forgetting a tech (scenario.py:155-174) --------------------------------------------------------

/// A civilization forgets a tech and every tech that needs it, however indirectly
/// (`scenario.py:155-174`). Returns them all, the tech itself included whether or not it was
/// known, sorted by name. The research queue and the future techs are left as they are, as
/// Python left them.
pub fn remove_tech(g: &mut Game, p: PlayerId, tech: TechId) -> Vec<TechId> {
    let r = g.rules;
    let known: Vec<TechId> =
        g.player(p).map(|pl| pl.tech.known.iter().collect()).unwrap_or_default();
    let mut drop = vec![tech];
    loop {
        let before = drop.len();
        for &t in &known {
            let needs_dropped = r
                .techs()
                .get(t)
                .is_some_and(|d| d.prerequisites.iter().any(|pre| drop.contains(pre)));
            if !drop.contains(&t) && needs_dropped {
                drop.push(t);
            }
        }
        if drop.len() == before {
            break;
        }
    }
    if let Some(pl) = g.player_mut(p, PlayerTouch::INDEX) {
        for &t in &drop {
            pl.tech.known.remove(t);
        }
    }
    drop.sort_by(|a, b| r.name(*a).cmp(&r.name(*b)));
    drop
}

// ---- The tools (tools.py:833-859) ---------------------------------------------------------------

/// A tech named loosely, as the tools take one (`rules.resolve("tech", ...)`).
fn resolve_tech(g: &Game, v: &Value) -> (Option<TechId>, String) {
    let text = py::str_of(v);
    (g.rules.resolve::<TechId>(&text), text)
}

/// `set_research`: what to research, or a far goal whose prerequisites come first
/// (`tools.set_research`, `research.set_research`, `research.py:109-135`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SetResearch {
    pub tech: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append: Option<Value>,
}

impl Rule for SetResearch {
    type Plan = Vec<TechId>;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError> {
        let (tech, text) = resolve_tech(g, &self.tech);
        let tech = tech.ok_or_else(|| {
            ActionError::new(
                ErrCode::BadParam,
                format!("Unknown technology '{}'. Use names like 'Bronze Working'.", echo(&text)),
            )
        })?;
        plan_research(g, pid, tech, self.append.as_ref().is_some_and(py::truthy))
    }

    fn apply(self, g: &mut Game, pid: PlayerId, path: Self::Plan) -> OutcomeSpec {
        apply_research(g, pid, &path);
        OutcomeSpec::render(move |g| research_result(g, pid, &path))
    }
}

/// `dequeue_research`: a tech comes off the research queue with every queued tech that needs it
/// (`tools.dequeue_research`, `research.py:138-157`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DequeueResearch {
    pub tech: Value,
}

impl Rule for DequeueResearch {
    type Plan = (Vec<TechId>, Vec<TechId>);

    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError> {
        let (tech, text) = resolve_tech(g, &self.tech);
        plan_dequeue(g, pid, tech, &text)
    }

    fn apply(self, g: &mut Game, pid: PlayerId, (kept, removed): Self::Plan) -> OutcomeSpec {
        apply_dequeue(g, pid, kept);
        OutcomeSpec::render(move |g| {
            let r = g.rules();
            let queue: Vec<&str> = g
                .player(pid)
                .map(|x| x.tech.queue.iter().filter_map(|&t| r.name(t)).collect())
                .unwrap_or_default();
            let removed: Vec<&str> = removed.iter().filter_map(|&t| r.name(t)).collect();
            json!({
                "removed": removed,
                "researching": current(g, pid).and_then(|t| r.name(t)),
                "queue": queue,
            })
        })
    }
}

/// `choose_free_tech`: spends a free tech on one the civilization could research now
/// (`tools.choose_free_tech`, `research.free_tech`, `research.py:234-244`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChooseFreeTech {
    pub tech: Value,
}

impl Rule for ChooseFreeTech {
    type Plan = TechId;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<Self::Plan, ActionError> {
        let (tech, text) = resolve_tech(g, &self.tech);
        plan_free_tech(g, pid, tech, &text)
    }

    fn apply(self, g: &mut Game, pid: PlayerId, tech: Self::Plan) -> OutcomeSpec {
        if let Some(pl) = g.player_mut(pid, PlayerTouch::OTHER) {
            pl.tech.free_techs -= 1;
        }
        add_tech(g, pid, tech, TechSource::Free);
        OutcomeSpec::render(move |g| {
            json!({
                "learned": g.rules().name(tech),
                "free_techs_left": g.player(pid).map_or(0, |x| x.tech.free_techs),
            })
        })
    }
}
