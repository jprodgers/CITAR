//! Properties P1 to P8 on games (package 1e-01, DESIGN.md 9.5): sequences of steps (tool calls
//! from [`ActionSpec`]s bound late, 70% valid, 20% of the wrong type, 10% random JSON; ends of
//! turn; `RandomAgent` turns) played from committed fixtures or generated duel and small maps,
//! on the shipped ruleset or the kitchen sink, every step checked by `citar_testkit::stability`.
//! Package 2-13 adds P8 with bot drivers: `basic-1` in every major's seat, its turns played
//! quietly and, in the noisy game, with reads before every step and noisy bots.
//!
//! The cases follow `PROPTEST_CASES`, 64 when it is unset (as in CI, `rust.yml`); the nightly run
//! takes them to 10,000. A failure is saved by proptest beside this file, in
//! `games.proptest-regressions`, which is committed with the fix so that the case runs first from
//! then on.

use std::cell::RefCell;

use citar_engine::game::{DebugOptions, Game};
use citar_testkit::agents::RandomAgent;
use citar_testkit::bots::Lineup;
use citar_testkit::spec::{ActionSpec, Shape, off_map};
use citar_testkit::stability::{self, Breach, Options, Step, null_below_top};
use citar_testkit::{fixtures, games};
use proptest::prelude::*;
use proptest::test_runner::Config;

/// The cases of a game property: `PROPTEST_CASES`, or 64.
pub fn cases() -> u32 {
    #[allow(clippy::disallowed_methods, reason = "a test's switch, not a game's input")]
    let env = std::env::var("PROPTEST_CASES").ok();
    env.and_then(|v| v.parse().ok()).unwrap_or(64)
}

/// The configuration of the game properties: [`cases`], shrinking bounded as proptest bounds it
/// by default (four times the cases), and failures kept beside this file
/// ([`regressions`]).
pub fn config() -> Config {
    Config { cases: cases(), failure_persistence: regressions(), ..Config::default() }
}

/// Where proptest keeps the failures it found: beside the test's source, as
/// `games.proptest-regressions`. Its default looks for a `lib.rs` above the file, which an
/// integration test has none of, and says so on every run before it falls back to the same.
pub fn regressions() -> Option<Box<dyn proptest::test_runner::FailurePersistence>> {
    Some(Box::new(proptest::test_runner::FileFailurePersistence::WithSource(
        "proptest-regressions",
    )))
}

/// Where a case starts.
#[derive(Clone, Debug)]
pub enum Start {
    /// A committed fixture, by its index in `fixtures::committed()`, with every city's citizens
    /// assigned by the engine.
    Fixture(usize),
    /// A generated duel (`small` false) or small map, after some rounds of `RandomAgent`s.
    Generated { small: bool, seed: u64, rounds: u8 },
    /// The same on the kitchen-sink ruleset, the Kitchen Sink nation in the first seat, for the
    /// unique types only it uses.
    KitchenSink { small: bool, seed: u64, rounds: u8 },
}

/// How many committed fixtures there are, counted once.
fn fixtures() -> usize {
    static COUNT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *COUNT.get_or_init(|| fixtures::committed().expect("the fixtures").len().max(1))
}

pub fn start() -> impl Strategy<Value = Start> {
    prop_oneof![
        6 => (0..fixtures()).prop_map(Start::Fixture),
        2 => (any::<bool>(), 0u64..10_000, 0u8..12)
            .prop_map(|(small, seed, rounds)| Start::Generated { small, seed, rounds }),
        1 => (any::<bool>(), 0u64..10_000, 0u8..12)
            .prop_map(|(small, seed, rounds)| Start::KitchenSink { small, seed, rounds }),
    ]
}

std::thread_local! {
    /// The fixtures loaded so far on this thread, each copied for a case.
    static LOADED: RefCell<Vec<Option<Game>>> = const { RefCell::new(Vec::new()) };
}

/// The game a start gives.
pub fn build(s: &Start) -> Game {
    match *s {
        Start::Fixture(i) => LOADED.with(|l| {
            // A case saved when there were more fixtures still starts from one.
            let i = i % fixtures();
            let mut l = l.borrow_mut();
            if l.len() <= i {
                l.resize(i + 1, None);
            }
            l[i].get_or_insert_with(|| {
                let f = fixtures::committed().expect("the fixtures").swap_remove(i);
                let mut g = games::from_fixture(&f, b"props", DebugOptions::OFF).expect("it loads");
                let _events = g.assign_every_city_for_test();
                g
            })
            .clone()
        }),
        Start::Generated { small, seed, rounds } | Start::KitchenSink { small, seed, rounds } => {
            let size = if small { "small" } else { "duel" };
            let mut g = if matches!(s, Start::KitchenSink { .. }) {
                games::kitchen_sink_game(size, seed, 200, b"props", DebugOptions::OFF)
            } else {
                let settings = games::random_settings(size, "continents", "wrap_x", seed, 200);
                games::new_game(&settings, b"props", DebugOptions::OFF)
            }
            .expect("a game");
            let mut agents = vec![RandomAgent::new(); g.state().players().len()];
            games::play_random(&mut g, &mut agents, u32::from(rounds), &mut |_, _| Ok(()))
                .expect("it plays");
            g
        }
    }
}

/// The shapes of DESIGN.md 9.5: 70 valid, 20 confused, 10 random.
pub fn shape() -> impl Strategy<Value = Shape> {
    prop_oneof![
        7 => Just(Shape::Valid),
        2 => any::<u8>().prop_map(Shape::Confused),
        1 => any::<u32>().prop_map(Shape::Random),
    ]
}

