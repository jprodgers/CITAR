//! The shared fixtures of protocol 2 from the helper's side (package 4-01, gates 1, 2 and 4):
//! every valid frame parses and serializes back to its expectation, every invalid frame is
//! refused with its fixture's code and path, every ignored frame is ignored, PROTOCOL.md's frame
//! table is exactly the types this side knows, and every transcript loads, with every frame in it
//! realizable into a frame that passes its checks and every acknowledgement of settings what a
//! correct helper sends. `tests/test_helper_protocol.py` runs the same files from the server's
//! side.

use std::collections::BTreeSet;

use citar_helper::conformance::matcher::{
    Bindings, Committed, Golden, Matcher, NoGolden, json_eq, realize,
};
use citar_helper::conformance::transcript::Action;
use citar_helper::conformance::{
    FixtureKind, canonical_examples, crate_dir, frame_fixtures, frame_table, transcripts,
};
use citar_helper::protocol::{
    ErrorCode, FrameSet, HelperFrame, Parsed, ProtocolError, Sender, ServerFrame,
    parse_helper_frame, parse_server_frame,
};
use serde_json::{Map, Value, json};

/// A frame from `from`, parsed and serialized again; or why it was refused; or the type ignored.
fn parse_from(from: Sender, text: &str) -> Result<Result<Value, String>, ProtocolError> {
    fn back<F: FrameSet>(p: Parsed<F>) -> Result<Value, String> {
        match p {
            Parsed::Frame { ts, frame } => Ok(frame.to_value(ts)),
            Parsed::Ignored { t } => Err(t),
        }
    }
    match from {
        Sender::Helper => parse_helper_frame(text).map(back),
        Sender::Server => parse_server_frame(text).map(back),
    }
}

#[test]
fn every_valid_frame_parses_and_serializes_back_to_its_expectation() {
    let fixtures = frame_fixtures("valid").expect("the valid fixtures read");
    assert!(fixtures.len() >= 40, "{} valid fixtures", fixtures.len());
    for f in &fixtures {
        let FixtureKind::Valid { expect, .. } = &f.kind else { unreachable!() };
        let back = parse_from(f.from, &f.text())
            .unwrap_or_else(|e| panic!("{}: refused: {e}", f.name))
            .unwrap_or_else(|t| panic!("{}: ignored as {t}", f.name));
        assert!(json_eq(&back, expect), "{}:\n  got    {back}\n  wanted {expect}", f.name);
    }
}

#[test]
fn every_invalid_frame_is_refused_with_its_code_and_path() {
    let fixtures = frame_fixtures("invalid").expect("the invalid fixtures read");
    assert!(fixtures.len() >= 60, "{} invalid fixtures", fixtures.len());
    let mut codes = BTreeSet::new();
    for f in &fixtures {
        let FixtureKind::Invalid { code, path, .. } = &f.kind else { unreachable!() };
        match parse_from(f.from, &f.text()) {
            Err(e) => {
                assert_eq!((e.code, e.path.as_str()), (*code, path.as_str()), "{}: {e}", f.name);
            }
            Ok(back) => panic!("{}: accepted as {back:?}, not refused {code} at {path:?}", f.name),
        }
        codes.insert(*code);
    }
    assert_eq!(codes, ErrorCode::ALL.into_iter().collect(), "every code has an invalid fixture");
}

#[test]
fn every_ignored_frame_is_ignored() {
    let fixtures = frame_fixtures("ignored").expect("the ignored fixtures read");
    assert!(!fixtures.is_empty());
    for f in &fixtures {
        match parse_from(f.from, &f.text()) {
            Ok(Err(_)) => {}
            other => panic!("{}: not ignored: {other:?}", f.name),
        }
    }
}

/// Each type each side sends has a canonical example, and each type with fields an invalid
/// fixture of its own.
#[test]
fn every_frame_type_has_a_canonical_example_and_its_own_invalid_fixture() {
    let canonical = canonical_examples().expect("the valid fixtures read");
    let invalid = frame_fixtures("invalid").expect("the invalid fixtures read");
    for from in [Sender::Helper, Sender::Server] {
        for t in from.types() {
            assert!(
                canonical.contains_key(&(from, (*t).to_owned())),
                "no {}.{t}.json",
                from.as_str()
            );
            let fieldless = ["ping", "pong", "rescan", "jobs_resume"].contains(t);
            let prefix = format!("{}.{t}.", from.as_str());
            if !fieldless {
                assert!(
                    invalid.iter().any(|f| f.name.starts_with(&prefix)),
                    "no invalid fixture {prefix}*.json"
                );
            }
        }
    }
}

