//! Saves v2 for the server (DESIGN.md P2.5, P2.5.3; package 2-11): the session's journal, the
//! snapshot it takes under its lock, and the container written off it. Replaces the session's
//! gzip JSON save (`citar/server/session.py`, `save` and `to_save`: the whole history serialised
//! and written under the lock).
//!
//! - **`Journal`**: one journal file of a session's timeline, open for appending (citar-store's
//!   `JournalWriter`, holding its OS lock), and the chunks taken from the game but not yet on
//!   the disk. `Game.save_snapshot(journal)` takes a chunk under the game's lock and queues it
//!   here at once, so no chunk is ever lost: a snapshot dropped unwritten (an autosave a newer
//!   one replaced) or a write that failed leaves its chunk queued, and the next write appends it,
//!   in order. A journal with no records and none queued is a new one: the first snapshot starts
//!   the game's journal over (`Game::restart_journal`), so its first chunk holds the whole
//!   history the game keeps.
//! - **`SaveSnapshot`**: the engine's `Snapshot` and how many chunks its state counts. `write`
//!   (the GIL released) appends the queued chunks up to that count and syncs them, then writes
//!   the state's JSON, zstd and the container (a temporary file and a rename) naming exactly
//!   that prefix of the journal. The chunks are on the disk before the container that names
//!   them, so no container ever names a record that is not.
//! - **`Save`**: a container read back (`read_save`), whose state `Game.load_save` loads with the
//!   records its journal prefix holds; `save_header` reads a header alone, never the body.
//!
//! Locks: a game's, then a journal's queue; a journal's file, then its queue. The queue's lock is
//! held only to push, look at or pop a chunk, so a snapshot under the game's lock never waits for
//! a write, which holds the file's.
//!
//! Errors: reading a save or its history is `LoadError`; a write that fails is `OSError`, and the
//! old save stays.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use citar_engine::save::Snapshot;
use citar_engine::save::journal::JournalChunk;
use citar_store::container::{FORMAT, VERSION};
use citar_store::{
    BodyParts, Container, Header, JournalRef, JournalWriter, SessionRef, StoreError, fork, in_use,
    read_container, read_header, write_container,
};
use pyo3::prelude::*;
use serde_json::{Value, json};

use crate::Bytes;
use crate::errors::{Failure, guarded, parse};

/// Headers read alone, and whole containers read, in this process: what the tests count to
/// show that listing saves never decodes a body.
static HEADERS_READ: AtomicU64 = AtomicU64::new(0);
static CONTAINERS_READ: AtomicU64 = AtomicU64::new(0);

/// A file's name, for messages.
fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into())
}

/// A store error met reading a save, a journal or a header, as the `LoadError` a player reads.
fn load_failure(e: StoreError) -> Failure {
    match e {
        StoreError::Legacy { path } => Failure::Load(format!(
            "{}: saved by the Python engine; archived with 0.1.5. This version reads only the \
             saves it writes.",
            name_of(&path)
        )),
        StoreError::Locked { path } => Failure::Load(format!(
            "{} is in use by another session of this game: close that one first.",
            name_of(&path)
        )),
        other => Failure::Load(other.to_string()),
    }
}

/// A store error met writing a save: the `OSError` that says it was not written.
fn write_failure(e: &StoreError) -> Failure {
    Failure::Os(format!("The save was not written: {e}"))
}

/// A journal reference as the facade hands it back: JSON bytes, strictly.
fn reference_of(json: &[u8]) -> Result<JournalRef, Failure> {
    serde_json::from_slice(json).map_err(|e| {
        Failure::Value(format!("A journal reference is {{file, records, bytes, head}}: {e}"))
    })
}

fn reference_json(r: &JournalRef) -> Value {
    json!({"file": r.file, "records": r.records, "bytes": r.bytes, "head": r.head})
}

// ---- The journal ---------------------------------------------------------------------------

/// A chunk taken from the game and not yet appended.
struct Chunk {
    seq: u32,
    json: Arc<[u8]>,
}

