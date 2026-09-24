//! Events, messages, thoughts and stats rows, moved to the chronicle (DESIGN.md 4.7).
//!
//! Python kept four lists in `GameState` (`state.py:352-356`); the engine keeps them in a
//! [`Chronicle`] beside the state, whose heads the state holds: counts, and a running hash of
//! what the engine produced in the order it was appended. Python did not record how the four
//! lists interleaved, so they are appended turn by turn, and within a turn events, then
//! messages, then thoughts, then stats rows (a round's stats are recorded at its end, after its
//! events). Each list keeps its own order.
//!
//! An event keeps its id, which must be its position from 1, as `Game.emit` gave it
//! (`game.py:876`), so the feed engine and host events share goes on from it. Its name
//! references (`game.py:842-858`) move from code points to UTF-8 byte offsets; its `x` and `y`,
//! derived from its tile, are checked and dropped; its data becomes the typed [`EventData`]. The
//! host's own event types (`agent_error`, `game_paused`, `game_resumed`) are host events, counted
//! only in the host's heads; any other type the engine does not emit is an error.

use serde_json::Value;

use super::Cx;
use super::read::{Obj, Path, Res, dict, flag, int, list, text};
use super::world::un_result;
use crate::base::ids::{DealId, EraId, EventId, MessageId, NegotiationId, PlayerId, UnitId};
use crate::base::sets::PlayerSet;
use crate::save::journal::Record;
use crate::state::chronicle::{
    Chronicle, ChronicleHeads, CivStats, EngineEvent, Event, EventData, EventType, HostHeads,
    Message, NameRef, RefKind, StatsRow, Thought,
};
use crate::state::diplo::NegStatus;

/// The event types a host adds, which the engine does not emit (session.py:359-637,
/// llm_agent.py:295-626).
pub const HOST_EVENTS: [&str; 3] = ["agent_error", "game_paused", "game_resumed"];

/// An entry read, waiting to be appended in turn order.
enum Entry {
    Event(Event),
    Message(Message),
    Thought(Thought),
    Stats(StatsRow),
}

/// The chronicle, and the heads it leaves.
pub(super) fn history(
    cx: &mut Cx<'_>,
    top: &Obj<'_>,
) -> Res<(Chronicle, ChronicleHeads, HostHeads)> {
    let width = cx.width;
    let events = top.each("events", |v, p| event(cx, width, v, p))?;
    let messages = top.each("messages", |v, p| message(cx, v, p))?;
    let thoughts = top.each("thoughts", |v, p| thought(cx, v, p))?;
    let stats = top.each("stats", |v, p| stats_row(cx, v, p))?;

    let mut chron = Chronicle::new();
    let mut heads = ChronicleHeads::default();
    let mut host = HostHeads::default();
    let mut rec = Record::new(&mut heads, &mut host, &mut chron);
    let mut lists = [
        events
            .into_iter()
            .map(|(t, e)| (t, Entry::Event(e)))
            .collect::<Vec<_>>()
            .into_iter()
            .peekable(),
        messages
            .into_iter()
            .map(|m| (m.turn, Entry::Message(m)))
            .collect::<Vec<_>>()
            .into_iter()
            .peekable(),
        thoughts
            .into_iter()
            .map(|t| (t.turn, Entry::Thought(t)))
            .collect::<Vec<_>>()
            .into_iter()
            .peekable(),
        stats
            .into_iter()
            .map(|s| (s.turn, Entry::Stats(s)))
            .collect::<Vec<_>>()
            .into_iter()
            .peekable(),
    ];
    let at = top.at("events");
    let mut seen_events = 0usize;
    loop {
        // The list whose next entry has the earliest turn; lists earlier in `lists` first.
        let next = lists
            .iter_mut()
            .enumerate()
            .filter_map(|(i, l)| l.peek().map(|(turn, _)| (*turn, i)))
            .min();
        let Some((_, i)) = next else { break };
        let Some((_, entry)) = lists.get_mut(i).and_then(Iterator::next) else { break };
        let appended = match entry {
            Entry::Event(ev) => {
                let expected = rec.take_event_id();
                if expected != Some(ev.id) {
                    return Err(at.index(seen_events).field("id").err(format!(
                        "event {} is not numbered by its place ({:?})",
                        ev.id,
                        expected.map(EventId::get)
                    )));
                }
                seen_events += 1;
                rec.event(ev)
            }
            Entry::Message(m) => rec.message(m),
            Entry::Thought(t) => {
                rec.thought(t);
                Ok(())
            }
            Entry::Stats(s) => rec.stats(s),
        };
        appended.map_err(|e| top.path().err(format!("the history does not hash: {e}")))?;
    }
    Ok((chron, heads, host))
}

