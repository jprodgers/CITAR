//! Deals: what each side of a proposal gives, whether it can, how it reads, and carrying it out
//! (`diplomacy.py:340-677`).
//!
//! - [`normalize_items`] and [`make_proposal`] read the items a caller writes as Python's
//!   `_normalize_items` and `_make_proposal` did (`diplomacy.py:343-383, 732-749`): leniently,
//!   numbers through Python's `int()` and names through the ruleset's loose lookup, into typed
//!   [`DealItem`]s. Mutual agreements go on both sides. A caller that holds typed items (a bot,
//!   the host) goes through [`fit_item`] and [`proposal_of`] instead, which check them as
//!   reading written ones would.
//! - [`validate_items`] refuses what a side cannot give (`diplomacy.py:400-492`), at proposal
//!   time, so an impossible deal is refused to whoever proposes it; [`describe_items`] and
//!   [`ra_cost`] are Python's.
//! - [`plan_deal`] and [`execute_deal`] split `execute_deal` (`diplomacy.py:530-609`) into the
//!   checks, which only read, and the transfer, which cannot fail.
//! - [`process_round`] is the round's end for deals and agreements (`diplomacy.py:644-677`): cut
//!   trades, expired deals and open borders, and research agreements paying out.
//!
//! What differs, on purpose (tests/rules/intended.toml, `deal-items-fit-their-fields`): an item
//! whose player id, city id or amount no game can hold is refused when it is proposed, where
//! Python stored it and refused the deal only when it was accepted (or never, for an amount).
//! Keys an item does not have are dropped, where Python kept them in the stored item. Mutual
//! agreements are added in [`DealItemKind::ALL`] order where Python followed a set's.

use serde_json::{Map, Value};

use super::relations::{
    WarReason, add_opinion, can_declare_war, civ_has, denounced, has_embassy, has_pact, is_friends,
    make_peace, meets_embassy_requirement, name, set_war,
};
use crate::base::ids::{CityId, DealId, PlayerId, ResourceId, TechId, TileIdx};
use crate::base::num;
use crate::base::py;
use crate::base::sets::PlayerSet;
use crate::base::stats::Stat;
use crate::game::derive::rev::{DiploTouch, PlayerTouch};
use crate::game::error::ActionError;
use crate::game::research::{self, TechSource};
use crate::game::{Game, conquest, economy, movement, triggers};
use crate::rules::defs::ResourceType;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::diplo::{
    Deal, DealItem, DealItemKind, Ongoing, OpinionKey, Relation, Side, Terms, side,
};
use crate::unique::UniqueType;
use crate::unique::trigger::{TriggerEvent, TriggerSite};

/// The item types, as the refusal of an unknown one lists them (`', '.join(ITEM_TYPES)`).
fn type_names() -> String {
    DealItemKind::ALL.iter().map(|k| k.name()).collect::<Vec<_>>().join(", ")
}

/// Python's `int()` of an item's field, or of its default when the item lacks it.
fn int_field(it: &Map<String, Value>, key: &str, default: Option<i64>) -> Option<i64> {
    match it.get(key) {
        Some(v) => py::int_of(v),
        None => default,
    }
}

/// A name the ruleset's loose lookup reads (`rules.resolve`): a string as it is, a number or a
/// boolean as Python's `str` writes it; a list or an object, which Python could not hash, reads
/// as nothing and is malformed.
fn name_text(v: Option<&Value>) -> Result<Option<String>, ()> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(_) | Value::Object(_)) => Err(()),
        Some(v) => Ok(Some(py::str_of(v))),
    }
}

/// Refuses a gold amount that is not positive, and a lump sum above the ruleset's most.
fn check_gold(g: &Game, kind: DealItemKind, n: i64) -> Result<(), ActionError> {
    if n <= 0 {
        return Err(ActionError::rule("Gold amounts must be positive."));
    }
    let most = g.rules().constants().formulas.max_gold_trade_offer;
    if kind == DealItemKind::Gold && n > i64::from(most) {
        return Err(ActionError::rule(format!("At most {most} gold per deal.")));
    }
    Ok(())
}

/// Whether a resource of the ruleset can be traded: a strategic or luxury one.
fn tradeable(g: &Game, id: ResourceId) -> bool {
    g.rules().resources().get(id).is_some_and(|d| d.kind != ResourceType::Bonus)
}

/// The turns a recurring term runs, held to 1..100 as Python held them.
fn held(turns: i64) -> i64 {
    turns.clamp(1, 100)
}

