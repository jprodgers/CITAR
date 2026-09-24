//! Integration tests of the engine's modules, against answers recorded from the Python engine
//! and against their own contracts. One file per module under `tests/engine/`.

mod engine {
    mod cities;
    mod convert;
    mod economy;
    mod eval;
    mod filters;
    mod game;
    mod game_eval;
    mod hex;
    mod kitchen_sink;
    mod mapgen;
    mod rules;
    mod save;
    mod scenario;
    mod state;
    mod tools;
    mod turns;
    mod uniques;
}
