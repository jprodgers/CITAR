//! A civilization's empire as a whole: its treasury and yields, happiness, resources, research,
//! score and progress (`views.empire_info`, `views.py:308-346`), and the facade's
//! `empire_summary`, `standing` and `standings` (`engine_api.py:483-497, 526-537`).

use serde::Serialize;
use serde_json::{Map, Value, json};

use super::{EMPIRE_YIELDS, rounded, tiles::resource_seen};
use crate::base::ids::PlayerId;
use crate::base::num;
use crate::base::stats::Stat;
use crate::game::derive::stats as memo;
use crate::game::economy::{self, Origin};
use crate::game::victory::{self, milestones};
use crate::game::{Game, great_people, query, religion, research};
use crate::rules::defs::ResourceType;
use crate::state::players::Player;

/// A civilization's happiness as the views show it (`economy.happiness`): the total, each source
/// that is not zero rounded to two places, its mood and, for a major, the luxuries it has.
#[must_use]
pub fn happiness_json(g: &Game, p: PlayerId) -> Value {
    let h = memo::happiness(g, p);
    let breakdown: Map<String, Value> = h
        .breakdown
        .iter()
        .filter(|&&(_, x)| x != 0.0)
        .map(|&(k, x)| (k.name().to_owned(), json!(num::round_ndigits(x, 2))))
        .collect();
    let mut out = json!({"total": h.total, "breakdown": breakdown, "status": h.status()});
    if h.major {
        let r = g.rules();
        let lux: Vec<&str> = h.luxury_types.iter().filter_map(|&x| r.name(x)).collect();
        out["luxury_types"] = json!(lux);
    }
    out
}

/// A strategic resource's lines for display (`economy.strategic_resources`): what the
/// civilization's sources give, what its units and buildings use, what it trades in and out,
/// and what is left.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Strategic {
    pub sources: i32,
    pub used: i32,
    pub imported: i32,
    pub exported: i32,
    pub available: i32,
}

/// A luxury's lines for display (`economy.luxury_resources`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Luxury {
    pub owned: i32,
    pub imported: i32,
    pub exported: i32,
    pub net: i32,
}

/// Every strategic resource of the ruleset with its lines for `p`, in the ruleset's order
/// (`economy.strategic_resources`, `economy.py:361-379`).
#[must_use]
pub fn strategic_resources(
    g: &Game,
    p: PlayerId,
) -> Vec<(crate::base::ids::ResourceId, Strategic)> {
    let r = g.rules();
    let mut out: Vec<_> = r
        .resources()
        .iter()
        .filter(|(_, d)| d.kind == ResourceType::Strategic)
        .map(|(id, _)| (id, Strategic::default()))
        .collect();
    for it in query::detailed_resources(g, p) {
        let Some((_, e)) = out.iter_mut().find(|(id, _)| *id == it.resource) else { continue };
        match it.origin {
            Origin::Units | Origin::Buildings => e.used -= it.amount,
            Origin::Trade if it.amount > 0 => e.imported += it.amount,
            Origin::Trade => e.exported += it.amount.abs(),
            _ => e.sources += it.amount,
        }
    }
    for (_, e) in &mut out {
        e.available = e.sources + e.imported - e.exported - e.used;
    }
    out
}

/// Every luxury of the ruleset with its lines for `p`, in the ruleset's order
/// (`economy.luxury_resources`, `economy.py:382-398`).
#[must_use]
pub fn luxury_resources(g: &Game, p: PlayerId) -> Vec<(crate::base::ids::ResourceId, Luxury)> {
    let r = g.rules();
    let mut out: Vec<_> = r
        .resources()
        .iter()
        .filter(|(_, d)| d.kind == ResourceType::Luxury)
        .map(|(id, _)| (id, Luxury::default()))
        .collect();
    for it in query::detailed_resources(g, p) {
        let Some((_, e)) = out.iter_mut().find(|(id, _)| *id == it.resource) else { continue };
        match it.origin {
            Origin::Trade if it.amount > 0 => e.imported += it.amount,
            Origin::Trade => e.exported += it.amount.abs(),
            _ => e.owned += it.amount,
        }
    }
    for (_, e) in &mut out {
        e.net = e.owned + e.imported - e.exported;
    }
    out
}

