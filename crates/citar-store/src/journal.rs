//! The journal (DESIGN.md P2.5.2): an append-only file of the engine's journal chunks.
//!
//! ```text
//! "CITARJNL" | u16 LE version (1) | records...
//! record: u32 LE payload length | u32 LE seq | u8 kind (1 = engine journal chunk)
//!         | u8 codec (0 raw, 1 zstd) | [u8; 8] blake3(payload) | payload
//! ```
//!
//! - [`JournalWriter::open`] scans the records and takes an exclusive OS lock, refusing a file
//!   another writer holds. It truncates only an incomplete tail (a crash mid-append). A complete
//!   record that fails its hash or breaks the sequence is corruption: nothing is truncated,
//!   appends are refused, and [`Recovered`] says where the good prefix ends, from which the
//!   session forks.
//! - [`JournalWriter::append`] refuses a sequence number that is not the next, so records are
//!   exactly the engine's chunks 0, 1, 2, ...
//! - A container names its journal by [`JournalRef`]; [`read_upto`] reads exactly that prefix and
//!   refuses one whose last record does not hash to its head. [`fork`] copies a prefix to a new
//!   file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::StoreError;

/// The first eight bytes of every journal.
pub const MAGIC: &[u8; 8] = b"CITARJNL";

/// The format's version, after the magic.
pub const VERSION: u16 = 1;

/// What a record holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum RecordKind {
    /// One of the engine's journal chunks (`Game::take_journal_chunk`).
    EngineChunk = 1,
}

/// How a record's payload is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Codec {
    Raw = 0,
    Zstd = 1,
}

/// The prefix of a journal a container was saved with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalRef {
    /// The journal's file name, beside the container.
    pub file: String,
    /// How many records the prefix holds.
    pub records: u32,
    /// Its length in bytes, magic and version included.
    pub bytes: u64,
    /// The hash of its last record's payload, 16 hex digits (empty for no records).
    pub head: String,
}

/// What [`JournalWriter::open`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recovered {
    /// The complete, good records.
    pub records: u32,
    /// Where they end.
    pub bytes: u64,
    /// The length of an incomplete last record it truncated, if there was one.
    pub torn: Option<u64>,
    /// Where corruption starts, if a complete record failed its hash or its order: nothing was
    /// truncated and appends are refused.
    pub corrupt_at: Option<u64>,
}

/// The one writer of a journal file, holding its OS lock.
#[derive(Debug)]
pub struct JournalWriter {
    path: PathBuf,
    /// The records written, so the next append's sequence number.
    records: u32,
    /// Where they end.
    bytes: u64,
    /// The last record's hash.
    head: [u8; 8],
    /// Whether open found corruption, which refuses appends.
    corrupt: bool,
}

impl JournalWriter {
    /// Opens (or creates) the journal at `path` for appending: scans its records, truncates an
    /// incomplete tail, and takes the OS lock.
    ///
    /// # Errors
    /// Another writer holds the file; it is not a journal; the file system refuses.
    pub fn open(path: &Path) -> Result<(Self, Recovered), StoreError> {
        let _ = path;
        Err(StoreError::NotYet("opening a journal"))
    }

    /// Appends chunk `seq`, which must be the next.
    ///
    /// # Errors
    /// A sequence number out of order; a journal open found corrupt; the file system refuses
    /// past the retries.
    pub fn append(&mut self, seq: u32, chunk: &[u8]) -> Result<(), StoreError> {
        if self.corrupt {
            return Err(StoreError::Corrupt {
                path: self.path.clone(),
                at: self.bytes,
                why: "appends are refused after corruption".to_owned(),
            });
        }
        if seq != self.records {
            return Err(StoreError::OutOfOrder { want: self.records, got: seq });
        }
        let _ = chunk;
        Err(StoreError::NotYet("appending to a journal"))
    }

    /// Flushes what was appended to the disk (`File::sync_data`).
    ///
    /// # Errors
    /// The file system refuses.
    pub fn sync(&mut self) -> Result<(), StoreError> {
        Err(StoreError::NotYet("syncing a journal"))
    }

    /// The prefix written so far, for the container that names it.
    #[must_use]
    pub fn reference(&self) -> JournalRef {
        let file =
            self.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let head = if self.records == 0 {
            String::new()
        } else {
            self.head.iter().map(|b| format!("{b:02x}")).collect()
        };
        JournalRef { file, records: self.records, bytes: self.bytes, head }
    }
}

/// The payloads of the prefix `r` of the journal at `path`, in order.
///
/// # Errors
/// The prefix runs past the good records, or its last record does not hash to its head.
pub fn read_upto(path: &Path, r: &JournalRef) -> Result<Vec<Vec<u8>>, StoreError> {
    let _ = (path, r);
    Err(StoreError::NotYet("reading a journal"))
}

/// Copies the prefix `r` of the journal at `path` to a new journal at `new`.
///
/// # Errors
/// The prefix is not intact; the file system refuses.
pub fn fork(path: &Path, r: &JournalRef, new: &Path) -> Result<(), StoreError> {
    let _ = (path, r, new);
    Err(StoreError::NotYet("forking a journal"))
}
