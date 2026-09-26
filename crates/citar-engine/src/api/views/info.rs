//! The panels a player opens beside the map: the tech tree, social policies, religion, great
//! people, victory and espionage (`views.py:505-617`, `espionage.py:501-515`).

use serde_json::{Map, Value, json};

use super::players::un_result;
use super::{known_name, rule_text};
use crate::base::ids::{BeliefId, PlayerId, PolicyId};
use crate::base::num;
use crate::game::religion::{self as rel, found, prophets};
use crate::game::victory::{self, milestones, un};
use crate::game::{Game, espionage, great_people, policies, research};
use crate::rules::defs::{BeliefKind, PolicyKind, ReligionProgress, SpyAction};
use crate::state::players::Spy;

/// The technology tree, each technology with its status (`known`, `available` or `locked`),
/// cost, prerequisites, progress, what it unlocks and its effects (`views.tech_tree`,
/// `views.py:505-523`); with what the civilization researches, its queue and its free techs.
#[must_use]
pub fn tech_tree(g: &Game, pid: PlayerId) -> Value {
    let Some(p) = g.player(pid) else { return Value::Null };
    let r = g.rules();
    let names = |ids: &mut dyn Iterator<Item = &str>| -> Value { json!(ids.collect::<Vec<_>>()) };
    let mut techs = Vec::new();
    for (t, def) in r.techs().iter() {
        let status = if g.has_tech(pid, Some(t)) {
            "known"
        } else if research::can_research(g, pid, t) {
            "available"
        } else {
            "locked"
        };
        let mut row = Map::new();
        row.insert("name".into(), json!(&*def.name));
        row.insert("era".into(), json!(r.name(def.era)));
        row.insert("status".into(), json!(status));
        row.insert("cost".into(), json!(research::tech_cost(g, pid, t)));
        row.insert(
            "prerequisites".into(),
            names(&mut def.prerequisites.iter().filter_map(|&x| r.name(x))),
        );
        let progress = p.tech.progress.get(&t).copied().unwrap_or(0.0);
        if progress != 0.0 {
            row.insert("progress".into(), json!(num::trunc_i64(progress)));
        }
        let un = &r.derived().unlocks[t];
        let mut unlocks = Map::new();
        if !un.units.is_empty() {
            unlocks.insert("units".into(), names(&mut un.units.iter().filter_map(|&x| r.name(x))));
        }
        if !un.buildings.is_empty() {
            unlocks.insert(
                "buildings".into(),
                names(&mut un.buildings.iter().filter_map(|&x| r.name(x))),
            );
        }
        if !un.improvements.is_empty() {
            unlocks.insert(
                "improvements".into(),
                names(&mut un.improvements.iter().filter_map(|&x| r.name(x))),
            );
        }
        if !un.reveals.is_empty() {
            unlocks
                .insert("reveals".into(), names(&mut un.reveals.iter().filter_map(|&x| r.name(x))));
        }
        if !unlocks.is_empty() {
            row.insert("unlocks".into(), Value::Object(unlocks));
        }
        let effects = rule_text(g, &def.uniques);
        if !effects.is_empty() {
            row.insert("effects".into(), json!(effects));
        }
        techs.push(Value::Object(row));
    }
    json!({
        "researching": research::current(g, pid).and_then(|t| r.name(t)),
        "queue": names(&mut p.tech.queue.iter().filter_map(|&t| r.name(t))),
        "free_techs": p.tech.free_techs,
        "techs": techs,
    })
}

/// A branch's finisher, adopted for free once the rest of the branch is (`<branch> Complete`).
fn finisher(g: &Game, branch: PolicyId) -> Option<PolicyId> {
    g.rules().policies().iter().find_map(|(id, d)| match d.kind {
        PolicyKind::Member { branch: b, finisher: true, .. } if b == branch => Some(id),
        _ => None,
    })
}

