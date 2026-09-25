//! City-state quests (`city_states.py:805-1214`; UnCiv's `QuestManager`).
//!
//! A city-state with a city gives each major it has met one or two individual quests at a time,
//! and now and then a global one, a contest or a camp to clear, to every major that could take
//! it on. What a quest is about is typed ([`QuestTarget`]): the camp's tile, the wonder, the
//! bully, a contest's starting score. Each end of the city-state's turn, individual quests done
//! are rewarded, those that no longer make sense or ran out are dropped, a contest that ran out
//! goes to the best score, and new quests are given when their countdowns reach 0 (from turn 30).
//! Some quests are completed as they happen: gold given, protection pledged, a city-state bullied
//! or conquered, a camp cleared.
//!
//! Draws: whether a quest fits a major, and its target, from `Purpose::Quest` keyed by the
//! city-state, the major, the quest's row and the turn (Python keyed by the quest's name), so
//! asking twice gives the same answer; the countdowns and the choice among the quests that fit
//! from `Purpose::Quests` keyed by the city-state and the turn.

use serde_json::{Value, json};

use super::influence::{add_influence, data, data_mut, pair};
use crate::base::ids::{BaseUnitId, BuildingId, PlayerId, QuestKindId, ResourceId, TileIdx};
use crate::base::num;
use crate::base::rng::{KeyPart, Purpose, Rng};
use crate::base::sets::{PlayerSet, ResourceSet, TerrainSet};
use crate::game::barbarians::is_camp_tile;
use crate::game::diplomacy::relations::name;
use crate::game::{Game, great_people, query, religion, workers};
use crate::rules::defs::{QuestKind, QuestScope, ResourceType};
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::cities::Constructible;
use crate::state::diplo::side;
use crate::state::players::{Player, Quest, QuestTarget};

/// The row of `quests.json` a quest was given from.
fn kind_of(g: &Game, q: &Quest) -> QuestKind {
    g.rules().quests()[q.kind].kind
}

/// A quest's name: its row's (`Route`).
fn quest_name(g: &Game, q: &Quest) -> String {
    g.rules().quests()[q.kind].name.to_string()
}

/// Tells one major about a city-state's quests.
fn tell(g: &mut Game, cs: PlayerId, to: PlayerId, text: &str) {
    let data = EventData { player: Some(cs), ..EventData::default() };
    g.emit(EngineEvent::CsQuest, text, Some(PlayerSet::single(to)), None, data, &[]);
}

/// How much more a gift of gold is worth to a city-state that asked for investment from the
/// donor (`_investment_multiplier`, `city_states.py:808-813`).
#[must_use]
pub fn investment_multiplier(g: &Game, cs: PlayerId, donor: PlayerId) -> f64 {
    let Some(d) = data(g, cs) else { return 1.0 };
    d.quests.iter().find(|q| q.assignee == donor && kind_of(g, q) == QuestKind::Invest).map_or(
        1.0,
        |q| match q.target {
            QuestTarget::Percent(p) => 1.0 + f64::from(p) / 100.0,
            _ => 1.5,
        },
    )
}

/// The resources a civilization has more than none of (`_res`, `city_states.py:912-915`).
fn resources_of(g: &Game, p: PlayerId) -> ResourceSet {
    query::detailed_resources(g, p).iter().filter(|x| x.amount > 0).map(|x| x.resource).collect()
}

/// Whether a tile has a road or railroad `p` may use (`movement.has_connection`,
/// `movement.py:288-309`): its route unless pillaged, a city centre whose owner knows the road,
/// or its own forest or jungle with `Forests and Jungles are roads`.
fn has_connection(g: &Game, p: PlayerId, t: TileIdx, forest_roads: bool) -> bool {
    let Some(tile) = g.tile(t) else { return false };
    if tile.route().is_some() && !tile.route_pillaged() {
        return true;
    }
    let r = g.rules();
    let known = &r.derived().known;
    if let Some(c) = g.city_at(t) {
        let imps = r.improvements();
        if g.has_tech(c.owner(), imps[known.railroad].tech_required)
            || g.has_tech(c.owner(), imps[known.road].tech_required)
        {
            return true;
        }
    }
    forest_roads
        && tile.owner() == Some(p)
        && tile.features().iter().any(|f| {
            let t = r.derived().features.get(f).copied();
            t.is_some() && (t == known.map.forest || t == known.map.jungle)
        })
}

