//! The pure-function properties of DESIGN.md 9.5 that package 1e-01 adds: the hex grid's
//! distance is a metric that equals a breadth-first search over the neighbours, wraps included,
//! and RNG streams keyed apart are independent. The others live beside the code they test:
//! - A* against Dijkstra, dynamic checks included: `game::path::tests` (the engine's own tests,
//!   which reach the search's internals), `the_search_finds_pythons_path` and
//!   `a_tree_answers_as_the_search`;
//! - incremental visibility and met sets against a full rebuild: the cache oracle, which P4 runs
//!   along every case of `props::games` (`vis::verify`), and `tests/engine/vis.rs`;
//! - combat damage monotonic in strength: `tests/engine/combat.rs`;
//! - `EntityStore` against a `BTreeMap`: `tests/engine/state.rs`;
//! - CSR builds independent of source order: `a_civilizations_index_does_not_depend_on_the_order_of_its_sources`
//!   above;
//! - the journal chunk round trip: `tests/engine/save.rs`.

use std::collections::VecDeque;

use citar_engine::base::hex::{HexGrid, MAX_SIDE, MIN_SIDE};
use citar_engine::base::ids::TileIdx;
use citar_engine::base::rng::{Purpose, Rng};
use proptest::prelude::*;

/// A grid of any size up to 40 by 40, with any wraps.
fn grid() -> impl Strategy<Value = HexGrid> {
    (MIN_SIDE..=40u16, MIN_SIDE..=40u16, any::<bool>(), any::<bool>()).prop_map(|(w, h, wx, wy)| {
        HexGrid::new(w, h, wx, wy).unwrap_or_else(|e| panic!("{w}x{h}: {e}"))
    })
}

/// Every tile's distance from `from`, by a breadth-first search over the neighbour table.
fn bfs(g: &HexGrid, from: TileIdx) -> Vec<u32> {
    let mut d = vec![u32::MAX; g.size() as usize];
    d[from.0 as usize] = 0;
    let mut queue = VecDeque::from([from]);
    while let Some(t) = queue.pop_front() {
        for n in g.neighbors(t) {
            if d[n.0 as usize] == u32::MAX {
                d[n.0 as usize] = d[t.0 as usize] + 1;
                queue.push_back(n);
            }
        }
    }
    d
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: super::games::regressions(),
        ..ProptestConfig::default()
    })]

    #[test]
    fn hex_distance_is_a_metric_equal_to_a_search_over_the_neighbours(
        g in grid(),
        picks in prop::collection::vec(any::<prop::sample::Index>(), 3),
    ) {
        let n = g.size() as usize;
        let [a, b, c] = [0, 1, 2].map(|i| TileIdx(u32::try_from(picks[i].index(n)).unwrap_or(0)));
        prop_assert_eq!(g.distance(a, a), 0);
        prop_assert_eq!(g.distance(a, b), g.distance(b, a), "symmetric");
        prop_assert!(g.distance(a, c) <= g.distance(a, b) + g.distance(b, c), "the triangle");
        let searched = bfs(&g, a);
        for t in g.tiles() {
            prop_assert_eq!(g.distance(a, t), searched[t.0 as usize], "from {:?} to {:?}", a, t);
        }
        // What `within` gives is what the search reaches within that radius.
        for r in [0u32, 1, 2, 5] {
            let mut near: Vec<TileIdx> = g.within(a, r);
            near.sort();
            let reached: Vec<TileIdx> = g.tiles().filter(|t| searched[t.0 as usize] <= r).collect();
            prop_assert_eq!(near, reached, "within {}", r);
        }
    }

    #[test]
    fn rng_streams_keyed_apart_are_independent(
        seed in any::<u64>(),
        p in prop::sample::select(Purpose::ALL),
        q in prop::sample::select(Purpose::ALL),
        a in prop::collection::vec(any::<u64>(), 0..4),
        b in prop::collection::vec(any::<u64>(), 0..4),
    ) {
        prop_assume!((p, &a) != (q, &b));
        let draw = |purpose: Purpose, keys: &[u64]| {
            let mut r = Rng::keyed(seed, purpose, keys);
            (0..64).map(|_| r.next_u64()).collect::<Vec<u64>>()
        };
        let (x, y) = (draw(p, &a), draw(q, &b));
        // Neither stream is the other, nor the other shifted by a few draws.
        for shift in 0..8 {
            prop_assert_ne!(&x[shift..shift + 32], &y[..32]);
            prop_assert_ne!(&y[shift..shift + 32], &x[..32]);
        }
        // And their draws do not move together: of 64 coin flips, the two agree on about half (the
        // bounds are six standard deviations out, so a nightly run of 10,000 cases does not flake).
        let same = x.iter().zip(&y).filter(|(u, v)| (*u >> 63) == (*v >> 63)).count();
        prop_assert!((8..=56).contains(&same), "{} of 64 flips agree", same);
    }
}

#[test]
fn the_largest_grids_are_metrics_too() {
    // The sides the strategy leaves out, checked once each wrap.
    for (wx, wy) in [(false, false), (true, false), (false, true), (true, true)] {
        let g = HexGrid::new(MAX_SIDE, 80, wx, wy).expect("a grid");
        let from = TileIdx(g.size() / 2 + 7);
        let searched = bfs(&g, from);
        for t in g.tiles().step_by(97) {
            assert_eq!(g.distance(from, t), searched[t.0 as usize], "{wx} {wy} {t:?}");
        }
    }
}
