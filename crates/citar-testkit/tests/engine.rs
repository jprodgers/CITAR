//! Integration tests of the engine's modules, against answers recorded from the Python engine
//! and against their own contracts. One file per module under `tests/engine/`.

mod engine {
    mod filters;
    mod hex;
    mod rules;
    mod state;
    mod uniques;
}
