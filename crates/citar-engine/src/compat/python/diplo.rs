//! Relations, open borders, opinions, deals and negotiations (`diplomacy.py:55-96, 530-609,
//! 695-790`; `state.py:348-351`).
//!
//! `relations["a,b"]` and `open_borders["x>y"]` fold into one [`Relation`] per pair, two-sided
//! fields indexed by [`side`]; whether a pair has met comes from the players' `met` lists, which
//! Python kept symmetric. Opinions leave the relation for the [`OpinionBook`], keyed by their
//! holder: a scenario's `"a>b"` entry (`scenario.py:429`), which Python never read, now counts as
//! `a`'s opinion of `b`. Deal items and terms read with `DealItem::from_json`, which is strict.

use serde_json::{Map, Value};

use super::players::PlayerOut;
use super::read::{Obj, Path, Res, dict, flag, int, real};
use super::{Cx, Drop};
use crate::base::ids::{DealId, NegotiationId, PlayerId, Turn};
use crate::base::sets::PlayerSet;
use crate::state::diplo::{
    Deal, DealItem, DealItemKind, Diplomacy, NegAction, NegEntry, NegStatus, Negotiation, Ongoing,
    OpinionKey, PairMatrix, Relation, Terms, side,
};

/// The diplomacy of the state's players.
pub(super) fn diplomacy(cx: &mut Cx<'_>, top: &Obj<'_>, players: &[PlayerOut]) -> Res<Diplomacy> {
    let n = u8::try_from(players.len()).unwrap_or(u8::MAX);
    let mut relations = PairMatrix::<Relation>::new(n);
    // Contact, from the players' lists.
    for (a, pa) in players.iter().enumerate() {
        let a = PlayerId(u8::try_from(a).unwrap_or(u8::MAX));
        for b in pa.met.iter() {
            let back = players.get(usize::from(b.0)).is_some_and(|pb| pb.met.contains(a));
            if !back {
                let at = top.at("players");
                return Err(at
                    .index(usize::from(a.0))
                    .field("met")
                    .err(format!("player {a} has met {b}, but not {b} {a}")));
            }
            if let Some(r) = relations.get_mut(a, b) {
                r.met = true;
            }
        }
    }
    let mut opinions: Vec<((PlayerId, PlayerId), [f64; OpinionKey::COUNT])> = Vec::new();
    let at = top.at("relations");
    dict(top.req("relations")?, &at, |k, v, p| {
        let (a, b) = pair_key(cx, k, ',', p)?;
        if a >= b {
            return Err(p.err("a relation is keyed by its lower player first"));
        }
        let o = Obj::new(v, *p)?;
        let r = relations.get_mut(a, b).ok_or_else(|| p.err("not a pair of players"))?;
        r.war = o.flag("war", false)?;
        r.war_declared_by = cx.opt_player(o.get("war_declared_by"), &o.at("war_declared_by"))?;
        r.since = o.int("since", 0)?;
        r.treaty_until = o.int("treaty_until", 0)?;
        r.friendship_until = o.int("friendship_until", 0)?;
        r.pact_until = o.int("pact_until", 0)?;
        r.ra_until = o.int("ra_until", 0)?;
        for (who, amount) in o.entries("ra_science", |k, x, pp| {
            Ok((one_of(cx.player_key(k, pp)?, a, b, pp)?, int::<i32>(x, pp)?))
        })? {
            r.ra_science[side(who, other(who, a, b))] = amount;
        }
        for (holder, host, on) in o.entries("embassy", |k, x, pp| {
            let (h, g) = pair_key(cx, k, '>', pp)?;
            one_of(h, a, b, pp)?;
            if one_of(g, a, b, pp)? == h {
                return Err(pp.err("an embassy with oneself"));
            }
            Ok((h, g, flag(x, pp)?))
        })? {
            r.embassy[side(holder, host)] = on;
        }
        for (who, until) in o.entries("denounced_by", |k, x, pp| {
            Ok((one_of(cx.player_key(k, pp)?, a, b, pp)?, int::<Turn>(x, pp)?))
        })? {
            r.denounced_until[side(who, other(who, a, b))] = until;
        }
        for (holder, about, values) in o.entries("opinion", |k, x, pp| {
            let (holder, about) = if k.contains('>') {
                // A scenario's opinion, written under "a>b" and read by nobody (scenario.py:429).
                pair_key(cx, k, '>', pp)?
            } else {
                let h = one_of(cx.player_key(k, pp)?, a, b, pp)?;
                (h, other(h, a, b))
            };
            if holder == about {
                return Err(pp.err("an opinion of oneself"));
            }
            Ok((holder, about, opinion_values(x, pp)?))
        })? {
            match opinions.iter_mut().find(|(k, _)| *k == (holder, about)) {
                Some((_, old)) => {
                    for (slot, x) in old.iter_mut().zip(values) {
                        if x.to_bits() != 0 {
                            *slot = x;
                        }
                    }
                }
                None => opinions.push(((holder, about), values)),
            }
        }
        o.finish()
    })?;
    let at = top.at("open_borders");
    dict(top.req("open_borders")?, &at, |k, v, p| {
        let (x, y) = pair_key(cx, k, '>', p)?;
        if x == y {
            return Err(p.err("open borders with oneself"));
        }
        let until: Turn = int(v, p)?;
        let r = relations.get_mut(x, y).ok_or_else(|| p.err("not a pair of players"))?;
        r.open_borders_until[side(x, y)] = until;
        Ok(())
    })?;
    let barbarians: PlayerSet =
        players.iter().filter(|p| p.player.is_barbarian()).map(|p| p.player.id()).collect();
    let mut d = Diplomacy::from_relations(relations, barbarians);
    for ((holder, about), values) in opinions {
        d.opinions.insert(holder, about, values);
    }
    d.deals = top.each("deals", |v, p| deal(cx, v, p))?;
    d.negotiations = top.each("negotiations", |v, p| negotiation(cx, v, p))?;
    Ok(d)
}

