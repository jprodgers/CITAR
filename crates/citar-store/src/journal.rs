//! The journal (DESIGN.md P2.5.2): an append-only file of the engine's journal chunks.
//!
//! ```text
//! "CITARJNL" | u16 LE version (1) | records...
//! record: u32 LE payload length | u32 LE seq | u8 kind (1 = engine journal chunk)
//!         | u8 codec (0 raw, 1 zstd) | [u8; 8] hash | payload
//! ```
//!
//! **The format, exactly.**
//! - The 8-byte magic `CITARJNL` and the version, 1, as a little-endian `u16`: 10 bytes.
//! - Then records, each an 18-byte head and its payload. Record `i` has `seq` `i`, kind 1 (one
//!   of the engine's journal chunks, `Game::take_journal_chunk`), and codec 0 (the chunk as it
//!   is) or 1 (one zstd frame, level 3, that records its content size; written only when it is
//!   smaller). A payload is at most [`MAX_CHUNK`] bytes, and so is a chunk.
//! - The hash is the first 8 bytes of blake3 over the previous record's hash (8 zero bytes
//!   before record 0), then the record's `seq` (`u32` LE), kind, codec and payload. Chaining
//!   makes a prefix's last hash, its [`JournalRef::head`], stand for the whole prefix, so a
//!   container naming `head` can only be read with the history it was saved with; covering the
//!   head's fields means every byte of a record is checked, not only the payload's.
//!
//! **Recovery.** [`JournalWriter::open`] scans the records and takes an exclusive OS lock,
//! refusing a file another writer holds.
//! - It truncates only an **incomplete tail**, what a crash mid-append leaves: a last record
//!   whose head is cut short, or whose length runs past the end of the file, or zeros to the
//!   end (a crash that grew the file without writing it). A record whose length runs past the
//!   end is not torn when the record can be seen to end earlier (its hash holds over a shorter
//!   payload that is followed by record `seq + 1`'s head, or by the end of the file): its length
//!   was damaged, which is corruption. A file shorter than the 10-byte header that is the start
//!   of one (a crash while it was created) is a fresh journal.
//! - A **complete record that fails its hash, breaks the sequence, or has an unknown kind or
//!   codec** is corruption (bit rot, a sync conflict), and so is a head that claims more than
//!   [`MAX_CHUNK`] bytes or a zero head followed by anything but zeros: no writer makes those.
//!   `open` then truncates nothing and refuses to append; [`Recovered`] says where the good
//!   prefix ends, from which the session forks.
//!
//! [`JournalWriter::append`] refuses a sequence number that is not the next, so records are
//! exactly the engine's chunks 0, 1, 2, ...; a `PermissionDenied` is tried again as a
//! container's rename is, and a failed append leaves the file as it was before it. A container
//! names its journal by [`JournalRef`]; [`read_upto`] reads exactly that prefix and refuses one
//! whose last record does not hash to its head. [`fork`] copies a prefix to a new file.
//! [`JournalWriter::truncate_to`] drops the records past one of its own prefixes, for a session
//! that continues the timeline from an older save (DESIGN.md P2.5.3).
//!
//! **Where it differs from DESIGN.md P2.5.2.** The design hashed the payload alone. The hash
//! here also covers the head's seq, kind and codec, so a damaged codec byte (which would make
//! a raw chunk read as zstd) is found at open like any other byte, and it is chained, so a head
//! names a whole history and not only its last chunk. The format is otherwise the design's.

use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::StoreError;
use crate::container::decode_frame;
use crate::disk::{self, Cleanup, Lock, Retry, io_err};

/// The first eight bytes of every journal.
pub const MAGIC: &[u8; 8] = b"CITARJNL";

/// The format's version, after the magic.
pub const VERSION: u16 = 1;

/// The file's header: the magic and the version.
const HEADER: [u8; 10] = *b"CITARJNL\x01\x00";

/// The header's length, where record 0 starts.
pub const HEADER_LEN: u64 = HEADER.len() as u64;

/// A record's head: length, seq, kind, codec and hash.
pub const RECORD_HEAD: u64 = 18;

/// The longest chunk, and so the longest payload: 256 MiB. A gargantuan game's chunk for a
/// round is some hundreds of kilobytes, and the one that carries a converted game's whole
/// history some tens of megabytes; the limit keeps a damaged length from asking for gigabytes.
pub const MAX_CHUNK: u64 = 256 << 20;

/// The zstd level of a compressed payload.
const LEVEL: i32 = 3;

/// Chunks shorter than this are written raw: a zstd frame's own bytes outweigh what it saves.
const COMPRESS_FROM: usize = 128;

/// How many places the check for a damaged length tries before it gives up and calls the
/// record corrupt rather than torn (truncating nothing).
const MAX_CANDIDATES: u32 = 1 << 20;

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
#[serde(deny_unknown_fields)]
pub struct JournalRef {
    /// The journal's file name, beside the container: a plain name, no folders.
    pub file: String,
    /// How many records the prefix holds.
    pub records: u32,
    /// Its length in bytes, magic and version included.
    pub bytes: u64,
    /// Its last record's hash, 16 lower-case hex digits (empty for no records). The hashes are
    /// chained, so it stands for the whole prefix.
    pub head: String,
}

