//! Events: emitting them, the names they mention, and what each viewer may see of them
//! (DESIGN.md 4.7, 8.4).
//!
//! Replaces `Game.emit`, `_name_index`, `_event_refs`, `events_for`, `_known_to`, `event_view`
//! and `_scrub_event` (`game.py:806-990`), and the listeners of `game.py:139` (every call returns
//! the events it appended as an [`EventBatch`] instead).
//!
//! **Emitting** ([`Game::emit`]) does five things, in Python's order:
//! 1. the possessive fix, "Aztecs's" to "Aztecs'" (`base::text::possessive_s`);
//! 2. a non-public event anchored on a tile reaches every major that sees the tile too, unless
//!    its type is private (`EngineEvent::is_private`, Python's `PRIVATE_EVENTS`): rivals are
//!    never told what a city built or who was born there;
//! 3. where its text names a civilization, a leader or a city, as `NameRef`s: through the name
//!    index ([`NameIndex`], an Aho-Corasick automaton rebuilt only when a name changes), then the
//!    `mentions` the index no longer knows, such as a razed city or a civilization's old name,
//!    where they overlap no reference found already;
//! 4. appending it to the chronicle;
//! 5. folding it into the engine's running hash, which covers everything but its id
//!    (`save::journal::Record`).
//!
//! **Scrubbing** ([`Game::event_view`]) shows an event to a player as Python's `_scrub_event`
//! did: civilizations, city-states, leaders and cities of players it has not met become
//! "Unknown Civilization", "Unknown City-State", "an unknown leader" and "an unknown city",
//! with Python's capitalisation and possessives; coordinates become "an unknown location"; the
//! tile and the name references are dropped, and the player fields it does not know are
//! cleared. The UN tally is keyed by player id here (DESIGN.md 4.6), so its unknown candidates
//! are listed in [`EventOut::unknown`] for a view to name, and an unknown winner is cleared.

use std::borrow::Cow;
use std::collections::BTreeMap;

use smallvec::SmallVec;

use super::Game;
use super::derive::rev::BitEq;
use crate::base::ids::{EventId, PlayerId, TileIdx};
use crate::base::sets::PlayerSet;
use crate::base::text::{NameScanner, find_word, is_space, possessive_s, scrub_coords};
use crate::save::journal::Record;
use crate::state::State;
use crate::state::chronicle::{EngineEvent, Event, EventData, EventType, NameRef, RefKind};

/// What an unmet civilization is called in a scrubbed event (`game.py:21`).
pub const UNKNOWN_CIV: &str = "Unknown Civilization";

/// What an unmet city-state is called in a scrubbed event (`game.py:22`).
pub const UNKNOWN_CS: &str = "Unknown City-State";

/// A name an event mentions that the name index may no longer know (`emit`'s `mentions`,
/// `game.py:853-858`): a civilization's old name, a destroyed city.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mention<'a> {
    pub name: &'a str,
    pub player: PlayerId,
    pub kind: RefKind,
}

impl<'a> Mention<'a> {
    /// A civilization's name.
    #[must_use]
    pub const fn civ(name: &'a str, player: PlayerId) -> Self {
        Self { name, player, kind: RefKind::Civ }
    }

    /// A city's name.
    #[must_use]
    pub const fn city(name: &'a str, player: PlayerId) -> Self {
        Self { name, player, kind: RefKind::City }
    }
}

/// Every civilization, city-state, leader and city name, and what each refers to
/// (`Game._name_index`, `game.py:814-840`). A derived value, rebuilt only when a name or a
/// city's owner changes.
#[derive(Clone, Debug, Default)]
pub struct NameIndex {
    /// Each name once, with the player and kind it refers to.
    entries: Vec<(Box<str>, PlayerId, RefKind)>,
    scanner: Option<NameScanner>,
}

