//! Integration tests of the engine's modules, against answers recorded from the Python engine
//! and against their own contracts. One file per module under `tests/engine/`.

mod engine {
    mod convert;
    mod eval;
    mod filters;
    mod game;
    mod hex;
    mod kitchen_sink;
    mod rules;
    mod save;
    mod state;
    mod uniques;
}