/// One event (`game.py:873-883`), and its turn.
fn event(cx: &mut Cx<'_>, width: u16, v: &Value, p: &Path<'_>) -> Res<(i32, Event)> {
    let o = Obj::new(v, *p)?;
    let id: u32 = o.int_req("id")?;
    let id = EventId::new(id).ok_or_else(|| o.at("id").err("0 is not an event id"))?;
    let turn: i32 = o.int_req("turn")?;
    let ty = o.text_req("type")?;
    let kind = match EngineEvent::from_name(ty) {
        Some(e) => EventType::Engine(e),
        None if HOST_EVENTS.contains(&ty) => EventType::Host(ty.into()),
        None => return Err(o.at("type").err(format!("no event type {ty:?}"))),
    };
    let text_v = o.text_req("text")?;
    let audience = match o.get("players") {
        None | Some(Value::Null) => None,
        Some(v) => Some(cx.player_set(list(v, &o.at("players"))?, &o.at("players"))?),
    };
    let tile = cx.opt_tile(o.get("idx"), &o.at("idx"))?;
    let (x, y) = (o.opt_int::<i64>("x")?, o.opt_int::<i64>("y")?);
    match tile {
        Some(t) => {
            let w = u32::from(width);
            let want = (i64::from(t.0 % w), i64::from(t.0 / w));
            if (x, y) != (Some(want.0), Some(want.1)) {
                return Err(o.at("x").err(format!("({x:?}, {y:?}) is not tile {t} at {want:?}")));
            }
        }
        None if x.is_some() || y.is_some() => {
            return Err(o.at("x").err("coordinates without a tile"));
        }
        None => {}
    }
    let data = match o.get("data") {
        None | Some(Value::Null) => None,
        Some(d) => event_data(cx, d, &o.at("data"))?.map(Box::new),
    };
    let refs = match o.get("refs") {
        None | Some(Value::Null) => Default::default(),
        Some(r) => name_refs(cx, text_v, r, &o.at("refs"))?,
    };
    o.finish()?;
    let ev = Event { id, turn, kind, text: text_v.into(), audience, tile, data, refs };
    Ok((turn, ev))
}

/// Where an event's text names a civilization, leader or city: Python's `[start, end, player,
/// kind]` spans in code points, as UTF-8 byte offsets.
fn name_refs(
    cx: &Cx<'_>,
    body: &str,
    v: &Value,
    p: &Path<'_>,
) -> Res<smallvec::SmallVec<[NameRef; 2]>> {
    // The byte offset of each code point, and of the end.
    let offsets: Option<Vec<u32>> = (!body.is_ascii()).then(|| {
        body.char_indices()
            .map(|(b, _)| u32::try_from(b).unwrap_or(u32::MAX))
            .chain(core::iter::once(u32::try_from(body.len()).unwrap_or(u32::MAX)))
            .collect()
    });
    let chars = offsets.as_ref().map_or(body.len(), |o| o.len() - 1);
    let byte = |cp: usize| -> Option<u32> {
        match &offsets {
            None => u32::try_from(cp).ok().filter(|_| cp <= body.len()),
            Some(o) => o.get(cp).copied(),
        }
    };
    let mut out = smallvec::SmallVec::new();
    for (i, r) in list(v, p)?.iter().enumerate() {
        let at = p.index(i);
        let [start, end, player, code] = list(r, &at)? else {
            return Err(at.err("a name reference is [start, end, player, kind]"));
        };
        let (s, e): (usize, usize) = (int(start, &at.index(0))?, int(end, &at.index(1))?);
        let (Some(sb), Some(eb)) = (byte(s), byte(e)) else {
            return Err(at.err(format!("{s}..{e} is past the text's {chars} characters")));
        };
        if s > e {
            return Err(at.err(format!("{s}..{e} runs backwards")));
        }
        let code = text(code, &at.index(3))?;
        let kind = code
            .chars()
            .next()
            .filter(|_| code.chars().count() == 1)
            .and_then(RefKind::from_code)
            .ok_or_else(|| at.index(3).err(format!("no name kind {code:?}")))?;
        out.push(NameRef { start: sb, end: eb, player: cx.player(player, &at.index(2))?, kind });
    }
    Ok(out)
}