/// The chunks waiting for the disk.
struct Queue {
    chunks: VecDeque<Chunk>,
    /// The number of the next chunk: the records the file holds and the chunks queued.
    next: u32,
}

impl Queue {
    /// The records the file holds.
    fn records(&self) -> u32 {
        self.next.saturating_sub(u32::try_from(self.chunks.len()).unwrap_or(u32::MAX))
    }
}

/// What the tests make a journal do (test operations only).
#[cfg(feature = "test-ops")]
#[derive(Default)]
struct Hooks {
    /// Writes whose first append fails, as a full disk or a held file would make it.
    fail_appends: u32,
    /// Writes that stop once their chunks are appended and synced, before the container: what a
    /// crash between the two leaves.
    stop_before_container: u32,
}

/// One journal of a session's timeline, open for appending (DESIGN.md P2.5.3).
#[pyclass(frozen, module = "citar._engine", name = "Journal")]
pub struct Journal {
    path: PathBuf,
    queue: Mutex<Queue>,
    /// The writer, holding the file's OS lock until it is closed.
    file: Mutex<Option<JournalWriter>>,
    open: AtomicBool,
    #[cfg(feature = "test-ops")]
    hooks: Mutex<Hooks>,
}

impl Journal {
    fn queue(&self) -> MutexGuard<'_, Queue> {
        // Held only to push, look at or pop a chunk, by code that cannot panic.
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn file(&self) -> MutexGuard<'_, Option<JournalWriter>> {
        // A panic while writing (none is known) would leave the writer as the store left it,
        // which the next append checks.
        self.file.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn closed(&self) -> Failure {
        Failure::Os(format!(
            "The save was not written: its journal {} is closed (the game was closed).",
            name_of(&self.path)
        ))
    }

    /// Queues the chunk the game just gave, under the game's lock, in `q`, this journal's queue,
    /// which the caller holds.
    fn push(q: &mut Queue, chunk: JournalChunk) {
        q.chunks.push_back(Chunk { seq: chunk.seq, json: chunk.json.into() });
        q.next = q.next.saturating_add(1);
    }

    /// Appends the queued chunks numbered below `end`, in order, and syncs the file: what a save
    /// whose state counts `end` chunks needs on the disk. The prefix it ends at, which must be
    /// exactly `end` records, for the container.
    fn flush_to(&self, end: u32) -> Result<JournalRef, Failure> {
        let mut file = self.file();
        let writer = file.as_mut().ok_or_else(|| self.closed())?;
        loop {
            let next = {
                let q = self.queue();
                q.chunks.front().filter(|c| c.seq < end).map(|c| (c.seq, Arc::clone(&c.json)))
            };
            let Some((seq, chunk)) = next else { break };
            self.hook_fail_append()?;
            writer.append(seq, &chunk).map_err(|e| write_failure(&e))?;
            self.queue().chunks.pop_front();
        }
        writer.sync().map_err(|e| write_failure(&e))?;
        let r = writer.reference();
        if r.records != end {
            return Err(Failure::Os(format!(
                "The save was not written: its state counts {end} chunks of history, and {} \
                 holds {}.",
                name_of(&self.path),
                r.records
            )));
        }
        Ok(r)
    }

