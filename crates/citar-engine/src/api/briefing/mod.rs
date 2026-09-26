//! What a language model reads to play its turn (DESIGN.md 8.1; package 1d-03): the briefing,
//! the turn's progress and the ASCII map.
//!
//! Replaces `citar/engine/briefing.py`: [`briefing`] (`briefing.py:491-701`), [`turn_progress`]
//! (`briefing.py:441-485`), [`alerts`] (`briefing.py:319-326`, the models' text of the alerts
//! `api::views::alerts` builds) and [`map::ascii_map`] (`briefing.py:28-142`). The briefing is
//! written to be sufficient: an agent that reads it should not need a dozen queries before it can
//! act (`briefing.py:492-502`). Everything is `&Game`: reading a briefing changes nothing
//! (property P8).
//!
//! What differs from Python, on purpose (each an entry of `refcheck/intended.toml` or
//! `tests/rules/intended.toml`, cited where it is made):
//! - a player the reader has not met is never named: whose turn it is, a city-state's ally, the
//!   owner of a resource on a tile it explored long ago. Each is "Unknown Civilization" or
//!   "Unknown City-State", as the events name them (`briefing-names-only-known-players`);
//! - the points of interest leave out the cities of civilizations and city-states the reader
//!   has not met, which it cannot have seen, where Python named them and their owners
//!   (`briefing-lists-only-cities-it-could-have-seen`);
//! - the civilizations it has met are listed by player id, and its policies branch by branch,
//!   a city's specialists and a religion's beliefs in the ruleset's order, where Python kept the
//!   order they were met, adopted, assigned and chosen in, a history no state keeps
//!   (`lists-in-rule-order`).
//!
//! What the Python engine got from its own numbers, Rust gets from its own: Marble's bonus in
//! its own city only (`marble-bonus-in-its-own-city`), a city's food as its total is
//! (`city-view-rounds-its-own-sums`), a civilian at 1 health rather than 0
//! (`civilians-at-zero-health`); the refcheck group `briefing` explains them where they show.

use crate::api::views::alerts::{alert_items, bombard_targets};
use crate::api::views::empire::strategic_resources;
use crate::api::views::tiles::resource_seen;
use crate::base::fmt::{PyFloat, PyRound};
use crate::base::ids::{PlayerId, PolicyId, UnitId};
use crate::base::num;
use crate::base::stats::Stat;
use crate::game::cities::construction::item_name;
use crate::game::cities::founding::found_check;
use crate::game::cities::stats::{
    current_construction, food_to_next_pop, max_health, production_cost, turns_to_build,
};
use crate::game::city_states::influence as csi;
use crate::game::derive::stats as memo;
use crate::game::diplomacy::negotiation::negotiation_view;
use crate::game::diplomacy::relations::{denounced, has_pact, is_friends};
use crate::game::events::{UNKNOWN_CIV, UNKNOWN_CS};
use crate::game::units::promotions::can_promote;
use crate::game::{Game, movement, policies, query, religion, research, units, victory};
use crate::rules::defs::PolicyKind;
use crate::state::chronicle::{EngineEvent, EventType};
use crate::state::cities::Constructible;
use crate::state::diplo::NegStatus;
use crate::state::units::Activity;
use crate::unique::UniqueType;

pub mod map;
mod orders;

pub use self::map::{MAX_RADIUS, MIN_RADIUS, anchor, ascii_map};

/// How many events since the reader's last turn a briefing lists (`briefing.py:641`).
const EVENTS_SHOWN: usize = 40;
/// How far back it looks for them (`Game.events_for`'s default limit, `game.py:891`).
const EVENTS_SCANNED: usize = 200;
/// How many city-states it names (`briefing.py:672`).
const CITY_STATES_SHOWN: usize = 12;
/// How many recent messages it quotes (`briefing.py:674`), and the characters of each.
const MESSAGES_SHOWN: usize = 10;
const MESSAGE_CHARS: usize = 600;
/// How many active deals it lists (`briefing.py:684`).
const DEALS_SHOWN: usize = 5;
/// The characters of the notebook it ends with, its end kept (`briefing.py:700`).
const NOTEBOOK_CHARS: usize = 3000;
/// How many extra entries of the local map it lists (`briefing.py:633`).
const LOCAL_ENTRIES_SHOWN: usize = 20;
/// The rows above and below the capital its local map shows (`briefing.py:627`).
const LOCAL_RADIUS: i64 = 5;

