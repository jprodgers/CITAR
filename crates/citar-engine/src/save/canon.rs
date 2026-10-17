//! The canonical encoding of state, `CANON_V1`, and of the chronicle's entries (DESIGN.md 4.7,
//! 4.10).
//!
//! The state types derive `Serialize`, and `base::digest::CanonSerializer`, which is not
//! human-readable, writes them in `CANON_V1`: rule ids as integers, floats as their bits, and
//! every custom form in its canonical shape:
//! - tiles and memories as the concatenated `canon_bytes` of each tile (`save::columns`);
//! - bitsets as raw words, an [`IdSet`](crate::base::sets::IdSet) as its fixed words;
//! - the pair matrix as every cell; entity stores as their live items in id order
//!   (`save::json`);
//! - a driver's memory as kind, version, then length and bytes;
//! - `HostOnly` values as nothing, so host activity never moves the digest.
//!
//! A float that is NaN or infinite has no encoding, so the digest refuses a state holding one;
//! [`check_finite`] runs the same walk to refuse it before a save.
//!
//! The chronicle's running hash (`ChronicleHeads::absorb`) takes each entry's canonical bytes
//! from here: an event without its id, which host events shift, and a message or a stats row
//! whole.
//!
//! Replaces nothing in Python, which had no digest.

use serde::Serialize;

use crate::base::digest::{CanonError, CanonSerializer, CanonSink, to_canon_vec};
use crate::base::ids::{TileIdx, Turn};
use crate::base::sets::PlayerSet;
use crate::state::State;
use crate::state::chronicle::{EventData, EventType, Message, NameRef, StatsRow};

/// A sink that keeps nothing: the walk alone is wanted.
struct Discard;

impl CanonSink for Discard {
    fn write(&mut self, _bytes: &[u8]) {}
}

/// The state's canonical bytes (for tests and tools; the digest streams them instead).
pub fn state_bytes(st: &State) -> Result<Vec<u8>, CanonError> {
    to_canon_vec(st)
}

/// Whether every float of the state is finite: the canonical walk, keeping nothing.
pub fn check_finite(st: &State) -> Result<(), CanonError> {
    let mut ser = CanonSerializer::unbuffered(Discard);
    st.serialize(&mut ser)?;
    ser.finish();
    Ok(())
}

/// What of an event the running hash covers: everything but its id.
#[derive(Serialize)]
struct EventEntry<'a> {
    turn: Turn,
    kind: &'a EventType,
    text: &'a str,
    audience: Option<PlayerSet>,
    tile: Option<TileIdx>,
    data: Option<&'a EventData>,
    refs: &'a [NameRef],
}

/// The bytes an event adds to the running hash: its turn, type, text, audience, tile, data and
/// references, not its id (DESIGN.md 4.7).
pub fn event_entry(e: &crate::state::chronicle::Event) -> Result<Vec<u8>, CanonError> {
    to_canon_vec(&EventEntry {
        turn: e.turn,
        kind: &e.kind,
        text: &e.text,
        audience: e.audience,
        tile: e.tile,
        data: e.data.as_deref(),
        refs: &e.refs,
    })
}

/// The bytes a message adds to the running hash: all of it.
pub fn message_entry(m: &Message) -> Result<Vec<u8>, CanonError> {
    to_canon_vec(m)
}

/// The bytes a stats row adds to the running hash: all of it.
pub fn stats_entry(s: &StatsRow) -> Result<Vec<u8>, CanonError> {
    to_canon_vec(s)
}