/// Gate 2: the frame types this side knows are PROTOCOL.md's table, row for row.
#[test]
fn the_frame_table_is_exactly_the_types_this_side_knows() {
    let md = std::fs::read_to_string(crate_dir().join("PROTOCOL.md")).expect("PROTOCOL.md reads");
    let table: BTreeSet<(String, Sender)> =
        frame_table(&md).expect("the table reads").into_iter().collect();
    let known: BTreeSet<(String, Sender)> = [Sender::Helper, Sender::Server]
        .into_iter()
        .flat_map(|s| s.types().iter().map(move |t| ((*t).to_owned(), s)))
        .collect();
    let rows = frame_table(&md).expect("the table reads").len();
    assert_eq!(rows, table.len(), "a row appears twice in the frame table");
    assert_eq!(table, known, "PROTOCOL.md's frame table against HelperFrame and ServerFrame");
    assert_eq!(
        <HelperFrame as FrameSet>::TYPES.len() + <ServerFrame as FrameSet>::TYPES.len(),
        rows
    );
}

/// Gate 4: every transcript loads, and every frame in it, realized as the scripted side would
/// send it (golden values from the committed sets, 4-07's `jobs`; the rest from the canonical
/// examples), passes its checks and reads back unchanged, so no field in a pattern is one the
/// protocol does not have; a frame sent broken on purpose is refused with its code.
#[test]
fn every_transcript_loads_and_its_frames_realize_into_valid_frames() {
    let all = transcripts().expect("the transcripts load");
    assert!(all.len() >= 60, "{} transcripts", all.len());
    let canonical = canonical_examples().expect("the valid fixtures read");
    let golden = Committed::load().expect("the golden sets read");
    let mut frames = 0;
    for t in &all {
        let mut bindings = Bindings::default();
        for (i, from, pattern, invalid) in t.frames() {
            let ty = pattern.get("t").and_then(Value::as_str).unwrap_or_default().to_owned();
            let example = canonical.get(&(from, ty));
            let real = realize(pattern, &mut bindings, &t.values, &golden, example)
                .unwrap_or_else(|e| panic!("{} step {i}: {e}", t.name));
            let got = parse_from(from, &real.to_string());
            match (invalid, got) {
                (None, Ok(Ok(back))) => {
                    assert!(
                        json_eq(&back, &real),
                        "{} step {i}: a field the protocol drops:\n  {real}\n  {back}",
                        t.name
                    );
                }
                (Some(code), Err(e)) => assert_eq!(e.code, code, "{} step {i}: {e}", t.name),
                (_, other) => {
                    panic!("{} step {i}: {real} gave {other:?}, invalid: {invalid:?}", t.name)
                }
            }
            frames += 1;
        }
    }
    assert!(frames > 300, "{frames} frames realized");
}