/// Reads one item a caller wrote (`_normalize_items`, `diplomacy.py:351-382`). `giver` names
/// the side that gives it, for the refusal of a city id no game can hold.
fn normalize_item(g: &Game, giver: PlayerId, raw: &Value) -> Result<DealItem, ActionError> {
    let kind = raw
        .as_object()
        .and_then(|m| m.get("type"))
        .and_then(Value::as_str)
        .and_then(DealItemKind::from_name);
    let (Some(kind), Some(orig)) = (kind, raw.as_object()) else {
        return Err(ActionError::rule(format!(
            "Invalid deal item {}. Valid types: {}.",
            py::repr(raw),
            type_names()
        )));
    };
    // Python converted the fields of a copy in place, and quoted the copy as it then was.
    let mut it = orig.clone();
    let malformed = |it: &Map<String, Value>| {
        ActionError::rule(format!("Malformed deal item {}.", py::repr(&Value::Object(it.clone()))))
    };
    let r = g.rules();
    // refcheck: deal-items-fit-their-fields
    let fit = |n: i64, it: &Map<String, Value>| i32::try_from(n).map_err(|_| malformed(it));
    let mut amount = 0i32;
    if matches!(kind, DealItemKind::Gold | DealItemKind::GoldPerTurn) {
        let n = int_field(&it, "amount", Some(0)).ok_or_else(|| malformed(&it))?;
        it.insert("amount".to_owned(), n.into());
        check_gold(g, kind, n)?;
        amount = fit(n, &it)?;
    }
    let mut turns = 0i32;
    if matches!(
        kind,
        DealItemKind::GoldPerTurn | DealItemKind::Resource | DealItemKind::OpenBorders
    ) {
        let dd = i64::from(g.speed().deal_duration);
        let n = int_field(&it, "turns", Some(dd)).ok_or_else(|| malformed(&it)).map(held)?;
        it.insert("turns".to_owned(), n.into());
        turns = fit(n, &it)?;
    }
    Ok(match kind {
        DealItemKind::Gold => DealItem::Gold { amount },
        DealItemKind::GoldPerTurn => DealItem::GoldPerTurn { amount, turns },
        DealItemKind::Resource => {
            let n = int_field(&it, "amount", Some(1)).ok_or_else(|| malformed(&it))?.max(1);
            it.insert("amount".to_owned(), n.into());
            let amount = fit(n, &it)?;
            let text = name_text(it.get("resource")).map_err(|()| malformed(&it))?;
            let resource = text
                .as_deref()
                .and_then(|t| r.resolve::<ResourceId>(t))
                .filter(|&id| tradeable(g, id));
            let Some(resource) = resource else {
                let shown = it.get("resource").map_or_else(|| "None".to_owned(), py::str_of);
                return Err(ActionError::rule(format!(
                    "'{shown}' is not a tradeable strategic or luxury resource."
                )));
            };
            DealItem::Resource { resource, amount, turns }
        }
        DealItemKind::OpenBorders => DealItem::OpenBorders { turns },
        DealItemKind::Embassy => DealItem::Embassy,
        DealItemKind::PeaceTreaty => DealItem::PeaceTreaty,
        DealItemKind::DeclarationOfFriendship => DealItem::DeclarationOfFriendship,
        DealItemKind::ResearchAgreement => DealItem::ResearchAgreement,
        DealItemKind::DefensivePact => DealItem::DefensivePact,
        DealItemKind::DeclareWar => {
            let n = it.get("target").and_then(py::int_of).ok_or_else(|| malformed(&it))?;
            let target = u8::try_from(n)
                .map(PlayerId)
                .map_err(|_| ActionError::rule("Invalid war target."))?;
            DealItem::DeclareWar { target }
        }
        DealItemKind::City => {
            let n = it.get("city_id").and_then(py::int_of).ok_or_else(|| malformed(&it))?;
            let city_id = u32::try_from(n).ok().and_then(CityId::new).ok_or_else(|| {
                ActionError::rule(format!("{} does not own city {n}.", name(g, giver)))
            })?;
            DealItem::City { city_id }
        }
        DealItemKind::ShareMap => DealItem::ShareMap,
        DealItemKind::Tech => {
            let text = name_text(it.get("tech")).map_err(|()| malformed(&it))?;
            let Some(tech) = text.as_deref().and_then(|t| r.resolve::<TechId>(t)) else {
                let shown = it.get("tech").map_or_else(|| "None".to_owned(), py::str_of);
                return Err(ActionError::rule(format!("Unknown tech '{shown}'.")));
            };
            DealItem::Tech { tech }
        }
    })
}

/// Reads the items one side would give, as a caller writes them (`_normalize_items`,
/// `diplomacy.py:343-383`): nothing for none, else a list of item objects.
///
/// # Errors
/// Not a list, an item of no known type, or one whose fields do not read.
pub fn normalize_items(
    g: &Game,
    giver: PlayerId,
    items: Option<&Value>,
) -> Result<Vec<DealItem>, ActionError> {
    match items {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(list)) => list.iter().map(|it| normalize_item(g, giver, it)).collect(),
        Some(_) => Err(ActionError::rule(
            "Deal items must be a list of objects like {\"type\": \"gold\", \"amount\": 50}.",
        )),
    }
}