impl JournalRef {
    /// What is wrong with the reference, if anything: a file name that is not a plain name, or
    /// counts that no journal could have.
    pub(crate) fn fault(&self) -> Option<String> {
        let f = &self.file;
        if f.is_empty()
            || f.len() > 255
            || f == "."
            || f == ".."
            || f.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
        {
            return Some(format!("{f:?} is not a plain file name"));
        }
        if self.records == 0 {
            if !self.head.is_empty() || self.bytes != HEADER_LEN {
                return Some("an empty prefix has no head and is the header's 10 bytes".to_owned());
            }
        } else if !disk::is_hex(&self.head, 16) {
            return Some(format!("head {:?} is not 16 lower-case hex digits", self.head));
        } else if self.bytes < HEADER_LEN + RECORD_HEAD * u64::from(self.records) {
            return Some(format!("{} records cannot fit in {} bytes", self.records, self.bytes));
        }
        None
    }
}

/// What [`JournalWriter::open`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recovered {
    /// The complete, good records.
    pub records: u32,
    /// Where they end.
    pub bytes: u64,
    /// The length of an incomplete tail it truncated, if there was one.
    pub torn: Option<u64>,
    /// Where corruption starts (the first bad record's first byte, which is where the good
    /// prefix ends), if a complete record failed its hash or its order: nothing was truncated
    /// and appends are refused.
    pub corrupt_at: Option<u64>,
}

/// The one writer of a journal file, holding its OS lock until it is dropped.
#[derive(Debug)]
pub struct JournalWriter {
    path: PathBuf,
    file: File,
    /// The records it holds, and where the next goes.
    tail: Tail,
    /// Where corruption starts, if open found it: appends are refused.
    corrupt: Option<u64>,
    retry: Retry,
}

/// Where a writer stands in its file: the records it holds, and each one's end and hash. The
/// file is never shorter than `bytes`, where the next record goes, and it holds nothing past
/// that unless `dirty`.
#[derive(Debug)]
struct Tail {
    /// The records, so the next append's sequence number.
    records: u32,
    /// Where they end.
    bytes: u64,
    /// The last record's hash (zeros before record 0).
    head: [u8; 8],
    /// Each record's end and hash, so a prefix can be checked and cut back to.
    ends: Vec<(u64, [u8; 8])>,
    /// Whether the file may hold bytes past `bytes`, which are cut before the next write: a write
    /// that failed part way, or a cut that failed.
    dirty: bool,
}

impl Tail {
    /// A journal with no records: its header alone.
    const fn empty() -> Self {
        Self { records: 0, bytes: HEADER_LEN, head: [0; 8], ends: Vec::new(), dirty: false }
    }

    /// Writes `record`, whose hash is `hash`, as the next record. On an error the tail and the
    /// file's first `bytes` are as they were, so the same record can be pushed again.
    fn push(
        &mut self,
        file: &mut impl Media,
        record: &[u8],
        hash: [u8; 8],
        retry: Retry,
    ) -> io::Result<()> {
        write_at(file, self.bytes, record, &mut self.dirty, retry)?;
        self.bytes += record.len() as u64;
        self.records += 1;
        self.head = hash;
        self.ends.push((self.bytes, hash));
        Ok(())
    }

    /// Cuts the file back to its first `k` records (at most the ones it holds) and syncs it.
    ///
    /// The tail moves to record `k` before the file is touched, and stays `dirty` until the cut
    /// is made. So when the cut fails, or its sync does, the next push makes the cut first: a
    /// record is never written past the end of the file, which would leave a gap of zeros that
    /// reads as corruption, and a container naming that record could never load.
    fn cut(&mut self, file: &mut impl Media, k: u32, retry: Retry) -> io::Result<()> {
        let k = k.min(self.records);
        let (end, head) = match k.checked_sub(1) {
            None => (HEADER_LEN, [0; 8]),
            Some(i) => self.ends.get(i as usize).copied().unwrap_or((self.bytes, self.head)),
        };
        self.ends.truncate(k as usize);
        self.records = k;
        self.bytes = end;
        self.head = head;
        self.dirty = true;
        disk::retry(retry, || file.set_len_to(end))?;
        self.dirty = false;
        disk::retry(retry, || file.sync_to_disk())
    }
}

impl JournalWriter {
    /// Opens (or creates) the journal at `path` for appending: takes the OS lock, scans its
    /// records, and truncates an incomplete tail. A journal with corruption opens (so the
    /// session can see where its good prefix ends) but refuses appends.
    ///
    /// # Errors
    /// [`StoreError::Locked`] when another writer holds the file; [`StoreError::Format`] when it
    /// is not a journal of this version; [`StoreError::Io`] when the file system refuses.
    pub fn open(path: &Path) -> Result<(Self, Recovered), StoreError> {
        let retry = Retry::DEFAULT;
        let io = |e| io_err(path, e);
        let mut file = disk::retry(retry, || {
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)
        })
        .map_err(io)?;
        disk::lock(&file, path, Lock::Exclusive)?;
        let len = file.metadata().map_err(io)?.len();

        if len < HEADER_LEN {
            // A fresh file, or one whose creation a crash cut short.
            let mut lead = Vec::new();
            (&mut file).take(HEADER_LEN).read_to_end(&mut lead).map_err(io)?;
            if !HEADER.starts_with(&lead) {
                return Err(not_a_journal(path, &lead));
            }
            disk::retry(retry, || {
                file.set_len(0)?;
                file.seek(SeekFrom::Start(0))?;
                file.write_all(&HEADER)?;
                file.sync_all()
            })
            .map_err(io)?;
            disk::sync_parent(path);
            let writer =
                Self { path: path.to_owned(), file, tail: Tail::empty(), corrupt: None, retry };
            let torn = (len > 0).then_some(len);
            return Ok((
                writer,
                Recovered { records: 0, bytes: HEADER_LEN, torn, corrupt_at: None },
            ));
        }