/// Every `<golden:SET/NAME[/FIELD]>` a transcript names resolves to a value of the committed sets
/// (4-01 left them to 4-07, which names the `jobs` set's entries), and a name the set lacks
/// does not.
#[test]
fn every_golden_reference_resolves() {
    fn walk(v: &Value, out: &mut BTreeSet<String>) {
        match v {
            Value::String(s) if s.starts_with("<golden:") => {
                out.insert(s.clone());
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            Value::Object(m) => m.values().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let dir = citar_helper::conformance::fixtures_dir().join("transcripts");
    let mut refs = BTreeSet::new();
    for entry in std::fs::read_dir(&dir).expect("the transcripts' folder") {
        let path = entry.expect("an entry").path();
        let text = std::fs::read_to_string(&path).expect("a transcript reads");
        walk(&serde_json::from_str(&text).expect("JSON"), &mut refs);
    }
    assert!(refs.len() >= 10, "{refs:?}");
    let golden = Committed::load().expect("the golden sets read");
    for r in &refs {
        let Some(Ok(Matcher::Golden { set, path })) = Matcher::parse(r) else {
            panic!("{r} is no golden matcher")
        };
        let v = golden.value(&set, &path).unwrap_or_else(|| panic!("{r} resolves to nothing"));
        assert!(!v.is_null(), "{r}");
    }
    let missing = ["duel-z".to_owned()];
    assert_eq!(golden.value("jobs", &missing), None);
    assert_eq!(golden.value("other", &["duel-a".to_owned()]), None);
    assert_eq!(NoGolden.value("jobs", &["duel-a".to_owned()]), None);
}

/// Every field table this side checks equals `protocol/v2/schema.json`, which the server's side
/// is held to as well: a bound written differently on one side fails here even where no fixture
/// reaches it. After a deliberate change, `CITAR_BLESS_SCHEMA=1` rewrites the file from this
/// side, and the server's test then shows where it differs.
#[test]
fn the_field_tables_are_the_committed_schema() {
    let path = citar_helper::conformance::fixtures_dir().join("schema.json");
    let ours = citar_helper::protocol::schema();
    if std::env::var_os("CITAR_BLESS_SCHEMA").is_some() {
        let text = serde_json::to_string_pretty(&ours).expect("the schema serializes");
        std::fs::write(&path, text + "\n").expect("schema.json is written");
    }
    let text = std::fs::read_to_string(&path).expect("protocol/v2/schema.json reads");
    let committed: Value = serde_json::from_str(&text).expect("schema.json is JSON");
    for (side, frames) in committed.as_object().expect("an object") {
        for (t, want) in frames.as_object().expect("an object per side") {
            let got = &ours[side.as_str()][t.as_str()];
            assert!(json_eq(got, want), "{side} {t}:\n  this side {got}\n  committed {want}");
        }
    }
    assert!(json_eq(&ours, &committed), "a frame type is missing on one side");
}

/// The vocabularies the transcript loader enforces are the ones PROTOCOL.md documents: the
/// background frames word for word, and every setup, endpoint and host key named there.
#[test]
fn the_loaders_vocabularies_are_protocol_md_s() {
    use citar_helper::conformance::transcript::{
        BACKGROUND, ENDPOINT_KEYS, HELPER_ACTS, HELPER_CONFIG, HELPER_HOST, HELPER_SETUP,
        HELPER_STATE, HOST_KEYS, HUB_ACTS, HUB_MACHINE, HUB_SETUP, HUB_STATE, RESPOND_KEYS,
    };
    let md = std::fs::read_to_string(crate_dir().join("PROTOCOL.md")).expect("PROTOCOL.md reads");
    let paragraph = |start: &str| -> String {
        let at = md.find(start).unwrap_or_else(|| panic!("PROTOCOL.md has no {start}"));
        let rest = &md[at..];
        rest[..rest.find("\n\n").unwrap_or(rest.len())].to_owned()
    };
    let ticked = |line: &str| -> Vec<String> {
        line.split('`').skip(1).step_by(2).map(str::to_owned).collect()
    };
    let background = paragraph("**Background frames.**");
    let first = background.lines().next().unwrap_or_default();
    assert_eq!(ticked(first.split(" come ").next().unwrap_or_default()), BACKGROUND);
    for (start, keys) in [
        (
            "**Setup keys**",
            [HELPER_SETUP, HELPER_CONFIG, HELPER_HOST, HUB_SETUP, HUB_MACHINE].concat(),
        ),
        ("**Endpoint steps**", [ENDPOINT_KEYS, RESPOND_KEYS].concat()),
        ("**Host steps**", HOST_KEYS.to_vec()),
        ("**Actions**", [HELPER_ACTS, HUB_ACTS].concat()),
        ("**State expectations**", [HELPER_STATE, HUB_STATE].concat()),
    ] {
        let text = paragraph(start);
        for key in keys {
            assert!(
                text.contains(&format!("`{key}`")) || text.contains(&format!("\"{key}\"")),
                "{start} does not name {key}"
            );
        }
    }
}

/// The acknowledgement a correct helper with `config` (a transcript's `setup.helper.config`) sends
/// for the site's settings (PROTOCOL.md, "Settings"): each figure the smaller of the site's and the
/// ceiling, the site's 0 meaning the ceiling; the kinds both list, in the order of `kind`; CPU work
/// on only when the site and the machine's own switch say on; model work on only when the site
/// says on and the helper has a model endpoint. `tests/helper_fixtures.py` `expected_ack` is the
/// server's copy, and 4-03's settings code is held to the same transcripts.
fn expected_ack(site: &Value, config: &Value) -> Value {
    const KINDS: [&str; 3] = ["lab_game", "ranking_game", "reference_game"];
    const ORDER: [&str; 7] = [
        "llm.enabled",
        "llm.slots",
        "cpu.enabled",
        "cpu.threads",
        "cpu.cpu_percent",
        "cpu.memory_mb",
        "cpu.work",
    ];
    let mut capped = Vec::new();
    let mut figure = |name: &'static str, asked: &Value, ceiling: u64| -> u64 {
        let asked = asked.as_u64().expect("a site figure");
        if asked == 0 {
            ceiling
        } else if ceiling < asked {
            capped.push(name);
            ceiling
        } else {
            asked
        }
    };
    let no_llm = config["no_llm"].as_bool().unwrap_or(false);
    let local = &config["cpu"];
    let on = |v: &Value| v.as_bool().expect("a switch");
    let slots = figure(
        "llm.slots",
        &site["llm"]["slots"],
        if no_llm { 0 } else { config["max_concurrent"].as_u64().unwrap_or(1) },
    );
    let threads =
        figure("cpu.threads", &site["cpu"]["threads"], local["threads"].as_u64().unwrap());
    let percent = figure(
        "cpu.cpu_percent",
        &site["cpu"]["cpu_percent"],
        local["cpu_percent"].as_u64().unwrap(),
    );
    let memory =
        figure("cpu.memory_mb", &site["cpu"]["memory_mb"], local["memory_mb"].as_u64().unwrap());
    let lists = |v: &Value, k: &str| v.as_array().is_some_and(|a| a.iter().any(|x| x == k));
    let site_work = &site["cpu"]["work"];
    if on(&site["llm"]["enabled"]) && no_llm {
        capped.push("llm.enabled");
    }
    if on(&site["cpu"]["enabled"]) && !on(&local["enabled"]) {
        capped.push("cpu.enabled");
    }
    if site_work.as_array().unwrap().iter().any(|k| !lists(&local["work"], k.as_str().unwrap())) {
        capped.push("cpu.work");
    }
    let work: Vec<&str> =
        KINDS.into_iter().filter(|k| lists(site_work, k) && lists(&local["work"], k)).collect();
    let capped: Vec<&str> = ORDER.into_iter().filter(|c| capped.contains(c)).collect();
    json!({
        "effective": {
            "llm": {"enabled": on(&site["llm"]["enabled"]) && !no_llm, "slots": slots},
            "cpu": {
                "enabled": on(&site["cpu"]["enabled"]) && on(&local["enabled"]),
                "threads": threads,
                "cpu_percent": percent,
                "memory_mb": memory,
                "work": work,
                "yield_to_llm": site["cpu"]["yield_to_llm"],
            },
        },
        "capped": capped,
    })
}

/// Whether a pattern holds no matcher: what the scripted side realizes is exactly it.
fn literal(v: &Value) -> bool {
    match v {
        Value::Object(m) => m.values().all(literal),
        Value::Array(a) => a.iter().all(literal),
        Value::String(s) => Matcher::parse(s).is_none(),
        _ => true,
    }
}

/// Every transcript that reaches a welcome states the site's settings (the hub's setup and the
/// welcome, the same object) and the helper's slots and CPU ceiling, and each acknowledgement the
/// helper plays is what those make by PROTOCOL.md's rules, written out in full: the helper's
/// settings code (4-03) is held to it, and the hub side's fake helper sends exactly that.
#[test]
fn every_acknowledgement_is_what_a_correct_helper_sends() {
    let mut acks = 0;
    for t in transcripts().expect("load") {
        let mut site: Option<Value> = None;
        for (i, step) in t.steps.iter().enumerate() {
            let Action::Frame { pattern, .. } = &step.action else { continue };
            let at = format!("{} step {i}", t.name);
            match pattern["t"].as_str() {
                Some("welcome") => {
                    let settings = pattern["settings"].clone();
                    assert!(
                        settings.is_object() && literal(&settings),
                        "{at}: settings written out"
                    );
                    if t.plays.hub.is_some() {
                        let machine = t.setup_hub.get("machine").expect("the hub's machine");
                        assert_eq!(machine["settings"], settings, "{at}: the hub's setup");
                    }
                    site = Some(settings);
                }
                Some("settings") => {
                    site = Some(json!({"llm": pattern["llm"], "cpu": pattern["cpu"]}));
                }
                Some("settings_ack") => {
                    let site = site.as_ref().unwrap_or_else(|| panic!("{at}: no settings"));
                    let got: Map<String, Value> = ["effective", "capped"]
                        .into_iter()
                        .map(|k| (k.to_owned(), pattern[k].clone()))
                        .collect();
                    let got = Value::Object(got);
                    assert!(literal(&got), "{at}: the acknowledgement written out");
                    if t.plays.helper.is_some() {
                        let config = t.setup_helper.get("config").expect("the helper's config");
                        assert!(config.get("cpu").is_some(), "{at}: the CPU ceiling given");
                        assert_eq!(got, expected_ack(site, config), "{at}");
                    }
                    acks += 1;
                }
                _ => {}
            }
        }
    }
    assert!(acks > 60, "{acks} acknowledgements");
}

#[test]
fn the_acknowledgement_rules() {
    let site = json!({
        "llm": {"enabled": true, "slots": 4, "allow_swaps": false},
        "cpu": {"enabled": true, "threads": 0, "cpu_percent": 25, "memory_mb": 4096,
                "work": ["reference_game", "lab_game"], "yield_to_llm": false}
    });
    let config = json!({"max_concurrent": 2, "cpu": {"enabled": false, "threads": 6,
        "cpu_percent": 50, "memory_mb": 1024, "work": ["lab_game", "ranking_game"]}});
    assert_eq!(
        expected_ack(&site, &config),
        json!({
            "effective": {"llm": {"enabled": true, "slots": 2},
                          "cpu": {"enabled": false, "threads": 6, "cpu_percent": 25, "memory_mb": 1024,
                                  "work": ["lab_game"], "yield_to_llm": false}},
            "capped": ["llm.slots", "cpu.enabled", "cpu.memory_mb", "cpu.work"]
        })
    );
    let mut no_llm = config.clone();
    no_llm["no_llm"] = json!(true);
    let ack = expected_ack(&site, &no_llm);
    assert_eq!(ack["effective"]["llm"], json!({"enabled": false, "slots": 0}));
    assert_eq!(ack["capped"][0], "llm.enabled");
    assert_eq!(ack["capped"][1], "llm.slots");
}

/// The transcripts the design names for 4-01's scope are all there (P4.2.9, Appendix E 4-01).
#[test]
fn the_transcripts_cover_the_protocol() {
    let names: BTreeSet<String> =
        transcripts().expect("load").into_iter().map(|t| t.name).collect();
    for want in [
        "handshake-welcome",
        "handshake-refused-token",
        "handshake-refused-key",
        "handshake-refused-protocol",
        "handshake-refused-rate",
        "handshake-refused-build",
        "handshake-v015-server",
        "handshake-v1-refusal-v015",
        "settings-capped",
        "settings-pushed",
        "catalog-on-change",
        "hardware-rescan",
        "power-minute",
        "status-quiet-hours",
        "completion-streamed-tool-call",
        "completion-not-streamed",
        "completion-busy",
        "cancel-mid-stream",
        "cancel-after-done",
        "lost-connection-cancels",
        "job-offer-to-ack",
        "job-reject-build",
        "job-abort",
        "job-deadline",
        "job-reconnect-keep",
        "job-reconnect-drop",
        "job-reconnect-after-send",
        "job-duplicate-result",
        "job-lost-offer",
        "job-silent-run",
        "job-reoffer-backoff",
        "jobs-pause",
        "update-notify",
        "keepalive-ping-pong",
        "shutdown-revoked",
        // 4-01's fix round: closes with no frame, the hub's own reactions, the reject reasons
        // and failures not yet shown, the lost ack.
        "close-bad-hello-received",
        "close-without-frame-reconnect",
        "close-without-frame-permanent-4401",
        "close-without-frame-permanent-4403",
        "close-without-frame-permanent-4409",
        "close-without-frame-protocol",
        "close-without-frame-rate",
        "settings-ack-overclaim",
        "completion-bad-done",
        "job-offer-malformed",
        "job-frame-malformed",
        "job-reject-limits",
        "job-reject-unverifiable",
        "job-failed-host",
        "job-result-ack-lost",
    ] {
        assert!(names.contains(want), "no transcript {want}");
    }
}
