//! The job host played inside the test process (its limits not applied, which would hold the test
//! runner): the start, the golden jobs, the offers' refusals, abort, deadlines and the end of its
//! input. The limits, the processes and the test jobs are `tests/host.rs`'s.

use std::io::{BufReader, Read, Write};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use serde_json::json;

use super::*;
use crate::golden::{GoldenSet, SELF_CHECK};

/// The host's standard input, fed line by line; it ends when the sender goes.
struct In {
    rx: Receiver<Vec<u8>>,
    buf: Vec<u8>,
    at: usize,
}

impl Read for In {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.at >= self.buf.len() {
            match self.rx.recv() {
                Ok(b) => {
                    self.buf = b;
                    self.at = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len() - self.at);
        out[..n].copy_from_slice(&self.buf[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// What a test does with each frame as the host writes it, inside the host's write (so the
/// thread that sends it waits for it).
type Hook = Box<dyn FnMut(&FromHost) + Send>;

/// The host's standard output, read back frame by frame.
struct OutPipe {
    tx: Sender<FromHost>,
    pending: Vec<u8>,
    hook: Option<Hook>,
}

impl Write for OutPipe {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(b);
        while let Some(i) = self.pending.iter().position(|c| *c == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=i).collect();
            let text = String::from_utf8_lossy(&line);
            let frame = FromHost::read(text.trim()).expect("the host writes its frames");
            if let Some(hook) = &mut self.hook {
                hook(&frame);
            }
            let _gone = self.tx.send(frame);
        }
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Played {
    to: Option<Sender<Vec<u8>>>,
    from: Receiver<FromHost>,
    done: JoinHandle<i32>,
    /// Its `ready`, once [`Played::ready`] read it.
    ready: Option<Ready>,
}

const WAIT: Duration = Duration::from_secs(60);

fn mine() -> String {
    crate::build::jobs_build(Ruleset::shared())
}

fn limits(threads: u32) -> ToHost {
    ToHost::Limits(LimitsFrame {
        jobs_build: mine(),
        threads,
        cpu_percent: 100,
        memory_mb: 4096,
        play_unverifiable: false,
    })
}

/// The server's own executor's limits: it plays its own work, verifiable or not.
fn own_work(threads: u32) -> ToHost {
    ToHost::Limits(LimitsFrame {
        jobs_build: mine(),
        threads,
        cpu_percent: 100,
        memory_mb: 4096,
        play_unverifiable: true,
    })
}

impl Played {
    fn start(first: &ToHost, setup: Setup) -> Self {
        Self::hooked(first, setup, |_| None)
    }

    /// Started with a hook on its output, made with a sender of its input (which the hook must
    /// drop once done with it: the host ends only when its input closes).
    fn hooked(
        first: &ToHost,
        setup: Setup,
        hook: impl FnOnce(Sender<Vec<u8>>) -> Option<Hook>,
    ) -> Self {
        let (to, rx) = channel();
        let (tx, from) = channel();
        let hook = hook(to.clone());
        let done = std::thread::spawn(move || {
            let input = BufReader::new(In { rx, buf: Vec::new(), at: 0 });
            serve(input, OutPipe { tx, pending: Vec::new(), hook }, setup)
        });
        let p = Self { to: Some(to), from, done, ready: None };
        p.send(first);
        p
    }

    fn ready(threads: u32) -> Self {
        Self::ready_with(threads, |_| None)
    }

    /// Ready, with a wait of `step` before every seat step of every game, so that a game outlasts
    /// what a test sends it on any machine: a fast runner played a 330-turn duel in under a second.
    fn ready_slowed(threads: u32, step: Duration) -> Self {
        Self::ready_from(threads, Setup { step_delay: step, ..Setup::default() }, |_| None)
    }

    fn ready_with(threads: u32, hook: impl FnOnce(Sender<Vec<u8>>) -> Option<Hook>) -> Self {
        Self::ready_from(threads, Setup::default(), hook)
    }

    fn ready_from(
        threads: u32,
        setup: Setup,
        hook: impl FnOnce(Sender<Vec<u8>>) -> Option<Hook>,
    ) -> Self {
        let mut p = Self::hooked(&limits(threads), setup, hook);
        let FromHost::Ready(r) = p.next() else { panic!("a ready host") };
        assert!(r.verifiable, "{r:?}");
        p.ready = Some(r);
        p
    }

    fn send(&self, f: &ToHost) {
        let mut line = f.line().into_bytes();
        line.push(b'\n');
        self.to.as_ref().expect("open").send(line).expect("the host reads");
    }

    fn send_raw(&self, text: &str) {
        self.to.as_ref().expect("open").send(format!("{text}\n").into_bytes()).expect("reads");
    }

    fn next(&self) -> FromHost {
        self.from.recv_timeout(WAIT).expect("a frame from the host")
    }

    /// The next frame about `run` (or `ready`/`fatal`), skipping the heartbeats.
    fn about(&self, run: &str) -> FromHost {
        loop {
            let f = self.next();
            if f.run() == Some(run) {
                return f;
            }
        }
    }

    fn close(mut self) -> i32 {
        drop(self.to.take());
        self.done.join().expect("the host returns")
    }
}

fn offer(run: &str, kind: &str, spec: Value, hash: &str, deadline_s: f64) -> ToHost {
    ToHost::JobOffer(Offer {
        run: run.to_owned(),
        job: Some("cj_test".to_owned()),
        kind: kind.to_owned(),
        spec,
        spec_hash: hash.to_owned(),
        jobs_build: mine(),
        deadline_s,
        mem_mb: 0,
        priority: 0,
    })
}

fn golden(name: &str) -> (ToHost, String, String) {
    let set = GoldenSet::committed().expect("the set");
    let job = &set.jobs[name];
    let (_, _, want) = job.expected(Ruleset::shared()).expect("expected");
    let run = format!("cr_{}", name.replace('-', "_"));
    let o = offer(&run, job.kind.name(), job.spec.clone(), &job.spec_hash, 600.0);
    (o, want, job.chain_head.clone())
}

/// A lab duel of `turns` rounds, and its hash.
fn duel(turns: u32) -> (Value, String) {
    let spec = json!({"exp": "host", "i": 0, "seed": 7, "map_type": "continents", "size": "duel",
                      "speed": "Quick", "turns": turns, "difficulty": "Prince",
                      "seats": [{"label": "a"}, {"label": "b"}]});
    let c = check_spec(Ruleset::shared(), Kind::LabGame, &spec, None).expect("a good spec");
    (c.canonical_value(), c.hash.to_hex())
}

#[test]
fn a_host_refuses_another_build_and_a_bad_start() {
    let setup = Setup::default();
    let other = ToHost::Limits(LimitsFrame {
        jobs_build: "000000000000".to_owned(),
        threads: 1,
        cpu_percent: 10,
        memory_mb: 100,
        play_unverifiable: false,
    });
    let p = Played::start(&other, setup);
    let FromHost::Fatal(f) = p.next() else { panic!("fatal") };
    assert_eq!(f.code, FatalCode::Build, "{f:?}");
    assert!(f.message.contains("000000000000") && f.message.contains(&mine()), "{f:?}");
    assert_eq!(p.close(), EXIT_REFUSED);

    let p = Played::start(&ToHost::Pause, setup);
    let FromHost::Fatal(f) = p.next() else { panic!("fatal") };
    assert_eq!(f.code, FatalCode::Protocol);
    assert_eq!(p.close(), EXIT_REFUSED);

    let zero = ToHost::Limits(LimitsFrame {
        jobs_build: mine(),
        threads: 0,
        cpu_percent: 10,
        memory_mb: 100,
        play_unverifiable: false,
    });
    let p = Played::start(&zero, setup);
    let FromHost::Fatal(f) = p.next() else { panic!("fatal") };
    assert_eq!(f.code, FatalCode::Limits, "{f:?}");
    assert_eq!(p.close(), EXIT_REFUSED);

    // A memory limit that leaves a run less than a megabyte, once the host's own is counted.
    let starved = ToHost::Limits(LimitsFrame {
        jobs_build: mine(),
        threads: 2,
        cpu_percent: 10,
        memory_mb: 2,
        play_unverifiable: false,
    });
    let p = Played::start(&starved, setup);
    let FromHost::Fatal(f) = p.next() else { panic!("fatal") };
    assert_eq!(f.code, FatalCode::Limits, "{f:?}");
    assert!(f.message.contains("leaves its 2 runs nothing"), "{f:?}");
    assert_eq!(p.close(), EXIT_REFUSED);
}

#[test]
fn a_ready_host_plays_the_golden_jobs_to_their_committed_hashes() {
    let p = Played::ready(2);
    let (lab, lab_hash, lab_head) = golden(SELF_CHECK);
    let (reference, ref_hash, ref_head) = golden("reference-a");
    p.send(&lab);
    p.send(&reference);
    let mut seen: BTreeMap<String, Vec<FromHost>> = BTreeMap::new();
    while seen.values().filter(|fs| fs.iter().any(|f| f.t() == "job_result")).count() < 2 {
        let f = p.next();
        if let Some(run) = f.run() {
            seen.entry(run.to_owned()).or_default().push(f);
        }
    }
    for (run, want, head) in
        [("cr_duel_a", lab_hash, lab_head), ("cr_reference_a", ref_hash, ref_head)]
    {
        let frames = &seen[run];
        assert!(matches!(frames.first(), Some(FromHost::JobAccept(_))), "{run}: {frames:?}");
        let Some(FromHost::JobResult(r)) = frames.last() else { panic!("{run}: {frames:?}") };
        assert_eq!(r.outcome.result_hash, want, "{run}");
        assert_eq!(r.outcome.chain.head, head, "{run}");
        // Counted, frees included: a short game holds a few MB at most, though it allocates far
        // more over its rounds.
        assert!(
            r.outcome.peak_mb > 0.0 && r.outcome.peak_mb < 64.0,
            "{run}: {}",
            r.outcome.peak_mb
        );
        assert!(r.outcome.paused_s.abs() < 1e-9 && r.outcome.wall_s > 0.0, "{r:?}");
    }
    assert_eq!(p.close(), 0);
}

#[test]
fn offers_are_refused_with_their_reasons() {
    let p = Played::ready(1);
    let (spec, hash) = duel(40);
    // A run that plays reports its progress every 25 rounds, from its own thread: a report may
    // come before the answer to an offer that names it, or before its abort, when the host's
    // reader is slow to get the CPU (it did on WSL, under nextest's other tests). The reports are
    // skipped, as the abort test below skips them.
    let reason = |p: &Played, run: &str| loop {
        match p.about(run) {
            FromHost::JobReject(r) => {
                assert!(!r.message.is_empty(), "{r:?}");
                break r.reason;
            }
            FromHost::JobProgress(_) => {}
            other => panic!("{run}: {other:?}"),
        }
    };
    let mut other_build = offer("cr_b", "lab_game", spec.clone(), &hash, 60.0);
    if let ToHost::JobOffer(o) = &mut other_build {
        o.jobs_build = "000000000000".to_owned();
    }
    p.send(&other_build);
    assert_eq!(reason(&p, "cr_b"), RejectReason::Build);
    p.send(&offer("cr_k", "gpu_game", spec.clone(), &hash, 60.0));
    assert_eq!(reason(&p, "cr_k"), RejectReason::BadSpec);
    p.send(&offer("cr_h", "lab_game", spec.clone(), &"0".repeat(64), 60.0));
    assert_eq!(reason(&p, "cr_h"), RejectReason::BadSpec);
    p.send(&offer("cr_d", "lab_game", spec.clone(), &hash, 0.5));
    assert_eq!(reason(&p, "cr_d"), RejectReason::BadSpec);
    p.send(&offer("cr bad id", "lab_game", spec.clone(), &hash, 60.0));
    assert_eq!(reason(&p, "cr bad id"), RejectReason::BadSpec);
    p.send_raw(r#"{"t": "job_offer", "run": "cr_u", "kind": "lab_game"}"#);
    assert_eq!(reason(&p, "cr_u"), RejectReason::BadSpec);
    // A host inside its parent holds no limit, so nothing of its own is counted: 90% of its 4,096
    // MB is a run's, and an offer above that is refused, though it is under memory_mb.
    let per_run = p.ready.as_ref().map(|r| r.memory_per_run_mb);
    assert_eq!(per_run, Some(3686), "{:?}", p.ready);
    let mut big = offer("cr_m", "lab_game", spec.clone(), &hash, 60.0);
    if let ToHost::JobOffer(o) = &mut big {
        o.mem_mb = 3687;
    }
    p.send(&big);
    assert_eq!(reason(&p, "cr_m"), RejectReason::Memory);
    // One thread: a second run is refused while the first plays, and a run id is not taken
    // twice.
    let (long, long_hash) = duel(330);
    p.send(&offer("cr_one", "lab_game", long.clone(), &long_hash, 600.0));
    assert!(matches!(p.about("cr_one"), FromHost::JobAccept(_)));
    p.send(&offer("cr_one", "lab_game", long.clone(), &long_hash, 600.0));
    assert_eq!(reason(&p, "cr_one"), RejectReason::BadSpec);
    p.send(&offer("cr_two", "lab_game", spec, &hash, 60.0));
    assert_eq!(reason(&p, "cr_two"), RejectReason::Busy);
    // An unknown frame type is ignored, a frame that does not read is logged.
    p.send_raw(r#"{"t": "something_new", "x": 1}"#);
    p.send_raw("not json");
    p.send(&ToHost::JobAbort(Abort { run: "cr_one".to_owned(), reason: "test".to_owned() }));
    loop {
        match p.about("cr_one") {
            FromHost::JobProgress(_) => {}
            FromHost::JobAborted(_) => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(p.close(), 0);
}

/// A run's end is sent once its thread is free: a parent that offers the next run the moment it
/// reads the end (here inside the host's own write of it, which waits while the host reads and
/// checks that offer) finds the host's one thread free, never `busy`.
#[test]
fn a_runs_end_is_sent_once_its_thread_is_free() {
    let (spec, hash) = duel(10);
    let mut next = offer("cr_next", "lab_game", spec.clone(), &hash, 60.0).line().into_bytes();
    next.push(b'\n');
    let p = Played::ready_with(1, move |to| {
        let mut to = Some(to);
        Some(Box::new(move |f: &FromHost| {
            if matches!(f, FromHost::JobResult(r) if r.run == "cr_first")
                && let Some(to) = to.take()
            {
                to.send(next.clone()).expect("the host reads");
                std::thread::sleep(Duration::from_millis(300));
            }
        }))
    });
    p.send(&offer("cr_first", "lab_game", spec, &hash, 60.0));
    let ended = |p: &Played, run: &str| loop {
        match p.about(run) {
            FromHost::JobAccept(_) | FromHost::JobProgress(_) => {}
            other => break other,
        }
    };
    assert!(matches!(ended(&p, "cr_first"), FromHost::JobResult(_)));
    assert!(matches!(ended(&p, "cr_next"), FromHost::JobResult(_)));
    assert_eq!(p.close(), 0);
}

#[test]
fn a_peak_step_is_an_eighth_of_a_run_within_its_bounds() {
    const MB: u64 = 1 << 20;
    assert_eq!(peak_step(160), 20 * MB);
    assert_eq!(peak_step(3686), 32 * MB, "at most 32 MiB");
    assert_eq!(peak_step(4), MB, "at least 1 MiB");
    assert_eq!(peak_step(u64::MAX), 32 * MB);
}

/// A run reports its peak as it grows, though no round passes: an `alloc` of 100 MB, whose steps
/// are not rounds, reports its peak at each 32 MiB of growth (an eighth of a run's 3,686 MB is
/// more), so the last report before a death says nearly how much the run reached.
#[cfg(feature = "test-jobs")]
#[test]
fn a_runs_peak_is_reported_as_it_grows() {
    let p = Played::ready(1);
    let (spec, hash) =
        test_jobs::hashed(test_jobs::ALLOC, &json!({"mb": 100, "step_mb": 4})).expect("an alloc");
    p.send(&offer("cr_grow", test_jobs::ALLOC, spec, &hash, 60.0));
    let mut peaks = Vec::new();
    let result = loop {
        match p.about("cr_grow") {
            FromHost::JobAccept(_) => {}
            FromHost::JobProgress(g) => peaks.push(g.peak_mb),
            FromHost::JobResult(r) => break r,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(peaks.len(), 3, "{peaks:?}");
    for (n, peak) in peaks.iter().enumerate() {
        let want = 32.0 * (n as f64 + 1.0);
        assert!(*peak >= want && *peak < want + 4.5, "{peaks:?}");
    }
    assert!((result.outcome.peak_mb - 100.0).abs() < 1.0, "{:?}", result.outcome);
    assert_eq!(p.close(), 0);
}

#[test]
fn a_host_whose_canary_fails_refuses_every_job() {
    let inject = Inject { halved: Some(0.0), ..Inject::default() };
    let p = Played::start(&limits(1), Setup { canary: inject, ..Setup::default() });
    let FromHost::Ready(r) = p.next() else { panic!("ready") };
    assert!(!r.verifiable && r.reason.as_deref().is_some_and(|w| w.contains("flushed")), "{r:?}");
    let (spec, hash) = duel(10);
    p.send(&offer("cr_x", "lab_game", spec, &hash, 60.0));
    let FromHost::JobReject(j) = p.about("cr_x") else { panic!("refused") };
    assert_eq!(j.reason, RejectReason::Unverifiable);
    assert_eq!(p.close(), 0);
}

#[test]
fn a_host_playing_its_parents_own_work_plays_when_it_is_not_verifiable() {
    // The server's executor (P4.5.6): a failed canary or self-check is reported, and the jobs
    // still play, to the same results (the injected values fail the check, not the game).
    let flushed = Inject { halved: Some(0.0), ..Inject::default() };
    for setup in [
        Setup { canary: flushed, ..Setup::default() },
        Setup { fail_self_check: true, ..Setup::default() },
    ] {
        let p = Played::start(&own_work(1), setup);
        let FromHost::Ready(r) = p.next() else { panic!("ready") };
        assert!(!r.verifiable && r.reason.is_some(), "{r:?}");
        let (o, want, head) = golden(SELF_CHECK);
        p.send(&o);
        let r = loop {
            match p.about("cr_duel_a") {
                FromHost::JobAccept(_) | FromHost::JobProgress(_) => {}
                FromHost::JobResult(r) => break r,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!((r.outcome.result_hash, r.outcome.chain.head), (want, head));
        assert_eq!(p.close(), 0);
    }
    // A helper's host is not told so, and refuses (a_host_whose_canary_fails_refuses_every_job).
    let line = limits(1).line();
    assert!(!line.contains("play_unverifiable"), "left out when off: {line}");
    assert!(own_work(1).line().contains("\"play_unverifiable\":true"));
}

#[test]
fn abort_ends_a_run_within_a_second_and_a_deadline_ends_one() {
    // 10 ms before each seat step makes a 330-turn duel last at least 6.6 s, so cr_a is still
    // playing when its abort comes and cr_d when its 1 s deadline (the least an offer may ask)
    // passes. Unslowed, a fast ARM runner's release build finished cr_d first (2026-10-24).
    let p = Played::ready_slowed(2, Duration::from_millis(10));
    let (spec, hash) = duel(330);
    p.send(&offer("cr_a", "lab_game", spec.clone(), &hash, 600.0));
    assert!(matches!(p.about("cr_a"), FromHost::JobAccept(_)));
    std::thread::sleep(Duration::from_millis(300));
    let asked = Instant::now();
    p.send(&ToHost::JobAbort(Abort { run: "cr_a".to_owned(), reason: String::new() }));
    loop {
        match p.about("cr_a") {
            FromHost::JobProgress(_) => {}
            FromHost::JobAborted(_) => break,
            other => panic!("{other:?}"),
        }
    }
    assert!(asked.elapsed() < Duration::from_secs(1), "{:?}", asked.elapsed());
    p.send(&offer("cr_d", "lab_game", spec, &hash, 1.0));
    assert!(matches!(p.about("cr_d"), FromHost::JobAccept(_)));
    let failed = loop {
        match p.about("cr_d") {
            FromHost::JobProgress(_) => {}
            FromHost::JobFailed(f) => break f,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(failed.code, FailCode::Deadline, "{failed:?}");
    assert!(failed.retryable && failed.message.contains("deadline"), "{failed:?}");
    assert_eq!(p.close(), 0);
}

#[test]
fn the_host_returns_soon_after_its_input_closes() {
    let p = Played::ready(1);
    let (spec, hash) = duel(330);
    p.send(&offer("cr_e", "lab_game", spec, &hash, 600.0));
    assert!(matches!(p.about("cr_e"), FromHost::JobAccept(_)));
    let closed = Instant::now();
    assert_eq!(p.close(), 0);
    assert!(closed.elapsed() < Duration::from_secs(2), "{:?}", closed.elapsed());
}

#[test]
fn frames_read_back_as_written() {
    let frames = [
        FromHost::JobAccept(RunRef { run: "cr_1".to_owned() }),
        FromHost::JobReject(Reject {
            run: "cr_1".to_owned(),
            reason: RejectReason::Busy,
            message: "two runs".to_owned(),
        }),
        FromHost::JobFailed(Failed {
            run: "cr_1".to_owned(),
            code: FailCode::Deadline,
            message: "late".to_owned(),
            retryable: true,
        }),
        FromHost::Heartbeat(Heartbeat { running: 1, paused: true, cpu_s: 1.5 }),
        FromHost::Fatal(Fatal { code: FatalCode::Limits, message: "no".to_owned() }),
    ];
    for f in frames {
        assert_eq!(FromHost::read(&f.line()), Ok(f.clone()), "{}", f.line());
        assert!(f.line().starts_with('{') && f.line().contains(&format!("\"t\":\"{}\"", f.t())));
    }
    assert!(FromHost::read(r#"{"t": "job_accept"}"#).is_err());
    assert!(FromHost::read(r#"{"t": "other"}"#).is_err());
    let pause = ToHost::Pause.line();
    assert!(matches!(receive(pause.as_bytes()), Received::Frame(ToHost::Pause)));
    assert!(matches!(receive(br#"{"t": "new_thing"}"#), Received::Unknown(_)));
    assert!(is_id("cr_0123-x") && !is_id("") && !is_id(&"x".repeat(65)) && !is_id("a b"));
}

/// A run held by a pause ends once it has lasted twice its deadline in all, though its active time
/// is short of it: paused time extends a deadline, up to twice it.
#[cfg(feature = "test-jobs")]
#[test]
fn a_run_paused_past_twice_its_deadline_fails() {
    let p = Played::ready(1);
    let (spec, hash) =
        test_jobs::hashed(test_jobs::BURN, &json!({"seconds": 1.0})).expect("a burn");
    p.send(&ToHost::Pause);
    p.send(&offer("cr_held", test_jobs::BURN, spec, &hash, 1.0));
    let started = Instant::now();
    let failed = loop {
        match p.about("cr_held") {
            FromHost::JobAccept(_) | FromHost::JobProgress(_) => {}
            FromHost::JobFailed(f) => break f,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(failed.code, FailCode::Deadline, "{failed:?}");
    assert!(failed.message.contains("twice its deadline"), "{failed:?}");
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(1900) && took < Duration::from_secs(10), "{took:?}");
    assert_eq!(p.close(), 0);
}