/// A proposal from what `speaker` would give and what it would receive from `other`, or `None`
/// when there is nothing concrete (`_make_proposal`, `diplomacy.py:732-749`): neither list
/// given, or both empty, since a proposal in which neither side gives anything could be
/// "accepted" into a deal of nothing. A mutual agreement on either side goes on both.
///
/// # Errors
/// An item that does not read.
pub fn make_proposal(
    g: &Game,
    speaker: PlayerId,
    other: PlayerId,
    give: Option<&Value>,
    receive: Option<&Value>,
) -> Result<Option<Terms>, ActionError> {
    let absent = |v: Option<&Value>| v.is_none_or(Value::is_null);
    if absent(give) && absent(receive) {
        return Ok(None);
    }
    Ok(complete_mutual([
        Side { giver: speaker, items: normalize_items(g, speaker, give)? },
        Side { giver: other, items: normalize_items(g, other, receive)? },
    ]))
}

/// A typed item checked and settled as [`normalize_items`] reads a written one: gold amounts
/// positive and a lump sum within the ruleset's most, recurring terms held to 1..100 turns, a
/// resource amount of at least one of a tradeable resource, a tech the ruleset has.
///
/// # Errors
/// What reading the same item written would refuse.
pub fn fit_item(g: &Game, item: DealItem) -> Result<DealItem, ActionError> {
    // An i32 held to 1..100 fits an i32.
    let turns = |t: i32| i32::try_from(held(i64::from(t))).unwrap_or(1);
    Ok(match item {
        DealItem::Gold { amount } => {
            check_gold(g, DealItemKind::Gold, i64::from(amount))?;
            item
        }
        DealItem::GoldPerTurn { amount, turns: t } => {
            check_gold(g, DealItemKind::GoldPerTurn, i64::from(amount))?;
            DealItem::GoldPerTurn { amount, turns: turns(t) }
        }
        DealItem::Resource { resource, amount, turns: t } => {
            if !tradeable(g, resource) {
                let shown =
                    g.rules().name(resource).map_or_else(|| resource.0.to_string(), str::to_owned);
                return Err(ActionError::rule(format!(
                    "'{shown}' is not a tradeable strategic or luxury resource."
                )));
            }
            DealItem::Resource { resource, amount: amount.max(1), turns: turns(t) }
        }
        DealItem::OpenBorders { turns: t } => DealItem::OpenBorders { turns: turns(t) },
        DealItem::Tech { tech } => {
            if g.rules().techs().get(tech).is_none() {
                return Err(ActionError::rule(format!("Unknown tech '{}'.", tech.0)));
            }
            item
        }
        DealItem::Embassy
        | DealItem::PeaceTreaty
        | DealItem::DeclarationOfFriendship
        | DealItem::ResearchAgreement
        | DealItem::DefensivePact
        | DealItem::DeclareWar { .. }
        | DealItem::City { .. }
        | DealItem::ShareMap => item,
    })
}

/// A proposal from typed items: what `speaker` would give, and what it would receive from
/// `other`, each checked by [`fit_item`]; `None` when neither side gives anything, as
/// [`make_proposal`].
///
/// # Errors
/// The first item [`fit_item`] refuses.
pub fn proposal_of(
    g: &Game,
    speaker: PlayerId,
    other: PlayerId,
    give: &[DealItem],
    receive: &[DealItem],
) -> Result<Option<Terms>, ActionError> {
    let fit =
        |items: &[DealItem]| items.iter().map(|&it| fit_item(g, it)).collect::<Result<Vec<_>, _>>();
    Ok(complete_mutual([
        Side { giver: speaker, items: fit(give)? },
        Side { giver: other, items: fit(receive)? },
    ]))
}

/// The two sides as a proposal, or `None` when neither gives anything; a mutual agreement on
/// either side goes on both, in [`DealItemKind::ALL`] order (`_make_proposal`,
/// `diplomacy.py:740-749`).
#[must_use]
pub fn complete_mutual(mut sides: [Side; 2]) -> Option<Terms> {
    if sides.iter().all(|s| s.items.is_empty()) {
        return None;
    }
    for kind in DealItemKind::ALL.into_iter().filter(|k| k.is_mutual()) {
        if sides.iter().any(|s| s.items.iter().any(|i| i.kind() == kind)) {
            for s in &mut sides {
                if !s.items.iter().any(|i| i.kind() == kind) {
                    s.items.push(mutual(kind));
                }
            }
        }
    }
    Some(Terms { sides })
}