/// The players of a key `"a,b"` or `"a>b"`.
fn pair_key(cx: &Cx<'_>, k: &str, sep: char, p: &Path<'_>) -> Res<(PlayerId, PlayerId)> {
    let (a, b) = k.split_once(sep).ok_or_else(|| p.err(format!("expected \"a{sep}b\"")))?;
    Ok((cx.player_key(a, p)?, cx.player_key(b, p)?))
}

/// `who`, which must be `a` or `b`.
fn one_of(who: PlayerId, a: PlayerId, b: PlayerId, p: &Path<'_>) -> Res<PlayerId> {
    if who == a || who == b {
        Ok(who)
    } else {
        Err(p.err(format!("player {who} is not of the pair {a}, {b}")))
    }
}

/// The other player of the pair `a`, `b`.
const fn other(who: PlayerId, a: PlayerId, b: PlayerId) -> PlayerId {
    if who.0 == a.0 { b } else { a }
}

/// One holder's opinions, by reason (`diplomacy.py:77-82`).
fn opinion_values(v: &Value, p: &Path<'_>) -> Res<[f64; OpinionKey::COUNT]> {
    let mut out = [0.0; OpinionKey::COUNT];
    dict(v, p, |k, x, pp| {
        let key =
            OpinionKey::from_name(k).ok_or_else(|| pp.err("no opinion reason of this name"))?;
        out[key as usize] = real(x, pp)?;
        Ok(())
    })?;
    Ok(out)
}

/// Terms as Python kept them, `{giver: [item]}`, read strictly.
fn terms(cx: &Cx<'_>, v: &Value, p: &Path<'_>) -> Res<Terms> {
    let t = Terms::from_json(v, cx.r).map_err(|e| p.err(e.0))?;
    for s in &t.sides {
        if usize::from(s.giver.0) >= cx.n {
            return Err(p.err(format!("player {} is not one of the players", s.giver)));
        }
    }
    Ok(t)
}

