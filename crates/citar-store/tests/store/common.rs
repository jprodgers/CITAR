//! What the store's tests share: folders that clean up after themselves, the property tests'
//! configuration, and valid headers, bodies and journals to start from.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use citar_store::journal::{JournalRef, JournalWriter};
use citar_store::{ChainRef, Header, SessionRef, container};
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};
use serde_json::{Map, Value, json};

/// A folder under cargo's temporary folder for integration tests, removed when dropped.
pub struct Dir(PathBuf);

impl Dir {
    pub fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let p = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join("citar-store")
            .join(format!("{name}-{}-{n}", std::process::id()));
        let _stale = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        Self(p)
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// The names of the files in the folder, sorted.
    pub fn names(&self) -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(&self.0)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _best_effort = std::fs::remove_dir_all(&self.0);
    }
}

/// A property test's cases: `PROPTEST_CASES`, or proptest's own default of 256.
///
/// Every case writes a file, and on Windows the virus scanner then reads it before anything
/// else may (some 2 to 10 ms a case on the laptop), so the gates' counts are runs of their own:
/// `PROPTEST_CASES=1000` for gate 1's round trips and `PROPTEST_CASES=10000` for gate 4, as
/// nightly.yml's props job runs them. CI's tests run 64 (`rust.yml`).
pub fn cases() -> u32 {
    std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(256)
}

/// Runs the property `test` on [`cases`] values of `strategy`, all in one folder made for the
/// run: on Windows a folder per case costs more than the case. Failures are kept beside
/// `source` (the caller's `file!()`), as `*.proptest-regressions` to commit with the fix, and
/// run first from then on.
///
/// # Panics
/// When the property fails, with proptest's smallest failing value.
pub fn check<S: Strategy>(
    source: &'static str,
    name: &str,
    strategy: S,
    test: impl Fn(&Dir, S::Value) -> Result<(), TestCaseError>,
) {
    let dir = Dir::new(name);
    let config = Config {
        cases: cases(),
        source_file: Some(source),
        failure_persistence: Some(Box::new(FileFailurePersistence::WithSource(
            "proptest-regressions",
        ))),
        ..Config::default()
    };
    let mut runner = TestRunner::new(config);
    if let Err(e) = runner.run(&strategy, |v| test(&dir, v)) {
        panic!("{name}: {e}\n{runner}");
    }
}

/// 64 lower-case hex digits.
pub fn hex64(seed: u8) -> String {
    (0..32u8).map(|i| format!("{:02x}", i.wrapping_mul(37).wrapping_add(seed))).collect()
}

/// A header as a session writes one.
pub fn header(journal: Option<JournalRef>) -> Header {
    Header {
        format: container::FORMAT.to_owned(),
        version: container::VERSION,
        saved_at: "2026-10-02T12:00:00Z".to_owned(),
        engine_build: "0123456789ab".to_owned(),
        rules: hex64(1),
        summary: json!({"turn": 42, "phase": "playing", "names": {"0": "Rome"}, "scores": {}}),
        session: SessionRef { id: "g1".to_owned(), name: "Game one".to_owned(), benchmark: false },
        journal,
    }
}

/// A small body's parts, as JSON text.
pub struct Body {
    pub session: Vec<u8>,
    pub metrics: Vec<u8>,
    pub state: Vec<u8>,
    pub chain: Option<ChainRef>,
}

impl Body {
    pub fn small() -> Self {
        Self {
            session: br#"{"id":"g1","seats":[{"pid":0,"kind":"human"}]}"#.to_vec(),
            metrics: br#"{"turns":{"0":12}}"#.to_vec(),
            state: br#"{"format":"citar-state","version":1,"tiles":"AAEC","x":-0.0}"#.to_vec(),
            chain: Some(ChainRef { head: hex64(9), rounds: 41 }),
        }
    }

    pub fn parts(&self) -> citar_store::BodyParts<'_> {
        citar_store::BodyParts {
            session: &self.session,
            metrics: &self.metrics,
            state: &self.state,
            chain: self.chain.as_ref(),
        }
    }
}

/// Writes a journal of `chunks` at `path` with the real writer, and returns the reference after
/// each record (`refs[k]` is the prefix of `k` records).
pub fn write_journal(path: &Path, chunks: &[Vec<u8>]) -> Vec<JournalRef> {
    let (mut w, rec) = JournalWriter::open(path).expect("a new journal opens");
    assert_eq!((rec.records, rec.torn, rec.corrupt_at), (0, None, None));
    let mut refs = vec![w.reference()];
    for (i, c) in chunks.iter().enumerate() {
        w.append(i as u32, c).expect("appends");
        refs.push(w.reference());
    }
    w.sync().expect("syncs");
    refs
}

/// A reference to the same prefix in another file.
pub fn renamed(r: &JournalRef, file: &str) -> JournalRef {
    JournalRef { file: file.to_owned(), ..r.clone() }
}

/// JSON objects of every shape: nested objects and arrays, strings with any characters, every
/// kind of number.
pub fn object() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        any::<f64>().prop_filter("finite", |f| f.is_finite()).prop_map(Value::from),
        ".{0,12}".prop_map(Value::from),
    ];
    let value = leaf.prop_recursive(4, 48, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::vec((".{0,8}", inner), 0..6)
                .prop_map(|kv| Value::Object(kv.into_iter().collect::<Map<_, _>>())),
        ]
    });
    prop::collection::vec((".{0,8}", value), 0..6)
        .prop_map(|kv| Value::Object(kv.into_iter().collect::<Map<_, _>>()))
}

/// An object written compact or pretty, with white space around it or not: the text a part is
/// given as, and the text it must read back as (without the white space around it).
pub fn object_text() -> impl Strategy<Value = (Vec<u8>, Vec<u8>)> {
    (object(), any::<bool>(), "[ \t\r\n]{0,3}", "[ \t\r\n]{0,3}").prop_map(
        |(v, pretty, before, after)| {
            let text = if pretty { serde_json::to_vec_pretty(&v) } else { serde_json::to_vec(&v) }
                .expect("JSON");
            let mut given = before.into_bytes();
            given.extend_from_slice(&text);
            given.extend_from_slice(after.as_bytes());
            (given, text)
        },
    )
}

/// Chunks of every kind a journal holds: empty, a few bytes, noise that does not compress, and
/// JSON-like text that does.
pub fn chunk() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        Just(Vec::new()),
        prop::collection::vec(any::<u8>(), 1..200),
        prop::collection::vec(any::<u8>(), 200..3000),
        (1usize..80, ".{1,20}").prop_map(|(n, s)| {
            format!("{{\"entries\":[{}]}}", vec![format!("{{\"text\":{s:?}}}"); n].join(","))
                .into_bytes()
        }),
    ]
}