/// The item of a mutual agreement's kind.
const fn mutual(kind: DealItemKind) -> DealItem {
    match kind {
        DealItemKind::PeaceTreaty => DealItem::PeaceTreaty,
        DealItemKind::ResearchAgreement => DealItem::ResearchAgreement,
        DealItemKind::DefensivePact => DealItem::DefensivePact,
        _ => DealItem::DeclarationOfFriendship,
    }
}

/// The gold each side pays for a research agreement: the dearer of the two civilizations' eras'
/// cost, scaled by the speed (`ra_cost`, `diplomacy.py:391-397`).
#[must_use]
pub fn ra_cost(g: &Game, a: PlayerId, b: PlayerId) -> i32 {
    let r = g.rules();
    let cost = |p| r.eras()[crate::game::derive::civ::era(g, p)].research_agreement_cost;
    num::trunc_i32(f64::from(cost(a).max(cost(b))) * g.speed().gold_cost_modifier)
}

/// Whether a proposal holds an item of this kind, on either side (`_has`).
fn has(proposal: &Terms, kind: DealItemKind) -> bool {
    proposal.has(kind)
}

/// Python's `int()` of a stock of gold, for messages.
fn whole(x: f64) -> i64 {
    num::trunc_i64(x)
}

/// Checks that `giver` can give each of `items` to `receiver` under `proposal`, or says which it
/// cannot (`validate_items`, `diplomacy.py:400-492`). Checked when a side proposes, so an
/// impossible deal is refused to whoever proposes it, and again when the deal is accepted.
///
/// # Errors
/// The first item `giver` cannot give, then gold it does not have.
pub fn validate_items(
    g: &Game,
    giver: PlayerId,
    receiver: PlayerId,
    items: &[DealItem],
    proposal: &Terms,
) -> Result<(), ActionError> {
    let refuse = |m: String| Err(ActionError::rule(m));
    let (gn, rn) = (name(g, giver), name(g, receiver));
    let at_war = g.at_war(giver, receiver);
    let gold = g.player(giver).map_or(0.0, |p| p.econ.gold);
    let turn = g.turn();
    let mut total_gold: i64 = 0;
    for it in items {
        match *it {
            DealItem::Gold { amount } => total_gold += i64::from(amount),
            DealItem::GoldPerTurn { amount, .. } => {
                let net = crate::game::derive::stats::civ_stats(g, giver).total[Stat::Gold];
                if net < f64::from(amount) {
                    return refuse(format!(
                        "{gn} only makes {} gold per turn; cannot pay {amount} per turn.",
                        whole(net)
                    ));
                }
            }
            DealItem::Resource { resource, amount, .. } => {
                let have = economy::resource_amount(g, giver, resource);
                if have < amount {
                    let rname = g.rules().name(resource).unwrap_or_default();
                    return refuse(format!(
                        "{gn} does not have {amount} spare {rname} (has {have})."
                    ));
                }
            }
            DealItem::OpenBorders { .. } => {
                if at_war && !has(proposal, DealItemKind::PeaceTreaty) {
                    return refuse(
                        "Open borders cannot be exchanged while at war (without a peace treaty)."
                            .to_owned(),
                    );
                }
                if !civ_has(g, giver, UniqueType::EnablesOpenBorders)
                    || !civ_has(g, receiver, UniqueType::EnablesOpenBorders)
                {
                    return refuse(
                        "Open borders need the right technology (Writing line) on both sides."
                            .to_owned(),
                    );
                }
                if !meets_embassy_requirement(g, giver, receiver) {
                    return refuse(
                        "Open borders require embassies in each other's capitals first.".to_owned(),
                    );
                }
            }
            DealItem::Embassy => {
                if !civ_has(g, giver, UniqueType::EnablesEmbassies)
                    || !civ_has(g, receiver, UniqueType::EnablesEmbassies)
                {
                    return refuse("Embassies need Writing on both sides.".to_owned());
                }
                if has_embassy(g, receiver, giver) {
                    return refuse(format!("{rn} already has an embassy with {gn}."));
                }
                if g.player(giver).and_then(|p| p.capital).is_none() {
                    return refuse(format!("{gn} has no capital."));
                }
            }
            DealItem::PeaceTreaty => {
                if !at_war {
                    return refuse(format!("{gn} and {rn} are not at war."));
                }
            }
            DealItem::DeclarationOfFriendship => {
                if at_war || denounced(g, giver, receiver) || denounced(g, receiver, giver) {
                    return refuse(
                        "Friendship is impossible while at war or after a denunciation.".to_owned(),
                    );
                }
                if is_friends(g, giver, receiver) {
                    return refuse("You are already friends.".to_owned());
                }
            }
            DealItem::ResearchAgreement => {
                if !civ_has(g, giver, UniqueType::EnablesResearchAgreements)
                    || !civ_has(g, receiver, UniqueType::EnablesResearchAgreements)
                {
                    return refuse("Research agreements need Education on both sides.".to_owned());
                }
                if !(is_friends(g, giver, receiver)
                    || has(proposal, DealItemKind::DeclarationOfFriendship))
                {
                    return refuse(
                        "Research agreements require a declaration of friendship.".to_owned(),
                    );
                }
                if !meets_embassy_requirement(g, giver, receiver) {
                    return refuse(
                        "Research agreements require embassies in each other's capitals."
                            .to_owned(),
                    );
                }
                if g.relation(giver, receiver).is_some_and(|r| r.ra_until >= turn) {
                    return refuse("You already have a research agreement.".to_owned());
                }
                let cost = ra_cost(g, giver, receiver);
                if gold < f64::from(cost) {
                    return refuse(format!(
                        "A research agreement costs each side {cost} gold; {gn} has {}.",
                        whole(gold)
                    ));
                }
                if research::all_researched(g, giver) {
                    return refuse(format!("{gn} has nothing left to research."));
                }
            }
            DealItem::DefensivePact => {
                if !civ_has(g, giver, UniqueType::EnablesDefensivePacts)
                    || !civ_has(g, receiver, UniqueType::EnablesDefensivePacts)
                {
                    return refuse("Defensive pacts need Chivalry on both sides.".to_owned());
                }
                if !(is_friends(g, giver, receiver)
                    || has(proposal, DealItemKind::DeclarationOfFriendship))
                {
                    return refuse(
                        "Defensive pacts require a declaration of friendship.".to_owned(),
                    );
                }
                if !meets_embassy_requirement(g, giver, receiver) {
                    return refuse(
                        "Defensive pacts require embassies in each other's capitals.".to_owned(),
                    );
                }
                if has_pact(g, giver, receiver) {
                    return refuse("You already have a defensive pact.".to_owned());
                }
            }
            DealItem::DeclareWar { target } => {
                let living = g.player(target).is_some_and(|p| !p.is_barbarian() && p.alive());
                if target == giver || target == receiver || !living {
                    return refuse("Invalid war target.".to_owned());
                }
                if let Some(why) = can_declare_war(g, giver, target) {
                    return refuse(format!("{gn}: {why}"));
                }
                // The target's pact would bring the receiver into the war against the giver, the
                // deal still in force between two at war.
                // refcheck: deal-war-on-a-partners-pact-refused
                if has_pact(g, target, receiver) {
                    return refuse(format!("{rn} has a defensive pact with {}.", name(g, target)));
                }
            }
            DealItem::City { city_id } => {
                let Some(c) = g.city(city_id).filter(|c| c.owner() == giver) else {
                    return refuse(format!("{gn} does not own city {}.", city_id.get()));
                };
                if g.player(giver).and_then(|p| p.capital) == Some(city_id) {
                    return refuse("Capitals cannot be traded.".to_owned());
                }
                if c.resistance > 0 {
                    return refuse(format!("{} is in resistance.", c.name));
                }
            }
            DealItem::ShareMap => {}
            DealItem::Tech { tech } => {
                if !g.state().config().tech_trading {
                    return refuse("Tech trading is disabled in this game.".to_owned());
                }
                let tname = g.rules().name(tech).unwrap_or_default();
                if !g.has_tech(giver, Some(tech)) {
                    return refuse(format!("{gn} does not know {tname}."));
                }
                if !research::can_research(g, receiver, tech) {
                    return refuse(format!(
                        "{rn} cannot receive {tname} (already known or missing prerequisites)."
                    ));
                }
            }
        }
    }
    #[allow(clippy::cast_precision_loss, reason = "a sum of i32 amounts is exact in an f64")]
    if total_gold as f64 > gold {
        return refuse(format!("{gn} only has {} gold.", whole(gold)));
    }
    Ok(())
}