/// A concluded deal (`diplomacy.py:530-609`).
fn deal(cx: &Cx<'_>, v: &Value, p: &Path<'_>) -> Res<Deal> {
    let o = Obj::new(v, *p)?;
    let id: u32 = o.int_req("id")?;
    let id = DealId::new(id).ok_or_else(|| o.at("id").err("0 is not a deal id"))?;
    let parties = o.each("parties", |v, p| cx.player(v, p))?;
    let &[a, b] = parties.as_slice() else {
        return Err(o.at("parties").err("a deal has two parties"));
    };
    let terms = terms(cx, o.req("terms")?, &o.at("terms"))?;
    let ongoing = o.each("ongoing", |v, p| ongoing(cx, v, p))?;
    let deal = Deal {
        id,
        turn: o.int_req("turn")?,
        parties: [a, b],
        terms,
        ongoing,
        active: o.flag("active", true)?,
        summary: o.text("summary", "")?.into(),
    };
    o.finish()?;
    Ok(deal)
}

/// A recurring item: the item's own keys, and who pays whom until when
/// (`diplomacy.py:554-557`).
fn ongoing(cx: &Cx<'_>, v: &Value, p: &Path<'_>) -> Res<Ongoing> {
    let o = Obj::new(v, *p)?;
    let from = cx.player(o.req("from")?, &o.at("from"))?;
    let to = cx.player(o.req("to")?, &o.at("to"))?;
    let until = o.int_req("until")?;
    let mut item = Map::new();
    for (k, x) in o.rest() {
        item.insert(k.to_owned(), x.clone());
    }
    let item = DealItem::from_json(&Value::Object(item), cx.r).map_err(|e| p.err(e.0))?;
    if !matches!(item.kind(), DealItemKind::GoldPerTurn | DealItemKind::Resource) {
        return Err(p.err(format!("a {} item does not recur", item.kind().name())));
    }
    Ok(Ongoing { item, from, to, until })
}

/// A negotiation (`diplomacy.py:695-790`). `exchanges` is its history's length, which only the
/// archived bots read; one that is not is counted as dropped.
fn negotiation(cx: &mut Cx<'_>, v: &Value, p: &Path<'_>) -> Res<Negotiation> {
    let o = Obj::new(v, *p)?;
    let id: u32 = o.int_req("id")?;
    let id = NegotiationId::new(id).ok_or_else(|| o.at("id").err("0 is not a negotiation id"))?;
    let status = o.text("status", "open")?;
    let proposal = match o.get("proposal") {
        None | Some(Value::Null) => None,
        Some(v) => Some(terms(cx, v, &o.at("proposal"))?),
    };
    let history = o.each("history", |v, p| entry(cx, v, p))?;
    let exchanges: usize = o.int("exchanges", history.len())?;
    if exchanges != history.len() {
        cx.report.note(Drop::Exchanges);
    }
    let deal = match o.opt_int::<u32>("deal_id")? {
        None => None,
        Some(d) => Some(DealId::new(d).ok_or_else(|| o.at("deal_id").err("0 is not a deal id"))?),
    };
    let neg = Negotiation {
        id,
        initiator: cx.player(o.req("initiator")?, &o.at("initiator"))?,
        responder: cx.player(o.req("responder")?, &o.at("responder"))?,
        turn: o.int_req("turn")?,
        status: NegStatus::from_name(status)
            .ok_or_else(|| o.at("status").err(format!("no negotiation status {status:?}")))?,
        awaiting: cx.opt_player(o.get("awaiting"), &o.at("awaiting"))?,
        proposal,
        proposal_by: cx.opt_player(o.get("proposal_by"), &o.at("proposal_by"))?,
        history,
        deal,
    };
    o.finish()?;
    Ok(neg)
}

/// One entry of a negotiation's history (`diplomacy.py:698-728`).
fn entry(cx: &Cx<'_>, v: &Value, p: &Path<'_>) -> Res<NegEntry> {
    let o = Obj::new(v, *p)?;
    let action = o.text_req("action")?;
    let e = NegEntry {
        seq: o.int_req("seq")?,
        by: cx.opt_player(o.get("by"), &o.at("by"))?,
        action: NegAction::from_name(action)
            .ok_or_else(|| o.at("action").err(format!("no negotiation action {action:?}")))?,
        message: o.text_req("message")?.into(),
        proposal: match o.get("proposal") {
            None | Some(Value::Null) => None,
            Some(v) => Some(terms(cx, v, &o.at("proposal"))?),
        },
        turn: o.int_req("turn")?,
        note: o.opt_text("note")?.map(Into::into),
    };
    o.finish()?;
    Ok(e)
}
