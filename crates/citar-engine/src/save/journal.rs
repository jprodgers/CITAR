//! Journal chunks, replay frames, and the chronicle rebuilt from chunks (DESIGN.md 4.11).
//!
//! **Chunks.** History is append-only and not part of the state, so a host saves it as it grows:
//! [`take_chunk`] encodes everything appended to the chronicle since the last take (events,
//! messages, thoughts, stats rows, action records and replay frames, in the order they were
//! appended) as one JSON payload, and counts it in `HostHeads::journal_seq`. Framing, compression
//! and the `.citar` container are `citar-store`'s (Phase 2).
//!
//! ```json
//! {"format": "citar-journal", "version": 1, "seq": 0,
//!  "entries": [{"event": {...}}, {"message": {...}}, {"stats": {...}}, {"frame": {...}}, ...]}
//! ```
//!
//! **Recording.** The game appends to its chronicle through a [`Record`], which folds each engine
//! entry into the running hash and counts each host one as it appends it, so the heads always
//! describe the history in the order it was written.
//!
//! **Rebuilding.** [`rebuild`] reads the chunks back in order through a [`Record`] into a
//! [`Chronicle`], recomputing the engine's running hash and every count as it goes, and says
//! whether the result agrees with the heads the state holds. A missing, unreadable or
//! disagreeing chunk leaves the history short (`LoadReport::chronicle_incomplete`); it never stops
//! the game.
//!
//! **Frames.** Python recorded the whole dynamic map every round (`victory.record_frame`), 88%
//! of every save. A [`FrameWriter`] records a [`FullFrame`] as a keyframe on the first frame
//! after a load and every 64 frames, and as a delta otherwise; a [`FrameDecoder`] turns the
//! records back into full frames. A frame is binary, little-endian:
//!
//! - a header: `CF`, the format version (1), `K` or `D`, and the turn (`i32`);
//! - a keyframe: the tile count (`u32`); the palettes of improvement, feature and unit type names
//!   (each a `u16` count of `u16`-length strings); the owner, improvement, route and feature
//!   layers (one byte per tile); each major's explored tiles (a `u8` count of majors, each its id,
//!   a `u32` word count and the `u64` words);
//! - a delta: the frame it was taken from, as its turn (`i32`) and 8 bytes of a hash of its
//!   palettes, layers and explored sets, which the decoder must be at; the changed tiles as 8-byte
//!   records (`u32` tile, then owner, improvement, route and feature); each major's newly explored
//!   tiles, as a `u32` count and LEB128 gaps (the first index, then each index less the previous
//!   one, less one);
//! - both, last: the cities in full (`u32` count; id `u32`, owner `u8`, tile `u32`, pop `u16`,
//!   capital `u8`, a `u16`-length name), the units packed at 8 bytes each (`u32` count; unit type
//!   `u16`, owner `u8`, hp `u8`, tile `u32`), and the range of event ids (`u32`, `u32`).
//!
//! The layers are Python's: owner 255 for none (254 at most), an improvement or top non-hill
//! feature as its palette position plus 1, the route level plus 4 when pillaged
//! (`victory.py:466-472`). Improvements, features and unit types are palette positions, and
//! the palettes are names (Python's frames named the unit type), so frames recorded before a
//! change of ruleset still read after it. A change of palette makes the next frame a keyframe,
//! and a decoder refuses a position past its palette.
//!
//! Replaces `victory.record_frame` (`citar/engine/victory.py:458-485`) and the history parts of
//! `GameState.to_dict` (`state.py:352-356`).

use serde::{Deserialize, Serialize};

use super::SaveError;
use super::canon;
use super::ctx::with_rules;
use crate::base::digest::CanonError;
use crate::base::ids::{EventId, PlayerId, Turn};
use crate::base::sets::BitSet;
use crate::rules::Ruleset;
use crate::state::State;
use crate::state::chronicle::{
    ActionRecord, Appended, Chronicle, ChronicleHeads, EntryKind, Event, EventType, FrameRecord,
    HostHeads, Message, StatsRow, Thought,
};

/// A chunk's `format`.
pub const FORMAT: &str = "citar-journal";

