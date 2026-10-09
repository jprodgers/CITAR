//! testkit's copies of the statistical baseline's game settings and seat aggressions
//! (`citar_testkit::golden::games::{baseline_settings, baseline_aggression}`), which the bot
//! golden set and `examples/paired.rs` play with, against citar-sim's own
//! (`GameSpec::config`, `baseline::aggression`): equal, so a golden game is a baseline game and a
//! paired trace replays the baseline's games.
//!
//! It lives here because testkit may not depend on citar-sim, and citar-bench depends on both
//! (DESIGN.md P2.2).

use citar_engine::base::ids::PlayerId;
use citar_engine::rules::Ruleset;
use citar_sim::baseline::{self, Options, game_spec};
use citar_testkit::golden::games::{baseline_aggression, baseline_settings};

/// Seeds at both ends of the range and the baseline's own.
const SEEDS: [u64; 6] = [0, 1, 5000, 5119, u64::MAX - 1, u64::MAX];

#[test]
fn the_golden_games_settings_are_the_baselines_for_every_size_and_map_type() {
    let rules = Ruleset::shared();
    let k = rules.constants();
    let sizes: Vec<String> = k.map_sizes.iter().map(|(_, m)| m.key.to_string()).collect();
    let maps: Vec<String> = k.map_types.iter().map(|(_, m)| m.key.to_string()).collect();
    assert!(sizes.iter().any(|s| s == "small") && sizes.iter().any(|s| s == "gargantuan"));
    for m in baseline::MAP_TYPES {
        assert!(maps.iter().any(|x| x == m), "the baseline's map type {m} is the ruleset's");
    }
    for size in &sizes {
        for map in &maps {
            for seed in SEEDS {
                let o = Options {
                    seed,
                    sizes: vec![size.clone()],
                    maps: vec![map.clone()],
                    ..Options::default()
                }
                .effective();
                let spec = game_spec(rules, &o, 0);
                assert_eq!((spec.seed, &*spec.size, &*spec.map_type), (seed, &**size, &**map));
                assert_eq!(
                    baseline_settings(size, map, seed),
                    spec.config(rules),
                    "{size} {map} seed {seed}"
                );
            }
        }
    }
}

#[test]
fn the_golden_games_aggressions_are_the_baselines_for_every_seat() {
    for seed in SEEDS.into_iter().chain(5000..5200) {
        for p in 0..=u8::MAX {
            let pid = PlayerId(p);
            let (copy, own) = (baseline_aggression(pid, seed), baseline::aggression(pid, seed));
            assert!(copy.to_bits() == own.to_bits(), "seat {p} seed {seed}: {copy} and {own}");
        }
    }
}
