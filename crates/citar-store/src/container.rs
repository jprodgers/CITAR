//! The `.citar` v2 container (DESIGN.md P2.5.1).
//!
//! ```text
//! "CITARSV2" | u32 LE header length | header JSON | one zstd frame: body JSON
//! header: {"format":"citar-save","version":2,"saved_at", "engine_build",
//!          "rules":"<RulesetId hex>", "summary": <save::summary of the state>,
//!          "session": {id, name, benchmark}, "journal": null | {"file", "records", "bytes", "head"}}
//! body:   {"session": {...}, "metrics": {...}, "state": <the engine's state JSON, spliced raw>,
//!          "chain": null | {"head","rounds"}}
//! ```
//!
//! **The format, exactly.**
//! - The 8-byte magic `CITARSV2`, then the header's length in bytes as a little-endian `u32`
//!   (at most [`MAX_HEADER`]), then the header: a UTF-8 JSON object with exactly the keys of
//!   [`Header`]. `format` is `citar-save` and `version` 2; `rules` is 64 lower-case hex digits;
//!   `summary` is an object; `journal` is null or a [`JournalRef`] (a plain file name, and a
//!   head that is empty exactly when the prefix has no records).
//! - Then one zstd frame (level 3) and nothing after it. The frame records its content size,
//!   at most [`MAX_BODY`], and a content checksum, so a damaged body is found when it is read.
//! - The body is a JSON object with exactly the keys `session`, `metrics`, `state` and `chain`,
//!   in that order when written. The first three are JSON objects spliced in as the writer was
//!   given them (without surrounding white space); `chain` is null or a [`ChainRef`].
//!
//! The header is uncompressed and small, so listing saves reads only headers, never a gargantuan
//! state's 6 MB. The body is zstd level 3, about 10x on this JSON; the state is spliced in raw
//! and handed to `Game::load` without a second parse (the body is scanned once when it is read,
//! to check its shape and find the state, which is 0.6 ms a megabyte). Writes are atomic: a
//! temporary file beside the target, synced, then a rename, retried eight times at 250 ms on
//! `PermissionDenied` (OneDrive and virus scanners); on Unix the folder is synced after.
//!
//! **Errors.** A file that does not start with the magic is [`StoreError::Format`] (a gzip file
//! is [`StoreError::Legacy`], a Python save), and so is a header of another format or version.
//! Past the magic, anything that does not parse or check (a header cut short or not JSON, a
//! frame that fails its checksum, a body of another shape) is [`StoreError::Corrupt`], at the
//! byte where the bad part starts. A header or body given to [`write_container`] that breaks
//! the format is [`StoreError::Invalid`], and nothing is written.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::ops::Range;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use crate::StoreError;
use crate::disk::{self, Cleanup, Retry, io_err};
use crate::journal::JournalRef;

/// The first eight bytes of every container.
pub const MAGIC: &[u8; 8] = b"CITARSV2";

/// The header's `format`.
pub const FORMAT: &str = "citar-save";

/// The header's `version`.
pub const VERSION: u32 = 2;

/// The zstd level of the body: about 10x on the state's JSON, at some 300 MB/s.
pub const LEVEL: i32 = 3;

/// The longest header read or written: 16 MiB. A real one is a few kilobytes (the summary
/// names every player); the limit keeps a damaged length from asking for gigabytes.
pub const MAX_HEADER: u32 = 16 << 20;

/// The longest body read or written, 256 MiB: some forty times a gargantuan late game's.
pub const MAX_BODY: u64 = 256 << 20;

/// The longest file [`read_container`] reads: the limits above, with room for the frame's own
/// bytes.
const MAX_FILE: u64 = 12 + MAX_HEADER as u64 + MAX_BODY + (MAX_BODY >> 7) + (1 << 20);

/// The first bytes of a zstd frame (its magic number, little-endian).
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// The first bytes of a gzip file: a version 1 save.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Where the header starts: after the magic and its length.
const HEADER_AT: u64 = 12;

/// The session a save belongs to, as the header names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRef {
    pub id: String,
    pub name: String,
    pub benchmark: bool,
}

/// A container's header: everything listing saves needs, uncompressed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    /// [`FORMAT`].
    pub format: String,
    /// [`VERSION`].
    pub version: u32,
    /// When it was saved, as the host writes it (ISO 8601).
    pub saved_at: String,
    /// The build that saved it (`citar_bot::build_id`).
    pub engine_build: String,
    /// The ruleset's id, 64 lower-case hex digits.
    pub rules: String,
    /// The engine's summary of the state (`save::summary`), a JSON object.
    pub summary: Value,
    pub session: SessionRef,
    /// The prefix of its journal the save was taken with; `None` for a save with no history.
    pub journal: Option<JournalRef>,
}