/// The empire summary (`views.empire_info`): treasury and yields per turn with their sources,
/// happiness, golden age, research, era, resources, unit supply, score, policies, religion and
/// the spaceship.
#[must_use]
pub fn empire_info(g: &Game, p: PlayerId) -> Value {
    let Some(pl) = g.player(p) else { return Value::Null };
    let r = g.rules();
    let (total, breakdown) = {
        let cs = memo::civ_stats(g, p);
        let bd: Map<String, Value> = cs
            .map
            .iter()
            .map(|(src, y)| {
                let keys: Vec<Stat> =
                    EMPIRE_YIELDS.into_iter().filter(|&k| y.keys.contains(k)).collect();
                (src.name().to_owned(), rounded(&y.stats, &keys, 1))
            })
            .collect();
        (cs.total, bd)
    };
    let strat: Map<String, Value> = strategic_resources(g, p)
        .into_iter()
        .filter(|&(res, e)| resource_seen(g, Some(p), res) && e != Strategic::default())
        .filter_map(|(res, e)| Some((r.name(res)?.to_owned(), json!(e))))
        .collect();
    let lux: Map<String, Value> = luxury_resources(g, p)
        .into_iter()
        .filter(|&(_, e)| e != Luxury::default())
        .filter_map(|(res, e)| Some((r.name(res)?.to_owned(), json!(e))))
        .collect();
    let cur = research::current(g, p);
    let era = query::era(g, p);
    let mut m = Map::new();
    m.insert("id".into(), json!(p.0));
    m.insert("name".into(), json!(&*pl.name));
    m.insert("leader".into(), json!(&*pl.leader));
    m.insert("nation".into(), json!(r.name(pl.nation)));
    m.insert("color".into(), json!(pl.color.to_hex()));
    m.insert("gold".into(), json!(num::trunc_i64(pl.econ.gold)));
    m.insert("culture".into(), json!(num::trunc_i64(pl.econ.culture)));
    m.insert("faith".into(), json!(num::trunc_i64(pl.econ.faith)));
    m.insert("per_turn".into(), rounded(&total, &EMPIRE_YIELDS, 1));
    m.insert("per_turn_breakdown".into(), Value::Object(breakdown));
    m.insert("happiness".into(), happiness_json(g, p));
    m.insert("golden_age".into(), golden_age(g, p, pl));
    m.insert("researching".into(), json!(cur.and_then(|t| r.name(t))));
    let queue: Vec<&str> = pl.tech.queue.iter().filter_map(|&t| r.name(t)).collect();
    m.insert("research_queue".into(), json!(queue));
    let progress =
        cur.map_or(0, |t| num::trunc_i64(pl.tech.progress.get(&t).copied().unwrap_or(0.0)));
    m.insert("research_progress".into(), json!(progress));
    m.insert("research_cost".into(), json!(cur.map(|t| research::tech_cost(g, p, t))));
    m.insert("research_turns".into(), json!(cur.and_then(|t| research::turns_left(g, p, Some(t)))));
    m.insert("techs_known".into(), json!(pl.tech.known.len()));
    m.insert("free_techs".into(), json!(pl.tech.free_techs));
    m.insert("future_techs".into(), json!(pl.tech.future_techs));
    m.insert("era".into(), json!(r.name(era)));
    m.insert("strategic_resources".into(), Value::Object(strat));
    m.insert("luxuries".into(), Value::Object(lux));
    m.insert(
        "unit_supply".into(),
        json!({
            "units": g.state().units().of(p).len(),
            "supply": economy::unit_supply(g, p),
            "production_penalty_percent": num::trunc_i64(economy::unit_supply_penalty(g, p)),
        }),
    );
    m.insert("score".into(), json!(victory::score(g, p).total));
    m.insert("capital".into(), json!(pl.capital.map(crate::base::ids::CityId::get)));
    let policies: Vec<&str> = pl.policy.adopted.iter().filter_map(|x| r.name(x)).collect();
    m.insert("policies".into(), json!(policies));
    m.insert("free_policies".into(), json!(pl.policy.free_policies));
    m.insert("free_great_people".into(), json!(pl.gp.free));
    if g.religion_enabled() {
        m.insert(
            "religion".into(),
            json!({
                "state": pl.religion.progress.name(),
                "name": pl.religion.founded.map(|x| religion::display_name(g, x)),
            }),
        );
    }
    if let Some(sci) = r.derived().known.victories.scientific
        && g.victory_enabled(sci)
    {
        m.insert("spaceship".into(), milestones::spaceship_status(g, p).to_json(g));
    }
    Value::Object(m)
}

