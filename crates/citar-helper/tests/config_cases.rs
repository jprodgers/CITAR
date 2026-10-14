//! Compatibility with protocol 1's configuration (design/phase4-helper.md P4.3.2; package 4-03,
//! gate 1): every case of `tests/data/helper_config_cases.json`, recorded from the Python
//! worker's `build_config` (`python -m tests.test_helper_config_cases --record`), gives the Rust
//! helper the same effective settings, or the same refusal; and a 0.1.5 `helper.json` lends no
//! CPU. Each case reads only its own directory and the variables it names, never the process's
//! environment.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use citar_helper::cli::Cli;
use citar_helper::config::{Sources, resolve};
use citar_helper::home::Home;
use clap::Parser as _;
use serde_json::Value;

fn table() -> Value {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/data/helper_config_cases.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).expect("the table is JSON")
}

fn slashed(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// As the table keeps a path-bearing string: forward slashes, the case's directory as `{dir}`.
fn clean(text: &str, dir: &Path) -> String {
    text.replace('\\', "/").replace(&slashed(dir), "{dir}")
}

fn subst(v: &str, dir: &Path) -> String {
    v.replace("{dir}", &slashed(dir))
}

fn case_dir(i: usize) -> PathBuf {
    let d = std::env::temp_dir().join(format!("citar-helper-cfg-{}-{i}", std::process::id()));
    let _fresh = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("a case directory");
    d
}

#[test]
fn every_recorded_configuration_reads_as_protocol_one_read_it() {
    let table = table();
    let fixes = table["fixes"].as_object().expect("fixes");
    let cases = table["cases"].as_array().expect("cases");
    assert!(cases.len() >= 30, "{} cases", cases.len());
    let mut failures = Vec::new();
    let mut migrated = 0;
    for (i, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().expect("a name");
        let dir = case_dir(i);
        if let Some(files) = case.get("files").and_then(Value::as_object) {
            for (file, content) in files {
                let text = match content {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                std::fs::write(dir.join(file), text).expect("a case file");
            }
        }
        let mut argv = vec!["citar-helper".to_owned()];
        for a in case.get("args").and_then(Value::as_array).into_iter().flatten() {
            argv.push(subst(a.as_str().expect("an argument"), &dir));
        }
        let cli = Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("{name}: {e}"));
        let env: BTreeMap<String, OsString> = case
            .get("env")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), OsString::from(v.as_str().unwrap_or_default())))
                    .collect()
            })
            .unwrap_or_default();
        let lookup = |k: &str| env.get(k).cloned();
        let home = Home::at(&dir);
        let got = resolve(&Sources { opts: &cli.opts, env: &lookup, home: &home }, None);
        let want = match case.get("fix") {
            Some(Value::String(f)) => {
                assert!(fixes.contains_key(f), "{name}: the fix {f} is not in fixes");
                case["helper"].clone()
            }
            _ => {
                let mut w = serde_json::Map::new();
                for k in ["expect", "error"] {
                    if let Some(v) = case.get(k) {
                        w.insert(k.to_owned(), v.clone());
                    }
                }
                Value::Object(w)
            }
        };
        let got_value = match &got {
            Ok(cfg) => {
                let text = clean(&cfg.compat().to_string(), &dir);
                serde_json::json!({"expect": serde_json::from_str::<Value>(&text).expect("json")})
            }
            Err(e) => serde_json::json!({"error": clean(e, &dir)}),
        };
        if got_value != want {
            failures.push(format!("{name}:\n  got    {got_value}\n  wanted {want}"));
        }
        if case.get("migrated") == Some(&Value::Bool(true)) {
            migrated += 1;
            let cfg = got.as_ref().expect("a migrated file resolves");
            assert!(!cfg.cpu.enabled, "{name}: a 0.1.5 helper.json lends no CPU");
        }
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }
    assert!(migrated >= 1, "the table holds a migrated helper.json");
    assert!(
        failures.is_empty(),
        "{} of {} differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
