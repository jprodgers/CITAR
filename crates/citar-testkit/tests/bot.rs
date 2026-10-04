//! Integration tests of the bots (DESIGN.md P2.3.11): one file per part under `tests/bot/`.
//!
//! Package 2-00a registered this root with the skeleton's own test; 2-01a added params, memory,
//! streams, owners and idle; 2-01b adds the economy (whole games) and turns (one turn's effects);
//! 2-03 units, war and the fixture sweep.

mod bot {
    mod economy;
    mod idle;
    mod memory;
    mod owners;
    mod params;
    mod skeleton;
    mod streams;
    mod sweep;
    mod turns;
    mod units;
    mod war;
}