        check_header(path, &mut file)?;
        let mut ends = Vec::new();
        let scan = scan(&mut file, len, u32::MAX, |_, end, h, _| {
            ends.push((end, h.hash));
            Ok(())
        })
        .map_err(io)?;
        let (torn, corrupt) = match scan.end {
            End::Clean => (None, None),
            End::Torn(n) => {
                disk::retry(retry, || {
                    file.set_len(scan.bytes)?;
                    file.sync_data()
                })
                .map_err(io)?;
                (Some(n), None)
            }
            End::Corrupt { at, .. } => (None, Some(at)),
        };
        file.seek(SeekFrom::Start(scan.bytes)).map_err(io)?;
        let recovered =
            Recovered { records: scan.records, bytes: scan.bytes, torn, corrupt_at: corrupt };
        let tail =
            Tail { records: scan.records, bytes: scan.bytes, head: scan.head, ends, dirty: false };
        let writer = Self { path: path.to_owned(), file, tail, corrupt, retry };
        Ok((writer, recovered))
    }

    /// Appends chunk `seq`, which must be the next. It is not on the disk until [`sync`]: the
    /// session appends what is pending, syncs, then writes the container that names it.
    ///
    /// [`sync`]: Self::sync
    ///
    /// # Errors
    /// [`StoreError::OutOfOrder`] for a sequence number that is not the next;
    /// [`StoreError::Corrupt`] when open found corruption; [`StoreError::Invalid`] for a chunk
    /// longer than [`MAX_CHUNK`]; [`StoreError::Io`] when the file system refuses past the
    /// retries, the file then as it was before the call and the chunk still the next to append.
    pub fn append(&mut self, seq: u32, chunk: &[u8]) -> Result<(), StoreError> {
        if let Some(at) = self.corrupt {
            return Err(StoreError::Corrupt {
                path: self.path.clone(),
                at,
                why: "appends are refused after corruption: fork the good prefix".to_owned(),
            });
        }
        if seq != self.tail.records {
            return Err(StoreError::OutOfOrder { want: self.tail.records, got: seq });
        }
        if seq == u32::MAX {
            return Err(StoreError::Invalid {
                path: self.path.clone(),
                why: "the journal holds as many records as a sequence number counts".to_owned(),
            });
        }
        if chunk.len() as u64 > MAX_CHUNK {
            return Err(StoreError::Invalid {
                path: self.path.clone(),
                why: format!("a chunk of {} bytes is over the limit of {MAX_CHUNK}", chunk.len()),
            });
        }
        let (record, hash) = encode(&self.tail.head, seq, chunk);
        self.tail.push(&mut self.file, &record, hash, self.retry).map_err(|e| io_err(&self.path, e))
    }

    /// Flushes what was appended to the disk (`File::sync_data`).
    ///
    /// # Errors
    /// The file system refuses.
    pub fn sync(&mut self) -> Result<(), StoreError> {
        self.file.sync_data().map_err(|e| io_err(&self.path, e))
    }

    /// Truncates the journal to the prefix `r`, which must be one of its own: the records a
    /// session appended for a save that never completed, dropped when an older save continues
    /// the timeline (DESIGN.md P2.5.3). Synced before it returns.
    ///
    /// # Errors
    /// [`StoreError::Mismatch`] when `r` is not a prefix of this journal (another count, end or
    /// head), and nothing changes; [`StoreError::Corrupt`] when open found corruption (fork
    /// instead); [`StoreError::Io`] when the file system refuses past the retries. The writer
    /// then stands at `r` all the same (its [`reference`](Self::reference) is `r`, and the next
    /// append is record `r.records`): the cut is made again before that append writes, so no
    /// record is ever written past the end of the file.
    pub fn truncate_to(&mut self, r: &JournalRef) -> Result<(), StoreError> {
        if let Some(at) = self.corrupt {
            return Err(StoreError::Corrupt {
                path: self.path.clone(),
                at,
                why: "a corrupt journal is forked, not truncated".to_owned(),
            });
        }
        let mismatch = |why: String| StoreError::Mismatch { path: self.path.clone(), why };
        if let Some(why) = r.fault() {
            return Err(mismatch(why));
        }
        let (end, head) = match r.records {
            0 => (HEADER_LEN, [0; 8]),
            n => *self.tail.ends.get(n as usize - 1).ok_or_else(|| {
                mismatch(format!("the journal holds {} records, not {n}", self.tail.records))
            })?,
        };
        if end != r.bytes {
            return Err(mismatch(format!(
                "record {} ends at byte {end}, not {}",
                r.records.saturating_sub(1),
                r.bytes
            )));
        }
        if r.records > 0 && disk::hex(&head) != r.head {
            return Err(mismatch(format!(
                "record {} hashes to {}, not {}: another timeline",
                r.records - 1,
                disk::hex(&head),
                r.head
            )));
        }
        self.tail.cut(&mut self.file, r.records, self.retry).map_err(|e| io_err(&self.path, e))
    }

    /// The prefix written so far, for the container that names it (with corruption, the good
    /// prefix, from which the session forks).
    #[must_use]
    pub fn reference(&self) -> JournalRef {
        let file =
            self.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let t = &self.tail;
        let head = if t.records == 0 { String::new() } else { disk::hex(&t.head) };
        JournalRef { file, records: t.records, bytes: t.bytes, head }
    }

    /// The journal's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Record `seq` holding `chunk`, after the record whose hash is `prev`: its bytes and its hash.
