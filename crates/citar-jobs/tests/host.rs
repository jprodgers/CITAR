//! The job host as a process, under the limits of the OS it runs on (design/phase4-helper.md
//! P4.4.3, P4.3.6; package 4-08's gates). This binary is its own host: started with
//! `CITAR_JOBS_HOST_CHILD=host` it runs `citar_jobs::host::main` on its standard input and output,
//! with the counting allocator a host has; with `fake-ready` it answers like a host of another
//! jobs build; with `parent` it starts a host and prints its pid, for the test of a parent that
//! dies. The tests start hosts with `host::parent::spawn`, as the helper does, under the
//! `systemd --user` scope where Linux offers one (`CITAR_JOBS_NO_SCOPE=1` turns it off, and
//! `CITAR_JOBS_REQUIRE_SCOPE=1` makes a missing scope a failure, as gate 2's WSL run asks).
//!
//! - `limits::*`: the CPU held to its share (by the host's own process CPU time, within its share
//!   x 1.15 + 0.05 cores and at least 40% of it), and a memory death classified, the host started
//!   again and its next job played (gates 1 and 2); what `ready` says holds, per OS (gates 1, 2
//!   and 8);
//! - `results::*`: the golden jobs give their committed hashes in a host capped at 10% and paused
//!   twice, and in one uncapped (gate 3);
//! - `ending::*`: a pause holds a game's round and extends its deadline, an abort ends a run
//!   within a second, a deadline ends one, and a host whose parent died is gone within two
//!   seconds (gate 4);
//! - `memory::*`: a run's peak is counted within 10%, two runs' apart (gate 5);
//! - `builds::*`: another jobs build refused on both sides, and a host whose canary fails refuses
//!   every job (gate 6);
//! - `fuzz::*`: 2,000 mutated lab and ranking specs and 2,000 mutated reference specs through a
//!   host's standard input, answered `bad_spec` or, when the spec is good, `busy` (P4.6).
//!
//! Every test keeps within P4.7.1's bounds: at most two cores' worth, ten seconds of burning and
//! 512 MB. `.config/nextest.toml` runs them one at a time, and the CPU test with nothing beside
//! it.

mod common;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead as _, Write as _};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use citar_engine::rules::Ruleset;
use citar_jobs::golden::{GoldenSet, SELF_CHECK};
use citar_jobs::host::parent::{self, Death, Event, Host, Program, SpawnError, Wrapper};
use citar_jobs::host::test_jobs::{ALLOC, BURN, hashed};
use citar_jobs::host::{
    Abort, FailCode, FatalCode, FromHost, Offer, ProgressFrame, Reject, RejectReason, ToHost,
};
use citar_jobs::limits::{Hardness, Requested, machine_cpus};
use citar_jobs::{Counting, Kind, check_spec, jobs_build};
use libtest_mimic::{Arguments, Failed, Trial};
use serde_json::{Value, json};

#[global_allocator]
static COUNTING: Counting = Counting;

const CHILD: &str = "CITAR_JOBS_HOST_CHILD";
const NO_SCOPE: &str = "CITAR_JOBS_NO_SCOPE";
const REQUIRE_SCOPE: &str = "CITAR_JOBS_REQUIRE_SCOPE";
/// How long a test waits for anything a host should do at once.
const WAIT: Duration = Duration::from_secs(60);

type Outcome = Result<(), Failed>;

fn fail<T>(why: impl Into<String>) -> Result<T, Failed> {
    Err(Failed::from(why.into()))
}

fn ensure(ok: bool, why: impl FnOnce() -> String) -> Outcome {
    if ok { Ok(()) } else { fail(why()) }
}

fn mine() -> String {
    jobs_build(Ruleset::shared())
}

/// The share of the machine that is at most two cores' worth (P4.7.1), as a percentage.
fn two_cores_percent() -> u32 {
    (200 / machine_cpus().max(1)).clamp(1, 100)
}

/// The wrapper the tests start hosts with.
fn wrapper() -> Wrapper {
    if std::env::var_os(NO_SCOPE).is_some() { Wrapper::Direct } else { Wrapper::detect() }
}

fn program(env: &[(&str, &str)]) -> Program {
    let mut vars = vec![(OsString::from(CHILD), OsString::from("host"))];
    vars.extend(env.iter().map(|(k, v)| (OsString::from(k), OsString::from(v))));
    Program {
        program: std::env::current_exe().expect("this test binary"),
        args: Vec::new(),
        env: vars,
    }
}