/// A player's name as `viewer` may read it (DESIGN.md 8.4): its own, anyone's it has met, the
/// barbarians', whom nobody meets; otherwise "Unknown Civilization" or "Unknown City-State", as
/// the events name them.
// refcheck: briefing-names-only-known-players
pub(crate) fn civ_name(g: &Game, viewer: PlayerId, p: PlayerId) -> &str {
    if p == viewer || g.has_met(viewer, p) || g.is_barbarian(p) {
        g.player(p).map_or("", |x| &x.name)
    } else if g.is_city_state(p) {
        UNKNOWN_CS
    } else {
        UNKNOWN_CIV
    }
}

/// The last `n` characters of `s`, whole characters (Python's `s[-n:]`).
fn tail_chars(s: &str, n: usize) -> &str {
    let count = s.chars().count();
    match s.char_indices().nth(count.saturating_sub(n)) {
        Some((i, _)) if count > n => &s[i..],
        _ => s,
    }
}

/// The first `n` characters of `s` (Python's `s[:n]`).
fn head_chars(s: &str, n: usize) -> &str {
    s.char_indices().nth(n).map_or(s, |(i, _)| &s[..i])
}

/// The problems of `pid`'s turn, each with the tool call that deals with it, as a model reads
/// them at the top of its briefing (`briefing.alerts`).
#[must_use]
pub fn alerts(g: &Game, pid: PlayerId) -> Vec<String> {
    alert_items(g, pid).into_iter().map(|a| a.llm).collect()
}

/// The event id at the end of `pid`'s last turn, so a briefing can say what happened since
/// (`briefing._last_turn_end_event`); 0 before it has ended one.
fn last_turn_end(g: &Game, pid: PlayerId) -> u32 {
    g.chronicle()
        .events()
        .iter()
        .rev()
        .find(|e| {
            e.kind == EventType::Engine(EngineEvent::TurnEnd)
                && e.data.as_ref().and_then(|d| d.player) == Some(pid)
        })
        .map_or(0, |e| e.id.get())
}

/// Whether an event is one a briefing leaves out: the turns' own announcements and messages,
/// which it quotes on their own (`briefing.py:637`).
fn routine(kind: &EventType) -> bool {
    match kind {
        EventType::Engine(e) => {
            matches!(e, EngineEvent::TurnStart | EngineEvent::TurnEnd | EngineEvent::Message)
        }
        EventType::Host(name) => matches!(&**name, "turn_start" | "turn_end" | "message"),
    }
}

/// The policies a civilization has adopted, the branches' finishers left out: each branch it
/// opened, then its policies, in the ruleset's order. Python listed them in the order adopted.
// refcheck: lists-in-rule-order
fn adopted_policies(g: &Game, pid: PlayerId) -> Vec<&str> {
    let Some(pl) = g.player(pid) else { return Vec::new() };
    let r = g.rules();
    let defs = r.policies();
    let adopted = &pl.policy.adopted;
    let mut order: Vec<PolicyId> = Vec::new();
    for (b, def) in defs.iter() {
        let PolicyKind::Branch { members, .. } = &def.kind else { continue };
        if adopted.contains(b) {
            order.push(b);
        }
        order.extend(members.iter().copied().filter(|&m| adopted.contains(m)));
    }
    // A policy no branch lists, which only a mod could have, comes last.
    let rest: Vec<PolicyId> = adopted.iter().filter(|p| !order.contains(p)).collect();
    order.extend(rest);
    order
        .into_iter()
        .filter(|&p| !matches!(defs[p].kind, PolicyKind::Member { finisher: true, .. }))
        .map(|p| &*defs[p].name)
        .collect()
}