/// The chunk is stored as zstd when it is long enough to gain and the frame is smaller.
fn encode(prev: &[u8; 8], seq: u32, chunk: &[u8]) -> (Vec<u8>, [u8; 8]) {
    let packed = (chunk.len() >= COMPRESS_FROM)
        .then(|| zstd::bulk::compress(chunk, LEVEL).ok())
        .flatten()
        .filter(|z| z.len() < chunk.len());
    let (codec, payload) = match &packed {
        Some(z) => (Codec::Zstd, z.as_slice()),
        None => (Codec::Raw, chunk),
    };
    let kind = RecordKind::EngineChunk as u8;
    let hash = record_hash(prev, seq, kind, codec as u8, payload);
    let mut record = Vec::with_capacity(RECORD_HEAD as usize + payload.len());
    let len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    record.extend_from_slice(&len.to_le_bytes());
    record.extend_from_slice(&seq.to_le_bytes());
    record.push(kind);
    record.push(codec as u8);
    record.extend_from_slice(&hash);
    record.extend_from_slice(payload);
    (record, hash)
}

/// Writes `record` at byte `at` of `file`, tried again on a [`disk::transient`] error. A write
/// that fails part way leaves `dirty` set and is cut back to `at` before the next try (and,
/// best effort, before returning), so a retried or later append never follows half a record.
/// A `dirty` file is cut back to `at` before the first try too.
fn write_at(
    file: &mut impl Media,
    at: u64,
    record: &[u8],
    dirty: &mut bool,
    retry: Retry,
) -> io::Result<()> {
    let done = disk::retry(retry, || {
        if *dirty {
            file.set_len_to(at)?;
            *dirty = false;
        }
        file.seek(SeekFrom::Start(at))?;
        *dirty = true;
        file.write_all(record)?;
        *dirty = false;
        Ok(())
    });
    if done.is_err() && *dirty && file.set_len_to(at).is_ok() {
        *dirty = false;
    }
    done
}

/// What a writer needs of its file: a [`File`], or a test's stand-in that fails on cue.
trait Media: Write + Seek {
    /// Sets the file's length (`File::set_len`).
    fn set_len_to(&mut self, len: u64) -> io::Result<()>;
    /// Flushes the file's bytes to the disk (`File::sync_data`).
    fn sync_to_disk(&mut self) -> io::Result<()>;
}

impl Media for File {
    fn set_len_to(&mut self, len: u64) -> io::Result<()> {
        self.set_len(len)
    }

    fn sync_to_disk(&mut self) -> io::Result<()> {
        self.sync_data()
    }
}

/// The payloads of the prefix `r` of the journal at `path`, in order, each as the chunk that
/// was appended (a zstd payload decompressed).
///
/// # Errors
/// [`StoreError::Mismatch`] when the prefix runs past the good records, ends elsewhere, or its
/// last record does not hash to its head (or `r` is not a prefix any journal has);
/// [`StoreError::Corrupt`] when a record inside it fails its check; [`StoreError::Locked`]
/// while a writer holds the file; [`StoreError::Format`] and [`StoreError::Io`] as for
/// [`JournalWriter::open`].
pub fn read_upto(path: &Path, r: &JournalRef) -> Result<Vec<Vec<u8>>, StoreError> {
    let mut out = Vec::with_capacity(r.records.min(1 << 16) as usize);
    let mut file = open_prefix(path, r)?;
    let len = file.metadata().map_err(|e| io_err(path, e))?.len();
    let scan = scan(&mut file, len, r.records, |_, _, h, payload| {
        out.push(match h.codec {
            0 => payload.to_vec(),
            _ => decode_frame(payload, MAX_CHUNK)?,
        });
        Ok(())
    })
    .map_err(|e| io_err(path, e))?;
    check_prefix(path, r, &scan)?;
    Ok(out)
}

/// Copies the prefix `r` of the journal at `path` to a new journal at `new`, which must not
/// exist yet, and syncs it.
///
/// # Errors
/// As [`read_upto`] for the prefix; [`StoreError::Io`] when `new` exists or cannot be written
/// (a partial copy is removed).
pub fn fork(path: &Path, r: &JournalRef, new: &Path) -> Result<(), StoreError> {
    let mut file = open_prefix(path, r)?;
    let len = file.metadata().map_err(|e| io_err(path, e))?.len();
    let scan = scan(&mut file, len, r.records, |_, _, _, _| Ok(())).map_err(|e| io_err(path, e))?;
    check_prefix(path, r, &scan)?;
    let out =
        OpenOptions::new().write(true).create_new(true).open(new).map_err(|e| io_err(new, e))?;
    let cleanup = Cleanup::new(new);
    file.seek(SeekFrom::Start(0)).map_err(|e| io_err(path, e))?;
    let mut w = BufWriter::with_capacity(1 << 16, out);
    let copied = io::copy(&mut (&mut file).take(r.bytes), &mut w).map_err(|e| io_err(new, e))?;
    if copied != r.bytes {
        return Err(StoreError::Mismatch {
            path: path.to_owned(),
            why: format!("the journal shrank while it was copied: {copied} of {} bytes", r.bytes),
        });
    }
    let out = w.into_inner().map_err(|e| io_err(new, e.into_error()))?;
    out.sync_all().map_err(|e| io_err(new, e))?;
    drop(out);
    cleanup.keep();
    disk::sync_parent(new);
    Ok(())
}

/// The journal at `path` opened for reading the prefix `r`, under a shared lock, its header
/// checked.
fn open_prefix(path: &Path, r: &JournalRef) -> Result<File, StoreError> {
    if let Some(why) = r.fault() {
        return Err(StoreError::Mismatch { path: path.to_owned(), why });
    }
    let mut file = File::open(path).map_err(|e| io_err(path, e))?;
    disk::lock(&file, path, Lock::Shared)?;
    check_header(path, &mut file)?;
    Ok(file)
}