/// Whether a major's capital reaches a city-state's capital by road (`_route_connected`,
/// `city_states.py:918-937`).
fn route_connected(g: &Game, major: PlayerId, cs_cap: TileIdx) -> bool {
    let Some(cap) = g.player(major).and_then(|p| p.capital).and_then(|c| g.city(c)) else {
        return false;
    };
    let forest_roads = crate::game::diplomacy::relations::civ_has(
        g,
        major,
        crate::unique::UniqueType::ForestsAndJunglesAreRoads,
    );
    let mut seen = crate::base::sets::BitSet::new();
    seen.insert(cap.tile().0);
    let mut stack = vec![cap.tile()];
    while let Some(cur) = stack.pop() {
        if cur == cs_cap {
            return true;
        }
        for n in g.grid().neighbors(cur) {
            if seen.contains(n.0) {
                continue;
            }
            if n == cs_cap || has_connection(g, major, n, forest_roads) {
                seen.insert(n.0);
                stack.push(n);
            }
        }
    }
    false
}

/// The last civilization to bully a city-state, which it would like dealt with
/// (`_most_recent_bully`, `city_states.py:940-948`): the one it remembers longest, the first by
/// id among equals.
fn most_recent_bully(g: &Game, cs: PlayerId) -> Option<PlayerId> {
    let d = data(g, cs)?;
    let mut best: Option<(PlayerId, i16)> = None;
    for (p, pair) in d.pairs.iter() {
        if pair.bullied > best.map_or(0, |(_, b)| b) {
            best = Some((p, pair.bullied));
        }
    }
    best.map(|(p, _)| p)
}

/// Whether `by` has ever denounced `target` (Python's `denounced_by`, which the quests read as
/// set or not).
fn ever_denounced(g: &Game, by: PlayerId, target: PlayerId) -> bool {
    g.relation(by, target).is_some_and(|r| r.denounced_until[side(by, target)] != 0)
}