/// The briefing's head: the turn, the treasury and yields, happiness, score and policies, the
/// strategic resources and the religion (`briefing.py:510-543`).
fn head(g: &Game, pid: PlayerId, out: &mut Vec<String>) {
    let Some(p) = g.player(pid) else { return };
    let r = g.rules();
    let era = r.name(query::era(g, pid)).unwrap_or("");
    let note = if g.current() == pid {
        "YOUR TURN".to_owned()
    } else {
        // refcheck: briefing-names-only-known-players
        format!("waiting ({}'s turn)", civ_name(g, pid, g.current()))
    };
    out.push(format!(
        "=== TURN {}/{} ({}) — {} (player {}, {}) — {era} — {note} ===",
        g.turn(),
        g.total_turns(),
        g.year_text(None),
        p.name,
        pid.0,
        r.name(p.nation).unwrap_or("")
    ));
    let grid = g.grid();
    if grid.wraps() {
        let (w, h) = (grid.width(), grid.height());
        let mut axes = Vec::new();
        let mut seams = Vec::new();
        if grid.wrap_x() {
            axes.push("east-west");
            seams.push(format!("x={} is next to x=0", w - 1));
        }
        if grid.wrap_y() {
            axes.push("north-south");
            seams.push(format!("y={} is next to y=0", h - 1));
        }
        out.push(format!(
            "The map ({w}x{h}) wraps {}: moving off one edge comes back on the opposite edge \
             ({}). Distances are measured the short way round.",
            axes.join(" and "),
            seams.join("; ")
        ));
    }
    let st = memo::civ_stats(g, pid).total;
    let hap = memo::happiness(g, pid);
    let research = match research::current(g, pid) {
        None => "nothing (choose with set_research!)".to_owned(),
        Some(cur) => {
            let done = num::trunc_i64(p.tech.progress.get(&cur).copied().unwrap_or(0.0));
            let turns = research::turns_left(g, pid, None)
                .filter(|&n| n != 0)
                .map_or_else(|| "?".to_owned(), |n| n.to_string());
            let mut s = format!(
                "{} ({done}/{}, {turns} turns)",
                r.techs()[cur].name,
                research::tech_cost(g, pid, cur)
            );
            if p.tech.queue.len() > 1 {
                let next: Vec<&str> =
                    p.tech.queue.iter().skip(1).take(3).map(|&t| &*r.techs()[t].name).collect();
                s.push_str(&format!(", then {}", next.join(", ")));
            }
            s
        }
    };
    out.push(format!(
        "Gold {} ({:+.0}/turn) · Science {:.0}/turn → {research}",
        num::trunc_i64(p.econ.gold),
        PyFloat(st[Stat::Gold]),
        PyFloat(st[Stat::Science])
    ));
    let golden = if p.econ.golden_age_turns != 0 {
        format!("ACTIVE {} turns", p.econ.golden_age_turns)
    } else {
        format!("{} pts", num::trunc_i64(p.econ.golden_age_points))
    };
    out.push(format!(
        "Culture {} ({:+.0}/turn; next policy {}) · Faith {} ({:+.0}/turn) · Happiness {} ({}) · \
         Golden age {golden}",
        num::trunc_i64(p.econ.culture),
        PyFloat(st[Stat::Culture]),
        policies::culture_cost(g, pid, None),
        num::trunc_i64(p.econ.faith),
        PyFloat(st[Stat::Faith]),
        hap.total,
        hap.status()
    ));
    // Sorted by name, as Python's `happiness` sorted them.
    let mut lux: Vec<&str> = hap.luxury_types.iter().filter_map(|&x| r.name(x)).collect();
    lux.sort();
    let pol = adopted_policies(g, pid);
    out.push(format!(
        "Luxuries: {} · Score {} · Policies: {}",
        if lux.is_empty() { "none".to_owned() } else { lux.join(", ") },
        victory::score(g, pid).total,
        if pol.is_empty() { "none".to_owned() } else { pol.join(", ") }
    ));
    let strat: Vec<String> = strategic_resources(g, pid)
        .into_iter()
        .filter(|&(res, v)| {
            resource_seen(g, Some(pid), res) && (v.sources != 0 || v.used != 0 || v.imported != 0)
        })
        .map(|(res, v)| {
            let mut s = format!(
                "{} {} free ({} src, {} used",
                r.resources()[res].name,
                v.available,
                v.sources,
                v.used
            );
            if v.imported != 0 {
                s.push_str(&format!(", +{} in", v.imported));
            }
            if v.exported != 0 {
                s.push_str(&format!(", -{} out", v.exported));
            }
            s.push(')');
            s
        })
        .collect();
    if !strat.is_empty() {
        out.push(format!("Strategic: {}", strat.join(" · ")));
    }
    if g.religion_enabled()
        && let Some(rel) = p.religion.founded
    {
        let beliefs: Vec<&str> =
            religion::all_beliefs(g, rel).into_iter().map(|b| &*r.beliefs()[b].name).collect();
        out.push(format!(
            "Religion: {} ({}) — beliefs: {}",
            religion::display_name(g, rel),
            p.religion.progress.name(),
            beliefs.join(", ")
        ));
    }
    let warnings = alerts(g, pid);
    if !warnings.is_empty() {
        out.push("\nALERTS (handle these first):".to_owned());
        out.extend(warnings.into_iter().map(|w| format!("  ! {w}")));
    }
}