/// The chunk format version.
pub const VERSION: u32 = 1;

/// Everything appended to the chronicle between two takes, as a host appends it to its journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalChunk {
    /// Its number: chunks are taken from 0 up.
    pub seq: u32,
    /// The payload.
    pub json: Vec<u8>,
}

/// How far into a chronicle the journal has been taken: a position in the append order and in
/// each list. Kept by the game beside the chronicle, never saved: a loaded game starts at the end
/// of the history its chunks rebuilt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JournalCursor {
    order: usize,
    events: usize,
    messages: usize,
    thoughts: usize,
    stats: usize,
    actions: usize,
    frames: usize,
}

impl JournalCursor {
    /// The cursor past everything `chron` holds.
    #[must_use]
    pub fn at_end(chron: &Chronicle) -> Self {
        Self {
            order: chron.order().len(),
            events: chron.events().len(),
            messages: chron.messages().len(),
            thoughts: chron.thoughts().len(),
            stats: chron.stats().len(),
            actions: chron.actions().len(),
            frames: chron.frames().frames.len(),
        }
    }
}

/// One entry of a chunk, borrowed from the chronicle.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum EntryRef<'a> {
    Event(&'a Event),
    Message(&'a Message),
    Thought(&'a Thought),
    Stats(&'a StatsRow),
    Action(&'a ActionRecord),
    Frame(&'a FrameRecord),
}

/// One entry of a chunk, read back by [`decode_chunk`].
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalEntry {
    Event(Event),
    Message(Message),
    Thought(Thought),
    Stats(StatsRow),
    Action(ActionRecord),
    Frame(FrameRecord),
}

/// Why a chunk would not decode.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a journal chunk does not decode: {0}")]
pub struct JournalError(pub String);

#[derive(Serialize)]
struct ChunkRef<'a> {
    format: &'static str,
    version: u32,
    seq: u32,
    entries: Vec<EntryRef<'a>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChunkDoc {
    format: String,
    version: u32,
    seq: u32,
    entries: Vec<JournalEntry>,
}

/// The entries of `chron` past `cursor`, in the order they were appended, moving the cursor to
/// the end.
fn entries_since<'a>(chron: &'a Chronicle, cursor: &mut JournalCursor) -> Vec<EntryRef<'a>> {
    let mut out = Vec::with_capacity(chron.order().len().saturating_sub(cursor.order));
    let frames = &chron.frames().frames;
    for kind in chron.order().get(cursor.order..).unwrap_or(&[]) {
        let entry = match kind {
            Appended::Event => {
                cursor.events += 1;
                chron.events().get(cursor.events - 1).map(EntryRef::Event)
            }
            Appended::Message => {
                cursor.messages += 1;
                chron.messages().get(cursor.messages - 1).map(EntryRef::Message)
            }
            Appended::Thought => {
                cursor.thoughts += 1;
                chron.thoughts().get(cursor.thoughts - 1).map(EntryRef::Thought)
            }
            Appended::Stats => {
                cursor.stats += 1;
                chron.stats().get(cursor.stats - 1).map(EntryRef::Stats)
            }
            Appended::Action => {
                cursor.actions += 1;
                chron.actions().get(cursor.actions - 1).map(EntryRef::Action)
            }
            Appended::Frame => {
                cursor.frames += 1;
                frames.get(cursor.frames - 1).map(EntryRef::Frame)
            }
        };
        out.extend(entry);
    }
    cursor.order = chron.order().len();
    out
}

/// The chunk of everything appended to `chron` since `cursor`, numbered `host.journal_seq`,
/// which it then counts; `None` if nothing was appended.
///
/// A chunk that does not encode moves neither the cursor nor the count, so the next take
/// carries its entries.
pub fn take_chunk(
    rules: &'static Ruleset,
    chron: &Chronicle,
    cursor: &mut JournalCursor,
    host: &mut HostHeads,
) -> Result<Option<JournalChunk>, SaveError> {
    if cursor.order >= chron.order().len() {
        return Ok(None);
    }
    let seq = host.journal_seq;
    let mut next = *cursor;
    let doc = ChunkRef {
        format: FORMAT,
        version: VERSION,
        seq,
        entries: entries_since(chron, &mut next),
    };
    let json = with_rules(rules, || serde_json::to_vec(&doc))
        .map_err(|e| SaveError::Json(e.to_string()))?;
    *cursor = next;
    host.journal_seq = seq.saturating_add(1);
    Ok(Some(JournalChunk { seq, json }))
}

/// A chunk's number and entries, in the order they were appended, with rule objects named in
/// `rules`.
pub fn decode_chunk(
    rules: &'static Ruleset,
    bytes: &[u8],
) -> Result<(u32, Vec<JournalEntry>), JournalError> {
    let doc: ChunkDoc = with_rules(rules, || serde_json::from_slice(bytes))
        .map_err(|e| JournalError(e.to_string()))?;
    if doc.format != FORMAT || doc.version != VERSION {
        return Err(JournalError(format!("not a {FORMAT} v{VERSION} chunk")));
    }
    Ok((doc.seq, doc.entries))
}

// ---- Recording --------------------------------------------------------------------------------

/// A history and its heads, borrowed together, so that every entry is appended and counted in one
/// step (DESIGN.md 4.7, 4.11).
///
/// The engine's running hash folds events, messages and stats rows in the order they are
/// appended, and the host's heads count everything else. [`rebuild`] replays a journal through a
/// `Record` and compares the heads it ends with, so the game appends through one too: an entry
/// pushed to the chronicle but not folded into the heads, or folded in another order, would leave
/// every later load reporting the history incomplete.
pub struct Record<'a> {
    heads: &'a mut ChronicleHeads,
    host: &'a mut HostHeads,
    chron: &'a mut Chronicle,
}

impl<'a> Record<'a> {
    /// A record into `chron`, counted in `heads` and `host`.
    pub const fn new(
        heads: &'a mut ChronicleHeads,
        host: &'a mut HostHeads,
        chron: &'a mut Chronicle,
    ) -> Self {
        Self { heads, host, chron }
    }

    /// A record into the game's `chron`, counted in `st`'s heads.
    pub fn of(st: &'a mut State, chron: &'a mut Chronicle) -> Self {
        let (heads, host) = st.heads_mut();
        Self { heads, host, chron }
    }

    /// The next event id, from the feed engine and host events share
    /// (`HostHeads::take_event_id`); `None` once `u32` is spent.
    pub fn take_event_id(&mut self) -> Option<EventId> {
        self.host.take_event_id()
    }

    /// Appends an event: an engine event folded into the running hash (all of it but its id), a
    /// host event counted among the host's. Nothing is appended if it has no canonical form.
    pub fn event(&mut self, ev: Event) -> Result<(), CanonError> {
        if matches!(ev.kind, EventType::Engine(_)) {
            let bytes = canon::event_entry(&ev)?;
            self.heads.absorb(EntryKind::Event, &bytes);
        } else {
            self.host.host_events = self.host.host_events.saturating_add(1);
        }
        self.chron.push_event(ev);
        Ok(())
    }

    /// Appends a message, folded into the running hash.
    pub fn message(&mut self, m: Message) -> Result<(), CanonError> {
        let bytes = canon::message_entry(&m)?;
        self.heads.absorb(EntryKind::Message, &bytes);
        self.chron.push_message(m);
        Ok(())
    }

    /// Appends a stats row, folded into the running hash, and keeps it as the newest
    /// (`ChronicleHeads::last_stats`, which `save::summary` reads).
    pub fn stats(&mut self, s: StatsRow) -> Result<(), CanonError> {
        let bytes = canon::stats_entry(&s)?;
        self.heads.absorb(EntryKind::Stats, &bytes);
        self.heads.last_stats = Some(s.clone());
        self.chron.push_stats(s);
        Ok(())
    }

    /// Appends a thought, counted as host activity.
    pub fn thought(&mut self, t: Thought) {
        self.host.thoughts = self.host.thoughts.saturating_add(1);
        self.chron.push_thought(t);
    }

    /// Appends an action record, counted as host activity.
    pub fn action(&mut self, a: ActionRecord) {
        self.host.actions = self.host.actions.saturating_add(1);
        self.chron.push_action(a);
    }

    /// Appends a replay frame (from [`FrameWriter::push`]), counted as host activity.
    pub fn frame(&mut self, f: FrameRecord) {
        self.host.frames = self.host.frames.saturating_add(1);
        self.chron.push_frame(f);
    }

    /// Appends an entry read back from a chunk, as its own method would.
    pub fn append(&mut self, e: JournalEntry) -> Result<(), CanonError> {
        match e {
            JournalEntry::Event(ev) => self.event(ev),
            JournalEntry::Message(m) => self.message(m),
            JournalEntry::Stats(s) => self.stats(s),
            JournalEntry::Thought(t) => {
                self.thought(t);
                Ok(())
            }
            JournalEntry::Action(a) => {
                self.action(a);
                Ok(())
            }
            JournalEntry::Frame(f) => {
                self.frame(f);
                Ok(())
            }
        }
    }
}

// ---- Rebuilding -------------------------------------------------------------------------------

/// The chronicle `chunks` rebuild, oldest first, and whether it is the whole history `heads`
/// and `host` count: the same number of chunks in sequence, the same counts of each kind of
/// entry, the same newest stats row, and the same running hash.
///
/// The running hash folded in rule ids as numbers, which a save loaded under another ruleset
/// (`same_rules` false) may name differently, so it is compared only under the same ruleset;
/// the counts and the sequence still find a missing or unreadable chunk.
pub fn rebuild(
    rules: &'static Ruleset,
    chunks: &mut dyn Iterator<Item = &[u8]>,
    heads: &ChronicleHeads,
    host: &HostHeads,
    same_rules: bool,
) -> (Chronicle, bool) {
    let mut chron = Chronicle::new();
    let mut hashed = ChronicleHeads::default();
    let mut counted = HostHeads::default();
    let mut complete = true;
    for bytes in chunks {
        let Ok((seq, entries)) = decode_chunk(rules, bytes) else {
            complete = false;
            continue;
        };
        if seq != counted.journal_seq {
            complete = false;
        }
        counted.journal_seq = seq.saturating_add(1);
        let mut rec = Record::new(&mut hashed, &mut counted, &mut chron);
        for e in entries {
            complete &= rec.append(e).is_ok();
        }
    }
    complete &= hashed.engine_events == heads.engine_events
        && hashed.messages == heads.messages
        && hashed.stats == heads.stats
        && (hashed.hash == heads.hash || !same_rules)
        && hashed.last_stats == heads.last_stats
        && counted.host_events == host.host_events
        && counted.thoughts == host.thoughts
        && counted.actions == host.actions
        && counted.frames == host.frames
        && counted.journal_seq == host.journal_seq;
    (chron, complete)
}

// ---- Frames -----------------------------------------------------------------------------------

/// Frames between keyframes.
pub const KEYFRAME_EVERY: u32 = 64;

/// The names a frame's numbers stand for, so that frames recorded under one ruleset still read
/// after a change of ruleset has renumbered its objects.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct FramePalette {
    /// The improvement layer's, numbered from 1.
    pub improvement: Vec<Box<str>>,
    /// The feature layer's, numbered from 1.
    pub feature: Vec<Box<str>>,
    /// The units' types, numbered from 0 ([`FrameUnit::base`]).
    pub unit: Vec<Box<str>>,
}