/// Whether a scan limited to `r.records` found exactly the prefix `r`.
fn check_prefix(path: &Path, r: &JournalRef, scan: &Scan) -> Result<(), StoreError> {
    let mismatch = |why: String| StoreError::Mismatch { path: path.to_owned(), why };
    if scan.records < r.records {
        return Err(match &scan.end {
            End::Corrupt { at, why } => {
                StoreError::Corrupt { path: path.to_owned(), at: *at, why: why.clone() }
            }
            End::Clean | End::Torn(_) => mismatch(format!(
                "the journal holds {} good records; the save names {}",
                scan.records, r.records
            )),
        });
    }
    if scan.bytes != r.bytes {
        return Err(mismatch(format!(
            "its first {} records end at byte {}, not {}",
            r.records, scan.bytes, r.bytes
        )));
    }
    if r.records > 0 && disk::hex(&scan.head) != r.head {
        return Err(mismatch(format!(
            "record {} hashes to {}, not the save's {}: the journal was rewritten, forked or \
             swapped",
            r.records - 1,
            disk::hex(&scan.head),
            r.head
        )));
    }
    Ok(())
}

/// Reads and checks a journal's 10-byte header from the start of `file`.
fn check_header(path: &Path, file: &mut File) -> Result<(), StoreError> {
    let mut lead = Vec::with_capacity(HEADER.len());
    file.seek(SeekFrom::Start(0)).map_err(|e| io_err(path, e))?;
    file.take(HEADER_LEN).read_to_end(&mut lead).map_err(|e| io_err(path, e))?;
    if lead.as_slice() == HEADER {
        return Ok(());
    }
    if lead.starts_with(MAGIC) && lead.len() == HEADER.len() {
        let v = u16::from_le_bytes([lead[8], lead[9]]);
        return Err(StoreError::Format {
            path: path.to_owned(),
            why: format!("a journal of version {v}; this build reads {VERSION}"),
        });
    }
    Err(not_a_journal(path, &lead))
}

fn not_a_journal(path: &Path, lead: &[u8]) -> StoreError {
    let why = if HEADER.starts_with(lead) {
        "cut short in its header".to_owned()
    } else {
        "not a CITAR journal (no CITARJNL at its start)".to_owned()
    };
    StoreError::Format { path: path.to_owned(), why }
}

/// A record's head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Head {
    len: u32,
    seq: u32,
    kind: u8,
    codec: u8,
    hash: [u8; 8],
}

impl Head {
    fn parse(b: &[u8; RECORD_HEAD as usize]) -> Self {
        let word = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        let mut hash = [0; 8];
        hash.copy_from_slice(&b[10..18]);
        Self { len: word(0), seq: word(4), kind: b[8], codec: b[9], hash }
    }
}

/// A record's hash: blake3 over the previous record's hash, its seq, kind and codec, and its
/// payload, cut to 8 bytes.
fn record_hash(prev: &[u8; 8], seq: u32, kind: u8, codec: u8, payload: &[u8]) -> [u8; 8] {
    let mut h = hasher(prev, seq, kind, codec);
    h.update(payload);
    first8(&h)
}

/// The hash of a record so far, before its payload.
fn hasher(prev: &[u8; 8], seq: u32, kind: u8, codec: u8) -> blake3::Hasher {
    let mut h = blake3::Hasher::new();
    h.update(prev);
    h.update(&seq.to_le_bytes());
    h.update(&[kind, codec]);
    h
}

fn first8(h: &blake3::Hasher) -> [u8; 8] {
    let mut out = [0; 8];
    out.copy_from_slice(&h.finalize().as_bytes()[..8]);
    out
}

/// How a scan ended.
#[derive(Clone, Debug, PartialEq, Eq)]
enum End {
    /// At the end of the file, or at the limit it was given.
    Clean,
    /// At an incomplete tail of this many bytes.
    Torn(u64),
    /// At a complete record that fails its check, starting at byte `at`.
    Corrupt { at: u64, why: String },
}

/// What a scan found: the good prefix and how it ends.
#[derive(Clone, Debug)]
struct Scan {
    records: u32,
    bytes: u64,
    head: [u8; 8],
    end: End,
}

