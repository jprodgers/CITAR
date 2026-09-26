//! Combat (package 1c-03, gate 5): the attack preview, the fight's numbers gathered once
//! (`combat::setup`) with the damage at both ends of the roll.
//!
//! The state is the committed fixture with the most fights (`standard-pangaea-normal-s1031/t120`),
//! loaded through the Python converter. The pairs are every ground or sea unit and a tile in its range
//! that holds an enemy it may attack, each unit readied (full movement, no attack spent) so that
//! the preview's checks pass. `combat/preview` is `combat::preview_of` on each pair in turn: the
//! checks, one setup and the four damages (the JSON the tool reports is the views'). Python rebuilt
//! the modifier stacks about six times for one preview (`combat.py:512-538`). `combat/validate`
//! and `combat/setup` are its two halves. Budget (DESIGN.md 10, report-only): the preview at or
//! under 1 µs. After Criterion, the run takes the median of its own timings and warns above it.
//!
//! ```text
//! cargo bench -p citar-bench --bench combat
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

use citar_engine::base::ids::{TileIdx, UnitId};
use citar_engine::game::Game;
use citar_engine::game::combat::{combatant, resolve, strength};
use citar_engine::game::units::health::attack_range;
use citar_engine::rules::Ruleset;
use citar_engine::rules::defs::Domain;
use citar_engine::unique::Combatant;
use citar_testkit::fixtures;
use criterion::Criterion;

const PREVIEW: Duration = Duration::from_micros(1);

/// The fixture's game.
fn fixture() -> Game {
    let f = fixtures::committed()
        .expect("the committed fixtures")
        .into_iter()
        .find(|f| f.case == "standard-pangaea-normal-s1031" && f.turn == 120)
        .expect("the fixture");
    let bytes = fixtures::read_state(&f).expect("a state");
    let (g, _) = Game::from_python(Ruleset::shared(), &bytes).expect("it loads");
    g
}

/// Every ground or sea unit and a tile in its range it may attack, the units readied.
fn pairs(g: &mut Game) -> Vec<(UnitId, TileIdx)> {
    let r = g.rules();
    let mut out = Vec::new();
    for u in g.state().units().iter() {
        let def = &r.base_units()[u.base];
        if !def.military || def.domain == Domain::Air {
            continue;
        }
        let range = u32::try_from(attack_range(g, u.id())).unwrap_or(0);
        for t in g.grid().within(u.tile(), range) {
            if t != u.tile()
                && (g.is_barbarian(u.owner()) || g.derived().vis().sees(u.owner(), t))
                && resolve::contains_attackable_enemy(g, t, Combatant::Unit(u.id())).is_none()
            {
                out.push((u.id(), t));
            }
        }
    }
    let ready: Vec<serde_json::Value> = out
        .iter()
        .map(|&(u, _)| serde_json::json!({"op": "ready_unit", "unit": u.get()}))
        .collect();
    citar_engine::api::testops::apply(g, &serde_json::Value::Array(ready)).expect("readied");
    out.retain(|&(u, t)| resolve::preview(g, u, t).is_ok());
    assert!(!out.is_empty(), "the fixture has fights to preview");
    out
}

/// The median of `n` timings of `batch` calls of `f`.
fn median(n: usize, batch: u32, mut f: impl FnMut()) -> Duration {
    let mut times: Vec<Duration> = (0..n)
        .map(|_| {
            let t = Instant::now();
            for _ in 0..batch {
                f();
            }
            t.elapsed() / batch
        })
        .collect();
    times.sort();
    times[n / 2]
}

fn main() {
    let mut g = fixture();
    let pairs = pairs(&mut g);
    println!("{} fights to preview", pairs.len());
    let mut k = 0usize;
    let mut preview = || {
        k = (k + 1) % pairs.len();
        let (u, t) = pairs[k];
        black_box(resolve::preview_of(&g, u, t).expect("a preview"));
    };
    let mut j = 0usize;
    let mut setup = || {
        j = (j + 1) % pairs.len();
        let (u, t) = pairs[j];
        let a = Combatant::Unit(u);
        let d = combatant::combatant_at(&g, t).expect("a defender");
        let s = strength::setup(&g, a, combatant::tile(&g, a), d, false);
        black_box((s.damage_to_defender(0.0), s.damage_to_attacker(1.0)));
    };
    let mut i = 0usize;
    let mut check = || {
        i = (i + 1) % pairs.len();
        let (u, t) = pairs[i];
        black_box(resolve::validate_attack(&g, u, t).expect("allowed"));
    };
    let mut c = Criterion::default().configure_from_args();
    c.bench_function("combat/validate", |b| b.iter(&mut check));
    c.bench_function("combat/preview", |b| b.iter(&mut preview));
    c.bench_function("combat/setup", |b| b.iter(&mut setup));
    c.final_summary();
    let took = median(31, 1_000, &mut preview);
    println!("combat/preview median: {took:?} (budget {PREVIEW:?}, report-only)");
    if took > PREVIEW {
        println!("warning: combat/preview is over its {PREVIEW:?} budget (report-only)");
    }
}