/// The `ActionSpec` strategy: every field any value, bound late to whatever the game offers.
pub fn action_spec() -> impl Strategy<Value = ActionSpec> {
    (any::<u8>(), any::<u8>(), any::<u16>(), (any::<i16>(), any::<i16>()), shape()).prop_map(
        |(tool, actor, target, coords, shape)| ActionSpec { tool, actor, target, coords, shape },
    )
}

/// A step: most often a call, now and then an end of turn or a `RandomAgent`'s turn.
pub fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        20 => action_spec().prop_map(Step::Call),
        2 => Just(Step::EndTurn),
        1 => Just(Step::Agent),
    ]
}

/// Up to 48 steps.
pub fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(step(), 1..48)
}

/// A step of P8 with bot drivers: a seat's turn more often than among [`step`]'s, since a bot's
/// turn is where a read could change a decision, with calls and ends of turn between them.
pub fn bot_step() -> impl Strategy<Value = Step> {
    prop_oneof![
        12 => action_spec().prop_map(Step::Call),
        2 => Just(Step::EndTurn),
        3 => Just(Step::Agent),
    ]
}

/// Up to 32 steps with bot turns among them.
pub fn bot_steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(bot_step(), 1..32)
}

/// A breach as a test case's failure, its property first so that a caller can tell which.
pub fn failed(b: &Breach) -> TestCaseError {
    TestCaseError::fail(format!("{:?}: {b}", b.property))
}

/// P1 to P7 along the steps, from `g`.
pub fn p1_to_p7(g: &Game, steps: &[Step], options: Options) -> Result<(), TestCaseError> {
    stability::play(g, steps, options).map(drop).map_err(|b| failed(&b))
}

/// P8 from `g`, and P2 to P7 on both runs.
pub fn p8(g: &Game, steps: &[Step], seed: u64, options: Options) -> Result<(), TestCaseError> {
    stability::reads_are_free(g, steps, seed, options).map_err(|b| failed(&b))
}

proptest! {
    #![proptest_config(config())]

    /// P1 to P7: no panic; a refusal changes nothing and reads as a sentence; the invariants
    /// hold after every call; the caches equal a cold rebuild every 10 steps and at the end; a
    /// save and a load every 25 steps give the digest saved; the turns do not stall.
    #[test]
    fn p1_to_p7_hold_along_any_calls(start in start(), steps in steps()) {
        p1_to_p7(&build(&start), &steps, Options::default())?;
    }

    /// P8: reads, snapshots, saves and refused calls before every step change no step's result
    /// and no digest.
    #[test]
    fn p8_reads_are_free(start in start(), steps in steps(), seed in any::<u64>()) {
        p8(&build(&start), &steps, seed, Options::default())?;
    }

    /// P8 with bot drivers (package 2-13): `basic-1` plays every seat's turn, and none of the
    /// reads, snapshots, saves and refused calls before every step, nor those its noisy bots make
    /// before and after their turns and answers, changes what it decides.
    #[test]
    fn p8_reads_are_free_with_bot_drivers(start in start(), steps in bot_steps(), seed in any::<u64>()) {
        p8(&build(&start), &steps, seed, Options { drivers: Lineup::Bot, ..Options::default() })?;
    }
}

proptest! {
    #![proptest_config(config())]

    /// The shapes do what they say: a valid spec sends every required parameter, each
    /// parameter it sends of the parameter's schema type, and its tiles on the map unless its
    /// offset puts them off it (`spec::off_map`).
    #[test]
    fn a_spec_binds_arguments_of_the_shape_it_names(start in start(), spec in action_spec()) {
        let g = build(&start);
        let tool = spec.tool_spec();
        let valid = ActionSpec { shape: Shape::Valid, ..spec }.bind(&g);
        prop_assert_eq!(valid.tool, tool.name());
        let args = valid.args.as_object().cloned().unwrap_or_default();
        for r in tool.args.required {
            prop_assert!(args.contains_key(*r), "{}: {} missing from {:?}", tool.name(), r, args);
        }
        for p in tool.args.params {
            if let Some(v) = args.get(p.name) {
                prop_assert!(p.json.fits(v), "{}: {} = {} is not {}", tool.name(), p.name, v, p.json.what());
            }
        }
        let coord = |name: &str| args.get(name).and_then(serde_json::Value::as_i64);
        if let (Some(x), Some(y)) = (coord("x"), coord("y")) {
            let on = i32::try_from(x).ok().zip(i32::try_from(y).ok())
                .is_some_and(|(x, y)| g.grid().in_bounds(x, y));
            prop_assert_eq!(on, !off_map(spec.coords), "{}: ({}, {}) from {:?}", tool.name(), x, y, spec.coords);
        }
    }
}

/// Random arguments hold nulls inside their values now and then, as a model's may: a class of
/// input the properties must meet, not avoid (the refusals that quote one back are read by
/// `stability::refusal_rule_broken`).
#[test]
fn random_arguments_nest_nulls_now_and_then() {
    let n = (0..2_000u32)
        .filter(|&seed| {
            let spec = ActionSpec {
                tool: 0,
                actor: 0,
                target: 0,
                coords: (0, 0),
                shape: Shape::Random(seed),
            };
            null_below_top(&citar_testkit::spec::random_args(spec.tool_spec(), seed))
        })
        .count();
    assert!(n >= 20, "{n} of 2,000 random arguments nest a null");
}

/// One spec in sixteen puts its tiles off the map.
#[test]
fn a_spec_puts_a_tile_off_the_map_one_time_in_sixteen() {
    let off = (i16::MIN..=i16::MAX).filter(|&dx| off_map((dx, 0))).count();
    assert_eq!(off, 65_536 / 16);
}