/// What a quest of row `k` would be about for `major`, if it could be given now (`_quest_valid`,
/// `city_states.py:816-909`): a city-state with a capital, at peace with a living major it has
/// met, and the quest's own conditions.
#[must_use]
pub fn quest_target(
    g: &Game,
    cs: PlayerId,
    k: QuestKindId,
    major: PlayerId,
) -> Option<QuestTarget> {
    let pl = g.player(cs)?;
    let d = pl.city_state.as_deref()?;
    let cap = pl.capital.and_then(|c| g.city(c))?;
    if !g.has_met(cs, major) || g.at_war(cs, major) || !g.player(major).is_some_and(Player::alive) {
        return None;
    }
    let r = g.rules();
    let def = r.quests().get(k)?;
    let mut rng = Rng::keyed(
        g.state().seed(),
        Purpose::Quest,
        &[cs.key(), major.key(), u64::from(k.0), g.turn().key()],
    );
    let (ctile, cid) = (cap.tile(), cap.id());
    match def.kind {
        QuestKind::Route => {
            if g.player_cities(major).next().is_none() || route_connected(g, major, ctile) {
                return None;
            }
            let near = g.player_cities(major).any(|c| {
                g.continent(c.tile()) == g.continent(ctile)
                    && g.grid().distance(c.tile(), ctile) <= 7
            });
            near.then_some(QuestTarget::None)
        }
        QuestKind::ClearBarbarianCamp => {
            let camps: Vec<TileIdx> =
                g.grid().within(ctile, 8).into_iter().filter(|&t| is_camp_tile(g, t)).collect();
            rng.pick(&camps).map(|&t| QuestTarget::Tile(t))
        }
        QuestKind::ConnectResource => {
            let own = resources_of(g, cs);
            let theirs = resources_of(g, major);
            let mut on_map = ResourceSet::new();
            for t in g.grid().tiles() {
                if let Some(res) = g.tile(t).and_then(crate::state::map::Tile::resource) {
                    on_map.insert(res);
                }
            }
            let cands: Vec<ResourceId> = on_map
                .iter()
                .filter(|&res| {
                    r.resources()[res].kind != ResourceType::Bonus
                        && workers::resource_visible(g, major, res)
                        && !own.contains(res)
                        && !theirs.contains(res)
                })
                .collect();
            rng.pick(&cands).map(|&res| QuestTarget::Resource(res))
        }
        QuestKind::ConstructWonder => {
            let built = &g.state().world().wonders_built;
            let cands: Vec<BuildingId> = r
                .buildings()
                .iter()
                .filter(|&(b, bd)| {
                    bd.is_wonder
                        && bd.unique_to.is_none()
                        && g.has_tech(major, bd.required_tech)
                        && !built.contains_key(&b)
                        && !g.state().cities().iter().any(|c| {
                            let done =
                                c.progress.get(&Constructible::Building(b)).copied().unwrap_or(0.0);
                            done * 3.0 > 0.0 && done * 3.0 > f64::from(bd.cost) - done
                        })
                })
                .map(|(b, _)| b)
                .collect();
            rng.pick(&cands).map(|&b| QuestTarget::Building(b))
        }
        QuestKind::AcquireGreatPerson => {
            let have: Vec<BaseUnitId> = g
                .player_units(major)
                .chain(g.player_units(cs))
                .filter(|u| r.base_units()[u.base].great_person)
                .map(|u| u.base)
                .collect();
            let mut cands: Vec<BaseUnitId> = great_people::great_people_types(g, major)
                .into_iter()
                .filter(|t| !have.contains(t))
                .collect();
            cands.sort();
            rng.pick(&cands).map(|&u| QuestTarget::UnitType(u))
        }
        QuestKind::ConquerCityState | QuestKind::BullyCityState => {
            if def.kind == QuestKind::ConquerCityState
                && d.personality == Some(crate::rules::defs::CityStatePersonality::Friendly)
            {
                return None;
            }
            let others: Vec<(PlayerId, u32)> = g
                .city_states(true)
                .filter(|q| q.id() != cs && g.has_met(major, q.id()) && g.has_met(cs, q.id()))
                .filter_map(|q| {
                    let t = q.capital.and_then(|c| g.city(c))?.tile();
                    Some((q.id(), g.grid().distance(ctile, t)))
                })
                .collect();
            let closest = others.iter().map(|&(_, dist)| dist).min()?;
            if closest > 20 {
                return None;
            }
            let cands: Vec<PlayerId> =
                others.into_iter().filter(|&(_, dist)| dist == closest).map(|(q, _)| q).collect();
            rng.pick(&cands).map(|&q| QuestTarget::Player(q))
        }
        QuestKind::FindPlayer => {
            let explored = |q: PlayerId| {
                g.player_cities(q)
                    .any(|c| g.player(major).is_some_and(|p| p.explored.contains(c.tile().0)))
            };
            let cands: Vec<PlayerId> = g
                .majors(true)
                .map(Player::id)
                .filter(|&q| q != major && g.has_met(major, q) && !explored(q))
                .collect();
            rng.pick(&cands).map(|&q| QuestTarget::Player(q))
        }
        QuestKind::FindNaturalWonder => {
            let mut all = TerrainSet::new();
            for t in g.grid().tiles() {
                if let Some(w) = g.tile(t).and_then(crate::state::map::Tile::wonder) {
                    all.insert(w);
                }
            }
            let known = g.player(major).map(|p| p.civ.natural_wonders).unwrap_or_default()
                | pl.civ.natural_wonders;
            let cands: Vec<_> = all.iter().filter(|&w| !known.contains(w)).collect();
            rng.pick(&cands).map(|&w| QuestTarget::NaturalWonder(w))
        }
        QuestKind::GiveGold | QuestKind::PledgeToProtect | QuestKind::DenounceCivilization => {
            let bully = most_recent_bully(g, cs)?;
            if def.kind == QuestKind::PledgeToProtect && d.protectors.contains(major) {
                return None;
            }
            if def.kind == QuestKind::DenounceCivilization {
                let rel = g.relation(major, bully);
                if bully == major || rel.is_none_or(|x| x.war) || ever_denounced(g, major, bully) {
                    return None;
                }
            }
            Some(QuestTarget::Player(bully))
        }
        QuestKind::SpreadReligion => {
            let pr = g.player(major)?.religion.founded?;
            if !religion::is_major(g, pr) || religion::majority_religion(g, cid) == Some(pr) {
                return None;
            }
            Some(QuestTarget::Religion(pr))
        }
        QuestKind::ContestCulture => {
            Some(QuestTarget::Baseline(g.player(major)?.econ.total_culture))
        }
        QuestKind::ContestFaith => {
            if !g.religion_enabled() {
                return None;
            }
            Some(QuestTarget::Baseline(g.player(major)?.econ.total_faith))
        }
        QuestKind::ContestTechnologies => Some(QuestTarget::Baseline(tech_score(g, major))),
        QuestKind::Invest => {
            let pct = def.params.first().copied().unwrap_or(50.0);
            Some(QuestTarget::Percent(i16::try_from(num::trunc_i64(pct)).unwrap_or(50)))
        }
    }
}

