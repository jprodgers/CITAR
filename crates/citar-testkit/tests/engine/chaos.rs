//! The chaos driver and the fuzz entry points (package 1e-01, DESIGN.md 9.5):
//! - chaos plays generated games and fixtures cleanly for a short while (the long runs are the
//!   `chaos` binary's: `cargo chaos --seconds 600`, gate 2);
//! - gate 3: a replay file reproduces an injected panic deterministically: chaos with the seeded
//!   panic writes its replay, which read back and played twice fails at the same step with the
//!   same message;
//! - chaos finds every seeded bug, and the replay of each fails the same way;
//! - `fuzz_one` and `load_state`, what the cargo-fuzz targets call, run on a few inputs.

use citar_engine::base::rng::{Purpose, Rng};
use citar_engine::game::seeded::SeededBug;
use citar_testkit::chaos::{self, Replay, Settings, Start};
use citar_testkit::stability::Property;
use citar_testkit::{fixtures, fuzz, games};

/// A `keep_going` that says yes `n` times.
fn times(mut n: u32) -> impl FnMut() -> bool {
    move || {
        n = n.saturating_sub(1);
        n > 0
    }
}

/// Plays games of chaos with `bug` planted until one fails, at most `games` of them.
fn first_failure(bug: SeededBug, games: u64) -> Replay {
    let settings = Settings { bug: Some(bug), rounds: 25, seed: 11, ..Settings::default() };
    for n in 0..games {
        let start = chaos::start_of(&settings, n).expect("a start");
        let (_, failed) =
            chaos::play_game(&settings, n, start, &mut || true).expect("the game starts");
        if let Some(r) = failed {
            return r;
        }
    }
    panic!("chaos did not find {bug:?} in {games} games");
}

#[test]
fn chaos_plays_generated_maps_and_fixtures_cleanly() {
    for from_fixtures in [false, true] {
        let settings = Settings { from_fixtures, rounds: 6, seed: 5, ..Settings::default() };
        // Two games' worth of steps: the budget is counted in checks of it.
        let report = chaos::run(&settings, &mut times(40)).expect("chaos runs");
        let failures: Vec<String> = report
            .failures
            .iter()
            .map(|r| {
                format!(
                    "{:?} at step {} of {:?}: {}",
                    r.failure.property, r.failure.step, r.start, r.failure.what
                )
            })
            .collect();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
        assert!(report.games >= 1 && report.calls > 0, "{report:?}");
        assert!(report.refused < report.calls, "some calls were taken: {report:?}");
    }
}

#[test]
fn a_replay_file_reproduces_an_injected_panic_deterministically() {
    // Gate 3: the seeded panic (a purchase carried out panics) is met by a RandomAgent or a call
    // within a few rounds; its replay, written out and read back, fails again at the same step
    // with the same message, every time.
    let recorded = first_failure(SeededBug::Panics, 6);
    assert_eq!(recorded.failure.property, Property::P1, "{:?}", recorded.failure);
    assert!(recorded.failure.what.contains("seeded bug"), "{:?}", recorded.failure);
    assert!(recorded.steps.len() > 1, "the panic came after some play");
    let text = chaos::to_json(&recorded).expect("a replay file");
    let back = chaos::from_json(&text).expect("it reads back");
    assert_eq!(back, recorded);
    for _ in 0..2 {
        let again = chaos::replay(&back).expect("it replays");
        assert_eq!(again.as_ref(), Some(&recorded.failure), "the same step, the same panic");
    }
    // Without the bug the same steps pass that step: the panic was the bug's.
    let clean = Replay { bug: None, ..recorded.clone() };
    let again = chaos::replay(&clean).expect("it replays");
    assert!(
        again.as_ref().is_none_or(|f| f.step > recorded.failure.step),
        "without the bug: {again:?}"
    );
}

#[test]
fn chaos_finds_every_seeded_bug_and_each_replay_fails_the_same_way() {
    for bug in SeededBug::ALL {
        let r = first_failure(bug, 6);
        let want = match bug {
            SeededBug::MutatesBeforeRefusing => [Property::P2, Property::P2],
            SeededBug::StaleVisibility | SeededBug::WrongTouch => [Property::P4, Property::P4],
            // Chaos plays no second game to compare with: the oracle sees the memo written.
            SeededBug::QueryWrites => [Property::P4, Property::P4],
            SeededBug::Panics => [Property::P1, Property::P1],
        };
        assert!(want.contains(&r.failure.property), "{bug:?}: {:?}", r.failure);
        let again = chaos::replay(&r).expect("it replays");
        assert_eq!(again.as_ref(), Some(&r.failure), "{bug:?}");
    }
}

#[test]
fn a_replay_names_its_start_and_refuses_another_version() {
    let r = Replay {
        version: chaos::REPLAY_VERSION + 1,
        start: Start::Fixture { name: "duel-continents-normal/t1".to_owned() },
        bug: None,
        options: citar_testkit::stability::Options::default(),
        steps: Vec::new(),
        failure: chaos::Failure { step: 0, property: Property::P1, what: String::new() },
    };
    assert!(chaos::replay(&r).is_err());
    let missing = Start::Fixture { name: "no-such-case/t1".to_owned() };
    assert!(missing.game().is_err());
    let r = Replay { version: chaos::REPLAY_VERSION, ..r };
    assert_eq!(chaos::replay(&r).expect("it replays"), None, "no steps, no failure");
}

#[test]
fn the_fuzz_entry_points_take_any_bytes() {
    let mut rng = Rng::keyed(7, Purpose::TestAgent, &[0xf022]);
    for n in 0..24u64 {
        let len = usize::try_from(8 + rng.below(400)).unwrap_or(8);
        let data: Vec<u8> = (0..len).map(|_| u8::try_from(rng.below(256)).unwrap_or(0)).collect();
        fuzz::fuzz_one(&data);
        fuzz::load_state(&data);
        if n % 8 == 0 {
            fuzz::fuzz_one(&[]);
        }
    }
    // A real save, whole and cut short.
    let f = fixtures::committed().expect("the fixtures").swap_remove(0);
    let mut g = games::from_fixture(&f, b"fuzz", citar_engine::game::DebugOptions::default())
        .expect("it loads");
    let _events = g.assign_every_city_for_test();
    let save = g.snapshot().to_json().expect("it saves");
    fuzz::load_state(&save);
    fuzz::load_state(&save[..save.len() / 2]);
}
