//! Integration tests of the bots (DESIGN.md P2.3.11): one file per part under `tests/bot/`.
//!
//! Package 2-00a registered this root with the skeleton's own test; 2-01a added params, memory,
//! streams, owners and idle; 2-01b adds the economy (whole games) and turns (one turn's effects);
//! 2-03 units, war and the fixture sweep; 2-05 diplomacy (whole games: the switches, saves) and
//! deals (one turn's diplomacy and what a deal is worth).

mod bot {
    mod deals;
    mod diplomacy;
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