impl Header {
    /// What is wrong with the header, if anything, past its format and version.
    fn fault(&self) -> Option<String> {
        if !disk::is_hex(&self.rules, 64) {
            return Some(format!("rules {:?} is not 64 lower-case hex digits", self.rules));
        }
        if !self.summary.is_object() {
            return Some("its summary is not a JSON object".to_owned());
        }
        self.journal.as_ref().and_then(JournalRef::fault).map(|why| format!("its journal: {why}"))
    }
}

/// The chain of round digests a body carries, if the game keeps one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainRef {
    /// The chain's head, 64 lower-case hex digits.
    pub head: String,
    /// The rounds it covers.
    pub rounds: u32,
}

impl ChainRef {
    fn fault(&self) -> Option<String> {
        (!disk::is_hex(&self.head, 64))
            .then(|| format!("the chain's head {:?} is not 64 lower-case hex digits", self.head))
    }
}

/// The parts of a body to write, each already JSON: the state is spliced in as it is.
#[derive(Clone, Copy, Debug)]
pub struct BodyParts<'a> {
    /// The session's own record, a JSON object.
    pub session: &'a [u8],
    /// Its metrics, a JSON object.
    pub metrics: &'a [u8],
    /// The engine's state JSON (`Snapshot::to_json`), an object.
    pub state: &'a [u8],
    pub chain: Option<&'a ChainRef>,
}

/// A container read back: its header and its body's JSON, decompressed and checked, with the
/// parts of the body found.
#[derive(Clone, Debug)]
pub struct Container {
    header: Header,
    body: Vec<u8>,
    session: Range<usize>,
    metrics: Range<usize>,
    state: Range<usize>,
    chain: Option<ChainRef>,
}

impl Container {
    /// The header.
    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    /// The header, without the body.
    #[must_use]
    pub fn into_header(self) -> Header {
        self.header
    }

    /// The whole body's JSON.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The state's JSON within the body, as it was spliced in: what `Game::load` takes.
    #[must_use]
    pub fn state(&self) -> &[u8] {
        self.part(&self.state)
    }

    /// The session's record, a JSON object.
    #[must_use]
    pub fn session(&self) -> &[u8] {
        self.part(&self.session)
    }

    /// The metrics, a JSON object.
    #[must_use]
    pub fn metrics(&self) -> &[u8] {
        self.part(&self.metrics)
    }

    /// The digest chain the body carries, if any.
    #[must_use]
    pub const fn chain(&self) -> Option<&ChainRef> {
        self.chain.as_ref()
    }

    fn part(&self, r: &Range<usize>) -> &[u8] {
        // The ranges were found in this body when it was read, and the body never changes.
        self.body.get(r.clone()).unwrap_or_default()
    }
}

/// Writes a container to `path` atomically: a temporary file beside it, synced, then a rename,
/// tried again on `PermissionDenied` (and Windows's sharing and lock violations) eight times,
/// 250 ms apart.
///
/// # Errors
/// [`StoreError::Invalid`] for a header or a body that breaks the format (nothing is written);
/// [`StoreError::Io`] when the file system refuses past the retries (the temporary file is
/// removed, and a container already at `path` is left as it was).
pub fn write_container(
    path: &Path,
    header: &Header,
    body: &BodyParts<'_>,
) -> Result<(), StoreError> {
    let invalid = |why: String| StoreError::Invalid { path: path.to_owned(), why };
    if header.format != FORMAT || header.version != VERSION {
        return Err(invalid(format!(
            "a header must say format {FORMAT:?} and version {VERSION}, not {:?} and {}",
            header.format, header.version
        )));
    }
    if let Some(why) = header.fault() {
        return Err(invalid(why));
    }
    let head = serde_json::to_vec(header).map_err(|e| invalid(format!("its header: {e}")))?;
    let head_len = u32::try_from(head.len())
        .ok()
        .filter(|&n| n <= MAX_HEADER)
        .ok_or_else(|| invalid(format!("a header of {} bytes is over the limit", head.len())))?;
    let session = object(body.session).map_err(|why| invalid(format!("the session: {why}")))?;
    let metrics = object(body.metrics).map_err(|why| invalid(format!("the metrics: {why}")))?;
    let state = object(body.state).map_err(|why| invalid(format!("the state: {why}")))?;
    if let Some(why) = body.chain.and_then(ChainRef::fault) {
        return Err(invalid(why));
    }
    let chain = serde_json::to_vec(&body.chain).map_err(|e| invalid(format!("the chain: {e}")))?;
    let pieces: [&[u8]; 9] = [
        b"{\"session\":",
        session,
        b",\"metrics\":",
        metrics,
        b",\"state\":",
        state,
        b",\"chain\":",
        &chain,
        b"}",
    ];
    let total: u64 = pieces.iter().map(|p| p.len() as u64).sum();
    if total > MAX_BODY {
        return Err(invalid(format!("a body of {total} bytes is over the limit of {MAX_BODY}")));
    }

    let (file, tmp) = disk::create_temp(path).map_err(|e| io_err(path, e))?;
    let cleanup = Cleanup::new(&tmp);
    write_file(file, &head, head_len, &pieces, total).map_err(|e| io_err(path, e))?;
    disk::retry(Retry::DEFAULT, || std::fs::rename(&tmp, path)).map_err(|e| io_err(path, e))?;
    cleanup.keep();
    disk::sync_parent(path);
    Ok(())
}