/// The reader's cities, each with its size, yields, growth and what it builds
/// (`briefing.py:549-585`).
fn cities(g: &Game, pid: PlayerId, out: &mut Vec<String>) {
    let Some(p) = g.player(pid) else { return };
    let r = g.rules();
    let list: Vec<_> = g.player_cities(pid).collect();
    out.push(format!("\nCITIES ({}):", list.len()));
    if list.is_empty() {
        out.push("  none — found a city with a Settler (found_city)!".to_owned());
    }
    for c in list {
        let id = c.id();
        let tot = memo::city_stats(g, id).total;
        let food = tot[Stat::Food];
        let (x, y) = g.xy(c.tile());
        let grow = if food > 0.0 {
            // Python's -(-a // b), each side cut to an integer first.
            let need = num::trunc_i64(f64::from(food_to_next_pop(g, id)) - c.food);
            let per = num::trunc_i64(food).max(1);
            format!("grows in {}", -num::floor_div(-need, per))
        } else if food < 0.0 {
            "STARVING".to_owned()
        } else {
            "stagnant".to_owned()
        };
        let mut build = match current_construction(c) {
            Some(Constructible::Perpetual(q)) => format!("converting production to {}", q.name()),
            Some(item) => format!(
                "building {} ({}/{}, {} turns)",
                item_name(r, item),
                num::trunc_i64(c.progress.get(&item).copied().unwrap_or(0.0)),
                production_cost(g, pid, item, Some(id)),
                turns_to_build(g, id, item)
            ),
            None if c.puppet => "puppet (builds on its own)".to_owned(),
            None => "IDLE — nothing in production!".to_owned(),
        };
        if c.queue.len() > 1 {
            let rest: Vec<&str> = c.queue.iter().skip(1).map(|&q| item_name(r, q)).collect();
            build.push_str(&format!(" then {}", rest.join(", ")));
        }
        let mut tags: Vec<String> = Vec::new();
        if p.capital == Some(id) {
            tags.push("capital".to_owned());
        }
        if c.puppet {
            tags.push("puppet".to_owned());
        }
        if c.resistance != 0 {
            tags.push(format!("resistance {}", c.resistance));
        }
        if c.razing {
            tags.push("RAZING".to_owned());
        }
        let most = max_health(g, id);
        if c.health < most {
            tags.push(format!("HP {}/{most}", c.health));
        }
        let specialists: Vec<String> = c
            .specialists
            .iter()
            .enumerate()
            .filter(|&(_, &n)| n > 0)
            .filter_map(|(i, &n)| {
                let s = crate::base::ids::SpecialistId(u8::try_from(i).ok()?);
                Some(format!("{} {n}", r.specialists().get(s)?.name))
            })
            .collect();
        if !specialists.is_empty() {
            tags.push(format!("specialists {}", specialists.join(", ")));
        }
        let tags = if tags.is_empty() { String::new() } else { format!(" {}", tags.join(", ")) };
        out.push(format!(
            "  [#{}] {} ({x},{y}) pop {}{tags} · food {:+.0} ({grow}) · prod {:.0} · gold {:.0} · \
             sci {:.0} · cul {:.0} · faith {:.0} · {build}",
            id.get(),
            c.name,
            c.pop,
            PyFloat(food),
            PyFloat(tot[Stat::Production]),
            PyFloat(tot[Stat::Gold]),
            PyFloat(tot[Stat::Science]),
            PyFloat(tot[Stat::Culture]),
            PyFloat(tot[Stat::Faith])
        ));
    }
}

