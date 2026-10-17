//! The binary's command line: `--version` prints the build and exits 0; `--self-test` runs the
//! job host's canary and self-check (4-08); `job-host` is the job host, played here from the
//! parent's side (`citar_jobs::host::parent`); a command line it does not take exits 2, as
//! argparse did; `setup` asks the first-run questions and saves them owner-only. The connection's
//! tests are `tests/real_binary.rs`.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_citar-helper");

#[test]
fn version_prints_the_build_and_exits_zero() {
    // An empty home of its own: a build without the development guard would otherwise look for
    // a key in the per-user CITAR folder.
    let home = std::env::temp_dir().join(format!("citar-helper-version-{}", std::process::id()));
    let out = Command::new(BIN)
        .arg("--version")
        .env("CITAR_DATA_DIR", &home)
        .output()
        .expect("the binary runs");
    assert!(out.status.success(), "{out:?}");
    let line = String::from_utf8(out.stdout).expect("UTF-8");
    let want = citar_helper::version::build_info().line();
    assert_eq!(line.trim_end(), want);
    let jobs = citar_jobs::jobs_build(citar_engine::rules::Ruleset::shared());
    assert!(line.contains("protocol 2") && line.contains(&format!("jobs build {jobs}")), "{line}");
}

/// `--self-test` prints the version, the build, the jobs build and the target, runs the canary
/// and the self-check, and passes (4-08's gate 6).
#[test]
fn self_test_prints_the_build_and_passes() {
    let out = Command::new(BIN).arg("--self-test").output().expect("the binary runs");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{out:?}");
    let info = citar_helper::version::build_info();
    let first = text.lines().next().unwrap_or_default();
    assert_eq!(first, info.line());
    let jobs = info.jobs_build.clone().expect("a jobs build");
    for part in [info.version, &info.build, &jobs, info.target] {
        assert!(first.contains(part), "{part} in {first}");
    }
    assert!(text.contains("canary: ok") && text.contains("self-check: ok (duel-a"), "{text}");
    assert!(text.trim_end().ends_with("self-test: passed"), "{text}");
}

/// `citar-helper job-host` is a job host: started as the helper will start it, it holds its
/// limits as this OS can, takes the parent's jobs build and plays a golden job to its committed
/// hash; told another jobs build, it refuses to start.
#[test]
fn job_host_plays_a_job_for_its_parent() {
    use citar_jobs::host::parent::{self, Event, Program, SpawnError, Wrapper};
    use citar_jobs::host::{FromHost, Offer, ToHost};
    use citar_jobs::limits::{Hardness, Requested};

    let rules = citar_engine::rules::Ruleset::shared();
    let program = Program { program: BIN.into(), args: vec!["job-host".into()], env: Vec::new() };
    let cpus = citar_jobs::limits::machine_cpus();
    let req =
        Requested { threads: 1, cpu_percent: (200 / cpus.max(1)).clamp(1, 100), memory_mb: 300 };
    let build = citar_jobs::jobs_build(rules);
    let wait = std::time::Duration::from_secs(120);
    let mut host = parent::spawn(&program, Wrapper::detect(), &req, &build, wait).expect("ready");
    let ready = host.ready().clone();
    assert!(ready.verifiable && ready.counting && ready.jobs_build == build, "{ready:?}");
    if cfg!(windows) {
        assert_eq!((ready.limits.cpu, ready.limits.memory), (Hardness::Hard, Hardness::Hard));
    }
    let set = citar_jobs::golden::GoldenSet::committed().expect("the set");
    let job = &set.jobs[citar_jobs::golden::SELF_CHECK];
    let (_, _, want) = job.expected(rules).expect("expected");
    host.send(&ToHost::JobOffer(Offer {
        run: "cr_helper".to_owned(),
        job: None,
        kind: job.kind.name().to_owned(),
        spec: job.spec.clone(),
        spec_hash: job.spec_hash.clone(),
        jobs_build: build.clone(),
        deadline_s: 600.0,
        mem_mb: 0,
        priority: 0,
    }))
    .expect("sent");
    let result = loop {
        match host.next(wait) {
            Some(Event::Frame(FromHost::JobResult(r))) => break r,
            Some(Event::Frame(FromHost::JobReject(r))) => panic!("{r:?}"),
            Some(Event::Frame(FromHost::JobFailed(f))) => panic!("{f:?}"),
            Some(Event::Frame(_)) => {}
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(result.outcome.result_hash, want);
    assert_eq!(result.outcome.chain.head, job.chain_head);
    assert!(result.outcome.peak_mb > 0.0, "{:?}", result.outcome);
    assert!(host.close().is_some_and(|s| s.success()));
    match parent::spawn(&program, Wrapper::Direct, &req, "000000000000", wait) {
        Err(SpawnError::Refused(f)) => assert!(f.message.contains("000000000000"), "{f:?}"),
        other => panic!("another jobs build: {other:?}"),
    }
}

#[test]
fn a_command_line_it_does_not_take_exits_two() {
    for args in [&["--bogus"][..], &["--provider", "vllm"], &["scan-everything"], &["--no"]] {
        let out = Command::new(BIN).args(args).output().expect("the binary runs");
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
    }
}

fn setup(home: &std::path::Path, answers: &str) -> std::process::Output {
    use std::io::Write as _;
    let mut child = Command::new(BIN)
        .arg("--home")
        .arg(home)
        .arg("setup")
        .env("CITAR_DATA_DIR", home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary runs");
    child.stdin.take().expect("stdin").write_all(answers.as_bytes()).expect("answered");
    child.wait_with_output().expect("it ends")
}

#[test]
fn setup_asks_saves_owner_only_and_keeps_the_other_settings() {
    let dir = std::env::temp_dir().join(format!("citar-helper-setup-{}", std::process::id()));
    let _fresh = std::fs::remove_dir_all(&dir);
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("home");
    let file = home.join("helper.json");
    std::fs::write(
        &file,
        r#"{"base_url": "http://gpu:1234/v1", "models": ["m"], "token": "cwt_old"}"#,
    )
    .expect("a 0.1.5 helper.json");
    let read = || -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(&file).expect("read")).expect("json")
    };

    let out = setup(&home, "citar.example.com\ncwt_new\ny\n");
    assert!(out.status.success(), "{out:?}");
    let v = read();
    assert_eq!(v["server_url"], "https://citar.example.com", "https:// added, as protocol 1 did");
    assert_eq!(v["token"], "cwt_new");
    assert_eq!(v["cpu"]["enabled"], true);
    assert_eq!(
        (v["base_url"].as_str(), v["models"][0].as_str()),
        (Some("http://gpu:1234/v1"), Some("m"))
    );
    assert_eq!(
        citar_jobs::sys::exposure(&file).expect("read"),
        None,
        "owner-only: it holds the token"
    );
    let asked = String::from_utf8_lossy(&out.stdout);
    assert!(asked.contains("Lend this machine's spare CPU to the server? [y/N]"), "{asked}");

    // Again: an empty answer keeps the server and the CPU choice; the token is new.
    let out = setup(&home, "\ncwt_newer\n\n");
    assert!(out.status.success(), "{out:?}");
    let v = read();
    assert_eq!(
        (v["server_url"].as_str(), v["token"].as_str()),
        (Some("https://citar.example.com"), Some("cwt_newer"))
    );
    assert_eq!(v["cpu"]["enabled"], true);

    // No token: refused, nothing changed.
    let out = setup(&home, "\n\n");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert_eq!(read()["token"], "cwt_newer");
    std::fs::remove_dir_all(&dir).expect("cleaned");
}