/// A civilization's technologies for the contest: known techs and future techs.
fn tech_score(g: &Game, p: PlayerId) -> i64 {
    g.player(p).map_or(0, |x| {
        i64::try_from(x.tech.known.len()).unwrap_or(0) + i64::from(x.tech.future_techs)
    })
}

/// How likely a quest's row is to be chosen for this city-state (`_quest_weight`,
/// `city_states.py:951-961`): its weights for the city-state's personality and type.
fn quest_weight(g: &Game, cs: PlayerId, k: QuestKindId) -> f64 {
    let def = &g.rules().quests()[k];
    let Some(d) = data(g, cs) else { return 1.0 };
    let mut w = 1.0;
    if let Some(p) = d.personality
        && let Some(&(_, x)) = def.weight_by_personality.iter().find(|&&(q, _)| q == p)
    {
        w *= x;
    }
    if let Some(t) = d.cs_type
        && let Some(&(_, x)) = def.weight_by_type.iter().find(|&&(q, _)| q == t)
    {
        w *= x;
    }
    w
}

/// One of `items`, weighted (`_weighted`, `city_states.py:964-974`): the first when no weight is
/// positive.
fn weighted(rng: &mut Rng, items: &[QuestKindId], weights: &[f64]) -> Option<QuestKindId> {
    let total: f64 = weights.iter().sum();
    if total <= 0.0 {
        return items.first().copied();
    }
    let mut left = rng.unit() * total;
    for (&it, &w) in items.iter().zip(weights) {
        left -= w;
        if left <= 0.0 {
            return Some(it);
        }
    }
    items.last().copied()
}

/// A quest's detail, as it is announced and shown (`_quest_detail`, `city_states.py:1070-1085`).
fn detail(g: &Game, q: &Quest) -> String {
    let r = g.rules();
    match q.target {
        QuestTarget::Tile(t) => format!(" (camp at {})", g.fmt_xy(t)),
        QuestTarget::Resource(res) => format!(" ({})", r.resources()[res].name),
        QuestTarget::Building(b) => format!(" ({})", r.buildings()[b].name),
        QuestTarget::UnitType(u) => format!(" ({})", r.base_units()[u].name),
        QuestTarget::NaturalWonder(w) => format!(" ({})", r.terrains()[w].name),
        QuestTarget::Player(p) => format!(" ({})", name(g, p)),
        QuestTarget::Religion(rel) => format!(" ({})", religion::display_name(g, rel)),
        QuestTarget::Percent(p) => format!(" (gold gifts give {p}% more influence)"),
        QuestTarget::None | QuestTarget::Baseline(_) => String::new(),
    }
}

/// A quest as a sentence a player can act on (`quest_text`, `city_states.py:1065-1067`).
#[must_use]
pub fn quest_text(g: &Game, q: &Quest) -> String {
    format!("{}{}", quest_name(g, q), detail(g, q))
}

/// Gives a major a quest (`_assign`, `city_states.py:1055-1062`), with its row's influence (40
/// by default) and its duration scaled by the speed.
fn assign(
    g: &mut Game,
    cs: PlayerId,
    k: QuestKindId,
    major: PlayerId,
    target: QuestTarget,
    scope: QuestScope,
) {
    let def = &g.rules().quests()[k];
    let q = Quest {
        kind: k,
        assignee: major,
        turn: g.turn(),
        scope,
        target,
        influence: def.influence.unwrap_or(40),
        duration: num::trunc_i32(g.speed().modifier * f64::from(def.duration.unwrap_or(0))),
    };
    let text = format!("{} assigned you a new quest: {}.", name(g, cs), quest_text(g, &q));
    if let Some(d) = data_mut(g, cs) {
        d.quests.push(q);
    }
    tell(g, cs, major, &text);
}

