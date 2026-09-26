//! Everyone a viewer knows of, and its diplomacy: the players, the agreements in force, what
//! could be traded, negotiations, deals, messages, the United Nations and the city-states
//! (`views.py:349-502`).

use serde_json::{Map, Value, json};

use super::empire::{luxury_resources, strategic_resources};
use super::name_of;
use super::tiles::resource_seen;
use crate::base::ids::{PlayerId, ResourceId};
use crate::base::num;
use crate::game::city_states::{actions as csa, influence as csi, quests};
use crate::game::diplomacy::relations::{denounced, has_embassy, has_pact, is_friends};
use crate::game::diplomacy::{deals, negotiation};
use crate::game::victory::{self, un};
use crate::game::{Game, query, research};
use crate::state::Phase;
use crate::state::diplo::{DealItem, NegStatus, Side, Terms, side};
use crate::state::players::PlayerKind;

/// The agreements whose possibility `trade_options` checks, in Python's order (`views.py:425`).
const AGREEMENTS: [&str; 5] = [
    "embassy",
    "open_borders",
    "declaration_of_friendship",
    "research_agreement",
    "defensive_pact",
];

/// How many settled negotiations `get_diplomacy` shows, the latest (`views.py:445`).
const RECENT_NEGOTIATIONS: usize = 5;

/// A player's kind as Python named it.
#[must_use]
pub const fn kind_name(k: PlayerKind) -> &'static str {
    match k {
        PlayerKind::Major => "major",
        PlayerKind::CityState => "city_state",
        PlayerKind::Barbarian => "barbarian",
    }
}

/// Where Python's `xs[-n:]` starts on a list of `len`: the last `n`, all of it for 0, and for a
/// negative `n` all but the first `-n`.
pub(crate) fn py_tail(len: usize, n: i64) -> usize {
    match n {
        0 => 0,
        n if n > 0 => len.saturating_sub(usize::try_from(n).unwrap_or(usize::MAX)),
        n => usize::try_from(n.unsigned_abs()).unwrap_or(usize::MAX).min(len),
    }
}

/// Everyone `viewer` knows of, with what it knows about each (`views.players_overview`): a
/// player it has not met is "Unknown civilization" or "Unknown city-state"; one it has met has
/// its name and leader, a major its nation, difficulty, score, era and cities, a city-state its
/// type, the viewer's influence and relationship, and its ally; and between the viewer and the
/// others, war, the peace treaty and the agreements in force.
#[must_use]
pub fn players_overview(g: &Game, viewer: Option<PlayerId>) -> Vec<Value> {
    let r = g.rules();
    let over = g.state().clock().phase != Phase::Playing;
    let mut out = Vec::new();
    for (id, p) in g.state().players().iter() {
        let met = viewer.is_none_or(|v| v == id || g.has_met(v, id)) || p.is_barbarian() || over;
        let mut m = Map::new();
        m.insert("id".into(), json!(id.0));
        m.insert("kind".into(), json!(kind_name(p.kind)));
        m.insert("color".into(), json!(p.color.to_hex()));
        m.insert("alive".into(), json!(p.alive()));
        m.insert("met".into(), json!(met));
        if !met {
            let name = if p.is_major() { "Unknown civilization" } else { "Unknown city-state" };
            m.insert("name".into(), json!(name));
            out.push(Value::Object(m));
            continue;
        }
        m.insert("name".into(), json!(&*p.name));
        m.insert("leader".into(), json!(&*p.leader));
        match p.kind {
            PlayerKind::Major => {
                let difficulty = p.seat().difficulty().unwrap_or(g.state().config().difficulty);
                m.insert("nation".into(), json!(r.name(p.nation)));
                m.insert("difficulty".into(), json!(r.name(difficulty)));
                m.insert("score".into(), json!(victory::score(g, id).total));
                m.insert("era".into(), json!(r.name(query::era(g, id))));
                m.insert("cities".into(), json!(g.state().cities().of(id).len()));
            }
            PlayerKind::CityState => {
                let cs = p.city_state.as_deref();
                let ty = cs.and_then(|d| d.cs_type).and_then(|t| r.city_state_types().get(t));
                m.insert("city_state_type".into(), json!(ty.map(|t| &*t.name)));
                if let Some(v) = viewer {
                    let inf = csi::influence(g, id, v);
                    m.insert("influence".into(), json!(num::round_ndigits(inf, 1)));
                    m.insert("relationship".into(), json!(csi::relationship(g, id, v).name()));
                }
                let ally = cs.and_then(|d| d.ally());
                let shown = match ally {
                    None => Value::Null,
                    Some(a) if viewer.is_none_or(|v| a == v || g.has_met(v, a)) => {
                        json!(name_of(g, a))
                    }
                    Some(_) => json!("unknown"),
                };
                m.insert("ally".into(), shown);
            }
            PlayerKind::Barbarian => {}
        }
        if let Some(v) = viewer.filter(|&v| v != id && !p.is_barbarian()) {
            let rel = g.relation(v, id).copied().unwrap_or_default();
            m.insert("at_war".into(), json!(rel.war));
            if rel.treaty_until >= g.turn() && !rel.war {
                m.insert("peace_treaty_until".into(), json!(rel.treaty_until));
            }
            if p.is_major() {
                agreements(g, v, id, &mut m);
            }
        }
        out.push(Value::Object(m));
    }
    out
}