impl NameIndex {
    /// The index of a state's names. Where two things share a name, a civilization wins over a
    /// leader, a leader over a city, and a later player over an earlier one, as Python's
    /// dictionary did; the barbarians' names, and leader names shorter than three characters,
    /// are left out.
    #[must_use]
    pub fn build(st: &State) -> Self {
        let mut lookup: BTreeMap<Box<str>, (PlayerId, RefKind)> = BTreeMap::new();
        let barbarian =
            |p: PlayerId| st.player(p).is_some_and(crate::state::players::Player::is_barbarian);
        for c in st.cities().iter() {
            if !c.name.is_empty() && !barbarian(c.owner()) {
                lookup.insert(c.name.clone(), (c.owner(), RefKind::City));
            }
        }
        for (id, p) in st.players().iter() {
            if p.is_barbarian() {
                continue;
            }
            if p.leader.chars().count() >= 3 {
                lookup.insert(p.leader.clone(), (id, RefKind::Leader));
            }
            if !p.name.is_empty() {
                lookup.insert(p.name.clone(), (id, RefKind::Civ));
            }
        }
        let entries: Vec<(Box<str>, PlayerId, RefKind)> =
            lookup.into_iter().map(|(n, (p, k))| (n, p, k)).collect();
        // Building fails only for patterns beyond the automaton's size limits, which names never
        // reach; without a scanner the text simply names nobody.
        let scanner = NameScanner::new(entries.iter().map(|(n, _, _)| &**n)).ok();
        Self { entries, scanner }
    }

    /// Each name with what it refers to, in name order.
    #[must_use]
    pub fn entries(&self) -> &[(Box<str>, PlayerId, RefKind)] {
        &self.entries
    }

    /// Where `text` names a civilization, leader or city (`Game._event_refs`,
    /// `game.py:842-858`): the index's matches, then each mention's where it overlaps none found
    /// before, sorted.
    #[must_use]
    pub fn refs(&self, text: &str, mentions: &[Mention<'_>]) -> SmallVec<[NameRef; 2]> {
        let mut refs: SmallVec<[NameRef; 2]> = SmallVec::new();
        let at = |start: usize, end: usize, player, kind| {
            Some(NameRef {
                start: u32::try_from(start).ok()?,
                end: u32::try_from(end).ok()?,
                player,
                kind,
            })
        };
        if let Some(s) = &self.scanner {
            for m in s.find_all(text) {
                if let Some(&(_, p, k)) = self.entries.get(m.name) {
                    refs.extend(at(m.start, m.end, p, k));
                }
            }
        }
        for m in mentions {
            for r in find_word(text, m.name) {
                let overlaps =
                    refs.iter().any(|x| (x.start as usize) < r.end && r.start < x.end as usize);
                if !overlaps {
                    refs.extend(at(r.start, r.end, m.player, m.kind));
                }
            }
        }
        refs.sort();
        refs
    }
}

impl BitEq for NameIndex {
    /// The automaton follows from the names.
    fn bit_eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

/// The events one call appended, oldest first (DESIGN.md 8.1): what Python's listeners were told.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventBatch {
    events: Vec<Event>,
}

impl EventBatch {
    /// A batch of these events.
    #[must_use]
    pub const fn new(events: Vec<Event>) -> Self {
        Self { events }
    }

    /// The events.
    #[must_use]
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// Whether the call appended none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// How many the call appended.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// The events, owned.
    #[must_use]
    pub fn into_events(self) -> Vec<Event> {
        self.events
    }

    /// Adds a later call's events.
    pub fn extend(&mut self, later: Self) {
        self.events.extend(later.events);
    }
}

/// An event as one viewer may see it (`Game.event_view`, `game.py:915-990`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventOut<'a> {
    /// The event, scrubbed if it named anyone the viewer does not know.
    pub event: Cow<'a, Event>,
    /// The players the event names that the viewer does not know: a view names them as
    /// unknown, the UN tally's candidates among them.
    pub unknown: PlayerSet,
}

