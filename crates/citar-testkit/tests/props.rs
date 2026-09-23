//! Property tests (DESIGN.md 9.5). Package 1a-02 brings the base layer's:
//! - `BitSet` behaves as a `BTreeSet<u32>`;
//! - `MinHeap` pops in the order of a stable sort by key, so ties come out in push order;
//! - `floor_div` and `floor_mod` satisfy Python's definition of `//` and `%` (the table recorded
//!   from Python is checked in `determinism.rs`, through `pyfmt.json`);
//! - an RNG key part of `None` never gives the stream of `Some(0)`.
//!
//! The number of cases follows `PROPTEST_CASES` (proptest's default is 256).

use std::collections::BTreeSet;

use citar_engine::base::collections::MinHeap;
use citar_engine::base::num::{floor_div, floor_mod};
use citar_engine::base::rng::{KeyPart, Purpose, Rng};
use citar_engine::base::sets::BitSet;
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum SetOp {
    Insert(u32),
    Remove(u32),
}

fn set_op() -> impl Strategy<Value = SetOp> {
    // Mostly small indices, so removals hit; some large, so the set grows by many words.
    let index = prop_oneof![4 => 0u32..300, 1 => 0u32..100_000];
    prop_oneof![3 => index.clone().prop_map(SetOp::Insert), 1 => index.prop_map(SetOp::Remove)]
}

fn build(ops: &[SetOp]) -> (BitSet, BTreeSet<u32>) {
    let mut bits = BitSet::new();
    let mut model = BTreeSet::new();
    for op in ops {
        match *op {
            SetOp::Insert(i) => assert_eq!(bits.insert(i), model.insert(i)),
            SetOp::Remove(i) => assert_eq!(bits.remove(i), model.remove(&i)),
        }
    }
    (bits, model)
}

#[derive(Clone, Debug)]
enum HeapOp {
    Push(u8),
    Pop,
}

proptest! {
    #[test]
    fn bit_set_is_a_set(ops in prop::collection::vec(set_op(), 0..200), probes in prop::collection::vec(0u32..400, 0..50)) {
        let (bits, model) = build(&ops);
        prop_assert_eq!(bits.iter().collect::<Vec<_>>(), model.iter().copied().collect::<Vec<_>>());
        prop_assert_eq!(bits.len(), model.len());
        prop_assert_eq!(bits.is_empty(), model.is_empty());
        for p in probes {
            prop_assert_eq!(bits.contains(p), model.contains(&p));
        }
    }

    #[test]
    fn bit_set_algebra_is_set_algebra(a in prop::collection::vec(set_op(), 0..120), b in prop::collection::vec(set_op(), 0..120)) {
        let (ba, ma) = build(&a);
        let (bb, mb) = build(&b);
        let as_vec = |s: &BitSet| s.iter().collect::<Vec<_>>();

        let mut union = ba.clone();
        union.union_with(&bb);
        prop_assert_eq!(as_vec(&union), ma.union(&mb).copied().collect::<Vec<_>>());

        let mut inter = ba.clone();
        inter.intersect_with(&bb);
        prop_assert_eq!(as_vec(&inter), ma.intersection(&mb).copied().collect::<Vec<_>>());

        let mut diff = ba.clone();
        diff.difference_with(&bb);
        prop_assert_eq!(as_vec(&diff), ma.difference(&mb).copied().collect::<Vec<_>>());

        prop_assert_eq!(ba.is_subset(&bb), ma.is_subset(&mb));
        prop_assert_eq!(ba == bb, ma == mb);
    }

    #[test]
    fn min_heap_pops_like_a_stable_sort(keys in prop::collection::vec(0u8..8, 0..300)) {
        let mut heap = MinHeap::new();
        for (i, &k) in keys.iter().enumerate() {
            heap.push(k, i);
        }
        let mut want: Vec<(u8, usize)> = keys.iter().copied().zip(0..).collect();
        // A stable sort by key keeps push order among equal keys.
        want.sort_by_key(|&(k, _)| k);
        let got: Vec<(u8, usize)> = std::iter::from_fn(|| heap.pop()).collect();
        prop_assert_eq!(got, want);
    }

    #[test]
    fn min_heap_interleaved_matches_a_model(ops in prop::collection::vec(prop_oneof![3 => (0u8..6).prop_map(HeapOp::Push), 2 => Just(HeapOp::Pop)], 0..300)) {
        let mut heap = MinHeap::new();
        // The model: (key, push number, value); pop takes the smallest (key, push number).
        let mut model: Vec<(u8, usize)> = Vec::new();
        for (seq, op) in ops.iter().enumerate() {
            match *op {
                HeapOp::Push(k) => {
                    heap.push(k, seq);
                    model.push((k, seq));
                }
                HeapOp::Pop => {
                    let want = model.iter().copied().enumerate().min_by_key(|&(_, e)| e).map(|(i, e)| {
                        model.remove(i);
                        e
                    });
                    prop_assert_eq!(heap.pop(), want);
                }
            }
            prop_assert_eq!(heap.len(), model.len());
        }
    }

    #[test]
    fn floor_division_is_pythons_i64(a in any::<i64>(), b in any::<i64>().prop_filter("nonzero", |b| *b != 0)) {
        let (q, r) = (floor_div(a, b), floor_mod(a, b));
        if a == i64::MIN && b == -1 {
            // Python's quotient, 2^63, is one past i64::MAX: saturated.
            prop_assert_eq!((q, r), (i64::MAX, 0));
        } else {
            // Python's definition: a == q*b + r, with r zero or of b's sign and smaller than b.
            prop_assert_eq!(i128::from(a), i128::from(q) * i128::from(b) + i128::from(r));
            prop_assert!(r == 0 || (r < 0) == (b < 0));
            prop_assert!(r.unsigned_abs() < b.unsigned_abs());
        }
    }

    #[test]
    fn floor_division_is_pythons_i32(a in any::<i32>(), b in any::<i32>().prop_filter("nonzero", |b| *b != 0)) {
        let (q, r) = (floor_div(a, b), floor_mod(a, b));
        let (q64, r64) = (floor_div(i64::from(a), i64::from(b)), floor_mod(i64::from(a), i64::from(b)));
        prop_assert_eq!(i64::from(r), r64);
        if a == i32::MIN && b == -1 {
            prop_assert_eq!(q, i32::MAX);
        } else {
            prop_assert_eq!(i64::from(q), q64);
        }
    }

    #[test]
    fn a_none_key_part_is_not_zero(
        seed in any::<u64>(),
        purpose in prop::sample::select(Purpose::ALL),
        keys in prop::collection::vec(prop::option::of(0u32..1_000_000), 1..6),
        at in any::<prop::sample::Index>(),
    ) {
        let i = at.index(keys.len());
        let mut with_none = keys.clone();
        with_none[i] = None;
        let mut with_zero = keys;
        with_zero[i] = Some(0);
        let words = |ks: &[Option<u32>]| ks.iter().map(|k| k.key()).collect::<Vec<u64>>();
        let a = Rng::keyed(seed, purpose, &words(&with_none)).next_u64();
        let b = Rng::keyed(seed, purpose, &words(&with_zero)).next_u64();
        prop_assert_ne!(a, b);
    }
}