/// The agreements in force between two civilizations, from `me`'s side (`views._agreements`).
fn agreements(g: &Game, me: PlayerId, other: PlayerId, m: &mut Map<String, Value>) {
    let rel = g.relation(me, other).copied().unwrap_or_default();
    let grant = rel.open_borders_until[side(me, other)];
    if grant != 0 {
        m.insert("open_borders_you_grant_until".into(), json!(grant));
    }
    let given = rel.open_borders_until[side(other, me)];
    if given != 0 {
        m.insert("open_borders_they_grant_until".into(), json!(given));
    }
    m.insert("embassy_in_their_capital".into(), json!(has_embassy(g, me, other)));
    m.insert("their_embassy_with_you".into(), json!(has_embassy(g, other, me)));
    if is_friends(g, me, other) {
        m.insert("friendship_until".into(), json!(rel.friendship_until));
    }
    if has_pact(g, me, other) {
        m.insert("defensive_pact_until".into(), json!(rel.pact_until));
    }
    if rel.ra_until >= g.turn() {
        m.insert("research_agreement_until".into(), json!(rel.ra_until));
    }
    if denounced(g, me, other) {
        m.insert("you_denounced_them".into(), json!(true));
    }
    if denounced(g, other, me) {
        m.insert("they_denounced_you".into(), json!(true));
    }
}

