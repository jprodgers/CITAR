//! The golden sets, checked in the test run on every OS rust.yml covers (DESIGN.md 9.6).
//!
//! determinism.yml runs the same check through the `golden` binary on all five targets and
//! compares the targets with each other; this test makes a mismatch with the committed files fail
//! an ordinary test run too.
//!
//! Package 1e-02 adds `same_process_twice` (DESIGN.md 7.5): two identical games in one process,
//! and a third on another thread, must play alike and read alike, which a hash map whose order
//! decides something would break, since every hash map of a process is seeded apart.

use citar_engine::api::views::ReplayFormat;
use citar_engine::base::ids::PlayerId;
use citar_engine::game::{DebugOptions, Game};
use citar_engine::save::canon;
use citar_engine::state::Phase;
use citar_testkit::{fixtures, games, golden};

#[test]
fn golden_sets_match_the_committed_files() {
    let reports = golden::check_all();
    let names: Vec<&str> = reports.iter().map(|r| r.name).collect();
    assert_eq!(
        names,
        [
            "rng", "libm", "pyfmt", "ruleset", "uniques", "filters", "gen", "states", "convert",
            "turns", "maps", "newgame", "load", "pass", "random"
        ]
    );
    let problems: Vec<String> = reports
        .iter()
        .flat_map(|r| r.problems.iter().map(move |p| format!("{}: {p}", r.name)))
        .collect();
    assert!(
        problems.is_empty(),
        "golden sets differ (bless with `cargo golden bless` only if the change is deliberate):\n{}",
        problems.join("\n")
    );
}

#[test]
fn blessing_reproduces_the_committed_files() {
    // The rendered text, not just the values: a bless on any target writes the same bytes.
    for (file, text) in golden::blessed_files() {
        let committed = golden::golden_dir().join(file);
        #[allow(clippy::disallowed_methods, reason = "reads the committed golden file")]
        let on_disk = std::fs::read_to_string(&committed).expect("the committed golden file");
        // Git may check text out with CRLF on Windows.
        assert_eq!(on_disk.replace("\r\n", "\n"), text, "{file} differs from a fresh bless");
    }
}

#[test]
fn the_whole_game_sets_are_blessed_and_checked_with_no_stage_waiting() {
    // Package 1c-10's gate 4: the load, pass and random sets are blessed and checked like any
    // other (the determinism workflow compares what the targets compute).
    assert!(golden::games::refusals().is_empty(), "no stage waits");
    let files: Vec<&str> = golden::blessed_files().iter().map(|(file, _)| *file).collect();
    for file in ["load.json", "pass.json", "random.json"] {
        assert!(files.contains(&file), "bless writes {file}");
    }
    // `golden_sets_match_the_committed_files` compares what they compute with the files.
}

#[test]
fn the_turns_set_is_blessed_and_checked_once_no_stage_waits() {
    // Package 1b-03's gate 4: `golden bless` refuses a set while the stages it depends on are
    // pending. The turns set depends on every stage of setup and of a turn, and since package
    // 1c-09 none is: it is blessed and checked like any other.
    assert!(golden::turns::waiting().is_empty(), "no stage of setup or of a turn waits");
    assert!(golden::turns::refusal().is_none());
    assert!(golden::bless_refusals().is_empty());
    assert!(
        golden::blessed_files().iter().any(|(file, _)| *file == "turns.json"),
        "bless writes the set"
    );
    let report = golden::turns::check_turns();
    assert!(report.waiting.is_empty());
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(report.computed.len(), 64);
    // The same answers twice: the game is a function of its settings.
    assert_eq!(golden::turns::check_turns().computed, report.computed);
}

#[test]
fn the_long_set_is_committed_with_a_row_for_each_of_its_games() {
    // Package 1e-02: the nightly run checks the long set on every target (`golden check
    // --long`); here only that its file is there and names the games the set plays, since
    // playing them takes about a minute in the ci profile.
    use citar_testkit::golden::games::LONG_GAMES;
    let file = golden::committed("long").expect("long.json is committed");
    let named: Vec<&str> = file["game_rows"]
        .as_array()
        .expect("game rows")
        .iter()
        .filter_map(|r| r.get(0).and_then(serde_json::Value::as_str))
        .collect();
    let want: Vec<String> = LONG_GAMES.iter().map(|p| p.name("long")).collect();
    assert_eq!(named, want);
    let rounds = file["round_rows"].as_array().expect("round rows").len();
    let played: u64 = file["game_rows"]
        .as_array()
        .expect("game rows")
        .iter()
        .filter_map(|r| r.get(2).and_then(serde_json::Value::as_u64))
        .sum();
    assert_eq!(rounds as u64, played, "a round row for every round each game played");
}