    #[cfg(feature = "test-ops")]
    fn hooks(&self) -> MutexGuard<'_, Hooks> {
        self.hooks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(feature = "test-ops")]
    fn hook_fail_append(&self) -> Result<(), Failure> {
        let mut h = self.hooks();
        if h.fail_appends == 0 {
            return Ok(());
        }
        h.fail_appends -= 1;
        Err(Failure::Os("The save was not written: the append failed (a test hook).".to_owned()))
    }

    #[cfg(not(feature = "test-ops"))]
    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    const fn hook_fail_append(&self) -> Result<(), Failure> {
        Ok(())
    }

    #[cfg(feature = "test-ops")]
    fn hook_stop_before_container(&self) -> Result<(), Failure> {
        let mut h = self.hooks();
        if h.stop_before_container == 0 {
            return Ok(());
        }
        h.stop_before_container -= 1;
        Err(Failure::Os(
            "The save was not written: it stopped before its container (a test hook).".to_owned(),
        ))
    }

    #[cfg(not(feature = "test-ops"))]
    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    const fn hook_stop_before_container(&self) -> Result<(), Failure> {
        Ok(())
    }

    /// Takes the game's snapshot for a save under the game's lock (`Game.save_snapshot`): the
    /// chunk of history since the last take goes into this journal's queue, and the snapshot
    /// counts it. A journal with no records and none queued starts the game's journal over.
    pub fn snapshot(&self, g: &mut citar_engine::game::Game) -> Result<SaveSnapshot, Failure> {
        if !self.open.load(Ordering::Acquire) {
            return Err(self.closed());
        }
        let mut q = self.queue();
        let seq = g.state().host().0.journal_seq;
        if seq != q.next {
            if q.next != 0 {
                return Err(Failure::Runtime(format!(
                    "This game's history goes on at chunk {seq}, and {} at {}: the journal of \
                     another timeline.",
                    name_of(&self.path),
                    q.next
                )));
            }
            g.restart_journal();
        }
        let (snapshot, chunk) = g.save_snapshot().map_err(|e| Failure::Runtime(e.to_string()))?;
        if let Some(chunk) = chunk {
            Self::push(&mut q, chunk);
        }
        Ok(SaveSnapshot { snapshot, end: q.next })
    }
}

#[pymethods]
impl Journal {
    /// Opens (or creates) the journal at `path` for appending, taking its OS lock: the journal
    /// and what opening it found, as JSON bytes: `{records, bytes, torn, corrupt_at,
    /// reference}`. An incomplete tail (a crash mid-append) is cut off (`torn` its length);
    /// corruption (`corrupt_at`, the bad record's first byte) leaves the file as it is and
    /// refuses appends, and `reference` is the good prefix to fork from. `LoadError` when
    /// another session holds the journal or the file is no journal.
    #[staticmethod]
    fn open(py: Python<'_>, path: PathBuf) -> PyResult<(Self, Bytes)> {
        Ok(guarded(py, || {
            let (writer, rec) = JournalWriter::open(&path).map_err(load_failure)?;
            let r = writer.reference();
            let found = json!({
                "records": rec.records, "bytes": rec.bytes, "torn": rec.torn,
                "corrupt_at": rec.corrupt_at, "reference": reference_json(&r),
            });
            let journal = Self {
                path,
                queue: Mutex::new(Queue { chunks: VecDeque::new(), next: rec.records }),
                file: Mutex::new(Some(writer)),
                open: AtomicBool::new(true),
                #[cfg(feature = "test-ops")]
                hooks: Mutex::new(Hooks::default()),
            };
            Ok((journal, Bytes(serde_json::to_vec(&found).unwrap_or_default())))
        })?)
    }

