//! What the two fuzz targets of `crates/citar-engine/fuzz` run (DESIGN.md 9.5, the decision on
//! fuzzing), on the stable toolchain so that the tests keep them compiling and working:
//! - [`load_state`] hands any bytes to `Game::load` as a save, which must refuse what is not one,
//!   and must load what is, without a panic;
//! - [`fuzz_one`] reads its bytes as a start and a sequence of steps ([`Step`], with each tool
//!   call an [`ActionSpec`] read through `arbitrary`) and plays them checking P2 to P7, as the
//!   properties do.
//!
//! Both panic on what they find, which is how libFuzzer learns of it. They are no gate: cargo
//! fuzz needs a nightly toolchain and runs in WSL at night; proptest and chaos are the gates.

use std::cell::RefCell;

use arbitrary::Unstructured;
use citar_engine::game::{DebugOptions, Game};
use citar_engine::rules::Ruleset;

use crate::agents::RandomAgent;
use crate::spec::ActionSpec;
use crate::stability::{Options, Run, Step};
use crate::{fixtures, games};

/// The most steps one input plays.
pub const MAX_STEPS: usize = 64;

/// Hands `data` to `Game::load` as a save with no journal.
///
/// # Panics
/// If the load does, or a state it loads fails its own invariants.
pub fn load_state(data: &[u8]) {
    let mut none = core::iter::empty();
    if let Ok((g, _)) = Game::load(Ruleset::shared(), data, &mut none) {
        let broken = crate::checks::invariants(&g);
        assert!(broken.is_empty(), "a save that loads breaks invariants: {broken:?}");
    }
}

/// How many starts [`fuzz_one`] picks from: two generated games, then the committed fixtures.
fn starts() -> usize {
    2 + fixtures::committed().map_or(0, |f| f.len())
}

std::thread_local! {
    /// The starts built so far, which each input clones.
    static STARTS: RefCell<Vec<Option<Game>>> = const { RefCell::new(Vec::new()) };
}

/// Start `i`: a duel or a small game on a generated map after ten rounds of `RandomAgent`s, or a
/// committed fixture with every city's citizens assigned by the engine.
fn build_start(i: usize) -> Option<Game> {
    if i < 2 {
        let size = if i == 0 { "duel" } else { "small" };
        let settings = games::random_settings(size, "continents", "wrap_x", 17 + i as u64, 200);
        let mut g = games::new_game(&settings, b"fuzz", DebugOptions::OFF).ok()?;
        let mut agents = vec![RandomAgent::new(); g.state().players().len()];
        games::play_random(&mut g, &mut agents, 10, &mut |_, _| Ok(())).ok()?;
        return Some(g);
    }
    let f = fixtures::committed().ok()?.into_iter().nth(i - 2)?;
    let mut g = games::from_fixture(&f, b"fuzz", DebugOptions::OFF).ok()?;
    let _events = g.assign_every_city_for_test();
    Some(g)
}

/// A copy of start `i`.
fn start(i: usize) -> Option<Game> {
    STARTS.with(|s| {
        let mut s = s.borrow_mut();
        if s.len() <= i {
            s.resize(i + 1, None);
        }
        if s[i].is_none() {
            s[i] = build_start(i);
        }
        s[i].clone()
    })
}

/// Reads `data` as a start and up to [`MAX_STEPS`] steps and plays them, checking P2 to P7.
///
/// # Panics
/// If a property breaks, or the engine panics.
pub fn fuzz_one(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let Ok(which) = u.arbitrary::<u8>() else { return };
    let Some(g) = start(usize::from(which) % starts()) else { return };
    let mut run = Run::new(g, Options { verify_every: 16, save_every: 32, noisy_agents: false });
    for _ in 0..MAX_STEPS {
        let Ok(tag) = u.arbitrary::<u8>() else { break };
        let step = match tag % 16 {
            0 => Step::EndTurn,
            1 => Step::Agent,
            _ => match u.arbitrary::<ActionSpec>() {
                Ok(spec) => Step::Call(spec),
                Err(_) => break,
            },
        };
        if let Err(b) = run.step(&step) {
            panic!("{b}");
        }
    }
    if let Err(b) = run.finish() {
        panic!("{b}");
    }
}