/// A civilization's golden age: the turns left, its progress toward the next and what that needs.
pub(crate) fn golden_age(g: &Game, p: PlayerId, pl: &Player) -> Value {
    json!({
        "turns_left": pl.econ.golden_age_turns,
        "progress": num::trunc_i64(pl.econ.golden_age_points),
        "needed": great_people::happiness_for_golden_age(g, p),
    })
}

/// The empire at a glance, as a negotiation answer reads it (`EngineGame.empire_summary`,
/// `engine_api.py:526-537`): `get_empire`'s data, how many cities it has, whom it is at war with
/// (in player-id order) and its notebook.
#[derive(Clone, Debug, PartialEq)]
pub struct EmpireSummary {
    /// `get_empire`'s answer.
    pub empire: Value,
    pub cities: u32,
    /// The names of the civilizations and city-states it has met and is at war with, by id.
    pub at_war_with: Vec<String>,
    pub notes: String,
}

impl EmpireSummary {
    /// As the facade returns it: the empire's keys, then `cities`, `at_war_with` and `notes`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut v = self.empire.clone();
        if let Some(m) = v.as_object_mut() {
            m.insert("cities".into(), json!(self.cities));
            m.insert("at_war_with".into(), json!(self.at_war_with));
            m.insert("notes".into(), json!(self.notes));
        }
        v
    }
}

/// How a major civilization is doing (`EngineGame.standing`, `engine_api.py:483-493`); the score
/// is the formula's even for one that has been eliminated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Standing {
    #[serde(skip)]
    pub player: PlayerId,
    pub score: i32,
    pub cities: u32,
    pub units: u32,
    pub population: u32,
    pub techs: u32,
    pub gold: i64,
    /// Its era's name.
    pub era: String,
}

impl Game {
    /// The empire at a glance (`EngineGame.empire_summary`); `None` for a player the game lacks.
    #[must_use]
    pub fn empire_summary(&self, pid: PlayerId) -> Option<EmpireSummary> {
        let pl = self.player(pid)?;
        let at_war_with = self
            .state()
            .diplo()
            .met_mask(pid)
            .iter()
            .filter(|&q| q != pid && self.at_war(pid, q))
            .map(|q| super::name_of(self, q).to_owned())
            .collect();
        Some(EmpireSummary {
            empire: empire_info(self, pid),
            cities: u32::try_from(self.state().cities().of(pid).len()).unwrap_or(u32::MAX),
            at_war_with,
            notes: pl.major.as_deref().map_or_else(String::new, |m| m.notes.to_string()),
        })
    }

    /// How one civilization is doing (`EngineGame.standing`).
    #[must_use]
    pub fn standing(&self, pid: PlayerId) -> Option<Standing> {
        let pl = self.player(pid)?;
        let st = self.state();
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        let population = st
            .cities()
            .of(pid)
            .iter()
            .filter_map(|&c| self.city(c))
            .map(|c| u32::from(c.pop))
            .sum();
        Some(Standing {
            player: pid,
            score: victory::score(self, pid).total,
            cities: count(st.cities().of(pid).len()),
            units: count(st.units().of(pid).len()),
            population,
            techs: count(pl.tech.known.len()),
            gold: num::trunc_i64(pl.econ.gold),
            era: self.rules().name(query::era(self, pid)).unwrap_or("").to_owned(),
        })
    }

    /// Every major civilization's standing, the eliminated ones included, by id
    /// (`EngineGame.standings`).
    #[must_use]
    pub fn standings(&self) -> Vec<Standing> {
        let majors: Vec<PlayerId> = self.majors(false).map(Player::id).collect();
        majors.into_iter().filter_map(|p| self.standing(p)).collect()
    }
}