/// Whether a quest has run out of time (`_expired`).
fn expired(g: &Game, q: &Quest) -> bool {
    q.duration > 0 && g.turn() >= q.turn + q.duration
}

/// Whether an individual quest is done (`_complete`, `city_states.py:1093-1119`).
fn complete(g: &Game, cs: PlayerId, q: &Quest) -> bool {
    let m = q.assignee;
    let cap = g.player(cs).and_then(|p| p.capital).and_then(|c| g.city(c));
    let r = g.rules();
    match (kind_of(g, q), q.target) {
        (QuestKind::Route, _) => cap.is_some_and(|c| route_connected(g, m, c.tile())),
        (QuestKind::ConstructWonder, QuestTarget::Building(b)) => {
            g.player_cities(m).any(|c| c.buildings.contains(b))
        }
        (QuestKind::ConnectResource, QuestTarget::Resource(res)) => {
            resources_of(g, m).contains(res)
        }
        (QuestKind::AcquireGreatPerson, QuestTarget::UnitType(t)) => {
            g.player_units(m).any(|u| u.base == t || r.base_units()[u.base].replaces == Some(t))
        }
        (QuestKind::FindPlayer, QuestTarget::Player(d)) => {
            g.player_cities(d).any(|c| g.player(m).is_some_and(|p| p.explored.contains(c.tile().0)))
        }
        (QuestKind::FindNaturalWonder, QuestTarget::NaturalWonder(w)) => {
            g.player(m).is_some_and(|p| p.civ.natural_wonders.contains(w))
        }
        (QuestKind::PledgeToProtect, _) => data(g, cs).is_some_and(|d| d.protectors.contains(m)),
        (QuestKind::DenounceCivilization, QuestTarget::Player(d)) => ever_denounced(g, m, d),
        (QuestKind::SpreadReligion, QuestTarget::Religion(rel)) => {
            cap.is_some_and(|c| religion::majority_religion(g, c.id()) == Some(rel))
        }
        _ => false,
    }
}

/// Whether a quest no longer makes sense (`_obsolete`, `city_states.py:1122-1132`): its camp is
/// gone, another built its wonder, or the civilization it names is gone.
fn obsolete(g: &Game, q: &Quest) -> bool {
    match (kind_of(g, q), q.target) {
        (QuestKind::ClearBarbarianCamp, QuestTarget::Tile(t)) => !is_camp_tile(g, t),
        (QuestKind::ConstructWonder, QuestTarget::Building(b)) => g
            .state()
            .world()
            .wonders_built
            .get(&b)
            .and_then(|&c| g.city(c))
            .is_some_and(|c| c.owner() != q.assignee),
        (
            QuestKind::ConquerCityState
            | QuestKind::BullyCityState
            | QuestKind::FindPlayer
            | QuestKind::DenounceCivilization,
            QuestTarget::Player(p),
        ) => !g.player(p).is_some_and(Player::alive),
        _ => false,
    }
}

/// Pays a quest's influence (`_reward`, `city_states.py:1135-1141`).
fn reward(g: &mut Game, cs: PlayerId, q: &Quest) {
    super::actions::refused(add_influence(g, cs, q.assignee, f64::from(q.influence)));
    if q.influence > 0 {
        let text = format!(
            "{} rewarded you with {} influence for completing the {} quest.",
            name(g, cs),
            q.influence,
            quest_name(g, q)
        );
        tell(g, cs, q.assignee, &text);
    }
}

/// Rewards and drops the quests that `keep` says are done, in order.
fn settle_quests(g: &mut Game, cs: PlayerId, done: impl Fn(&Quest) -> bool) {
    let Some(quests) = data(g, cs).map(|d| d.quests.clone()) else { return };
    let (hit, kept): (Vec<Quest>, Vec<Quest>) = quests.into_iter().partition(|q| done(q));
    if hit.is_empty() {
        return;
    }
    if let Some(d) = data_mut(g, cs) {
        d.quests = kept;
    }
    for q in &hit {
        reward(g, cs, q);
    }
}

/// Completes a major's quests of a kind as it does what they ask (`_complete_quests`,
/// `city_states.py:1144-1153`): gold given, protection pledged.
pub fn complete_quests(g: &mut Game, cs: PlayerId, major: PlayerId, kind: QuestKind) {
    let r = g.rules();
    settle_quests(g, cs, |q| q.assignee == major && r.quests()[q.kind].kind == kind);
}