// ---- same_process_twice (DESIGN.md 7.5) --------------------------------------------------------

/// Everything a host can read of a game that could carry an order a hash map gave it: the
/// state's canonical bytes, its save, every major's view and briefing and the spectator's view,
/// the standings and the replay.
fn reads(g: &Game) -> Vec<(String, Vec<u8>)> {
    let mut out = vec![
        ("canonical state".to_owned(), canon::state_bytes(g.state()).expect("a canonical state")),
        ("save".to_owned(), g.snapshot().to_json().expect("a save")),
        ("spectator's view".to_owned(), g.view_json(None, 50)),
        ("standings".to_owned(), format!("{:?}", g.standings()).into_bytes()),
        ("replay".to_owned(), g.replay_data(ReplayFormat::Delta)),
    ];
    let majors: Vec<PlayerId> =
        g.state().players().iter().filter(|(_, p)| p.is_major()).map(|(id, _)| id).collect();
    for p in majors {
        out.push((format!("player {}'s view", p.0), g.view_json(Some(p), 50)));
        out.push((format!("player {}'s briefing", p.0), g.briefing(p).into_bytes()));
    }
    out
}

fn same_reads(a: &Game, b: &Game, when: &str) {
    for ((what, x), (_, y)) in reads(a).iter().zip(reads(b).iter()) {
        assert!(x == y, "{when}: the two games' {what} differ");
    }
}

/// Plays two copies of a game round by round, side by side, comparing each round's digest and,
/// every `every` rounds and at the end, everything a host reads; then a third copy on another
/// thread, whose hash maps are seeded apart from this thread's, compared round by round.
fn twice(start: &(dyn Fn() -> Game + Sync), rounds: u32, every: u32) {
    let (mut a, mut b) = (start(), start());
    let (mut agents_a, mut agents_b) = (games::agents_for(&a), games::agents_for(&b));
    let mut digests = Vec::new();
    for round in 1..=rounds {
        if a.phase() != Phase::Playing {
            break;
        }
        let mut ra = Vec::new();
        let mut rb = Vec::new();
        games::play_random(&mut a, &mut agents_a, 1, &mut games::keep(&mut ra)).expect("game a");
        games::play_random(&mut b, &mut agents_b, 1, &mut games::keep(&mut rb)).expect("game b");
        assert_eq!(ra, rb, "round {round}: the two games part");
        digests.extend(ra);
        if round % every == 0 {
            same_reads(&a, &b, &format!("round {round}"));
        }
    }
    same_reads(&a, &b, "the end");
    let played = u32::try_from(digests.len()).expect("a count");
    // The engine runs no threads (DESIGN.md 6.13); the test runs this game on one only so that
    // its hash maps are seeded from another thread's keys.
    #[allow(clippy::disallowed_methods, reason = "a thread of the test's, not of the engine")]
    let elsewhere = std::thread::scope(|s| {
        s.spawn(|| {
            let mut c = start();
            let mut agents = games::agents_for(&c);
            let mut rc = Vec::new();
            games::play_random(&mut c, &mut agents, played, &mut games::keep(&mut rc))
                .expect("game c");
            (rc, reads(&c))
        })
        .join()
        .expect("the other thread's game")
    });
    assert_eq!(elsewhere.0, digests, "the game on another thread parts");
    for ((what, x), (_, y)) in reads(&a).iter().zip(&elsewhere.1) {
        assert!(x == y, "the game on another thread: its {what} differs");
    }
}

