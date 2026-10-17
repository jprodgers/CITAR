//! A helper that cheats on purpose (design/phase4-helper.md P4.4.6; the `test-faults` feature,
//! which S7's nextest turns on with every other), driven in-process: a fake job host plays the
//! golden lab duel (its committed outcome), and each mode changes what the helper sends as its
//! table says. The server's own check of a claim (`citar_jobs::verify_claim`) is run on each
//! result, to show which faults it catches at once (a signature by another key) and which only a
//! replica can (a changed score, a changed digest: validly signed, the content wrong).
#![cfg(feature = "test-faults")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use citar_helper::faults::Faults;
use citar_helper::jobs::{HostEvent, HostStart, Hosts, JobSetup, JobSide, StartFailure};
use citar_helper::key::MachineKey;
use citar_helper::llm::BoxFuture;
use citar_helper::log::Log;
use citar_helper::protocol::{HelperFrame, JobOffer, JobResult, SettingsAck};
use citar_jobs::golden::{GoldenJob, GoldenSet, SELF_CHECK};
use citar_jobs::host::{self as jh, FromHost, Ready, RunRef, ToHost};
use citar_jobs::limits::Requested;
use citar_jobs::{ClaimVerdict, Expected};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::Instant;

const MACHINE: &str = "sv_faulty";

fn golden() -> GoldenJob {
    GoldenSet::committed().expect("the golden set").jobs[SELF_CHECK].clone()
}

/// The golden duel's outcome as this build gives it, with 1.5 CPU seconds.
fn outcome() -> jh::Outcome {
    let job = golden();
    let (_checked, result, result_hash) =
        job.expected(citar_engine::rules::Ruleset::shared()).expect("its expected result");
    jh::Outcome {
        result,
        result_hash,
        chain: job.chain(),
        cpu_s: 1.5,
        wall_s: 1.6,
        paused_s: 0.0,
        peak_mb: 3.0,
    }
}

/// What the fake host was told, and its way to answer.
#[derive(Default)]
struct Rec {
    offers: Vec<String>,
    aborts: Vec<String>,
    events: Option<mpsc::UnboundedSender<HostEvent>>,
}

/// Job hosts that start at once, accept every offer and answer an abort; the test makes them
/// finish a run (`finish`).
#[derive(Clone, Default)]
struct Fake {
    rec: Arc<Mutex<Rec>>,
}

impl Fake {
    fn finish(&self, run: &str) {
        let r = self.rec.lock().expect("the record");
        let frame = FromHost::JobResult(Box::new(jh::ResultFrame {
            run: run.to_owned(),
            outcome: outcome(),
        }));
        let _sent = r.events.as_ref().expect("a host").send(HostEvent::Frame(frame));
    }
}

impl Hosts for Fake {
    fn self_test(&self) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    fn start(
        &self,
        limits: Requested,
        jobs_build: String,
    ) -> BoxFuture<'static, Result<HostStart, StartFailure>> {
        let rec = Arc::clone(&self.rec);
        Box::pin(async move {
            let (cmds_tx, mut cmds_rx) = mpsc::unbounded_channel::<ToHost>();
            let (events_tx, events_rx) = mpsc::unbounded_channel();
            rec.lock().expect("the record").events = Some(events_tx.clone());
            let held = Arc::clone(&rec);
            tokio::spawn(async move {
                while let Some(cmd) = cmds_rx.recv().await {
                    match cmd {
                        ToHost::JobOffer(o) => {
                            held.lock().expect("the record").offers.push(o.run.clone());
                            let accept = FromHost::JobAccept(RunRef { run: o.run });
                            let _sent = events_tx.send(HostEvent::Frame(accept));
                        }
                        ToHost::JobAbort(a) => {
                            held.lock().expect("the record").aborts.push(a.run.clone());
                            let gone = FromHost::JobAborted(RunRef { run: a.run });
                            let _sent = events_tx.send(HostEvent::Frame(gone));
                        }
                        _ => {}
                    }
                }
            });
            let ready = Ready {
                limits: citar_jobs::Held {
                    cpu: citar_jobs::Hardness::Hard,
                    memory: citar_jobs::Hardness::Hard,
                },
                verifiable: true,
                jobs_build,
                threads: limits.threads.max(1),
                reason: None,
                memory_per_run_mb: 400,
                own_mb: 5.0,
                cgroup: None,
                cpu_s: 0.0,
                pid: 1,
                counting: true,
                notes: Vec::new(),
            };
            Ok(HostStart { ready, cmds: cmds_tx, events: events_rx })
        })
    }
}