/// Completes a major's quests of a kind about `target` (any, with none) as something happens
/// (`_quest_event`, `city_states.py:1156-1165`): a city-state bullied or conquered.
pub fn quest_event(
    g: &mut Game,
    cs: PlayerId,
    kind: QuestKind,
    major: PlayerId,
    target: Option<PlayerId>,
) {
    let r = g.rules();
    settle_quests(g, cs, |q| {
        q.assignee == major
            && r.quests()[q.kind].kind == kind
            && target.is_none_or(|t| q.target == QuestTarget::Player(t))
    });
}

/// A camp cleared by a major: the city-states that wanted it cleared reward that major, and
/// forget the quest for everyone (`camp_cleared`, `city_states.py:1168-1175`).
pub fn camp_cleared(g: &mut Game, t: TileIdx, major: PlayerId) {
    let r = g.rules();
    let css: Vec<PlayerId> = g.city_states(true).map(Player::id).collect();
    for cs in css {
        let Some(quests) = data(g, cs).map(|d| d.quests.clone()) else { continue };
        let about = |q: &Quest| {
            r.quests()[q.kind].kind == QuestKind::ClearBarbarianCamp
                && q.target == QuestTarget::Tile(t)
        };
        if !quests.iter().any(about) {
            continue;
        }
        let win = quests.iter().find(|q| about(q) && q.assignee == major).copied();
        if let Some(d) = data_mut(g, cs) {
            d.quests.retain(|q| !about(q));
        }
        if let Some(q) = win {
            reward(g, cs, &q);
        }
    }
}

/// A camp gone without a unit clearing it: its quests go with it (`camp_removed`,
/// `city_states.py:1300-1303`).
pub fn camp_removed(g: &mut Game, t: TileIdx) {
    let r = g.rules();
    let css: Vec<PlayerId> = g.city_states(true).map(Player::id).collect();
    for cs in css {
        let about = |q: &Quest| {
            r.quests()[q.kind].kind == QuestKind::ClearBarbarianCamp
                && q.target == QuestTarget::Tile(t)
        };
        if data(g, cs).is_some_and(|d| d.quests.iter().any(about))
            && let Some(d) = data_mut(g, cs)
        {
            d.quests.retain(|q| !about(q));
        }
    }
}

/// A contest's score for its assignee: what it gained since the quest was given
/// (`_resolve_contest`'s `score`).
fn contest_score(g: &Game, q: &Quest) -> i64 {
    let QuestTarget::Baseline(base) = q.target else { return 0 };
    let Some(p) = g.player(q.assignee) else { return 0 };
    match kind_of(g, q) {
        QuestKind::ContestCulture => p.econ.total_culture - base,
        QuestKind::ContestFaith => p.econ.total_faith - base,
        QuestKind::ContestTechnologies => tech_score(g, q.assignee) - base,
        _ => 0,
    }
}

/// A global quest that ran out is decided (`_resolve_contest`, `city_states.py:1178-1200`): the
/// best score wins, if it is above 0; everyone else is told it has ended.
fn resolve_contest(g: &mut Game, cs: PlayerId, k: QuestKindId) {
    let Some(quests) = data(g, cs).map(|d| d.quests.clone()) else { return };
    let mine: Vec<Quest> = quests.iter().filter(|q| q.kind == k).copied().collect();
    let best = mine.iter().map(|q| contest_score(g, q)).max().unwrap_or(0);
    if let Some(d) = data_mut(g, cs) {
        d.quests.retain(|q| q.kind != k);
    }
    for q in &mine {
        let s = contest_score(g, q);
        if s > 0 && s == best {
            reward(g, cs, q);
        } else {
            let text = format!("The {} quest for {} has ended.", quest_name(g, q), name(g, cs));
            tell(g, cs, q.assignee, &text);
        }
    }
}

/// The quests a city-state could give now, by row, of a scope.
fn rows(g: &Game, scope: QuestScope) -> Vec<QuestKindId> {
    g.rules().quests().iter().filter(|(_, d)| d.scope == scope).map(|(k, _)| k).collect()
}