/// A deal's items as a sentence, for messages and the log (`describe_items`,
/// `diplomacy.py:495-527`): "60 gold, open borders for 30 turns", or "nothing".
#[must_use]
pub fn describe_items(g: &Game, items: &[DealItem]) -> String {
    let r = g.rules();
    let parts: Vec<String> = items
        .iter()
        .map(|it| match *it {
            DealItem::Gold { amount } => format!("{amount} gold"),
            DealItem::GoldPerTurn { amount, turns } => {
                format!("{amount} gold/turn for {turns} turns")
            }
            DealItem::Resource { resource, amount, turns } => {
                format!("{amount} {} for {turns} turns", r.name(resource).unwrap_or_default())
            }
            DealItem::OpenBorders { turns } => format!("open borders for {turns} turns"),
            DealItem::Embassy => "an embassy".to_owned(),
            DealItem::PeaceTreaty => "a peace treaty".to_owned(),
            DealItem::DeclarationOfFriendship => "a declaration of friendship".to_owned(),
            DealItem::ResearchAgreement => "a research agreement".to_owned(),
            DealItem::DefensivePact => "a defensive pact".to_owned(),
            DealItem::DeclareWar { target } => {
                let who =
                    g.player(target).map_or_else(|| target.0.to_string(), |p| p.name.to_string());
                format!("a declaration of war on {who}")
            }
            DealItem::City { city_id } => match g.city(city_id) {
                Some(c) => format!("the city of {}", c.name),
                None => format!("the city of {}", city_id.get()),
            },
            DealItem::ShareMap => "their world map".to_owned(),
            DealItem::Tech { tech } => {
                format!("the technology {}", r.name(tech).unwrap_or_default())
            }
        })
        .collect();
    if parts.is_empty() { "nothing".to_owned() } else { parts.join(", ") }
}

