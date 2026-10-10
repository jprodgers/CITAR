//! A barbarian round (package 1c-06).
//!
//! The state is a small map with raging barbarians at turn 120 (`small-continents-raging-s1005`
//! of the local corpus, when `CITAR_REFCHECK_CORPUS` names it: the nearest to turn 100 it has),
//! else the committed small map with raging barbarians at turn 50 (`small-pangaea-raging`);
//! loaded through the Python converter. `barbarians/round` times stage S0 on a fresh copy of the
//! game each time: the barbarians' units start their turn, every unit acts, the camps spawn and
//! new ones may appear, and the round's settle. Budget (DESIGN.md 10): 2 ms. Report-only: its
//! parts, the camps' turn alone and the units' start.

use citar_bench::{Suite, fixtures, median_on_copies};
use citar_engine::base::ids::PlayerId;
use citar_engine::game::{Game, barbarians, units};
use criterion::Criterion;

/// Stage S0: the barbarians' units start their turn and act, their camps spawn, and it settles.
fn round(g: &mut Game, bid: PlayerId) {
    units::turn::start_units(g, bid);
    barbarians::take_turn(g);
    g.settle_for_bench();
}

pub fn run(s: &mut Suite, cr: &mut Criterion) {
    let (g, name) =
        fixtures::corpus_game("small-continents-raging-s1005", 120).unwrap_or_else(|| {
            (
                fixtures::committed_game("small-pangaea-raging", 50),
                "small-pangaea-raging/t50".into(),
            )
        });
    let bid = g.barbarian_id().expect("the barbarians");
    let camps = g.state().world().camps.values().filter(|c| !c.destroyed).count();
    println!(
        "{name}: {} barbarian units, {camps} standing camps, aggression {}",
        g.player_units(bid).count(),
        barbarians::aggression(&g)
    );
    cr.bench_function("barbarians/round", |b| {
        b.iter_batched(
            || g.clone(),
            |mut copy| {
                round(&mut copy, bid);
                copy
            },
            criterion::BatchSize::LargeInput,
        );
    });
    if fixtures::has_corpus() {
        s.put("barbarians/round", median_on_copies(&g, 31, |copy| round(copy, bid)));
    } else {
        // The committed state is not the budget's (turn 50, not 120): for the record only.
        s.note(
            "barbarians/round (small-pangaea-raging/t50)",
            median_on_copies(&g, 31, |copy| {
                round(copy, bid);
            }),
        );
    }
    s.note("barbarians/camps", median_on_copies(&g, 31, barbarians::update_camps));
    s.note(
        "barbarians/units_start",
        median_on_copies(&g, 31, |g| units::turn::start_units(g, bid)),
    );
}