fn start_with(req: Requested, wrapper: Wrapper, env: &[(&str, &str)]) -> Result<Host, Failed> {
    parent::spawn(&program(env), wrapper, &req, &mine(), parent::READY_WAIT)
        .map_err(|e| format!("the host did not start: {e}").into())
}

fn start(req: Requested, env: &[(&str, &str)]) -> Result<Host, Failed> {
    start_with(req, wrapper(), env)
}

fn offer(run: &str, kind: &str, spec: &Value, hash: &str, deadline_s: f64) -> ToHost {
    ToHost::JobOffer(Offer {
        run: run.to_owned(),
        job: None,
        kind: kind.to_owned(),
        spec: spec.clone(),
        spec_hash: hash.to_owned(),
        jobs_build: mine(),
        deadline_s,
        mem_mb: 0,
        priority: 0,
    })
}

fn test_offer(run: &str, kind: &str, spec: &Value, deadline_s: f64) -> Result<ToHost, Failed> {
    let (spec, hash) = hashed(kind, spec)?;
    Ok(offer(run, kind, &spec, &hash, deadline_s))
}

fn send(host: &mut Host, f: &ToHost) -> Outcome {
    host.send(f).map_err(|e| format!("the host stopped reading: {e}").into())
}

/// The next frame, or a failure naming why there is none.
fn frame(host: &mut Host) -> Result<(FromHost, Instant), Failed> {
    match host.next(WAIT) {
        Some(Event::Frame(f)) => Ok((f, Instant::now())),
        Some(Event::Garbled(line)) => fail(format!("the host wrote {line}")),
        Some(Event::Gone) => {
            let death = host.death(Duration::from_secs(5));
            fail(format!("the host ended: {death}; its log: {:?}", host.take_log()))
        }
        None => fail(format!("nothing from the host in {WAIT:?}")),
    }
}

/// Frames until `run` ends (a result, a failure, an abort or a refusal), each with when it came.
fn until_end(host: &mut Host, run: &str) -> Result<Vec<(FromHost, Instant)>, Failed> {
    let mut seen = Vec::new();
    loop {
        let (f, at) = frame(host)?;
        let end = f.run() == Some(run)
            && matches!(
                f,
                FromHost::JobResult(_)
                    | FromHost::JobFailed(_)
                    | FromHost::JobAborted(_)
                    | FromHost::JobReject(_)
            );
        seen.push((f, at));
        if end {
            return Ok(seen);
        }
    }
}

/// The next frame about `run`, skipping the rest.
fn about(host: &mut Host, run: &str) -> Result<(FromHost, Instant), Failed> {
    loop {
        let (f, at) = frame(host)?;
        if f.run() == Some(run) {
            return Ok((f, at));
        }
    }
}

fn accepted(host: &mut Host, run: &str) -> Outcome {
    match about(host, run)?.0 {
        FromHost::JobAccept(_) => Ok(()),
        other => fail(format!("{run}: {other:?}")),
    }
}

fn result_of(frames: &[(FromHost, Instant)]) -> Result<citar_jobs::host::Outcome, Failed> {
    match frames.last().map(|(f, _)| f) {
        Some(FromHost::JobResult(r)) => Ok(r.outcome.clone()),
        other => fail(format!("no result: {other:?}")),
    }
}