    /// The journal's path.
    #[getter]
    fn path(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    /// The records on the disk (or written to it and not yet synced).
    #[getter]
    fn records(&self) -> u32 {
        self.queue().records()
    }

    /// The chunks taken from the game and not yet appended.
    #[getter]
    fn pending(&self) -> usize {
        self.queue().chunks.len()
    }

    /// Whether the journal is closed (`close`).
    #[getter]
    fn is_closed(&self) -> bool {
        !self.open.load(Ordering::Acquire)
    }

    /// The prefix on the disk, as JSON bytes `{file, records, bytes, head}`: with corruption,
    /// the good prefix. Waits for a write in progress. `OSError` once closed.
    fn reference(&self, py: Python<'_>) -> PyResult<Bytes> {
        Ok(guarded(py, || {
            let file = self.file();
            let writer = file.as_ref().ok_or_else(|| self.closed())?;
            Ok(Bytes(serde_json::to_vec(&reference_json(&writer.reference())).unwrap_or_default()))
        })?)
    }

    /// Cuts the journal back to the prefix `reference_json` (one of its own), dropping the
    /// records past it: the chunks of a save that never completed, when an older save continues
    /// the timeline (DESIGN.md P2.5.3). Synced before it returns. `LoadError` for a reference
    /// that is not this journal's, or a journal that opened corrupt (fork it instead), and when
    /// chunks are queued; `OSError` when the disk refuses (the cut is made again before the
    /// next append).
    fn truncate_to(&self, py: Python<'_>, reference_json: &[u8]) -> PyResult<()> {
        let r = reference_of(reference_json)?;
        Ok(guarded(py, || {
            let mut file = self.file();
            let writer = file.as_mut().ok_or_else(|| self.closed())?;
            let mut q = self.queue();
            if !q.chunks.is_empty() {
                return Err(Failure::Load(format!(
                    "{} has chunks waiting to be appended: it is cut back only as it opened.",
                    name_of(&self.path)
                )));
            }
            match writer.truncate_to(&r) {
                Ok(()) => {
                    q.next = r.records;
                    Ok(())
                }
                Err(e @ StoreError::Io { .. }) => {
                    // The writer stands at `r` all the same, and cuts again before it appends.
                    q.next = r.records;
                    Err(Failure::Os(e.to_string()))
                }
                Err(e) => Err(load_failure(e)),
            }
        })?)
    }

    /// Lets go of the file and its lock, after the write in progress. Returns how many chunks
    /// were still waiting, which no save names. Closing twice is nothing.
    fn close(&self, py: Python<'_>) -> PyResult<usize> {
        Ok(guarded(py, || {
            let writer = self.file().take();
            self.open.store(false, Ordering::Release);
            drop(writer);
            Ok(self.queue().chunks.len())
        })?)
    }

    /// Makes the next `fail_appends` writes fail at their first append, and the next
    /// `stop_before_container` writes stop once their chunks are on the disk, before the
    /// container (DESIGN.md P2.5.3's failures, for the tests). Test operations only.
    #[cfg(feature = "test-ops")]
    #[pyo3(name = "_hooks", signature = (fail_appends = 0, stop_before_container = 0))]
    fn set_hooks(&self, fail_appends: u32, stop_before_container: u32) {
        *self.hooks() = Hooks { fail_appends, stop_before_container };
    }

    fn __repr__(&self) -> String {
        let q = self.queue();
        format!(
            "<citar._engine.Journal {} records {} pending {}{}>",
            name_of(&self.path),
            q.records(),
            q.chunks.len(),
            if self.open.load(Ordering::Acquire) { "" } else { " closed" }
        )
    }
}

// ---- The snapshot and the write ------------------------------------------------------------

/// A game's state taken for a save under the game's lock (`Game.save_snapshot`), written off it.
#[pyclass(frozen, module = "citar._engine", name = "SaveSnapshot")]
pub struct SaveSnapshot {
    snapshot: Snapshot,
    /// The chunks of history the state counts: the records its container names.
    end: u32,
}

impl SaveSnapshot {
    fn write_now(
        &self,
        path: &Path,
        journal: &Journal,
        session_json: &[u8],
        metrics_json: &[u8],
        saved_at: &str,
    ) -> Result<(), Failure> {
        if path.parent() != journal.path.parent() {
            return Err(Failure::Value(format!(
                "A save is written beside its journal: {} is not in {}'s folder.",
                path.display(),
                name_of(&journal.path)
            )));
        }
        let session = parse(session_json, "The session")?;
        let r = journal.flush_to(self.end)?;
        journal.hook_stop_before_container()?;
        let state = self.snapshot.to_json().map_err(|e| Failure::Runtime(e.to_string()))?;
        let rules = self.snapshot.rules();
        let header = Header {
            format: FORMAT.to_owned(),
            version: VERSION,
            saved_at: saved_at.to_owned(),
            engine_build: citar_bot::build_id(rules),
            rules: rules.id().to_hex(),
            summary: listing(&state, &session)?,
            session: session_ref(&session),
            journal: Some(r),
        };
        let body =
            BodyParts { session: session_json, metrics: metrics_json, state: &state, chain: None };
        write_container(path, &header, &body).map_err(|e| match e {
            StoreError::Invalid { .. } => Failure::Value(e.to_string()),
            other => write_failure(&other),
        })
    }
}

/// The header's summary: the engine's summary of the state (`save::summary`, the facade's
/// `state_summary`), and the type of each of the session's seats, by seat, which listing saves
/// shows beside each civilization's name.
fn listing(state: &[u8], session: &Value) -> Result<Value, Failure> {
    let summary =
        citar_engine::save::summary(state).map_err(|e| Failure::Runtime(e.to_string()))?;
    let mut v = serde_json::to_value(&summary).map_err(|e| Failure::Runtime(e.to_string()))?;
    if let (Some(m), Some(seats)) =
        (v.as_object_mut(), session.get("seats").and_then(Value::as_array))
    {
        let types: Vec<Value> =
            seats.iter().map(|s| s.get("type").cloned().unwrap_or(Value::Null)).collect();
        m.insert("seats".to_owned(), Value::Array(types));
    }
    Ok(v)
}

/// The session a save belongs to, as its header names it, from the session's record.
fn session_ref(session: &Value) -> SessionRef {
    let text = |k: &str| session.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    let benchmark = match session.get("benchmark") {
        None | Some(Value::Null | Value::Bool(false)) => false,
        Some(Value::Object(m)) => !m.is_empty(),
        Some(_) => true,
    };
    SessionRef { id: text("id"), name: text("name"), benchmark }
}

#[pymethods]
impl SaveSnapshot {
    /// The turn the state is at.
    #[getter]
    fn turn(&self) -> i32 {
        self.snapshot.state().clock().turn
    }