/// The bytes of a container into `file`, then synced: the magic, the header, and the body's
/// pieces through one zstd frame.
fn write_file(
    file: File,
    head: &[u8],
    head_len: u32,
    pieces: &[&[u8]],
    total: u64,
) -> std::io::Result<()> {
    let mut w = BufWriter::with_capacity(1 << 18, file);
    w.write_all(MAGIC)?;
    w.write_all(&head_len.to_le_bytes())?;
    w.write_all(head)?;
    let mut z = zstd::stream::write::Encoder::new(&mut w, LEVEL)?;
    z.include_checksum(true)?;
    z.include_contentsize(true)?;
    z.set_pledged_src_size(Some(total))?;
    for p in pieces {
        z.write_all(p)?;
    }
    z.finish()?;
    let file = w.into_inner().map_err(std::io::IntoInnerError::into_error)?;
    file.sync_all()
}

/// `bytes` as one JSON object and nothing else, without surrounding white space.
fn object(bytes: &[u8]) -> Result<&[u8], String> {
    let raw: &RawValue = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if !raw.get().starts_with('{') {
        return Err("not a JSON object".to_owned());
    }
    Ok(raw.get().as_bytes())
}

/// The header of the container at `path`, without decompressing the body. It checks that a
/// zstd frame follows the header, not the frame itself: [`read_container`] does that.
///
/// # Errors
/// [`StoreError::Format`] for a file that is not a container of this version (and
/// [`StoreError::Legacy`] for a version 1 save); [`StoreError::Corrupt`] for one whose header is
/// cut short, does not parse or does not check; [`StoreError::Io`] when it cannot be read.
pub fn read_header(path: &Path) -> Result<Header, StoreError> {
    let io = |e| io_err(path, e);
    let mut file = File::open(path).map_err(io)?;
    let len = file.metadata().map_err(io)?.len();
    let mut lead = Vec::with_capacity(12);
    (&mut file).take(HEADER_AT).read_to_end(&mut lead).map_err(io)?;
    let n = header_len(path, &lead, len)?;
    let mut head = Vec::new();
    (&mut file).take(u64::from(n) + 4).read_to_end(&mut head).map_err(io)?;
    let (head, frame) = head.split_at(head.len().min(n as usize));
    let header = parse_header(path, head)?;
    if frame != ZSTD_MAGIC {
        return Err(corrupt(path, HEADER_AT + u64::from(n), "no zstd frame after the header"));
    }
    Ok(header)
}

/// The container at `path`, header and body, the body decompressed and its shape checked.
///
/// # Errors
/// As [`read_header`], and [`StoreError::Corrupt`] for a body that does not decompress (its
/// checksum fails, it is cut short, bytes follow its frame) or is not the body's shape.
pub fn read_container(path: &Path) -> Result<Container, StoreError> {
    let io = |e| io_err(path, e);
    let mut file = File::open(path).map_err(io)?;
    let len = file.metadata().map_err(io)?.len();
    if len > MAX_FILE {
        return Err(StoreError::Format {
            path: path.to_owned(),
            why: format!("{len} bytes is larger than any container this build writes"),
        });
    }
    let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    (&mut file).take(MAX_FILE).read_to_end(&mut bytes).map_err(io)?;
    let lead = bytes.get(..bytes.len().min(12)).unwrap_or_default();
    let n = header_len(path, lead, bytes.len() as u64)?;
    let frame_at = 12 + n as usize;
    let header = parse_header(path, bytes.get(12..frame_at).unwrap_or_default())?;
    let frame = bytes.get(frame_at..).unwrap_or_default();
    let body = decode_frame(frame, MAX_BODY)
        .map_err(|why| corrupt(path, frame_at as u64, &format!("its body: {why}")))?;
    drop(bytes);
    let parts = body_parts(&body).map_err(|why| {
        corrupt(path, frame_at as u64, &format!("its body is not a container's: {why}"))
    })?;
    Ok(Container {
        header,
        session: parts.session,
        metrics: parts.metrics,
        state: parts.state,
        chain: parts.chain,
        body,
    })
}