/// Social policies: culture, the next policy's cost, and each branch with its status, its
/// policies and their effects, and what can be adopted now (`views.policies_info`,
/// `views.py:526-544`).
#[must_use]
pub fn policies_info(g: &Game, pid: PlayerId) -> Value {
    let Some(p) = g.player(pid) else { return Value::Null };
    let r = g.rules();
    let eligible = policies::adoptable_policies(g, pid);
    let adoptable: Vec<PolicyId> =
        if policies::can_adopt_any(g, pid) { eligible.clone() } else { Vec::new() };
    let adopted = |x: PolicyId| p.policy.adopted.contains(x);
    let mut branches = Vec::new();
    for (b, def) in r.policies().iter() {
        let PolicyKind::Branch { era, members, .. } = &def.kind else { continue };
        let done = finisher(g, b);
        let status = if done.is_some_and(adopted) {
            "completed"
        } else if adopted(b) {
            "open"
        } else if adoptable.contains(&b) {
            "adoptable"
        } else if eligible.contains(&b) {
            "available"
        } else {
            "locked"
        };
        let list: Vec<Value> = members
            .iter()
            .map(|&m| {
                let md = &r.policies()[m];
                let requires: Vec<&str> = match &md.kind {
                    PolicyKind::Member { requires, .. } => {
                        requires.iter().filter_map(|&x| r.name(x)).collect()
                    }
                    PolicyKind::Branch { .. } => Vec::new(),
                };
                json!({
                    "name": &*md.name,
                    "adopted": adopted(m),
                    "adoptable": adoptable.contains(&m),
                    "requires": requires,
                    "effects": rule_text(g, &md.uniques),
                })
            })
            .collect();
        let completion = done.map(|f| rule_text(g, &r.policies()[f].uniques)).unwrap_or_default();
        branches.push(json!({
            "branch": &*def.name,
            "era": r.name(*era),
            "status": status,
            "opening_effects": rule_text(g, &def.uniques),
            "policies": list,
            "completion_effects": completion,
        }));
    }
    let mut now: Vec<&str> = adoptable.iter().filter_map(|&x| r.name(x)).collect();
    now.sort();
    let adopted_names: Vec<&str> = p.policy.adopted.iter().filter_map(|x| r.name(x)).collect();
    json!({
        "culture": num::trunc_i64(p.econ.culture),
        "next_policy_cost": policies::culture_cost(g, pid, None),
        "free_policies": p.policy.free_policies,
        "adopted": adopted_names,
        "adoptable_now": now,
        "branches": branches,
    })
}

/// Beliefs by name, each with its effects.
fn beliefs_with_effects(g: &Game, list: Vec<BeliefId>) -> Value {
    let r = g.rules();
    let m: Map<String, Value> = list
        .into_iter()
        .map(|b| (r.beliefs()[b].name.to_string(), json!(rule_text(g, &r.beliefs()[b].uniques))))
        .collect();
    Value::Object(m)
}

/// Belief names.
fn belief_names(g: &Game, list: Vec<BeliefId>) -> Value {
    let r = g.rules();
    json!(list.into_iter().filter_map(|b| r.name(b)).collect::<Vec<_>>())
}