    /// The chunks of history the state counts: the journal records its container names.
    #[getter]
    fn records(&self) -> u32 {
        self.end
    }

    /// Writes the save to `path`, beside `journal`, with the GIL released: the journal's queued
    /// chunks up to this snapshot's appended and synced, then the state's JSON, zstd and the
    /// container (a temporary file, then a rename over the old save) naming that prefix of the
    /// journal. `session_json` is the session's record, which the body keeps; its `id`, `name`
    /// and `benchmark` name the save's session in the header, and its seats' types go into the
    /// header's summary. `metrics_json` is the session's metrics; `saved_at` the time, ISO 8601.
    /// `OSError` when the save was not written: the chunks stay queued for the next write, and
    /// the old save stays.
    fn write(
        &self,
        py: Python<'_>,
        path: PathBuf,
        journal: &Bound<'_, Journal>,
        session_json: &[u8],
        metrics_json: &[u8],
        saved_at: &str,
    ) -> PyResult<()> {
        let journal = journal.get();
        Ok(guarded(py, || self.write_now(&path, journal, session_json, metrics_json, saved_at))?)
    }

    fn __repr__(&self) -> String {
        format!("<citar._engine.SaveSnapshot turn {} records {}>", self.turn(), self.end)
    }
}

// ---- Reading saves -------------------------------------------------------------------------

/// A container read back (`read_save`): its header, the session's record and metrics, and the
/// state, which `Game.load_save` loads.
#[pyclass(frozen, module = "citar._engine", name = "Save")]
pub struct Save {
    path: PathBuf,
    container: Arc<Container>,
}

impl Save {
    /// The container and where it was read from, for `Game.load_save`.
    pub fn parts(&self) -> (&Path, Arc<Container>) {
        (&self.path, Arc::clone(&self.container))
    }
}

#[pymethods]
impl Save {
    /// Where it was read from.
    #[getter]
    fn path(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    /// The header, as JSON bytes: `{format, version, saved_at, engine_build, rules, summary,
    /// session: {id, name, benchmark}, journal: {file, records, bytes, head} | null}`.
    fn header(&self) -> Bytes {
        Bytes(serde_json::to_vec(self.container.header()).unwrap_or_default())
    }

    /// The session's record, as JSON bytes.
    fn session(&self) -> Bytes {
        Bytes(self.container.session().to_vec())
    }

    /// The session's metrics, as JSON bytes.
    fn metrics(&self) -> Bytes {
        Bytes(self.container.metrics().to_vec())
    }

    fn __repr__(&self) -> String {
        format!("<citar._engine.Save {}>", name_of(&self.path))
    }
}

/// Reads the save at `path`: the header and the body, decompressed and checked. `LoadError` for a
/// file that is no save of this version (a version 1 save of the Python engine is refused by
/// name) or is damaged.
#[pyfunction]
pub fn read_save(py: Python<'_>, path: PathBuf) -> PyResult<Save> {
    Ok(guarded(py, || {
        CONTAINERS_READ.fetch_add(1, Ordering::Relaxed);
        let container = read_container(&path).map_err(load_failure)?;
        Ok(Save { path, container: Arc::new(container) })
    })?)
}

/// The header of the save at `path`, as JSON bytes (as `Save.header`), without reading its
/// body: what listing saves reads. `LoadError` as `read_save`.
#[pyfunction]
pub fn save_header(py: Python<'_>, path: PathBuf) -> PyResult<Bytes> {
    Ok(guarded(py, || {
        HEADERS_READ.fetch_add(1, Ordering::Relaxed);
        let header = read_header(&path).map_err(load_failure)?;
        Ok(Bytes(serde_json::to_vec(&header).unwrap_or_default()))
    })?)
}

/// The history a save's container names, read from `journal` (DESIGN.md P2.5.2): exactly the
/// prefix, checked against its head. `LoadError` saying the save does not load when the journal
/// is missing, damaged inside the prefix, shorter or of another timeline.
pub fn history(save: &Path, header: &Header, journal: &Path) -> Result<Vec<Vec<u8>>, Failure> {
    let Some(r) = header.journal.as_ref() else { return Ok(Vec::new()) };
    citar_store::read_upto(journal, r).map_err(|e| match e {
        StoreError::Locked { .. } => load_failure(e),
        other => Failure::Load(format!(
            "{}'s history ({} records of {}) cannot be read, so the save does not load: \
             {other}. A save from before the damage may.",
            name_of(save),
            r.records,
            name_of(journal)
        )),
    })
}

/// Copies the prefix `reference_json` of the journal at `path` to a new journal at `new_path`,
/// which must not exist yet, and syncs it: a new timeline from a save (DESIGN.md P2.5.3).
/// `LoadError` when the prefix cannot be read (held by a writer, damaged, of another
/// timeline); `OSError` when the copy cannot be written.
#[pyfunction]
pub fn fork_journal(
    py: Python<'_>,
    path: PathBuf,
    reference_json: &[u8],
    new_path: PathBuf,
) -> PyResult<()> {
    let r = reference_of(reference_json)?;
    Ok(guarded(py, || {
        fork(&path, &r, &new_path).map_err(|e| match e {
            StoreError::Io { path: ref p, .. } if *p == new_path => write_failure(&e),
            other => load_failure(other),
        })
    })?)
}

/// Whether a session (of this process or another) holds the journal at `path` now. A missing
/// file is in nobody's use.
#[pyfunction]
pub fn journal_in_use(py: Python<'_>, path: PathBuf) -> PyResult<bool> {
    Ok(guarded(py, || in_use(&path).map_err(|e| Failure::Os(e.to_string())))?)
}

/// How many headers alone and whole saves this process has read: `(save_header, read_save)`.
/// Test operations only.
#[cfg(feature = "test-ops")]
#[pyfunction]
#[pyo3(name = "_saves_read")]
pub fn saves_read() -> (u64, u64) {
    (HEADERS_READ.load(Ordering::Relaxed), CONTAINERS_READ.load(Ordering::Relaxed))
}