/// The header's length from a file's first (up to) twelve bytes, checked against the file's
/// length `len`: the magic must be there, and the header within the file and the limit.
fn header_len(path: &Path, lead: &[u8], len: u64) -> Result<u32, StoreError> {
    if lead.starts_with(&GZIP_MAGIC) {
        return Err(StoreError::Legacy { path: path.to_owned() });
    }
    if !lead.starts_with(MAGIC) {
        return Err(StoreError::Format {
            path: path.to_owned(),
            why: "not a CITAR save (no CITARSV2 at its start)".to_owned(),
        });
    }
    let Some(n) = lead.get(8..12).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes) else {
        return Err(corrupt(path, 8, "cut short before its header's length"));
    };
    if n > MAX_HEADER {
        return Err(corrupt(path, 8, &format!("a header of {n} bytes is over the limit")));
    }
    if HEADER_AT + u64::from(n) > len {
        return Err(corrupt(path, HEADER_AT, "cut short in its header"));
    }
    Ok(n)
}

/// The header's JSON: another format or version is [`StoreError::Format`], anything else
/// wrong is [`StoreError::Corrupt`].
fn parse_header(path: &Path, bytes: &[u8]) -> Result<Header, StoreError> {
    let bad = |why: String| corrupt(path, HEADER_AT, &format!("its header: {why}"));
    let value: Value = serde_json::from_slice(bytes).map_err(|e| bad(e.to_string()))?;
    let (format, version) = (value.get("format"), value.get("version"));
    if format.and_then(Value::as_str) != Some(FORMAT) {
        return Err(StoreError::Format {
            path: path.to_owned(),
            why: format!("its header's format is {}, not {FORMAT:?}", shown(format)),
        });
    }
    if version.and_then(Value::as_u64) != Some(u64::from(VERSION)) {
        return Err(StoreError::Format {
            path: path.to_owned(),
            why: format!("a save of version {}; this build reads {VERSION}", shown(version)),
        });
    }
    let header: Header = serde_json::from_value(value).map_err(|e| bad(e.to_string()))?;
    match header.fault() {
        Some(why) => Err(bad(why)),
        None => Ok(header),
    }
}

/// A header value as the error shows it.
fn shown(v: Option<&Value>) -> String {
    v.map_or_else(|| "missing".to_owned(), Value::to_string)
}

fn corrupt(path: &Path, at: u64, why: &str) -> StoreError {
    StoreError::Corrupt { path: path.to_owned(), at, why: why.to_owned() }
}

/// The content of `frame`, which must be exactly one zstd frame that records its content size,
/// at most `max` bytes. Allocates what the frame's bytes can hold, never what a damaged size
/// claims.
pub(crate) fn decode_frame(frame: &[u8], max: u64) -> Result<Vec<u8>, String> {
    if !frame.starts_with(&ZSTD_MAGIC) {
        return Err("not a zstd frame".to_owned());
    }
    let size = zstd::zstd_safe::find_frame_compressed_size(frame)
        .map_err(|_| "a damaged or cut short zstd frame".to_owned())?;
    if size != frame.len() {
        return Err(format!("{} bytes after its zstd frame", frame.len().saturating_sub(size)));
    }
    let content = match zstd::zstd_safe::get_frame_content_size(frame) {
        Ok(Some(n)) if n <= max => n,
        Ok(Some(n)) => return Err(format!("{n} bytes is over the limit of {max}")),
        Ok(None) => return Err("a zstd frame without its content size".to_owned()),
        Err(_) => return Err("a damaged zstd frame header".to_owned()),
    };
    // Real JSON compresses some 10-20x; a frame whose content size says far more than its bytes
    // could hold is read into a buffer that grows as the content comes.
    let guess = content.min((frame.len() as u64).saturating_mul(64).saturating_add(1 << 16));
    let mut out = Vec::with_capacity(usize::try_from(guess).unwrap_or(0));
    let mut z = zstd::stream::read::Decoder::with_buffer(frame)
        .map_err(|e| format!("zstd: {e}"))?
        .single_frame();
    (&mut z)
        .take(content.saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|e| format!("zstd: {e}"))?;
    if out.len() as u64 != content {
        return Err(format!("{} bytes where the frame says {content}", out.len()));
    }
    Ok(out)
}

