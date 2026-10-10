//! The long runs with bots in the seats (package 2-13; DESIGN.md P2.1.3, P2.3.11): the soak and
//! chaos take `--drivers random|bot|mixed` (`citar_testkit::bots::Lineup`), and property P8 runs
//! with bot drivers in its noisy game. The runs themselves are the binaries' and the nightly
//! properties'; these show that each lineup seats what it says, plays cleanly for a short while,
//! keeps its replays reproducible, and that a noisy bot makes noise.

use citar_engine::base::ids::PlayerId;
use citar_engine::game::seeded::SeededBug;
use citar_engine::game::{DebugOptions, Game};
use citar_testkit::agents;
use citar_testkit::bots::{BotCounts, CountingBot, LOOPING, Lineup, Seat};
use citar_testkit::chaos::{self, Replay, Settings};
use citar_testkit::games;
use citar_testkit::soak::{self, Probe};
use citar_testkit::stability::{self, Options, Property, Step};

/// A probe with no clock and no memory, which never stops a run.
struct Endless;

impl Probe for Endless {
    fn now_ns(&mut self) -> u64 {
        0
    }
    fn reset_peak(&mut self) {}
    fn peak_bytes(&mut self) -> Option<u64> {
        None
    }
    fn keep_going(&mut self) -> bool {
        true
    }
}

/// A `keep_going` that says yes `n` times.
fn times(mut n: u32) -> impl FnMut() -> bool {
    move || {
        n = n.saturating_sub(1);
        n > 0
    }
}

/// A small game of four majors from `seed`.
fn small(seed: u64) -> Game {
    let settings = games::random_settings("small", "continents", "wrap_x", seed, 200);
    games::new_game(&settings, b"bot-stability", DebugOptions::OFF).expect("a small game")
}

#[test]
fn a_lineup_is_named_and_seats_what_it_says() {
    for l in Lineup::ALL {
        assert_eq!(Lineup::named(l.name()), Some(l));
    }
    assert_eq!(Lineup::named("bots"), None);
    assert_eq!(Lineup::default(), Lineup::Random, "a replay without one had agents");
    let g = small(3);
    let majors: Vec<PlayerId> = g.majors(true).map(|p| p.id()).collect();
    assert_eq!(majors.len(), 4);
    let bots = |l: Lineup, g: &Game| -> Vec<bool> {
        let seats = l.seats(g, false);
        majors.iter().map(|p| seats[usize::from(p.0)].bot().is_some()).collect()
    };
    assert_eq!(bots(Lineup::Random, &g), [false; 4]);
    assert_eq!(bots(Lineup::Bot, &g), [true; 4]);
    // Mixed: half the seats, and the other half in a game of the other parity of seed.
    let a = bots(Lineup::Mixed, &g);
    let b = bots(Lineup::Mixed, &small(4));
    assert_eq!(a.iter().filter(|&&x| x).count(), 2, "{a:?}");
    assert!(a.iter().zip(&b).all(|(x, y)| x != y), "{a:?} {b:?}");
    // The lineup reads the game, so the same game seats the same drivers.
    assert_eq!(bots(Lineup::Mixed, &g.clone()), a);
}

#[test]
fn a_bot_soak_and_a_mixed_one_play_cleanly_and_count_the_bots_actions() {
    for drivers in [Lineup::Bot, Lineup::Mixed] {
        // A duel and a small game, 40 rounds each, the oracle and a save and load every 10.
        let settings = soak::Settings {
            games: 2,
            seed: 13,
            max_rounds: Some(40),
            oracle_every: 10,
            save_every: 10,
            drivers,
            ..soak::Settings::default()
        };
        let report = soak::run(&settings, &mut Endless, &mut |_| {}).expect("a soak");
        assert_eq!(report.games.len(), 2);
        for g in &report.games {
            assert!(!g.failed(), "{drivers:?} {}: {:?} {:?}", g.game.label(), g.panic, g.failures);
            assert_eq!(g.rounds, 40, "{}", g.game.label());
            let b = g.bots.as_ref().expect("bots played");
            assert!(b.turns >= 40 && b.taken > 0, "{drivers:?} {}: {b:?}", g.game.label());
            assert!(!b.looped(), "{b:?}");
        }
    }
    // Agents alone report no bots.
    let settings = soak::Settings { games: 1, max_rounds: Some(5), ..soak::Settings::default() };
    let report = soak::run(&settings, &mut Endless, &mut |_| {}).expect("a soak");
    assert_eq!(report.games[0].bots, None);
}