/// The reader's units, civilians first, each with its movement and orders; `*` marks those
/// still wanting orders (`briefing.py:587-609`). Returns them in that order.
fn unit_lines(g: &Game, pid: PlayerId, out: &mut Vec<String>) -> Vec<UnitId> {
    let r = g.rules();
    let sc = f64::from(r.constants().move_scale);
    let mut list: Vec<(bool, UnitId)> =
        g.player_units(pid).map(|u| (r.base_units()[u.base].military, u.id())).collect();
    list.sort();
    out.push(format!("\nUNITS ({}) — '*' = needs orders:", list.len()));
    for &(_, id) in &list {
        let Some(u) = g.unit(id) else { continue };
        let (x, y) = g.xy(u.tile());
        let idle = orders::needs_orders(g, id);
        let mut act = match u.activity {
            Some(a) => a.name().to_owned(),
            None if u.moves > 0 => "ready".to_owned(),
            None => "done".to_owned(),
        };
        if matches!(u.activity, Some(Activity::Explore | Activity::Goto | Activity::Automate))
            && u.moves <= 0
        {
            act.push_str(" (already moved this turn)");
        }
        let steps = g.state().tiles().builds(u.tile());
        match u.activity {
            Some(a @ (Activity::Build | Activity::Automate)) if !steps.is_empty() => {
                let work: Vec<String> = steps
                    .iter()
                    .filter(|s| s.turns_left >= 0)
                    .map(|s| {
                        format!("{} ({} turns)", r.improvements()[s.improvement].name, s.turns_left)
                    })
                    .collect();
                let auto = if a == Activity::Automate { "automated, " } else { "" };
                act = format!("{auto}building {}", work.join(" then "));
            }
            Some(Activity::Goto) if u.goto.is_some() => {
                act = format!("moving to {}", u.goto.map(|t| g.fmt_xy(t)).unwrap_or_default());
            }
            _ => {}
        }
        let mut extra: Vec<String> = Vec::new();
        if u.hp < 100 {
            extra.push(format!("hp {}", u.hp));
        }
        if can_promote(g, id) {
            extra.push("PROMOTION READY".to_owned());
        }
        if movement::is_embarked(g, id) {
            extra.push("embarked".to_owned());
        }
        let extra =
            if extra.is_empty() { String::new() } else { format!(" · {}", extra.join(", ")) };
        out.push(format!(
            "  {} [#{}] {} ({x},{y}) moves {}/{} · {act}{extra}",
            if idle { '*' } else { ' ' },
            id.get(),
            r.base_units()[u.base].name,
            PyRound(f64::from(u.moves) / sc, 1),
            PyRound(f64::from(movement::max_moves(g, id)) / sc, 1)
        ));
    }
    list.into_iter().map(|(_, id)| id).collect()
}

/// The map around the anchor, what is in it, and what else is known of nearby
/// (`briefing.py:623-634`).
fn local_map(g: &Game, pid: PlayerId, out: &mut Vec<String>) {
    let Some(at) = anchor(g, pid) else { return };
    out.push("\nLOCAL MAP (legend: get_map legend=true):".to_owned());
    let (map, entries) = map::ascii_map_parts(g, pid, Some(g.xy(at)), LOCAL_RADIUS);
    out.push(map);
    // The reader's own units are in the list above.
    let extras: Vec<&str> = entries
        .iter()
        .map(|e| e.trim())
        .filter(|e| {
            ["resource", "unit", "city", "natural"].iter().any(|k| e.starts_with(k))
                && !e.contains("yours hp")
        })
        .take(LOCAL_ENTRIES_SHOWN)
        .collect();
    if !extras.is_empty() {
        out.push(format!("Also in this window: {}", extras.join("; ")));
    }
    out.extend(map::points_of_interest(g, pid, at));
}

