//! Religions, world wonders, the United Nations, barbarian camps, the clock and the id counters
//! (`state.py:335-364`, `religion.py:473-477`, `victory.py:97-104, 196-220`,
//! `barbarians.py:190`).
//!
//! Religions were keyed by name in founding order; they become [`ReligionId`]s, their positions.
//! A pantheon is named by its belief, a religion by its row of `religions.json`. The UN tally was
//! keyed by civilization name, which a rename breaks; it is keyed by player id now, so each name
//! must still name exactly one player.
//!
//! [`ReligionId`]: crate::base::ids::ReligionId

use std::collections::BTreeMap;

use serde_json::Value;

use super::Cx;
use super::read::{Obj, Path, Res, dict, int, key_int, list};
use crate::base::ids::{BeliefId, BuildingId, CampId, CityId, PlayerId};
use crate::base::sets::{BeliefSet, PlayerSet};
use crate::rules::defs::BeliefType;
use crate::state::diplo::Diplomacy;
use crate::state::world::{Camp, Religion, ReligionName, Un, UnResult, World};
use crate::state::{IdCounters, Phase, TurnClock};

/// The founded religions and pantheons, in founding order; their names go to the context, so
/// that everything naming one can find it.
pub(super) fn religions<'a>(cx: &mut Cx<'a>, top: &Obj<'a>) -> Res<Vec<Religion>> {
    let at = top.at("religions");
    let v = top.req("religions")?;
    let mut names = Vec::new();
    let out = dict(v, &at, |k, x, p| {
        names.push(k);
        religion(cx, k, x, p)
    })?;
    if out.len() > 256 {
        return Err(at.err(format!("{} religions; at most 256 are numbered", out.len())));
    }
    cx.religions = names;
    Ok(out)
}

/// One religion or pantheon, keyed by `key`.
fn religion(cx: &Cx<'_>, key: &str, v: &Value, p: &Path<'_>) -> Res<Religion> {
    let o = Obj::new(v, *p)?;
    let name = o.text("name", key)?;
    if name != key {
        return Err(o.at("name").err(format!("religion {name:?} is filed under {key:?}")));
    }
    let r = cx.r;
    let rname = match r.religions().iter().find(|(_, n)| ***n == *key) {
        Some((id, _)) => ReligionName::Religion(id),
        None => {
            let b: BeliefId = cx.id_of(key, &o.at("name"))?;
            if r.beliefs().get(b).map(|d| d.kind) != Some(BeliefType::Pantheon) {
                return Err(o
                    .at("name")
                    .err(format!("{key:?} is neither a religion nor a pantheon")));
            }
            ReligionName::Pantheon(b)
        }
    };
    let beliefs = |field: &str, kinds: [BeliefType; 2]| -> Res<BeliefSet> {
        let at = o.at(field);
        let mut set = BeliefSet::new();
        for (i, b) in o.each(field, |v, p| cx.named::<BeliefId>(v, p))?.into_iter().enumerate() {
            let kind = r.beliefs().get(b).map(|d| d.kind);
            if !kind.is_some_and(|k| kinds.contains(&k)) {
                return Err(at.index(i).err("a belief of another type"));
            }
            if !set.insert(b) {
                return Err(at.index(i).err("a belief listed twice"));
            }
        }
        Ok(set)
    };
    let religion = Religion {
        name: rname,
        display: o.text("display", key)?.into(),
        founder: cx.player(o.req("founder")?, &o.at("founder"))?,
        founder_beliefs: beliefs("founder_beliefs", [BeliefType::Founder, BeliefType::Enhancer])?,
        follower_beliefs: beliefs(
            "follower_beliefs",
            [BeliefType::Pantheon, BeliefType::Follower],
        )?,
        blocked_holy: o.flag("blocked_holy", false)?,
    };
    o.finish()?;
    Ok(religion)
}