/// A city as a frame shows it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FrameCity {
    pub id: u32,
    pub name: Box<str>,
    pub owner: u8,
    pub tile: u32,
    pub pop: u16,
    pub capital: bool,
}

/// A unit as a frame shows it: 8 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameUnit {
    /// Its type: a position in the palette's `unit` names.
    pub base: u16,
    pub owner: u8,
    pub hp: u8,
    pub tile: u32,
}

/// The dynamic map at the end of a round, whole: what Python's frames held.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FullFrame {
    pub turn: Turn,
    pub palette: FramePalette,
    /// Owner per tile: the player id, 255 for none.
    pub owner: Vec<u8>,
    /// Improvement per tile: its palette position plus 1, 0 for none.
    pub improvement: Vec<u8>,
    /// Route per tile: 1 road, 2 railroad, plus 4 when pillaged.
    pub route: Vec<u8>,
    /// Top feature per tile other than hills: its palette position plus 1, 0 for none.
    pub feature: Vec<u8>,
    /// Each major's explored tiles, by player id ascending.
    pub explored: Vec<(PlayerId, BitSet)>,
    pub cities: Vec<FrameCity>,
    pub units: Vec<FrameUnit>,
    /// The ids of the events of the round: after `.0`, up to `.1`.
    pub event_range: (u32, u32),
}