#[test]
fn same_process_twice() {
    // DESIGN.md 7.5's backstop for what clippy cannot see: a hash map iterated where its order
    // decides something. Every `RandomState` of one process is seeded apart from the others, so
    // two identical games in one process part the moment one does, in their digests or in
    // anything a host reads of them.
    let debug = DebugOptions::default();
    let small = games::random_settings("small", "archipelago", "wrap_x", 7_101, 60);
    twice(&|| games::new_game(&small, b"twice", debug).expect("a small game"), 60, 15);
    twice(
        &|| games::kitchen_sink_game("duel", 7_102, 60, b"twice:sink", debug).expect("a sink game"),
        40,
        20,
    );
    // A late fixture: dozens of cities, every system in play.
    let found = fixtures::committed().expect("the fixtures");
    let late = found
        .iter()
        .find(|f| f.name == "small-continents-normal-s1025/t280")
        .expect("the late fixture");
    twice(&|| games::from_fixture(late, b"twice:late", debug).expect("the fixture"), 6, 3);
}

#[test]
fn a_game_that_leaves_its_file_leaves_its_state_where_it_does() {
    // `golden check --states`: a watch over a game whose committed digest is wrong at one round
    // writes the state as that round ended and the one before, listed in divergence.jsonl, and
    // `golden dump` gives the same state.
    use citar_testkit::golden::divergence::{self, Watch};
    use citar_testkit::golden::dump;
    use citar_testkit::golden::games::{played, random_games};
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("divergence-test");
    #[allow(clippy::disallowed_methods, reason = "a scratch folder")]
    let cleared = std::fs::remove_dir_all(&dir);
    drop(cleared);
    let play = random_games()[0];
    let name = play.name("random");
    let committed = golden::committed("random").expect("random.json");
    let mut rows = committed["round_rows"].clone();
    let at = rows
        .as_array_mut()
        .expect("round rows")
        .iter_mut()
        .find(|r| r[0] == name.as_str() && r[1] == 7)
        .expect("round 7");
    at[2] = serde_json::Value::from("0".repeat(64));
    // What an earlier check left in the folder: its list, a state it names, and a file it does
    // not name. Each check starts the list afresh, and removes no file the list does not name.
    #[allow(clippy::disallowed_methods, reason = "a scratch folder")]
    {
        std::fs::create_dir_all(&dir).expect("the folder");
        std::fs::write(
            dir.join("divergence.jsonl"),
            "{\"file\": \"stale.json\"}\n{\"file\": \"../outside.json\"}\n",
        )
        .expect("a stale list");
        std::fs::write(dir.join("stale.json"), "{}").expect("a stale state");
        std::fs::write(dir.join("notes.txt"), "kept").expect("a file of the user's");
    }
    let rerun = || {
        divergence::set_dir(Some(dir.clone())).expect("the folder starts afresh");
        assert!(Watch::new("random", &name, Watch::rows_of(Some(&rows), &name)).is_some());
        let p = played("random", &play, &[], Some(&rows));
        divergence::set_dir(None).expect("no folder");
        assert!(p.problems.is_empty(), "{:?}", p.problems);
    };
    rerun();
    // A second check into the same folder lists its own states, not the first's as well.
    rerun();
    #[allow(clippy::disallowed_methods, reason = "a scratch folder")]
    {
        assert!(!dir.join("stale.json").exists(), "the stale state went with its list");
        assert!(dir.join("notes.txt").exists(), "a file no list names stays");
    }
    #[allow(clippy::disallowed_methods, reason = "the artifacts are files")]
    let list = std::fs::read_to_string(dir.join("divergence.jsonl")).expect("the list");
    let entries: Vec<serde_json::Value> =
        list.lines().map(|l| serde_json::from_str(l).expect("a JSON line")).collect();
    assert_eq!(entries.len(), 2, "{list}");
    let turn_and_agrees = |e: &serde_json::Value| (e["turn"].as_i64(), e["agrees"].as_bool());
    assert_eq!(turn_and_agrees(&entries[0]), (Some(7), Some(false)));
    assert_eq!(turn_and_agrees(&entries[1]), (Some(6), Some(true)));
    let file = entries[0]["file"].as_str().expect("a file name");
    #[allow(clippy::disallowed_methods, reason = "the artifacts are files")]
    let written = std::fs::read(dir.join(file)).expect("the state");
    let dumped = dump::dump(&format!("random:{name}"), Some(7)).expect("the dump");
    assert!(written == dumped, "the dump is the state the check wrote");
    let before = dump::dump(&format!("random:{name}"), Some(6)).expect("the dump");
    assert!(written != before);
}