/// Wonders, the UN and camps, beside the religions already read.
pub(super) fn world(cx: &mut Cx<'_>, top: &Obj<'_>, religions: Vec<Religion>) -> Res<World> {
    let wonders_built: BTreeMap<BuildingId, CityId> = top
        .entries("wonders_built", |k, v, p| Ok((cx.id_of(k, p)?, cx.city(v, p)?)))?
        .into_iter()
        .collect();
    let un = un(cx, top.req("un")?, &top.at("un"))?;
    let camps = top
        .entries("camps", |k, v, p| {
            let id: u32 = key_int(k, p)?;
            let id = CampId::new(id).ok_or_else(|| p.err("0 is not a camp id"))?;
            let o = Obj::new(v, *p)?;
            let camp = Camp {
                tile: cx.tile(o.req("idx")?, &o.at("idx"))?,
                countdown: o.int("countdown", 0)?,
                spawned: o.int("spawned", -1)?,
                destroyed: o.flag("destroyed", false)?,
            };
            o.finish()?;
            Ok((id, camp))
        })?
        .into_iter()
        .collect();
    Ok(World { religions, wonders_built, un, camps })
}

/// The United Nations (`victory.py:97-104`): `{}` until it was first read.
fn un(cx: &mut Cx<'_>, v: &Value, p: &Path<'_>) -> Res<Un> {
    let o = Obj::new(v, *p)?;
    let votes = o
        .entries("votes", |k, x, pp| Ok((cx.player_key(k, pp)?, cx.opt_player(Some(x), pp)?)))?
        .into_iter()
        .collect();
    let results = match o.get("results") {
        None | Some(Value::Null) => None,
        Some(r) => Some(un_result(cx, r, &o.at("results"))?),
    };
    let won = match o.get("won") {
        None => PlayerSet::EMPTY,
        Some(w) => cx.player_set(list(w, &o.at("won"))?, &o.at("won"))?,
    };
    let un = Un {
        next_vote: o.opt_int("next_vote")?,
        votes,
        results,
        won,
        processed_turn: o.opt_int("processed_turn")?,
    };
    o.finish()?;
    Ok(un)
}

/// A world leader vote's result (`victory.py:216-217`): the tally, most votes first, by
/// civilization name, which must name exactly one player.
pub(super) fn un_result(cx: &Cx<'_>, v: &Value, p: &Path<'_>) -> Res<UnResult> {
    let o = Obj::new(v, *p)?;
    let tally = o.entries("tally", |name, n, pp| {
        let mut named = cx.player_names.iter().enumerate().filter(|(_, x)| **x == name);
        let who = match (named.next(), named.next()) {
            (Some((i, _)), None) => PlayerId(u8::try_from(i).unwrap_or(u8::MAX)),
            (None, _) => return Err(pp.err("no player is called this now")),
            (Some(_), Some(_)) => return Err(pp.err("more than one player is called this")),
        };
        Ok((who, int::<u16>(n, pp)?))
    })?;
    let result = UnResult {
        turn: o.int_req("turn")?,
        tally,
        votes_needed: o.int_req("votes_needed")?,
        winner: cx.opt_player(o.get("winner"), &o.at("winner"))?,
    };
    o.finish()?;
    Ok(result)
}

/// Where the game is in time.
pub(super) fn clock(cx: &Cx<'_>, top: &Obj<'_>) -> Res<TurnClock> {
    let phase = match top.text("phase", "playing")? {
        "playing" => Phase::Playing,
        "over" => Phase::Over,
        other => return Err(top.at("phase").err(format!("no phase {other:?}"))),
    };
    Ok(TurnClock {
        turn: top.int_req("turn")?,
        current: cx.player(top.req("current")?, &top.at("current"))?,
        turn_started: top.flag("turn_started", false)?,
        phase,
        winner: cx.opt_player(top.get("winner"), &top.at("winner"))?,
        victory: cx.opt_named(top.get("victory"), &top.at("victory"))?,
    })
}

/// The id counters: units, cities and camps from Python's shared `next_id`, deals and
/// negotiations past their lists (`diplomacy.py:540, 781`).
pub(super) fn ids(top: &Obj<'_>, d: &Diplomacy) -> Res<IdCounters> {
    let next: u32 = top.int_req("next_id")?;
    if next == 0 {
        return Err(top.at("next_id").err("ids start at 1"));
    }
    let mut ids = IdCounters::starting_at(next);
    // Python numbered them len + 1; past the largest in use either way.
    let past = |len: usize, max: Option<u32>| {
        u32::try_from(len).unwrap_or(u32::MAX).max(max.unwrap_or(0)).saturating_add(1)
    };
    ids.deal = past(d.deals.len(), d.deals.iter().map(|x| x.id.get()).max());
    ids.negotiation = past(d.negotiations.len(), d.negotiations.iter().map(|x| x.id.get()).max());
    Ok(ids)
}