impl FullFrame {
    /// The frame of `st` under `rules`, for the round whose events are `event_range`
    /// (`victory.record_frame`).
    #[must_use]
    pub fn capture(rules: &Ruleset, st: &State, event_range: (u32, u32)) -> Self {
        let features = &rules.derived().features;
        let hill = rules.derived().known.hill;
        let names = |ids: &mut dyn Iterator<Item = crate::base::ids::TerrainId>| -> Vec<Box<str>> {
            ids.map(|t| Box::from(rules.name(t).unwrap_or(""))).collect()
        };
        let palette = FramePalette {
            improvement: rules.improvements().as_slice().iter().map(|i| i.name.clone()).collect(),
            feature: names(&mut features.as_slice().iter().copied()),
            unit: rules.base_units().as_slice().iter().map(|u| u.name.clone()).collect(),
        };
        let tiles = st.tiles().as_slice();
        let mut owner = Vec::with_capacity(tiles.len());
        let mut improvement = Vec::with_capacity(tiles.len());
        let mut route = Vec::with_capacity(tiles.len());
        let mut feature = Vec::with_capacity(tiles.len());
        for t in tiles {
            owner.push(t.owner().map_or(255, |p| p.0.min(254)));
            improvement.push(t.improvement().map_or(0, |i| i.0.saturating_add(1)));
            let level = t.route().map_or(0, |r| r as u8);
            route.push(level + if t.route_pillaged() { 4 } else { 0 });
            let mut f = t.features();
            f.remove(hill);
            feature.push(f.top().map_or(0, |x| x.0.saturating_add(1)));
        }
        let explored = st
            .players()
            .iter()
            .filter(|(_, p)| p.is_major())
            .map(|(id, p)| (id, p.explored.clone()))
            .collect();
        let cities = st
            .cities()
            .iter()
            .map(|c| FrameCity {
                id: c.id().get(),
                name: c.name.clone(),
                owner: c.owner().0,
                tile: c.tile().0,
                pop: c.pop,
                capital: st.player(c.owner()).is_some_and(|p| p.capital == Some(c.id())),
            })
            .collect();
        let units = st
            .units()
            .iter()
            .map(|u| FrameUnit {
                base: u.base.0,
                owner: u.owner().0,
                hp: u8::try_from(u.hp.max(0)).unwrap_or(u8::MAX),
                tile: u.tile().0,
            })
            .collect();
        Self {
            turn: st.clock().turn,
            palette,
            owner,
            improvement,
            route,
            feature,
            explored,
            cities,
            units,
            event_range,
        }
    }
}

