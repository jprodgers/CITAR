//! The bot's random streams (DESIGN.md P2.3.5, package 2-01a gate 3): every draw is
//! `Rng::keyed(seed, Purpose::BotBase, [stream, pid, turn, ...])`. The words are frozen and the
//! first draws of each stream pinned, so a change to either, which would move every game a bot
//! plays, fails here first. They replace Python's two sequential generators and the two stream
//! tests of `test_bot_diplomacy` (`test_diplomacy_has_its_own_stream`,
//! `test_handing_diplomacy_to_the_model_leaves_the_other_draws_alone`).

use citar_bot::Stream;
use citar_engine::base::ids::PlayerId;
use citar_engine::base::rng::{Purpose, Rng};
use citar_engine::game::setup::config_from_value;
use citar_engine::game::{DebugOptions, Game};
use citar_engine::rules::Ruleset;
use citar_testkit::script::map_doc;
use serde_json::json;

/// The key words, frozen: `[stream, pid, turn, ...]` begins with these.
#[test]
fn the_words_are_frozen() {
    let words: Vec<(Stream, u64)> = Stream::ALL.iter().map(|&s| (s, s.word())).collect();
    assert_eq!(
        words,
        [
            (Stream::Research, 1),
            (Stream::Spies, 2),
            (Stream::Peace, 3),
            (Stream::Friendship, 4),
            (Stream::WarPrep, 5),
        ]
    );
    assert_eq!(Purpose::BotBase.code(), 0x1000_0000);
}

/// The first three words and the first unit draw of each stream for one seed, seat, turn and
/// key (seed 5000, the small baselines' first; player 2; turn 40; key 7).
const PINNED: [(Stream, [u64; 3], f64); 5] = [
    (
        Stream::Research,
        [10_089_939_268_282_782_797, 15_567_115_727_641_742_696, 5_509_642_790_518_619_162],
        0.546_976_703_745_949_6,
    ),
    (
        Stream::Spies,
        [8_028_041_490_903_364_091, 14_343_376_414_732_434_286, 4_953_107_492_657_220_125],
        0.435_201_001_262_059_74,
    ),
    (
        Stream::Peace,
        [17_577_070_279_470_091_534, 7_856_102_588_469_477_124, 3_981_788_085_141_438_309],
        0.952_854_889_146_593_2,
    ),
    (
        Stream::Friendship,
        [8_978_872_292_480_722_021, 10_280_780_819_538_621_773, 9_733_145_214_197_063_847],
        0.486_745_642_298_874_96,
    ),
    (
        Stream::WarPrep,
        [8_720_456_541_348_058_467, 10_745_238_805_143_709_816, 16_977_805_528_509_046_174],
        0.472_736_896_359_749_6,
    ),
];

#[test]
fn the_first_draws_of_each_stream_are_pinned() {
    let draws: Vec<(Stream, [u64; 3], f64)> = PINNED
        .iter()
        .map(|&(s, _, _)| {
            let mut r = s.keyed(5000, PlayerId(2), 40, &[7]);
            let words = [r.next_u64(), r.next_u64(), r.next_u64()];
            (s, words, s.keyed(5000, PlayerId(2), 40, &[7]).unit())
        })
        .collect();
    let now: Vec<String> =
        draws.iter().map(|(s, w, u)| format!("    (Stream::{s:?}, {w:?}, {u:?}),")).collect();
    for ((s, words, unit), (_, want_words, want_unit)) in draws.iter().zip(PINNED) {
        assert_eq!(*words, want_words, "{s:?}'s first words moved; now:\n{}", now.join("\n"));
        assert_eq!(unit.to_bits(), want_unit.to_bits(), "{s:?}'s first unit draw moved");
    }
    // The stream is the purpose's, keyed exactly so.
    let r = Stream::Peace.keyed(5000, PlayerId(2), 40, &[7]);
    assert_eq!(r, Rng::keyed(5000, Purpose::BotBase, &[3, 2, 40, 7]));
    // A negative turn is its two's complement bits, as every key of the engine.
    let r = Stream::Research.keyed(1, PlayerId(0), -1, &[]);
    assert_eq!(r, Rng::keyed(1, Purpose::BotBase, &[1, 0, u64::from(u32::MAX)]));
}

/// Each stream, seat, turn and key draws its own numbers: handing one category to a language
/// model, or asking in another order, cannot move another draw (plan 2.1).
#[test]
fn every_key_is_its_own_stream() {
    let first =
        |s: Stream, p: u8, turn: i32, more: &[u64]| s.keyed(77, PlayerId(p), turn, more).next_u64();
    let mut seen = std::collections::BTreeSet::new();
    for s in Stream::ALL {
        for p in [0, 1, 63] {
            for turn in [0, 1, 300] {
                for more in [&[][..], &[0], &[1], &[0, 0], &[1, 4]] {
                    assert!(seen.insert(first(s, p, turn, more)), "{s:?} {p} {turn} {more:?}");
                }
            }
        }
    }
    // Other seeds, other draws.
    assert_ne!(first(Stream::WarPrep, 1, 5, &[2]), {
        Stream::WarPrep.keyed(78, PlayerId(1), 5, &[2]).next_u64()
    });
}

/// In a game, the stream is keyed by the game's seed and its current turn.
#[test]
fn a_games_stream_is_keyed_by_its_seed_and_turn() {
    let (doc, _) = map_doc("arena").expect("the arena");
    let cfg = json!({"seed": 4242, "map": doc, "players": [{}, {}], "city_states": 0,
                     "barbarians": "off", "ruins": false});
    let r = Ruleset::shared();
    let (mut g, _) = Game::new(r, &config_from_value(r, cfg).expect("settings")).expect("a game");
    g.set_debug_options(DebugOptions::ALL);
    let seed = g.state().seed();
    for _ in 0..2 {
        let turn = g.turn();
        for s in Stream::ALL {
            assert_eq!(
                s.rng(&g, PlayerId(1), &[9]),
                s.keyed(seed, PlayerId(1), turn, &[9]),
                "{s:?} on turn {turn}"
            );
        }
        g.end_turn(PlayerId(0)).expect("seat 0 ends");
        g.end_turn(PlayerId(1)).expect("seat 1 ends");
    }
    assert!(g.turn() > 0);
}