/// What happened since the reader's last turn, as it may see it (`briefing.py:636-642`).
fn events(g: &Game, pid: PlayerId, out: &mut Vec<String>) {
    let since = last_turn_end(g, pid);
    let seen = g.events_for(Some(pid), since, EVENTS_SCANNED);
    let evs: Vec<_> = seen.iter().filter(|e| !routine(&e.event.kind)).collect();
    out.push("\nEVENTS since your last turn:".to_owned());
    if evs.is_empty() {
        out.push("  (none)".to_owned());
    }
    for e in &evs[evs.len().saturating_sub(EVENTS_SHOWN)..] {
        out.push(format!("  - T{}: {}", e.event.turn, e.event.text));
    }
}

/// The civilizations and city-states the reader has met, recent messages, open negotiations and
/// active deals (`briefing.py:644-684`).
fn diplomacy(g: &Game, pid: PlayerId, out: &mut Vec<String>) {
    let r = g.rules();
    let turn = g.turn();
    out.push("\nDIPLOMACY:".to_owned());
    // By player id, where Python kept the order it met them in.
    // refcheck: lists-in-rule-order
    let met: Vec<_> = g.majors(true).filter(|q| q.id() != pid && g.has_met(pid, q.id())).collect();
    if met.is_empty() {
        out.push("  You have not met any other civilization yet.".to_owned());
    }
    for qp in met {
        let q = qp.id();
        let rel = g.relation(pid, q);
        let status = match rel {
            Some(x) if x.war => "AT WAR".to_owned(),
            Some(x) if x.treaty_until >= turn => format!("peace treaty until T{}", x.treaty_until),
            _ => "peace".to_owned(),
        };
        let mut tags: Vec<&str> = Vec::new();
        if g.has_open_borders(pid, q) {
            tags.push("you grant open borders");
        }
        if g.has_open_borders(q, pid) {
            tags.push("they grant you open borders");
        }
        if is_friends(g, pid, q) {
            tags.push("friends");
        }
        if has_pact(g, pid, q) {
            tags.push("defensive pact");
        }
        if rel.is_some_and(|x| x.ra_until >= turn) {
            tags.push("research agreement");
        }
        if denounced(g, q, pid) {
            tags.push("they denounced you");
        }
        let tags = if tags.is_empty() { String::new() } else { format!("; {}", tags.join(", ")) };
        out.push(format!(
            "  - player {} {} ({}): {status}; score {}; {} cities{tags}",
            q.0,
            qp.name,
            r.name(qp.nation).unwrap_or(""),
            victory::score(g, q).total,
            g.player_cities(q).count()
        ));
    }
    let css: Vec<String> = g
        .city_states(true)
        .filter(|q| g.has_met(pid, q.id()))
        .take(CITY_STATES_SHOWN)
        .map(|q| {
            let id = q.id();
            let data = q.city_state.as_deref();
            let ty = data
                .and_then(|d| d.cs_type)
                .and_then(|t| r.city_state_types().get(t))
                .map_or("None", |t| &*t.name);
            let ally = match data.and_then(|d| d.ally()) {
                // refcheck: briefing-names-only-known-players
                Some(a) if a != pid => format!(", ally {}", civ_name(g, pid, a)),
                _ => String::new(),
            };
            format!(
                "{} #{} ({ty}, {}, influence {:.0}{ally})",
                q.name,
                id.0,
                csi::relationship(g, id, pid).name(),
                PyFloat(csi::influence(g, id, pid))
            )
        })
        .collect();
    if !css.is_empty() {
        out.push(format!("  City-states: {}", css.join("; ")));
    }
    let msgs: Vec<_> = g
        .chronicle()
        .messages()
        .iter()
        .filter(|m| m.to.contains(pid) && m.turn >= turn - 1)
        .collect();
    for m in &msgs[msgs.len().saturating_sub(MESSAGES_SHOWN)..] {
        out.push(format!(
            "  ✉ T{} from {}: \"{}\"",
            m.turn,
            g.player(m.from).map_or("", |x| &x.name),
            head_chars(&m.text, MESSAGE_CHARS)
        ));
    }
    for n in g.negotiations() {
        if n.status != NegStatus::Open || (n.initiator != pid && n.responder != pid) {
            continue;
        }
        let v = negotiation_view(g, n, pid);
        let with = v.get("with_name").and_then(|x| x.as_str()).unwrap_or("");
        let prop = v
            .get("current_proposal")
            .and_then(|p| p.get("summary"))
            .and_then(|s| s.as_str())
            .unwrap_or("no concrete proposal");
        let who = if v.get("your_move").and_then(serde_json::Value::as_bool) == Some(true) {
            "YOUR MOVE".to_owned()
        } else {
            format!("waiting for {with}")
        };
        out.push(format!("  ⚖ negotiation #{} with {with} ({who}): {prop}", n.id.get()));
    }
    let active: Vec<&str> = g
        .state()
        .diplo()
        .deals
        .iter()
        .filter(|d| d.active && d.parties.contains(&pid))
        .map(|d| &*d.summary)
        .collect();
    if !active.is_empty() {
        out.push(format!(
            "  Active deals: {}",
            active[active.len().saturating_sub(DEALS_SHOWN)..].join(" | ")
        ));
    }
}