/// Why a frame record would not decode.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a replay frame does not decode: {0}")]
pub struct FrameError(pub String);

/// Records frames as keyframes and deltas: the last frame is kept to take the next delta from.
#[derive(Clone, Debug, Default)]
pub struct FrameWriter {
    last: Option<FullFrame>,
    since_key: u32,
}

impl FrameWriter {
    /// A writer whose first frame will be a keyframe, as after a load.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The record of `frame`: a keyframe when there is no frame to take a delta from, every
    /// [`KEYFRAME_EVERY`] frames, or when the tiles, the palette or the majors changed or a tile
    /// was forgotten, which a delta cannot say; else a delta from the last frame.
    pub fn push(&mut self, frame: &FullFrame) -> FrameRecord {
        let key = match &self.last {
            None => true,
            Some(last) => {
                self.since_key >= KEYFRAME_EVERY
                    || last.owner.len() != frame.owner.len()
                    || last.palette != frame.palette
                    || last.explored.len() != frame.explored.len()
                    || last
                        .explored
                        .iter()
                        .zip(&frame.explored)
                        .any(|((p, a), (q, b))| p != q || !a.is_subset(b))
            }
        };
        let mut out = Vec::new();
        out.extend_from_slice(b"CF");
        out.push(1);
        out.push(if key { b'K' } else { b'D' });
        out.extend_from_slice(&frame.turn.to_le_bytes());
        match (&self.last, key) {
            (Some(last), false) => {
                // The frame it was taken from, which a decoder must be at to apply it.
                out.extend_from_slice(&last.turn.to_le_bytes());
                out.extend_from_slice(&base_check(last));
                write_delta(&mut out, last, frame);
            }
            _ => write_key(&mut out, frame),
        }
        write_tail(&mut out, frame);
        self.since_key = if key { 1 } else { self.since_key + 1 };
        self.last = Some(frame.clone());
        FrameRecord { turn: frame.turn, keyframe: key, bytes: out.into_boxed_slice() }
    }
}