/// Faith, pantheon and religion, the beliefs a civilization could choose, the faith its next
/// pantheon and great prophet need, and the religions of the world (`views.religion_info`,
/// `views.py:547-577`).
#[must_use]
pub fn religion_info(g: &Game, pid: PlayerId) -> Value {
    let Some(p) = g.player(pid) else { return Value::Null };
    if !g.religion_enabled() {
        return json!({"enabled": false});
    }
    let state = p.religion.progress;
    let mut m = Map::new();
    m.insert("enabled".into(), json!(true));
    m.insert("faith".into(), json!(num::trunc_i64(p.econ.faith)));
    m.insert("state".into(), json!(state.name()));
    m.insert("your_religion".into(), json!(p.religion.founded.map(|x| rel::display_name(g, x))));
    if let Some(x) = p.religion.founded {
        m.insert("your_beliefs".into(), belief_names(g, rel::all_beliefs(g, x)));
    }
    if found::can_found_pantheon(g, pid).is_none() || state == ReligionProgress::None {
        m.insert("faith_for_pantheon".into(), json!(prophets::faith_for_pantheon(g, 0)));
        let pantheon = BeliefKind::Type(crate::rules::defs::BeliefType::Pantheon);
        m.insert(
            "pantheon_beliefs_available".into(),
            beliefs_with_effects(g, rel::beliefs_available(g, pantheon)),
        );
    }
    if prophets::can_generate_prophet(g, pid, true) {
        m.insert(
            "faith_for_next_great_prophet".into(),
            json!(prophets::faith_for_next_prophet(g, pid)),
        );
    }
    m.insert("religions_remaining_to_found".into(), json!(prophets::remaining_foundable(g)));
    if matches!(
        state,
        ReligionProgress::Pantheon
            | ReligionProgress::Founding
            | ReligionProgress::Religion
            | ReligionProgress::Enhancing
    ) {
        let enhancing = matches!(state, ReligionProgress::Religion | ReligionProgress::Enhancing);
        let need = found::beliefs_to_choose(g, pid, enhancing);
        let counts: Map<String, Value> =
            need.iter().map(|&(k, n)| (k.name().to_owned(), json!(n))).collect();
        m.insert("beliefs_to_choose_when_founding_or_enhancing".into(), Value::Object(counts));
        let avail: Map<String, Value> = need
            .iter()
            .filter(|&&(k, _)| k != BeliefKind::Any)
            .map(|&(k, _)| {
                (k.name().to_owned(), beliefs_with_effects(g, rel::beliefs_available(g, k)))
            })
            .collect();
        m.insert("available_beliefs".into(), Value::Object(avail));
    }
    let world: Vec<Value> = g
        .state()
        .world()
        .religions
        .iter()
        .enumerate()
        .filter_map(|(i, x)| {
            let id = crate::base::ids::ReligionId(u8::try_from(i).ok()?);
            rel::is_major(g, id).then(|| {
                json!({
                    "religion": rel::display_name(g, id),
                    "founder": known_name(g, pid, x.founder),
                    "cities_following": rel::cities_following(g, id),
                    "beliefs": belief_names(g, rel::all_beliefs(g, id)),
                })
            })
        })
        .collect();
    m.insert("world_religions".into(), Value::Array(world));
    Value::Object(m)
}

/// Great person points toward each kind and what the next needs, the points the cities earn
/// each turn, the free great people to choose, and the golden age (`views.great_people_info`,
/// `views.py:580-596`).
#[must_use]
pub fn great_people_info(g: &Game, pid: PlayerId) -> Value {
    let Some(p) = g.player(pid) else { return Value::Null };
    let r = g.rules();
    // Python read each kind's points under its pool's name, which no points were kept under, so
    // it showed none (`views.py:586-587`); these are the points the kind has, from its cities or
    // its battles.
    // refcheck: great-people-view-shows-the-points
    let pools: Vec<Value> = great_people::great_people_types(g, pid)
        .into_iter()
        .map(|gp| {
            let pts = p.gp.points.get(&gp).or_else(|| p.gp.combat_points.get(&gp)).copied();
            json!({
                "great_person": r.name(gp),
                "points": num::trunc_i64(pts.unwrap_or(0.0)),
                "needed": great_people::points_required(g, pid, gp),
            })
        })
        .collect();
    let mut per_turn: Vec<(crate::base::ids::BaseUnitId, i64)> = Vec::new();
    for &c in g.state().cities().of(pid) {
        for (k, v) in great_people::city_gpp(g, c) {
            match per_turn.iter_mut().find(|(x, _)| *x == k) {
                Some((_, n)) => *n += v,
                None => per_turn.push((k, v)),
            }
        }
    }
    let per_turn: Map<String, Value> = per_turn
        .into_iter()
        .filter(|&(_, v)| v != 0)
        .filter_map(|(k, v)| Some((r.name(k)?.to_owned(), json!(v))))
        .collect();
    json!({
        "progress": pools,
        "points_per_turn": per_turn,
        "free_great_people_to_choose": p.gp.free,
        "great_people_earned": p.gp.earned,
        "golden_age": super::empire::golden_age(g, pid, p),
    })
}

