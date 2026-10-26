//! The job side's own rules beyond the shared transcripts (design/phase4-helper.md P4.3.6,
//! PROTOCOL.md "Jobs"; package 4-10's fix round), driven in-process against a fake job host whose
//! starts wait for the test to let them through:
//! - a server's `jobs_pause` holds for the connection it came on: the games go on when it ends,
//!   and the next connection's server says it again if its benchmark lasts;
//! - a `settings` that comes while a job host starts is applied once that start is over, in turn,
//!   and acknowledged with the figures of the host its own limits start;
//! - an acknowledgement that went without the host's figures (the host was ready after the hold,
//!   or started again under new limits once its run ended) goes again with them, marked `again`;
//! - an offer that waits for a host's start is answered within its time to live (`limits`), one
//!   the server withdrew is not, and what a connection left unanswered goes with it;
//! - a helper that stops refuses the offers waiting for its host (`disabled`).
//!
//! They run on tokio's paused clock: the waits for a host and an offer's time to live take no time.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use citar_helper::jobs::{
    ACK_HOLD, HostEvent, HostStart, Hosts, JobSetup, JobSide, OFFER_MARGIN, StartFailure,
};
use citar_helper::llm::BoxFuture;
use citar_helper::log::Log;
use citar_helper::protocol::{HelperFrame, JobAbort, JobOffer, RejectReason, SettingsAck};
use citar_jobs::host::{FailCode, Failed, FromHost, Ready, RunRef, ToHost};
use citar_jobs::limits::Requested;
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

/// What the fake hosts were asked and told.
#[derive(Default)]
struct Rec {
    /// The limits each start was asked for, in order.
    starts: Vec<Requested>,
    /// The pause (true) and resume (false) commands each started host received, by start.
    pauses: Vec<Vec<bool>>,
    /// Each started host's way to say something to the helper, by start.
    events: Vec<mpsc::UnboundedSender<HostEvent>>,
}

/// Job hosts that start once the test opens their gate (open from the first, or later), accept
/// every offer, and record what the helper tells them.
#[derive(Clone)]
struct GatedHosts {
    rec: Arc<Mutex<Rec>>,
    gate: watch::Receiver<bool>,
}