/// Scans the records of a journal of `len` bytes from `src` (whose header was checked), at most
/// `limit` of them, calling `visit(index, end, head, payload)` for each good one; an `Err` from
/// `visit` makes that record corrupt. Never allocates more than the longest record the file
/// holds.
fn scan<R: Read + Seek>(
    src: &mut R,
    len: u64,
    limit: u32,
    mut visit: impl FnMut(u32, u64, &Head, &[u8]) -> Result<(), String>,
) -> io::Result<Scan> {
    src.seek(SeekFrom::Start(HEADER_LEN))?;
    let mut rd = BufReader::with_capacity(1 << 16, src);
    let mut pos = HEADER_LEN;
    let mut prev = [0u8; 8];
    let mut n = 0u32;
    let mut payload = Vec::new();
    let at = |records, bytes, head, end| Scan { records, bytes, head, end };
    loop {
        if n == limit || pos == len {
            return Ok(at(n, pos, prev, End::Clean));
        }
        let rest = len.saturating_sub(pos);
        if rest < RECORD_HEAD {
            return Ok(at(n, pos, prev, End::Torn(rest)));
        }
        let mut raw = [0u8; RECORD_HEAD as usize];
        rd.read_exact(&mut raw)?;
        let h = Head::parse(&raw);
        let corrupt = |why: String| Ok(at(n, pos, prev, End::Corrupt { at: pos, why }));
        if raw.iter().all(|&b| b == 0) {
            // A crash that grew the file without writing it leaves zeros to its end. No record
            // is all zeros (its kind is 1), so a zero head that is not followed by zeros alone
            // is corruption.
            if zeros_to_end(&mut rd, rest - RECORD_HEAD)? {
                return Ok(at(n, pos, prev, End::Torn(rest)));
            }
            return corrupt(format!("zeros where record {n}'s head belongs"));
        }
        if h.kind != RecordKind::EngineChunk as u8 {
            return corrupt(format!("record {n} has kind {}, not 1", h.kind));
        }
        if h.codec > Codec::Zstd as u8 {
            return corrupt(format!("record {n} has codec {}, not 0 or 1", h.codec));
        }
        if h.seq != n {
            return corrupt(format!("record {n} says it is record {}: out of order", h.seq));
        }
        if u64::from(h.len) > MAX_CHUNK {
            return corrupt(format!("record {n} is {} bytes, longer than any record", h.len));
        }
        let end = pos + RECORD_HEAD + u64::from(h.len);
        if end > len {
            let start = pos + RECORD_HEAD;
            let src = rd.get_mut();
            return match ends_before(src, start, len, &prev, &h, n)? {
                Some(false) => Ok(at(n, pos, prev, End::Torn(rest))),
                Some(true) => corrupt(format!(
                    "record {n}'s length runs past the end of the file, but the record ends \
                     before it: its length is damaged"
                )),
                None => corrupt(format!(
                    "record {n}'s length runs past the end of the file, and whether it is torn \
                     cannot be told"
                )),
            };
        }
        payload.clear();
        payload.resize(h.len as usize, 0);
        rd.read_exact(&mut payload)?;
        if record_hash(&prev, h.seq, h.kind, h.codec, &payload) != h.hash {
            return corrupt(format!("record {n} fails its hash"));
        }
        if let Err(why) = visit(n, end, &h, &payload) {
            return corrupt(format!("record {n}: {why}"));
        }
        prev = h.hash;
        pos = end;
        n += 1;
    }
}

/// Whether the next `n` bytes of `rd` are all zeros.
fn zeros_to_end(rd: &mut impl Read, n: u64) -> io::Result<bool> {
    let mut buf = [0u8; 1 << 13];
    let mut left = n;
    while left > 0 {
        let want = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
        let got = rd.read(&mut buf[..want])?;
        if got == 0 {
            return Ok(false);
        }
        if buf[..got].iter().any(|&b| b != 0) {
            return Ok(false);
        }
        left -= got as u64;
    }
    Ok(true)
}