/// What a delta is taken from, in 8 bytes: the start of a blake3 hash of the palettes, the tile
/// layers and the explored sets, everything a delta changes or leaves as it was. A delta carries
/// its base's, so one applied to any other frame (a chunk lost between them, a keyframe of
/// another run at the same turn) is refused instead of decoding wrong.
fn base_check(f: &FullFrame) -> [u8; 8] {
    let mut h = blake3::Hasher::new();
    for list in [&f.palette.improvement, &f.palette.feature, &f.palette.unit] {
        h.update(&len32(list.len()).to_le_bytes());
        for name in list {
            h.update(&len32(name.len()).to_le_bytes());
            h.update(name.as_bytes());
        }
    }
    for layer in [&f.owner, &f.improvement, &f.route, &f.feature] {
        h.update(&len32(layer.len()).to_le_bytes());
        h.update(layer);
    }
    h.update(&len32(f.explored.len()).to_le_bytes());
    for (p, set) in &f.explored {
        h.update(&[p.0]);
        let words = set.words();
        h.update(&len32(words.len()).to_le_bytes());
        for w in words {
            h.update(&w.to_le_bytes());
        }
    }
    let mut out = [0u8; 8];
    out.copy_from_slice(&h.finalize().as_bytes()[..8]);
    out
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// A length, which the formats bound well below `u32::MAX` (at most 65,536 tiles, 64 players).
fn len32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    let n = b.len().min(usize::from(u16::MAX));
    put_u16(out, n as u16);
    out.extend_from_slice(&b[..n]);
}