impl Hosts for GatedHosts {
    fn self_test(&self) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    fn start(
        &self,
        limits: Requested,
        jobs_build: String,
    ) -> BoxFuture<'static, Result<HostStart, StartFailure>> {
        let rec = Arc::clone(&self.rec);
        let mut gate = self.gate.clone();
        Box::pin(async move {
            let (cmds_tx, mut cmds_rx) = mpsc::unbounded_channel::<ToHost>();
            let (events_tx, events_rx) = mpsc::unbounded_channel();
            let idx = {
                let mut r = rec.lock().expect("the record");
                r.starts.push(limits);
                r.pauses.push(Vec::new());
                r.events.push(events_tx.clone());
                r.starts.len() - 1
            };
            while !*gate.borrow_and_update() {
                if gate.changed().await.is_err() {
                    return Err(StartFailure {
                        message: "the test ended".into(),
                        ..StartFailure::default()
                    });
                }
            }
            let held = Arc::clone(&rec);
            tokio::spawn(async move {
                while let Some(cmd) = cmds_rx.recv().await {
                    match cmd {
                        ToHost::Pause => held.lock().expect("the record").pauses[idx].push(true),
                        ToHost::Resume => held.lock().expect("the record").pauses[idx].push(false),
                        ToHost::JobOffer(o) => {
                            let accept = FromHost::JobAccept(RunRef { run: o.run });
                            let _sent = events_tx.send(HostEvent::Frame(accept));
                        }
                        _ => {}
                    }
                }
            });
            let threads = limits.threads.max(1);
            let ready = Ready {
                limits: citar_jobs::Held {
                    cpu: citar_jobs::Hardness::Hard,
                    memory: citar_jobs::Hardness::Hard,
                },
                verifiable: true,
                jobs_build,
                threads,
                reason: None,
                memory_per_run_mb: per_run(limits),
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

/// The memory each run has on a fake host holding `limits`: a tenth spare, shared by its threads.
fn per_run(limits: Requested) -> u64 {
    limits.memory_mb * 9 / 10 / u64::from(limits.threads.max(1))
}

/// The settings acknowledgement the session hands the job side: CPU work on with these figures.
fn ack(threads: u32, pct: u32, mem: u64, yield_to_llm: bool) -> SettingsAck {
    serde_json::from_value(json!({
        "effective": {"llm": {"enabled": false, "slots": 0},
                      "cpu": {"enabled": true, "threads": threads, "cpu_percent": pct,
                              "memory_mb": mem, "work": ["lab_game"],
                              "yield_to_llm": yield_to_llm}},
        "capped": [], "limits": {"cpu": "soft", "memory": "soft"}
    }))
    .expect("an acknowledgement")
}

/// An offer of the golden lab duel (its spec and hash as committed), run `run`, for this helper's
/// jobs build.
fn offer(side: &JobSide, run: &str) -> JobOffer {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../citar-jobs/golden/jobs.json").to_owned();
    let golden: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("the golden jobs"))
            .expect("json");
    let job = &golden["jobs"]["duel-a"];
    serde_json::from_value(json!({
        "job": "cj_0123456789abcdef", "run": run, "kind": "lab_game", "spec": job["spec"],
        "spec_hash": job["spec_hash"], "jobs_build": side.jobs_build(), "deadline_s": 300.0,
        "mem_mb": 16, "priority": 0, "offer_ttl_s": 30.0
    }))
    .expect("an offer")
}

/// A job side whose hosts start once `open` says so (open at once when `open` is true).
async fn side(open: bool) -> (JobSide, GatedHosts, watch::Sender<bool>) {
    let (gate_tx, gate) = watch::channel(open);
    let hosts = GatedHosts { rec: Arc::default(), gate };
    let setup = JobSetup { hosts: None, key: None, results: None, ..Default::default() };
    let side = JobSide::start(&setup, Arc::new(hosts.clone()), true, Log::memory()).await;
    (side, hosts, gate_tx)
}

/// The frames the job side sent, of every kind, until `within` passes with none.
async fn frames(
    rx: &mut mpsc::UnboundedReceiver<HelperFrame>,
    within: Duration,
) -> Vec<HelperFrame> {
    let mut out = Vec::new();
    while let Ok(Some(f)) = tokio::time::timeout(within, rx.recv()).await {
        out.push(f);
    }
    out
}

/// The next frame the job side sends that `pick` takes, within `within`.
async fn next<T>(
    rx: &mut mpsc::UnboundedReceiver<HelperFrame>,
    within: Duration,
    pick: impl Fn(HelperFrame) -> Option<T>,
) -> Option<T> {
    let deadline = Instant::now() + within;
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

async fn next_ack(
    rx: &mut mpsc::UnboundedReceiver<HelperFrame>,
    within: Duration,
) -> Option<SettingsAck> {
    next(rx, within, |f| match f {
        HelperFrame::SettingsAck(a) => Some(a),
        _ => None,
    })
    .await
}

fn rejects(frames: &[HelperFrame]) -> BTreeMap<String, RejectReason> {
    frames
        .iter()
        .filter_map(|f| match f {
            HelperFrame::JobReject(r) => Some((r.run.clone(), r.reason)),
            _ => None,
        })
        .collect()
}

/// Waits (on the paused clock) until `cond` holds of the record.
async fn until(hosts: &GatedHosts, what: &str, cond: impl Fn(&Rec) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond(&hosts.rec.lock().expect("the record")) {
        assert!(Instant::now() < deadline, "not within 5 s: {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_jobs_pause_holds_for_the_connection_it_came_on() {
    let (side, hosts, _gate) = side(true).await;
    let (out1, mut rx1) = mpsc::unbounded_channel();
    side.connected(out1, "sv_jobside".to_owned());
    side.settings(ack(2, 50, 1024, true));
    assert!(next_ack(&mut rx1, Duration::from_secs(5)).await.is_some());
    side.benchmark(true);
    until(&hosts, "the host paused", |r| r.pauses[0] == [true]).await;
    // The connection drops: its server's jobs_resume can no longer come, so the games go on.
    side.disconnected();
    until(&hosts, "the host resumed", |r| r.pauses[0] == [true, false]).await;
    let (out2, mut rx2) = mpsc::unbounded_channel();
    side.connected(out2, "sv_jobside".to_owned());
    side.settings(ack(2, 50, 1024, true));
    let a = next_ack(&mut rx2, Duration::from_secs(5)).await.expect("the next connection's ack");
    assert_eq!(a.memory_per_run_mb, Some(460), "the same host, its figures");
    tokio::time::sleep(Duration::from_secs(1)).await;
    {
        let r = hosts.rec.lock().expect("the record");
        assert_eq!(r.starts.len(), 1, "the same limits keep the same host");
        assert_eq!(r.pauses[0], [true, false], "nothing on the new connection paused it");
    }
    assert_eq!(side.snapshot().jobs_paused, 0);
    // Its server says it again (its benchmark lasts): held again.
    side.benchmark(true);
    until(&hosts, "paused again", |r| r.pauses[0] == [true, false, true]).await;
}

#[tokio::test(start_paused = true)]
async fn a_settings_that_comes_while_a_host_starts_is_applied_in_its_turn() {
    let (side, hosts, gate) = side(false).await;
    let (out, mut rx) = mpsc::unbounded_channel();
    side.connected(out, "sv_jobside".to_owned());
    side.settings(ack(2, 50, 1024, true));
    tokio::time::sleep(Duration::from_millis(100)).await;
    // The owner lowers the share while the host starts.
    side.settings(ack(1, 10, 400, false));
    tokio::time::sleep(Duration::from_millis(100)).await;
    gate.send(true).expect("the gate");
    let a1 = next_ack(&mut rx, Duration::from_secs(5)).await.expect("the first ack");
    let a2 = next_ack(&mut rx, Duration::from_secs(5)).await.expect("the second ack");
    assert_eq!((a1.effective.cpu.cpu_percent, a1.memory_per_run_mb), (50, Some(460)));
    assert_eq!(
        (a2.effective.cpu.cpu_percent, a2.memory_per_run_mb),
        (10, Some(360)),
        "acknowledged with the figures of the host its own limits started"
    );
    assert_eq!((a1.again, a2.again), (None, None));
    let starts: Vec<u32> =
        hosts.rec.lock().expect("the record").starts.iter().map(|l| l.cpu_percent).collect();
    assert_eq!(starts, [50, 10], "the second settings' host runs");
    assert_eq!(side.snapshot().memory_per_run_mb, Some(360));
    assert!(next_ack(&mut rx, Duration::from_secs(30)).await.is_none(), "nothing more to say");
}

#[tokio::test(start_paused = true)]
async fn a_host_ready_after_the_hold_is_acknowledged_again_with_its_figures() {
    let (side, _hosts, gate) = side(false).await;
    let (out, mut rx) = mpsc::unbounded_channel();
    side.connected(out, "sv_jobside".to_owned());
    let t0 = Instant::now();
    side.settings(ack(2, 50, 1024, true));
    let first = next_ack(&mut rx, ACK_HOLD * 2).await.expect("the held ack");
    assert!(t0.elapsed() >= ACK_HOLD, "it waited for the host");
    assert_eq!((first.memory_per_run_mb, first.again), (None, None), "the host was not ready");
    gate.send(true).expect("the gate");
    let again = next_ack(&mut rx, Duration::from_secs(5)).await.expect("the ack again");
    assert_eq!((again.memory_per_run_mb, again.again), (Some(460), Some(true)));
    assert_eq!(again.effective, first.effective, "the same settings");
    assert_eq!(again.limits.cpu.as_str(), "hard", "the host's own hardness");
    assert!(next_ack(&mut rx, Duration::from_secs(60)).await.is_none(), "once");
}

#[tokio::test(start_paused = true)]
async fn a_host_started_again_under_new_limits_is_acknowledged_again() {
    let (side, hosts, _gate) = side(true).await;
    let (out, mut rx) = mpsc::unbounded_channel();
    side.connected(out, "sv_jobside".to_owned());
    side.settings(ack(1, 50, 1024, true));
    let a = next_ack(&mut rx, Duration::from_secs(5)).await.expect("the ack");
    assert_eq!(a.memory_per_run_mb, Some(921));
    side.offer(offer(&side, "cr_restart"), false);
    let accepted = next(&mut rx, Duration::from_secs(5), |f| match f {
        HelperFrame::JobAccept(r) => Some(r.run),
        _ => None,
    })
    .await;
    assert_eq!(accepted.as_deref(), Some("cr_restart"));
    // Less memory while the run plays: acknowledged at once, without a figure (the host running
    // holds the old limits until its run ends).
    side.settings(ack(1, 50, 512, true));
    let b = next_ack(&mut rx, Duration::from_secs(5)).await.expect("the ack of the change");
    assert_eq!((b.effective.cpu.memory_mb, b.memory_per_run_mb, b.again), (512, None, None));
    // The run ends; the host is started again under the new limits, and its figures follow.
    let events = hosts.rec.lock().expect("the record").events[0].clone();
    let failed = Failed {
        run: "cr_restart".to_owned(),
        code: FailCode::Host,
        message: "the test ended it".to_owned(),
        retryable: true,
    };
    events.send(HostEvent::Frame(FromHost::JobFailed(failed))).expect("the host's word");
    let again = next_ack(&mut rx, Duration::from_secs(5)).await.expect("the ack again");
    assert_eq!(
        (again.effective.cpu.memory_mb, again.memory_per_run_mb, again.again),
        (512, Some(460), Some(true))
    );
    assert_eq!(hosts.rec.lock().expect("the record").starts.len(), 2);
    assert_eq!(side.snapshot().memory_per_run_mb, Some(460));
}

#[tokio::test(start_paused = true)]
async fn offers_waiting_for_a_host_are_answered_within_their_time_to_live() {
    let (side, _hosts, _gate) = side(false).await;
    let (out, mut rx) = mpsc::unbounded_channel();
    side.connected(out, "sv_jobside".to_owned());
    let t0 = Instant::now();
    side.settings(ack(1, 50, 1024, true));
    // During the acknowledgement's hold, then after it, then one the server withdraws.
    side.offer(offer(&side, "cr_held"), false);
    tokio::time::sleep(ACK_HOLD + Duration::from_secs(2)).await;
    side.offer(offer(&side, "cr_after"), false);
    side.offer(offer(&side, "cr_withdrawn"), false);
    side.abort(JobAbort { run: "cr_withdrawn".to_owned(), reason: "expired".to_owned() });
    let mut answered = BTreeMap::new();
    while let Some((run, reason)) = next(&mut rx, Duration::from_secs(60), |f| match f {
        HelperFrame::JobReject(r) => Some((r.run, r.reason)),
        _ => None,
    })
    .await
    {
        answered.insert(run, (reason, t0.elapsed()));
    }
    let ttl = Duration::from_secs(30);
    let (reason, at) = answered["cr_held"];
    assert_eq!(reason, RejectReason::Limits);
    assert!(at >= ttl - OFFER_MARGIN && at < ttl, "{at:?}: within its time to live, just before");
    let (reason, at) = answered["cr_after"];
    assert_eq!(reason, RejectReason::Limits);
    let since = at - (ACK_HOLD + Duration::from_secs(2));
    assert!(since >= ttl - OFFER_MARGIN && since < ttl, "{since:?} after it came");
    assert!(!answered.contains_key("cr_withdrawn"), "the server withdrew it: no answer");
}

#[tokio::test(start_paused = true)]
async fn what_a_connection_left_unanswered_goes_with_it() {
    let (side, _hosts, gate) = side(false).await;
    let (out1, mut rx1) = mpsc::unbounded_channel();
    side.connected(out1, "sv_jobside".to_owned());
    side.settings(ack(1, 50, 1024, true));
    side.offer(offer(&side, "cr_old"), false);
    tokio::time::sleep(Duration::from_secs(1)).await;
    side.disconnected();
    let (out2, mut rx2) = mpsc::unbounded_channel();
    side.connected(out2, "sv_jobside".to_owned());
    side.settings(ack(1, 50, 1024, true));
    tokio::time::sleep(Duration::from_secs(1)).await;
    gate.send(true).expect("the gate");
    let sent = frames(&mut rx2, Duration::from_secs(40)).await;
    let acks: Vec<&SettingsAck> = sent
        .iter()
        .filter_map(|f| match f {
            HelperFrame::SettingsAck(a) => Some(a),
            _ => None,
        })
        .collect();
    assert_eq!(acks.len(), 1, "one acknowledgement for the one settings it was sent: {sent:?}");
    assert_eq!(acks[0].memory_per_run_mb, Some(921));
    assert!(rejects(&sent).is_empty(), "the old connection's offer is not answered on the new");
    assert!(frames(&mut rx1, Duration::from_millis(10)).await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_stopping_helper_refuses_the_offers_waiting_for_its_host() {
    let (side, _hosts, _gate) = side(false).await;
    let (out, mut rx) = mpsc::unbounded_channel();
    side.connected(out, "sv_jobside".to_owned());
    side.settings(ack(1, 50, 1024, true));
    side.offer(offer(&side, "cr_waiting"), false);
    tokio::time::sleep(Duration::from_secs(1)).await;
    side.stop(Duration::from_secs(5)).await;
    let sent = frames(&mut rx, Duration::from_millis(100)).await;
    assert_eq!(
        rejects(&sent).get("cr_waiting"),
        Some(&RejectReason::Disabled),
        "refused at once: {sent:?}"
    );
}