/// For a record whose length runs past the end of a file of `len` bytes, its payload starting at
/// `start`: whether the record can be seen to end before the end of the file, which means its
/// length was damaged, not cut short by a crash. It ends at `p` when the bytes from `start` to
/// `p` hash to its hash and record `n + 1`'s head starts at `p` (seq, kind and codec), or `p` is
/// the end of the file. `None` when too many places look like a next head to try them all.
fn ends_before<R: Read + Seek>(
    src: &mut R,
    start: u64,
    len: u64,
    prev: &[u8; 8],
    h: &Head,
    n: u32,
) -> io::Result<Option<bool>> {
    src.seek(SeekFrom::Start(start))?;
    let mut rest = src.take(len.saturating_sub(start));
    let mut hasher = hasher(prev, h.seq, h.kind, h.codec);
    let next = n.checked_add(1).map(u32::to_le_bytes);
    // `win` holds bytes read but not yet dropped; those before `hashed` are in the hasher. Every
    // place before `look` has been looked at for record n + 1's head, so the bytes before it can
    // be hashed and dropped whenever the window is trimmed: a later place hashes past them.
    let mut win: Vec<u8> = Vec::new();
    let (mut hashed, mut look) = (0usize, 0usize);
    let mut tried = 0u32;
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let got = rest.read(&mut buf)?;
        win.extend_from_slice(&buf[..got]);
        if let Some(next) = next {
            // A head's seq, kind and codec are its bytes 4 to 9: a place needs ten bytes.
            while look + 10 <= win.len() {
                let w = &win[look..look + 10];
                if w[4..8] == next
                    && w[8] == RecordKind::EngineChunk as u8
                    && w[9] <= Codec::Zstd as u8
                {
                    tried += 1;
                    if tried > MAX_CANDIDATES {
                        return Ok(None);
                    }
                    hasher.update(&win[hashed..look]);
                    hashed = look;
                    if first8(&hasher) == h.hash {
                        return Ok(Some(true));
                    }
                }
                look += 1;
            }
        }
        if got == 0 {
            break;
        }
        // Keep only the few bytes a head may still start in (all of them hashed past, when no
        // record can follow this one).
        let cut = if next.is_some() { look } else { win.len() };
        hasher.update(&win[hashed..cut]);
        win.drain(..cut);
        hashed = 0;
        look -= cut.min(look);
    }
    // The record ends with the file.
    hasher.update(&win[hashed..]);
    Ok(Some(first8(&hasher) == h.hash))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// A journal's bytes with these chunks, as `append` writes them.
    fn journal(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut out = HEADER.to_vec();
        let mut prev = [0u8; 8];
        for (i, c) in chunks.iter().enumerate() {
            let seq = i as u32;
            let hash = record_hash(&prev, seq, 1, 0, c);
            out.extend_from_slice(&(c.len() as u32).to_le_bytes());
            out.extend_from_slice(&seq.to_le_bytes());
            out.extend_from_slice(&[1, 0]);
            out.extend_from_slice(&hash);
            out.extend_from_slice(c);
            prev = hash;
        }
        out
    }

    fn scanned(bytes: &[u8]) -> Scan {
        let mut c = Cursor::new(bytes);
        scan(&mut c, bytes.len() as u64, u32::MAX, |_, _, _, _| Ok(())).expect("in memory")
    }

    fn chunks() -> Vec<Vec<u8>> {
        (0..6u8).map(|i| (0..u32::from(i) * 7 + 3).map(|j| (j as u8) ^ i).collect()).collect()
    }

    #[test]
    fn a_whole_journal_scans_clean() {
        let b = journal(&chunks());
        let s = scanned(&b);
        assert_eq!((s.records, s.bytes, s.end), (6, b.len() as u64, End::Clean));
    }

    /// Every single-byte change of every byte inside a complete record, all 255 of them, in
    /// memory: corruption at that record, never a torn tail (the file-level test tries fewer).
    #[test]
    fn every_change_of_every_byte_is_corruption_at_its_record() {
        let cs = chunks();
        let b = journal(&cs);
        let mut starts = vec![HEADER_LEN];
        for c in &cs {
            let last = *starts.last().unwrap_or(&0);
            starts.push(last + RECORD_HEAD + c.len() as u64);
        }
        for p in HEADER_LEN as usize..b.len() {
            let k = starts.iter().rposition(|&s| s <= p as u64).unwrap_or(0);
            for v in 1..=255u8 {
                let mut bad = b.clone();
                bad[p] ^= v;
                let s = scanned(&bad);
                assert_eq!(s.records, k as u32, "byte {p} ^ {v}");
                assert_eq!(s.bytes, starts[k], "byte {p} ^ {v}");
                assert!(
                    matches!(s.end, End::Corrupt { at, .. } if at == starts[k]),
                    "byte {p} ^ {v}: {:?}",
                    s.end
                );
            }
        }
    }

    #[test]
    fn a_cut_short_journal_is_torn_at_its_last_whole_record() {
        let cs = chunks();
        let b = journal(&cs);
        let mut ends = vec![HEADER_LEN];
        for c in &cs {
            let last = *ends.last().unwrap_or(&0);
            ends.push(last + RECORD_HEAD + c.len() as u64);
        }
        for t in HEADER_LEN as usize..=b.len() {
            let s = scanned(&b[..t]);
            let k = ends.iter().rposition(|&e| e <= t as u64).unwrap_or(0);
            assert_eq!((s.records, s.bytes), (k as u32, ends[k]), "cut at {t}");
            let want = if ends[k] == t as u64 { End::Clean } else { End::Torn(t as u64 - ends[k]) };
            assert_eq!(s.end, want, "cut at {t}");
        }
    }

    #[test]
    fn zeros_after_the_last_record_are_a_torn_tail() {
        let mut b = journal(&chunks());
        let whole = b.len() as u64;
        b.extend_from_slice(&[0; 4096]);
        let s = scanned(&b);
        assert_eq!((s.records, s.bytes, s.end), (6, whole, End::Torn(4096)));
        // Not if anything but zeros follows.
        b.push(1);
        assert!(matches!(scanned(&b).end, End::Corrupt { .. }));
    }

    #[test]
    fn a_long_damaged_length_is_found_far_from_the_end() {
        // Record 1 of 6 claims to run past the end; record 2's head shows where it really ends.
        let cs: Vec<Vec<u8>> = (0..6).map(|i| vec![b'a' + i as u8; 3000]).collect();
        let mut b = journal(&cs);
        let at = HEADER_LEN as usize + RECORD_HEAD as usize + 3000;
        b[at + 2] = 0x7f;
        let s = scanned(&b);
        assert_eq!(s.records, 1);
        assert!(matches!(s.end, End::Corrupt { at: a, .. } if a == at as u64), "{:?}", s.end);
    }

    #[test]
    fn a_writer_that_fails_part_way_leaves_nothing_behind() {
        /// A file whose next `fails` writes each take `ok` bytes and then refuse the rest.
        struct Flaky {
            data: Vec<u8>,
            pos: u64,
            fails: u32,
            ok: usize,
            half: bool,
        }
        impl Write for Flaky {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                if self.fails > 0 {
                    if self.half {
                        self.half = false;
                        self.fails -= 1;
                        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
                    }
                    // Part of a record lands before the refusal.
                    let n = buf.len().min(self.ok);
                    let at = self.pos as usize;
                    self.data.truncate(at);
                    self.data.extend_from_slice(&buf[..n]);
                    self.pos += n as u64;
                    self.half = true;
                    return Ok(n);
                }
                let at = self.pos as usize;
                self.data.truncate(at);
                self.data.extend_from_slice(buf);
                self.pos += buf.len() as u64;
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl Seek for Flaky {
            fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
                if let SeekFrom::Start(p) = to {
                    self.pos = p;
                }
                Ok(self.pos)
            }
        }
        impl Media for Flaky {
            fn set_len_to(&mut self, len: u64) -> io::Result<()> {
                self.data.truncate(len as usize);
                Ok(())
            }
            fn sync_to_disk(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let fast = Retry { tries: 4, wait: std::time::Duration::from_millis(1) };
        // Two partial writes, each followed by a refusal, then it goes through.
        let mut f = Flaky { data: b"0123456789".to_vec(), pos: 10, fails: 2, ok: 3, half: false };
        let mut dirty = false;
        let got = write_at(&mut f, 10, b"abcdefgh", &mut dirty, fast);
        assert!(got.is_ok(), "{got:?}");
        assert_eq!(f.data, b"0123456789abcdefgh");
        assert!(!dirty);
        // It never goes through: the file is as it was.
        let mut f = Flaky { data: b"0123456789".to_vec(), pos: 10, fails: 100, ok: 3, half: false };
        let got = write_at(&mut f, 10, b"abcdefgh", &mut dirty, fast);
        assert!(got.is_err());
        assert_eq!(f.data, b"0123456789");
        assert!(!dirty);
    }

    /// A file in memory, as a [`File`] behaves (a write past its end fills the gap with zeros),
    /// whose next length change or sync can be made to fail.
    struct Mem {
        data: Vec<u8>,
        pos: u64,
        /// The next `set_len_to` fails: before the length changes (`Some(false)`) or after it
        /// (`Some(true)`).
        fail_set_len: Option<bool>,
        /// The next `sync_to_disk` fails.
        fail_sync: bool,
    }

    impl Write for Mem {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let at = self.pos as usize;
            let end = at + buf.len();
            if self.data.len() < end {
                self.data.resize(end, 0);
            }
            self.data[at..end].copy_from_slice(buf);
            self.pos = end as u64;
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Seek for Mem {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            if let SeekFrom::Start(p) = to {
                self.pos = p;
            }
            Ok(self.pos)
        }
    }

    impl Media for Mem {
        fn set_len_to(&mut self, len: u64) -> io::Result<()> {
            match self.fail_set_len.take() {
                Some(false) => Err(io::Error::other("the length is not changed")),
                Some(true) => {
                    self.data.resize(len as usize, 0);
                    Err(io::Error::other("the length changed, then an error"))
                }
                None => {
                    self.data.resize(len as usize, 0);
                    Ok(())
                }
            }
        }
        fn sync_to_disk(&mut self) -> io::Result<()> {
            if std::mem::take(&mut self.fail_sync) {
                return Err(io::Error::other("the sync fails"));
            }
            Ok(())
        }
    }

    /// The tail a writer opening `bytes` (a clean journal) stands at.
    fn tail_of(bytes: &[u8]) -> Tail {
        let mut ends = Vec::new();
        let mut c = Cursor::new(bytes);
        let s = scan(&mut c, bytes.len() as u64, u32::MAX, |_, end, h, _| {
            ends.push((end, h.hash));
            Ok(())
        })
        .expect("in memory");
        assert_eq!(s.end, End::Clean);
        Tail { records: s.records, bytes: s.bytes, head: s.head, ends, dirty: false }
    }

    /// `truncate_to` cuts through a [`Tail`]. However the cut fails (its length change refused,
    /// or made and then reported as an error, or its sync refused), the record appended next
    /// follows the kept records directly: never past the end of the file with zeros between,
    /// which would read as corruption at that record.
    #[test]
    fn a_cut_that_fails_never_leads_to_a_write_past_the_end() {
        let cs = chunks();
        let b = journal(&cs);
        let fast = Retry { tries: 3, wait: std::time::Duration::from_millis(1) };
        let kept = tail_of(&b).ends[2].0 as usize;
        for (how, set_len, sync) in [
            ("its length change is refused", Some(false), false),
            ("its length changes, then an error", Some(true), false),
            ("its sync is refused", None, true),
            ("nothing fails", None, false),
        ] {
            let mut tail = tail_of(&b);
            let mut f = Mem { data: b.clone(), pos: 0, fail_set_len: set_len, fail_sync: sync };
            let got = tail.cut(&mut f, 3, fast);
            assert_eq!(got.is_err(), how != "nothing fails", "{how}");
            assert_eq!((tail.records, tail.bytes), (3, kept as u64), "{how}: it stands at 3");
            let (record, hash) = encode(&tail.head, 3, b"after the cut");
            tail.push(&mut f, &record, hash, fast).expect(how);
            assert_eq!(&f.data[..kept], &b[..kept], "{how}: the kept records are untouched");
            assert_eq!(&f.data[kept..], record.as_slice(), "{how}: the new record follows them");
            let s = scanned(&f.data);
            assert_eq!((s.records, s.bytes, s.end), (4, f.data.len() as u64, End::Clean), "{how}");
            assert_eq!((tail.records, tail.bytes, tail.dirty), (4, s.bytes, false), "{how}");
            assert_eq!(tail.head, s.head, "{how}");
        }
    }

    #[test]
    fn references_that_no_journal_has_are_faults() {
        let ok =
            JournalRef { file: "journal.cjnl".into(), records: 2, bytes: 60, head: "0".repeat(16) };
        assert_eq!(ok.fault(), None);
        let empty =
            JournalRef { file: "journal.cjnl".into(), records: 0, bytes: 10, head: String::new() };
        assert_eq!(empty.fault(), None);
        for bad in [
            JournalRef { file: "../journal.cjnl".into(), ..ok.clone() },
            JournalRef { file: "a\\b".into(), ..ok.clone() },
            JournalRef { file: "c:x".into(), ..ok.clone() },
            JournalRef { file: String::new(), ..ok.clone() },
            JournalRef { file: "..".into(), ..ok.clone() },
            JournalRef { head: "0".repeat(15), ..ok.clone() },
            JournalRef { head: "A".repeat(16), ..ok.clone() },
            JournalRef { bytes: 40, ..ok.clone() },
            JournalRef { head: "0".repeat(16), ..empty.clone() },
            JournalRef { bytes: 11, ..empty.clone() },
        ] {
            assert!(bad.fault().is_some(), "{bad:?}");
        }
    }
}
