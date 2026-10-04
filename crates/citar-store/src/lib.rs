//! CITAR's save files (DESIGN.md P2.5): the `.citar` v2 [`container`] and the append-only
//! [`journal`] beside it.
//!
//! A container is a small uncompressed JSON header (what listing saves reads) and one zstd frame
//! holding the body, with the engine's state JSON spliced in raw. A journal holds the engine's
//! journal chunks (replay frames and history) as framed records with sequence numbers and
//! chained hashes, written off the session lock; a container names the prefix of its journal it
//! was saved with ([`JournalRef`]), so a save never loads with another timeline's history. Each
//! module's docs specify its format byte for byte.
//!
//! It replaces the Python session's save path (`citar/server/session.py`, `save` and `to_save`:
//! one gzip JSON document written whole, history and replay frames included) and what package
//! 1a-09 left to the hosts (DESIGN.md 4.11: framing, torn-tail recovery and zstd).
//!
//! **Nothing here panics on any input,** and nothing allocates more than a file holds or a
//! format's limit allows ([`container::MAX_BODY`], [`journal::MAX_CHUNK`]): every failure is a
//! [`StoreError`].
//!
//! **Files other programs hold for a moment.** OneDrive and virus scanners open a file just
//! written, and Windows then refuses to rename over it or to write it. A write that meets
//! `PermissionDenied` (or, on Windows, a sharing or lock violation, which Python's
//! `PermissionError` also covers) is tried again eight times, 250 ms apart, as the Python
//! session's save did.
//!
//! **Locks.** A [`JournalWriter`] holds an exclusive OS lock on its file (`File::try_lock`), and
//! [`read_upto`] and [`fork`] a shared one while they read. Windows's locks are mandatory, so a
//! journal a writer holds cannot be read there at all; Linux is held to the same rule by the
//! shared lock, so the two behave alike: a journal a writer holds is [`StoreError::Locked`] to
//! everyone else until the writer is dropped.

#![forbid(unsafe_code)]

pub mod container;
mod disk;
pub mod journal;

use std::path::PathBuf;

pub use self::container::{
    BodyParts, ChainRef, Container, Header, SessionRef, read_container, read_header,
    write_container,
};
pub use self::journal::{JournalRef, JournalWriter, Recovered, fork, read_upto};

/// Why a store call failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The file system refused.
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The bytes are not a container or a journal of a version this build reads.
    #[error("{}: {why}", path.display())]
    Format { path: PathBuf, why: String },
    /// A version 1 save: the gzip JSON document the Python engine wrote, which this build does
    /// not read (DESIGN.md P2.8: it is archived with 0.1.5).
    #[error("{} is a version 1 save (gzip JSON, written by the Python engine)", path.display())]
    Legacy { path: PathBuf },
    /// A complete record or frame fails its hash or its order, or a part of a container does not
    /// parse: bit rot, a sync conflict, or a file cut short.
    #[error("{}: corrupt at byte {at}: {why}", path.display())]
    Corrupt { path: PathBuf, at: u64, why: String },
    /// Another writer holds the journal's lock (or a reader its shared one, for a moment).
    #[error("{} is locked: another session is writing or reading it", path.display())]
    Locked { path: PathBuf },
    /// An append out of sequence: records are the engine's chunks 0, 1, 2, ...
    #[error("record {got} is out of order: the next is {want}")]
    OutOfOrder { want: u32, got: u32 },
    /// A save names a journal prefix the file does not hold: it was rewritten, forked or
    /// swapped, and the save is refused rather than loaded with another timeline's history.
    #[error("{}: {why}", path.display())]
    Mismatch { path: PathBuf, why: String },
    /// What was given to write breaks the format: a header that is not this version's, a body
    /// part that is not a JSON object, a chunk larger than a record holds. Nothing was written.
    #[error("{}: {why}", path.display())]
    Invalid { path: PathBuf, why: String },
}
