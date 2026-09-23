# citar-engine

The CITAR game engine in Rust: pure, deterministic, and fast. No I/O, no threads, no clock and no
C code. The same seed and the same actions give the same game, digest for digest, on Windows,
Linux and macOS, on x64 and arm64.

[DESIGN.md](DESIGN.md) is the design. This file holds the rules a reviewer checks: the layers,
and the rules no lint can see. [CONTRIBUTING.md](../../CONTRIBUTING.md#rust) has the build setup
and the dev loop.

## What checks what

| Rule | Checked by |
|---|---|
| No hash-order iteration; no platform maths; no half-away-from-zero rounding (`f64::round`, `libm::round`) under a name that does not say so; no order-breaking removals from an `IndexMap`, `IndexSet` or `serde_json::Map`, their entry APIs included; no unstable sorts of slices, `IndexMap`s or `IndexSet`s; no file system, network, processes, environment, threads, or console (print macros and `std::io::{stdin, stdout, stderr}`) | clippy, with the strict [clippy.toml](../../clippy.toml) at the root |
| No `unsafe`, no `exit`, no `dbg!`; no `Change` dropped by a bare `g.set_x();` or by `let _ = g.set_x();` | the workspace lints in the root [Cargo.toml](../../Cargo.toml) |
| Only allow-listed dependencies, with `libm` pinned and without its `arch` feature | `cargo xtask check` |
| One version for the Rust workspace and `citar/__init__.py` | `cargo xtask check` |
| Layering, and who may call `State`'s mutable accessors | `cargo xtask check` |
| Generated files up to date; no `Pending` stage or `NotPorted` left once its time has come | `cargo xtask check` ([xtask/check.toml](../../xtask/check.toml) holds the switches) |
| No `Change` discarded by `_ = g.set_x();`, `let _x = g.set_x();` or `drop(g.set_x())`, which no lint sees | review |
| Everything below | review |

## Layers

Each top-level module is a layer, and a layer may use only the layers listed for it
(DESIGN.md 3.1):

| Layer | Module | May use |
|---|---|---|
| 0 | `base` | nothing |
| 1 | `rules` | `base`, `unique` |
| 1 | `unique` | `base`, `rules` |
| 2 | `state` | `base`, `rules`, `unique` |
| 2 | `save` | `base`, `rules`, `unique`, `state` |
| 2 | `compat` | `base`, `rules`, `unique`, `state`, `save` |
| 3a | `mapgen` | `base`, `rules`, `unique`, `state` |
| 3 | `game` | all of the above |
| 4 | `api` | everything |

- `rules` and `unique` are one layer in two modules: tables hold compiled uniques, and the
  compiler resolves names against the tables.
- Inside a layer anything goes. The game systems call each other freely, as the Python modules
  did.
- **Reach another layer by its layer path**, `crate::base::ids::CityId` or enough `super::`s.
  `cargo xtask check` follows every path that reaches the crate root, including those inside
  macro bodies and those that climb through `self::` and use groups (`use super::{super::x}`),
  so it refuses what would hide the layer a path lands in: a glob of the crate root
  (`use crate::*`, or `use super::*` from a layer's `mod.rs`), an alias of it
  (`use crate as root`, `use super as root`), and a crate-root re-export used from inside a
  layer.
- **A `#[macro_export]` macro belongs to the layer that defines it**, although it lives at the
  crate root: `crate::from_game!()` counts as a use of `game`, so `base` may not call it.
  Macros defined in `lib.rs` (`crate::assert_send!`) may be used everywhere.
- A new top-level module fails the check until it is added to the table in
  `xtask/src/check/layers.rs`.
- **Only `game/mutate.rs`, `save/` and `compat/` may call** `State::{tiles_mut, units_mut,
  cities_mut, players_mut, diplo_mut, world_mut}`. Everything else writes through `Game`'s
  setters, which return a `#[must_use] Change`, or through a `Touch` that bumps revisions before
  it hands out `&mut` (DESIGN.md 6.4). A write that skips the revision bump leaves a memo
  serving a stale answer.

## The rules no lint can see

1. **Persisted and digested types use integers of explicit width, never `usize`.** A save
   written on one target must load, and digest the same, on every other.
2. **Floats in state are finite.** Invariants assert it, and the digest refuses NaN and
   infinity: NaN has many bit patterns, and which one a platform produces is not specified.
3. **Every random draw comes from `Rng::keyed`.** A `Purpose` discriminant never changes once
   assigned, since that would reroll every draw of that purpose in every game. Optional key
   parts go through `KeyPart`, so `None` never collides with tile 0 or player 0.
4. **Every max, min or sort that decides something ends its key in a unique id or tile index.**
   Otherwise a tie is broken by iteration order, and a change anywhere upstream changes the
   game. Remember that `Iterator::max_by_key` returns the last of equal elements and Python's
   `max` the first; `base::order::argmax_first` matches Python.
5. **No recursion over unbounded structures.** The Windows main-thread stack is 1 MB, and a
   stack overflow aborts the process rather than panicking, so it cannot be caught and turned
   into one poisoned game. Filter trees are depth-capped at load, and memo validation depth is
   bounded by the memo graph (at most 8).
6. **Debug-only code never writes state.** Otherwise a debug build and a release build play
   different games.
7. **Game logic never compares strings.** Names are resolved to typed ids when the ruleset
   loads, and rule code compares ids. That is faster, and safer: a typo in a string compare
   fails silently, while a name that does not resolve fails when the ruleset loads.
8. **Layering and restricted mutation access** (above) hold. `cargo xtask check` enforces
   both.

## Also

- Each module's doc comment names the Python lines it replaces, so a reviewer can put the two
  side by side.
- Comments say why, not what.
- A rule effect whose dependency is not ported yet returns `Err(NotPorted("combat::nuke"))`,
  never `todo!()`. A turn or setup stage whose system is not ported yet is `Pending("<package>")`.
- **Write a marker's argument as a string literal at the marker**, because `cargo xtask check`
  counts markers by it: `NotPorted("combat::nuke")`, a helper named `not_ported("combat::nuke")`
  (or `not_ported!`), `Porting::Pending("1b-05")`. A constant or a parameter passed through
  fails the check; a helper under any other name would hide the marker, so it is a review item.
  From 1e-04 any path to the variant, such as `ErrCode::NotPorted`, fails too.
- Integration tests live in `crates/citar-testkit`; this crate has only `#[cfg(test)]` unit tests
  and doctests.

## Features

| Feature | Default | Effect |
|---|---|---|
| `embedded-ruleset` | yes | the ruleset files compiled in through `include_bytes!`, and `Ruleset::shared()` |
| `legacy` | no | `compat::python`, the Python-state converter; refcheck, testkit and bench only |
| `test-ops` | no | `api::testops` and `api::inspect`, for rule scripts; never in shipped builds |
| `checks` | no | invariants and the cache oracle in release builds (debug builds always have them) |
| `stats` | no | memo hit and miss counters, search node counts, settles per round |