/// Progress toward every enabled victory: the turn and its limit, the milestones, scores, the
/// original capitals it has found, the spaceship and the United Nations (`views.victory_info`,
/// `views.py:599-617`).
#[must_use]
pub fn victory_info(g: &Game, pid: PlayerId) -> Value {
    let Some(me) = g.player(pid) else { return Value::Null };
    let r = g.rules();
    let enabled: Vec<&str> =
        milestones::enabled_victories(g).map(|v| &*r.victories()[v].name).collect();
    let scores: Map<String, Value> = g
        .majors(false)
        .filter(|q| q.alive() && (q.id() == pid || g.has_met(pid, q.id())))
        .map(|q| (q.name.to_string(), json!(victory::score(g, q.id()).total)))
        .collect();
    let capitals: Vec<Value> = g
        .state()
        .cities()
        .iter()
        .filter(|c| {
            c.original_capital
                && g.player(c.founder).is_some_and(crate::state::players::Player::is_major)
                && me.explored.contains(c.tile().0)
        })
        .map(|c| json!({"city": &*c.name, "owner": known_name(g, pid, c.owner())}))
        .collect();
    let mut m = Map::new();
    m.insert("turn".into(), json!(g.turn()));
    m.insert("year".into(), json!(g.year_text(None)));
    m.insert("turn_limit".into(), json!(g.total_turns()));
    m.insert("enabled_victories".into(), json!(enabled));
    m.insert("your_progress".into(), victory::victory_progress(g, pid));
    m.insert("your_score".into(), victory::score(g, pid).to_json());
    m.insert("scores".into(), Value::Object(scores));
    m.insert("original_capitals".into(), Value::Array(capitals));
    if let Some(sci) = r.derived().known.victories.scientific
        && g.victory_enabled(sci)
    {
        m.insert("spaceship".into(), milestones::spaceship_status(g, pid).to_json(g));
    }
    let world = &g.state().world().un;
    if let Some(next) = world.next_vote {
        m.insert(
            "united_nations".into(),
            json!({
                "next_vote_turn": next,
                "votes_needed": un::votes_needed(g),
                "last_result": world.results.as_ref().map(|res| un_result(g, Some(pid), res)),
            }),
        );
    }
    Value::Object(m)
}

/// One spy as the client shows it (`espionage.spy_view`): where it is, what it does, the turns
/// left of a countdown or a theft, and the chance of a coup it could stage.
fn spy_view(g: &Game, pid: PlayerId, s: &Spy) -> Value {
    let city = s.city.and_then(|c| g.city(c));
    let mut m = Map::new();
    m.insert("name".into(), json!(&*s.name));
    m.insert("rank".into(), json!(s.rank));
    m.insert("location".into(), json!(city.map_or("hideout", |c| &*c.name)));
    m.insert("city_id".into(), json!(city.map(|c| c.id().get())));
    m.insert("action".into(), json!(s.action.name()));
    let counts = matches!(
        s.action,
        SpyAction::Moving
            | SpyAction::EstablishingNetwork
            | SpyAction::Dead
            | SpyAction::StealingTech
            | SpyAction::RiggingElections
    );
    if counts && s.turns > 0 {
        m.insert("turns".into(), json!(s.turns));
    }
    if city.is_some() && espionage::can_coup(g, pid, s) {
        let chance = espionage::coup_chance(g, pid, s, false);
        m.insert("coup_chance_percent".into(), json!(num::round_half_even_i64(chance * 100.0)));
    }
    Value::Object(m)
}

/// A civilization's spies, and whether espionage is on at all (`espionage.espionage_view`).
#[must_use]
pub fn espionage_view(g: &Game, pid: PlayerId) -> Value {
    let spies: Vec<Value> = espionage::spies(g, pid).iter().map(|s| spy_view(g, pid, s)).collect();
    json!({"enabled": g.espionage_enabled(), "spies": spies})
}
