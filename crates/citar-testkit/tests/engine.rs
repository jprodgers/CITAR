//! Integration tests of the engine's modules, against answers recorded from the Python engine
//! and against their own contracts. One file per module under `tests/engine/`.

mod engine {
    mod advisor;
    mod cities;
    mod city_states;
    mod combat;
    mod convert;
    mod diplomacy;
    mod economy;
    mod eval;
    mod filters;
    mod game;
    mod game_eval;
    mod hex;
    mod kitchen_sink;
    mod mapgen;
    mod production;
    mod religion;
    mod rules;
    mod save;
    mod scenario;
    mod state;
    mod tools;
    mod turns;
    mod uniques;
    mod units;
    mod victory;
    mod vis;
    mod workers;
}