/// A city-state's quests advance a turn (`_quests_end_turn`, `city_states.py:977-1052`): the
/// countdowns, the quests of the dead and of enemies dropped, contests decided, individual quests
/// rewarded or dropped, then new quests given; and a call for help when barbarians close in.
pub fn quests_end_turn(g: &mut Game, cs: PlayerId) {
    if g.player_cities(cs).next().is_none() || data(g, cs).is_none() {
        return;
    }
    let turn = g.turn();
    let mut rng = Rng::keyed(g.state().seed(), Purpose::Quests, &[cs.key(), turn.key()]);
    let sp = g.speed().modifier;
    let majors: Vec<PlayerId> = g.majors(true).map(Player::id).collect();
    let mut timers = data(g, cs).map(|d| d.timers.clone()).unwrap_or_default();
    if turn >= 30 {
        let draw = |rng: &mut Rng, first: u64, later: i64, spread: u64| {
            let n = if turn == 30 {
                i64::try_from(rng.below(first)).unwrap_or(0)
            } else {
                later + i64::try_from(rng.below(spread)).unwrap_or(0)
            };
            #[allow(clippy::cast_precision_loss, reason = "a few dozen turns")]
            i16::try_from(num::trunc_i64(n as f64 * sp)).unwrap_or(i16::MAX)
        };
        if timers.global == -1 {
            timers.global = draw(&mut rng, 20, 40, 25);
        }
        for &m in &majors {
            if timers.individual(m) == -1 {
                timers.individual.insert(m, draw(&mut rng, 20, 20, 25));
            }
        }
    }
    if timers.global > 0 {
        timers.global -= 1;
    }
    for v in timers.individual.values_mut() {
        if *v > 0 {
            *v -= 1;
        }
    }
    let live = |g: &Game, q: &Quest| {
        g.player(q.assignee).is_some_and(Player::alive) && !g.at_war(cs, q.assignee)
    };
    let quests = data(g, cs).map(|d| d.quests.clone()).unwrap_or_default();
    let kept: Vec<Quest> = quests.into_iter().filter(|q| live(g, q)).collect();
    if let Some(d) = data_mut(g, cs) {
        d.timers = timers;
        d.quests = kept.clone();
    }
    let mut ended: Vec<QuestKindId> = kept
        .iter()
        .filter(|q| q.scope == QuestScope::Global && expired(g, q))
        .map(|q| q.kind)
        .collect();
    ended.sort();
    ended.dedup();
    for k in ended {
        resolve_contest(g, cs, k);
    }
    // Individual quests.
    let quests = data(g, cs).map(|d| d.quests.clone()).unwrap_or_default();
    let mut keep = Vec::with_capacity(quests.len());
    // Each quest done or dropped, in order: true for done.
    let mut ends: Vec<(Quest, bool)> = Vec::new();
    for q in quests {
        if q.scope != QuestScope::Individual {
            keep.push(q);
        } else if complete(g, cs, &q) {
            ends.push((q, true));
        } else if obsolete(g, &q) || expired(g, &q) {
            ends.push((q, false));
        } else {
            keep.push(q);
        }
    }
    if let Some(d) = data_mut(g, cs) {
        d.quests = keep;
    }
    for (q, done) in &ends {
        if *done {
            reward(g, cs, q);
        } else {
            let text = format!(
                "{} no longer needs your help with the {} quest.",
                name(g, cs),
                quest_name(g, q)
            );
            tell(g, cs, q.assignee, &text);
        }
    }
    // A new global quest.
    let has_global = |g: &Game| {
        data(g, cs).is_some_and(|d| d.quests.iter().any(|q| q.scope == QuestScope::Global))
    };
    if data(g, cs).is_some_and(|d| d.timers.global == 0) && !has_global(g) {
        let open: Vec<PlayerId> =
            majors.iter().copied().filter(|&m| g.has_met(cs, m) && !g.at_war(cs, m)).collect();
        let cands: Vec<QuestKindId> = rows(g, QuestScope::Global)
            .into_iter()
            .filter(|&k| {
                let least = g.rules().quests()[k].minimum_civs.unwrap_or(1);
                let fits = open.iter().filter(|&&m| quest_target(g, cs, k, m).is_some()).count();
                i64::try_from(fits).unwrap_or(0) >= i64::from(least)
            })
            .collect();
        if !cands.is_empty() {
            let weights: Vec<f64> = cands.iter().map(|&k| quest_weight(g, cs, k)).collect();
            if let Some(k) = weighted(&mut rng, &cands, &weights) {
                for &m in &open {
                    if let Some(t) = quest_target(g, cs, k, m) {
                        assign(g, cs, k, m, t, QuestScope::Global);
                    }
                }
            }
            if let Some(d) = data_mut(g, cs) {
                d.timers.global = -1;
            }
        }
    }
    // New individual quests.
    let due: Vec<(PlayerId, i16)> = data(g, cs)
        .map(|d| d.timers.individual.iter().map(|(&m, &c)| (m, c)).collect())
        .unwrap_or_default();
    for (m, cd) in due {
        if cd != 0 || !g.player(m).is_some_and(Player::alive) {
            continue;
        }
        let quests = data(g, cs).map(|d| d.quests.clone()).unwrap_or_default();
        let theirs = |q: &Quest| q.scope == QuestScope::Individual && q.assignee == m;
        if quests.iter().filter(|q| theirs(q)).count() >= 2 || pair(g, cs, m).bullied > 0 {
            continue;
        }
        let cands: Vec<QuestKindId> = rows(g, QuestScope::Individual)
            .into_iter()
            .filter(|&k| {
                !quests.iter().any(|q| q.kind == k && q.assignee == m)
                    && quest_target(g, cs, k, m).is_some()
            })
            .collect();
        if cands.is_empty() {
            continue;
        }
        let weights: Vec<f64> = cands.iter().map(|&k| quest_weight(g, cs, k)).collect();
        let Some(k) = weighted(&mut rng, &cands, &weights) else { continue };
        if let Some(t) = quest_target(g, cs, k, m) {
            assign(g, cs, k, m, t, QuestScope::Individual);
        }
        if let Some(d) = data_mut(g, cs) {
            d.timers.individual.insert(m, -1);
        }
    }
    // A call for help against the barbarians.
    let cooldown = data(g, cs).map_or(0, |d| d.barb_help_cd);
    if super::turn::threatening_barbarians(g, cs) >= 2 && cooldown == 0 {
        let text = format!(
            "{} is being invaded by barbarians! Destroy barbarians near their territory to earn \
             influence.",
            name(g, cs)
        );
        for &m in &majors {
            if g.has_met(cs, m) && !g.at_war(cs, m) {
                tell(g, cs, m, &text);
            }
        }
        if let Some(d) = data_mut(g, cs) {
            d.barb_help_cd = 30;
        }
    }
    if let Some(d) = data_mut(g, cs)
        && d.barb_help_cd > 0
    {
        d.barb_help_cd -= 1;
    }
}