/// Checks a deal between `a` and `b` on these terms, as accepting it does (`execute_deal`,
/// `diplomacy.py:534-539`): each side can give what it gives, and a deal between two at war
/// makes peace.
///
/// # Errors
/// What a side cannot give, or a deal at war with no peace treaty in it.
pub fn plan_deal(g: &Game, a: PlayerId, b: PlayerId, terms: &Terms) -> Result<(), ActionError> {
    validate_items(g, a, b, terms.gives(a), terms)?;
    validate_items(g, b, a, terms.gives(b), terms)?;
    if g.at_war(a, b) && !has(terms, DealItemKind::PeaceTreaty) {
        return Err(ActionError::rule("While at war, a deal must include a peace treaty."));
    }
    Ok(())
}

/// Adds to a stock of gold, as a deal pays it.
fn pay(g: &mut Game, p: PlayerId, gold: f64) {
    if let Some(x) = g.player_mut(p, PlayerTouch::STOCKS) {
        x.econ.gold += gold;
    }
}

/// Edits a relation a deal writes, which exists for any two players of the game.
fn edit(g: &mut Game, a: PlayerId, b: PlayerId, f: impl FnOnce(&mut Relation)) {
    let done = g.update_relation(a, b, f);
    debug_assert!(done.is_ok(), "two players of the game: {done:?}");
}

/// Carries out a deal [`plan_deal`] allowed, and records it (`execute_deal`,
/// `diplomacy.py:540-609`): peace first, then each side's items in order, `a`'s then `b`'s (a
/// mutual agreement once), then the wars agreed to; the deal is announced to both. Returns its
/// id.
///
/// The deal is recorded before the wars, where Python appended it after them: should a war it
/// sets off reach its own parties, which [`validate_items`] refuses, the war ends the deal as
/// any war between them does.
pub fn execute_deal(g: &mut Game, a: PlayerId, b: PlayerId, terms: &Terms) -> Option<DealId> {
    let items_a = terms.gives(a).to_vec();
    let items_b = terms.gives(b).to_vec();
    let id = g.next_deal_id()?;
    let turn = g.turn();
    let mut ongoing = Vec::new();
    let mut done: Vec<DealItemKind> = Vec::new();
    if has(terms, DealItemKind::PeaceTreaty) {
        let peace = make_peace(g, a, b);
        debug_assert!(peace.is_ok(), "two players of the game make peace: {peace:?}");
        done.push(DealItemKind::PeaceTreaty);
    }
    for (giver, receiver, items) in [(a, b, &items_a), (b, a, &items_b)] {
        for &it in items {
            give(g, a, b, giver, receiver, it, turn, &mut ongoing, &mut done);
        }
    }
    let summary = format!(
        "{} gives {}; {} gives {}.",
        name(g, a),
        describe_items(g, &items_a),
        name(g, b),
        describe_items(g, &items_b)
    );
    let wars: Vec<(PlayerId, PlayerId, PlayerId)> = [(a, b, &items_a), (b, a, &items_b)]
        .into_iter()
        .flat_map(|(giver, receiver, items)| {
            items.iter().filter_map(move |it| match *it {
                DealItem::DeclareWar { target } => Some((giver, receiver, target)),
                _ => None,
            })
        })
        .collect();
    g.edit_diplo(DiploTouch::DEALS).deals.push(Deal {
        id,
        turn,
        parties: [a, b],
        terms: Terms {
            sides: [Side { giver: a, items: items_a }, Side { giver: b, items: items_b }],
        },
        ongoing,
        active: true,
        summary: summary.clone().into(),
    });
    for (giver, receiver, target) in wars {
        if can_declare_war(g, giver, target).is_some() {
            continue;
        }
        let war = set_war(g, giver, target, WarReason::Deal);
        debug_assert!(war.is_ok(), "two players of the game go to war: {war:?}");
        let text = format!(
            "{} declared war on {} (as agreed with {})!",
            name(g, giver),
            name(g, target),
            name(g, receiver)
        );
        g.emit(EngineEvent::WarDeclared, &text, None, None, EventData::default(), &[]);
    }
    let text = format!("Deal concluded between {} and {}: {summary}", name(g, a), name(g, b));
    let audience: PlayerSet = [a, b].into_iter().collect();
    let data = EventData { deal: Some(id), ..EventData::default() };
    g.emit(EngineEvent::Deal, &text, Some(audience), None, data, &[]);
    Some(id)
}