/// A job side playing `modes`, connected, its CPU lent: (the side, its host, its frames, its key).
async fn helper(
    modes: &[&str],
) -> (JobSide, Fake, mpsc::UnboundedReceiver<HelperFrame>, Arc<MachineKey>) {
    let faults =
        Faults::parse(&modes.iter().map(|m| (*m).to_owned()).collect::<Vec<_>>()).expect("modes");
    let key = Arc::new(MachineKey::generate().expect("a key"));
    let hosts = Fake::default();
    let setup = JobSetup {
        hosts: None,
        key: Some(Arc::clone(&key)),
        results: None,
        faults,
        ..Default::default()
    };
    let side = JobSide::start(&setup, Arc::new(hosts.clone()), true, Log::memory()).await;
    let (out, mut rx) = mpsc::unbounded_channel();
    side.connected(out, MACHINE.to_owned());
    let ack: SettingsAck = serde_json::from_value(json!({
        "effective": {"llm": {"enabled": false, "slots": 0},
                      "cpu": {"enabled": true, "threads": 1, "cpu_percent": 50, "memory_mb": 1024,
                              "work": ["lab_game"], "yield_to_llm": true}},
        "capped": [], "limits": {"cpu": "soft", "memory": "soft"}
    }))
    .expect("an acknowledgement");
    side.settings(ack);
    next(&mut rx, |f| matches!(f, HelperFrame::SettingsAck(_)).then_some(())).await.expect("ack");
    (side, hosts, rx, key)
}

fn offer(side: &JobSide, run: &str) -> JobOffer {
    let job = golden();
    serde_json::from_value(json!({
        "job": format!("cj_{}", &run[3..]), "run": run, "kind": "lab_game", "spec": job.spec,
        "spec_hash": job.spec_hash, "jobs_build": side.jobs_build(), "deadline_s": 300.0,
        "mem_mb": 16, "priority": 0, "offer_ttl_s": 30.0
    }))
    .expect("an offer")
}

async fn next<T>(
    rx: &mut mpsc::UnboundedReceiver<HelperFrame>,
    pick: impl Fn(HelperFrame) -> Option<T>,
) -> Option<T> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(f)) => {
                if let Some(t) = pick(f) {
                    return Some(t);
                }
            }
            _ => return None,
        }
    }
}

/// Offers run `run`, waits for its accept, has the host finish it, and returns its result frame.
async fn play(
    side: &JobSide,
    host: &Fake,
    rx: &mut mpsc::UnboundedReceiver<HelperFrame>,
    run: &str,
) -> JobResult {
    side.offer(offer(side, run), false);
    let accepted = next(rx, |f| match f {
        HelperFrame::JobAccept(r) => Some(r.run),
        _ => None,
    })
    .await;
    assert_eq!(accepted.as_deref(), Some(run));
    host.finish(run);
    next(rx, |f| match f {
        HelperFrame::JobResult(r) => Some(r),
        _ => None,
    })
    .await
    .expect("a result")
}

/// The server's check of a result frame's claim, against the records it keeps for `run`.
fn verify(
    frame: &JobResult,
    key: &MachineKey,
    side: &JobSide,
) -> Result<(), citar_jobs::ClaimRefused> {
    let job = golden();
    let rules = citar_engine::rules::Ruleset::shared();
    let checked =
        citar_jobs::check_spec(rules, job.kind, &job.spec, Some(&job.spec_hash)).expect("the spec");
    let jobs_build = side.jobs_build().to_owned();
    let public = key.public_b64();
    let job_id = format!("cj_{}", &frame.run[3..]);
    let expected = Expected {
        job: &job_id,
        run: &frame.run,
        machine: MACHINE,
        jobs_build: &jobs_build,
        key: &public,
    };
    let claim = serde_json::to_value(&frame.claim).expect("the claim");
    let result = Value::Object(frame.outcome.result.clone());
    let chain = serde_json::to_value(&frame.outcome.chain).expect("the chain");
    citar_jobs::verify_claim(rules, &checked, &expected, &claim, &frame.signature, &result, &chain)
}

#[tokio::test(start_paused = true)]
async fn an_honest_helpers_result_is_the_golden_one_and_its_claim_verifies() {
    let (side, host, mut rx, key) = helper(&[]).await;
    let r = play(&side, &host, &mut rx, "cr_honest0000000001").await;
    assert_eq!(r.outcome.result_hash, outcome().result_hash);
    assert_eq!(r.outcome.chain.head, golden().chain_head);
    assert_eq!(verify(&r, &key, &side), Ok(()));
}