/// The whole turn briefing a model reads at the start of its turn (`briefing.briefing`): its
/// empire, the alerts, its cities and units with the orders open to them, the techs to choose
/// from, the map around its capital, the events since its last turn, diplomacy, what is left to
/// do and its notebook. Empty for a player the game lacks.
#[must_use]
pub fn briefing(g: &Game, pid: PlayerId) -> String {
    let Some(p) = g.player(pid) else { return String::new() };
    let mut lines: Vec<String> = Vec::new();
    head(g, pid, &mut lines);
    cities(g, pid, &mut lines);
    let units = unit_lines(g, pid, &mut lines);
    lines.extend(orders::unit_options(g, pid, &units));
    lines.extend(orders::city_options(g, pid));
    let cur = research::current(g, pid);
    if cur.is_none() {
        lines.extend(orders::available_techs(g, pid));
    }
    local_map(g, pid, &mut lines);
    events(g, pid, &mut lines);
    diplomacy(g, pid, &mut lines);
    let mut todo: Vec<&str> = Vec::new();
    if cur.is_none() && !research::available_techs(g, pid).is_empty() {
        todo.push("choose research (set_research)");
    }
    if g.player_cities(pid).any(|c| c.queue.is_empty() && !c.puppet) {
        todo.push("set production in idle cities");
    }
    if units.iter().any(|&u| orders::needs_orders(g, u)) {
        todo.push("give orders to units marked '*' (or fortify/sleep them)");
    }
    if units.iter().any(|&u| can_promote(g, u)) {
        todo.push("promote units that are ready");
    }
    if policies::can_adopt_any(g, pid) {
        todo.push("adopt a social policy (adopt_policy)");
    }
    if !todo.is_empty() {
        lines.push(format!("\nTO DO: {}. Call end_turn when finished.", todo.join("; ")));
    }
    let notes = p.major.as_deref().map_or("", |m| &*m.notes);
    if !notes.is_empty() {
        lines.push(format!("\nYOUR NOTEBOOK:\n{}", tail_chars(notes, NOTEBOOK_CHARS)));
    }
    lines.join("\n")
}