/// What each side has that the other might want, for the deal builder's suggestions
/// (`views.trade_options`, `views.py:408-437`): gold, spare luxuries the other lacks, strategic
/// resources to spare, techs the other could research, and the agreements both sides could sign
/// now, with a research agreement's cost.
#[must_use]
pub fn trade_options(g: &Game, pid: PlayerId, other: PlayerId) -> Value {
    let r = g.rules();
    let (lux_me, lux_them) = (luxury_resources(g, pid), luxury_resources(g, other));
    let (strat_me, strat_them) = (strategic_resources(g, pid), strategic_resources(g, other));
    let net = |l: &[(ResourceId, super::empire::Luxury)], res: ResourceId| {
        l.iter().find(|(x, _)| *x == res).map_or(0, |(_, e)| e.net)
    };
    let spare = |from: &[(ResourceId, super::empire::Luxury)],
                 to: &[(ResourceId, super::empire::Luxury)]| {
        let mut names: Vec<&str> = from
            .iter()
            .filter(|&&(res, e)| e.net >= 2 && net(to, res) <= 0)
            .filter_map(|&(res, _)| r.name(res))
            .collect();
        names.sort();
        json!(names)
    };
    let strategic = |l: &[(ResourceId, super::empire::Strategic)],
                     seen: &dyn Fn(ResourceId) -> bool| {
        let m: Map<String, Value> = l
            .iter()
            .filter(|&&(res, e)| e.available > 0 && seen(res))
            .filter_map(|&(res, e)| Some((r.name(res)?.to_owned(), json!(e.available))))
            .collect();
        Value::Object(m)
    };
    let their_gold = g.player(other).map_or(0, |p| num::trunc_i64(p.econ.gold));
    let mut they = Map::new();
    they.insert("luxuries".into(), spare(&lux_them, &lux_me));
    they.insert(
        "strategic".into(),
        strategic(&strat_them, &|res| resource_seen(g, Some(pid), res)),
    );
    let mut you = Map::new();
    you.insert("luxuries".into(), spare(&lux_me, &lux_them));
    you.insert("strategic".into(), strategic(&strat_me, &|_| true));
    if g.state().config().tech_trading {
        let techs = |from: PlayerId, to: PlayerId| {
            let mut names: Vec<&str> = g
                .player(from)
                .map(|p| {
                    p.tech
                        .known
                        .iter()
                        .filter(|&t| research::can_research(g, to, t))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
                .into_iter()
                .filter_map(|t| r.name(t))
                .collect();
            names.sort();
            json!(names)
        };
        they.insert("techs".into(), techs(other, pid));
        you.insert("techs".into(), techs(pid, other));
    }
    let mut possible = Vec::new();
    for t in AGREEMENTS {
        let Ok(items) = deals::normalize_items(g, pid, Some(&json!([{"type": t}]))) else {
            continue;
        };
        let terms = Terms {
            sides: [
                Side { giver: pid, items: items.clone() },
                Side { giver: other, items: items.clone() },
            ],
        };
        if deals::validate_items(g, pid, other, &items, &terms).is_ok()
            && deals::validate_items(g, other, pid, &items, &terms).is_ok()
        {
            possible.push(t);
        }
    }
    let mut m = Map::new();
    m.insert("their_gold".into(), json!(their_gold));
    m.insert("they_could_give".into(), Value::Object(they));
    m.insert("you_could_give".into(), Value::Object(you));
    m.insert("agreements_possible_now".into(), json!(possible));
    if possible.contains(&"research_agreement") {
        m.insert("research_agreement_cost_each".into(), json!(deals::ra_cost(g, pid, other)));
    }
    Value::Object(m)
}

/// The UN's last result as `viewer` may see it: the tally by name, the candidates it has not met
/// named as unknown as a scrubbed event names them (`game.py:980-988`), and the winner only if it
/// knows them. Python's diplomacy and victory panels gave the result with every name.
// refcheck: un-results-name-only-known-candidates
pub(crate) fn un_result(
    g: &Game,
    viewer: Option<PlayerId>,
    res: &crate::state::world::UnResult,
) -> Value {
    let known = g.known_to(viewer);
    let mut tally = Map::new();
    let mut unknown = 0;
    for &(p, n) in &res.tally {
        let name = match known {
            Some(k) if !k.contains(p) => {
                unknown += 1;
                let base = if g.is_city_state(p) {
                    crate::game::events::UNKNOWN_CS
                } else {
                    crate::game::events::UNKNOWN_CIV
                };
                if tally.contains_key(base) {
                    format!("{base} ({unknown})")
                } else {
                    base.to_owned()
                }
            }
            _ => name_of(g, p).to_owned(),
        };
        tally.insert(name, json!(n));
    }
    let winner = res.winner.filter(|&w| known.is_none_or(|k| k.contains(w)));
    json!({
        "turn": res.turn,
        "tally": tally,
        "votes_needed": res.votes_needed,
        "winner": winner.map(|p| p.0),
    })
}

/// Relations, negotiations, deals and recent messages, from `pid`'s side
/// (`views.diplomacy_info`, `views.py:440-471`): the majors it has met with what could be traded
/// with each, its open and latest settled negotiations as it sees them, the deals in force, its
/// last `message_limit` messages, and the United Nations' next vote.
#[must_use]
pub fn diplomacy_info(g: &Game, pid: PlayerId, message_limit: i64) -> Value {
    let r = g.rules();
    let mine: Vec<_> =
        g.negotiations().iter().filter(|n| n.initiator == pid || n.responder == pid).collect();
    let open: Vec<Value> = mine
        .iter()
        .filter(|n| n.status == NegStatus::Open)
        .map(|n| negotiation::negotiation_view(g, n, pid))
        .collect();
    let settled: Vec<_> = mine.iter().filter(|n| n.status != NegStatus::Open).collect();
    let recent: Vec<Value> = settled[settled.len().saturating_sub(RECENT_NEGOTIATIONS)..]
        .iter()
        .map(|n| negotiation::negotiation_view(g, n, pid))
        .collect();
    let turn = g.turn();
    let mut deals_out = Vec::new();
    for d in &g.state().diplo().deals {
        if !d.active || !d.parties.contains(&pid) {
            continue;
        }
        let other = if d.parties[1] == pid { d.parties[0] } else { d.parties[1] };
        let ongoing: Vec<Value> = d
            .ongoing
            .iter()
            .filter(|o| o.until >= turn)
            .map(|o| {
                let (resource, amount) = match o.item {
                    DealItem::Resource { resource, amount, .. } => (r.name(resource), Some(amount)),
                    DealItem::GoldPerTurn { amount, .. } | DealItem::Gold { amount } => {
                        (None, Some(amount))
                    }
                    _ => (None, None),
                };
                json!({
                    "type": o.item.kind().name(),
                    "resource": resource,
                    "amount": amount,
                    "direction": if o.from == pid { "out" } else { "in" },
                    "until_turn": o.until,
                })
            })
            .collect();
        deals_out.push(json!({
            "id": d.id.get(),
            "with": other.0,
            "with_name": name_of(g, other),
            "turn": d.turn,
            "you_give": deals::describe_items(g, d.terms.gives(pid)),
            "you_receive": deals::describe_items(g, d.terms.gives(other)),
            "ongoing": ongoing,
        }));
    }
    let msgs: Vec<_> =
        g.chronicle().messages().iter().filter(|m| m.from == pid || m.to.contains(pid)).collect();
    let messages: Vec<Value> = msgs[py_tail(msgs.len(), message_limit)..]
        .iter()
        .map(|m| {
            let to: Vec<&str> = m.to.iter().map(|p| name_of(g, p)).collect();
            json!({
                "turn": m.turn,
                "from": m.from.0,
                "from_name": name_of(g, m.from),
                "to": to,
                "text": &*m.text,
            })
        })
        .collect();
    let mut players: Vec<Value> = players_overview(g, Some(pid))
        .into_iter()
        .filter(|p| {
            p["id"] != json!(pid.0) && p["kind"] == json!("major") && p["met"] == json!(true)
        })
        .collect();
    for p in &mut players {
        let alive = p["alive"] == json!(true);
        let id = p["id"].as_u64().and_then(|n| u8::try_from(n).ok()).map(PlayerId);
        if let (true, Some(other), Some(m)) = (alive, id, p.as_object_mut()) {
            m.insert("trade_options".into(), trade_options(g, pid, other));
        }
    }
    let mut out = Map::new();
    out.insert("players".into(), Value::Array(players));
    out.insert("open_negotiations".into(), Value::Array(open));
    out.insert("recent_negotiations".into(), Value::Array(recent));
    out.insert("active_deals".into(), Value::Array(deals_out));
    out.insert("messages".into(), Value::Array(messages));
    let world = &g.state().world().un;
    if let Some(next) = world.next_vote {
        let your_vote = match world.votes.get(&pid) {
            None => json!("not cast"),
            Some(c) => json!(c.map(|p| p.0)),
        };
        out.insert(
            "united_nations".into(),
            json!({
                "next_vote_turn": next,
                "voting_open": un::vote_open(g),
                "your_vote": your_vote,
                "last_result": world.results.as_ref().map(|res| un_result(g, Some(pid), res)),
            }),
        );
    }
    Value::Object(out)
}

/// The city-states `pid` has met, with its influence and standing, their allies, bonuses,
/// quests and what tribute they would pay (`views.city_states_info`, `views.py:474-502`).
#[must_use]
pub fn city_states_info(g: &Game, pid: PlayerId) -> Vec<Value> {
    let r = g.rules();
    let mut out = Vec::new();
    let explored =
        |t: crate::base::ids::TileIdx| g.player(pid).is_some_and(|p| p.explored.contains(t.0));
    for q in g.city_states(true) {
        let id = q.id();
        if !g.has_met(pid, id) {
            continue;
        }
        let Some(d) = q.city_state.as_deref() else { continue };
        let ty = d.cs_type.and_then(|t| r.city_state_types().get(t));
        let texts = |u: &crate::unique::table::SourceUniques| -> Vec<String> {
            let t = r.uniques();
            u.ids().map(|x| t.text_of(x).to_owned()).collect()
        };
        let mut m = Map::new();
        m.insert("id".into(), json!(id.0));
        m.insert("name".into(), json!(&*q.name));
        m.insert("type".into(), json!(ty.map(|t| &*t.name)));
        m.insert("personality".into(), json!(d.personality.map(|p| p.name())));
        m.insert("influence".into(), json!(num::round_ndigits(csi::influence(g, id, pid), 1)));
        m.insert("resting_point".into(), json!(csi::resting_point(g, id, pid)));
        m.insert("relationship".into(), json!(csi::relationship(g, id, pid).name()));
        m.insert("ally".into(), json!(d.ally().map(|a| name_of(g, a))));
        m.insert("you_protect".into(), json!(d.protectors.contains(pid)));
        m.insert("at_war".into(), json!(g.at_war(pid, id)));
        m.insert("friend_bonuses".into(), json!(ty.map(|t| texts(&t.friend)).unwrap_or_default()));
        m.insert("ally_bonuses".into(), json!(ty.map(|t| texts(&t.ally)).unwrap_or_default()));
        if let Some(cap) = q.capital.and_then(|c| g.city(c)).filter(|c| explored(c.tile())) {
            let (x, y) = g.xy(cap.tile());
            m.insert("capital".into(), json!({"city_id": cap.id().get(), "x": x, "y": y}));
        }
        if let Some(res) = d.resource {
            m.insert("unique_luxury".into(), json!(r.name(res)));
        }
        if let Some(u) = d.unique_unit {
            m.insert("gifts_unit".into(), json!(r.name(u)));
        }
        // Python matched the quests on a key its quest list never had, so it listed none
        // (`views.py:496-497`).
        // refcheck: city-state-view-lists-its-quests
        let offered: Vec<String> = d
            .quests
            .iter()
            .filter(|x| x.assignee == pid)
            .map(|x| quests::quest_text(g, x))
            .collect();
        m.insert("quests".into(), json!(offered));
        let will = csa::tribute_willingness(g, id, pid, false);
        m.insert(
            "tribute".into(),
            json!({
                "would_pay": will > 0,
                "gold": (will > 0).then(|| csa::tribute_gold_amount(g)),
                "willingness": will,
            }),
        );
        out.push(Value::Object(m));
    }
    out
}
