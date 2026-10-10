//! The production advisor (package 1c-07).
//!
//! The state is a small map at turn 200 (`small-continents-normal-s1025/t200` of the local
//! corpus, when `CITAR_REFCHECK_CORPUS` names it), else the same game at turn 280, the latest
//! committed fixture; loaded through the Python converter. `advisor/call_per_city` asks the
//! advisor what each city of a living major would build (`advisor::advise_production`, at
//! automatic production's parameters) in turn, the time of one call. Budget (DESIGN.md 10): a
//! call at or under 50 µs. Report-only: the same asked through one `advisor::Advisor` a
//! civilization, as the bot keeps one for a civilization's turn, and the what-if of each building
//! each of them could build (`cities::what_if::what_if_building`), the time of one.

use std::hint::black_box;

use citar_bench::{Suite, fixtures, median};
use citar_engine::base::ids::{BuildingId, CityId, PlayerId};
use citar_engine::game::Game;
use citar_engine::game::advisor::{self, AdvisorParams};
use citar_engine::game::cities::construction::buildable_items;
use citar_engine::game::cities::what_if::what_if_building;
use criterion::Criterion;

const CASE: &str = "small-continents-normal-s1025";

/// Every city of a living major, with its owner.
fn cities(g: &Game) -> Vec<(PlayerId, CityId)> {
    g.majors(true).flat_map(|p| g.player_cities(p.id()).map(move |c| (p.id(), c.id()))).collect()
}

pub fn run(s: &mut Suite, cr: &mut Criterion) {
    let (g, name) = fixtures::corpus_game(CASE, 200)
        .unwrap_or_else(|| (fixtures::late(), format!("{CASE}/t280 (no corpus)")));
    let pp = AdvisorParams::auto_production();
    let all = cities(&g);
    let asks: Vec<(CityId, BuildingId)> = all
        .iter()
        .flat_map(|&(_, c)| {
            let items = buildable_items(&g, c);
            items
                .buildings
                .iter()
                .chain(items.wonders.iter())
                .map(move |b| (c, b))
                .collect::<Vec<_>>()
        })
        .collect();
    println!(
        "{name}: {} cities of living majors, {} buildings they could build",
        all.len(),
        asks.len()
    );
    let calls = u32::try_from(all.len()).unwrap_or(u32::MAX).max(1);
    let whatifs = u32::try_from(asks.len()).unwrap_or(u32::MAX).max(1);
    let every_city = || {
        for &(p, c) in &all {
            black_box(advisor::advise_production(black_box(&g), p, c, &pp));
        }
    };
    let every_city_kept = || {
        let mut i = 0;
        while i < all.len() {
            let p = all[i].0;
            let adv = advisor::Advisor::new(black_box(&g), p, &pp);
            while i < all.len() && all[i].0 == p {
                black_box(adv.advise(black_box(&g), all[i].1));
                i += 1;
            }
        }
    };
    let every_building = || {
        for &(c, b) in &asks {
            black_box(what_if_building(black_box(&g), c, b));
        }
    };
    cr.bench_function("advisor/every_city", |b| b.iter(every_city));
    cr.bench_function("advisor/every_city_kept", |b| b.iter(every_city_kept));
    cr.bench_function("advisor/every_what_if", |b| b.iter(every_building));
    s.put("advisor/call_per_city", median(11, 3, every_city) / calls);
    s.note("advisor/call_per_city_kept", median(11, 3, every_city_kept) / calls);
    s.note("advisor/what_if", median(11, 3, every_building) / whatifs);
}