/// What an event says in fields (`game.py:860`, the keywords `emit` was passed), typed; `None`
/// if it says nothing.
fn event_data(cx: &mut Cx<'_>, v: &Value, p: &Path<'_>) -> Res<Option<EventData>> {
    let o = Obj::new(v, *p)?;
    let pl = |cx: &Cx<'_>, key: &str| cx.opt_player(o.get(key), &o.at(key));
    let mut d = EventData {
        player: pl(cx, "player")?,
        a: pl(cx, "a")?,
        b: pl(cx, "b")?,
        attacker: pl(cx, "attacker")?,
        defender: pl(cx, "defender")?,
        awaiting: pl(cx, "awaiting")?,
        killer: pl(cx, "killer")?,
        owner: pl(cx, "owner")?,
        old_owner: pl(cx, "old_owner")?,
        new_owner: pl(cx, "new_owner")?,
        sender: pl(cx, "sender")?,
        winner: pl(cx, "winner")?,
        ..EventData::default()
    };
    d.unit = match o.opt_int::<u32>("unit")? {
        None => None,
        Some(u) => Some(UnitId::new(u).ok_or_else(|| o.at("unit").err("0 is not a unit id"))?),
    };
    d.city = cx.opt_city(o.get("city"), &o.at("city"))?;
    d.deal = match o.opt_int::<u32>("deal")? {
        None => None,
        Some(x) => Some(DealId::new(x).ok_or_else(|| o.at("deal").err("0 is not a deal id"))?),
    };
    d.negotiation = match o.opt_int::<u32>("negotiation")? {
        None => None,
        Some(x) => Some(
            NegotiationId::new(x)
                .ok_or_else(|| o.at("negotiation").err("0 is not a negotiation id"))?,
        ),
    };
    d.message = match o.opt_int::<u32>("message")? {
        None => None,
        Some(x) => {
            Some(MessageId::new(x).ok_or_else(|| o.at("message").err("0 is not a message id"))?)
        }
    };
    d.item = match o.opt_text("item")? {
        None => None,
        Some(name) => Some(cx.constructible(name, &o.at("item"))?),
    };
    d.unit_type = cx.opt_named(o.get("unit_type"), &o.at("unit_type"))?;
    d.building = cx.opt_named(o.get("building"), &o.at("building"))?;
    d.improvement = cx.opt_named(o.get("improvement"), &o.at("improvement"))?;
    d.tech = cx.opt_named(o.get("tech"), &o.at("tech"))?;
    d.policy = cx.opt_named(o.get("policy"), &o.at("policy"))?;
    d.belief = cx.opt_named(o.get("belief"), &o.at("belief"))?;
    d.era = match o.opt_int::<u8>("era")? {
        None => None,
        Some(e) if usize::from(e) < cx.r.eras().len() => Some(EraId(e)),
        Some(e) => return Err(o.at("era").err(format!("era {e} is not in the ruleset"))),
    };
    d.religion = cx.opt_religion(o.get("religion"), &o.at("religion"))?;
    d.reward = cx.opt_named(o.get("reward"), &o.at("reward"))?;
    d.victory = cx.opt_named(o.get("victory"), &o.at("victory"))?;
    d.status = match o.opt_text("status")? {
        None => None,
        Some(s) => Some(
            NegStatus::from_name(s)
                .ok_or_else(|| o.at("status").err(format!("no negotiation status {s:?}")))?,
        ),
    };
    d.gold = o.opt_int("gold")?;
    d.citizen_killed = match o.get("citizen_killed") {
        None | Some(Value::Null) => None,
        Some(v) => Some(flag(v, &o.at("citizen_killed"))?),
    };
    d.results = match o.get("results") {
        None | Some(Value::Null) => None,
        Some(r) => Some(Box::new(un_result(cx, r, &o.at("results"))?)),
    };
    o.finish()?;
    Ok((d != EventData::default()).then_some(d))
}

