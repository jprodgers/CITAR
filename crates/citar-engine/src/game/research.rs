//! Research: which techs a civilization knows, what it researches, and what learning a tech
//! brings (`research.py`).
//!
//! Package 1b-02 ports what the scenario operations need:
//! - repeatable and unresearchable techs (`research.py:14-17, 52-58`);
//! - the research path to a goal and setting the research (`research.py:81-135`), split as the
//!   action pipeline runs a tool: [`plan_research`] checks and only reads, [`apply_research`]
//!   writes and cannot fail, and [`research_result`] reads the result from the settled game;
//! - learning a tech (`add_tech`, `research.py:284-331`), as far as the systems it touches are
//!   ported, and forgetting one with every tech that needs it (`scenario.py:155-174`).
//!
//! Package 1b-07 ports the rest of research: costs, progress and overflow, the era a tech brings
//! and the units it makes obsolete in production queues. Package 1b-08 fires the uniques a tech
//! triggers. Each is marked where it belongs.
//!
//! One wording differs on purpose: Python announced a tech a scenario granted as "Rome scenario
//! Pottery.", putting the source's key where the verb goes; here it is "Rome was granted
//! Pottery." (refcheck: scenario-tech-announcement-wording).

use serde_json::{Value, json};

use super::core::has_type;
use super::derive::rev::PlayerTouch;
use super::error::ActionError;
use super::{Game, Porting, pending, pending_or};
use crate::base::ids::{PlayerId, TechId};
use crate::base::sets::PlayerSet;
use crate::rules::Ruleset;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::unique::{Ctx, UniqueType};

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
/// than one (`research.py:129-131`).
pub fn apply_research(g: &mut Game, p: PlayerId, path: &[TechId]) {
    let goal = (path.len() > 1).then(|| path[path.len() - 1]);
    if let Some(pl) = g.player_mut(p, PlayerTouch::RESEARCH) {
        pl.tech.queue = path.to_vec();
        pl.tech.goal = goal;
    }
    // update_research_progress (research.py:222-231): overflow may complete the new tech, which
    // leaves the queue then.
    pending(Porting::Pending("1b-07"));
}

/// What setting the research reports (`research.py:132-135`), read after the write and the
/// settle, since overflow may already have completed the first tech: what is researched now,
/// the turns it takes and the queue; and, when `path` (what [`plan_research`] made) is more than
/// one tech, its goal and the path itself.
#[must_use]
pub fn research_result(g: &Game, p: PlayerId, path: &[TechId]) -> Value {
    let r = g.rules;
    let names = |ts: &[TechId]| -> Vec<&str> { ts.iter().filter_map(|&t| r.name(t)).collect() };
    let queue: &[TechId] = g.player(p).map_or(&[], |pl| &pl.tech.queue);
    let current = queue.first().and_then(|&t| r.name(t));
    // turns_left (research.py:160-173) needs the tech's cost and the civilization's science.
    let turns = pending_or(Porting::Pending("1b-07"), Value::Null);
    let mut out = json!({"researching": current, "turns": turns, "queue": names(queue)});
    if let (Some(&goal), Some(o)) = (path.last(), out.as_object_mut())
        && path.len() > 1
    {
        o.insert("goal".into(), json!(r.name(goal)));
        o.insert("path".into(), json!(names(path)));
    }
    out
}

/// A civilization learns a tech (`research.py:284-331`): it joins the known techs (a repeatable
/// one counts once more), leaves the research queue, and is announced to the civilization with
/// what it unlocks.
pub fn add_tech(g: &mut Game, p: PlayerId, tech: TechId, source: TechSource) {
    let r = g.rules;
    let Some(def) = r.techs().get(tech) else { return };
    let Some(pl) = g.player(p) else { return };
    let new = !pl.tech.known.contains(tech);
    let repeatable = is_repeatable(r, tech);
    let civ_name = pl.name.clone();
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
    // The uniques the tech triggers, and `upon discovering [tech]` (research.py:312-317).
    pending(Porting::Pending("1b-08"));
    // Obsolete units leave production queues, and a new era is entered (research.py:318-321).
    pending(Porting::Pending("1b-07"));
    // Yields may have changed: every city of the civilization reassigns its citizens
    // (research.py:322-324).
    let cities: Vec<_> = g.st.cities().of(p).to_vec();
    for c in cities {
        g.pending.flag_city(c);
    }
    // update_research_progress (research.py:326).
    pending(Porting::Pending("1b-07"));
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
