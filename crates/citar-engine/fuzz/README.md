# Fuzz targets of citar-engine

Two cargo-fuzz targets (DESIGN.md 9.5, the decision on fuzzing):

- `load_state`: any bytes handed to `Game::load` as a save. It must refuse what is not a save and
  never panic, and a state it loads must keep its invariants.
- `fuzz_one`: any bytes read as a start (a generated duel or small game, or a committed fixture)
  and up to 64 steps (tool calls bound late from `ActionSpec`s, ends of turn, `RandomAgent`
  turns), each checked for the properties P2 to P7, as the property tests check them.

What each target does lives in `citar_testkit::fuzz`, on the stable toolchain, and the testkit
tests run it on a few inputs (`tests/engine/chaos.rs`), so the targets keep compiling and working.
They are not a gate: proptest and chaos are. libFuzzer needs a nightly toolchain and a C++
compiler, so the targets run in WSL at night.

## Running them (WSL)

Clone into `~/` (never build under `/mnt/c`), then, with a nightly toolchain, cargo-fuzz and a
C++ compiler installed:

```sh
cd crates/citar-engine/fuzz
cargo +nightly fuzz run load_state -- -max_total_time=600
cargo +nightly fuzz run fuzz_one -- -max_total_time=1200 -max_len=512
```

`fuzz_one` reads the fixtures from the repository, so run it from the clone. A crash is saved
under `artifacts/<target>/`; `cargo +nightly fuzz run <target> <file>` plays it again.

On a stable toolchain without a C++ compiler, `cargo check --no-default-features` checks that the
targets build.