/// What is still to do this turn, in one line, for a model to read after each batch of actions
/// (`briefing.turn_progress`): its cities, research, idle cities and units, promotions, a policy
/// to adopt, cities that can still bombard and open negotiations, then whether it may end its
/// turn. Empty for a player the game lacks.
#[must_use]
pub fn turn_progress(g: &Game, pid: PlayerId) -> String {
    if g.player(pid).is_none() {
        return String::new();
    }
    let r = g.rules();
    let mut parts: Vec<String> = Vec::new();
    let named: Vec<String> =
        g.player_cities(pid).map(|c| format!("{} #{}", c.name, c.id().get())).collect();
    parts.push(format!(
        "cities: {}",
        if named.is_empty() { "none yet".to_owned() } else { named.join(", ") }
    ));
    let cur = research::current(g, pid);
    let can_research = !research::available_techs(g, pid).is_empty();
    if let Some(t) = cur {
        parts.push(format!("research: {} (set)", r.techs()[t].name));
    } else if can_research {
        parts.push("research: NOT SET".to_owned());
    }
    let idle: Vec<String> = g
        .player_cities(pid)
        .filter(|c| c.queue.is_empty() && !c.puppet)
        .map(|c| format!("{} #{}", c.name, c.id().get()))
        .collect();
    if !idle.is_empty() {
        parts.push(format!("cities with no production: {}", idle.join(", ")));
    }
    let need: Vec<String> = g
        .player_units(pid)
        .filter(|u| orders::needs_orders(g, u.id()))
        .map(|u| {
            let (x, y) = g.xy(u.tile());
            let here = units::type_has(g, u.base, UniqueType::FoundCity)
                && found_check(g, pid, u.tile()).is_none();
            format!(
                "#{} {} at ({x},{y}){}",
                u.id().get(),
                r.base_units()[u.base].name,
                if here { " (can found a city here)" } else { "" }
            )
        })
        .collect();
    parts.push(format!(
        "units still needing orders: {}",
        if need.is_empty() { "none".to_owned() } else { need.join("; ") }
    ));
    let promos: Vec<String> = g
        .player_units(pid)
        .filter(|u| can_promote(g, u.id()))
        .map(|u| format!("#{}", u.id().get()))
        .collect();
    if !promos.is_empty() {
        parts.push(format!("promotions available: {}", promos.join(", ")));
    }
    if policies::can_adopt_any(g, pid) {
        parts.push("a social policy can be adopted".to_owned());
    }
    let shots: Vec<String> = bombard_targets(g, pid)
        .into_iter()
        .filter_map(|b| {
            let c = g.city(b.city)?;
            Some(format!("{} #{} -> {}", c.name, b.city.get(), g.fmt_xy(b.target)))
        })
        .collect();
    if !shots.is_empty() {
        parts.push(format!("cities that can still bombard (city_attack): {}", shots.join(", ")));
    }
    let chats: Vec<_> = g
        .negotiations()
        .iter()
        .filter(|n| n.status == NegStatus::Open && (n.initiator == pid || n.responder == pid))
        .collect();
    if !chats.is_empty() {
        // end_turn is refused while any of these is open, so the model has to hear of them.
        let list: Vec<String> = chats
            .iter()
            .map(|n| match n.awaiting {
                Some(a) if a == pid => format!("#{} YOUR MOVE", n.id.get()),
                Some(a) => format!("#{} waiting for {}", n.id.get(), civ_name(g, pid, a)),
                None => format!("#{} waiting", n.id.get()),
            })
            .collect();
        parts.push(format!("open negotiations (end_turn waits for them): {}", list.join(", ")));
    }
    let done = need.is_empty()
        && idle.is_empty()
        && (cur.is_some() || !can_research)
        && !chats.iter().any(|n| n.awaiting == Some(pid));
    let tail = if done {
        " Everything is handled — call end_turn now."
    } else {
        " Handle what remains, then call end_turn."
    };
    format!(
        "TURN PROGRESS (current state; this supersedes the briefing): {}.{tail}",
        parts.join(" | ")
    )
}

impl Game {
    /// A language model's start-of-turn briefing (DESIGN.md 8.1; [`briefing`]).
    #[must_use]
    pub fn briefing(&self, pid: PlayerId) -> String {
        briefing(self, pid)
    }

    /// What is still to do this turn, in one line ([`turn_progress`]).
    #[must_use]
    pub fn turn_progress(&self, pid: PlayerId) -> String {
        turn_progress(self, pid)
    }
}
