//! Gate 1 of package 1b-01: memos revalidate only when their inputs move, early cutoff stops
//! the cascade, a read after a write sees the write, and a cycle panics on the `RefCell`.

use core::cell::Cell;

use super::*;

/// A little world: two input revisions and a counter of each memo's recomputes.
struct World {
    now: Rev,
    x: Rev,
    y: Rev,
    x_value: Cell<i64>,
}

impl World {
    fn new() -> Self {
        Self { now: Rev::START, x: Rev::START, y: Rev::START, x_value: Cell::new(1) }
    }

    fn bump_x(&mut self, value: i64) {
        self.now = Rev(self.now.0 + 1);
        self.x = self.now;
        self.x_value.set(value);
    }

    fn bump_y(&mut self) {
        self.now = Rev(self.now.0 + 1);
        self.y = self.now;
    }
}

/// `a` reads input `x` (its value's sign); `b` reads `a` and input `y`.
struct Memos {
    a: CopyMemo<i64>,
    b: Memo<Vec<i64>>,
    a_runs: Cell<u32>,
    b_runs: Cell<u32>,
}

impl Memos {
    fn new() -> Self {
        Self { a: CopyMemo::new(), b: Memo::new(), a_runs: Cell::new(0), b_runs: Cell::new(0) }
    }

    fn a(&self, w: &World) -> i64 {
        self.a.get(
            w.now,
            || w.x,
            || {
                self.a_runs.set(self.a_runs.get() + 1);
                w.x_value.get().signum()
            },
        )
    }

    fn b(&self, w: &World) -> Vec<i64> {
        self.b
            .get(
                w.now,
                || {
                    let _a = self.a(w);
                    self.a.changed().max(w.y)
                },
                || {
                    self.b_runs.set(self.b_runs.get() + 1);
                    vec![self.a(w) * 10]
                },
            )
            .clone()
    }
}

#[test]
fn a_memo_revalidates_only_when_its_inputs_move() {
    let mut w = World::new();
    let m = Memos::new();
    assert_eq!(m.a(&w), 1);
    assert_eq!(m.a(&w), 1);
    assert_eq!(m.a_runs.get(), 1, "a hit at the same revision");
    // Another input moved: validated, not recomputed.
    w.bump_y();
    assert_eq!(m.a(&w), 1);
    assert_eq!(m.a_runs.get(), 1);
    assert_eq!(m.a.stamp().verified(), w.now);
    // Its input moved: recomputed.
    w.bump_x(-5);
    assert_eq!(m.a(&w), -1);
    assert_eq!(m.a_runs.get(), 2);
    assert_eq!(m.a.changed(), w.now);
}

#[test]
fn early_cutoff_stops_the_cascade() {
    let mut w = World::new();
    let m = Memos::new();
    assert_eq!(m.b(&w), [10]);
    assert_eq!((m.a_runs.get(), m.b_runs.get()), (1, 1));
    // x moves, but a's answer stays 1: a recomputes, b does not.
    w.bump_x(7);
    assert_eq!(m.b(&w), [10]);
    assert_eq!((m.a_runs.get(), m.b_runs.get()), (2, 1));
    assert!(m.a.changed() < m.a.stamp().verified(), "a's value did not change");
    // Now it does change: both recompute.
    w.bump_x(-7);
    assert_eq!(m.b(&w), [-10]);
    assert_eq!((m.a_runs.get(), m.b_runs.get()), (3, 2));
    // b's own input moves alone: b recomputes to the same value and keeps its changed stamp.
    let changed = m.b.changed();
    w.bump_y();
    assert_eq!(m.b(&w), [-10]);
    assert_eq!((m.a_runs.get(), m.b_runs.get()), (3, 3));
    assert_eq!(m.b.changed(), changed);
}

#[test]
fn floats_compare_as_bits() {
    assert!(!0.0f64.bit_eq(&-0.0));
    assert!(f64::NAN.bit_eq(&f64::NAN));
    let mut s = Stats::default();
    let t = s;
    s.0[0] = -0.0;
    assert!(!s.bit_eq(&t));
    assert!(vec![Some(1.5f64)].bit_eq(&vec![Some(1.5)]));
}

#[test]
#[should_panic(expected = "borrow")]
fn a_cycle_panics_on_the_refcell() {
    let w = World::new();
    let m: Memo<Vec<i64>> = Memo::new();
    let read = |m: &Memo<Vec<i64>>| m.get(w.now, || w.x, Vec::new).len();
    // The memo's inputs read the memo itself.
    let _never = m.get(w.now, || Rev(read(&m) as u64), Vec::new);
}

#[test]
#[should_panic(expected = "memo cycle")]
fn a_cycle_through_a_copy_memo_panics_too() {
    let w = World::new();
    let m: CopyMemo<i64> = CopyMemo::new();
    let _never = m.get(w.now, || w.x, || m.get(w.now, || w.x, || 1));
}

#[test]
fn a_panicking_compute_leaves_the_memo_usable() {
    let w = World::new();
    let m: CopyMemo<i64> = CopyMemo::new();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        m.get(w.now, || w.x, || panic!("a bug in the compute"))
    }));
    assert!(r.is_err());
    assert_eq!(m.get(w.now, || w.x, || 3), 3, "not left busy");
}

#[test]
fn the_tile_log_reaches_back_as_far_as_it_keeps() {
    let mut log = TileChangeLog::new();
    for i in 0..3u64 {
        log.push(Rev(i + 2), TileIdx(i as u32));
    }
    let since: Vec<TileIdx> = log.since(Rev(2)).map(Iterator::collect).unwrap_or_default();
    assert_eq!(since, [TileIdx(1), TileIdx(2)]);
    assert_eq!(log.rev(), Rev(4));
    for i in 0..(TileChangeLog::CAP as u64 + 10) {
        log.push(Rev(i + 10), TileIdx(0));
    }
    assert!(log.since(Rev(2)).is_none(), "the oldest entries were dropped");
    assert!(log.since(log.rev()).is_some_and(|mut it| it.next().is_none()));
}

#[cfg(feature = "embedded-ruleset")]
#[test]
fn a_read_through_the_game_after_a_write_sees_it() {
    use crate::game::core::testing;
    let mut g = testing::duel();
    let names = |g: &crate::game::Game| -> Vec<String> {
        g.derived().names(g.state()).entries().iter().map(|(n, _, _)| n.to_string()).collect()
    };
    assert!(names(&g).contains(&"Greece".to_owned()));
    if let Some(p) = g.player_mut(crate::base::ids::PlayerId(1), PlayerTouch::NAME) {
        p.name = "Hellas".into();
    }
    let after = names(&g);
    assert!(after.contains(&"Hellas".to_owned()) && !after.contains(&"Greece".to_owned()));
}