#[test]
fn a_bot_counts_as_looping_past_the_limit() {
    let mut b = CountingBot::basic1();
    b.worst = (LOOPING, "move_unit", 12);
    let counts = BotCounts::of(&[Seat::Bot(b.clone())]).expect("a bot");
    assert!(!counts.looped(), "the limit itself is no loop");
    b.worst.0 = LOOPING + 1;
    let counts = BotCounts::of(&[Seat::Bot(b)]).expect("a bot");
    assert!(counts.looped());
    assert_eq!(counts.worst, (LOOPING + 1, "move_unit", 12));
}

#[test]
fn chaos_with_bots_and_with_both_plays_cleanly() {
    for drivers in [Lineup::Bot, Lineup::Mixed] {
        for from_fixtures in [false, true] {
            let mut settings =
                Settings { from_fixtures, rounds: 6, seed: 9, ..Settings::default() };
            settings.options.drivers = drivers;
            let report = chaos::run(&settings, &mut times(40)).expect("chaos runs");
            let failures: Vec<String> = report
                .failures
                .iter()
                .map(|r| {
                    format!(
                        "{:?} at step {}: {}",
                        r.failure.property, r.failure.step, r.failure.what
                    )
                })
                .collect();
            assert!(failures.is_empty(), "{drivers:?}: {}", failures.join("\n"));
            assert!(report.games >= 1 && report.rounds > 0, "{drivers:?}: {report:?}");
        }
    }
}

#[test]
fn a_bot_chaos_replay_records_its_lineup_and_fails_the_same_way() {
    let mut settings =
        Settings { bug: Some(SeededBug::Panics), rounds: 25, seed: 11, ..Settings::default() };
    settings.options.drivers = Lineup::Bot;
    let recorded = (0..6)
        .find_map(|n| {
            let start = chaos::start_of(&settings, n).expect("a start");
            chaos::play_game(&settings, n, start, &mut || true).expect("the game starts").1
        })
        .expect("chaos with bots finds the seeded panic");
    assert_eq!(recorded.failure.property, Property::P1, "{:?}", recorded.failure);
    assert_eq!(recorded.version, chaos::REPLAY_VERSION);
    let text = chaos::to_json(&recorded).expect("a replay file");
    assert!(text.contains("\"drivers\": \"bot\""), "{text}");
    let back = chaos::from_json(&text).expect("it reads back");
    assert_eq!(back, recorded);
    for _ in 0..2 {
        assert_eq!(chaos::replay(&back).expect("it replays").as_ref(), Some(&recorded.failure));
    }
}

#[test]
fn a_replay_of_version_one_has_random_agents() {
    // What a version-1 file holds: options without a lineup.
    let mut v: serde_json::Value = serde_json::from_str(
        &chaos::to_json(&Replay {
            version: 1,
            start: chaos::start_of(&Settings::default(), 0).expect("a start"),
            bug: None,
            options: Options::default(),
            steps: vec![Step::Agent, Step::EndTurn, Step::Agent],
            failure: chaos::Failure { step: 0, property: Property::P1, what: String::new() },
        })
        .expect("a replay"),
    )
    .expect("json");
    v["options"].as_object_mut().expect("options").shift_remove("drivers");
    let r = chaos::from_json(&v.to_string()).expect("a version-1 replay reads");
    assert_eq!(r.options.drivers, Lineup::Random);
    assert_eq!(chaos::replay(&r).expect("it replays"), None, "agents' turns, no failure");
}

#[test]
fn noisy_bots_make_noise_and_change_nothing() {
    // P8 with bot drivers, on one start: bot turns, ends of turn and calls, played quietly and
    // with reads before every step and noisy bots; both runs agree step by step.
    let mut g = small(21);
    let mut seats = Lineup::Bot.seats(&g, false);
    games::play_random(&mut g, &mut seats, 8, &mut |_, _| Ok(())).expect("it plays");
    let steps = [Step::Agent, Step::Agent, Step::EndTurn, Step::Agent, Step::Agent, Step::Agent];
    let options = Options { drivers: Lineup::Bot, ..Options::default() };
    stability::reads_are_free(&g, &steps, 77, options).expect("reads are free with bots");
    // The noise P8 makes between steps aside, a noisy bot reads and refuses inside its turn, and
    // a quiet one does not.
    let _before = agents::take_noise_count();
    let mut noisy = stability::Run::new(g.clone(), Options { noisy_agents: true, ..options });
    noisy.step(&Step::Agent).expect("a noisy bot's turn");
    assert!(agents::take_noise_count() > 0, "the noisy bot read and refused");
    let mut quiet = stability::Run::new(g, options);
    quiet.step(&Step::Agent).expect("a quiet bot's turn");
    assert_eq!(agents::take_noise_count(), 0, "a quiet bot is quiet");
}
