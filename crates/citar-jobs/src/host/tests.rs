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

/// The host's standard output, read back frame by frame.
struct OutPipe {
    tx: Sender<FromHost>,
    pending: Vec<u8>,
}

impl Write for OutPipe {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(b);
        while let Some(i) = self.pending.iter().position(|c| *c == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=i).collect();
            let text = String::from_utf8_lossy(&line);
            let frame = FromHost::read(text.trim()).expect("the host writes its frames");
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
}

const WAIT: Duration = Duration::from_secs(60);

fn mine() -> String {
    crate::build::jobs_build(Ruleset::shared())
}

fn limits(threads: u32) -> ToHost {
    ToHost::Limits(LimitsFrame { jobs_build: mine(), threads, cpu_percent: 100, memory_mb: 4096 })
}

impl Played {
    fn start(first: &ToHost, setup: Setup) -> Self {
        let (to, rx) = channel();
        let (tx, from) = channel();
        let done = std::thread::spawn(move || {
            let input = BufReader::new(In { rx, buf: Vec::new(), at: 0 });
            serve(input, OutPipe { tx, pending: Vec::new() }, setup)
        });
        let p = Self { to: Some(to), from, done };
        p.send(first);
        p
    }

    fn ready(threads: u32) -> Self {
        let p = Self::start(&limits(threads), Setup::default());
        let FromHost::Ready(r) = p.next() else { panic!("a ready host") };
        assert!(r.verifiable, "{r:?}");
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
    });
    let p = Played::start(&zero, setup);
    let FromHost::Fatal(f) = p.next() else { panic!("fatal") };
    assert_eq!(f.code, FatalCode::Limits, "{f:?}");
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
        assert!(r.outcome.peak_mb > 0.0, "the run's memory is counted: {r:?}");
        assert!(r.outcome.paused_s.abs() < 1e-9 && r.outcome.wall_s > 0.0, "{r:?}");
    }
    assert_eq!(p.close(), 0);
}

#[test]
fn offers_are_refused_with_their_reasons() {
    let p = Played::ready(1);
    let (spec, hash) = duel(40);
    let reason = |p: &Played, run: &str| match p.about(run) {
        FromHost::JobReject(r) => {
            assert!(!r.message.is_empty(), "{r:?}");
            r.reason
        }
        other => panic!("{run}: {other:?}"),
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
    let mut big = offer("cr_m", "lab_game", spec.clone(), &hash, 60.0);
    if let ToHost::JobOffer(o) = &mut big {
        o.mem_mb = 4097;
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
    assert!(matches!(p.about("cr_one"), FromHost::JobAborted(_)));
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
fn abort_ends_a_run_within_a_second_and_a_deadline_ends_one() {
    let p = Played::ready(2);
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
