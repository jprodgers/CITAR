//! Any bytes as a save: `Game::load` must refuse what is not one, and a state it loads must keep
//! its invariants (`citar_testkit::fuzz::load_state`).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    citar_testkit::fuzz::load_state(data);
});