/// Where the parts of a body are.
struct Parts {
    session: Range<usize>,
    metrics: Range<usize>,
    state: Range<usize>,
    chain: Option<ChainRef>,
}

/// The body's shape: exactly the four keys, the first three objects.
fn body_parts(body: &[u8]) -> Result<Parts, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Doc<'a> {
        #[serde(borrow)]
        session: &'a RawValue,
        #[serde(borrow)]
        metrics: &'a RawValue,
        #[serde(borrow)]
        state: &'a RawValue,
        chain: Option<ChainRef>,
    }
    let doc: Doc<'_> = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let base = body.as_ptr().addr();
    let range = |raw: &RawValue, what: &str| {
        let s = raw.get();
        if !s.starts_with('{') {
            return Err(format!("its {what} is not a JSON object"));
        }
        // The raw value borrows from `body`, so its address is inside it.
        let start = s.as_ptr().addr().checked_sub(base).ok_or("a part outside the body")?;
        Ok(start..start + s.len())
    };
    if let Some(why) = doc.chain.as_ref().and_then(ChainRef::fault) {
        return Err(why);
    }
    Ok(Parts {
        session: range(doc.session, "session")?,
        metrics: range(doc.metrics, "metrics")?,
        state: range(doc.state, "state")?,
        chain: doc.chain,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_must_be_one_whole_frame_with_its_size() {
        let data = b"{\"a\": 1}".repeat(100);
        let one = zstd::bulk::compress(&data, LEVEL).expect("compresses");
        assert_eq!(decode_frame(&one, MAX_BODY).as_deref(), Ok(data.as_slice()));
        let mut two = one.clone();
        two.extend_from_slice(&one);
        assert!(decode_frame(&two, MAX_BODY).is_err(), "a second frame");
        let mut trailing = one.clone();
        trailing.push(0);
        assert!(decode_frame(&trailing, MAX_BODY).is_err(), "a byte after the frame");
        assert!(decode_frame(&one[..one.len() - 1], MAX_BODY).is_err(), "cut short");
        assert!(decode_frame(&one, 10).is_err(), "over the limit");
        let mut sizeless = Vec::new();
        let mut z = zstd::stream::write::Encoder::new(&mut sizeless, LEVEL).expect("encoder");
        z.include_contentsize(false).expect("set");
        z.write_all(&data).expect("writes");
        z.finish().expect("finishes");
        assert!(decode_frame(&sizeless, MAX_BODY).is_err(), "no content size");
        assert!(decode_frame(b"", MAX_BODY).is_err());
        assert!(decode_frame(&data, MAX_BODY).is_err(), "not zstd");
    }

    #[test]
    fn a_frame_that_lies_about_its_size_allocates_only_what_it_holds() {
        // A tiny frame that truly holds 200 MB of zeros: within the limit, so it decodes; the
        // buffer grows as the zeros come rather than being sized up front by the header.
        let zeros = vec![0u8; 1 << 20];
        let frame = zstd::bulk::compress(&zeros, LEVEL).expect("compresses");
        assert!(frame.len() < 1024);
        assert_eq!(decode_frame(&frame, MAX_BODY).map(|v| v.len()), Ok(1 << 20));
        assert!(decode_frame(&frame, (1 << 20) - 1).is_err());
    }

    #[test]
    fn the_body_s_parts_are_found_where_they_were_spliced() {
        let body = br#"{"session":{"id":"g"},"metrics":{},"state":{"format":"citar-state","x":[1,2]},"chain":null}"#;
        let p = body_parts(body).expect("a body");
        assert_eq!(&body[p.state.clone()], br#"{"format":"citar-state","x":[1,2]}"#);
        assert_eq!(&body[p.session.clone()], br#"{"id":"g"}"#);
        assert_eq!(&body[p.metrics.clone()], b"{}");
        assert!(p.chain.is_none());
        for bad in [
            &br#"{"session":{},"metrics":{},"state":[],"chain":null}"#[..],
            br#"{"session":{},"metrics":{},"state":{},"chain":null,"more":1}"#,
            br#"{"session":{},"metrics":{},"chain":null}"#,
            br#"{"session":{},"metrics":{},"state":{},"state":{},"chain":null}"#,
            br#"{"session":{},"metrics":{},"state":{},"chain":{"head":"ab","rounds":1}}"#,
            br#"{"session":{},"metrics":{},"state":{}"#,
            b"[]",
        ] {
            assert!(body_parts(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
    }
}