/// The quests the city-states a major has met offer it (`quests_for`, `city_states.py:1203-1214`):
/// `city_state`, `city_state_id`, `quest`, `influence` and `turns_left` (null without a limit).
#[must_use]
pub fn quests_for(g: &Game, major: PlayerId) -> Vec<Value> {
    let mut out = Vec::new();
    for q in g.city_states(true) {
        if !g.has_met(major, q.id()) {
            continue;
        }
        let Some(d) = q.city_state.as_deref() else { continue };
        for x in d.quests.iter().filter(|x| x.assignee == major) {
            let left = (x.duration != 0).then(|| x.turn + x.duration - g.turn());
            out.push(json!({
                "city_state": &*q.name, "city_state_id": q.id().0, "quest": quest_text(g, x),
                "influence": x.influence, "turns_left": left,
            }));
        }
    }
    out
}

/// A city-state's quests as scripts read them: `name`, `assignee`, `scope`, `turn`, `influence`,
/// `duration` and `text`.
#[must_use]
pub fn quests_json(g: &Game, cs: PlayerId) -> Value {
    let Some(d) = data(g, cs) else { return Value::Array(Vec::new()) };
    Value::Array(
        d.quests
            .iter()
            .map(|q| {
                let scope = match q.scope {
                    QuestScope::Individual => "individual",
                    QuestScope::Global => "global",
                };
                json!({
                    "name": quest_name(g, q), "assignee": q.assignee.0, "scope": scope,
                    "turn": q.turn, "influence": q.influence, "duration": q.duration,
                    "text": quest_text(g, q),
                })
            })
            .collect(),
    )
}