/// One item of a deal changes hands (`diplomacy.py:547-596`).
#[allow(clippy::too_many_arguments, reason = "the loop's state, as Python's locals")]
fn give(
    g: &mut Game,
    a: PlayerId,
    b: PlayerId,
    giver: PlayerId,
    receiver: PlayerId,
    it: DealItem,
    turn: i32,
    ongoing: &mut Vec<Ongoing>,
    done: &mut Vec<DealItemKind>,
) {
    match it {
        DealItem::Gold { amount } => {
            pay(g, giver, -f64::from(amount));
            pay(g, receiver, f64::from(amount));
        }
        DealItem::GoldPerTurn { turns, .. } | DealItem::Resource { turns, .. } => {
            ongoing.push(Ongoing { item: it, from: giver, to: receiver, until: turn + turns });
        }
        DealItem::OpenBorders { turns } => {
            edit(g, giver, receiver, |r| {
                r.open_borders_until[side(giver, receiver)] = turn + turns
            });
        }
        DealItem::Embassy => {
            edit(g, giver, receiver, |r| r.embassy[side(receiver, giver)] = true);
            let capital = g.player(giver).and_then(|p| p.capital).and_then(|c| g.city(c));
            if let Some(t) = capital.map(crate::state::cities::City::tile) {
                let around = g.grid().within(t, 2);
                g.reveal_tiles(receiver, &around);
            }
        }
        DealItem::ShareMap => {
            let explored: Vec<TileIdx> = g
                .player(giver)
                .map(|p| p.explored.iter().map(TileIdx).collect())
                .unwrap_or_default();
            g.reveal_tiles(receiver, &explored);
        }
        DealItem::Tech { tech } => research::add_tech(g, receiver, tech, TechSource::Trade),
        DealItem::City { city_id } => {
            conquest::move_to_civ(g, city_id, receiver);
            if let Some(t) = g.city(city_id).map(crate::state::cities::City::tile) {
                let strangers: Vec<_> = g
                    .units_at(t)
                    .filter(|u| u.owner() != receiver)
                    .map(crate::state::units::Unit::id)
                    .collect();
                for u in strangers {
                    movement::teleport_to_closest(g, u);
                }
            }
        }
        DealItem::DeclareWar { .. } => {}
        DealItem::PeaceTreaty
        | DealItem::DeclarationOfFriendship
        | DealItem::ResearchAgreement
        | DealItem::DefensivePact => {
            if done.contains(&it.kind()) {
                return;
            }
            done.push(it.kind());
            agree(g, a, b, it.kind(), turn);
        }
    }
}

/// A mutual agreement between the deal's parties takes effect (`diplomacy.py:576-596`).
fn agree(g: &mut Game, a: PlayerId, b: PlayerId, kind: DealItemKind, turn: i32) {
    let both = |g: &mut Game, e: TriggerEvent| {
        for p in [a, b] {
            triggers::fire(g, &TriggerSite::civ(p), &e, true, None);
        }
    };
    let names = format!("{} and {}", name(g, a), name(g, b));
    let data = EventData { a: Some(a), b: Some(b), ..EventData::default() };
    match kind {
        DealItemKind::DeclarationOfFriendship => {
            edit(g, a, b, |r| r.friendship_until = turn + 30);
            add_opinion(g, a, b, OpinionKey::Friendship, 35.0);
            add_opinion(g, b, a, OpinionKey::Friendship, 35.0);
            let text = format!("{names} signed a Declaration of Friendship!");
            g.emit(EngineEvent::Friendship, &text, None, None, data, &[]);
            both(g, TriggerEvent::DeclaringFriendship);
        }
        DealItemKind::ResearchAgreement => {
            let cost = f64::from(ra_cost(g, a, b));
            pay(g, a, -cost);
            pay(g, b, -cost);
            let until = turn + g.speed().deal_duration;
            edit(g, a, b, |r| {
                r.ra_until = until;
                r.ra_science = [0, 0];
            });
        }
        DealItemKind::DefensivePact => {
            let until = turn + g.speed().deal_duration;
            edit(g, a, b, |r| r.pact_until = until);
            let text = format!("{names} signed a Defensive Pact!");
            g.emit(EngineEvent::Pact, &text, None, None, data, &[]);
            both(g, TriggerEvent::SigningDefensivePact);
        }
        _ => {}
    }
}