/// The pid's process is gone (or a zombie no one waits for), within `wait`.
fn gone(pid: u32, wait: Duration) -> bool {
    let until = Instant::now() + wait;
    let mut sys = sysinfo::System::new();
    let pid = sysinfo::Pid::from_u32(pid);
    loop {
        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        let alive =
            sys.process(pid).is_some_and(|p| !matches!(p.status(), sysinfo::ProcessStatus::Zombie));
        if !alive {
            return true;
        }
        if Instant::now() > until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// What `ready` must say holds on this OS, started as `wrapper`.
fn expected_held(wrapper: Wrapper) -> (Hardness, Hardness) {
    if cfg!(windows) {
        (Hardness::Hard, Hardness::Hard)
    } else if cfg!(target_os = "linux") {
        if wrapper == Wrapper::Scope {
            (Hardness::Hard, Hardness::Hard)
        } else {
            (Hardness::Soft, Hardness::Hard)
        }
    } else {
        (Hardness::Soft, Hardness::Soft)
    }
}

/// Gate 2's run requires the scope.
fn scope_required() -> Outcome {
    if std::env::var_os(REQUIRE_SCOPE).is_some() && wrapper() != Wrapper::Scope {
        return fail("CITAR_JOBS_REQUIRE_SCOPE is set and the systemd --user scope is not here");
    }
    Ok(())
}

// --- limits -------------------------------------------------------------------------------------

/// `ready` says how hard each limit holds, as P4.3.6's table says for this OS and wrapper.
fn ready_says_how_the_limits_hold() -> Outcome {
    scope_required()?;
    let w = wrapper();
    let req = Requested { threads: 1, cpu_percent: two_cores_percent(), memory_mb: 300 };
    let host = start_with(req, w, &[])?;
    let r = host.ready().clone();
    let want = expected_held(w);
    ensure((r.limits.cpu, r.limits.memory) == want, || format!("{w:?}: {r:?}, want {want:?}"))?;
    ensure(r.verifiable && r.counting && r.jobs_build == mine() && r.threads == 1, || {
        format!("{r:?}")
    })?;
    println!("{w:?}: {:?}, {}", r.limits, r.notes.join("; "));
    if cfg!(target_os = "linux") && w == Wrapper::Scope {
        ensure(r.cgroup.as_deref().is_some_and(|c| c.ends_with(".scope")), || format!("{r:?}"))?;
    }
    // Gate 8 (macOS, CI-only): soft, soft, and the background band.
    if cfg!(target_os = "macos") {
        ensure(r.notes.iter().any(|n| n.contains("background band (PRIO_DARWIN_BG)")), || {
            format!("{:?}", r.notes)
        })?;
    }
    if cfg!(target_os = "linux") {
        // Without the wrapper: soft CPU (nice), hard memory (RLIMIT_DATA).
        let direct = start_with(req, Wrapper::Direct, &[])?;
        let held = direct.ready().limits;
        ensure((held.cpu, held.memory) == (Hardness::Soft, Hardness::Hard), || {
            format!("direct: {held:?}")
        })?;
    }
    Ok(())
}

/// Gate 1 (and 2's first check): two burning runs in a host given 10% of the machine use, by the
/// host's own CPU time, at most that share x 1.15 + 0.05 cores and at least 40% of it.
fn cpu_is_held_to_its_share() -> Outcome {
    scope_required()?;
    let w = wrapper();
    if expected_held(w).0 != Hardness::Hard {
        println!("skipped: the CPU is not held hard here ({w:?}); ready says so");
        return Ok(());
    }
    let cpus = machine_cpus();
    let req = Requested { threads: 2, cpu_percent: 10, memory_mb: 512 };
    let share = req.cores(cpus);
    ensure(share <= 2.0, || format!("10% of {cpus} CPUs is over two cores"))?;
    let mut host = start_with(req, w, &[])?;
    let burn = json!({"seconds": 8.0});
    send(&mut host, &test_offer("cr_burn_1", BURN, &burn, 60.0)?)?;
    send(&mut host, &test_offer("cr_burn_2", BURN, &burn, 60.0)?)?;
    let (mut accepted, mut ended) = (0, 0);
    let (mut first, mut last) = (None, None);
    while ended < 2 {
        let (f, at) = frame(&mut host)?;
        match &f {
            FromHost::JobAccept(_) => accepted += 1,
            FromHost::JobResult(_) => ended += 1,
            FromHost::Heartbeat(h) => {
                if accepted == 2 && h.running == 2 && first.is_none() {
                    first = Some((h.cpu_s, at));
                }
            }
            FromHost::JobProgress(_) => {}
            other => return fail(format!("{other:?}")),
        }
    }
    // The heartbeat a run's end sends, once both have ended.
    while last.is_none() {
        if let (FromHost::Heartbeat(h), at) = frame(&mut host)?
            && h.running == 0
        {
            last = Some((h.cpu_s, at));
        }
    }
    let (c0, t0) = first.ok_or("no heartbeat with both running")?;
    let (c1, t1) = last.ok_or("no heartbeat after both ended")?;
    let wall = t1.duration_since(t0).as_secs_f64();
    let rate = (c1 - c0) / wall;
    println!(
        "{cpus} CPUs, share {share:.2} cores: {:.2} CPU s over {wall:.2} s, {rate:.3} cores",
        c1 - c0
    );
    ensure(wall > 6.0, || format!("the runs took {wall:.2} s"))?;
    ensure(rate <= share * 1.15 + 0.05, || {
        format!("{rate:.3} cores, over {share:.2} x 1.15 + 0.05")
    })?;
    ensure(rate >= share * 0.4, || format!("{rate:.3} cores, under 40% of {share:.2}"))
}

/// Gate 1's and 2's memory check: a run that allocates past the host's memory ends the host,
/// classified `memory` from what showed it; a new host plays the next job.
fn a_memory_death_is_classified_and_the_next_host_plays() -> Outcome {
    scope_required()?;
    memory_death(wrapper())
}

/// Gate 2, the wrapper turned off: Linux holds the memory by `RLIMIT_DATA`, and the death is still
/// `memory`.
fn without_the_scope_linux_still_holds_memory() -> Outcome {
    if !cfg!(target_os = "linux") {
        println!("skipped: Linux only");
        return Ok(());
    }
    memory_death(Wrapper::Direct)
}

fn memory_death(w: Wrapper) -> Outcome {
    let req = Requested { threads: 1, cpu_percent: two_cores_percent(), memory_mb: 200 };
    let mut host = start_with(req, w, &[])?;
    let held = host.ready().limits;
    // Held hard, or (macOS) by the watchdog, which the held allocation gives time to see it.
    if held.memory != Hardness::Hard && !cfg!(target_os = "macos") {
        println!("skipped: memory is not held here ({w:?}, {held:?})");
        return Ok(());
    }
    let big = json!({"mb": 400, "hold_s": 3.0});
    send(&mut host, &test_offer("cr_big", ALLOC, &big, 60.0)?)?;
    accepted(&mut host, "cr_big")?;
    loop {
        match host.next(WAIT) {
            Some(Event::Gone) => break,
            Some(Event::Frame(FromHost::JobResult(_))) => {
                return fail("an allocation past the limit succeeded");
            }
            Some(_) => {}
            None => return fail("the host lived on past its memory"),
        }
    }
    let death = host.death(Duration::from_secs(5));
    let Death::Memory(why) = &death else {
        return fail(format!("{w:?}: {death}; its log {:?}", host.take_log()));
    };
    println!("{w:?}: {why}");
    let wanted = if cfg!(target_os = "macos") {
        "watchdog"
    } else if cfg!(target_os = "linux") && w == Wrapper::Scope {
        "oom_kill"
    } else {
        "allocation failed"
    };
    ensure(why.contains(wanted), || format!("{w:?}: {why}, not from \"{wanted}\""))?;
    let failures = host.failures(&death);
    ensure(
        matches!(
            failures.as_slice(),
            [FromHost::JobFailed(f)] if f.run == "cr_big" && f.code == FailCode::Memory && f.retryable
        ),
        || format!("{failures:?}"),
    )?;
    // The parent starts a host again, and the next job runs.
    let mut again = start_with(req, w, &[])?;
    send(&mut again, &test_offer("cr_next", ALLOC, &json!({"mb": 50}), 60.0)?)?;
    let frames = until_end(&mut again, "cr_next")?;
    let out = result_of(&frames)?;
    ensure(out.result["mb"] == 50, || format!("{out:?}"))
}

// --- results ------------------------------------------------------------------------------------

/// Gate 3: the shortest golden lab job and the reference job, played in a host capped at 10% and
/// paused twice (each step slowed, so the pauses land mid-game), and in an uncapped host, give
/// the committed result hashes and chain heads.
fn results_do_not_depend_on_the_limits() -> Outcome {
    let set = GoldenSet::committed()?;
    let names = [SELF_CHECK, "reference-a"];
    let offers: Vec<(String, ToHost, String, String)> = names
        .iter()
        .map(|name| {
            let job = &set.jobs[*name];
            let (_, _, want) = job.expected(Ruleset::shared())?;
            let run = format!("cr_{}", name.replace('-', "_"));
            let o = offer(&run, job.kind.name(), &job.spec, &job.spec_hash, 600.0);
            Ok::<_, String>((run, o, want, job.chain_head.clone()))
        })
        .collect::<Result<_, _>>()?;
    let mut hashes = BTreeMap::new();
    for (label, cpu_percent, slow) in [("uncapped", 100, None), ("capped", 10, Some("15"))] {
        let req = Requested { threads: 2, cpu_percent, memory_mb: 512 };
        let env: Vec<(&str, &str)> =
            slow.map(|ms| vec![("CITAR_JOBS_TEST_STEP_MS", ms)]).unwrap_or_default();
        let mut host = start(req, &env)?;
        for (_, o, _, _) in &offers {
            send(&mut host, o)?;
        }
        let mut results = BTreeMap::new();
        // Paused twice: each pause is held until every run shows it is held, and the second is
        // sent once every run shows it was let go (each game's steps are slowed, so both are still
        // playing): no sleep decides whether a pause landed.
        let (mut held, mut let_go) = (BTreeMap::new(), BTreeMap::new());
        let mut accepted = 0;
        let (mut pauses, mut resumes) = (0u32, 0u32);
        let mut waiting = slow.is_some();
        while results.len() < offers.len() {
            let (f, _) = frame(&mut host)?;
            match f {
                FromHost::JobAccept(_) => accepted += 1,
                FromHost::JobProgress(p) => {
                    let h = held.entry(p.run.clone()).or_insert(0u32);
                    if p.paused {
                        *h += 1;
                    } else if *h > *let_go.get(&p.run).unwrap_or(&0) {
                        let_go.insert(p.run.clone(), *h);
                    }
                }
                FromHost::JobResult(r) => {
                    results.insert(r.run.clone(), r.outcome);
                }
                FromHost::JobFailed(f) => return fail(format!("{label}: {f:?}")),
                FromHost::JobReject(r) => return fail(format!("{label}: {r:?}")),
                _ => {}
            }
            if !waiting {
                continue;
            }
            let every = |m: &BTreeMap<String, u32>, n: u32| {
                offers.iter().all(|(run, ..)| m.get(run).copied().unwrap_or(0) == n)
            };
            if accepted == offers.len()
                && pauses < 2
                && every(&held, pauses)
                && every(&let_go, pauses)
            {
                send(&mut host, &ToHost::Pause)?;
                pauses += 1;
            } else if resumes < pauses && every(&held, pauses) && every(&let_go, pauses - 1) {
                std::thread::sleep(Duration::from_millis(300));
                send(&mut host, &ToHost::Resume)?;
                resumes += 1;
                if pauses == 2 {
                    waiting = false;
                }
            }
        }
        if slow.is_some() {
            ensure(!waiting && pauses == 2, || {
                format!("{label}: paused {pauses} times, {held:?}")
            })?;
        }
        for (run, _, want, head) in &offers {
            let out = &results[run];
            ensure(&out.result_hash == want && &out.chain.head == head, || {
                format!("{label} {run}: {} {}, want {want} {head}", out.result_hash, out.chain.head)
            })?;
            if slow.is_some() {
                ensure(out.paused_s >= 0.55, || format!("{label} {run}: paused {}", out.paused_s))?;
            }
            hashes.entry(run.clone()).or_insert_with(Vec::new).push(out.result_hash.clone());
        }
        println!("{label}: {results:?}", results = results.keys().collect::<Vec<_>>());
    }
    ensure(hashes.values().all(|h| h.len() == 2 && h[0] == h[1]), || format!("{hashes:?}"))
}

// --- ending -------------------------------------------------------------------------------------

/// Gate 4: a paused game's round does not move for two seconds, then moves again, with its
/// paused time reported; a burn paused past its deadline in wall time finishes (the deadline
/// counts no paused time), one without a pause past its deadline fails.
fn a_pause_holds_a_run_and_extends_its_deadline() -> Outcome {
    let req = Requested { threads: 2, cpu_percent: two_cores_percent(), memory_mb: 512 };
    let mut host = start(req, &[("CITAR_JOBS_TEST_STEP_MS", "20")])?;
    let set = GoldenSet::committed()?;
    let job = &set.jobs[SELF_CHECK];
    send(&mut host, &offer("cr_game", "lab_game", &job.spec, &job.spec_hash, 600.0))?;
    send(&mut host, &test_offer("cr_ok", BURN, &json!({"seconds": 2.0}), 3.0)?)?;
    accepted(&mut host, "cr_game")?;
    std::thread::sleep(Duration::from_millis(400));
    send(&mut host, &ToHost::Pause)?;
    let held_at = |f: &FromHost| match f {
        FromHost::JobProgress(p) if p.run == "cr_game" => Some(p.clone()),
        _ => None,
    };
    let (paused, since) = loop {
        let (f, at) = frame(&mut host)?;
        if let Some(p) = held_at(&f)
            && p.paused
        {
            break (p, at);
        }
    };
    std::thread::sleep(Duration::from_secs(2));
    send(&mut host, &ToHost::Resume)?;
    let mut resumed: Option<(ProgressFrame, Instant)> = None;
    let mut ends = BTreeMap::new();
    while ends.len() < 2 {
        let (f, at) = frame(&mut host)?;
        if let Some(p) = held_at(&f) {
            ensure(p.paused || p.round == paused.round || resumed.is_some(), || {
                format!("the round moved while paused: {paused:?} then {p:?}")
            })?;
            if !p.paused && resumed.is_none() {
                resumed = Some((p, at));
            }
        }
        if let Some(run) = f.run().map(str::to_owned)
            && matches!(f, FromHost::JobResult(_) | FromHost::JobFailed(_))
        {
            ends.insert(run, f);
        }
    }
    let (resumed, back) = resumed.ok_or("no progress when the game was let go")?;
    ensure(
        resumed.round == paused.round && back.duration_since(since) >= Duration::from_secs(2),
        || format!("paused {paused:?}, resumed {resumed:?} after {:?}", back.duration_since(since)),
    )?;
    ensure(resumed.paused_s >= 1.9, || format!("{resumed:?}"))?;
    let FromHost::JobResult(game) = &ends["cr_game"] else {
        return fail(format!("{:?}", ends["cr_game"]));
    };
    let (_, _, want) = job.expected(Ruleset::shared())?;
    ensure(game.outcome.result_hash == want && game.outcome.paused_s >= 1.9, || {
        format!("{:?}", game.outcome.paused_s)
    })?;
    let FromHost::JobResult(burn) = &ends["cr_ok"] else {
        return fail(format!("the paused burn: {:?}", ends["cr_ok"]));
    };
    ensure(burn.outcome.paused_s >= 1.9 && burn.outcome.wall_s > 3.0, || {
        format!("{:?}", burn.outcome)
    })?;
    // No pause: the deadline holds.
    send(&mut host, &test_offer("cr_late", BURN, &json!({"seconds": 3.0}), 1.5)?)?;
    let frames = until_end(&mut host, "cr_late")?;
    match frames.last().map(|(f, _)| f) {
        Some(FromHost::JobFailed(f)) if f.code == FailCode::Deadline => Ok(()),
        other => fail(format!("{other:?}")),
    }
}

/// Gate 4: an abort ends a run within a second.
fn an_abort_ends_a_run_within_a_second() -> Outcome {
    let req = Requested { threads: 1, cpu_percent: two_cores_percent(), memory_mb: 300 };
    let mut host = start(req, &[("CITAR_JOBS_TEST_STEP_MS", "20")])?;
    let set = GoldenSet::committed()?;
    let job = &set.jobs["reference-a"];
    send(&mut host, &offer("cr_ab", "reference_game", &job.spec, &job.spec_hash, 600.0))?;
    accepted(&mut host, "cr_ab")?;
    std::thread::sleep(Duration::from_millis(300));
    let asked = Instant::now();
    send(
        &mut host,
        &ToHost::JobAbort(Abort { run: "cr_ab".to_owned(), reason: "test".to_owned() }),
    )?;
    let frames = until_end(&mut host, "cr_ab")?;
    let (last, at) = frames.last().ok_or("nothing")?;
    ensure(matches!(last, FromHost::JobAborted(_)), || format!("{last:?}"))?;
    let took = at.duration_since(asked);
    println!("aborted in {took:?}");
    ensure(took < Duration::from_secs(1), || format!("{took:?}"))
}

/// The child mode `parent`: starts a host (playing a burn), prints its pid, and waits to be
/// killed.
fn parent_child() -> ExitCode {
    let req = Requested { threads: 1, cpu_percent: two_cores_percent(), memory_mb: 300 };
    let Ok(mut host) = start(req, &[]) else { return ExitCode::from(1) };
    let Ok(o) = test_offer("cr_orphan", BURN, &json!({"seconds": 30.0}), 60.0) else {
        return ExitCode::from(1);
    };
    if host.send(&o).is_err()
        || !matches!(host.next(WAIT), Some(Event::Frame(FromHost::JobAccept(_))))
    {
        return ExitCode::from(1);
    }
    println!("pid {}", host.pid());
    let _flushed = std::io::stdout().flush();
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// Gate 4: a host whose parent dies exits within two seconds, leaving no process.
fn a_host_whose_parent_dies_is_gone_within_two_seconds() -> Outcome {
    let mut parent = Command::new(std::env::current_exe()?)
        .env(CHILD, "parent")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut line = String::new();
    let out = parent.stdout.take().ok_or("no output")?;
    std::io::BufReader::new(out).read_line(&mut line)?;
    let pid: u32 = line
        .trim()
        .strip_prefix("pid ")
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| format!("the parent said {line:?}"))?;
    ensure(!gone(pid, Duration::ZERO), || format!("host {pid} is not running"))?;
    parent.kill()?;
    let killed = Instant::now();
    let _reaped = parent.wait();
    let ok = gone(pid, Duration::from_secs(2));
    println!("host {pid} gone {ok} in {:?}", killed.elapsed());
    ensure(ok, || format!("host {pid} outlived its parent by 2 s"))
}

// --- memory -------------------------------------------------------------------------------------

/// Gate 5: a 100 MB run's peak is counted within 10%, and two runs at once are counted apart.
fn peaks_are_counted_per_run() -> Outcome {
    let req = Requested { threads: 2, cpu_percent: two_cores_percent(), memory_mb: 512 };
    let mut host = start(req, &[])?;
    let near = |peak: f64, mb: f64| (peak - mb).abs() <= mb * 0.1;
    send(&mut host, &test_offer("cr_100", ALLOC, &json!({"mb": 100}), 60.0)?)?;
    let out = result_of(&until_end(&mut host, "cr_100")?)?;
    ensure(near(out.peak_mb, 100.0), || format!("a 100 MB run's peak: {}", out.peak_mb))?;
    let hold = json!({"mb": 60, "hold_s": 1.0});
    send(&mut host, &test_offer("cr_60", ALLOC, &hold, 60.0)?)?;
    let hold = json!({"mb": 150, "hold_s": 1.0});
    send(&mut host, &test_offer("cr_150", ALLOC, &hold, 60.0)?)?;
    let mut peaks = BTreeMap::new();
    while peaks.len() < 2 {
        if let (FromHost::JobResult(r), _) = frame(&mut host)? {
            peaks.insert(r.run.clone(), r.outcome.peak_mb);
        }
    }
    println!("peaks {peaks:?}");
    ensure(near(peaks["cr_60"], 60.0) && near(peaks["cr_150"], 150.0), || format!("{peaks:?}"))
}

// --- builds -------------------------------------------------------------------------------------

/// The child mode `fake-ready`: a host of another jobs build, as far as `ready` goes.
fn fake_ready() -> ExitCode {
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return ExitCode::from(1);
    }
    println!(
        "{{\"t\":\"ready\",\"limits\":{{\"cpu\":\"hard\",\"memory\":\"hard\"}},\"verifiable\":true,\
         \"jobs_build\":\"ffffffffffff\",\"threads\":1,\"cgroup\":null,\"cpu_s\":0.0,\"pid\":{},\
         \"counting\":true}}",
        std::process::id()
    );
    let _flushed = std::io::stdout().flush();
    let mut rest = String::new();
    while std::io::stdin().read_line(&mut rest).is_ok_and(|n| n > 0) {
        rest.clear();
    }
    ExitCode::SUCCESS
}

/// Gate 6: a host told another jobs build refuses to start; a parent refuses a `ready` of another
/// build; a host whose canary (either check) or self-check is forced off says it is not
/// verifiable and refuses every job.
fn other_builds_and_a_failed_canary_are_refused() -> Outcome {
    let req = Requested { threads: 1, cpu_percent: two_cores_percent(), memory_mb: 300 };
    let refused = parent::spawn(&program(&[]), wrapper(), &req, "000000000000", WAIT);
    match refused {
        Err(SpawnError::Refused(f)) if f.code == FatalCode::Build => {}
        other => return fail(format!("another build: {other:?}")),
    }
    let mut fake = program(&[]);
    fake.env = vec![(OsString::from(CHILD), OsString::from("fake-ready"))];
    match parent::spawn(&fake, Wrapper::Direct, &req, &mine(), WAIT) {
        Err(SpawnError::Build { got, .. }) if got == "ffffffffffff" => {}
        other => return fail(format!("a ready of another build: {other:?}")),
    }
    for (switch, injected, named) in [
        ("CITAR_JOBS_TEST_CANARY", "flushed", "canary"),
        ("CITAR_JOBS_TEST_CANARY", "read_as_zero", "canary"),
        ("CITAR_JOBS_TEST_SELF_CHECK", "fail", "self-check"),
    ] {
        let mut host = start(req, &[(switch, injected)])?;
        let r = host.ready().clone();
        ensure(!r.verifiable && r.reason.as_deref().is_some_and(|w| w.contains(named)), || {
            format!("{injected}: {r:?}")
        })?;
        send(&mut host, &test_offer("cr_x", BURN, &json!({"seconds": 1.0}), 60.0)?)?;
        match about(&mut host, "cr_x")?.0 {
            FromHost::JobReject(Reject { reason: RejectReason::Unverifiable, .. }) => {}
            other => return fail(format!("{injected}: {other:?}")),
        }
    }
    Ok(())
}

// --- fuzz ---------------------------------------------------------------------------------------

/// P4.6: 2,000 mutated lab and ranking specs and 2,000 mutated reference specs through a host's
/// standard input: never a death; a spec the parser refuses is `bad_spec`, one it takes (sent with
/// its hash) is `busy`, as the host's one thread is taken.
fn mutated_specs_through_standard_input() -> Outcome {
    let req = Requested { threads: 1, cpu_percent: two_cores_percent(), memory_mb: 300 };
    let mut host = start(req, &[])?;
    send(&mut host, &test_offer("cr_hold", ALLOC, &json!({"mb": 1, "hold_s": 60.0}), 600.0)?)?;
    accepted(&mut host, "cr_hold")?;
    let rules = Ruleset::shared();
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let (lab, reference) = (common::lab(), common::reference());
    let mut want = BTreeMap::new();
    for i in 0..4000u32 {
        let (kind, base, paths) = match i % 4 {
            0 => (Kind::LabGame, &lab, 400),
            1 => (Kind::RankingGame, &lab, 400),
            _ => (Kind::ReferenceGame, &reference, 100),
        };
        let r = next();
        let spec = common::mutate(
            base,
            usize::try_from(r % paths).unwrap_or(0),
            (r >> 16) as u8,
            (r >> 24) as u8,
            next(),
        );
        let (hash, reason) = match check_spec(rules, kind, &spec, None) {
            Ok(c) => (c.hash.to_hex(), RejectReason::Busy),
            Err(_) => ("0".repeat(64), RejectReason::BadSpec),
        };
        let run = format!("cr_f{i}");
        send(&mut host, &offer(&run, kind.name(), &spec, &hash, 60.0))?;
        want.insert(run, reason);
    }
    let mut counts = BTreeMap::new();
    let mut seen = 0;
    while seen < want.len() {
        match frame(&mut host)?.0 {
            FromHost::JobReject(r) => {
                let expected = want.get(&r.run).ok_or_else(|| format!("{r:?}"))?;
                ensure(r.reason == *expected, || format!("{r:?}, want {expected:?}"))?;
                *counts.entry(format!("{:?}", r.reason)).or_insert(0u32) += 1;
                seen += 1;
            }
            FromHost::Heartbeat(_) | FromHost::JobProgress(_) => {}
            other => return fail(format!("{other:?}")),
        }
    }
    println!("{counts:?}");
    ensure(counts.len() == 2, || format!("both answers reached: {counts:?}"))?;
    send(&mut host, &ToHost::JobAbort(Abort { run: "cr_hold".to_owned(), reason: String::new() }))?;
    let end = until_end(&mut host, "cr_hold")?;
    ensure(matches!(end.last(), Some((FromHost::JobAborted(_), _))), || format!("{end:?}"))
}

fn main() -> ExitCode {
    match std::env::var(CHILD).as_deref() {
        Ok("host") => {
            let code = citar_jobs::host::main(std::io::stdin().lock(), std::io::stdout());
            return ExitCode::from(u8::try_from(code).unwrap_or(1));
        }
        Ok("fake-ready") => return fake_ready(),
        Ok("parent") => return parent_child(),
        _ => {}
    }
    let args = Arguments::from_args();
    let t = |name: &str, f: fn() -> Outcome| Trial::test(name, f);
    let trials = vec![
        t("limits::ready_says_how_the_limits_hold", ready_says_how_the_limits_hold),
        t("limits::cpu_is_held_to_its_share", cpu_is_held_to_its_share),
        t(
            "limits::a_memory_death_is_classified_and_the_next_host_plays",
            a_memory_death_is_classified_and_the_next_host_plays,
        ),
        t(
            "limits::without_the_scope_linux_still_holds_memory",
            without_the_scope_linux_still_holds_memory,
        ),
        t("results::do_not_depend_on_the_limits", results_do_not_depend_on_the_limits),
        t(
            "ending::a_pause_holds_a_run_and_extends_its_deadline",
            a_pause_holds_a_run_and_extends_its_deadline,
        ),
        t("ending::an_abort_ends_a_run_within_a_second", an_abort_ends_a_run_within_a_second),
        t(
            "ending::a_host_whose_parent_dies_is_gone_within_two_seconds",
            a_host_whose_parent_dies_is_gone_within_two_seconds,
        ),
        t("memory::peaks_are_counted_per_run", peaks_are_counted_per_run),
        t(
            "builds::other_builds_and_a_failed_canary_are_refused",
            other_builds_and_a_failed_canary_are_refused,
        ),
        t("fuzz::mutated_specs_through_standard_input", mutated_specs_through_standard_input),
    ];
    libtest_mimic::run(&args, trials).exit_code()
}