#[tokio::test(start_paused = true)]
async fn result_alters_one_score_from_its_job_on_and_signs_it_validly() {
    let (side, host, mut rx, key) = helper(&["result:2"]).await;
    let first = play(&side, &host, &mut rx, "cr_result000000001").await;
    assert_eq!(first.outcome.result_hash, outcome().result_hash, "job 1 is honest");
    for run in ["cr_result000000002", "cr_result000000003"] {
        let r = play(&side, &host, &mut rx, run).await;
        assert_ne!(r.outcome.result_hash, outcome().result_hash, "{run} altered");
        assert_eq!(r.outcome.chain.head, golden().chain_head, "the same chain");
        assert_eq!(r.claim.result_hash, r.outcome.result_hash);
        assert_eq!(verify(&r, &key, &side), Ok(()), "validly signed: only a replica tells");
    }
}

#[tokio::test(start_paused = true)]
async fn result_at_alters_only_the_jobs_it_names() {
    let (side, host, mut rx, _key) = helper(&["result-at:2,4"]).await;
    let mut altered = Vec::new();
    for n in 1..=5 {
        let r = play(&side, &host, &mut rx, &format!("cr_resultat00000000{n}")).await;
        if r.outcome.result_hash != outcome().result_hash {
            altered.push(n);
        }
    }
    assert_eq!(altered, [2, 4]);
}

#[tokio::test(start_paused = true)]
async fn digest_changes_one_round_and_folds_the_chain_from_it() {
    let (side, host, mut rx, key) = helper(&["digest:12"]).await;
    let r = play(&side, &host, &mut rx, "cr_digest000000001").await;
    assert_eq!(r.outcome.result_hash, outcome().result_hash, "the result as it was");
    assert_ne!(r.outcome.chain.head, golden().chain_head);
    assert_eq!(citar_jobs::parted_at(&golden().digests, &r.outcome.chain.digests), Some(12));
    assert_eq!(verify(&r, &key, &side), Ok(()), "a chain that folds: only a replica tells");
}

#[tokio::test(start_paused = true)]
async fn sig_signs_with_another_key_and_the_server_refuses_it() {
    let (side, host, mut rx, key) = helper(&["sig"]).await;
    let r = play(&side, &host, &mut rx, "cr_sig000000000001").await;
    assert_eq!(r.claim.key, key.public_b64(), "the claim names the machine's own key");
    let e = verify(&r, &key, &side).expect_err("another key's signature");
    assert_eq!(e.verdict, ClaimVerdict::Signature, "{e}");
}

#[tokio::test(start_paused = true)]
async fn cpu_reports_its_factor_and_hold_holds_the_result_reporting_the_time_as_cpu() {
    let (side, host, mut rx, _key) = helper(&["cpu:3"]).await;
    let r = play(&side, &host, &mut rx, "cr_cpu000000000001").await;
    assert!((r.outcome.cpu_s - 4.5).abs() < 1e-9, "three times 1.5 s: {}", r.outcome.cpu_s);
    let (side, host, mut rx, _key) = helper(&["hold:20"]).await;
    let run = "cr_hold00000000001";
    side.offer(offer(&side, run), false);
    next(&mut rx, |f| matches!(f, HelperFrame::JobAccept(_)).then_some(())).await.expect("accept");
    let t0 = Instant::now();
    host.finish(run);
    let r = next(&mut rx, |f| match f {
        HelperFrame::JobResult(r) => Some(r),
        _ => None,
    })
    .await
    .expect("a result");
    assert!(t0.elapsed() >= Duration::from_secs(20), "held {:?}", t0.elapsed());
    assert!((r.outcome.cpu_s - 21.5).abs() < 1e-9, "the held time as CPU: {}", r.outcome.cpu_s);
}

#[tokio::test(start_paused = true)]
async fn abandon_accepts_its_job_and_never_finishes_it() {
    let (side, host, mut rx, _key) = helper(&["abandon:1"]).await;
    let run = "cr_abandon00000001";
    side.offer(offer(&side, run), false);
    let accepted = next(&mut rx, |f| match f {
        HelperFrame::JobAccept(r) => Some(r.run),
        _ => None,
    })
    .await;
    assert_eq!(accepted.as_deref(), Some(run), "it is accepted");
    // its host lets it go; the server hears nothing more of it
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.rec.lock().expect("the record").aborts.is_empty() {
        assert!(Instant::now() < deadline, "its host was not told to let it go");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let more = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match rx.recv().await {
                Some(HelperFrame::JobProgress(_) | HelperFrame::JobResult(_))
                | Some(HelperFrame::JobFailed(_) | HelperFrame::JobAborted(_)) => return true,
                Some(_) => {}
                None => return false,
            }
        }
    })
    .await;
    assert!(more.is_err(), "nothing more of the abandoned run was sent");
    assert!(side.snapshot().running.is_empty());
    // the next job is played as it comes
    let r = play(&side, &host, &mut rx, "cr_abandon00000002").await;
    assert_eq!(r.outcome.result_hash, outcome().result_hash);
}
