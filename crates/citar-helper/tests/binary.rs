//! The binary's command line: `--version` prints the build and exits 0; a command line it does not
//! take exits 2, as argparse did; `setup` asks the first-run questions and saves them owner-only.
//! The connection's tests are `tests/real_binary.rs`.

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
    assert!(line.contains("protocol 2") && line.contains("jobs build none"), "{line}");
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
