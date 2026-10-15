//! The `jobs` golden set (design/phase4-helper.md P4.4.5), with its own harness: run it in both
//! profiles, `cargo nextest run -p citar-jobs` (the `ci` profile's checks in a dev build) and
//! `cargo nextest run -p citar-jobs --cargo-profile release`; `determinism.yml` runs it on five
//! targets in `ci` and `release`. It builds the engine without `legacy` or `test-ops`, unlike
//! citar-testkit's goldens.
//!
//! - `job::NAME` plays each job and holds everything to `golden/jobs.json`: the canonical spec
//!   and its hash, the bots, the result and its hash, the chain's head, rounds and digests;
//! - `determinism::*`: one job twice in one process and in two processes (this binary started
//!   again with `CITAR_JOBS_GOLDEN_CHILD=NAME`, which plays NAME and prints its hashes), and a
//!   lab and a reference job slowed between seat steps give the same hashes (no clock in a
//!   result, 4-07's gate 3);
//! - `self_check`: the self-check a job host runs passes, on the set's shortest job.
//!
//! **Blessing** (a deliberate change of what a job computes, as `cargo golden bless`):
//! `CITAR_BLESS_JOBS=1 cargo test -p citar-jobs --test golden -- bless` rewrites the file from
//! this build; commit it saying why. The bless trial is ignored without the variable.

use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

use citar_engine::rules::Ruleset;
use citar_jobs::golden::{GoldenJob, GoldenSet, SELF_CHECK};
use citar_jobs::{
    Flow, Kind, Output, Progress, Unattended, chain_check, check_spec, jobs_build, result_hash, run,
};
use libtest_mimic::{Arguments, Failed, Trial};
use serde_json::{Value, json};

const CHILD: &str = "CITAR_JOBS_GOLDEN_CHILD";
const BLESS: &str = "CITAR_BLESS_JOBS";

fn rules() -> &'static Ruleset {
    Ruleset::shared()
}

/// Plays one job of the set.
fn play(job: &GoldenJob) -> Result<(citar_jobs::Checked, Output), String> {
    let checked = check_spec(rules(), job.kind, &job.spec, None).map_err(|e| e.to_string())?;
    let out = run(rules(), &checked, &mut Unattended).map_err(|e| e.to_string())?;
    Ok((checked, out))
}

fn result_value(out: &Output) -> Result<Value, String> {
    serde_json::to_value(&out.result).map_err(|e| e.to_string())
}

fn same<T: PartialEq + std::fmt::Debug>(what: &str, got: T, want: T) -> Result<(), Failed> {
    if got == want { Ok(()) } else { Err(format!("{what}: got {got:?}, want {want:?}").into()) }
}

/// A job played, against the committed file.
fn check_job(name: &str, job: &GoldenJob) -> Result<(), Failed> {
    let (checked, out) = play(job)?;
    same("the canonical spec", checked.canonical_value(), job.spec.clone())?;
    same("the spec hash", checked.hash.to_hex(), job.spec_hash.clone())?;
    same("the bots", checked.bots(), job.bots.clone())?;
    same("the rounds", out.chain.rounds, job.rounds)?;
    same("the digests", &out.chain.digests, &job.digests)?;
    same("the chain head", &out.chain.head, &job.chain_head)?;
    same("the result", result_value(&out)?, job.result.clone())?;
    same("the result hash", &out.result_hash, &job.result_hash)?;
    // The committed result read back as the server reads one gives the committed hash, and the
    // committed chain folds to its head.
    same(
        "the committed result's hash",
        result_hash(job.kind, &job.result)?,
        job.result_hash.clone(),
    )?;
    chain_check(&checked.hash, &job.chain()).map_err(|e| format!("{name}: {e}"))?;
    Ok(())
}

/// The hashes one run gives, as a line.
fn hashes(out: &Output) -> Value {
    json!({"result_hash": out.result_hash, "chain_head": out.chain.head, "rounds": out.chain.rounds})
}

/// The child: play NAME and print its hashes and this build's jobs build.
fn child(name: &str) {
    let set = GoldenSet::committed().unwrap_or_else(|e| panic!("{e}"));
    let job = set.jobs.get(name).unwrap_or_else(|| panic!("no golden job {name}"));
    let (_, out) = play(job).unwrap_or_else(|e| panic!("{name}: {e}"));
    let mut line = hashes(&out);
    line["jobs_build"] = json!(jobs_build(rules()));
    println!("{line}");
}

fn in_another_process(name: &str) -> Result<Value, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = Command::new(exe).env(CHILD, name).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("the child failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().rev().find(|l| l.starts_with('{')).ok_or("no line from the child")?;
    serde_json::from_str(line).map_err(|e| e.to_string())
}