/// The round's end for deals and agreements (`process_round`, `diplomacy.py:644-677`): a
/// resource trade whose giver can no longer supply it is cut short; a deal whose recurring parts
/// have all run out expires, and one with none ends; open borders past their turn close; a
/// research agreement in force banks each side's science, and the round after it ends pays both
/// the smaller sum.
pub(crate) fn process_round(g: &mut Game) {
    let turn = g.turn();
    let n = g.state().diplo().deals.len();
    for i in 0..n {
        // The deals list keeps every deal ever made, so a round reads the active ones in place
        // and copies out only the resource trades still running (an `Ongoing` is `Copy`).
        let trades: Vec<(usize, Ongoing, ResourceId)> = match g.state().diplo().deals.get(i) {
            Some(d) if d.active => d
                .ongoing
                .iter()
                .enumerate()
                .filter(|(_, o)| o.until >= turn)
                .filter_map(|(j, o)| match o.item {
                    DealItem::Resource { resource, .. } => Some((j, *o, resource)),
                    _ => None,
                })
                .collect(),
            _ => continue,
        };
        // Each trade cut is written at once, as Python's was, so the next reads the supply as
        // it then stands.
        for (j, o, resource) in trades {
            if economy::resource_amount(g, o.from, resource) >= 0 {
                continue;
            }
            if let Some(x) =
                g.edit_diplo(DiploTouch::DEALS).deals.get_mut(i).and_then(|d| d.ongoing.get_mut(j))
            {
                x.until = turn - 1;
            }
            let text = format!(
                "A trade of {} between {} and {} was cut short.",
                g.rules().name(resource).unwrap_or_default(),
                name(g, o.from),
                name(g, o.to)
            );
            let audience: PlayerSet = [o.from, o.to].into_iter().collect();
            g.emit(EngineEvent::DealCut, &text, Some(audience), None, EventData::default(), &[]);
        }
        let Some((expired, ends, id, [a, b])) = g.state().diplo().deals.get(i).map(|d| {
            let expired = !d.ongoing.is_empty() && d.ongoing.iter().all(|o| o.until < turn);
            (expired, expired || d.ongoing.is_empty(), d.id, d.parties)
        }) else {
            continue;
        };
        if ends && let Some(x) = g.edit_diplo(DiploTouch::DEALS).deals.get_mut(i) {
            x.active = false;
        }
        if expired {
            let text = format!("A deal between {} and {} has expired.", name(g, a), name(g, b));
            let audience: PlayerSet = [a, b].into_iter().collect();
            let data = EventData { deal: Some(id), ..EventData::default() };
            g.emit(EngineEvent::DealExpired, &text, Some(audience), None, data, &[]);
        }
    }
    let pairs: Vec<(PlayerId, PlayerId, Relation)> =
        g.state().diplo().relations().pairs().map(|(a, b, r)| (a, b, *r)).collect();
    for &(a, b, r) in &pairs {
        // Python deleted the lapsed entry; a turn of 0 is none.
        if r.open_borders_until.iter().any(|&u| u != 0 && u < turn) {
            edit(g, a, b, |x| {
                for u in &mut x.open_borders_until {
                    if *u < turn {
                        *u = 0;
                    }
                }
            });
        }
    }
    for &(a, b, r) in &pairs {
        if r.ra_until >= turn {
            let add = |p| {
                num::trunc_i32(crate::game::derive::stats::civ_stats(g, p).total[Stat::Science])
            };
            let (sa, sb) = (add(a), add(b));
            edit(g, a, b, |x| {
                x.ra_science[side(a, b)] = x.ra_science[side(a, b)].saturating_add(sa);
                x.ra_science[side(b, a)] = x.ra_science[side(b, a)].saturating_add(sb);
            });
        } else if r.ra_until > 0 && r.ra_until == turn - 1 {
            let bonus = r.ra_science[0].min(r.ra_science[1]);
            for p in [a, b] {
                if let Some(x) = g.player_mut(p, PlayerTouch::RESEARCH) {
                    x.tech.ra_bonus = x.tech.ra_bonus.saturating_add(bonus);
                }
            }
            edit(g, a, b, |x| x.ra_science = [0, 0]);
            let text = format!(
                "The research agreement between {} and {} has concluded.",
                name(g, a),
                name(g, b)
            );
            let audience: PlayerSet = [a, b].into_iter().collect();
            g.emit(
                EngineEvent::ResearchAgreement,
                &text,
                Some(audience),
                None,
                EventData::default(),
                &[],
            );
        }
    }
}
