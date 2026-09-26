//! What needs a player's attention this turn (`briefing.alert_items` and `bombard_targets`,
//! `briefing.py:168-316`): each alert with a text for people, one for models that names the tool
//! call that deals with it, and where it is. The client view shows them without the models'
//! text; package 1d-03's briefing lists the models' (`briefing.alerts`).

use serde_json::{Map, Value, json};

use crate::base::fmt::PyFloat;
use crate::base::ids::{CityId, PlayerId, TileIdx, UnitId};
use crate::base::stats::Stat;
use crate::game::cities::borders;
use crate::game::combat::{self, city as ccity, combatant};
use crate::game::derive::stats as memo;
use crate::game::religion::found;
use crate::game::units::promotions;
use crate::game::victory::un;
use crate::game::vis::sight::unit_visible_to;
use crate::game::{Game, espionage, policies, research};
use crate::rules::defs::{Domain, SpyAction};
use crate::state::diplo::NegStatus;
use crate::unique::filter::Combatant;

/// One thing that needs attention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    /// Its type: `threat`, `starving`, `research`, ...
    pub kind: &'static str,
    /// For people.
    pub text: String,
    /// For models: the exact tool call that deals with it.
    pub llm: String,
    /// Where it is on the map.
    pub tile: Option<TileIdx>,
    /// What it concerns: `city`, `unit`, `player` or `negotiation`, and its id.
    pub ids: Vec<(&'static str, u32)>,
}

impl Alert {
    /// As the client shows it (`client_view`), without the models' text; with `llm`, whole.
    #[must_use]
    pub fn to_json(&self, g: &Game, llm: bool) -> Value {
        let mut m = Map::new();
        m.insert("type".into(), json!(self.kind));
        m.insert("text".into(), json!(self.text));
        if llm {
            m.insert("llm".into(), json!(self.llm));
        }
        for &(k, id) in &self.ids {
            m.insert(k.into(), json!(id));
        }
        if let Some(t) = self.tile {
            let (x, y) = g.xy(t);
            m.insert("x".into(), json!(x));
            m.insert("y".into(), json!(y));
        }
        Value::Object(m)
    }
}

/// A city's best shot this turn: the target tile, the damage it would deal, and whether that
/// kills (`briefing.bombard_targets`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bombard {
    pub city: CityId,
    pub target: TileIdx,
    pub damage: i32,
    pub kills: bool,
}

/// Each of `p`'s cities that can still bombard this turn, with its best target: the one it
/// would kill, else the one it would hurt most, the first of equals in Python's `within` order
/// (`briefing.py:168-187`).
#[must_use]
pub fn bombard_targets(g: &Game, p: PlayerId) -> Vec<Bombard> {
    let mut out = Vec::new();
    for c in g.player_cities(p) {
        let id = c.id();
        if ccity::can_bombard(g, id).is_some() {
            continue;
        }
        let a = Combatant::City(id);
        let mut best: Option<Bombard> = None;
        // Of equal shots the first in Python's `within` order wins, as it did.
        let mut targets = ccity::bombard_targets(g, id);
        targets.sort_by_key(|&t| borders::within_order(g, c.tile(), t));
        for t in targets {
            let Some(d) = combatant::combatant_at(g, t) else { continue };
            let damage = combat::setup(g, a, c.tile(), d, false).damage_to_defender(0.5);
            let kills = damage >= combatant::hp(g, d);
            if best.is_none_or(|b| (kills, damage) > (b.kills, b.damage)) {
                best = Some(Bombard { city: id, target: t, damage, kills });
            }
        }
        out.extend(best);
    }
    out
}