/// One job twice here and twice in other processes: one chain head, one result hash.
fn twice(set: &GoldenSet) -> Result<(), Failed> {
    let job = &set.jobs[SELF_CHECK];
    let (_, a) = play(job)?;
    let (_, b) = play(job)?;
    same("twice in one process", hashes(&b), hashes(&a))?;
    same("twice in one process: the digests", &b.chain.digests, &a.chain.digests)?;
    let mut here = hashes(&a);
    here["jobs_build"] = json!(jobs_build(rules()));
    let one = in_another_process(SELF_CHECK)?;
    let two = in_another_process(SELF_CHECK)?;
    same("in two processes", &two, &one)?;
    same("in another process and in this one", &one, &here)?;
    Ok(())
}

/// A lab job and a reference job slowed between seat steps give the hashes they give unslowed:
/// no clock reaches a result.
fn slowed(set: &GoldenSet) -> Result<(), Failed> {
    for name in [SELF_CHECK, "reference-a"] {
        let job = &set.jobs[name];
        let checked = check_spec(rules(), job.kind, &job.spec, None)?;
        let mut steps = 0u32;
        let mut sleepy = |_: &Progress| {
            steps += 1;
            std::thread::sleep(Duration::from_millis(2));
            Flow::Go
        };
        let out = run(rules(), &checked, &mut sleepy)?;
        if steps < job.rounds {
            return Err(format!("{name}: the control was asked only {steps} times").into());
        }
        same(&format!("{name} slowed: the result hash"), &out.result_hash, &job.result_hash)?;
        same(&format!("{name} slowed: the chain head"), &out.chain.head, &job.chain_head)?;
    }
    Ok(())
}

/// Rewrites the file from this build.
fn bless(set: &GoldenSet) -> Result<(), Failed> {
    let mut jobs = BTreeMap::new();
    for (name, job) in &set.jobs {
        let (checked, out) = play(job)?;
        if job.kind != checked.kind {
            return Err(format!("{name}: kind").into());
        }
        jobs.insert(
            name.clone(),
            GoldenJob {
                kind: job.kind,
                spec: checked.canonical_value(),
                spec_hash: checked.hash.to_hex(),
                bots: checked.bots(),
                result: result_value(&out)?,
                result_hash: out.result_hash.clone(),
                chain_head: out.chain.head.clone(),
                rounds: out.chain.rounds,
                digests: out.chain.digests.clone(),
            },
        );
    }
    let about = "The jobs golden set (design/phase4-helper.md P4.4.5): four short jobs, each with \
                 its canonical spec and spec hash, the bot versions it names, its result and \
                 result hash, and its chain (head, rounds and every round's digest). Checked by \
                 crates/citar-jobs/tests/golden.rs in the ci and release profiles, on five targets \
                 in determinism.yml; the self-check plays duel-a. Blessed with CITAR_BLESS_JOBS=1 \
                 cargo test -p citar-jobs --test golden -- bless, in a commit saying why.";
    let blessed = GoldenSet { about: about.to_owned(), jobs };
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("golden/jobs.json");
    std::fs::write(&path, blessed.text()?).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(())
}

fn main() {
    if let Ok(name) = std::env::var(CHILD) {
        child(&name);
        return;
    }
    let args = Arguments::from_args();
    let set = match GoldenSet::committed() {
        Ok(set) => set,
        Err(e) => {
            let trial = Trial::test("load", move || Err(Failed::from(e)));
            libtest_mimic::run(&args, vec![trial]).exit();
        }
    };
    let blessing = std::env::var_os(BLESS).is_some();
    let mut trials: Vec<Trial> = set
        .jobs
        .iter()
        .map(|(name, job)| {
            let (name, job) = (name.clone(), job.clone());
            Trial::test(format!("job::{name}"), move || check_job(&name, &job))
        })
        .collect();
    let s = set.clone();
    trials.push(Trial::test("determinism::twice_here_and_in_two_processes", move || twice(&s)));
    let s = set.clone();
    trials.push(Trial::test("determinism::slowed_between_seat_steps", move || slowed(&s)));
    let s = set.clone();
    trials.push(Trial::test("set::the_self_check_plays_the_shortest_job", move || {
        let shortest = s.jobs.iter().min_by_key(|(n, j)| (j.rounds, n.as_str())).map(|(n, _)| n);
        same("the shortest job", shortest.map(String::as_str), Some(SELF_CHECK))?;
        same("the kinds", s.jobs.values().filter(|j| j.kind == Kind::ReferenceGame).count(), 1)?;
        Ok(())
    }));
    trials.push(Trial::test("self_check", || {
        citar_jobs::self_check(rules()).map(|_| ()).map_err(Failed::from)
    }));
    let s = set;
    trials.push(Trial::test("bless", move || bless(&s)).with_ignored_flag(!blessing));
    libtest_mimic::run(&args, trials).exit();
}
