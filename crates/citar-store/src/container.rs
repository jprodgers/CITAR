//! The `.citar` v2 container (DESIGN.md P2.5.1):
//!
//! ```text
//! "CITARSV2" | u32 LE header length | header JSON | one zstd frame: body JSON
//! header: {"format":"citar-save","version":2,"saved_at", "engine_build",
//!          "rules":"<RulesetId hex>", "summary": <save::summary of the state>,
//!          "session": {id, name, benchmark}, "journal": {"file", "records", "bytes", "head"}}
//! body:   {"session": {...}, "metrics": {...}, "state": <the engine's state JSON, spliced raw>,
//!          "chain": null | {"head","rounds"}}
//! ```
//!
//! The header is uncompressed and small, so listing saves reads only headers, never a gargantuan
//! state's 6 MB. The body is zstd level 3, about 10x on this JSON; the state is spliced in raw
//! and handed to `Game::load` without a second parse. Writes are atomic: a temporary file, then
//! a rename, retried eight times at 250 ms on `PermissionDenied` (OneDrive and virus scanners).

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::StoreError;
use crate::journal::JournalRef;

/// The first eight bytes of every container.
pub const MAGIC: &[u8; 8] = b"CITARSV2";

/// The header's `format`.
pub const FORMAT: &str = "citar-save";

/// The header's `version`.
pub const VERSION: u32 = 2;

/// The session a save belongs to, as the header names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRef {
    pub id: String,
    pub name: String,
    pub benchmark: bool,
}

/// A container's header: everything listing saves needs, uncompressed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Header {
    /// [`FORMAT`].
    pub format: String,
    /// [`VERSION`].
    pub version: u32,
    /// When it was saved, as the host writes it (ISO 8601).
    pub saved_at: String,
    /// The build that saved it (`citar_bot::build_id`).
    pub engine_build: String,
    /// The ruleset's id, 64 hex digits.
    pub rules: String,
    /// The engine's summary of the state (`save::summary`).
    pub summary: Value,
    pub session: SessionRef,
    /// The prefix of its journal the save was taken with; `None` for a save with no history.
    pub journal: Option<JournalRef>,
}

/// The chain of round digests a body carries, if the game keeps one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainRef {
    /// The chain's head, hex.
    pub head: String,
    /// The rounds it covers.
    pub rounds: u32,
}

/// The parts of a body to write, each already JSON: the state is spliced in as it is.
#[derive(Clone, Copy, Debug)]
pub struct BodyParts<'a> {
    /// The session's own record, a JSON object.
    pub session: &'a [u8],
    /// Its metrics, a JSON object.
    pub metrics: &'a [u8],
    /// The engine's state JSON (`Snapshot::to_json`).
    pub state: &'a [u8],
    pub chain: Option<&'a ChainRef>,
}

/// A container read back: its header and its body's JSON, decompressed.
#[derive(Clone, Debug)]
pub struct Container {
    pub header: Header,
    /// The body JSON.
    pub body: Vec<u8>,
}

impl Container {
    /// The state's JSON within the body, as it was spliced in: what `Game::load` takes.
    ///
    /// # Errors
    /// A body that is not the container's shape.
    pub fn state(&self) -> Result<&[u8], StoreError> {
        let _ = &self.body;
        Err(StoreError::NotYet("reading a container's body"))
    }
}

/// Writes a container to `path` atomically: a temporary file beside it, then a rename.
///
/// # Errors
/// The file system refuses past the retries.
pub fn write_container(
    path: &Path,
    header: &Header,
    body: &BodyParts<'_>,
) -> Result<(), StoreError> {
    let _ = (path, header, body);
    Err(StoreError::NotYet("writing a container"))
}

/// The header of the container at `path`, without decompressing the body.
///
/// # Errors
/// Not a container of this version; the file cannot be read.
pub fn read_header(path: &Path) -> Result<Header, StoreError> {
    let _ = path;
    Err(StoreError::NotYet("reading a container's header"))
}

/// The container at `path`, header and body.
///
/// # Errors
/// Not a container of this version, a body that does not decompress, or the file cannot be
/// read.
pub fn read_container(path: &Path) -> Result<Container, StoreError> {
    let _ = path;
    Err(StoreError::NotYet("reading a container"))
}
