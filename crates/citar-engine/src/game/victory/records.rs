//! What each round records for the graphs and the replay: a statistics row per major civilization
//! (`record_stats`, `victory.py:424-455`) and a frame of the dynamic map (`record_frame`,
//! `victory.py:458-485`), stages R2 and R3.
//!
//! The rows are chronicle entries, digested through the engine's running hash; the frames are
//! host activity, encoded by `save::journal`'s [`FrameWriter`](crate::save::journal::FrameWriter)
//! as a keyframe or a delta from the last frame.

use crate::base::num;
use crate::base::stats::Stat;
use crate::game::Game;
use crate::game::{economy, query};
use crate::save::journal::{self, FullFrame, Record};
use crate::state::chronicle::{CivStats, StatsRow};

/// One major civilization's statistics now (`victory.py:434-454`), or an eliminated one's.
#[must_use]
pub fn civ_stats_row(g: &Game, p: crate::base::ids::PlayerId) -> CivStats {
    let Some(pl) = g.player(p) else { return CivStats::eliminated(p) };
    if !pl.alive() {
        return CivStats::eliminated(p);
    }
    let y = query::civ_stats(g, p).total;
    let tenth = |s: Stat| num::round_ndigits(y[s], 1);
    let (mut cities, mut population) = (0_u16, 0_u32);
    for c in g.player_cities(p) {
        cities = cities.saturating_add(1);
        population = population.saturating_add(u32::from(c.pop));
    }
    CivStats {
        player: p,
        alive: true,
        score: super::score::score(g, p).total,
        cities,
        population,
        land: economy::owned_tiles(g, p).map_or(0, |t| num::saturate_u32(t.len())),
        techs: u16::try_from(pl.tech.known.len()).unwrap_or(u16::MAX),
        policies: u16::try_from(pl.policy.adopted.len()).unwrap_or(u16::MAX),
        military: i64::from(super::score::military_strength(g, p)),
        gold: num::trunc_i64(pl.econ.gold),
        gold_per_turn: tenth(Stat::Gold),
        science: tenth(Stat::Science),
        culture: tenth(Stat::Culture),
        faith: tenth(Stat::Faith),
        production: tenth(Stat::Production),
        happiness: query::happiness(g, p).total,
        era: query::era(g, p),
        units: num::saturate_u32(g.player_units(p).count()),
        golden_age: pl.econ.golden_age_turns > 0,
    }
}

/// Stage R2: this round's row of every major civilization, in id order (`record_stats`).
pub(crate) fn record_stats(g: &mut Game) {
    let turn = g.turn();
    let civs: Vec<CivStats> = g
        .state()
        .players()
        .iter()
        .filter(|(_, p)| p.is_major())
        .map(|(id, _)| civ_stats_row(g, id))
        .collect();
    let row = StatsRow { turn, civs };
    // A row has a canonical form unless one of its floats is not finite, which invariant
    // PLAYER-1 reports of the stocks it is read from.
    let recorded = Record::of(&mut g.st, &mut g.chron).stats(row);
    debug_assert!(recorded.is_ok(), "a stats row that does not encode: {recorded:?}");
}

#[cfg(feature = "test-ops")]
std::thread_local! {
    /// The frames recorded on this thread as they were captured, before encoding (feature
    /// `test-ops`), for the test that decodes the chronicle's frames against them.
    static CAPTURED: core::cell::RefCell<Vec<FullFrame>> =
        const { core::cell::RefCell::new(Vec::new()) };
}

/// Every frame recorded on this thread since the last call, as captured before it was encoded
/// (feature `test-ops`).
#[cfg(feature = "test-ops")]
#[must_use]
pub fn take_frames_for_test() -> Vec<FullFrame> {
    CAPTURED.with(|f| core::mem::take(&mut *f.borrow_mut()))
}

/// Stage R3: the frame of the dynamic map at the end of the round (`record_frame`), with the
/// events since the last frame: a keyframe or a delta from the last frame this game recorded
/// since it was built or loaded.
pub(crate) fn record_frame(g: &mut Game) {
    let from = g.chron.frames().frames.last().and_then(journal::event_range).map_or(0, |r| r.1);
    let to = g.st.host().next_event_id.saturating_sub(1);
    let frame = FullFrame::capture(g.rules, &g.st, (from, to));
    let rec = g.frames.push(&frame);
    #[cfg(feature = "test-ops")]
    CAPTURED.with(|f| f.borrow_mut().push(frame));
    Record::of(&mut g.st, &mut g.chron).frame(rec);
}