/// The alerts of `p`'s turn, in Python's order (`briefing.alert_items`).
#[must_use]
#[allow(clippy::too_many_lines, reason = "one list, in the order the briefing reads it")]
pub fn alert_items(g: &Game, p: PlayerId) -> Vec<Alert> {
    let Some(pl) = g.player(p) else { return Vec::new() };
    let mut out = Vec::new();
    let mut add = |kind,
                   text: String,
                   llm: Option<String>,
                   tile: Option<TileIdx>,
                   ids: Vec<(&'static str, u32)>| {
        let llm = llm.unwrap_or_else(|| text.clone());
        out.push(Alert { kind, text, llm, tile, ids });
    };
    let gpt = memo::civ_stats(g, p).total[Stat::Gold];
    if gpt < 0.0 {
        let head = if pl.econ.gold > 0.0 {
            // Python's `int(gold // -gpt)`.
            let turns = crate::base::num::trunc_i64((pl.econ.gold / -gpt).floor());
            format!(
                "Gold is falling ({:+.0}/turn): the treasury runs out in about {turns} turns.",
                PyFloat(gpt)
            )
        } else {
            format!(
                "The treasury is empty and gold is falling ({:+.0}/turn): science suffers, and at \
                 -200 gold units are disbanded.",
                PyFloat(gpt)
            )
        };
        add(
            "gold",
            format!("{head} Fix: fewer units, Markets, gold focus, trade luxuries for gold."),
            Some(format!(
                "{head} Fix: disband obsolete units (unit_order disband), build Markets, \
                 set_city_focus gold, trade luxuries for gold."
            )),
            None,
            Vec::new(),
        );
    }
    let hap = memo::happiness(g, p).total;
    if hap < -10 {
        add(
            "happiness",
            format!(
                "Empire VERY UNHAPPY ({hap}): cities stop growing, combat penalties, rebels may \
                 appear. Fix: Temples/Colosseums, new luxury types, no new cities for now."
            ),
            None,
            None,
            Vec::new(),
        );
    } else if hap < 0 {
        add(
            "happiness",
            format!(
                "Empire unhappy ({hap}): city growth is slowed and Settlers cost more time. Fix: \
                 happiness buildings, new luxury types (improve them or trade), fewer new cities."
            ),
            None,
            None,
            Vec::new(),
        );
    }
    if pl.econ.golden_age_turns > 0 {
        add(
            "golden_age",
            format!(
                "Golden Age: {} turns left (+production, +gold, +culture).",
                pl.econ.golden_age_turns
            ),
            None,
            None,
            Vec::new(),
        );
    }
    let r = g.rules();
    let vis = g.derived().vis();
    let hostile: Vec<(UnitId, TileIdx)> = g
        .state()
        .units()
        .iter()
        .filter(|u| {
            vis.sees(p, u.tile())
                && g.at_war(p, u.owner())
                && r.base_units()[u.base].military
                && unit_visible_to(g, p, u.id())
        })
        .map(|u| (u.id(), u.tile()))
        .collect();
    let bombard = bombard_targets(g, p);
    let unit_type = |u: UnitId| g.unit(u).map_or("", |x| &*r.base_units()[x.base].name);
    for c in g.player_cities(p) {
        let (id, at) = (c.id(), c.tile());
        let shot = bombard.iter().find(|b| b.city == id);
        let near: Vec<&(UnitId, TileIdx)> =
            hostile.iter().filter(|&&(_, t)| g.grid().distance(t, at) <= 3).collect();
        if !near.is_empty() {
            let garrison = g
                .military_at(at)
                .filter(|m| m.owner() == p)
                .map_or("NONE", |m| &*r.base_units()[m.base].name);
            let who: Vec<String> = near
                .iter()
                .take(4)
                .filter_map(|&&(u, t)| {
                    let x = g.unit(u)?;
                    Some(format!(
                        "{} {} {} hp {}",
                        super::name_of(g, x.owner()),
                        unit_type(u),
                        g.fmt_xy(t),
                        x.hp
                    ))
                })
                .collect();
            let line = format!(
                "City {} #{} (HP {}) is threatened by {} hostile unit(s): {}. Garrison: {garrison}.",
                c.name,
                id.get(),
                c.health,
                near.len(),
                who.join(", ")
            );
            let (human, llm) = match shot {
                Some(b) => {
                    let (x, y) = g.xy(b.target);
                    let kills = if b.kills { ", kills it" } else { "" };
                    (
                        format!(
                            "{line} It can bombard {} now (~{} damage).",
                            g.fmt_xy(b.target),
                            b.damage
                        ),
                        format!(
                            "{line} Bombard now: city_attack city_id={} x={x} y={y} (~{} damage{kills}).",
                            id.get(),
                            b.damage
                        ),
                    )
                }
                None => (line.clone(), line),
            };
            add("threat", human, Some(llm), Some(at), vec![("city", id.get())]);
        } else if let Some(b) = shot {
            let (x, y) = g.xy(b.target);
            add(
                "bombard",
                format!(
                    "City {} can bombard an enemy at {} (~{} damage).",
                    c.name,
                    g.fmt_xy(b.target),
                    b.damage
                ),
                Some(format!(
                    "City {} #{} can bombard: city_attack city_id={} x={x} y={y} (~{} damage).",
                    c.name,
                    id.get(),
                    id.get(),
                    b.damage
                )),
                Some(at),
                vec![("city", id.get())],
            );
        }
        if memo::city_stats(g, id).total[Stat::Food] < 0.0 {
            add(
                "starving",
                format!("City {} is starving: work more food tiles or build farms.", c.name),
                Some(format!(
                    "City {} #{} is starving: set_city_focus food, or build farms.",
                    c.name,
                    id.get()
                )),
                Some(at),
                vec![("city", id.get())],
            );
        }
        if c.puppet && c.founder != p && c.turn_acquired >= g.turn() - 1 {
            add(
                "conquest",
                format!(
                    "You captured {}; it is a puppet. You may annex, raze or liberate it.",
                    c.name
                ),
                Some(format!(
                    "You captured {} #{} (now a puppet). Decide: city_status city_id={} \
                     status=annex|raze|liberate, or leave it as a puppet.",
                    c.name,
                    id.get(),
                    id.get()
                )),
                Some(at),
                vec![("city", id.get())],
            );
        }
        if c.queue.is_empty() && !c.puppet {
            add(
                "idle_city",
                format!("City {} has nothing in production.", c.name),
                Some(format!(
                    "City {} #{} has nothing in production: set_production.",
                    c.name,
                    id.get()
                )),
                Some(at),
                vec![("city", id.get())],
            );
        }
    }
    for u in g.player_units(p) {
        let at = u.tile();
        if r.base_units()[u.base].military || g.city_at(at).is_some() || g.military_at(at).is_some()
        {
            continue;
        }
        let danger = hostile.iter().find(|&&(e, t)| {
            g.grid().distance(t, at) <= 2
                && g.unit(e).is_some_and(|x| r.base_units()[x.base].domain == Domain::Land)
        });
        if let Some(&(e, t)) = danger {
            add(
                "civilian_danger",
                format!(
                    "{} #{} at {} is {} tile(s) from a hostile {}: undefended civilians get \
                     captured.",
                    unit_type(u.id()),
                    u.id().get(),
                    g.fmt_xy(at),
                    g.grid().distance(t, at),
                    unit_type(e)
                ),
                None,
                Some(at),
                vec![("unit", u.id().get())],
            );
        }
    }
    if research::current(g, p).is_none()
        && !research::available_techs(g, p).is_empty()
        && g.player_cities(p).next().is_some()
    {
        add(
            "research",
            "Choose a technology to research.".to_owned(),
            Some("No research set: set_research tech=<name>.".to_owned()),
            None,
            Vec::new(),
        );
    }
    let free = pl.tech.free_techs;
    if free > 0 {
        let plural = if free == 1 { "y" } else { "ies" };
        add(
            "free_tech",
            format!("You may choose {free} free technolog{plural}."),
            Some(
                "Free technology available: choose_free_tech tech=<an available tech>.".to_owned(),
            ),
            None,
            Vec::new(),
        );
    }
    if policies::can_adopt_any(g, p) {
        let cost = if pl.policy.free_policies != 0 {
            "free".to_owned()
        } else {
            format!("{} culture", policies::culture_cost(g, p, None))
        };
        add(
            "policy",
            format!("You can adopt a social policy ({cost})."),
            Some(
                "You can adopt a social policy: adopt_policy policy=<name> (get_policies lists \
                 options)."
                    .to_owned(),
            ),
            None,
            Vec::new(),
        );
    }
    if pl.gp.free > 0 {
        add(
            "great_person",
            "You may choose a free Great Person.".to_owned(),
            Some(
                "Free Great Person available: choose_great_person great_person=<Great \
                 Scientist|Great Engineer|...>."
                    .to_owned(),
            ),
            None,
            Vec::new(),
        );
    }
    if g.religion_enabled() && found::can_found_pantheon(g, p).is_none() {
        add(
            "pantheon",
            "You have enough faith to found a pantheon.".to_owned(),
            Some(
                "Enough faith for a pantheon: found_pantheon belief=<name> (get_religion lists \
                 beliefs)."
                    .to_owned(),
            ),
            None,
            Vec::new(),
        );
    }
    for u in g.player_units(p) {
        let Some(back) = u.return_offer else { continue };
        let who = if g.has_met(p, back) { super::name_of(g, back) } else { "its original owner" };
        let (ty, id) = (unit_type(u.id()), u.id().get());
        add(
            "return_civilian",
            format!("Recaptured {ty} #{id}: return it to {who}, or keep it?"),
            Some(format!(
                "You recaptured {ty} #{id} from barbarians; it belonged to {who}. Decide this \
                 turn: return_civilian unit_id={id} (goodwill), or keep=true. Unanswered, you \
                 keep it."
            )),
            Some(u.tile()),
            vec![("unit", id)],
        );
    }
    let promos: Vec<UnitId> =
        g.player_units(p).map(|u| u.id()).filter(|&u| promotions::can_promote(g, u)).collect();
    if !promos.is_empty() {
        let human: Vec<String> =
            promos.iter().take(6).map(|&u| format!("{} #{}", unit_type(u), u.get())).collect();
        let llm: Vec<String> =
            promos.iter().take(6).map(|&u| format!("#{} {}", u.get(), unit_type(u))).collect();
        add(
            "promotion",
            format!("Units ready for promotion: {}", human.join(", ")),
            Some(format!("Promotions available (promote_unit): {}", llm.join(", "))),
            None,
            Vec::new(),
        );
    }
    if g.espionage_enabled() {
        let idle: Vec<&str> = espionage::spies(g, p)
            .iter()
            .filter(|s| s.action == SpyAction::None)
            .map(|s| &*s.name)
            .collect();
        if !idle.is_empty() {
            let names = idle.join(", ");
            add(
                "spy",
                format!("Idle spies: {names}."),
                Some(format!("Idle spies ({names}): move_spy spy=<name> city_id=<id>.")),
                None,
                Vec::new(),
            );
        }
    }
    if un::vote_open(g) && !g.state().world().un.votes.contains_key(&p) {
        add(
            "un_vote",
            "The United Nations vote is open: cast your vote.".to_owned(),
            Some(
                "United Nations vote open: un_vote candidate=<player id or 'abstain'>.".to_owned(),
            ),
            None,
            Vec::new(),
        );
    }
    for n in g.negotiations() {
        if n.status != NegStatus::Open || (n.initiator != p && n.responder != p) {
            continue;
        }
        let o = if n.responder == p { n.initiator } else { n.responder };
        let other = super::name_of(g, o);
        let nid = n.id.get();
        let ids = vec![("player", u32::from(o.0)), ("negotiation", nid)];
        if n.awaiting == Some(p) {
            add(
                "negotiation",
                format!("{other} awaits your reply in negotiation #{nid}."),
                Some(format!(
                    "Negotiation #{nid} with {other} awaits your response: respond_negotiation, \
                     with a message."
                )),
                None,
                ids,
            );
        } else {
            // The end_turn tool refuses while this is open, so it is as much a to-do as one
            // waiting on the player.
            add(
                "negotiation",
                format!(
                    "Waiting for {other} to answer negotiation #{nid}: you can end your turn \
                     once they reply, or withdraw it."
                ),
                Some(format!(
                    "Negotiation #{nid} is waiting for {other}'s answer. end_turn is refused until \
                     it is settled; to give up on it, respond_negotiation action=reject with a \
                     message."
                )),
                None,
                ids,
            );
        }
    }
    out
}
