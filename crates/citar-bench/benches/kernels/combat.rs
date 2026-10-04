//! Combat (packages 1c-03, 1e-03): the attack preview, the fight's numbers gathered once
//! (`combat::setup`) with the damage at both ends of the roll, and the damage over the pairs
//! refcheck recorded.
//!
//! The state is the committed fixture with the most fights (`standard-pangaea-normal-s1031/t120`),
//! loaded through the Python converter. The pairs are every ground or sea unit and a tile in its
//! range that holds an enemy it may attack, each unit readied (full movement, no attack spent) so
//! that the preview's checks pass. `combat/preview` is `combat::preview_of` on each pair in turn:
//! the checks, one setup and the four damages (the JSON the tool reports is the views'). Python
//! rebuilt the modifier stacks about six times for one preview (`combat.py:512-538`). Budget
//! (DESIGN.md 10): the preview at or under 1 µs. Report-only: `combat/validate` and
//! `combat/setup`, its two halves, and `combat/recorded_previews`, the preview of every attack
//! Python's `combat_previews` group recorded on the twelve committed fixtures.

use std::hint::black_box;

use citar_bench::{Suite, fixtures, median};
use citar_engine::base::ids::{TileIdx, UnitId};
use citar_engine::game::Game;
use citar_engine::game::combat::{combatant, resolve, strength};
use citar_engine::game::units::health::attack_range;
use citar_engine::rules::defs::Domain;
use citar_engine::unique::Combatant;
use criterion::Criterion;

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

/// Every committed fixture's game with the attacks Python previewed on it (the attacking unit and
/// the target of each fight of `queries.combat_previews` the game allows).
fn recorded_previews() -> Vec<(Game, Vec<(UnitId, TileIdx)>)> {
    fixtures::committed()
        .iter()
        .map(|f| {
            let g = fixtures::load(f);
            let rec = fixtures::recorded(f, "combat_previews");
            let list = rec.get("fights").and_then(|f| f.as_array()).cloned().unwrap_or_default();
            let pairs = list
                .iter()
                .filter_map(|e| {
                    let u = e.get("attacker")?.get("unit")?.as_u64()?;
                    let u = UnitId::new(u32::try_from(u).ok()?)?;
                    let t = TileIdx(u32::try_from(e.get("target")?.as_u64()?).ok()?);
                    resolve::preview_of(&g, u, t).ok()?;
                    Some((u, t))
                })
                .collect();
            (g, pairs)
        })
        .collect()
}

pub fn run(s: &mut Suite, c: &mut Criterion) {
    let mut g = fixtures::committed_game("standard-pangaea-normal-s1031", 120);
    let pairs = pairs(&mut g);
    let recorded = recorded_previews();
    let all: Vec<(usize, UnitId, TileIdx)> = recorded
        .iter()
        .enumerate()
        .flat_map(|(i, (_, ps))| ps.iter().map(move |&(u, t)| (i, u, t)))
        .collect();
    println!("{} fights to preview; {} recorded previews the games allow", pairs.len(), all.len());
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
    let mut q = 0usize;
    let mut rec = || {
        if all.is_empty() {
            return;
        }
        q = (q + 1) % all.len();
        let (x, u, t) = all[q];
        black_box(resolve::preview_of(&recorded[x].0, u, t).ok());
    };
    c.bench_function("combat/validate", |b| b.iter(&mut check));
    c.bench_function("combat/preview", |b| b.iter(&mut preview));
    c.bench_function("combat/setup", |b| b.iter(&mut setup));
    c.bench_function("combat/recorded_previews", |b| b.iter(&mut rec));
    s.put("combat/preview", median(31, 1_000, &mut preview));
    s.note("combat/validate", median(31, 1_000, &mut check));
    s.note("combat/setup", median(31, 1_000, &mut setup));
    if !all.is_empty() {
        s.note("combat/recorded_previews", median(31, 1_000, &mut rec));
    }
}