impl Game {
    /// Emits an engine event (`Game.emit`, `game.py:860-878`): `audience` `None` is public.
    /// Returns its id; `None` only if the event ids are spent.
    pub(crate) fn emit(
        &mut self,
        kind: EngineEvent,
        text: &str,
        audience: Option<PlayerSet>,
        tile: Option<TileIdx>,
        data: EventData,
        mentions: &[Mention<'_>],
    ) -> Option<EventId> {
        let audience = self.widen(audience, tile, kind.is_private());
        self.record(EventType::Engine(kind), text, audience, tile, data, mentions)
    }

    /// Records a host's event: counted in the host heads, never digested.
    pub(crate) fn emit_host_event(
        &mut self,
        kind: &str,
        text: &str,
        audience: Option<PlayerSet>,
        data: EventData,
    ) -> Option<EventId> {
        self.record(EventType::Host(kind.into()), text, audience, None, data, &[])
    }

    /// The audience of a non-public event anchored on a tile, with every major that sees the
    /// tile, unless the event is private (`game.py:871-875`).
    fn widen(
        &self,
        audience: Option<PlayerSet>,
        tile: Option<TileIdx>,
        private: bool,
    ) -> Option<PlayerSet> {
        let mut set = audience?;
        if let Some(t) = tile
            && !private
        {
            for (p, pl) in self.st.players().iter() {
                if pl.is_major() && !set.contains(p) && self.dv.vis.sees(p, t) {
                    set.insert(p);
                }
            }
        }
        Some(set)
    }

    fn record(
        &mut self,
        kind: EventType,
        text: &str,
        audience: Option<PlayerSet>,
        tile: Option<TileIdx>,
        data: EventData,
        mentions: &[Mention<'_>],
    ) -> Option<EventId> {
        let text = possessive_s(text);
        let refs = self.dv.names(&self.st).refs(&text, mentions);
        let turn = self.st.clock().turn;
        let data = (data != EventData::default()).then(|| Box::new(data));
        let mut rec = Record::of(&mut self.st, &mut self.chron);
        let id = rec.take_event_id()?;
        let ev = Event { id, turn, kind, text: text.into(), audience, tile, data, refs };
        // An event has a canonical form unless it holds a float that is not finite, and event
        // data holds none.
        rec.event(ev).ok().map(|()| id)
    }

    /// The players whose identity `pid` knows: itself, everyone it has met, and the barbarians
    /// (`Game._known_to`, `game.py:898-906`). `None` for a spectator, who knows everyone.
    #[must_use]
    pub fn known_to(&self, pid: Option<PlayerId>) -> Option<PlayerSet> {
        let p = pid?;
        self.st.player(p)?;
        let mut known = self.st.diplo().met_mask(p);
        known.insert(p);
        for b in self.st.barbarians().iter() {
            known.insert(b);
        }
        Some(known)
    }

    /// The events `pid` may see after id `since`, oldest first, at most `limit` of the newest,
    /// scrubbed for it (`Game.events_for`, `game.py:880-896`); a spectator (`None`) sees them all
    /// as they happened.
    #[must_use]
    pub fn events_for(&self, pid: Option<PlayerId>, since: u32, limit: usize) -> Vec<EventOut<'_>> {
        let known = self.known_to(pid);
        let mut out = Vec::new();
        for ev in self.chron.events_since(since).iter().rev() {
            if out.len() >= limit {
                break;
            }
            let hears = match (pid, ev.audience) {
                (None, _) | (_, None) => true,
                (Some(p), Some(a)) => a.contains(p),
            };
            if hears {
                out.push(self.scrub(ev, known));
            }
        }
        out.reverse();
        out
    }

    /// One event as `pid` may see it (`Game.event_view`, `game.py:908-912`).
    #[must_use]
    pub fn event_view<'a>(&self, ev: &'a Event, pid: Option<PlayerId>) -> EventOut<'a> {
        self.scrub(ev, self.known_to(pid))
    }

    /// The event with every player outside `known` made anonymous (`game.py:914-990`).
    fn scrub<'a>(&self, ev: &'a Event, known: Option<PlayerSet>) -> EventOut<'a> {
        let Some(known) = known else {
            return EventOut { event: Cow::Borrowed(ev), unknown: PlayerSet::default() };
        };
        let n = self.st.players().len();
        let in_game = |p: PlayerId| usize::from(p.0) < n;
        let mut hidden = PlayerSet::default();
        for r in &ev.refs {
            if !known.contains(r.player) {
                hidden.insert(r.player);
            }
        }
        if let Some(d) = &ev.data {
            for (_, p) in d.players() {
                if !known.contains(p) && in_game(p) {
                    hidden.insert(p);
                }
            }
        }
        let mut unknown = hidden;
        let mut tally_hidden = false;
        if let Some(res) = ev.data.as_ref().and_then(|d| d.results.as_deref())
            && (!res.tally.is_empty() || res.winner.is_some())
        {
            for &(p, _) in &res.tally {
                if !known.contains(p) {
                    tally_hidden = true;
                    unknown.insert(p);
                }
            }
            if res.winner.is_some_and(|w| !known.contains(w)) {
                tally_hidden = true;
            }
        }
        if hidden.is_empty() && !tally_hidden {
            return EventOut { event: Cow::Borrowed(ev), unknown };
        }
        let mut out = ev.clone();
        let text = self.anonymise(&ev.text, &ev.refs, hidden);
        out.text = scrub_coords(&text).into();
        out.refs.clear();
        out.tile = None;
        if let Some(d) = out.data.as_deref_mut() {
            for p in [
                &mut d.player,
                &mut d.a,
                &mut d.b,
                &mut d.attacker,
                &mut d.defender,
                &mut d.awaiting,
                &mut d.killer,
                &mut d.owner,
                &mut d.old_owner,
                &mut d.new_owner,
                &mut d.sender,
                &mut d.winner,
            ] {
                if p.is_some_and(|x| hidden.contains(x)) {
                    *p = None;
                }
            }
            if let Some(res) = d.results.as_deref_mut()
                && res.winner.is_some_and(|w| !known.contains(w))
            {
                res.winner = None;
            }
        }
        EventOut { event: Cow::Owned(out), unknown }
    }

    /// `text` with each reference to a hidden player replaced (`game.py:936-956`).
    fn anonymise(&self, text: &str, refs: &[NameRef], hidden: PlayerSet) -> String {
        let mut parts = String::with_capacity(text.len() + 16);
        let mut pos = 0usize;
        for r in refs {
            let (start, mut end) = (r.start as usize, r.end as usize);
            // References come from the engine, or from a save that validated; one that does not
            // fall on the text's characters is skipped rather than trusted.
            let (Some(before_all), Some(name)) = (text.get(..start), text.get(start..end)) else {
                continue;
            };
            if !hidden.contains(r.player) || start < pos {
                continue;
            }
            let mut rep = match r.kind {
                RefKind::City => "an unknown city".to_owned(),
                RefKind::Leader => "an unknown leader".to_owned(),
                RefKind::Civ if self.is_city_state(r.player) => UNKNOWN_CS.to_owned(),
                RefKind::Civ => UNKNOWN_CIV.to_owned(),
            };
            let before = before_all.trim_end_matches(is_space);
            if rep.starts_with(char::is_lowercase)
                && before.chars().next_back().is_none_or(|c| matches!(c, '.' | '!' | '?' | '"'))
            {
                rep = capitalise(&rep);
            }
            // "Aztecs' Warrior" becomes "Unknown Civilization's Warrior".
            let rest = &text[end..];
            let mut after = rest.chars();
            if after.next() == Some('\'')
                && !after.next().is_some_and(char::is_alphanumeric)
                && name.ends_with('s')
                && !rep.ends_with('s')
            {
                rep.push_str("'s");
                end += 1;
            }
            parts.push_str(&text[pos..start]);
            parts.push_str(&rep);
            pos = end;
        }
        parts.push_str(text.get(pos..).unwrap_or(""));
        parts
    }
}

/// The text with its first character upper-cased, as Python's `rep[0].upper() + rep[1:]`.
fn capitalise(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests;
