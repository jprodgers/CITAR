//! CITAR's save files (DESIGN.md P2.5): the `.citar` v2 [`container`] and the append-only
//! [`journal`] beside it.
//!
//! A container is a small uncompressed JSON header (what listing saves reads) and one zstd frame
//! holding the body, with the engine's state JSON spliced in raw. A journal holds the engine's
//! journal chunks (replay frames and history) as framed records with sequence numbers and hashes,
//! written off the session lock; a container names the prefix of its journal it was saved with
//! ([`JournalRef`]), so a save never loads with another timeline's history.
//!
//! Nothing here panics on any input: every failure is a [`StoreError`].
//!
//! Package 2-00a wrote the types and signatures; every function returns
//! [`StoreError::NotYet`] until package 2-02 writes the formats and removes the variant.

#![forbid(unsafe_code)]

pub mod container;
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
    /// A complete record or frame fails its hash or its order: bit rot or a sync conflict.
    #[error("{}: corrupt at byte {at}: {why}", path.display())]
    Corrupt { path: PathBuf, at: u64, why: String },
    /// Another writer holds the journal's lock.
    #[error("{} is open in another writer", path.display())]
    Locked { path: PathBuf },
    /// An append out of sequence: records are the engine's chunks 0, 1, 2, ...
    #[error("record {got} is out of order: the next is {want}")]
    OutOfOrder { want: u32, got: u32 },
    /// A save names a journal prefix the file does not hold: it was rewritten, forked or
    /// swapped, and the save is refused rather than loaded with another timeline's history.
    #[error("{}: {why}", path.display())]
    Mismatch { path: PathBuf, why: String },
    /// What package 2-02 writes; it removes the variant.
    #[error("not built yet: {0} (package 2-02)")]
    NotYet(&'static str),
}