fn put_varint(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn write_key(out: &mut Vec<u8>, f: &FullFrame) {
    put_u32(out, len32(f.owner.len()));
    for list in [&f.palette.improvement, &f.palette.feature, &f.palette.unit] {
        put_u16(out, u16::try_from(list.len()).unwrap_or(u16::MAX));
        for name in list.iter().take(usize::from(u16::MAX)) {
            put_str(out, name);
        }
    }
    for layer in [&f.owner, &f.improvement, &f.route, &f.feature] {
        out.extend_from_slice(layer);
    }
    out.push(u8::try_from(f.explored.len()).unwrap_or(u8::MAX));
    for (p, set) in &f.explored {
        out.push(p.0);
        let words = set.words();
        put_u32(out, len32(words.len()));
        for w in words {
            out.extend_from_slice(&w.to_le_bytes());
        }
    }
}

fn write_delta(out: &mut Vec<u8>, last: &FullFrame, f: &FullFrame) {
    let changed: Vec<usize> = (0..f.owner.len())
        .filter(|&i| {
            f.owner[i] != last.owner[i]
                || f.improvement[i] != last.improvement[i]
                || f.route[i] != last.route[i]
                || f.feature[i] != last.feature[i]
        })
        .collect();
    put_u32(out, len32(changed.len()));
    for i in changed {
        put_u32(out, len32(i));
        out.extend_from_slice(&[f.owner[i], f.improvement[i], f.route[i], f.feature[i]]);
    }
    out.push(u8::try_from(f.explored.len()).unwrap_or(u8::MAX));
    for ((p, set), (_, before)) in f.explored.iter().zip(&last.explored) {
        out.push(p.0);
        let fresh: Vec<u32> = set.iter().filter(|&i| !before.contains(i)).collect();
        put_u32(out, len32(fresh.len()));
        let mut prev: Option<u32> = None;
        for i in fresh {
            put_varint(out, prev.map_or(i, |p| i - p - 1));
            prev = Some(i);
        }
    }
}

fn write_tail(out: &mut Vec<u8>, f: &FullFrame) {
    put_u32(out, len32(f.cities.len()));
    for c in &f.cities {
        put_u32(out, c.id);
        out.push(c.owner);
        put_u32(out, c.tile);
        put_u16(out, c.pop);
        out.push(u8::from(c.capital));
        put_str(out, &c.name);
    }
    put_u32(out, len32(f.units.len()));
    for u in &f.units {
        put_u16(out, u.base);
        out.push(u.owner);
        out.push(u.hp);
        put_u32(out, u.tile);
    }
    put_u32(out, f.event_range.0);
    put_u32(out, f.event_range.1);
}

/// Reads a frame's bytes.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], FrameError> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.bytes.len());
        let end = end.ok_or_else(|| FrameError(format!("it ends early, at byte {}", self.at)))?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, FrameError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, FrameError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, FrameError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, FrameError> {
        let b = self.take(8)?;
        let mut w = [0u8; 8];
        w.copy_from_slice(b);
        Ok(u64::from_le_bytes(w))
    }

    fn str(&mut self) -> Result<Box<str>, FrameError> {
        let n = usize::from(self.u16()?);
        let b = self.take(n)?;
        core::str::from_utf8(b).map(Box::from).map_err(|_| FrameError("a name is not UTF-8".into()))
    }

    fn varint(&mut self) -> Result<u32, FrameError> {
        let mut v: u32 = 0;
        for shift in (0..35).step_by(7) {
            let b = self.u8()?;
            v |= u32::from(b & 0x7f).checked_shl(shift).unwrap_or(0);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(FrameError("a varint is too long".into()))
    }

    /// A count of items at least `each` bytes long, refused if the bytes left cannot hold them.
    fn count(&mut self, each: usize) -> Result<usize, FrameError> {
        let n = self.u32()? as usize;
        if n.saturating_mul(each) > self.bytes.len() - self.at {
            return Err(FrameError(format!("{n} items cannot fit the bytes left")));
        }
        Ok(n)
    }
}

/// Turns frame records back into full frames, keyframe by keyframe and delta by delta.
#[derive(Clone, Debug, Default)]
pub struct FrameDecoder {
    last: Option<FullFrame>,
}

impl FrameDecoder {
    /// A decoder that waits for a keyframe.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The full frame `rec` records; a delta needs the frame before it.
    pub fn apply(&mut self, rec: &FrameRecord) -> Result<FullFrame, FrameError> {
        let mut r = Reader { bytes: &rec.bytes, at: 0 };
        if r.take(3)? != b"CF\x01" {
            return Err(FrameError("not a version 1 frame".into()));
        }
        let key = match r.u8()? {
            b'K' => true,
            b'D' => false,
            k => return Err(FrameError(format!("unknown frame kind {k}"))),
        };
        if key != rec.keyframe {
            return Err(FrameError("the record and its bytes disagree on the kind".into()));
        }
        let turn = r.u32()? as i32;
        if turn != rec.turn {
            return Err(FrameError("the record and its bytes disagree on the turn".into()));
        }
        let mut f = if key {
            read_key(&mut r)?
        } else {
            let last = self
                .last
                .as_ref()
                .ok_or_else(|| FrameError("a delta before any keyframe".into()))?;
            let base_turn = r.u32()? as i32;
            let check = r.take(8)?;
            if base_turn != last.turn || check != base_check(last) {
                return Err(FrameError(format!(
                    "a delta taken from the frame of turn {base_turn}, applied to another \
                     (the decoder is at turn {})",
                    last.turn
                )));
            }
            read_delta(&mut r, last)?
        };
        f.turn = turn;
        read_tail(&mut r, &mut f)?;
        if r.at != r.bytes.len() {
            return Err(FrameError("bytes left after the frame".into()));
        }
        check_palette(&f)?;
        self.last = Some(f.clone());
        Ok(f)
    }
}

/// Every improvement, feature and unit type of a decoded frame is in its palette, so a reader
/// can look each one up.
fn check_palette(f: &FullFrame) -> Result<(), FrameError> {
    let layers = [
        ("improvement", &f.improvement, f.palette.improvement.len()),
        ("feature", &f.feature, f.palette.feature.len()),
    ];
    for (what, layer, len) in layers {
        if let Some(t) = layer.iter().position(|&x| usize::from(x) > len) {
            return Err(FrameError(format!("tile {t}: {what} {} is past its palette", layer[t])));
        }
    }
    if let Some(u) = f.units.iter().find(|u| usize::from(u.base) >= f.palette.unit.len()) {
        return Err(FrameError(format!("unit type {} is past its palette", u.base)));
    }
    Ok(())
}

fn read_key(r: &mut Reader<'_>) -> Result<FullFrame, FrameError> {
    let n = r.count(4)?;
    let mut f = FullFrame::default();
    for list in [&mut f.palette.improvement, &mut f.palette.feature, &mut f.palette.unit] {
        let k = usize::from(r.u16()?);
        for _ in 0..k {
            list.push(r.str()?);
        }
    }
    f.owner = r.take(n)?.to_vec();
    f.improvement = r.take(n)?.to_vec();
    f.route = r.take(n)?.to_vec();
    f.feature = r.take(n)?.to_vec();
    let majors = r.u8()?;
    for _ in 0..majors {
        let p = PlayerId(r.u8()?);
        let k = r.count(8)?;
        let words = (0..k).map(|_| r.u64()).collect::<Result<Vec<_>, _>>()?;
        f.explored.push((p, BitSet::from_words(words)));
    }
    Ok(f)
}

fn read_delta(r: &mut Reader<'_>, last: &FullFrame) -> Result<FullFrame, FrameError> {
    let mut f = FullFrame {
        palette: last.palette.clone(),
        owner: last.owner.clone(),
        improvement: last.improvement.clone(),
        route: last.route.clone(),
        feature: last.feature.clone(),
        explored: last.explored.clone(),
        ..FullFrame::default()
    };
    let changed = r.count(8)?;
    for _ in 0..changed {
        let i = r.u32()? as usize;
        if i >= f.owner.len() {
            return Err(FrameError(format!("tile {i} is off the frame's map")));
        }
        f.owner[i] = r.u8()?;
        f.improvement[i] = r.u8()?;
        f.route[i] = r.u8()?;
        f.feature[i] = r.u8()?;
    }
    let majors = usize::from(r.u8()?);
    if majors != f.explored.len() {
        return Err(FrameError("a delta changes the majors".into()));
    }
    for (p, set) in &mut f.explored {
        if r.u8()? != p.0 {
            return Err(FrameError("a delta lists the majors in another order".into()));
        }
        let k = r.count(1)?;
        let mut prev: Option<u32> = None;
        for _ in 0..k {
            let gap = r.varint()?;
            let i = match prev {
                None => gap,
                Some(p) => p
                    .checked_add(gap)
                    .and_then(|x| x.checked_add(1))
                    .ok_or_else(|| FrameError("an explored tile past u32".into()))?,
            };
            if i >= len32(f.owner.len()) {
                return Err(FrameError(format!("explored tile {i} is off the frame's map")));
            }
            set.insert(i);
            prev = Some(i);
        }
    }
    Ok(f)
}

fn read_tail(r: &mut Reader<'_>, f: &mut FullFrame) -> Result<(), FrameError> {
    let cities = r.count(14)?;
    for _ in 0..cities {
        f.cities.push(FrameCity {
            id: r.u32()?,
            owner: r.u8()?,
            tile: r.u32()?,
            pop: r.u16()?,
            capital: match r.u8()? {
                0 => false,
                1 => true,
                b => return Err(FrameError(format!("capital flag {b}"))),
            },
            name: r.str()?,
        });
    }
    let units = r.count(8)?;
    for _ in 0..units {
        f.units.push(FrameUnit { base: r.u16()?, owner: r.u8()?, hp: r.u8()?, tile: r.u32()? });
    }
    f.event_range = (r.u32()?, r.u32()?);
    Ok(())
}