/// A message between civilizations (`diplomacy.py:306-311`).
fn message(cx: &mut Cx<'_>, v: &Value, p: &Path<'_>) -> Res<Message> {
    let o = Obj::new(v, *p)?;
    let id: u32 = o.int_req("id")?;
    let to = match o.get("to") {
        None => PlayerSet::EMPTY,
        Some(v) => cx.player_set(list(v, &o.at("to"))?, &o.at("to"))?,
    };
    let m = Message {
        id: MessageId::new(id).ok_or_else(|| o.at("id").err("0 is not a message id"))?,
        turn: o.int_req("turn")?,
        from: cx.player(o.req("from")?, &o.at("from"))?,
        to,
        text: o.text_req("text")?.into(),
    };
    o.finish()?;
    Ok(m)
}

/// A seat's recorded reasoning or note (`engine_api.py:642-645`).
fn thought(cx: &Cx<'_>, v: &Value, p: &Path<'_>) -> Res<Thought> {
    let o = Obj::new(v, *p)?;
    let t = Thought {
        turn: o.int_req("turn")?,
        player: cx.player(o.req("player")?, &o.at("player"))?,
        text: o.text_req("text")?.into(),
        kind: o.opt_text("kind")?.map(Into::into),
    };
    o.finish()?;
    Ok(t)
}

/// One round's statistics (`victory.py:423-455`): a row per major, by id.
fn stats_row(cx: &Cx<'_>, v: &Value, p: &Path<'_>) -> Res<StatsRow> {
    let o = Obj::new(v, *p)?;
    let turn = o.int_req("turn")?;
    let at = o.at("players");
    let mut civs = dict(o.req("players")?, &at, |k, x, pp| {
        let player = cx.player_key(k, pp)?;
        civ_stats(cx, player, x, pp)
    })?;
    civs.sort_by_key(|c| c.player);
    if civs.windows(2).any(|w| w[0].player == w[1].player) {
        return Err(at.err("a player is listed twice"));
    }
    o.finish()?;
    Ok(StatsRow { turn, civs })
}

/// One civilization's row; an eliminated one's is `{"alive": false, "score": 0}`.
fn civ_stats(cx: &Cx<'_>, player: PlayerId, v: &Value, p: &Path<'_>) -> Res<CivStats> {
    let o = Obj::new(v, *p)?;
    let era: u8 = o.int("era", 0)?;
    if usize::from(era) >= cx.r.eras().len() {
        return Err(o.at("era").err(format!("era {era} is not in the ruleset")));
    }
    let row = CivStats {
        player,
        alive: o.flag("alive", true)?,
        score: o.int("score", 0)?,
        cities: o.int("cities", 0)?,
        population: o.int("population", 0)?,
        land: o.int("land", 0)?,
        techs: o.int("techs", 0)?,
        policies: o.int("policies", 0)?,
        military: o.int("military", 0)?,
        gold: o.int("gold", 0)?,
        gold_per_turn: o.real("gold_per_turn", 0.0)?,
        science: o.real("science", 0.0)?,
        culture: o.real("culture", 0.0)?,
        faith: o.real("faith", 0.0)?,
        production: o.real("production", 0.0)?,
        happiness: o.int("happiness", 0)?,
        era: EraId(era),
        units: o.int("units", 0)?,
        golden_age: o.flag("golden_age", false)?,
    };
    o.finish()?;
    Ok(row)
}
