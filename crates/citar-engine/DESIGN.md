# citar-engine: the Rust engine for CITAR 0.1.6 (Phase 1 design)

**Status.** Final design, 2026-09-23, by the lead architect. It merges five area designs:
- workspace and build;
- state, serialisation and digest;
- ruleset and the unique language;
- turn pipeline, caches and API;
- testing and validation.

A critic then reviewed the merged design. I checked every finding against the code at `4b5a912`. The accepted findings are folded into the body. Where I rejected a finding or changed it, Appendix A says why.

**Sources:**
- `ops/plan-0.1.6.md`, including the 2026-09-23 revision: no bit parity, stability and speed first, fix Python bugs rather than port them, deterministic across platforms from day one;
- `ops/phase0-spec.md`;
- `refcheck/README.md`;
- `citar/engine_api.py`;
- `ops/review-0.1.6/*.json`;
- the Python engine at `v0.1.6` = `4b5a912`. Every `file:line` citation is to that commit.

**What the review changed.** Ten things of substance:
1. **Work-package order.** Turn flow and new-game setup are now skeletons with explicit stage tables. They land early (1b-03); each system package fills its own stages. Goldens are blessed only when every stage is ported.
2. **Settle.** It runs to a fixed point, only after a successful write. Pending work is never saved. `happiness_seen` is committed only at two fixed stages of a turn. So the number of queries or refused calls a host makes cannot change a game (new property P8).
3. **Memos validate themselves on read** through `&Game`, using `Cell`/`RefCell` stamps. The "ensure, then read" protocol is gone, and so is its debug-only freshness check.
4. **Every write to `State` goes through `game::mutate`.** Either a setter returns a `#[must_use] Change`, or a `Touch` bumps revisions before `&mut` is handed out. Seat changes are a `Change`.
5. **Host activity is kept out of the digest.** Host events, thoughts, action records and journal bookkeeping are counted in host-only heads.
6. **Seats carry saved, digested `DriverMemory`** for the Phase 2 bot.
7. **Actions render their results after the settle.** `found_city` gets an owner package.
8. **First contact also fires on owner changes.** Liberation has a proper `revive_player`.
9. **The pathfinding cost cache holds only static costs.** Fog, units, cities and territory are checked per node.
10. **Event emitting keeps Python's private events, `mentions` and coordinate scrubbing.**

Smaller changes cover the unique record size, Python number formatting, seeds and maps supplied by the host, RNG key encoding, `libm` features, float hashing, the budgets, the Python-state converter (now test-only), and saves across ruleset changes.

**Housekeeping first:**
- **The as_player fix is already on `v0.1.6`.** The relayed request was to roll `claude/sweet-raman-2ff95b` (as_player views refused while a human plays) into `v0.1.6`.
  - `v0.1.6`, `origin/v0.1.6` and that branch all point at `4b5a912` ("Test that the god view itself stays closed while a human plays"), on top of `8108f7f` ("Refuse as_player views while a human is playing").
  - `git merge-base --is-ancestor claude/sweet-raman-2ff95b v0.1.6` exits 0.
  - GitHub has no separate pull request for the branch; `gh pr list` shows none. Its commits are on `v0.1.6`, the head of PR #5 (v0.1.6 → main), so they ship with the phase PR. There is nothing to merge.
- **Worktrees:**
  - `.claude/worktrees/inspiring-wu-f6c9f3` (on the sweet-raman branch) and `.claude/worktrees/quirky-hugle-e9a767` (`security-access-fixes`, merged) are clean and can be removed.
  - `.claude/worktrees/priceless-turing-9d3a63` is merged too, but it holds an uncommitted 38-line test in `tests/test_bots.py` (a PYTHONHASHSEED determinism check). It is left alone.

---

## 1. Goals and non-goals

### 1.1 Goals

1. **Stability.**
   - No input can cause a panic: not a tool call, a scenario op, a saved state or ruleset bytes.
   - Every action runs three steps:
     1. it checks, on `&self`;
     2. it applies, in an infallible step;
     3. it settles.

     A refused action returns before anything is written. The digest is unchanged, no event is emitted and nothing settles.
   - **Reads never change a game.** Every query, view and briefing takes `&self`. So the number of queries, views, snapshots and refused calls a host makes can never change the course of a game (property P8).
   - Invariants and a cache oracle run in every test build.
   - If a panic does escape, it poisons that one game. It never takes down the process.
2. **Speed.** The plan's floors are:
   - a small 4-bot Quick game at least 20x faster than Python (25 s or less, once the Phase 2 bot exists);
   - a gargantuan game at least 30x faster;
   - the god view in 20 ms or less;
   - autosave lock time of 100 ms or less.

   The engine budget in §10 aims well past these: the engine-only small game at 5 s or less, gargantuan at 90 s or less.
3. **Determinism.** The same seed and the same actions give identical per-round digests on all five targets:
   - Windows x64;
   - Linux x64 and arm64;
   - macOS arm64 and x64.

   This holds in every build profile, in a single process that runs a game twice, and whatever reads the host interleaves. Proof of work (plan C2) depends on it.
4. **Correct rules without parity.** Porting mistakes are caught three ways:
   - reference checks on 262 recorded states;
   - rule scripts rewritten from the behavioural Python tests;
   - a statistical comparison of whole games (Phase 2).

   Every deliberate difference goes in `refcheck/intended.toml` with a reason, and that list becomes the changelog's list of rule fixes.
5. **Readable, maintainable code.**
   - One responsibility per file, with names taken from the game.
   - Comments say why, not what.
   - Each module's doc comment names the Python lines it replaces.
   - Rule code never compares strings.
6. **A coarse host API** shaped like `citar/engine_api.py`. Its `__all__` is the public surface. Everything heavy is one call with bytes or typed values at the boundary.

### 1.2 Non-goals for Phase 1

- **Bit parity with Python.** Python bugs are not ported (§4, §5.12 and §6 list the fixes).
- **The bot, headless runner, `citar-sim` and PyO3 bindings.** These are Phase 2. Phase 1 plays majors with a keyed random legal-action driver (`RandomAgent`) in `citar-testkit`.
- **File I/O of any kind in the engine.** Maps, scenarios and saves on disk stay in Python. So does drawing a fresh seed: the facade draws it, and the engine requires one.
- **The on-disk save container, zstd compression and journal file framing.** These are Phase 2, in a small `citar-store` crate. The engine produces and consumes uncompressed bytes.
- **Intra-turn parallelism.** No threads, no rayon (§6.13). Throughput comes from running games side by side.
- **Loading Python-format states outside tests.** The Python-state converter (feature `legacy`) serves refcheck, testkit and bench, and never ships.
  - Saves and scenarios from 0.1.5 are archived (plan G).
  - No scenario ships in `citar/data`, which holds only `ruleset/`, `custom/`, `collectors/` and `game.json`.
- **Unique types the Python engine never handled.** The engine supports every unique type and conditional the Python engine handled: the 402 the shipped ruleset uses and 125 more (§5.4, the owner's decision of 2026-09-23 on open question 2). UnCiv's other 110 types do not load, and the error says what to add.
  - The 125 extras compile from package 1a-05b on, and the kitchen-sink test ruleset uses each of them.
  - 1a-07 evaluates the extra conditionals with the others. Each 1b and 1c package implements the extra effects and triggers of its own systems, and tests them with the kitchen sink.

### 1.3 Phase 1 exit criteria

The plan's §7 exit, made concrete:

| Criterion | Measured by |
|---|---|
| Refcheck clean | `cargo refcheck run --fixtures refcheck/fixtures-mini --fixtures refcheck/fixtures-late --fixtures refcheck/corpus --strict`: 0 unexplained, 0 stale, over all 262 states and 14 groups |
| Rule tests pass | about 110 Phase 1 scripts pass on Rust, and on Python except checks tagged `intended` |
| No panics, no corruption | proptest properties P1-P8 at 10,000 cases; chaos for 20 minutes on each OS; a soak of 2,000 small and 200 larger games with invariants on; all with 0 failures |
| Determinism | every golden set identical on 5 targets, under both the `ci` and `release` profiles, and in `same_process_twice` |
| Engine-only speed | §10 gates: a pass round at least 20x faster than Python on every corpus state, god view within 20 ms, snapshot lock within 100 ms on gargantuan |
| Nothing left pending | no `NotPorted` error and no `Pending` stage remains (`cargo xtask check`) |

### 1.4 The load-bearing decisions

| Topic | Decision |
|---|---|
| Crates | Phase 1 has five crates:<br>- `citar-engine` (library, pure);<br>- `citar-testkit` (all integration tests, scripts, goldens, agents);<br>- `citar-refcheck` (tool);<br>- `citar-bench` (criterion and gungraun);<br>- `xtask`.<br>Phase 2 adds `citar-bot`, `citar-sim`, `citar-py` and `citar-store`. |
| Ruleset in a game | `&'static Ruleset`. Hosts load the embedded ruleset once; tests leak one per `RulesetId`. |
| RNG | Our own xoshiro256++ with SplitMix64 key absorption: `Rng::keyed(seed, Purpose, &[u64])`, where optional key parts encode `None` distinctly. No `rand`. No sequential stream; a persisted `combat_seq` counter replaces Python's `g.rng`. |
| Maths | `libm =0.2.16` (default features off) through `base::num`. The std transcendental functions, `f64::round` and unstable sorts are banned by clippy. Python's `round`, `repr(float)` and `round(x, n)` are reproduced in `base::num` and `base::fmt`. |
| Caches | Self-validating memos: revision stamps in `Cell`s, validated lazily on read through `&Game`, with early cutoff. Unique indexes are memos rebuilt from their sources, not maintained by hooks. The lagged values `happiness_seen` and `last_gold_rate` are persisted state, committed at fixed stages. |
| Writes | All mutable access to `State` goes through `game::mutate`, in one of two ways:<br>- setters that return `#[must_use] Change`;<br>- `city_mut`/`player_mut`/`unit_mut` with a `Touch` that bumps revisions first. |
| Settle | Runs only after a successful write, and at the stage points of a turn. It runs to a fixed point. Pending work is empty at every settle point, so it is never saved. |
| History | Events, messages, thoughts, stats rows, actions and frames live in an in-memory `Chronicle`, not in `State`. `State` keeps:<br>- engine heads: counts and a running hash, digested;<br>- host heads: counts of host events, thoughts, action records and frames, saved but never digested.<br>History leaves the game as journal chunks. |
| Tools | A typed `Action` enum, the tools' argument specs and the lenient argument normaliser land with each rule system (1b-1c). The JSON registry and model-facing descriptions land in 1d. |
| Bot boundary | The engine defines `trait SeatDriver: Send` and the `drive` state machine. A seat saves the driver's memory as opaque `DriverMemory`. The bot crate (Phase 2) implements the trait. The production advisor moves into the engine in Phase 1. |

---

## 2. Workspace and build

### 2.1 Crates

| Crate | Kind | Phase | Purpose |
|---|---|---|---|
| `citar-engine` | lib | 1 | The game. `#![forbid(unsafe_code)]`, no I/O, no threads, no clock, no C dependencies. |
| `citar-testkit` | lib + bins `golden`, `chaos`, `soak` | 1 | Every integration test, all in its `tests/`:<br>- `rules.rs`, the libtest-mimic script runner;<br>- `props.rs`;<br>- `determinism.rs`;<br>- `engine.rs`.<br>Also the scenario and script interpreter, fixture loading, `RandomAgent`, and the golden files. |
| `citar-refcheck` | lib + bin | 1 | Reference checks against the recorded Python answers. |
| `citar-bench` | lib + benches | 1 | criterion suites (wall clock on the laptop), gungraun suites (instruction counts in CI), `thresholds.toml`, `perfgate`. |
| `xtask` | bin | 1 | Four commands:<br>- `check`: dependency allow-list, version equality, layering, restricted mutation access, generated files up to date, no `NotPorted`, no `Pending` stages once their gate has passed;<br>- `gen-uniques`;<br>- `perf`;<br>- `ci-local`. |
| `citar-bot` | lib | 2 | The live bot as a named version. Implements `SeatDriver`. Talks only to `game::query` and `Action`. |
| `citar-sim` | bin | 2 | Headless runner and baseline JSONL writer. |
| `citar-py` | cdylib `citar._engine` | 2 | PyO3 0.29, abi3-py311. Each `Game` sits in a `std::sync::Mutex` inside its `#[pyclass]`. Releases the GIL on every heavy call. |
| `citar-store` | lib | 2 | Journal file framing (checked records, torn-tail recovery), the zstd codec, the `.citar` v2 container. Used by citar-py, citar-sim and the helper. |
| `citar-helper` | bin | 4 | Links the engine and the bot. |

**Decision (tests location).** All integration tests live in `citar-testkit`. `citar-engine` itself has only `#[cfg(test)]` unit tests and doctests. This removes the dev-dependency cycle between testkit and engine.

**Decision (benches).** Benchmarks go in a separate `citar-bench` crate. That keeps criterion and gungraun out of the engine manifest, and lets benchmarks use testkit's fixture loader and `RandomAgent`.

### 2.2 Repository layout (Phase 1 additions)

```
Cargo.toml  Cargo.lock (committed)  rust-toolchain.toml  rustfmt.toml  clippy.toml (strict)
.cargo/config.toml    aliases: xtask, refcheck, golden
.config/nextest.toml  profiles default and ci
crates/citar-engine/  README.md (layer rules and the rules no lint can see), unique_types.tsv, unique_supported.toml,
                      src/{lib.rs, base/, rules/, unique/, state/, save/, compat/, mapgen/, game/, api/}, fuzz/ (excluded)
crates/citar-testkit/ src/{script/, agents/, fixtures.rs, golden.rs}, src/bin/{golden,chaos,soak}.rs,
                      tests/{rules.rs, props.rs, determinism.rs, engine.rs, engine/*.rs}, golden/{rng,libm,pyfmt,determinism}.json
crates/citar-refcheck/ clippy.toml (relaxed), src/{main.rs, fixture.rs, compare/, intended.rs, enforced.rs, report.rs, answer/*.rs}
crates/citar-bench/   clippy.toml (relaxed), benches/{kernels,turns,io}.rs, benches/iai.rs, thresholds.toml, src/bin/perfgate.rs
xtask/                clippy.toml (relaxed)
tests/rules/*.toml, tests/rules/maps/*.json, tests/rules/README.md    rule scripts and arena maps
tests/rulescript.py, tests/test_rule_scripts.py                        the Python runner (built on engine_api)
refcheck/fixtures-late/                                                three committed corpus states (about 0.7 MB)
refcheck/intended.toml (v2), refcheck/enforced.toml, refcheck/ratchet.json, refcheck/uniques.json.gz, refcheck/perf/
scripts/refcheck/{hex_vectors,pyfmt_vectors,rules_dump,uniques_dump,filters,not_met_dump,advisor_dump,turn_timing}.py
.github/workflows/{rust.yml, determinism.yml, nightly.yml}
```

- **The late fixtures** are copied from the corpus: `small-continents-normal-s1025/t280` (338 KB), `standard-pangaea-normal-s1031/t120` (218 KB) and `scenario-small-continents-s3001/t61` (123 KB).
- **Why a separate folder.** `record.py --quick` deletes every `*.json.gz` under `fixtures-mini` (`scripts/refcheck/record.py:253-257`), so the late states cannot live there.
- **The count.** 9 mini + 3 late + 250 corpus = 262 states. The 3 late states duplicate corpus files on purpose, so CI has late-game coverage without the corpus.
- **The one-off recorder scripts** under `scripts/refcheck/` use the Python engine directly, as the existing recorder does. Each writes a small committed JSON table that a Rust unit test reads.

### 2.3 Root manifest

```toml
[workspace]
resolver = "3"
members = ["crates/citar-engine", "crates/citar-testkit", "crates/citar-refcheck", "crates/citar-bench", "xtask"]
exclude = ["crates/citar-engine/fuzz"]

[workspace.package]
version = "0.1.6"        # xtask check: equals citar/__init__.py __version__
edition = "2024"
rust-version = "1.98"
license = "MPL-2.0"
publish = false

[workspace.dependencies]
citar-engine  = { path = "crates/citar-engine", default-features = false }
citar-testkit = { path = "crates/citar-testkit" }
# engine (the xtask allow-list: exactly these, plus their transitive closure)
serde        = { version = "1.0.229", features = ["derive"] }
serde_json   = { version = "1.0.151", features = ["preserve_order", "float_roundtrip"] }
indexmap     = { version = "2.14", features = ["serde"] }
rustc-hash   = "2.1"
smallvec     = { version = "1.15", features = ["union", "const_generics", "serde"] }
libm         = { version = "=0.2.16", default-features = false }  # exact pin; no arch paths (see below)
blake3       = { version = "1.8", default-features = false, features = ["std", "pure"] }
thiserror    = "2.0"
base64       = "0.23"
aho-corasick = "1.1"
bitflags     = "2.13"
# tools and tests only
clap = { version = "4.6", features = ["derive"] }
flate2 = "1.1"
toml = "1.1"
globset = "0.4"
similar = "3.2"
rayon = "1"
libtest-mimic = "0.8"
proptest = { version = "1.11", default-features = false, features = ["std"] }
arbitrary = "1.4"
criterion = { version = "0.8", default-features = false, features = ["cargo_bench_support"] }
gungraun = "0.19"         # the successor of iai-callgrind
```

These versions were checked against crates.io on 2026-09-23. The locally cached copies match: libm 0.2.16, serde_json 1.0.151, indexmap 2.14.2, blake3 1.8.7, criterion 0.8.2, proptest 1.11.0, toml 1.1.6.

**`libm` features.** Its default feature `arch` swaps in hardware versions of `sqrt`, `fma` and `rint` on x86-64 and aarch64 (`src/math/arch/mod.rs`). All three are correctly rounded either way, so they could not change a result. They are turned off anyway, so no one has to re-argue the point after a `libm` bump.

**`serde_json` features, and why:**
- **`float_roundtrip`** makes `load(save(s))` reproduce every float bit, and so the digest.
- **`preserve_order`** keeps document order in `Value`. That order matters for `rules_client()`: the browser iterates the ruleset objects in insertion order.
- **`arbitrary_precision`** stays off.

**Decision (no bytemuck, no zstd, no rand in the engine).**
- *bytemuck.* `#[derive(Pod)]` emits `unsafe impl`, which clashes with `forbid(unsafe_code)`. The digest instead writes each tile through a 16-byte `Tile::canon_bytes()`.
- *zstd.* It is C code, so it moves to `citar-store`.
- *rand / rand_chacha.* Covered in §7.

**Features of `citar-engine`:**

| Feature | Default | Effect |
|---|---|---|
| `embedded-ruleset` | yes | `rules::embedded()` pulls in the 24 ruleset files through `include_bytes!`, and adds `Ruleset::shared()` |
| `legacy` | no | `compat::python`, the strict Python `GameState.to_dict()` converter. Enabled by refcheck, testkit and bench only; never by citar-py or the helper. |
| `test-ops` | no | `api::testops` and `api::inspect`, for rule scripts. On in testkit and in CI builds of citar-py; never in shipped wheels or the helper. |
| `checks` | no | Compiles invariants and the cache oracle into release builds. In debug builds they are always compiled. Whether they run is decided at runtime by `DebugOptions`. |
| `stats` | no | Memo hit and miss counters, search node counts, settles per round |

### 2.4 Profiles

```toml
[profile.dev]
opt-level = 1                      # whole-game tests are 10-30x slower at 0
[profile.dev.package."*"]
opt-level = 3
debug = false
[profile.release]                  # ships: wheel extension, helper, citar-sim
opt-level = 3
lto = "fat"
codegen-units = 1
panic = "unwind"                   # never abort: hosts turn a panic into one poisoned game
overflow-checks = true             # kept unless the bench report shows > 3% cost
[profile.ci]
inherits = "release"
lto = false
codegen-units = 16
incremental = false
debug-assertions = true
[profile.profiling]
inherits = "release"
debug = "line-tables-only"
```

**Why the profile cannot change a digest.** Rust never contracts `a*b+c` into a fused multiply-add and has no fast-math mode. The one exception is NaN bit patterns, and the digest refuses NaN outright.

**How this is checked.** `determinism.yml` runs on linux-x64 under both `ci` and `release`.

**Decision.** The testing area's `release-checked` profile and the workspace area's `ci` profile are the same thing; it is called `ci`.

### 2.5 Lints

`[workspace.lints]` is the workspace area's curated list, and every crate uses it:
- `unsafe_code = "deny"`, with the engine adding `#![forbid(unsafe_code)]`;
- `clippy::all` as warn, with CI running `-D warnings`;
- `unused_must_use = "deny"`, and `clippy::let_underscore_must_use = "deny"`, which adds `let _ = change`;
- `iter_over_hash_type = "deny"`;
- `float_cmp`, `lossy_float_literal`, `mem_forget = "deny"`, `exit = "deny"`, `dbg_macro`, `todo`, `unimplemented`.

**The root `clippy.toml` is strict,** and each I/O crate carries a relaxed file that replaces it:
- **disallowed types:** `std::collections::{HashMap, HashSet, BinaryHeap}`, `std::hash::RandomState`, `std::time::{Instant, SystemTime}`, `std::fs::{File, OpenOptions}`, `std::net::{TcpStream, TcpListener, UdpSocket}`, `std::process::Command`;
- **disallowed methods:**
  - `f64`/`f32` `powf`, `powi`, `exp`, `exp2`, `exp_m1`, `ln`, `ln_1p`, `log`, `log2`, `log10`, `cbrt`, `hypot`, the trig and hyperbolic functions, `mul_add`;
  - `f64::round`, `f32::round` and `libm::{round, roundf, Libm::round}`, with the reason pointing to `num::round_half_even` (Python's `round()`) or `num::round_half_away`;
  - `indexmap::IndexMap::{remove, swap_remove}` and `indexmap::IndexSet::{remove, swap_remove}`, because they reorder entries that were ordered by insertion. `shift_remove` is the sanctioned form. The same goes for their other swap forms, for the `swap_remove` of the entry APIs, and for `serde_json::Map`, whose plain `remove` is `swap_remove` under `preserve_order`;
  - `slice::sort_unstable*` and `select_nth_unstable*`, and the `sort_unstable*` and `sorted_unstable_by` of `IndexMap` and `IndexSet`;
  - `std::fs::*`, `std::env::*`, `std::thread::{spawn, sleep}`, `std::process::exit`, `std::io::{stdin, stdout, stderr}`;
- **disallowed macros:** `print`, `println`, `eprint`, `eprintln`, `dbg`.

**What I verified on Rust 1.98.1**, with a scratch workspace mirroring this layout:
- the root `clippy.toml` applies to a nested member crate;
- `f64::powf`, `f64::powi`, `f64::sin`, `f64::mul_add` (both method-call and path-call syntax), `slice::sort_unstable` and `slice::sort_unstable_by_key` are each reported;
- `BinaryHeap` and `HashMap` are reported as disallowed types;
- `iter_over_hash_type` fires on a `for` loop over a `HashSet`, but **not** on `map.values().sum()`.

So banning the hash types outright is what closes that hole. `base::collections::LookupMap` is the one sanctioned wrapper: an `FxHashMap` behind `#[allow(clippy::disallowed_types)]` that exposes no iteration at all.

**Decision (no grep lint).** The check above shows clippy matches inherent f64 methods, so the testing area's `xtask lint-det` grep is dropped. `same_process_twice` stays as the backstop.

**Rules no lint can see.** These are written in `crates/citar-engine/README.md` and are part of review:
1. Persisted and digested types use integers of explicit width, never `usize`.
2. Floats in state are finite. Invariants assert it, and the digest refuses NaN and infinity.
3. Every random draw comes from `Rng::keyed`:
   - a `Purpose` discriminant never changes once assigned;
   - optional key parts go through `KeyPart`, so `None` never collides with 0.
4. Every max, min or sort that decides something ends its key in a unique id or tile index.
5. No recursion over unbounded structures. The Windows main-thread stack is 1 MB, and an overflow aborts rather than panics. Filter trees are depth-capped at load; memo validation depth is bounded by the memo graph (at most 8).
6. Debug-only code never writes state.
7. Game logic never compares strings.
8. Layering (§3.1) and restricted mutation access (§6.4) are checked by `cargo xtask check`.

### 2.6 CI

**Decision (workflow files).** Separate workflows, not jobs in `test.yml`:
- path filters keep Python-only pull requests fast;
- the five-target matrix gets its own summary.

`test.yml` is unchanged. It picks up `tests/test_rule_scripts.py` on its own through `unittest discover`.

**`rust.yml`.** It runs on pull requests and pushes that touch `crates/**`, `Cargo.*`, `refcheck/**`, `tests/rules/**`, `citar/data/**` or the workflow itself.

| Job | Runs on | What it runs |
|---|---|---|
| lint | ubuntu | `cargo fmt --check`; `cargo clippy --workspace --all-targets --all-features --locked -D warnings`; `cargo xtask check`; `cargo doc` with `-D warnings` |
| test | ubuntu, windows, macos-arm64 | `cargo nextest run --workspace --all-features --cargo-profile ci --profile ci`, which covers:<br>- unit tests;<br>- rule scripts;<br>- props at 64 cases;<br>- the short golden set.<br>Then `cargo test --doc`. |
| refcheck | ubuntu | `fixtures-mini` + `fixtures-late` on the groups in `enforced.toml`, plus the ratchet |
| perf | ubuntu, pull requests only | gungraun instruction counts against the base branch. More than +5% fails unless the pull request carries the label `perf-accepted`. |

**`determinism.yml`.**
- The matrix: linux-x64 (`ubuntu-24.04`), linux-arm64 (`ubuntu-24.04-arm`), windows-x64, macos-arm64 and macos-x64 (`macos-15-intel`; if that label is retired, the fallback is `x86_64-apple-darwin` under Rosetta).
- Each runs `golden check --out golden-<target>.json`.
- A compare job fails in two cases:
  - the targets disagree with each other, which is a determinism bug;
  - they agree with each other but differ from the committed file, which is a behaviour change: bless and commit.
- The ci-vs-release run happens on linux-x64.

**`nightly.yml`:**
- the full-corpus refcheck `--strict` (needs the corpus asset, open question 1);
- props at 10,000 cases;
- chaos for 20 minutes on each OS;
- a reduced soak;
- the long golden set;
- a criterion trend run.

**Caching and tools.** `Swatinem/rust-cache@v2` runs with shared keys `ci` and `lint`, and `taiki-e/install-action` installs nextest.

**Dependabot.** A cargo entry runs weekly with all crates grouped. `libm` is ignored; it is bumped by hand with re-blessed digests.

### 2.7 Local setup on the laptop

- **Target directory outside OneDrive.** The repository lives in OneDrive, and a `target/` there causes os error 32 and gigabytes of sync churn.
  - Set `CARGO_TARGET_DIR` to a folder outside it, one per worktree. Implementation agents run in `.claude/worktrees/*`, and a shared target directory would serialise their builds and thrash between branches. For example: `C:\dev\target\citar-<worktree>`.
  - Prefer a Dev Drive.
- **WSL.** Clone into `~/`; do not build under `/mnt/c`. gungraun needs valgrind, so local instruction counts run in WSL.
- **While the Python baseline runs** (8 workers, §9.8):
  - set `CARGO_BUILD_JOBS=4`;
  - treat criterion numbers as indicative. Until 1e-03, criterion thresholds are report-only, with a hard failure only above 3x the budget (§10).
- **The dev loop:**
  - `cargo nextest run`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo refcheck run --fixtures refcheck/fixtures-mini`
  - `cargo golden check`
  - `cargo xtask perf`

---

## 3. Module map and porting order

### 3.1 Layers

```
base    (0)  ids, sets, collections, num, fmt, rng, order, hex, text, stats, digest -> nothing
rules   (1)  Ruleset, typed tables, names, constants, client JSON, gen tables       -> base, unique
unique  (1)  UniqueType, compiler, filters, conditionals, evaluation, index build   -> base, rules
state   (2)  the persisted model, Change, Chronicle types, config                   -> base, rules, unique
save    (2)  JSON codec, canonical digest, load/validate, journal chunks, summary   -> base, rules, unique, state
compat  (2)  compat::python (feature legacy)                                        -> base, rules, unique, state, save
mapgen  (3a) procedural maps                                                         -> base, rules, unique, state
game    (3)  Game, Derived, mutate, every rule system, turn flow, setup, Action, query, invariants -> all of the above
api     (4)  host surface: tools, views, briefing, scenario, maps, debug, inspect    -> everything
```

- **One layer, two modules.** `rules` and `unique` depend on each other: tables hold compiled uniques, and the compiler resolves names against the tables. They are one layer in two modules for readability.
- **Game systems may call each other.** The Python import graph is fully cyclic (for example combat → conquest → cities → combat), and that is fine inside one layer.
- **Enforcement.** `xtask check` scans `use crate::…` paths per directory. It also restricts calls to `State::{tiles_mut, units_mut, cities_mut, players_mut, diplo_mut, world_mut, config_mut}` to `game/mutate.rs`, `save/` and `compat/`.
- **Decision (module names).** The base layer is `base`, not `core`, which would clash with the `core` crate. The pipeline area's modules are folded in as follows:
  - rng, math and order go into `base`;
  - derive, vis, path, turn and setup go under `game/`;
  - the AI modules go under their systems: `game::barbarians`, `game::city_states::ai`, `game::automation`.
- **Decision (ids).** Every id newtype, entity ids and rule ids alike, lives in `base::ids` and is generated by one `define_id!` macro. The unique evaluator (layer 1) needs `PlayerId`, `CityId`, `UnitId` and `TileIdx`, so they cannot live in `state`.

### 3.2 Module tree

```
src/lib.rs            forbid(unsafe_code); 64-bit and little-endian asserts; Send (not Sync) asserts; re-exports
src/base/
  ids.rs              define_id!; entity ids; rule ids; IdVec<I,T>
  sets.rs             IdSet<I, const W>, BitSet, PlayerSet(u64), PlayerVec<T>
  collections.rs      DetMap/DetSet (IndexMap with FxBuildHasher), LookupMap (no iteration), MinHeap (ties by push order)
  num.rs              libm wrappers, floor_div/floor_mod, round_half_even, round_half_away, round_ndigits, saturating casts
  fmt.rs              PyFloat (Python repr of a float), fmt helpers for model-facing numbers
  rng.rs              Rng (xoshiro256++), Purpose, KeyPart, keyed derivation, distributions
  order.rs            argmax_first/argmin_first, total_cmp keys
  hex.rs              HexGrid: odd-r and cube, wraps, neighbour table, distance, within/ring/line
  text.rs             name normalisation (rules.py:26-30), possessives (game.py:17-23), word-boundary scanning, coordinate scrubber
  stats.rs            Stat, Stats([f64; 7]), StatMask
  digest.rs           CanonSerializer (a serde Serializer over a 64 KB block buffer) and Digest
src/rules/            mod.rs (Ruleset), source.rs (RulesetFiles, RulesetId, embedded), raw.rs, defs.rs, load.rs,
                      derived.rs, names.rs (resolve), constants.rs, client.rs, errors.rs, gen_tables.rs
src/unique/           gen.rs (GENERATED), text.rs, params.rs, compile.rs, filter/{mod,expr,parse,statics,unit,tile,city,civ}.rs,
                      cond.rs, countable.rs, world.rs (EvalWorld, TileFacts, Ctx), query.rs (uq), index.rs (Csr, CivIndex,
                      CivSources), trigger.rs (TriggerKind, TriggerCond, TriggerEvent, OneTimeEffect)
src/state/            mod.rs (State, TurnClock, IdCounters), store.rs (EntityStore), map.rs (MapInfo, Tile, Tiles),
                      memory.rs, units.rs, cities.rs, players.rs, diplo.rs, world.rs, config.rs, chronicle.rs, change.rs
src/save/             mod.rs (Snapshot), ctx.rs, json.rs, columns.rs, canon.rs, chain.rs, validate.rs, migrate.rs,
                      summary.rs, journal.rs (chunks and frame deltas)
src/compat/python/    mirror.rs, convert.rs, report.rs
src/mapgen/           options, noise, landmass, terrain, rivers, starts, wonders, resources, ruins, generate, prepare, metrics
src/game/             core.rs (Game), mutate.rs (Touch, the Game setters), pending.rs, events.rs, eval.rs (EvalView),
                      debug.rs, invariants.rs, action.rs, query.rs, advisor.rs
  derive/             mod.rs (Derived), rev.rs (Rev, Revs, Stamp, Memo, CopyMemo, CondDeps mapping), civ.rs, city.rs,
                      tile.rs, conn.rs, buildable.rs, jobs.rs, danger.rs, spatial.rs, oracle.rs
  vis/                los.rs, visibility.rs, effects.rs
  path/               class.rs, cost.rs (MoveCosts, RouteLayer), node.rs (dynamic per-node checks), astar.rs, tree.rs, cache.rs
  turn/               stages.rs (the stage tables), driver.rs, settle.rs, drive.rs (SeatDriver, DriverMemory, Stop)
  setup.rs            Game::new: the setup stage table
  tiles.rs economy.rs research.rs policies.rs religion.rs great_people.rs triggers.rs ruins.rs units.rs movement.rs
  cities/{uniques,stats,citizens,borders,construction,purchase,queue,free_buildings,connections,founding,lifecycle}.rs
  combat/{combatant,strength,resolve,city,air,nuke}.rs  conquest.rs workers.rs actions.rs automation.rs barbarians.rs
  city_states/{influence,actions,quests,ai}.rs  espionage.rs  diplomacy/{category,relations,deals,negotiation}.rs
  victory/{score,un,milestones,records}.rs
src/api/              game.rs (host methods), error.rs, summary.rs, tools/{args,normalize,registry,query_tools,text}.rs,
                      views/{client,info,events,replay}.rs, briefing.rs, scenario.rs, maps.rs, debug.rs,
                      inspect.rs and testops.rs (feature test-ops)
```

### 3.3 Python → Rust

| Python | Rust | Package |
|---|---|---|
| hexmap.py:1-214 | `base::hex` | 1a-02 |
| game.py:557-559 (`state_rng`), 118-122, 995-1002 (`g.rng`, `save_rng`); mapgen.py:1476-1490 (`_side_rng`) | `base::rng` | 1a-02 |
| rules.py:37-342, `citar/data/**` | `rules::*` | 1a-03 |
| unique_types.py; uniques.py:28-213 | `unique::{gen, text, params, compile}` | 1a-05 |
| uniques.py:294-776; mapgen unique consumers | `unique::filter`, `rules::gen_tables` | 1a-06 |
| uniques.py:215-293, 778-1083; economy.py:64-147; triggers.py:13-74; cities.py:1169-1193 | `unique::{cond, countable, world, query, index, trigger}` | 1a-07 |
| state.py:13-405; game.py:32-60 | `state::*` | 1a-08 |
| session.py save path, `victory.record_frame` (victory.py:458-485) | `save::*` | 1a-09 |
| `GameState.to_dict` format | `compat::python` | 1a-10 |
| game.py:100-145, 320-540, 565-609, 656-994 | `game::{core, mutate, events, derive::rev}` | 1b-01 |
| scenario.py:36-536 (16 ops); tools.py:113-127 (argument coercion) | `api::scenario` (framework), `api::tools::{args, normalize}` | 1b-02; ops land with their systems |
| turns.py:20-202; game.py:1004-1037; game.py:145-318 skeleton; maps.py validate and tiles_from_rows | `game::turn`, `game::setup` (stage tables) | 1b-03 |
| mapgen.py:1-1721; maps.py:80-100, 346-365 | `mapgen::*`, `api::maps::generate_map` | 1b-04 |
| economy.py:64-353, 500-746; cities.py:34-104; units.py:21-53; turns.py:120-131 | `game::{economy, derive::civ}`, `game::cities::uniques` | 1b-05 |
| tiles.py:1-391; cities.py:246-926, 1967-2072; economy.py:404-499 | `game::{tiles, derive::tile, cities::{stats, citizens, connections}}` | 1b-06 |
| cities.py:927-1965, 2073-2391; research.py; policies.py | `game::cities::{borders, construction, purchase, queue, free_buildings, founding, lifecycle}`, `research`, `policies` | 1b-07 |
| religion.py (not the unit actions); great_people.py; triggers.py:75-374; ruins.py | `game::{religion, great_people, triggers, ruins}` | 1b-08 |
| visibility.py; units.py sight; espionage.py:444-451 | `game::vis` | 1c-01 |
| units.py; movement.py | `game::{units, movement, path}` | 1c-02 |
| combat.py; conquest.py | `game::{combat, conquest}` | 1c-03 |
| workers.py; actions.py; automation.py; religion.py spread actions; tools.py found_city | `game::{workers, actions, automation, derive::jobs}` | 1c-04 |
| diplomacy.py; espionage.py (spies, theft) | `game::{diplomacy, espionage}` | 1c-05 |
| barbarians.py; city_states.py; espionage.py (elections, rigging, coups) | `game::{barbarians, city_states, espionage}` | 1c-06 |
| cities.py:1696-1717; bots/basic.py:777-875, 1146-1590 | `game::advisor` | 1c-07 |
| victory.py; turns.py:190-202 | `game::victory` | 1c-08 |
| game.py:145-318 (remaining stages); maps.py:323-365 (`prepare`) | `game::setup`, `mapgen::prepare`, `game::turn::drive` | 1c-09 |
| tools.py (61 tools) | `game::action` (typed, with each system), then `api::tools::registry` (JSON, text) | 1b-1c, then 1d-01 |
| views.py; game.py:891-990; briefing.py; maps.py; scenario.py:537-645 | `api::{views, briefing, maps, scenario}` | 1d-02, 1d-03 |

**Where the facade's names land.** `engine_api.__all__` maps as follows:
- `rules_version`, `rules_client`, `max_players`, `map_sizes`, `speeds`, `difficulties`, `resolve_name` and `ruleset_counts` → `Ruleset` methods;
- `map_types` → `mapgen::MAP_TYPES`;
- `tool_list` and `tool_kind` → `api::tools`;
- `validate_map`, `blank_map`, `generate_map` (now requires a seed, which the facade draws) and `map_summary` → `api::maps`;
- `scenario_ops_help` and `scenario_summary` → `api::scenario`;
- `DIPLOMACY_CATEGORIES`, `item_category` and `proposal_categories` → `game::diplomacy::category`;
- `state_summary` → `save::summary`;
- `RULES_OVERVIEW` and `MAP_LEGEND` → `api::text`;
- `bot_instance`, `bot_set_diplomacy`, `bot_owns_negotiation` and `run_game` → `citar-bot` (Phase 2);
- `list_*`, `load_*`, `save_map` and `delete_*` stay in Python;
- `EngineGame` → `Game` (§8).

### 3.4 Porting order and slicing

The sub-phases are:
- 1a, foundation;
- 1b, the game core, skeletons, map generation and the economy core;
- 1c, everything else through turn flow, setup and the driver;
- 1d, tool registry, views and briefing;
- 1e, hardening.

The work breakdown gives the packages and their gates. Five rules keep each package verifiable when it is done:

1. **Every system package brings its refcheck answer module** (`citar-refcheck/src/answer/<group>.rs`) and adds its paths to `refcheck/enforced.toml`. The ratchet then protects them.
2. **Every system package brings the rest of its surface:**
   - its typed `Action` variants;
   - the argument specs of its tools (names, JSON types, required list, used by `normalize` from day one);
   - its scenario ops and its rule scripts;
   - its `RandomAgent` moves, so the agent exercises new actions as soon as they exist.
3. **Every system package wires its own stages into the stage tables** of §6.2 and §6.14. A stage whose system is not ported yet is listed as `Pending("<package>")` and runs as an explicit no-op. Goldens refuse to bless while any stage they depend on is pending.
4. **A rule effect whose dependency is not ported yet returns `Err(NotPorted("combat::nuke"))`.** It never uses `todo!()`. Tests assert on it, and `xtask check` fails 1e-04 if any `NotPorted` remains.
5. **A rule script lands with the package that ports what it tests,** never earlier. It is validated on Python first.

---

## 4. State model

### 4.1 Principles

1. **Typed, with names only at the edges.** Every ruleset reference is a `u8` or `u16` rule id, and every entity is a newtype id. Names appear only in the JSON save, in journal chunks, in the Python converter and in views.
2. **One serde derive, two encodings.**
   - The save is JSON in human-readable mode, with rule ids written as names.
   - The digest is `CANON_V1`: a binary encoding in non-human-readable mode, with rule ids written as integers.
   - So what is saved is hashed, and what is derived (`#[serde(skip)]`) is not. The one exception is `HostOnly<T>`: it is saved but hashes as nothing.
3. **Persisted and derived stay separate.** `State` is saved. `Derived` (§6) is a pure function of `State` and can always be recomputed cold with the same result. Anything that cannot be recomputed cold is a field of `State`, including lagged values.
4. **Pending work is never persisted.** Work raised during a call (citizen rechecks, dirty vision sources) lives in `Game.pending`. It is drained by settle, and it is empty at every settle point, where saves and snapshots are taken.
5. **Reads never create state.** Python creates state when reading, in `diplomacy.relation` (`diplomacy.py:65-74`), `victory._un`, a city's religious pressures and `great_people.points_required`. Rust gives these explicit defaults, so `digest(load(save(s))) == digest(s)`.
6. **Writes are funnelled.** Outside `state`, only `game::mutate` can obtain `&mut` to any part of `State` (§6.4).
7. **Iteration order is defined everywhere.**
   - Entity stores iterate by ascending id.
   - Maps are `BTreeMap`, or `IndexMap` where insertion order means something (with `shift_remove` only).
   - Lists with set meaning are kept sorted, or are bitsets.
8. **History is not state.** Events, messages, thoughts, stats rows, actions and replay frames are append-only. They live in the in-memory `Chronicle` held by `Game`, and `State` keeps only its heads (§4.7).

### 4.2 Ids and containers

```rust
pub type Turn = i32;                        // negatives are sentinels (sacked_turn = -1000)
pub struct PlayerId(pub u8);                // at most 64 players (PlayerSet is a u64); today's maximum is 24+32+1 = 57
pub struct TileIdx(pub u32);                // y * width + x, odd-r (hexmap.py:1-11)
pub struct UnitId(NonZeroU32);  pub struct CityId(NonZeroU32);  pub struct CampId(NonZeroU32);
pub struct DealId(NonZeroU32);  pub struct NegotiationId(NonZeroU32);
pub struct EventId(NonZeroU32); pub struct MessageId(NonZeroU32);
pub struct ReligionId(u8);                  // founding order: an index into World::religions
// rule ids: u16 unless tile-hot
TechId, BaseUnitId (units.json), UnitTypeId (unit_types.json), BuildingId, PromotionId, PolicyId (branches first),
BeliefId, NationId, PersonalityId, QuestKindId, RuinId, UniqueId, CondId, StatsId, FracId, TextId(u32), AbilityKey,
UnitFilterId, TileFilterId, CityFilterId, CivFilterId, CombatantFilterId;
TerrainId(u8), FeatureId(u8), ResourceId(u8), ImprovementId(u8), EraId(u8), SpeedId(u8), DifficultyId(u8),
VictoryId(u8), SpecialistId(u8), CityStateTypeId(u8), RulesReligionId(u8), TagId(u8)
```

- **Id allocation.** Entity ids are monotonic and never reused.
  - `IdCounters { unit, city, camp, deal, negotiation, combat_seq }` are persisted.
  - Message ids come from the engine heads.
  - Event ids come from the host heads' `next_event_id`, a single feed sequence shared by engine and host events (§4.7).
- **Converting Python ids.** Python drew units, cities and camps from one shared `next_id`, starting at 1 (`state.py:343`, `game.py:551-555`), so `NonZeroU32` is safe. The converter starts all three counters at Python's `next_id`, so converted ids stay unique across kinds.
- **Decision (unit naming).** UnCiv's names win: `BaseUnitId` is a row of `units.json` (127 of them) and `UnitTypeId` is a row of `unit_types.json` (28 of them). `Unit.base: BaseUnitId`.
- **Decision (set widths):**

  | Set | Words | Used |
  |---|---|---|
  | `TechSet = IdSet<TechId, 2>` | 2 | 80 |
  | `PolicySet` | 2 | 70 |
  | `BuildingSet` | 4 | 124 |
  | `BaseUnitSet` | 4 | 127 |
  | `PromotionSet` | 4 | 106 |
  | `BeliefSet` | 2 | 56 |
  | `TerrainSet` | 1 | 33 |
  | `ResourceSet` | 1 | 35 |
  | `ImprovementSet` | 1 | 35 |
  | `EraSet` | 1 | 9 |
  | `FeatureSet(u16)` | — | 10 |

  `PromotionSet` takes the rules area's 256 bits; 128 would leave mods almost no room. A table larger than its set is a load error that names the constant to raise.
- **`EntityStore<I, T>`** holds units and cities:
  - fields `slots: Vec<Option<T>>` in ascending id order, `ids: Vec<I>`, `pos: Vec<u32>` (raw id to slot; `u32::MAX` means absent) and `live: u32`;
  - `insert` requires a new id greater than the last, so it is always a push;
  - `get` is O(1) through `pos`;
  - `remove` leaves a tombstone, and the store compacts when there are more than 64 dead slots and they are more than a quarter of the live count;
  - iteration is by ascending id; `get2_mut` serves attacker/defender pairs; `ids()` takes a snapshot for loops that mutate.
- **Decision (derived per-entity arrays).** Derived per-entity data is indexed by raw id (`IdVec<CityId, CityMemos>`, grown on demand). Ids are never reused, and even a converted Python game with a shared id space stays within a few tens of thousands of entries.

### 4.3 Map and tiles

**`MapInfo`** is `{ width: u16, height: u16, wrap_x: bool, wrap_y: bool, continents: Vec<u16> }`, where `u16::MAX` means water. The `HexGrid` (in `base::hex`) is derived from it:
- a neighbour table `Vec<[u32; 6]>` in Python's direction order E, NE, NW, W, SW, SE (`hexmap.py:14-16`), with `u32::MAX` off the map;
- cube coordinates;
- the wrap shifts.

**Decision (tile order in `within`).** Python sorts `within(idx, r)` by distance, keeping its dq/dr enumeration order among ties (`hexmap.py:138-157`), and caches up to 200k results. Rust generates the order ring by ring, with no cache:
- each ring starts at the east neighbour;
- it goes counter-clockwise, following the direction table.

The order is documented and deterministic. Rules that pick among ties use an explicit tie key (§7.4). Where a Python rule took "the first tile in `within` order", as `find_spawn_tile` does (game.py:787-794), the new order is a listed intended difference.

**`Tile`** is a 16-byte `#[repr(C)]` plain struct (an array of structs), with private fields read through typed accessors:

```rust
pub struct Tile {
    terrain: TerrainId,      // u8, base terrain (9 in the data)
    wonder: u8,              // natural-wonder TerrainId + 1, 0 = none (14 in the data)
    resource: u8,            // ResourceId + 1
    resource_amount: u8,
    improvement: u8,         // ImprovementId + 1
    route: RouteBits,        // bits 0-1 none/road/railroad; bit 2 route pillaged; bit 3 improvement pillaged
    owner: u8,               // PlayerId, 0xFF = none
    river: u8,               // 6-bit edge mask, the grid's direction order
    features: FeatureSet,    // u16 over the 10 terrain features, Hill included
    _reserved: u16,
    city: u32,               // raw CityId that owns this tile, 0 = none (state.py:78)
}
const _: () = assert!(core::mem::size_of::<Tile>() == 16);
```

**Why an array of structs.** The hot loops read many fields of a tile and its neighbours:
- A* edge cost reads terrain, features, river edge, route and pillage bits, and owner;
- yields read almost every field;
- worker scoring reads improvement, resource and features.

One 64-byte cache line holds 4 tiles, and a whole 160×100 map is 256 KB, which fits in L2. Kernels that want one field per tile (movement cost classes, sight heights, cached yields) keep derived arrays of their own.

**Features.**
- `FeatureSet::top()` returns the feature with the highest layer (Hill lowest, Fallout highest; the rules loader supplies the order). That reproduces Python's "last non-hill feature" (`state.py:101-107`) for every legal combination.
- Python's separate `fallout` bool is OR-ed into the features. Nukes appended Fallout to `features` anyway (`combat.py:1203-1215`), so worker automation, which read the bool (`automation.py:354`), never cleared real fallout. Listed in `intended.toml`.

**`Tiles { tiles: Vec<Tile>, builds: BTreeMap<TileIdx, BuildQueue> }`**, where `BuildQueue = SmallVec<[BuildStep; 2]>`. The setters (`set_owner(idx, Option<(PlayerId, CityId)>)`, `set_improvement`, `set_route`, `set_pillaged`, `set_features`, `set_terrain`, `set_resource`, `push_build`, `pop_build`) each return `#[must_use] Change` (§6.4). Only `game::mutate` can reach them.

**Explored tiles and memory.**
- `Player.explored: BitSet` is persisted for every non-barbarian player.
- The visible counts and bitsets are derived (§6.9).
- Last-seen memory is kept for majors only. Python also kept it for city-states (visibility.py:141-146) and never read it.

```rust
pub struct TileMemory { features: FeatureSet, improvement: u8, route: RouteBits, owner: u8, flags: u8, _pad: [u8; 2] } // 8 B
pub struct TileMemoryLayer { tiles: Vec<TileMemory>, cities: BTreeMap<TileIdx, CityMemory> }
pub struct CityMemory { name: Box<str>, pop: u16, owner: PlayerId }
```

That is 128 KB per major on a 160×100 map. In Python, memory was most of a player's 3 MB of JSON.

### 4.4 Units and cities

Entity structs keep their fields `pub` for reading, but `&mut Unit` and `&mut City` exist only inside `state` and `game::mutate` (§6.4). Owner, tile and ownership links are private even there, so only the setters that emit `Change`s can move them.

```rust
pub struct Unit {
    pub id: UnitId, pub base: BaseUnitId, owner: PlayerId, tile: TileIdx,   // owner and tile private: only Units changes them
    pub hp: i16, pub moves: i32, pub xp: i32, pub promotions: PromotionSet, pub promotion_count: u8,
    pub pending_promotions: u8, pub fortify: u8, pub activity: Option<Activity>, pub goto: Option<TileIdx>,
    pub path: Vec<TileIdx>, pub order_wait: u8, pub attacks: u8, pub interceptions: u8, pub acted: bool,
    pub set_up: bool, pub name: Option<Box<str>>, pub camp: Option<CampId>, pub created_turn: Turn,
    pub religion: Option<ReligionId>, pub religious_strength: i16, pub religious_strength_lost: i16,
    pub abilities_used: SmallVec<[(AbilityKey, u8); 2]>,   // sorted; replaces the "placeholder|params" keys (units.py:417,472)
    carried_by: Option<UnitId>, pub origin_city: Option<CityId>, pub original_owner: Option<PlayerId>,
    pub return_offer: Option<PlayerId>, pub explore: ExploreMemory,       // moved off Player.flags (automation.py:236-285)
}
pub enum Activity { Fortify, FortifyHeal, Sleep, SleepHeal, Heal, Build, Goto, Explore, Automate, AirSweep }
```

**`Units`** is the store plus two structural indexes, rebuilt on load and never persisted:
- occupancy, as an intrusive list per tile in ascending id order (`occ_head: Vec<u32>`, `occ_next: Vec<u32>`);
- `by_owner: PlayerVec<Vec<UnitId>>`.

Its operations are `spawn`, `despawn`, `relocate`, `set_owner`, `board` and `unboard`, plus the reads `at(tile)`, `of(owner)` and `get`.
- `relocate` cascades to units whose `carried_by` is the moving unit, as `place_unit` does (game.py:766-785), and keeps occupancy in step.
- `despawn` clears `carried_by` on the units it carried (game.py:751-764).
- `Unit.build` and `due_heal` are dropped: nothing ever reads them.

```rust
pub struct City {
    pub id: CityId, pub name: Box<str>, owner: PlayerId, tile: TileIdx, pub founder: PlayerId,
    pub previous_owner: Option<PlayerId>, pub founded_turn: Turn, pub turn_acquired: Turn, pub original_capital: bool,
    pub pop: u16, pub food: f64, pub culture: f64, pub tiles_claimed: u16, pub tiles_bought: u16,
    pub buildings: BuildingSet, pub free_buildings: BuildingSet,          // moved from Player.free_buildings[str(cid)]
    pub queue: SmallVec<[Constructible; 4]>, pub progress: BTreeMap<Constructible, f64>, pub overflow: f64,
    pub bought_this_turn: SmallVec<[Constructible; 2]>, pub auto_production: bool,
    pub worked: Vec<TileIdx>, pub locked: Vec<TileIdx>,                   // sorted
    pub specialists: [u8; MAX_SPECIALISTS], pub manual_specialists: bool, pub focus: CityFocus, pub avoid_growth: bool,
    pub citizens_settled: bool,    // NEW: this engine has assigned this city's citizens at least once (§6.8)
    pub health: i32, pub damaged_turn: Turn, pub attacked: bool, pub sacked_turn: Turn, pub puppet: bool,
    pub resistance: i16, pub razing: bool,
    pub pressures: SmallVec<[(Option<ReligionId>, i32); 4]>,               // sorted; None = no religion; seeded at creation
    pub religions_adopted: SmallVec<[ReligionId; 2]>, pub holy_city_of: Option<ReligionId>,
    pub wltkd: i16, pub demanded_resource: Option<ResourceId>, pub demand_countdown: i16,
}
pub enum Constructible { Building(BuildingId), Unit(BaseUnitId), Perpetual(Perpetual /* Gold|Science|Nothing */) }
```

- **`Cities`** is the store plus `by_owner`, with `found`, `set_owner` and `remove`.
- **City at a tile.** There is no separate index: `city_at(idx)` reads `tiles[idx].city` and checks that city's tile.
- **Operations that span containers** live on `State`: `transfer_city` (cities, tiles and capitals), `kill_player` and `revive_player`, its inverse:
  - `alive = true`;
  - `eliminated_turn = None`.

  Liberation needs it (conquest.py:262-265). Vision and eligibility for first contact follow from `Change::PlayerAlive`.
- **A pressure bug fixed.** A city's religious pressures are seeded when the city is created, not on first read.

### 4.5 Players and the typed flags

```rust
pub struct Player {
    pub id: PlayerId, pub kind: PlayerKind /* Major|CityState|Barbarian */, pub name: Box<str>, pub leader: Box<str>,
    pub color: Rgb, pub nation: NationId, pub seat: Seat, pub alive: bool, pub eliminated_turn: Option<Turn>,
    pub capital: Option<CityId>, pub original_capital: Option<CityId>, pub founded_city: bool, pub city_counter: u16,
    pub start_tile: Option<TileIdx>, pub explored: BitSet,
    pub econ: Economy,        // gold, culture, faith: f64; golden_age_points/turns/count; total_culture, total_faith: i64;
                              // culture_hist, science_hist: [i32; 8];
                              // last_gold_rate: f64 (NEW, written at E2);
                              // happiness_seen: i32 (NEW, committed at S1 and E1, §6.6)
    pub tech: TechState,      // known: TechSet; queue: Vec<TechId>; goal; progress: BTreeMap<TechId,f64>; overflow;
                              // free_techs; future_techs; ra_bonus: i32
    pub policy: PolicyState,  // adopted: PolicySet; adopted_count; free_policies
    pub gp: GreatPeople,      // points/combat_points: BTreeMap<BaseUnitId,f64>; pool_threshold; combat_threshold; free;
                              // earned; prophets_earned; maya_limited; long_count_pool
    pub faith: ReligionState, // progress: None|Pantheon|Founding|Religion|Enhancing|Enhanced; founded; free_beliefs;
                              // choose_pantheon_belief
    pub civ: CivExtras,       // temp_uniques: Vec<TempUnique { unique: UniqueId, turns: i16 }>; built/bought_increasing;
                              // free_stat_buildings; free_specific_buildings; natural_wonders: TerrainSet;
                              // units_gained: BaseUnitSet; revolt_in: Option<i16>; last_ruins: [Option<RuinId>; 2];
                              // explore_skip: Vec<TileIdx> (sorted, capped at 300)
    pub major: Option<Box<MajorData>>,          // memory, spies, spy_eras_earned, spaceship, notes, cs_attacks, cs_gp_gift
    pub city_state: Option<Box<CityStateData>>, // type, personality, resource, unique_unit, influence: PlayerVec<f64>, ally,
                                                // protectors: PlayerSet, quests, timers, pairs: PlayerVec<CsPair>,
                                                // war_quests, election_in, barb_help_cd, recently_bullied
}
pub struct Seat { controller: Controller, handicap: Handicap, auto: AutoDecisions, overrides: SeatOverrides,
                  difficulty: Option<DifficultyId>, driver: Option<DriverMemory> }   // the Phase 0 split (state.py:20-63)
pub enum Controller { Human, Llm, Mcp, Bot, Hybrid, Minor, Barbarian }
pub struct DriverMemory { pub kind: u16, pub version: u16, pub bytes: Box<[u8]> }   // opaque to the engine
```

**Driver memory.**
- **What it is for.** The Python bot keeps its war plans, preparations, garrisons and escorts in the object (`bots/basic.py:678-697`), so they are lost on every save and load. Plan 2.1 wants typed bot memory "saved with the game".
- **Where it lives.** `Seat.driver` is that slot, frozen into save format v1 now so Phase 2 needs no format bump.
- **What the engine does with it.** It never interprets the bytes. It saves them as base64, digests them as a length plus bytes, and hands them to the seat's driver on each turn (§6.12).
- **What it makes possible.** A proof-of-work job resumed from a save plays exactly as the uninterrupted run.

**The 28 keys of `Player.flags` become typed fields,** following the state area's table:
- `start` → `start_tile`;
- `culture_last8` and `science_last8` → `econ.*_hist`;
- `ra_science` → `tech.ra_bonus`;
- `gp_threshold` → `gp.pool_threshold`;
- `revolt_in` → `civ.revolt_in`;
- the explorer keys → `Unit.explore`;
- `pairs`, `quest_state`, `war_quests`, `election_in`, `barb_help_cd` and `recently_bullied` → `city_state`;
- `eras_spy_earned`, `cs_attacks` and `cs_gp_gift` → `major`.

**Two bugs fixed and listed in `intended.toml`:**
- **`last_gold_rate` was read but never written.** `civ_stats_gold` reads it (`cities.py:777-784`), but neither `gold_rate_prev` nor `last_gold_rate` has a writer, so the citizen rule "gold per turn < 0" (`cities.py:765`) never fired. Rust writes the rate at stage E2.
- **`gained_<unit>` was read but never written** (`movement.py:134`), so Carthage's mountain crossing never triggered. Rust keeps `civ.units_gained`.

**Dropped as dead:**
- `last_stats` (written at `turns.py:90`, never read);
- `unreachable_explore`, `cs_unit_timer`, `tribute_turn`, `ruins_rewards`, `spy_eras`, `faith_buys` and the float `gp_threshold`;
- the `GameState` fields `barbarian_state`, `capture_ids` and `first_discovered`, which no module outside `state.py` reads or writes.

**Sets replace ordered lists** for techs, policies, natural wonders, protectors, buildings and met civilizations:
- Views sort them in ruleset order, or by player id for civilizations.
- Refcheck compares them as multisets.
- `empire_summary`'s `at_war_with` followed `Player.met`'s meeting order (engine_api.py:534). It is now by player id, which is one intended entry.

**Quest data.** Quest `data1` becomes `QuestTarget`: None, Tile, Resource, Building, UnitType, Player, NaturalWonder, Religion, Baseline(i64) or Percent(i16), chosen by quest kind. `data2` is always empty and is dropped.

### 4.6 Diplomacy and world

```rust
pub struct Relation {        // unordered pair (lo < hi); folds in relations["a,b"] and open_borders["a>b"]
    pub met: bool, pub war: bool, pub war_declared_by: Option<PlayerId>, pub since: Turn, pub treaty_until: Turn,
    pub friendship_until: Turn, pub pact_until: Turn, pub ra_until: Turn, pub ra_science: [i32; 2],
    pub embassy: [bool; 2], pub denounced_until: [Turn; 2], pub open_borders_until: [Turn; 2],
}
pub struct PairMatrix<T> { n: u8, cells: Vec<T> }   // idx(lo, hi) = hi*(hi-1)/2 + lo
pub struct Diplomacy { relations: PairMatrix<Relation>, war_mask: PlayerVec<PlayerSet> /* structural, rebuilt on load */,
                       met_mask: PlayerVec<PlayerSet> /* structural */, pub opinions: OpinionBook /* BTreeMap<(holder, about), [f64; 16]> */,
                       pub deals: Vec<Deal>, pub negotiations: Vec<Negotiation> }
pub enum DealItem { Gold{amount}, GoldPerTurn{amount, turns}, Resource{resource, amount, turns}, OpenBorders{turns},
                    Embassy, PeaceTreaty, DeclarationOfFriendship, ResearchAgreement, DefensivePact,
                    DeclareWar{target}, City{city_id}, ShareMap, Tech{tech} }       // serde tag = "type", snake_case
pub struct Negotiation { id, initiator, responder, turn, status: NegStatus, awaiting: Option<PlayerId>,
                         proposal: Option<Terms>, proposal_by: Option<PlayerId>, history: Vec<NegEntry>, deal: Option<DealId> }
pub struct NegEntry { seq: u16, by: Option<PlayerId>, action: NegAction /* Open|Reply|Counter|Accept|Reject|Close */,
                      message: Box<str>, proposal: Option<Terms>, turn: Turn, note: Option<Box<str>> }
pub struct World { religions: Vec<Religion>, wonders_built: BTreeMap<BuildingId, CityId>, un: Un, camps: BTreeMap<CampId, Camp> }
```

- **Hot checks are O(1).** `at_war(a, b)` is one bit test on `war_mask[a]`, and barbarians are always at war (`game.py:664-672`). `has_met` is one bit test on `met_mask[a]`.
- **The Phase 0 chat model is kept:** `seq`, a required message, `note`, the Close action and `by: None`. `exchanges` is dropped.
- **Scenario opinions now count.** The scenario op wrote them under the key `"a>b"` (`scenario.py:429`), but readers look them up by `str(holder)` (`diplomacy.py:86-96`), so they never counted. The converter moves them to holder `a`.
- **The UN tally is keyed by player id.** Python keyed it by civilization name, which breaks after a rename.
- **"Enhanced" is derived from a religion's beliefs.** Python read a key it never set.

### 4.7 The chronicle and its heads

```rust
pub struct Event { id: EventId, turn: Turn, kind: EventType /* Engine(EngineEvent) | Host(Box<str>) */, text: Box<str>,
                   audience: Option<PlayerSet> /* None = public */, tile: Option<TileIdx>, data: Option<Box<EventData>>,
                   refs: SmallVec<[NameRef; 2]>, origin: Origin }
pub struct NameRef { start: u32, end: u32 /* UTF-8 byte offsets */, player: PlayerId, kind: RefKind /* Civ|Leader|City */ }
pub struct Chronicle { events: Vec<Event>, messages: Vec<Message>, thoughts: Vec<Thought>, stats: Vec<StatsRow>,
                       actions: Vec<ActionRecord>, frames: FrameLog }
pub struct ChronicleHeads {   // in State, digested: only what the engine produces
    engine_events: u32, messages: u32, stats: u32, hash: [u8; 32], last_stats: Option<StatsRow> }
pub struct HostHeads {        // in State as HostOnly<HostHeads>: saved, never digested
    next_event_id: u32, host_events: u32, thoughts: u32, actions: u32, frames: u32, journal_seq: u32 }
```

- **Event types.** `EngineEvent` covers the 97 event types the engine emits today; a test greps `citar/engine` to confirm the list is complete. `EngineEvent::is_private()` is a const table ported from `PRIVATE_EVENTS` (game.py:808-812).
- **Event data.** `EventData` is a flat struct of typed optional fields covering every key `emit` passes today. Scrubbing therefore walks typed player fields instead of `_EVENT_PID_KEYS`.
- **Name references.** `refs` are computed at emit time (§8.4) and stored as byte offsets. The converter turns Python's code-point offsets into byte offsets.
- **Decision (what the digest covers).** Host activity must not move the digest, or the proof-of-work chain would depend on server incidents and on what a host chose to log. Host activity here means:
  - host events: `agent_error`, `game_paused` and `game_resumed` (session.py:359-637, llm_agent.py:295-626);
  - thoughts (`add_thought`);
  - action records;
  - frame and journal bookkeeping.

  So the heads are split in two:
  - **Engine heads (`ChronicleHeads`).** They count engine events, messages and stats rows, and keep a running hash `hash = blake3(hash ‖ kind ‖ canon(entry))`. For an event, `canon(entry)` covers turn, kind, text, audience, tile, data and refs, but **not its id**. The running hash catches divergence in event text, which the state digest alone would miss.
  - **Host heads (`HostHeads`).** They count everything else, and assign `EventId`s from one feed sequence shared by engine and host events, so `events(since)` stays one ordered feed. A host event shifts later event ids, but ids are not digested and no state refers to them.

  The critic's alternative was to drop the chronicle hash entirely. It was rejected: the hash is cheap, and it is the only thing that pins event wording across platforms.
- **Stats rows for Phase 2.**
  - `StatsRow` must carry baseline.py's 33 `STAT_KEYS`.
  - `war_declared` and `city_captured` events must carry attacker/defender and old/new owner.
  - The Phase 2 runner writes the baseline JSONL from these.

### 4.8 `State` and `Game`

```rust
pub struct State {
    pub config: GameConfig, pub map: MapInfo, pub tiles: Tiles, pub players: PlayerVec<Player>,
    pub units: Units, pub cities: Cities, pub diplo: Diplomacy, pub world: World,
    pub clock: TurnClock,          // turn, current, turn_started, phase: Playing|Over, winner, victory
    pub seed: u64,                 // config seed; every Rng stream derives from it (§7)
    pub ids: IdCounters,           // unit, city, camp, deal, negotiation, combat_seq
    pub chronicle: ChronicleHeads, // engine heads, digested
    pub host: HostOnly<HostHeads>, // host heads, saved, not digested
}
```

All the `pub` fields are readable through `&State`. `&mut` access to any of them goes through the `pub(crate)` accessors that only `game::mutate`, `save` and `compat` may call (§6.4).

`Game`, in §6.1, holds:
- `rules: &'static Ruleset`, `st: State`, `dv: Derived`, `chron: Chronicle`;
- the pending-work set, the effect queue and the frame writer;
- `DebugOptions` and the poison flag.

**Decision (`&'static Ruleset`).** The alternatives were `Arc<Ruleset>` (pipeline and state areas) and `&'static` (workspace area). `&'static` wins:
- `let r = g.rules;` is a plain copy, so split borrows are easy in free rule functions;
- hosts load the embedded ruleset once with `Ruleset::shared()`;
- tests and tools call `Ruleset::leak(files)`, which is interned by `RulesetId` so each ruleset leaks once.

**`GameConfig`** is typed:
- `SpeedId`, `DifficultyId`, map type, edges, barbarian level, victory toggles, resource options and `diplomacy.max_chat_messages`;
- `seed: u64`, always present: the host draws it when the lobby leaves it empty;
- `map: MapSource`, either `Generated { size, type }` or `Editor { id, size }`, an editor map by id. The document itself is a setup argument: `Game::new` takes a `NewGame`, the settings plus the inline `MapDoc`, reads it into the tiles and drops it, as Python kept only the id (game.py:174). Kept in `GameConfig` it would be saved twice, walked by every round's digest, and digested in the key order the lobby sent. The host resolves a map id to its document; Python's `maps.load_map` call inside `Game.new` (game.py:165-170) stays in Python.
- `host: HostOnly<BTreeMap<String, Value>>` keeps keys the engine does not know verbatim. It is saved but never digested.

Seats live only in `Player.seat`.

**As built in 1a-08** (§4.2-4.8, §6.4):
- **Files.** `state/{mod, store, map, memory, units, cities, players, diplo, world, config, chronicle, change}.rs`, with unit tests in `state/tests.rs`; the gates' properties and Python checks are in `crates/citar-testkit/tests/engine/state.rs`.
- **Private parts.** `State`'s parts are private fields read through getters (`st.tiles()`, `st.player(p)`, `st.clock()`, ...), not `pub` fields: a `pub` field would hand anyone holding `&mut State` a way round the accessors. The same goes for what a `Change` must report: a unit's owner, tile and carrier, a city's owner and tile, both ids, a player's seat and whether it is alive, and a city-state's ally. `State::from_parts(StateParts)` builds a state from its parts (save, converter) after checking they fit and rebuilding the indexes; `into_parts` takes it apart.
- **Accessors.** `config_mut` joins the restricted list, since nearly every cache reads the settings, and `cargo xtask check` also fails if `state/mod.rs` stops defining one of them. `ids_mut`, `chronicle_mut` and `host_mut` are `pub(crate)` and unrestricted: counters and history heads feed no cache.
- **Changes.** `TileOwner { t, old, new }` carries a `TileClaim` (owner and city: a scenario can give a tile an owner and no city). `CityRemoved { c, owner, at }` carries what the removed city no longer can. A write that moves several things returns `Changes`, `#[must_use]` too, in order: `Units::relocate` (the unit, then its cargo), `State::transfer_city` (the city, then its tiles), `State::kill_player` (its units' removals, then `PlayerAlive`), `Diplomacy::update` (`Met` if contact changed, `Diplo` if anything else did), `Units::despawn` (the removal, then each unit it carried leaving it). Boarding and leaving a carrier, including a carrier's removal leaving its cargo, report `UnitPlaced` with `from == to`: being carried matters to healing, city air capacity and carrier capacity. The setters beyond the tiles', units' and cities': `State::{set_clock, set_controller, set_auto_decision, set_seat_difficulty, set_ally}`.
- **Units.** A carried unit moved off its carrier's tile leaves it. Carriers do not nest. `Units::verify` and `Cities::verify` check the indexes; `State::check_indexes` checks them all.
- **Loading is safe by construction.** Whatever order `save` and `compat` call things in:
  - `EntityStore` refuses an id above `store::MAX_ENTITY_ID` (2^24) with `StoreError::TooLarge` before anything grows, so a corrupt id cannot ask for gigabytes (the store's lookup table and the occupancy links grow to the largest id: 64 MB each at worst).
  - `State::from_parts` refuses, as `StateError::Mismatch`: a tile, unit or city owner that is not a player; a tile claimed by a missing city; a city off the map; an explored set or a major's memory that does not fit the map; an id counter at or below a unit, city, camp, deal or negotiation id in use. Units off the map and broken carrier links were already refused.
  - `Tiles::from_parts` refuses a build queue off the map or empty rather than dropping it, and `DriverMemory::new` refuses more than `MAX_LEN` bytes.
- **Players.** The religion state is `Player.religion` (the design's `faith`, which read as the stock). The seat model is `Seat::new` (defaults, then overrides), `Seat::restore` (a save's fields, a missing handicap derived) and `SeatOverrides::parse`, which checks the lobby's `handicap` and `auto` with Python's messages. City-state quest timers keep Python's -1 for "not scheduled"; `QuestTimers.individual` is sparse. `DriverMemory`'s fields are private: `DriverMemory::new` holds it to `MAX_LEN` (4 MiB) on the way in, so the engine never writes a save it would refuse to load.
- **Diplomacy.** A `PairMatrix<Relation>` covers every pair, barbarians included; `Diplomacy::new(n, barbarians)` puts the barbarians at war with everyone in the masks. Two-sided relation fields are indexed by `diplo::side(p, q)`, 0 for the lower id. The 16 opinion reasons are `OpinionKey`. `DealItem::{to_json, from_json}` and `Terms::{to_json, from_json}` read and write Python's dicts with a `&Ruleset`; `scripts/refcheck/deal_items.py` records the dicts the test compares.
- **Config.** `MapSource::Generated { size, map_type, edges, dims }` and `Editor { id, size }`, where `size` is the lobby size nearest the map's area, which views show (game.py:177-179). `NewGame::{generated, editor}` pair the settings with the editor document so that they agree. Map sizes, map types and barbarian levels are the new ids `MapSizeId`, `MapTypeId` and `BarbarianLevelId`: `Constants` holds those lists as `IdVec`s, `Constants::{map_size_id, map_type_id, barbarian_level_id}` find one by its exact key (what a save writes), and the loader refuses a list longer than 256. They are not `Named`: `NameKind` is Python's `Rules.tables()`, resolved loosely by display name for tools. Victories switched off are listed (`disabled_victories`), so the default is all of them. `GameConfig::new` takes the essentials; `config_from_json` (1b-03) fills the rest.
- **History.** `EngineEvent` is generated from a table of the 97 types with their `is_private`; `PRIVATE_EVENTS` has two more names that nothing emits (`build_cancelled`, `city_razing`). `Event` has no `origin`: `EventType` already tells engine events from host ones. `EventData` has the 32 keys `emit` is passed. A stats row is `StatsRow { turn, civs }` with every key `victory.record_stats` wrote, which includes `baseline.py`'s eight `STAT_KEYS`. `ChronicleHeads::absorb` folds an entry into the running hash; `HostHeads::take_event_id` hands out the shared event ids from 1. `FrameLog` holds the frames `save::journal` encodes.
- **Serialisation is 1a-09's.** The state types derived no serde in 1a-08: the save writes rule ids as names, which needs `save::ctx`, so 1a-09 added the derives and the custom encodings together (§4.9-4.11, as built). `Tile::canon_bytes` and `TileMemory::canon_bytes` are what `save::columns` hashes.
- **Specialists.** `City.specialists` is `[u8; MAX_SPECIALISTS]` with `sets::MAX_SPECIALISTS = 8`, and the loader refuses a larger table.

### 4.9 Save format v1 (JSON)

The top-level keys are written in this order:

```json
{"format":"citar-state","version":1,"engine":"0.1.6+<BUILD_ID>","rules":{"id":"<RulesetId hex>"},
 "config":{...},"map":{"width":...,"continents":"<b64 u16le>"},
 "tiles":{"palette":{"terrain":[...],"feature":[...],"resource":[...],"improvement":[...]},
          "terrain":"<b64>","wonder":"<b64>",...,"features":"<b64 u16le>","city":"<b64 u32le>","builds":[[idx,[["Farm",5]]]]},
 "clock":{...},"ids":{...},"chronicle":{...},"host":{...},"players":[...],"units":[...],"cities":[...],
 "diplomacy":{"relations":[[lo,hi,{...}]],"opinions":[[holder,about,{...}]],"deals":[...],"negotiations":[...]},
 "world":{...}}
```

- **Rule ids are written as names.** `save::ctx::with_rules(rules, || ...)` installs a scoped thread-local ruleset context. Only the save and journal codecs install it, and a missing context is a serde error, never a panic.
- **Ruleset changes.**
  - **A different ruleset loads with a warning.** A save whose `RulesetId` differs from the loading ruleset's is a `LoadReport.rules_changed` warning, not an error. Saves therefore survive a balance tweak or a reordered ruleset.
  - **An unresolvable name is an error.** Loading fails only when a saved name no longer resolves: `LoadError::UnknownName { path, name }`.
  - **Proof-of-work verification is separate and exact.** It requires `RulesetId` and `BUILD_ID` to match.
- **Columns.** Tiles and memory layers are written as base64 columns, each layer with its own palette. That is about 20x smaller than Python's 14-element rows.
- **Floats** are written with ryu and parsed with `float_roundtrip`, so `load(save(s)) == s` bit for bit, including `-0.0`.
- **Banned attributes.** `skip_serializing_if`, `flatten` and `untagged` are banned in state types, because they would make the canonical encoding ambiguous. A test scans `src/state/**` for them, and the files outside it whose types a state holds (`rules/{defs,mod}.rs`, `base/{sets,ids,codec,stats}.rs`).
- **Versioning.** `save::migrate::upgrade(&mut Value, from)` applies migrations at the JSON level. An unknown top-level key is a `LoadError` in every build: behaviour never depends on the build profile.
- **`Snapshot`** is a deep clone of `State` plus `&'static Ruleset`. It costs about 1-3 ms at gargantuan and is taken under the session lock, which fixes the torn saves. `Snapshot::to_json()` runs off the lock.
- **Loading.**
  - `Game::load(rules, state_json, chunks)` deserialises, then calls `rebuild_indexes()` and `validate()`.
  - `validate()` checks referential integrity, ranges, finite floats, `PlayerVec` lengths, palettes and `DriverMemory` sizes. `State::from_parts` already refuses entity ids above `MAX_ENTITY_ID`, owners that are not players, tiles claimed by missing cities, cities off the map, explored sets and memories that do not fit, and id counters behind ids in use (§4.8, as built), so `validate()` need not repeat those.
  - A corrupt save returns a `LoadError` and can never panic later.
- **`save::summary(bytes)`** replaces `engine_api.state_summary`, reading `chronicle.last_stats` for the scores.

### 4.10 Canonical digest

- **`CANON_V1`** is implemented by `base::digest::CanonSerializer`, with `is_human_readable() == false`. The encoding:
  - bool as 1 byte; integers as little-endian at their declared width;
  - floats as `to_bits()`, as they are; NaN or infinity is an error;
  - strings as a `u32` length plus the bytes; `Option` as a tag;
  - enum variants as a `u32` index;
  - sequences and maps as a `u32` length plus the items; an unknown length is an error;
  - structs as their fields in order.
- **Decision (floats).** The earlier draft mapped `-0.0` to `+0.0`. The arithmetic is deterministic, so a platform producing `-0.0` where another produces `+0.0` is a real divergence. Canonicalising would hide it instead of preventing it, so the bits are hashed as they are.
- **Custom canonical forms:**
  - `Tiles` and `TileMemoryLayer` as the concatenated `canon_bytes()` of each tile;
  - bitsets as raw words; rule ids as integers;
  - `PairMatrix` as every cell; `EntityStore` as its live items in id order;
  - `DriverMemory` as kind, version, then length and bytes;
  - `HostOnly` as nothing.
- **The digest and the chain:**
  - `Digest = blake3("CITAR-DIGEST" ‖ CANON_V1 ‖ RulesetId ‖ canon(State))`.
  - `chain_0 = blake3("CITAR-CHAIN" ‖ spec_hash)`, then `chain_t = blake3(chain_{t-1} ‖ turn_le ‖ digest_t)`.
  - The digest is taken at the end of each round, after `end_round` and before the next `begin_turn`. It is opt-in: on for jobs and tests, off for lobby games.
- **Cost.**
  - `CanonSerializer` writes into a reusable 64 KB block buffer and feeds blake3 whole blocks. Tile arrays go straight through as large slices, so blake3 gets its multi-chunk SIMD path instead of one tiny update per field.
  - About 4 MB is encoded at gargantuan late game: about 2-3 ms per round. The bench gate (1a-09) is 5 ms.

### 4.11 Journal chunks, and what waits for Phase 2

- **In the engine (Phase 1).** `Game::take_journal_chunk() -> JournalChunk { seq, json: Vec<u8> }` encodes everything added to the chronicle since the last take, including frames and host activity, and bumps `host.journal_seq`.
- **Frames** replace `victory.record_frame`, which made up 88% of every Python save:
  - a **keyframe** goes out on the first frame after a load and every 64 rounds: full owner, improvement, route, feature and explored layers, plus a palette;
  - every **other frame is a delta**:
    - changed tiles as 8-byte records;
    - newly explored tile indices per major, varint-delta coded;
    - the full cities list;
    - units packed at 8 bytes each;
    - the event range.
- **Loading history.** `Game::load` accepts the chunk payloads (`&mut dyn Iterator<Item = &[u8]>`) and rebuilds the `Chronicle`. If the counts or the running hash disagree with the heads, it sets `LoadReport.chronicle_incomplete`. The simulation is unaffected either way.
- **Replay data.** `replay_data(format)` serves two formats:
  - `Full` is the current frame shape, which `replay.js:91-118` decodes: full owner, improvement, route and feature layers, and each major's explored bitmap, per frame. It is kept for the transition.
  - `Delta` is keyframes plus deltas, as stored.

  Expanding full frames on the server recreates the payload bloat plan 6.8 removes. So Phase 3's web work teaches `replay.js` the `Delta` format, and `Full` is then retired.
- **In `citar-store` (Phase 2):**
  - record framing: a `CITARJNL` header, then records of `len | kind | codec | blake3[..8] | payload`;
  - torn-tail recovery, and zstd;
  - the `.citar` v2 container: `zstd({format:"citar-save", version:2, session, metrics, engine:<state JSON>, journal:<file>})`. The host appends the journal before it writes the snapshot.

**Decision (journal framing).** The engine is kept pure, and hosts share one codec crate.

**As built in 1a-09** (§4.9-4.11):
- **Files.** `save/{mod, ctx, columns, json, canon, chain, validate, migrate, summary, journal}.rs`, and `base/codec.rs` for the forms that need no ruleset. The gates' tests are in `crates/citar-testkit/tests/engine/save.rs`; the synthetic states they round-trip are `citar_testkit::states`; the digest bench is `crates/citar-bench/benches/digest.rs`.
- **One derive, two encodings.** Every state type derives `Serialize` and `Deserialize` with `deny_unknown_fields`, and a form that differs between the two encodings asks `is_human_readable`: JSON reads well and stays small, `CANON_V1` stays fixed-width. `base::codec` has the forms that need no ruleset (`pairs` for maps whose keys JSON cannot write as strings, `bytes_b64`, `hash_hex`, and `serde_by_name` for fieldless enums: the name in JSON, the index in `CANON_V1`); `IdSet` is its members in JSON and its `W` words canonically, `BitSet` base64 of its words in JSON, `PlayerSet` its ids in JSON and its `u64` canonically. The forms that need the ruleset or the containers' private parts are trait impls in `save` (a crate may implement a trait for its own type in any module): rule ids in `save::ctx`; the map, tiles, build steps and memories in `save::columns`; units, cities, diplomacy and driver memory in `save::json`. `State`'s own derive is the canonical form; the save writes its own top level.
- **Names.** `save::ctx::with_rules(&'static Ruleset, f)` sets a thread-local that is put back even if `f` panics. The 16 `NameKind` tables are named by `Ruleset::name` and `lookup`; features by their terrain's name; city-state types, founded-religion names, quest kinds and ruin rewards by their names; map sizes, map types and barbarian levels by their lobby keys; a `UniqueId` (a temporary unique) by its `UniqueMeta::key` in 16 hex digits, its identity across rulesets; an `AbilityKey` or a `TextId` (a great-person pool) by its text. A name that does not resolve is recorded in the context, so the loader reports `UnknownName { path: "cities[3] (building)", name }`. A city's specialists are written as `{name: count}` for the non-zero ones, so a reordered specialist table cannot misread them.
- **The document.** The seed is in `config`, not at the top level. Relations are written only where they differ from a fresh one, in the matrix's order; opinions as `[holder, about, {reason: value}]` with every value whose bits are not `+0.0`. A save refuses a non-finite float before writing anything (`SaveError::NonFinite`, by the canonical walk), since JSON would write `null`. A gargantuan late-game save is about 6 MB; writing one takes about 21 ms and loading it about 50 ms on the laptop.
- **Loading** (`save::load(rules, bytes, chunks) -> Loaded { state, chronicle, report }`; `json::read_state` without the history). The document is read into a JSON value, checked for its `format`, upgraded by `migrate::upgrade`, and every top-level key checked (`UnknownKey`; a missing key is `Json`). The map's shape is checked before the unit indexes are sized by it. Players, units and cities are read one by one, so an error names the item (`units[17]`); more than 64 players are refused before the relations' player-set masks are built. The four lists kept sorted by rule id (`disabled_victories`, resource rules, `free_specific_buildings`, `abilities_used`) are sorted again, stably, since under a reordered ruleset the same names have other ids; a name listed twice stays twice for `validate` to refuse. `Units::from_units`, `Cities::from_cities`, the diplomacy (relations in order, no opinion about oneself) and `State::from_parts` build the state, and `validate` checks it. Their refusals (`StoreError::TooLarge`, `TileError`, `DriverTooLarge`, `StateError::Mismatch`) are `LoadError::Invalid` with a place. `LoadReport` carries `rules_changed: Option<(saved RulesetId, saved engine version)>`, `chronicle_incomplete` and the save's `engine`. A save round-trips through a ruleset with its buildings, units, resources and victories reversed and back to the same canonical bytes.
- **`validate`** checks what §4.9 lists that `from_parts` does not: capitals held by their player; cities that may since have been razed (original capitals, units' homes, spies' cities, deal cities, built wonders) against the city counter; players, tiles and religions everywhere else they are named, each major's remembered owners and the opinions' holders and subjects (never oneself) included; every rule id within its table, remembered improvements and features included (for converted states, where no name was resolved); sorted lists sorted; player-indexed lists one entry per player (a city-state's influence and pairs, which `State::new` sizes; tightened in the 1a-10 fix round, so that indexing by a player cannot panic and a missing slot and a zero one cannot digest apart); entity counters from 1 (a counter at 0 would never hand out an id) and within `MAX_ENTITY_ID + 1`; majors, and only majors, with major data, and city-states likewise; the barbarian aggression a percentage and explicit map dimensions within the grid's; every float finite. It stops at 100 problems.
- **The digest.** `save::digest(rules, st)`, or a `Digester` that keeps its 64 KB buffer between rounds; `DigestChain::{new(spec_hash), push(turn, &digest), head(), rounds()}`, and `DigestChain::resume(head, rounds)` after a load: the chain covers states, so no state holds it, and a host that chains keeps its head and length beside the save (from Phase 2 in the `.citar` container's session record). The synthetic gargantuan state (`Shape::GARGANTUAN`: 24 majors, 32 city-states, 400 cities, 2,500 units, 80% explored and remembered) is 4.1 MB canonically and digests in 3.0 ms on the laptop; the bench fails above 15 ms. The golden set `states` pins the digests of three checked-in states (`crates/citar-testkit/testdata/states/`, written once by `golden states`), and the determinism workflow compares it on the five targets.
- **Chronicle entries.** `save::canon::{event_entry, message_entry, stats_entry}` are the bytes `ChronicleHeads::absorb` folds in: an event without its id, and a message or a stats row whole. `Chronicle` records the order entries were appended in (`order()`, `Appended`), since the running hash folds events, messages and stats rows in as they happen. `journal::Record::{new(heads, host, chron), of(&mut st, &mut chron)}` appends and counts in one step (`event`, `message`, `stats`, `thought`, `action`, `frame`, and `append` for a `JournalEntry`): the game appends through it, and `rebuild` replays through it, so the two cannot drift apart.
- **Journal.** `journal::take_chunk(rules, &chronicle, &mut JournalCursor, &mut HostHeads) -> Option<JournalChunk>` writes `{"format":"citar-journal","version":1,"seq","entries":[{"event":{...}}, ...]}` in append order and counts `journal_seq`; a chunk that does not encode moves neither the cursor nor the count. The game keeps the cursor, which is never saved (`JournalCursor::at_end` after a load). `journal::decode_chunk` reads one chunk back as `JournalEntry`s; `journal::rebuild` reads chunks into a `Chronicle`, recomputing the running hash and every engine and host count, and reports whether they all agree with the state's heads and the chunks came in sequence. The running hash folded rule ids as numbers, so under another ruleset (`rules_changed`) `rebuild(.., same_rules: false)` compares the counts, the sequence and the newest stats row but not the hash.
- **Frames.** `FullFrame::capture(rules, st, event_range)` is Python's frame (owner; improvement and top non-hill feature as palette position + 1; route level + 4 if pillaged; majors' explored tiles; cities; units packed at 8 bytes; the event range), with the improvement, feature and unit type names as its palette (Python's frames named the unit type), so frames outlive a renumbering ruleset. `FrameWriter::push` writes a keyframe first, every 64 frames, and whenever a delta could not say what changed (the tile count, the palette or the majors changed, or a tile was forgotten). A delta names the frame it was taken from (its turn and 8 bytes of a blake3 of its palettes, layers and explored sets). `FrameDecoder::apply` reads records back, and refuses a delta applied to any other frame (a lost chunk) and a position past its palette. The binary layout is in `save::journal`'s module doc; `replay_data` (1d-02) builds on these.
- **Summary.** `save::summary(bytes) -> Summary` reads the format and version first (another version is `LoadError::Version`, whatever its shape; an older one goes through `migrate::upgrade`), then only the parts it needs, without a ruleset, and keeps `state_summary`'s keys; an editor map's type is `custom`. `crates/citar-testkit/data/summary_duel.json` is its fixture.
- **Carried over from 1a-05b.** `state::players` re-exports `SpyAction` and `ReligionProgress` from `rules::defs` instead of keeping copies, and `ReligionState::free_beliefs` is `[u8; BeliefKind::COUNT]`, indexed by `BeliefKind::index` (§5.6).

### 4.12 The Python converter (`compat::python`, feature `legacy`, test-only)

- **The entry point.** `state_from_python(json, rules) -> Result<Converted { state, chronicle, report }, ConvertError>`. It is strict:
  - any unresolved name, unknown key, unknown event type or NaN fails with its JSON path;
  - there is no lenient mode.
- **Who uses it.** Refcheck, testkit and bench. It never ships (§1.2).
- **Mirror types.** Permissive `Py*` mirror structs parse the JSON: tiles as 14-tuples in `Tile._ORDER` (`state.py:84-85`), and flags as `Value`.
- **Name resolution** tries an exact match first, then the ruleset's case-insensitive resolver.
- **What is kept:** tile, player, unit and city ids, and `turn_started`.
- **What is dropped:** `rng_state` (MT19937 state; `seed` comes from the config instead), every dead field listed in §4.4-4.6, and the list orders of set-like fields. Each dropped item is listed in `ConvertReport`.
- **Fields Python lacks** are set as follows:
  - `citizens_settled = false`;
  - `last_gold_rate = 0`, which is Python's effective value;
  - `combat_seq = 0`;
  - `driver = None`;
  - `happiness_seen` is committed once from 0. That is the value Python's conditionals see after a cold load: `happiness()` sees 0 for its own conditionals while it computes (economy.py:404-433), and every later read sees that result.
- **Settling.** `Game::from_python(rules, json) -> Result<(Game, ConvertReport)>` converts, builds `Derived`, then runs one settle. That settle is the counterpart of Python's refresh on load (`scripts/refcheck/common.py:375-386`). Its changes are reported, which is what the `fixed_point` refcheck group checks.
- **Decision.** The feature is called `legacy`. The earlier draft shipped a lenient mode in citar-py for Python-format scenarios, which contradicts plan G (archive everything). No scenario ships, and the only one on the laptop is already archived. It was dropped. If one is ever needed, a one-off offline conversion command in `citar-sim` can be added.

**As built in 1a-10** (§4.12, §9.2, §9.6):
- **Files.** `compat/python/{mod, read, names, config, players, entities, diplo, world, history}.rs`, with unit tests in `compat/python/tests.rs`. The gates' tests are in `crates/citar-testkit/tests/engine/convert.rs`, which reads the fixtures through `citar_testkit::fixtures`; the corpus joins them when `CITAR_REFCHECK_CORPUS` names its folder.
- **A reader instead of mirror structs.** The JSON is parsed once into a `serde_json::Value` and read through a path-tracking reader (`read::Obj`), which takes each field by name and refuses, at `finish`, any key nobody took. Serde mirror structs with `deny_unknown_fields` name an unknown key but not the place of a wrong value deeper down, and placing every error (`players[3].flags.pairs["5"].wary`) would have needed a dependency off the allow-list; reading and mapping are also one pass instead of two. A JSON parse error (Python writes `NaN` and `Infinity`) is placed by scanning the text up to the byte serde stopped at. Python's typing is read as loosely as Python wrote it: a whole number written as a float (`50.0`) is accepted, and a missing dataclass field takes its default, as `from_dict` gave it.
- **Entry point.** `state_from_python(json, rules) -> Result<Converted { state, chronicle, report }, ConvertError { path, message }>`. The converted state passes `State::from_parts` and `save::validate` before it is returned, so everything that converts saves and loads.
- **Report.** `ConvertReport` counts what was dropped by `Dropped` kind (22 of them), each where Python held something there (a unit's `build` that is not `None`, not every unit): the dead fields of §4.4-4.6 and `rng_state`; the barbarians' explored tiles and the city-states' memories, which nothing read; explorer entries of units that are gone or no longer the player's; free buildings of cities the player no longer holds; a negotiation's `exchanges` that is not its history's length; a quest's `data2`; and, as one kind, every list that became a set whose order was not ascending. Over the 262 fixture states that is `rng_state`, `last_stats`, gone explorers, barbarians' explored tiles, city-state memories, two free-building entries and 76,997 list orders.
- **Choices the design left open.**
  - Python did not record how events, messages, thoughts and stats rows interleaved. They are appended turn by turn, within a turn events, then messages, thoughts and stats rows, each list in its own order, and the engine heads are hashed in that order. Event ids must be their positions from 1 (`game.py:876`) and are handed out by the feed as they are appended, so the host heads go on from them; message ids are kept.
  - The settings keys the engine does not read (`on_disconnect`, `reconnect_seconds`, the lobby's `players`, anything newer) go to `GameConfig::host` verbatim, as `config_from_json` keeps them. Everywhere else an unknown key is an error.
  - A city whose pressures nothing had read (`{}`) is seeded with 100 toward no religion, what Python's first read made (`religion.py:105-109`), since the state now seeds a city at founding.
  - An emptied build queue (`[]`) is no queue; quest countdowns of -1 are left out (`QuestTimers.individual` is sparse); a city-state's influence and pairs have one entry per player, however many keys Python held, as every state keeps them.
  - `Unit::with_carrier` and `CityStateData::with_ally` set the two private links on parts not yet in a state. They are `pub(crate)` and compiled only with `legacy`, since only the converter calls them (a save sets both as it loads); in play the links change only through `Units::board` and `State::set_ally`, which report the change.
  - Message ids, like event ids, must be their positions from 1 (`diplomacy.py:308`): no counter holds the next message id, so the engine numbers new messages from the heads' count, which any other numbering would collide with. A scenario's opinion key `"a>b"` must name its relation's own pair, and a reason set both there and in the holder's own entry is refused, so the result does not depend on key order.
  - Python's host event types (`agent_error`, `game_paused`, `game_resumed`) become `EventType::Host`, counted only in the host heads. Any other type the engine does not emit is an error.
- **Refcheck.** Each fixture is converted once in `run::check_fixture` and handed to every group as `Ctx::converted` (from 1b-01, loaded once with `Game::from_python` and handed over as `Ctx::game`); a state that does not convert is a load failure, and the report lists the run's drops as information. The `state_echo` module projects the fixture's state in Python's vocabulary (the settings with the map edges, resource and diplomacy options and host keys, the id counter, each tile's continent, tiles, players with their seats' overrides, flags, explored tiles by index and each major's memory of every tile, units, cities, relations, opinions, deals, negotiations, religions, the UN, camps, events with their name references in code points, messages, thoughts and stats rows) and builds the same projection from the converted state's public reads and the ruleset's names. It leaves out what the report drops and reads the lists that became sets as multisets. It is clean on all 262 states, enforced, and at 0 in `ratchet.json`. A conversion that panics is a load failure too, not the end of the run (`run::guarded`, which `Game::from_python` must keep).
- **Golden set `convert`** (`golden/convert.json`): for each committed fixture, the digest after conversion, its canonical length and the count dropped. The determinism workflow also runs when the committed fixtures change.
- **Never shipped.** `cargo xtask check` (its `features` check) refuses a crate other than refcheck, testkit and bench that turns on `legacy` or depends on one of them, and an engine whose default features include it. It counts every engine feature that implies `legacy` as test-only too, and reads a member's own feature table (`x = ["citar-engine/legacy"]`, `citar-engine?/…`, or under a rename) as well as its dependency's feature list.

---

## 5. Ruleset and uniques

### 5.1 What the shipped ruleset holds

These are the rules area's census figures, taken with the Python parser.

**Uniques:**
- 1,615 unique texts, 954 of them distinct;
- 342 distinct main types in use;
- 60 modifier types: 49 conditionals, 3 trigger conditions, 6 unit-action modifiers and 2 meta modifiers (game speed, timed);
- one unknown text, `"Aircraft"`, used 3 times.

**Python's coverage** (as counted for 1a-05b, §5.6). Python names 405 of UnCiv's 637 types as `U.<name>`. It reads 89 more as live `_COND` keys and 12 more in its `_META` set (`uniques.py:896-1019`), which makes 506. The other six of its 95 `_COND` keys are its own wordings, which no UnCiv type has, so they can never match.

The engine supports 527 types, of which the shipped ruleset uses 402 and 125 are extra. The 527 are:
- Python's 506;
- the UnCiv wordings of five of the six dead keys (the sixth's wording was already among the 506);
- 16 types the shipped ruleset uses that Python never named by type. Two of these Python read by their text (`game.py:30`, `bots/basic.py:1386`), and 14 nothing reads (the inert types).

These figures replace the design-time census of 509 handled and 121 unused.

**Files.** 22 files in `citar/data/ruleset/` (excluding `NOTICE.md`), plus `custom/nations.json` and `game.json`: 24 files in all (`rules.py:37-99`). `citar/data/collectors/` holds hardware scripts and is not ruleset data.

**Table sizes:**

| Table | Size | Table | Size |
|---|---|---|---|
| techs | 80 | resources | 35 |
| base units | 127 | improvements | 35 |
| unit types | 28 | beliefs | 56 |
| buildings | 124 | policies | 10 branches + 60 |
| promotions | 106 | nations | 82 + BenchmarkCiv |
| terrains | 33: 9 base, 10 features, 14 natural wonders | eras | 9 |

### 5.2 How the ruleset is delivered

```rust
pub struct RulesetFiles<'a> { pub files: Vec<(&'a str, &'a [u8])> }   // "ruleset/techs.json", "custom/nations.json", "game.json"
impl Ruleset {
    pub fn load(files: &RulesetFiles<'_>) -> Result<Ruleset, RulesetErrors>;
    pub fn leak(files: &RulesetFiles<'_>) -> Result<&'static Ruleset, RulesetErrors>;   // interned by RulesetId
    #[cfg(feature = "embedded-ruleset")] pub fn shared() -> &'static Ruleset;           // OnceLock over embedded()
    pub fn id(&self) -> RulesetId;                                                        // 32-byte blake3
    pub fn version(&self) -> String;                                                      // "2-<first 12 hex of id>"
}
pub const BUILD_ID: &str = match option_env!("CITAR_BUILD_ID") { Some(s) => s, None => "dev" };
```

- **Embedding.** `rules::source::embedded()` has one `include_bytes!` per file, relative to `CARGO_MANIFEST_DIR/../../citar/data/`. `.gitattributes` already pins `*.json` to LF.
- **Decision (`include_bytes!`).** The workspace area's `include_bytes!` wins over `include_str!`, feeding `RulesetFiles`.
- **Decision (`RulesetId`).** blake3 over a canonical binary walk of each parsed `serde_json::Value`:
  - files in sorted name order;
  - object keys in document order;
  - numbers tagged as u64, i64 or f64 bits;
  - strings with a length prefix.

  It is immune to CRLF checkouts, reformatting and changes in serde_json's number formatting. Hashing JSON text was rejected because serde_json 1.0.151 prints `1e-5` as `0.00001`. `rules_version()` returns `Ruleset::version()`, and proof of work signs `BUILD_ID` plus `RulesetId`.
- **The Phase 2 move.** At the swap, `citar/data/{ruleset,custom,game.json}` moves into `crates/citar-engine/data/`. By then Rust is the only reader, and the move makes the maturin sdist self-contained.
- **Runtime modding.** Modders can pass other bytes: citar-py will accept an override directory. Proof-of-work jobs always use the embedded ruleset.

### 5.3 Typed tables

**The raw layer** (`raw.rs`) mirrors the JSON:
- serde with `deny_unknown_fields`, so a misspelt field is an error;
- `IndexMap` for tables keyed by name, keeping JSON order;
- keys that start with `_` are skipped (`rules.py:78-81`);
- custom nations are merged as Python merges them.

**Typed definitions** (`defs.rs`), each table an `IdVec` in JSON order:
- `TechDef`, `EraDef`, `BaseUnitDef`, `UnitTypeDef`, `BuildingDef`, `PromotionDef`, `TerrainDef`, `ResourceDef`, `ImprovementDef`;
- `PolicyDef { kind: Branch | Member { branch, requires, finisher } }`;
- `BeliefDef`, `NationDef`, `SpecialistDef`, `SpeedDef` (with its year table), `DifficultyDef`;
- `VictoryDef` (milestones compiled to `enum Milestone`), `QuestDef` (with its `QuestTarget` kind), `RuinDef`, `PersonalityDef`;
- `CityStateTypeDef` (with `friend` and `ally` `SourceUniques`);
- `Constants`, the 32 typed formula constants from `game.json`, plus map sizes, map types, barbarian levels and diplomacy settings;
- `fracs: Vec<f64>`, the interned fractional unique parameters (§5.5).

**Derived at load:**
- `tech_order` (by column, then name as UTF-8 bytes, which is code-point order as in Python);
- the unlocks per tech; `upgrade_from`;
- unique units, buildings and improvements per nation;
- major and city-state nations (excluding `WillNotBeChosenForNewGames`);
- great-person units, spaceship parts, `max_turns`;
- `stat_related` (`rules.py:169-180`), unit classes and flags;
- the feature layer order, `move_scale`, the builder class of each unit, and `feature_removals` precomputed.

**Validation** replaces `rules.py:238-260`. It checks every cross-reference, duplicate names, the speed tables, set capacities, and every unique and filter error. All problems are collected into one `RulesetErrors` report, each with file, object and text.

**As built in 1a-03:**
- **Strict input.** A JSON object with the same key twice is an error; Python and serde's `Value` both kept the last one silently. Every object's name must equal its key.
- **Consistent input.** A branch's `members` are exactly its policies other than the finisher, and each of them names that branch. The `game.json` values later code divides by, loops over or seats players with are checked: `move_scale` and `unit_upgrade_cost.round_to` are at least 1, the counts, distances and turn numbers are not negative, `map_size_predefined` rises by radius, and `max_players` plus the barbarians fit a `PlayerSet`.
- **Ids that are data.** Eras must be listed by `number` from 0, so `EraId` is the era's number, which rules use as an index (`rules.py:108-111`). `FeatureId` is a feature's layer: Hill first, Fallout last, the rest in file order, so `FeatureSet::top()` is the highest bit. `Derived::features` maps a `FeatureId` to its `TerrainId`.
- **Objects the engine names.** Improvements Python told apart by name (`workers.py:22-26`) get an `ImprovementKind`, and quests (`city_states.py:815-1191`) a `QuestKind`, whose `target()` is the kind of `QuestTarget` the quest holds; a quest the engine does not know is an error. The objects themselves are resolved once into `Derived::known`: Hill, Fallout, Road and Railroad are required; Repair, the order cancel, City center, City ruins, Ancient ruins and Barbarian encampment are optional, as Python treated them. `Known` holds only these so far. Python also names Worker (`units.py:165, 623`, `city_states.py:680`), the Settler fallback (`units.py:164`), the great people the AI prefers (`great_people.py:214`), Palace (`cities.py:2193`), The Wheel (`cities.py:1986`), Prince (`economy.py:47-49`), Ancient era (`game.py:42, 161, 345`, `cities.py:2162`, `city_states.py:50`) and the victories (`game.py:46`, `victory.py`). The package that ports each of those lines adds the object to `Known`, required or optional as Python treated it, instead of comparing names.
- **Read-only tables.** Outside the crate a `Ruleset`'s tables are read through accessors (`techs()`, `base_units()`, `derived()`, ...), so a loaded ruleset cannot drift from its `RulesetId`, name indexes and derived tables; a variant is loaded from edited files. Inside the crate the fields are `pub(crate)`, for the loader and the unique compiler. `speeds()` and `difficulties()` are the tables; the facade's name lists are `speed_names()` and `difficulty_names()`.
- **Typed lookups.** `resolve::<I>(text)`, `lookup::<I>(name)` and `name(id)` take the table from the id type through the `Named` trait, so a unit's position cannot come back as a `TechId`. The facade's string-keyed `resolve_name(kind, text)` returns the name.
- **Before the compiler.** Until 1a-05, the derived tables that depend on a unique's type (great people, spaceship parts, rough terrain, great improvements, major nations, `stat_related`, builder classes) read placeholders through `unique::text`, which ports `split_modifiers`, `placeholder` and `parse_stats` (`uniques.py:28-90`). The compiler replaces these reads.
- **The set widths** are constants in `base::sets` (`TECH_WORDS` and so on), and a table over its width names the constant to raise.

**Facade reads:**
- `client_json()` is built once from the preserved raw values plus the derived lists, in the shape of `to_client` (`rules.py:303-331`);
- `resolve(kind, text)` uses sorted per-table slices after normalising the text (`rules.py:28-30, 263-270`);
- also `counts()`, `max_players()`, `map_sizes()`, `speeds()` and `difficulties()`.

### 5.4 Generating `UniqueType`

- **`unique_types.tsv`** is vendored: 637 UnCiv types with name, placeholder and signature, under an MPL-2.0 notice. It is converted once from `unique_types.py`, and a Python `--from-unciv` mode refreshes it from `UniqueType.kt`.
- **`unique_supported.toml`** says, for each used type:
  - its role: effect, flag, requirement, one_time, action, mapgen, ai, inert (with a reason), cond, trigger, action_mod or meta;
  - its field names and kind overrides;
  - its derive stages.
- **`cargo xtask gen-uniques`** writes `src/unique/gen.rs`:
  - `#[repr(u16)] enum UniqueType` (637 variants);
  - `TYPE_INFO`, `BY_PLACEHOLDER` (sorted) and `ParamKind`;
  - the payload structs in `mod p`, the `UniqueData`, `CondData`, `TriggerCond` and `OneTimeEffect` shapes, and the `Payload` impls.

  The output is committed, and `xtask check` regenerates it and fails on any diff.
- **Decision (generator in Rust).** The generator lives in `xtask`, so the Rust build does not depend on Python once the Python engine is archived.
- **Decision (support scope).** Every unique type and conditional the Python engine handled gets a payload, not only those the shipped ruleset uses. The owner settled open question 2 this way on 2026-09-23: the goal is every Civilization ruleset from 1 to 7, and custom ones. The first draft supported only the used types, which departed from plan §3 and from `docs/MODDING.md`'s "a unique the engine already knows needs no code". A placeholder of any other UnCiv type is a load error saying what to add: one TOML line, one evaluation arm, one test.

### 5.5 The compiler

The compiler ports the bracket-aware `split_modifiers` and `placeholder` functions (`uniques.py:28-76`). Then, for each source object in canonical order, and each text in JSON order, it takes these steps:

1. **Look up the placeholder.** Unknown text with no parameters and no modifiers is a tag candidate (§5.6). Anything else unknown is `UnknownUnique`.
2. **Check the type is supported,** else `UnsupportedUnique`.
3. **Compile each parameter by its `ParamKind`.** There are about 45 small compilers:
   - `stats` become an interned `StatsId` (44 distinct), and an unknown stat is an error;
   - amounts are range-checked `i32`s;
   - fractions are parsed with `str::parse` (correctly rounded) and interned as `FracId(u16)` into `Ruleset.fracs`;
   - names become ids, and filters become filter ids or `SetRef`s;
   - countables become `Countable`;
   - small vocabularies become enums;
   - union kinds compile per domain into an `ObjectFilter`.
4. **Apply per-type fixups.** For example, `UnitStartingPromotions [relevant]` becomes the base-unit set whose unit type is in the promotion's `unitTypes` (`units.py:153`).
5. **Fold the modifiers by role:**
   - `cond` becomes a `Cond`;
   - `trigger` becomes a `TriggerCond`, and more than one is an error;
   - `action_mod` becomes `ActionMods`;
   - `ModifiedByGameSpeed` becomes a flag;
   - `for [n] turns` becomes `timed`;
   - anything else, including `for every []`, which Python stubs to 1 (`uniques.py:1081-1083`), is an error.
6. **Mark the unique `LOCAL`** when a city filter or conditional is `in this city` (`uniques.py:130`).
7. **For timed uniques, compile a temporary variant** without the timer. This replaces `economy.temp_unique`'s mutation after construction (`economy.py:64-75`).

**The record, split into a hot part and a cold part:**

```rust
pub struct Unique { pub data: UniqueData, pub conds: CondSpan, pub deps: CondDeps /* u32; top 8 bits hold UFlags */ }
const _: () = assert!(core::mem::size_of::<Unique>() <= 32);   // 24 as laid out
pub struct UniqueMeta { pub ty: UniqueType, pub role: Role, pub source: Source, pub text: TextId,
                        pub key: u64 /* FNV-1a-64 of (source kind, source name, occurrence, text): RNG key and save identity */,
                        pub timed: Option<u16>, pub temp_variant: Option<UniqueId>, pub trigger: Option<TriggerCond>,
                        pub actions: ActionMods, pub ability: Option<AbilityKey> }
pub struct SourceUniques { pub all: Range<u16>, pub civ: Box<[UniqueId]>, pub local: Box<[UniqueId]>,
                           pub on_gain: Box<[UniqueId]>, pub triggered: Box<[UniqueId]>, pub actions: Box<[UniqueId]>,
                           pub ai: Box<[UniqueId]>, pub tags: TagSet, pub cond_tags: TagSet }
```

- **The layout.** Payloads are at most 12 bytes and 4-byte aligned: they hold ids (`StatsId`, `FracId`, filter ids, `SetRef`) and `i32` amounts, never an `f64`, an inline `Stats` or a bitset.
  - `UniqueData` is therefore 16 bytes: the payload plus a `u16` tag.
  - `Unique` is 16 + 4 (`CondSpan`: u16 start, u16 length) + 4 (`CondDeps`) = 24 bytes.
  - The earlier draft's `f64` fractions and separate `UFlags` byte made it 40 bytes, over its own 32-byte assert.
  - Only `TileGenerationConditions` is boxed.
- **`UniqueMeta.key`** is stable under unrelated ruleset edits, and distinct for the same text on two sources (§7.2).
- **`UniqueId`s follow Python's `civ_umaps` source order:** Nation, Building, Policy, Tech, Temporary, Era, city-state bonuses, Belief, Resource, Global, then Terrain, Improvement, UnitType, Unit, Promotion, Ruins. Sorting by id is therefore canonical.
- **Base units** inherit their unit type's uniques and tags without copying the uniques.

### 5.6 Tags, and `"Aircraft"`

The rule for unknown texts:
- An unknown text becomes a `Tag(TagId)` only if it has no parameters, no modifiers, and appears as a term in at least one filter.
- An unknown text that nothing references stays an error, because it is a typo.

`Aircraft` qualifies. `units.json:1451` and `promotions.json:675-701` filter on `[Aircraft]`, which is UnCiv's own convention. It cannot be replaced by `Air`, because `Air` also covers missiles.

**As built in 1a-05** (§5.4-5.6):
- **Files.** `unique_types.tsv` (637 rows: name, placeholder, signature) and `unique_supported.toml` sit beside `Cargo.toml` in `crates/citar-engine/`. `scripts/gen_unique_types.py --tsv` refreshes the TSV from UnCiv. `cargo xtask gen-uniques` writes `src/unique/gen.rs`, and `xtask check` regenerates it and fails on any difference. `gen` is a reserved word in edition 2024, so the module is `unique::generated`, loaded from `gen.rs` by `#[path]`. rustfmt skips it.
- **The supported list** had 423 entries at 1a-05 (1a-05b raised it to 527, below): the 402 types the ruleset uses, plus the 21 triggers Python fires that the ruleset does not use. Python fires 24 kinds in all; three of them are used. The roles break down as 164 effects, 92 flags, 10 requirements, 27 one-time effects, 11 actions, 1 AI weight, 23 map-generation types, 14 inert types (each with a reason), 49 conditionals, 24 triggers, 6 action modifiers and 2 meta modifiers.
  - `stages` lists the engine systems that read a type: the Python modules that read it, mapped to the game modules of §3.2. An inert type is one Python never reads. The generator refuses a type that is not inert and names no stage, since the packages find their work by stage.
  - `gain = true` marks the five standing effects that `triggers.py`'s TRIGGERABLE also fires once when their source is gained, such as free buildings and free promotions.
  - `name:kind` overrides a field's kind. `amount16` keeps `BuyUnitsIncreasingCost`'s payload within 12 bytes; from the 1a-05b fix round, `nonNegativeAmount` refuses a negative landmass count (below).
- **Parameters** have 49 `ParamKind`s. Amounts are `i32` within ±1,000,000, with the positive and non-negative kinds checked; `+15` reads as 15, and a non-integer is an error. There are 44 distinct `StatsId`s and 19 `FracId`s.
  - Names become ids, looked up exactly. `[greatPerson]` is checked only as a unit, because whether a unit is a great person is known only after this pass.
  - Filters are handles to their text. `UnitFilterId`, `TileFilterId` (which also serves terrain filters and `simpleTerrain`), `CityFilterId`, `CivFilterId` and `CombatantFilterId` each index a table of texts. Static filters (base unit, building, improvement, resource, tech, era) are `SetRef`s into `StaticFilter { domain, text, members }`, and 1a-06 fills in the members. The union kinds (`tileFilter/buildingFilter` and the like) are an `ObjectFilterId`, compiled once per allowed kind.
  - Countables are parsed here (`unique::countable`); 1a-07 evaluates them.
- **The record.** `Unique` is 24 bytes and `UniqueData` 16. The top byte of `Unique`'s `CondDeps` word holds `UFlags`: LOCAL, SPEED, TIMED, TRIGGERED, ACTION and TEMPORARY. Every conditional reads `CondDeps::all()` until 1a-07 assigns its classes.
  - `UniqueMeta.ty` is `None` for a tag.
  - `occurrence` counts the identical texts before this one on the same source.
  - The key is FNV-1a-64 over the length-prefixed source kind name, source name, occurrence (as `u16`) and text. The same text on two sources, or twice on one, gets distinct keys, and adding a unique moves no other key. A temporary variant hashes the kind `Temporary`, the name `<kind>/<name>` of its original, and the original's occurrence and text.
  - Limited actions carry an `AbilityKey` interned from Python's `ph|params` key (`units.py:417`).
- **Temporary variants** share their original's data and conditionals, and drop both the timer and any trigger. They sit in one block after the techs' uniques. The city-state bonuses are laid out as every type's friend bonuses, then every type's ally bonuses, then the types' own uniques, so that `Source`'s order is id order.
- **Partitions** (`SourceUniques`):
  - a triggered unique goes to `triggered`;
  - a timed one goes to `on_gain`;
  - effects, flags and tags go to `civ`, and also to `on_gain` with `gain`. A building's or a resource's that are LOCAL go to `local` instead: Python split only buildings' uniques (`economy.py:108`, `cities.py:59`), and the resource case is the Marble decision of §5.12. On every other source (beliefs, policies, nations and the rest) `in this city` is the city in context, as `in all cities` is (`uniques.py:668`), so a LOCAL unique there stays in `civ` and keeps its LOCAL bit for evaluation. `local` is empty for every source but buildings and resources;
  - one-time effects go to `on_gain`, or to `actions` when they carry action modifiers;
  - actions go to `actions`, and AI weights to `ai`.
- **Tags.** A tag is any placeholder without parameters that a filter names as a term, whether known (`Rough terrain`, `Great Improvement`, `Spaceship part`, `Fresh water`) or unknown (`Aircraft`). It is split into terms as `multi_filter` splits it. The filters inside uniques count, and so do the ruleset's other filters (`terrainsCanBeBuiltOn`, start biases). An unknown text is named by its trimmed placeholder, as `has_tag` read `u.ph`, so `"Aircraft "` is the Aircraft tag. A source's `tags` hold its unconditional tags and `cond_tags` those under conditionals; a base unit's also hold its unit type's, as §5.5 says, so the Fighter unit carries `Aircraft` while the `Aircraft` unique stays on the Fighter unit type. The shipped ruleset has 5 tags. Tags are judged even when other texts fail, so one report holds every problem; the bracketed terms of a failed text count as named, so that its failure does not also report its tag as a typo.
- **Errors** are four new `RulesetErrorKind`s:
  - `UnknownUnique`: no UnCiv type, or an unknown text no filter names;
  - `UnsupportedUnique`: not in `unique_supported.toml`, and the message says what to add;
  - `UniqueParameter`: a parameter that does not compile;
  - `UniqueModifier`: two triggers, a `for every` multiplier, a modifier used twice, a unique and a modifier in each other's place, or a modifier the unique's role has no use for. A trigger goes only on a one-time effect, a `gain` effect or a timed effect (Python stood a triggered standing effect from the start, since triggers do not filter, `uniques.py:790-791`); action modifiers only on an action or a one-time effect; a timer only on an effect or a flag (Python stored a timed unique for its turns instead of applying it, `triggers.py:88-92`, so a timed one-time effect never happened).
  - The existing `Capacity` kind covers the limits: at most 65,535 uniques and 65,535 conditionals (one past the last id must still fit a `u16`), and at most as many tags as a `TagSet` holds.
- **The loader** numbers the feature layers first, because uniques name features by them. Then it compiles the uniques, then derives the tables. The derived tables (rough, great improvements, great people, spaceship parts, major nations, `stat_related`) read compiled uniques by type. `Derived::builder_classes` now holds `ObjectFilterId`s. Loading takes about 7 ms in release.
- **Checks.** The golden `uniques.json` snapshots every compiled unique, and every source's `all` range, partitions and tags. The refcheck `uniques` group compares each text with `scripts/refcheck/uniques_dump.py`'s record of how Python read it, and runs enforced with 0 unexplained differences.
- **Left for 1a-07.** The `OneTimeEffect` shapes; `UniqueData`'s one-time payloads are their input.

**As built in 1a-05b** (§5.4, full support; the owner's decision of 2026-09-23):
- **The supported list** has 527 entries: the 402 types the shipped ruleset uses, and 125 more. Each of the 125 is marked `# (extra)` in `unique_supported.toml`:
  - 55 main types: 28 effects, 11 flags, 1 requirement, 12 one-time effects, 2 actions and 1 map-generation type;
  - 45 conditionals;
  - 22 triggers: the 21 Python fired, and `upon being defeated`, which Python let through and never fired;
  - 3 display modifiers.

  The roles now break down as 192 effects, 103 flags, 11 requirements, 39 one-time effects, 13 actions, 1 AI weight, 24 map-generation types, 14 inert types, 94 conditionals, 25 triggers, 6 action modifiers and 5 meta modifiers. UnCiv's other 110 types still do not load.
- **Python's six dead `_COND` keys** (§5.12) are its own wordings of UnCiv conditionals, and no UnCiv ruleset writes them. Their UnCiv wordings are supported instead:
  - `outside a Golden Age` is `when not in a Golden Age`;
  - `in tiles adjacent to []` and `in tiles not adjacent to []` are `in tiles adjacent to [] tiles` and `in tiles not adjacent to [] tiles`;
  - `for [] players` is `for [] Civilizations`;
  - `if no other Civilization has adopted []` is `if no Civilization has adopted []`;
  - `when [] units` is `for [] units`, which the shipped ruleset already uses.
- **Parameters** have 54 `ParamKind`s. Three are new:
  - `speed` is a `SpeedId`;
  - `beliefType` is a `BeliefKind`: a belief type, or `Any`, which is how `religion.py:544-575` counted a civilization's choices;
  - `spyAction` is a `SpyAction`, named as Python named it in any case (`espionage.py:101`) or as UnCiv does.

  Game state shares these two vocabularies, so they live in `rules::defs` beside `BeliefType`, not in `unique::params`. A spy's current action is the same `SpyAction` a unique names (`from_name` reads saves exactly; `from_ruleset_text` reads rulesets leniently). `BeliefKind::index` gives each kind a slot, `Any` included, so a civilization's free beliefs are a `[u8; BeliefKind::COUNT]`: `Gain a free [Any] belief` has somewhere to go.

  `pediaLink` and `validationWarning` are kept as text.
- **Display modifiers.** `<hidden from users>`, `<Civilopedia link []>` and `<Suppress warning []>` change how UnCiv shows a unique, and no rule. Python passed over them (`uniques.py:1013-1019`), and the compiler folds them into nothing.
- **Map generation** reads `Must be on [n] largest landmasses` into `NaturalWonderGen::on_largest` (`mapgen.py:734-736`). Both landmass counts are `nonNegativeAmount`s: Python's `continents_by_size[:n]` counted a negative n from the end, and a Rust slice of that many landmasses would panic. A count may still exceed the number of landmasses, so 1b-04 takes `min(n, len)` of them.
- **Hurrying a wonder.** `actions.py:87` offered `hurry_construction` to a unit with `Can speed up the construction of a wonder`, and `great_people.py:313` then refused it, since it checked only `Can speed up construction of a building`. The wonder type's stages name `great_people` too, so 1b-08 ports the fix: either type may hurry, and the wonder type only when the city is building a wonder.
- **The kitchen sink.** `crates/citar-testkit/testdata/rulesets/kitchen_sink/` is a small mod over the shipped files, written as JSON merge patches (RFC 7396). It adds:
  - a nation, two buildings, seven units, a promotion, an improvement and a natural wonder;
  - a belief, a city-state type and four ruins.

  Between them these objects use every extra type, each where a ruleset would put it. `citar_testkit::rulesets::kitchen_sink()` loads the result. Its tests check three things:
  - every text compiles on its object;
  - the shipped ruleset and the kitchen sink together use every supported type;
  - the `(extra)` marks name exactly the types the shipped ruleset does not use.
- **Who implements them.** The extras compile, and nothing reads them yet. 1a-07 evaluates every conditional. Each 1b and 1c package implements the extra effects and triggers whose `stages` name its systems, and tests them with the kitchen sink.

### 5.7 Filters

- **The grammar** is Python's (`uniques.py:294-327`): `{a} {b}` is a conjunction split at depth 0, and `non-[x]` is a negation. Parsed filters become `Expr<L> = Const | Leaf | Not | All | Any`, deduplicated by (domain, text). Trees are depth-capped at load.
- **Static domains become bitsets.** These are base unit, building, terrain, improvement, resource, tech, era, policy and promotion.
  - At load, each term is evaluated against every row, with a port of Python's single-term predicate, giving a bitset.
  - `All`, `Any` and `Not` then combine bitsets.
  - A lookup at runtime is one bit test.

  This retires the 9.7 million `multi_filter` calls and the global `_filter_cache` (`rules.py:127,147`).
- **Dynamic domains become pruned trees** over `UnitLeaf`, `TileLeaf`, `CityLeaf` and `CivLeaf` enums. Each term becomes the disjunction of the branches of Python's if-chain that could ever match, and is then constant-folded. A term that matches nothing is a load error. `CivLeaf::{HumanPlayer, AiPlayer}` read the seat's handicap and are tagged `CondDeps::SEAT`.
- **Map-generation filters** may use only terrain-level leaves, and are evaluated through `TileFacts`.

**As built in 1a-06** (§5.7):
- **Files.** `unique/filter/{mod, parse, expr, statics, unit, tile, city, civ}.rs`, and `unique/world.rs` with `TileFacts` and `FilterFacts`. The loader compiles the filters in a stage of their own after the derived tables, which the predicates read (`rough`, `any_wonder`, `stat_related`, a unit's role and domain). Loading takes about 8 ms in release.
- **Static domains.** Ten: the nine above, and the nations a civilization filter names. `StaticFilter` is `{ domain, text, members: BitSet, fixed }`, `fixed` marking the relevant-promotion sets the compiler decided; `UniqueTable::in_set(s, id)` is the bit test. It takes a `StaticId`, a sealed trait of the ten domains' id types that names each one's `StaticDomain`, and debug builds check the id is of the filter's domain (as `StaticFilter::contains` does), so that a building set is never asked about a tech. `statics::members(rules, domain, text)` evaluates any text, for tools and tests. Eras take the grammar like every other domain (Python's `era_matches` read one term). A resource's `improvementStats` names are its non-zero stats.
- **Dynamic filters.** `UniqueTable::filters()` holds a tree per handle: unit, tile, city, civilization and combatant filters. Leaves hold their static parts as typed sets inline (`BaseUnitSet`, `TerrainSet`, `NationSet`, ...), so a leaf is one bit test without an indirection. `NationSet` is new, and the loader refuses more than 128 nations (`sets::NATION_WORDS`).
  - A `TileFilter` holds the full form (`tile_matches`), the terrain form (`tile_terrain_matches`), the terrains the text names read as `terrain_matches` reads one terrain, and whether map generation can read it (`terrain_level`).
  - A `CombatantFilter` holds a unit tree and a city tree in which `City` holds (`combat.py:91-96`).
  - Several words are sets of terrains, so that they need nothing but a tile's terrains: `Water`, `Land`, `Featureless`, `Open terrain`, and `Elevated`, which is a terrain map generation raises (`Occurs in chains` or `in groups`: Mountain and Hill). Python compared the names Mountain and Hill there, and map generation the uniques. `Coastal` is land next to the coast.
  - The aliases: the long and short city words (`in capital`, `Capital`), `Wounded` and `wounded units`, `Barbarian(s)`, `City-State(s)`, `Fresh water` and `Fresh Water`, `non-fresh water` as the opposite of fresh water (`tiles.py:259`, not the `non-[x]` form), and map generation's `Rough` for `Rough terrain`.
- **Folding.** `Leaf` says what is exact for a leaf: its constant, and which leaves merge under `All` and `Any`. A unit's base unit, a civilization's nation and a tile's resource are one value, so their sets merge both ways; a tile's terrains, a unit's promotions, a tile's improvement and route, and a city's buildings are several, so they merge under `Any` only. `{Military} {Land}` is one set test.
- **Errors** are the new `RulesetErrorKind::Filter`. A filter a unique reads directly (a parameter, a conditional's, a trigger's, a countable's) must have every term match something; it is reported once, at its first use. So must an improvement's `terrainsCanBeBuiltOn` and a start bias. A filter nested deeper than 16 does not load. A filter is checked in the form its reader reads: `Must be on`, `Must not be on` (`cities.py:1231-1233`) and `[stats] in cities on [terrainFilter] tiles` (`cities.py:386`) read the city's tile by its terrain alone, so `Must be on [Farm]` does not load, though `Must be next to [Farm]` does; map generation's filters are held to the terrain by `rules::gen_tables`. An object filter records its parameter kind (`ObjectFilter.kind`): its tiles are read as tile-terrain filters for `[improvementFilter/terrainFilter]` (`workers.py:201-202`) and as full tile filters otherwise. It drops the kinds its text never selects (a tile tree that folds to `false`, an empty set), and is an error when no kind is left. A kind it still selects despite a term that matches nothing of that kind is an error too: `non-[Temple]` as tiles is every tile, which Python read that way and the text did not mean. A handle interned only for an object filter's other kinds (`[Factory]` as tiles) is not an error.
- **Evaluation** reads `FilterFacts: TileFacts`, answered by the world; 1a-07's `EvalWorld` builds on it. `Filters::{unit_matches, tile_matches, tile_terrain_matches, gen_matches, city_matches, civ_matches, combatant_matches}` take the viewer as Python did: a unit's `UnitScope { this, viewer }`, a city's viewer defaulting to its owner, a combatant unit with nothing in context. `filter::{unit, tile, city, civ, combatant}_filter(rules, text)` compile any text the same way.
- **What a leaf reads** (`Leaf::deps`):
  - the seat, `SEAT`, for `Human player` and `AI player`;
  - the war state, `WAR`, for `Hostile`, enemy land and enemy cities (`in enemy cities`, `in non-enemy foreign cities`); `Known` reads who has met whom, which `WAR` stands for too;
  - `Open Borders` reads the agreement and the turn it ends (`game.py:705-708`): `WAR | TURN`;
  - `Friendly` reads a declared friendship and the turn it ends, or a city-state's influence (`game.py:677-686`); friendly and foreign land read met, open borders and the turn they end, a city-state's influence, and the viewer's own `City-State territory always counts as friendly territory` (`tiles.py:151-165`). No class stands for influence or for a civilization's uniques, so these three read every class (`CondDeps::all()`), which is always correct;
  - `TECHS` for resource visibility; `RELIGION_STATE` for the religion city words.

  The entity in context is the reader's class (`UNIT`, `CITY`, `TILE`, `COMBAT`), which 1a-07 adds where it evaluates the filter. As decided in 1a-07:
  - met, open borders and declared friendship are part of `WAR`, which is the whole diplomatic state (one revision covers it);
  - influence and a civilization's uniques get no class of their own. `Friendly`, friendly land and foreign land read another civilization's state, which no class of the civilization in context can name, so they keep `CondDeps::all()` (1b-06 added `INFLUENCE`: `Friendly` reads `WAR | TURN | INFLUENCE`; the land leaves, which read the viewer's uniques, keep every class);
  - the city leaves that read beyond the city got classes: `Capital` reads `CITY_COUNT`, `Garrisoned` `UNIT_SET`, and `ConnectedToCapital` every class (1b-06: `Capital` reads `CITY`, `ConnectedToCapital` `CONNECTED`). Besides roads, harbours, borders and techs, the connection reads the owner's own `Forests and Jungles are roads` (`cities.py:1985`), and a civilization's uniques have no class.
- **Checks.** `scripts/refcheck/filters.py` records Python's truth table of every filter text the ruleset writes (205) in each of the ten domains into `crates/citar-testkit/data/filters.json`, and the 2,050 Rust sets equal them, as does every static filter of the loaded ruleset. Mock worlds test every dynamic leaf. Proptests keep folding's meaning, on abstract leaves (`tests/props.rs`) and on the engine's own: random pairs of real unit, tile, city and civilization leaves merge exactly (`Leaf::and`, `Leaf::or` and `Leaf::constant` agree with evaluation on random mock worlds), and random trees over them answer the same folded as not (`tests/engine/filters.rs`). The golden `filters.json` snapshots every compiled tree.

### 5.8 Conditionals and `CondDeps`

- **The variants.** `Cond { data: CondData, deps: CondDeps, text: TextId }` has one variant per supported conditional (94: the 49 the shipped ruleset uses and the 45 package 1a-05b added), grouped as game, civ, city, unit, combat and tile.
- **Map-generation only.** `InRegionOfType` and `InRegionExceptOfType` exist only in `GenCond`; on an effect they are a load error. As built in 1a-06, they compile as conditionals and the compiler refuses them (`UniqueModifier`) on any unique but a map-generation or an inert one (the start-quality uniques carry them).
- **Decision (condition scopes).** One `bitflags` `CondDeps` (24 bits, leaving the top 8 bits of the `u32` for `UFlags`; 26 and 6 since package 1b-06 added `CONNECTED` and `INFLUENCE`, as built there):
  - civ-level classes: TURN, HAPPINESS_SEEN, STOCKS, RESOURCES, GOLDEN_AGE, WAR, ERA, TECHS, POLICIES, RESEARCH_QUEUE, RELIGION_STATE, CIV_BUILDINGS, GLOBAL_BUILDINGS, GLOBAL_POLICIES, CITY_COUNT, UNIT_SET, SEAT, CONFIG, CHANCE;
  - context-local classes: CITY, UNIT, TILE, COMBAT.

  The compiler tags each `Cond` and `Countable` with its classes. `Unique.deps` is the OR of its conditions, and an empty set skips evaluation entirely. `Revs::cond(p, deps)` maps each class to the revisions it reads (§6.3).
- **Evaluation.** `applies(u, ctx, w)` is a `match` per condition, ported from the lambdas at `uniques.py:896-1010`.
  - An unknown conditional can no longer fail closed silently (`uniques.py:794`), because it cannot compile.
  - `Chance(p)` rolls `Rng::keyed(seed, Purpose::Chance, &[turn, meta.key, civ.key(), tile.key(), unit.key()])`. The `key()` parts are `KeyPart` encodings, so a missing civ, tile or unit is distinct from id 0.
- **Refusal texts.** `Cond::describe()` reproduces the refusal texts of `cities._not_met` (`cities.py:1169-1193`). This includes the two building-count texts that name the civilization's own equivalent building.
- **Hoisting.** `applies_scoped(u, ctx, w, mask)` lets callers evaluate the civ-level parts once and only the context-local parts per tile.

**As built in 1a-07** (§5.8):
- **Files.** `unique/cond.rs`: `deps_of`, `assign_deps`, `applies`, `applies_scoped`, `in_scope`, `holds` (one arm per variant), `chance_keys`, `Cond::describe`, `equivalent_building`, `Problem` and `ProblemKind`. `applies` takes the unique's id, which holds the key a chance draw needs and the speed flag.
- **Classes are assigned at load.** A conditional's classes depend on its filters' leaves, which compile after the uniques. So the loader assigns them at the end of the filter stage (`cond::assign_deps`) and then recomputes every `Unique.deps`. Each class is defined on `CondDeps`, for `Revs::cond` (1b-01) to map:
  - a conditional that reads nothing but ids in its context reads `CONFIG`, such as `for [Major] Civilizations`, which reads a nation fixed at setup. So `Unique.deps` is empty exactly when a unique has no conditionals;
  - the civilization conditionals read their one class (`TECHS`, `ERA`, `POLICIES` or `RELIGION_STATE` for a policy or a belief, and so on). The difficulty ones read `SEAT | CONFIG`, a chance `CHANCE | TURN | TILE | UNIT`, and `if no Civilization has adopted` `GLOBAL_POLICIES | CITY_COUNT`;
  - a conditional over the civilization's cities or units adds `CITY_COUNT` or `UNIT_SET` to what its filter's leaves read, and a context-local one adds its entity's class;
  - `CITY` is the city a rule means (`Ctx::rel_city`): the city in context, else our side's city in a fight, else the city whose territory the tile in context is, when the civilization in context owns it. Python built such contexts for unworked tiles and for every unit (`tiles.py:284`, `units.py:38`), where `<in [Capital] cities>` asks about the territory's city. So the city conditionals read `CITY | TILE`, and a memo keyed by a tile or a unit that evaluates one validates against that city's revisions too (1b-01);
  - `MAP` (bit 23) is every tile's state as `TILE` describes the one in context. The conditionals that read tiles around the one in context (`with [a] to [b] neighboring`, `within [n] tiles of`, `in tiles [not] adjacent to`) read `TILE | MAP` and their filter's leaves. So the Celts' `<with [1] to [2] neighboring [{unimproved} {Forest}] tiles>` and Polynesia's `<within [2] tiles of a [Moai]>` read neither the turn nor the units, and a memo that evaluates them survives a unit's step;
  - `in cities connected to the capital` reads every class, since the trade network reads the civilization's own uniques (until 1b-06 gave the network its class, `CONNECTED`).

  `CondDeps::LOCAL` and `CondDeps::CIV_LEVEL` name the two halves.
- **Hoisting.** `applies_scoped(id, ctx, w, mask)` evaluates the conditionals that read a class of `mask` and no context-local class outside it. `CIV_LEVEL` and `LOCAL` split every unique's conditionals in two, and the two calls agree with `applies`: a test runs every unique of the kitchen sink in four contexts.
- **Python's edge cases.**
  - With no civilization in context, a conditional about the civilization fails. The exceptions are the ones Python wrote as negations, `before adopting []` and `without []`, and the stat comparisons, which read a stock of 0.
  - Tutorials and water maps never hold.
  - `Ctx::IGNORE`, Python's `ctx is None` and `ignore`, makes every unique apply.
  - The region conditionals, which only map generation reads, answer as `GenCond` does.
- **Python behaviour fixed**, each an entry of `refcheck/intended.toml` with the groups that will show it, cited at its arm as `// refcheck: <id>`:
  - the building conditionals read a building filter (`if [Wonder] is constructed`), where Python compared the text with building names (`building-conditionals-read-a-filter`);
  - `when between [a] and [b] [stat]` scales both bounds by game speed, as the other two comparisons did (`between-stat-scales-by-speed`);
  - `if no Civilization has adopted []` counts beliefs (`no-civ-adopted-counts-beliefs`);
  - `vs [] units` never matches a city (`vs-units-never-matches-a-city`).
- **`for units with []`** and `for units without []` take a promotion or the status `Set Up` (`PromotionOrStatus`, the parameter kind `promotionOrStatus`), as Python read both (`uniques.py:974-975`).
- **Cost.** The set tests are word by word (`StaticFilter::intersects` and `count_in`): a civilization's techs, a city's buildings, every city's buildings for `by anybody`. `within [n] tiles of` walks the rings with an early exit and no vector (`HexGrid::any_within`).
- **Refusal texts.** `Cond::describe(rules, nation)` gives the text, and `uq::requirement_problems` gives the whole text for `Can only be built`. `scripts/refcheck/not_met_dump.py` records Python's `_not_met` into `crates/citar-testkit/data/not_met.json`, and the Rust texts equal it:
  - every requirement of the shipped buildings and units with every conditional forced to fail, for each nation whose text differs (54 rows);
  - the distinct failing requirements of every city of the committed fixtures (24 rows).
- **Checks** (`tests/engine/eval.rs`). The tests use a mock `EvalWorld` over tables, whose indexes `unique::index` builds, and a test nation `Eval Test` over the kitchen sink. It carries `[+1 Gold]` under each of the 92 conditionals an effect may carry, in 103 texts (some in several forms: a belief besides a policy, a resource and happiness besides gold, `Set Up` besides a promotion), and the three stat comparisons `(modified by game speed)`; the two region conditionals come from the shipped start biases. The tests cover:
  - one case per `CondData` variant, true and false, in a `match` without a wildcard, each form through the same case;
  - a table of each conditional's classes, and that the shipped tile-neighbourhood conditionals read neither the turn, nor chance, nor the units;
  - no civilization in context, and `Ctx::IGNORE`;
  - the chance key;
  - the city conditionals in a tile's and a unit's context (the territory's city), a city's health in its own fight, and the speed scaling of the three stat comparisons on Marathon and Quick.

### 5.9 Countables, triggers and one-time effects

- **Countables.** `Countable = Int | Turns | Cities | Units | CompletedBranches | Stat | UnitsMatching | CitiesMatching | RemainingCivs | BuildingsMatching` (`uniques.py:738-772`). `BuildingsMatching` sums the civ's per-building counts.
- **Triggers.** `TriggerKind` has one kind per supported trigger (25 from package 1a-05b on), each fired at one site in the engine (turn start and end, research, entering an era, declaring war, expending a unit, and so on).
  - `TriggerCond` is matched against a typed `TriggerEvent`.
  - `fire(w, site, &event, include_unit) -> SmallVec<[UniqueId; 4]>` reads the civ index, then the city's local index, then the unit's profile, which is Python's order (`triggers.py:38-65`). A kind with nothing registered is an empty slice.
- **One-time effects.** Effects decode to a fully resolved `OneTimeEffect` enum, with a kind for each of the 39 one-time types (12 of them added by package 1a-05b) and for the standing effects that also happen on gain, such as `FreeUnits`, `FreeTechs`, `GainStat`, `RevealTiles`, `FreeBuilding` and `Timed`.
  - They are applied in a second phase by `game::triggers::apply_one_time(g: &mut Game, id, site)`, which ports `triggers.py:75-367`.
  - Each trigger's RNG is keyed `Purpose::Trigger, [meta.key, civ, tile.key(), turn]`.

**As built in 1a-07** (§5.9):
- **Countables.** `Countable::eval(w, ctx) -> Option<i64>` counts one. A count of a civilization's things with no civilization in context is `None`, Python's `None`, which fails the comparison. A stock is truncated as `int()` truncated it, and `Food` in a city is its stored food. `Countable::deps(filters)` gives what the count reads.
- **Triggers** (`unique/trigger.rs`):
  - `TriggerKind` has 25 kinds, and `ty()` gives each one's trigger type. `TriggerEvent` is what happened, holding what the trigger's parameter is compared with. `TriggerCond::kind` and `TriggerCond::matches(event, civ, w)` compare them. `TriggerSite` is where it happened.
  - The indexes hold a triggered unique at its trigger's type (§5.12), so `fire` reads one run per index: the civilization's (with the resource layer), the city's local index and its majority religion's follower index (Python's `local_umaps` held both), and, with `include_unit`, the unit's profile. A unique fires once per copy.
  - `upon gaining a [unit]` reads its filter wherever it fires; `great_people.py:140` fired it for every great person. `upon being defeated` is a kind like the others, which combat fires for the unit that dies (package 1c-03). Both are rule differences no refcheck group can show, since the groups ask questions of a standing state and triggers fire only while a turn is played, so they are recorded here and not in `intended.toml`.
  - A unit that is going away is in its event as `UnitFacts` (owner, base unit, promotions, wounded, embarked, set up), taken before it is removed: `LosingUnit`, `DefeatingUnit` and `ExpendingUnit`. `Filters::unit_facts_match` reads them, as `unit_matches` reads a unit with nothing in context, so a site may fire after the removal, as Python's did (`combat.py:602-608, 831`).
- **One-time effects.** `OneTimeEffect::decode(rules, id)` gives 31 kinds:
  - one per one-time type, merged where two types differ only in a count or a placement: `FreeUnits` for the three free-unit types, and `FreePolicies`, `FreeTechs`, `GoldenAge`, `GainStat` (a fixed amount or a range) and `Adopt` (a policy or a belief) for two each;
  - `Unit(UnitEffect)` for the nine `[This Unit]` types. Losing movement is a negative `Movement`;
  - the five `gain` effects: `FreeBuilding`, `FreeStatBuildings`, `FreeSpecificBuildings`, `PromoteUnits` and `CityStateGreatPersonGift`;
  - `Timed { variant, turns }` for any timed unique.

  `[in this city]` on a one-time effect is `CityScope::ThisCity`, as Python's `_cities_for` read it, though as a city filter it selects every city. The table keeps its handle for this (`UniqueTable::is_this_city`). A test decodes every one-time, `gain` and timed unique of the kitchen sink, and meets every kind.

### 5.10 Tables for map generation, AI and milestones

- **Map generation.** The 24 map-generation types compile into `TerrainGen`, `ResourceGen` and `NaturalWonderGen`, with `GenCond { tiles, without, regions, except_regions }`. They never reach a unique index.
- **AI.** `AiChoiceWeight` (76 uses) becomes a per-object `ai` list for the advisor and the bot.
- **Victory.** Victory milestones compile to `Milestone`.

**As built in 1a-06** (§5.10):
- **`Ruleset::gen_tables()`** (`rules::gen_tables::GenTables`) holds `terrains` (`TerrainGen` per terrain), `resources` (`ResourceGen`), `wonders` (`NaturalWonderGen` per natural wonder), `ai`, `inert` and `placed`. The shipped ruleset has 23 map-generation types in use, not 24, and 322 map-generation uniques, every one in `placed`.
- **Conditions.** `GenCond::holds(filters, w, tile)` reads the tiles through `TileFacts`. Map generation builds no start regions, so `in [region] Regions` never holds and `in all except [region] Regions` always does, as Python skipped the one and ignored the other. Only `Doesn't generate naturally`, `Never receives any resources`, the frequency and the two weightings, and `Neighboring tiles will convert to` take conditions; a condition on another map-generation unique, or any conditional but tiles and regions, does not load.
- **Where each type goes.** Terrain types go on terrains, resource types on resources, natural-wonder types on natural wonders; anywhere else is an error. `Becomes [x] when adjacent to [River]` is the tile's own river (`Near::River`, `mapgen.py:888-891`); any other filter is a neighbour's. The filters map generation reads, start biases included, must be terrain-level (`TileFilter::terrain_level`). The tables and `StartBias` hold them as `GenFilter`, a `TileFilterId` only the loader makes and only a terrain-level one of a loaded ruleset; `Filters::gen_matches` takes nothing else, and debug builds check it.
- **AI weights** are per tech, policy, belief, promotion, building and unit (76 in the shipped ruleset), each an `AiWeight { percent, unique }` whose unique's conditionals decide whether it holds. A weight on anything else does not load.
- **Milestones.** `VictoryDef.milestones` is a list of `MilestoneDef { milestone, text }`, read at link time: `Build`, `AnyoneBuilds`, `SpaceshipComplete`, `CompletePolicyBranches(n)`, `CaptureAllCapitals`, `DestroyAllPlayers`, `WinDiplomaticVote`, `HighestScoreAfterMaxTurns` (`victory.py:249-270`). A milestone the engine does not know is an error, where Python answered no forever.
- **Inert uniques** are listed as `Inert { unique, reason }`, 64 in the shipped ruleset.
- **Outside uniques.** An improvement's `terrainsCanBeBuiltOn` is a `TerrainSet`, and a nation's `start_bias` holds tile filters.
- **Checks.** The golden `gen.json` snapshots every table; a test holds `placed` to every map-generation unique.

### 5.11 The evaluation API

```rust
#[derive(Clone, Copy, Default)]
pub struct Ctx { pub civ: Option<PlayerId>, pub city: Option<CityId>, pub unit: Option<UnitId>, pub tile: Option<TileIdx>,
                 pub combat: Option<CombatCtx>, pub ignore_conditionals: bool }
pub trait EvalWorld: TileFacts {
    fn rules(&self) -> &Ruleset;
    fn civ_index(&self, p: PlayerId, layer: IndexLayer) -> IndexRef<'_>;   // validates the memo on read
    fn city_local(&self, c: CityId) -> IndexRef<'_>;  fn follower(&self, r: ReligionId) -> IndexRef<'_>;
    /* plain facts: turn, happiness_seen, stocks, techs, era, war, counts, seat, tile and unit facts ... */
}
pub mod uq { civ, civ_no_resources, city, unit, unit_and_civ, terrains, object, raw, any, sum_i32, requirement_problems }
```

- **`EvalWorld`** is a trait of plain facts: game, civ, city, unit and tile. It is generic and monomorphised, and deliberately not object-safe.
- **One production implementation,** `game::eval::EvalView<'a> { rules, st: &'a State, dv: &'a Derived, revs: &'a Revs }`. Mock worlds let the conditions be tested in 1a, before any `Game` exists.
- **Reads validate themselves.** Every derived read inside `EvalView` goes through a self-validating memo (§6.3).
  - A caller never has to know which memos an evaluation will touch. That depends on the `CondDeps` of whichever uniques it happens to hit, which no call site can know.
  - There is no `ensure_*` family and no debug-only freshness assert.
  - A stale read is impossible in every build profile.

**As built in 1a-07** (§5.11, pinning risk 7):
- **`Ctx`** is as drawn. `Ctx::{IGNORE, civ, city, unit, fight, tile}` build one, and `resolve` derives the civilization and the tile as Python's constructor did. A context written field by field must resolve itself: `applies` and `applies_scoped` check `Ctx::is_resolved` in debug builds, since a city, unit or fight without its civilization would fail every civilization conditional silently. `rel_unit`, `rel_tile` and `rel_city` port the three properties. `CombatCtx` is `{ our, their, attacked_tile, action: Option<CombatAction> }`.
- **`EvalWorld: FilterFacts`** has:
  - `rules`, `seed` and `grid`;
  - the settings: turn, speed, starting era, a seat's difficulty (or the game's), victories, religion, espionage and nuclear weapons;
  - `civs` and `cities`, as iterators;
  - civilization facts: at war with anyone, golden age, happiness as seen, stocks, resources, era, techs, what it researches, policies, completed branches, beliefs, `ReligionProgress`, prophets earned, capital, cities and units;
  - city facts: tile, health, stored food, population, specialists, unemployed citizens and majority followers;
  - tile facts: its city, its landmass and the units on it;
  - unit facts: tile, health and used actions;
  - the four index reads: `civ_index(p, IndexLayer)`, `city_local`, `follower` and `unit_index`.

  Each group's doc names its class. The iterators are return-position `impl Iterator`. `ReligionProgress` lives in `rules::defs` beside `SpyAction`, for game state to share.
- **`IndexRef<'a>`** is `Plain(&Csr)` or `Memo(Ref<Csr>)`, and derefs to a `Csr`.
- **`uq`** (`unique::query`):
  - `civ`, `civ_no_resources`, `city`, `unit`, `unit_and_civ`, `terrains`, `object` and `raw` give `Hits`, an iterator of `Hit { id, unique, n }`. It walks the indexes lazily and evaluates each candidate's conditionals;
  - `any` and `sum_i32` consume one;
  - `requirement_problems(w, ids, ctx, p)` reports why requirements are not met;
  - a query of one object's uniques skips its timed and triggered ones, as `matching` skipped timed ones.

### 5.12 Unique indexes: memos, not hooks

**Decision (index maintenance).** The rules area maintained the per-civ CSR indexes eagerly through about 12 mutation hooks. The pipeline area treated `CivIndex` as a memo, and the memo wins. `unique::index` keeps:
- the CSR structure: `Csr { start: Box<[u16]>, entries: Vec<Entry { id: UniqueId, n: u16 }> }`, sorted by (slot, id), where `n` counts copies (five Monuments are one entry with n = 5, and their conditions are evaluated once);
- `CivIndex::build(rules, &CivSources)`;
- `placeholder_counts` for refcheck.

`game::derive::civ` builds `CivSources` from `State` and rebuilds the index whenever the civ's `index` revision moves, with an early cutoff when the result is equal. The reasons:
- a rebuild is 300-500 entries sorted, about 5-10 µs, and happens a few times per civ per turn;
- a missed hook is impossible, because every write to an index input goes through a `Touch` or `Change` that bumps the revision (§6.4);
- the "incremental equals rebuild" check becomes the general cache oracle.

**The same applies to:**
- the city-local building index;
- the per-religion follower index, shared by every city that follows it;
- the unit profile table: `LookupMap<(BaseUnitId, PromotionSet), ProfileId>`, append-only per game and never iterated.

**A city query** chains, in Python's order (`cities.py:48-89`):
1. the city's local index;
2. the majority religion's follower index;
3. the civ's main index;
4. the civ's resource layer.

**Python behaviour fixed** here, each with an entry in `intended.toml` once refcheck shows it:
- unknown conditionals and unparseable parameters are load errors;
- the six dead `_COND` keys are gone, and their UnCiv wordings are supported instead (§5.6);
- there are no global caches;
- conditionals read committed happiness and staged resource supply;
- the Marble fix. The unique `[+15]% Production when constructing [All] wonders [in this city]` applies to every city in Python, because resource uniques ignore `in this city`. **Decision:** a resource unique marked LOCAL applies only in cities that own an improved tile with that resource.

**As built in 1a-07** (§5.12):
- **`Csr`** has one run per `UniqueType`: `start` holds `UniqueType::COUNT + 1` offsets. A standing unique (effect, flag or typed tag) sits at its own type, and a triggered one at its trigger's type. A tag with no UnCiv type is not indexed, because filters read tags from their sources. Copies merge into `n`, saturating.
- **`CivSources`** is `{ nation, buildings: Vec<(BuildingId, u16)>, policies, techs, temporary: Vec<UniqueId>, era, city_states: Vec<(CityStateTypeId, CityStateBonus)>, founder_beliefs, resources: ResourceSet }`.
  - `resources` is the resource layer: empty for `CivIndex` and filled for `CivIndexFull`, so one build function makes both. The city query's step 4 is therefore inside step 3's index.
  - A building's or a resource's LOCAL uniques go to `index::city_local(rules, buildings, resources)` instead. Every other source's uniques go whole into the civilization's index.
  - `index::follower(rules, beliefs)` and `index::unit_profile(rules, base, promotions)` build the other two indexes. `placeholder_counts` counts by each unique's own placeholder.
- **Checks.** A proptest in `tests/props.rs` builds random sources over the kitchen sink. The index is the same with the lists shuffled or reversed, sorted by (type, id), with each unique once.

**As built in 1b-05** (§5.12, the Marble decision): a resource's LOCAL uniques hold in a city when the city owns a tile that gives its owner the resource (`economy::tile_provides_resource`: revealed, improved and unpillaged, or the city's own tile) **and** the owner's supply has some of it (`ResourceSupply::positive`). A resource traded away or used up gives them nowhere, as Python's resource layer held only what the supply had. They sit in the city's own index: `CityLocal` holds the buildings' LOCAL uniques, and `CityLocalFull` merges in those of the resources, so a city query reads them first (steps 1 to 4 become `CityLocalFull`, follower, `CivIndexFull`). The supply's view reads `CityLocal`, so no resource unique, local or not, changes the supply (§6.6), as Python's `local_umaps` held none.

---

## 6. Turn pipeline, caches and algorithms

### 6.1 `Game` and the five rules

```rust
pub struct Game {
    rules: &'static Ruleset,
    st: State,
    dv: Derived,               // every cache: self-validating memos (Cell/RefCell), visibility counts; never saved
    chron: Chronicle,
    pending: Pending,          // recheck: BitSet (raw CityId); sight: dirty vision sources; empty at every settle point
    fx: EffectQueue,           // follow-ups from derived reactions (first contact, explored, memory)
    frames: FrameWriter,       // last frame for deltas (derived)
    batch_start: u32,          // first event id of the current public call
    debug: DebugOptions,       // { invariants: bool, verify_caches: bool }, not saved
    poisoned: Option<Box<str>>,
}
```

1. **`Derived` is a pure function of `State`.** Dropping it and recomputing cold gives identical answers; `verify_caches` checks exactly this.
2. **Reads take `&self` and validate lazily.** A memo checks its own inputs when it is read (§6.3). No query, view or evaluation needs `&mut`, and no read can change `State`.
3. **Writes go through `game::mutate`** (§6.4). No other code can get `&mut` to state.
4. **Consequential writes happen only in settle.** These are the writes caused by other writes: citizen assignment, and sight with everything it reveals.
   - Settle runs at the end of every successful mutating call and at the ◆ points of a turn, to a fixed point (§6.7).
   - It never runs after a refusal, a query or a view.
   - Pending work is empty at every settle point, and saves, snapshots and digests are taken only there.
5. **Deterministic by construction** (§7).

**Threading.** `Game` is `Send + Clone`, but not `Sync`: `Cell` and `RefCell` are `Send` and not `Sync`, which is what the earlier draft overlooked when it ruled them out. Hosts hold a `Game` behind `&mut` or a lock. citar-py puts it in a `std::sync::Mutex` inside the `#[pyclass]`, which must be `Sync`. Panics are caught with `catch_unwind(AssertUnwindSafe(..))`, and a caught panic poisons the game, so no broken state is observed afterwards.

### 6.2 Stages of a turn

This ports `turns.py:20-118` and `190-202` and `game.py:1004-1037`. ◆ marks a settle point. Python's `g.invalidate()` calls at `turns.py:38, 56, 87, 108` and `115` become settle points or disappear.

**`start_player_turn(g, p)`:**
- **S0.** A dead civ returns at once. A barbarian runs `units::start_turn` for each unit, then `barbarians::take_turn`, then ◆, then returns.
- **S1.** Commit `happiness_seen(p)` (§6.6), then ◆.
- **S2.** Only if the civ has cities: research progress, great people, religion start, the Maya calendar.
- **S3.** Majors only: the city-state great-person gift tick, and revolts (`Purpose::Revolt`/`RevoltDelay`).
- **S4.** Triggers "upon turn start".
- **S5.** `cities::start_turn` for each city in id order. Citizens are only flagged here.
- **S6.** `units::start_turn` in id order.
- **S7.** ◆ Flagged citizens are reassigned here.
- **S8.** A city-state runs `city_states::ai::take_turn`. Anyone else runs `automation::run_unit_orders`, with a settle after each move order.
- **S9.** ◆ Victory check; `research_needed` and `turn_start` events.

**`end_player_turn(g, p)`:**
- **E0.** For a major: expire negotiations and clear return offers. A dead civ returns. A barbarian runs `units::end_turn` and returns.
- **E1.** Triggers "upon turn end", then commit `happiness_seen(p)`, then ◆.
- **E2.** Read `CivStats(p)`. Write `last_gold_rate` (a bug fix, §4.5) and the totals. A change of sign flags the civ's cities.
- **E3.** Policies (culture); city-state end of turn; gold and bankruptcy; research; religion (faith); espionage; great people.
- **E4.** `cities::end_turn` in order `(!razing, CityId)`, so razing cities go first, as Python's stable sort on `not c.razing` does.
- **E5.** Temporary uniques expire, then ◆.
- **E6.** Golden-age progress from `Happiness(p).total`; worker builds; `units::end_turn`; ◆; victory check; the `turn_end` event.

**`end_round(g)`:** eliminations; then `diplomacy::process_round`, `victory::record_stats`, the frame delta, `turn += 1`, the UN vote, victory and the turn limit; then the optional digest.

**`Game::end_turn(pid)`** keeps `Game.end_turn`'s semantics:
- it ends `pid`'s turn;
- it advances to the next living player;
- it auto-plays city-states and barbarians inside the call;
- it stops at the next major.

The chat rule that refuses `end_turn` belongs to the `EndTurn` action, not to `Game::end_turn` (phase0-spec A1.5).

**Decision (stage tables).** The stages are data, not a long function:

```rust
pub struct Stage { pub id: StageId, pub name: &'static str, pub run: fn(&mut Game, PlayerId), pub porting: Porting }
pub enum Porting { Ported, Pending(&'static str /* work package id */) }
pub static PLAYER_START: [Stage; 10];  pub static PLAYER_END: [Stage; 7];  pub static ROUND_END: [Stage; 6];
```

- 1b-03 lands the tables, the driver loop and the settle points, with every system stage `Pending`.
- Each system package flips its own stages to `Ported`. For example, 1b-07 ports S2's research, E3's research and policies, and S5/E4 `cities::start_turn` and `cities::end_turn`.
- A pending stage is an explicit no-op, and `inspect` lists it.
- `golden bless` refuses while any stage is pending, and so does `xtask check` from 1c-10. So turn flow exists from 1b onward, while goldens are only ever blessed on the complete pipeline.

### 6.3 Revisions and memos

```rust
pub struct Rev(u64);   // never saved, hashed or used as an RNG key
pub struct Revs { pub now: Rev, pub tile: Vec<Rev>, pub tile_owner: Vec<Rev>, pub tile_height: Vec<Rev>,
                  pub tile_log: TileChangeLog, pub routes: Rev, pub cities: Rev, pub unit_pos: Rev, pub diplo: Rev,
                  pub wonders: Rev, pub names: Rev, pub turn: Rev, pub civ: Vec<CivRevs>, pub city: IdVec<CityId, CityRevs> }
pub struct CivRevs { index: Rev, stocks: Rev, research: Rev, happiness_seen: Rev, gold_rate: Rev, seat: Rev, units: Rev, .. }
pub struct Stamp { verified: Cell<Rev>, changed: Cell<Rev> }
pub struct CopyMemo<T: Copy + BitEq> { stamp: Stamp, value: Cell<T> }   // Stats, totals, small values
pub struct Memo<T: BitEq> { stamp: Stamp, value: RefCell<T> }          // Csr, CityMods, Buildable lists
```

`Revs` move only under `&mut Game`. Memos live in `Derived`, keyed as in §6.5.

**Reading a memo** (for example `Derived::city_stats(&self, v: &EvalView, c) -> Stats`):
1. **Hit.** If `verified == now`, return the value: a copy for a `CopyMemo`, a `Ref<'_, T>` for a `Memo`. That is one compare.
2. **Validate.** Otherwise validate each upstream memo first (recursively; the memo graph is at most 8 deep). Take the maximum of their `changed` stamps and of the input revisions, including those `Revs::cond(p, deps)` maps the memo's `CondDeps` to.
3. **Still valid.** If that maximum is at most `verified`, set `verified = now` and return.
4. **Recompute.** Otherwise recompute into a temporary and compare it with the stored value (floats as bits). Only if they differ, store it and set `changed = now`. Then set `verified = now`. That early cutoff is what stops a tech that adds no tile yield from touching any tile.

**Why the borrows are safe:**
- A `Ref` is handed out only after the memo is verified at `now`.
- Revisions move only under `&mut Game`, which cannot coexist with a live `Ref`.
- So a recompute never meets an outstanding borrow of its own memo.

The one way to get a `BorrowMutError` is a dependency cycle between memos, which is a bug. It panics in tests, and poisons the game in release. Cycles in the rules themselves are broken by persisted values: `happiness_seen` and `last_gold_rate` (§6.6).

**Cost.** A hit is a `Cell<Rev>` compare, plus a `RefCell` flag increment for a `Memo`. With `stats`, hit and miss counters are `Cell<u64>`s.

### 6.4 Writes: `Change`, `Touch` and effects

Mutable access to `State` is restricted to `game::mutate`. `State::{tiles_mut, units_mut, cities_mut, players_mut, diplo_mut, world_mut, config_mut}` are `pub(crate)`, and `xtask check` allows calls to them only from `game/mutate.rs`, `save/` (loading) and `compat/` (conversion). Rule code writes through `Game` in exactly two ways.

**1. Setters returning `#[must_use] Change`.** These are for writes whose consequences need the new state: ownership, placement, a city appearing or disappearing, a tile changing.

```rust
#[must_use] pub enum Change {
    TileInput(TileIdx), TileHeight(TileIdx), TileOwner { t, old, new }, UnitPlaced { u, owner, from, to },
    UnitOwner { u, old, new }, UnitRemoved { u, owner, at }, CityAdded(CityId), CityRemoved(CityId), CityTiles(CityId),
    CityOwner { c, old, new }, Diplo { a, b }, Met { a, b }, Alliance { cs, old, new }, Spy(PlayerId),
    Seat(PlayerId), PlayerAlive(PlayerId), Turn, Names,
}
```

- **How a setter is applied.** `Game` wraps each state setter. For example, `g.set_tile_owner(t, o)` calls the setter and passes the `Change` to `g.changed(ch)`, which:
  1. bumps the revisions;
  2. calls `dv.on(&st, rules, ch) -> Effects`, which reads `State` and never writes it;
  3. queues effects and pending work.

  With `unused_must_use` and `clippy::let_underscore_must_use` denied, dropping a `Change` as a bare statement or through `let _ =` does not compile. `_ = ...`, `let _x = ...` and `drop(...)` get past both lints, so they are review items.
- **Seat changes are `Change::Seat(p)`.** It bumps the civ's `seat` and `index` revisions and flags all its cities for a citizen recheck. Three things depend on the seat:
  - citizen weighting depends on the controller (`BOT_MANAGED`, cities.py:756);
  - the Human/AI player filters depend on the handicap;
  - yields and costs depend on difficulty.

**2. Touches**, for field edits on one entity, which bump before handing out `&mut`:

```rust
pub(crate) fn city_mut(&mut self, c: CityId, t: CityTouch) -> &mut City;         // CORE | WORK | STOCKS | RELIGION | NAME
pub(crate) fn player_mut(&mut self, p: PlayerId, t: PlayerTouch) -> &mut Player; // INDEX | STOCKS | RESEARCH | HAPPINESS_SEEN
                                                                                 // | GOLD_RATE | NAME | CITY_STATE | SPIES
pub(crate) fn unit_mut(&mut self, u: UnitId, t: UnitTouch) -> &mut Unit;         // CORE | BASE | MOVES | SIGHT
```

- **What a touch does.** It bumps the matching revisions and raises the matching pending work before returning:
  - `WORK` and `CORE` flag the city for a citizen recheck;
  - `SIGHT` marks the unit's vision source dirty;
  - `NAME` bumps `names`.
- **What a touch cannot reach.** The fields whose effects need the new state are private even from `&mut City` and `&mut Unit`, so they can only move through setters: unit owner, tile and carrier; city owner and tile; tile fields.
- **Over-bumping is cheap,** because early cutoff stops the cascade.
- **Under-bumping is caught.** A touch with the wrong flag is caught by `verify_caches` (§9.4), which runs at every settle in scripts, properties and chaos. The `stats` counters show over-bumping as redundant recomputes.

**Effects.** `Derived::on` turns changes into effects. Applying effects writes state:
- explored bits and memory snapshots;
- `meet`;
- natural-wonder bonuses;
- recheck flags.

Applying one may produce further `Change`s. The effect queue drains in a fixed order, sorted by `(kind, civ, other, tile)`. The loop is bounded, and asserts if it runs away. This replaces Python's re-entrant chain of `refresh` → `meet` → `emit` (`visibility.py:128-168`) in a form the borrow checker accepts.

### 6.5 The memo graph

| Memo | Key | Inputs | Cutoff | Replaces |
|---|---|---|---|---|
| `CivIndex` | civ | civ `index` rev (techs, policies, era, temporary uniques, founder beliefs, city-state bonuses, non-local buildings) | CSR equality | economy.py:91-147, which was rebuilt 49,582 times in 100 turns |
| `ResourceSupply` | civ | `CivIndex`, `resources_in` (tiles, units, buildings, deals, allied city-states), conditions | the itemised list | economy.py:235-353 |
| `CivIndexFull` | civ | `CivIndex` plus the resource layer from `ResourceSupply.positive` | CSR equality | economy.py:77-88, and the `None` guard at :323-336 |
| `CityLocal` | city | city `core` | CSR equality | the local half of cities.py:48-89 |
| `FollowerIndex` | religion | the religion's beliefs | CSR equality | follower beliefs copied into each city |
| `CityMods` | city | `CivIndexFull`, `CityLocal`, `FollowerIndex`, city `core`, conditions | struct equality | city unique lists rebuilt on every call (cities.py:69-89) |
| `TileYield` | tile | `tile`, `tile_owner`, `CityMods` | bits of `Stats` | tiles.py:265-349, cleared on every unit move |
| `CityHappiness`, `CityStats` | city | city `work` and `core`, `CityMods`, the `TileYield` of worked tiles, `Connectivity`, civ `happiness_seen` and `seat` | totals | cities.py:498-587 |
| `Happiness` | civ | `CityHappiness` of each city, `ResourceSupply`, `CivIndexFull` | total | economy.py:419-499 (`_hap_busy`) |
| `CivStats` | civ | `CityStats` of each city, units, diplomacy, `CivIndexFull` | totals | `economy.civ_stats` |
| `Connectivity` | civ | routes, cities, civ `index`, diplomacy | map equality | cities.py:1991-2067 (831 BFS searches per round) |
| `SightMods`, `UnitSight` | civ, unit | `CivIndexFull`, the unit's core | value | visibility.py:74-93, units.py `sight` |
| `Buildable` | city | `CivIndexFull`, city `core`, civ cities, wonders, `ResourceSupply` | list | cities.py:1347 (98 ms for 22 cities) |
| `JobMap` | (civ, builder class) | per tile: `tile`, `tile_owner`, civ `index`, luxuries | per tile | automation.py:313-356 |
| `DangerMap` | civ | `unit_pos`, diplomacy, civ visibility | bitset | automation.py:149-157 |
| `CityNeighbours` | global | cities | — | religion.py:266-300, which was O(C²) |
| `MoveCosts` | move class | `tile_log`, civ `index` | per tile | the static part of movement.py:122-160 and 330-377 |
| `RouteLayer` | civ | `tile_log`, cities, techs of every city owner, civ `index` | per tile | movement.py:282-310 (`route_at`, `has_connection`) |
| `Zoc` | civ | `unit_pos`, cities, diplomacy | bitset | movement.py:312-327 |
| `LosCache` | (tile, radius, mode) | `tile_height` within radius + 1 | evicted eagerly | `_static` |

Yields seen by a civ that does not own the tile go in a `RefCell<LookupMap<(PlayerId, TileIdx), CopyMemo<Stats>>>` that is cleared each turn.

### 6.6 Three cycles made explicit

1. **Happiness.**
   - **What reads it.** Conditionals ("while the empire is happy") and citizen ranking read the persisted `econ.happiness_seen`.
   - **When it is written.** It is committed only at fixed stages: S1 and E1 of the civ's own turn, once for every civ during setup, and once after a Python conversion.
   - **Why this is stable.** Within a player's turn it is a constant. The number of calls cannot make it oscillate, and a save and load between any two calls changes nothing.
   - **Why the earlier draft was wrong.** It committed at every settle. Citizen ranking doubles the weight of happiness below 0 (cities.py:767-768), and crossing 0 re-flags every city, so each call could flip citizens and happiness back and forth.
   - **Why S1 and E1.** S1 gives the turn a value that includes everything other players did. E1 lets the end-of-turn economy (E2-E6: gold, research, growth, golden ages) see the turn's own actions. The critic proposed S1 and E5; E5 comes after growth and production, which would then run on the value from the start of the turn.
   - **What Python did.** It kept the lag inside one computation (`_hap_busy`/`_last_hap`, economy.py:404-433) and ranked citizens on the live value (cities.py:748). Both differences are intended entries. These are UnCiv's stored-stat semantics.
2. **Resource supply** is a three-step chain: `CivIndex` (without resource uniques) → `ResourceSupply` → `CivIndexFull`. Conditions evaluated during supply read step 1, as Python's `_civ_uniques_nores` did.
3. **Gold.** Citizen ranking reads `last_gold_rate` (cities.py:765, 777-784), which is persisted at E2, never the live figure. A change of sign flags the civ's cities.

**As built in 1b-05** (§5.11, §5.12, §6.2, §6.5, §6.6):
- **Files.** `game/derive/civ.rs` (the memos and tables, `sources`, `cond`, `verify`; tests with the gate-3 proptest in `civ/tests.rs`, proptest a dev-dependency of the engine), `game/economy.rs`, `game/cities/{mod, uniques}.rs`, `game/units.rs`, `research::player_era`; in `unique`, `Csr::merged`, `index::{resource_layer, unindexed}`, `IndexRef::Shared` and `uq::local` (a city's own and religion's uniques, `cities.local_uniques`); `crates/citar-testkit/tests/engine/economy.rs`; the refcheck `civs` module; `crates/citar-bench/benches/index.rs`.
- **`CivIndex`** is valid while the civilization's `index` revision stands; `sources(g, p)` gathers its `CivSources` (buildings counted over its cities, the city-states it has met that count it a friend or are allied with it, its religion's founder beliefs, the era from its techs). **`CivIndexFull`** is `CivIndex` merged with the resource layer (`Csr::merged`, `index::resource_layer` of the supply's positive resources), validated against the two memos' `changed` stamps. **`ResourceSupply`** (`economy::compute_supply`, the itemised list and the net totals in first-appearance order, zeros kept as Python's dict kept them) validates against revisions only: its civilization's `roster` (a new `CivRevs` field: units made, lost, given away or upgraded, which `UnitTouch::BASE` marks; not moved, healed, promoted or given orders, which `CORE` covers), `index`, `cities`, `buildings` and `city_state`, the same of each allied city-state, each of their cities' `religion`, the global `turn`, `diplo` (deals), `alliances`, `cities` (a city-state dying) and `religions`, the tiles changed since its last verification (`TileChangeLog::since`) that one of them owns, and the conditionals of the three unique types it evaluates (the civilization-level classes once per owner, the local ones city by city). So a move, a heal, a promotion, an order, growth or a citizen reassignment never recomputes it. `touch_unit` takes the unit's owner for `roster`. `roster` is the supply's alone: unit upkeep also reads where a unit stands, its promotions and its unit-level conditionals, so a memo of it validates against `units`, `units_core`, `cities` and its conditionals.
- **`CityLocal`** and **`CityLocalFull`** are memos per city, created and dropped with the city (`CivCaches::track`, from `Game::changed`). `CityLocal` holds the buildings' LOCAL uniques and validates against the city's `buildings`. `CityLocalFull` adds the LOCAL uniques of the resources the city's tiles give (`economy::provided_resources`) that its owner's supply has some of (the Marble decision, §5.12); it validates against `CityLocal`'s and the supply's `changed` stamps, the owner's `index` and `cities`, the global `cities` and the changed tiles of the city's territory. `EvalView::city_local` lends `CityLocalFull`, and `CityLocal` in the supply's view. The **follower index** and the **unit profiles** are tables keyed by value (the follower beliefs; the base unit and its promotions), pure functions of their keys, that only grow and are never iterated; they lend `Arc<Csr>` (`IndexRef::Shared`), so a lookup may add a key while other indexes are read. `Game` stays `Send`.
- **The supply's view** (`EvalView::for_supply`): every civilization's `Full` layer reads as `NoResources`, every city's own index as `CityLocal`, and every resource as none, so no supply depends on its own or another's resource uniques (an ally and its city-states read each other's cities). A memo read while a supply is computed must not read `CivIndexFull` or `CityLocalFull` with a view of its own, or it forms a cycle; so far none does (`CityLocal`, the follower and profile indexes and the era are pure).
- **`RESOURCES`.** `derive::civ::cond(g, deps, ctx)` maps it to the supply memo's stamp and the rest through `Revs::cond`; memos that evaluate uniques validate with it. `Revs::cond` and `Revs::cond_civ` are crate-private, and a debug build refuses `RESOURCES` with a civilization in context there (a release build reads it as the current revision: correct, but never valid, so a memo validated with it would recompute on every read). The testkit reads `civ::cond`.
- **The oracle.** `Game::verify_caches` adds `derive::civ::verify`, which compares every civilization's three memos, every city's two local indexes and every follower and unit index with a cold game built from a clone of the state; the fixture gate runs it on all 262 states. It also checks the era and owned-tiles memos. The proptest (256 cases) drives random writes to every input (buildings, techs, policies, temporary uniques and their expiry, influence and contact with a city-state, war, religions, resources and improvements and pillage, units made, removed, promoted, upgraded, moved and given away, cities founded, razed and changing hands, Geneva eliminated and revived, its ally set, resource deals made, run out, cancelled and a civilization trading away all it has, seats handed over, the turn) with reads between them. Half its resources are ones with LOCAL uniques and half its new units need a resource, so the supply's inputs and the Marble decision are reached often: dropping the supply stamp from `CityLocalFull`, the `turn` from the supply, or `roster` from a unit's gift, loss or upgrade each fails it.
- **Stages.** E3 banks `trunc(last_gold_rate)`, the rate stage E2 commits (package 1b-06), after bankruptcy: while the treasury is at -200 or below and the rate negative, a military unit is disbanded (own land first, fewest promotions and experience first, its cargo with it) and the rate is read again, which until 1b-06 adds back the unit upkeep saved (`Pending("1b-06")`); the disband refund needs purchase costs (`Pending("1b-07")`). E5 counts temporary uniques down and drops the spent ones, moving the index only then.
- **Economy.** `unit_maintenance`, `transport_upkeep` (routes only, as Python read `Costs [n] [stat] per turn` and `... when in your territory` on roads alone; it walks the civilization's owned tiles, a memo per civilization valid while its `cities` revision stands, which every change of a tile's owner moves for both sides), `unit_supply`, `unit_supply_deficit`, `unit_supply_penalty`. A negative free-unit allowance counts as none, where Python's slice counted from the end. The kitchen-sink extras of this subsystem are tested in `tests/engine/economy.rs`: `BaseUnitSupply`, `UnitSupplyPerCity`, `UnitSupplyPerPop`, and `ImprovementMaintenance` on a road (the kitchen sink puts it on an improvement, where Python and Rust read no maintenance); the timed uniques' temporary variants run out at E5.
- **Also ported for the index:** `research::player_era` (the era's uniques are in it), kept as a memo per civilization valid while its `index` revision stands (`derive::civ::era`), which `sources`, `EvalView::civ_era` and `query::era` read, since era conditionals are asked once per unique; and the `monotonic` AI base values, with Prince among the known objects (`Known::prince`). `cities::uniques::contains_building` reads each building's equivalents (those replacing it or tagged with its name), resolved at load in `rules::Derived::building_equivalents`.
- **Refcheck `civs`** answers and enforces `resource_supply`, `detailed_resources`, `unique_index`, `unit_maintenance`, `unit_supply` and `era`, with no difference on the 12 committed states or the 250 of the corpus. `unique_index` counts by placeholder the entries of `CivIndexFull` with their copies and the uniques of the same sources the index leaves out by design (`index::unindexed`: one-time, action, AI, map-generation, requirement and inert uniques, tags of no type, and a resource's that hold in one city); a unique of no type counts under its text. The three 1a-07 entries that covered `civs[*].**` (building conditionals, between-stat speed scaling, beliefs counting as adopted) no longer list `civs`: they hid every difference of the group. The ratchet holds the group's missing paths (848 on the committed states).
- **Criterion** (laptop): a lookup in a verified `CivIndexFull` 3 ns, a `CivIndex` rebuild of the largest index of the late fixture 2.8 µs, gathering its sources 0.7 µs.
- **Left to later packages:** `upon turn start` and `upon turn end` (rows S4 and E1, 1b-08, which also fills religious majorities, so `uq::city` reads a follower index); the civilization's stats and happiness (1b-06).

**As built in 1b-06** (§5.8, §5.12, §6.2, §6.5-6.8, §6.11, §9.2, §9.3, §9.7), after its fix round:
- **Files.** Engine: `game/tiles.rs`, `game/religion.rs` (a city's followers and majority, the reads 1b-08 builds on), `game/cities/{stats, connections, citizens, founding}.rs`, `game/derive/stats.rs`, `unique/record.rs`, and in `game/economy.rs` happiness, the empire-wide stats, `stat_map`, `civ_stats`, gold per turn and the commits; the actions `SetCityFocus`, `SetSpecialists` and `WorkTile`. Testkit: `tests/engine/cities.rs` and eight rule scripts (`cities_{work_tile, focus, specialists, blockade, adjacency, share_tile}`, `economy_{unhappy_growth, happiness_commit}`). Refcheck: `answer/{tile_yields, city_stats}.rs` and four more `civs` paths. Bench: `citar-bench/benches/stats.rs`. The memos live in `derive::stats`, not the `derive::tile` of the Scope.
- **No `TileTags`.** §6.11's per-tile bitset of tile-filter facts is not built: the compiled filters (§5.7) already test a tile in a few set lookups, and a tile's yield recomputes in 0.3 µs against the 1 µs budget, so a second representation of the tile's facts to keep in step bought nothing.
- **What a memo read, recorded** (`unique::record`). A computation run with `recorded(f)` collects the classes of every unique it evaluates (`applies` notes `UniqueTable::reads(id)`: its conditionals' classes and those of the filters, countables and population its parameters name, computed at load by `cond::assign_deps`), whether the unique applies or not, since a conditional that fails now may hold after a write; a lazy query that stops early records what decided its answer. A computation may note a class of state it reads directly (unit upkeep notes `UNIT` when the units in cities are free). `Memo::get` and `CopyMemo::get` validate and recompute `isolated`: what a memo upstream reads is its own, and the memo downstream validates against its stamp. The recorder is the thread's, empty between computations; a later `par` must carry it into its sections (§6.13). So a memo validates against the classes its own last computation read, not against every class the ruleset's uniques of those types could read (the first build's `StatsDeps`, which on the shipped ruleset was every class, through `connected to capital` and `with a garrison`, so nearly every write recomputed every city's and civilization's stats).
- **Classes** (§5.8). `CondDeps` has 26 bits (`UFlags` the top 6 of the word): `CONNECTED` is the trade network of the civilization in context and of the owners of the cities in context and in the fight, which `civ::cond` maps to the `Connectivity` memo's stamp (the revisions alone read it as moved on every write, which the supply, computing the network without the memo, does), and which `in cities connected to the capital` (`CITY | TILE | CONNECTED`) and the city leaf read instead of every class; `INFLUENCE` is every city-state's influence, read by the friendly leaf (`WAR | TURN | INFLUENCE`) and the friendly and foreign land leaves (still every class: they read the viewer's own uniques), so `WAR` is the relations alone. `[in capital]` reads `CITY`, the city's own fact (a counted city's is in the `CITY_COUNT` whatever counts adds), and `CITY` no longer maps to where the city's citizens work: that is `TILE`'s (its centre's territory city's worked tiles and specialists), which every conditional about citizens reads too. An adjacency filter (`[stats] for each adjacent [tileFilter]`) reads the neighbours' own facts in the tile's memo, and `MAP` only for leaves that read more of a neighbour (`TileFilter::deps_around`).
- **Tile yields.** `tiles::compute_tile_yield(g, t, viewer, city, mods, &mut deps)` ports `tiles._tile_stats` and adds to `deps` the classes of every conditional and filter it evaluated. The owner's view of a tile as its own city works it is a `CopyMemo` per tile with its recorded classes beside it; any other viewer or city is an entry of a table keyed by `(tile, viewer, city)`, validated alike, which the oracle walks, which loses a removed city's entries, and which is emptied at each turn; a city seen by a civilization other than its owner is computed and not kept, since no rule asks. A tile validates against its own inputs and its six neighbours' (fresh water, the coast, adjacency), its owner, which cities exist, the settings, the viewer's `index` and golden age (`CivRevs::golden_age`, moved by `PlayerTouch::GOLDEN_AGE` when the turns left cross 0: `Game::set_golden_age_turns` picks it or `STOCKS`), and `CivIndexFull` with no city, its city's `CityMods` and its classes.
- **`CityMods`** (`tiles::city_mods`): a city's tile modifiers gathered once, every conditional that does not read the tile evaluated there (§6.11), so a tile evaluates only its own filters and the conditionals that read it. It validates against `CityLocalFull`, `CivIndexFull`, the city's `core`, `buildings` and `religion`, the religions and settings, and its own recorded classes.
- **City stats.** A source's stats keep the keys Python's dict held (`Yields { stats, keys: StatMask }`), zeros included, since the answers are compared key by key. Three memos per city, each with its recorded classes: `CityBase` (what does not depend on where the citizens work: the buildings' yields, the uniques' flat yields by source, the food percentage and the tiles that yield without a citizen; valid across reassignments, against the city's `core`, `buildings`, `religion` and territory, `CityRevs::tiles`, moved by a tile of its territory changing or changing hands), `CityHappiness` (`CityParts`: the base, the tiles, specialists and happiness list; it validates against the city itself but its stocks, its territory, its base, and the yields of the tiles its last computation added, which it keeps) and `CityStats` (against its parts, the city, its owner's index, golden age, seat, connectivity and supply deficit, its capital, and in We Love The King Day its owner's happiness). The uniques that add to each building are gathered once per city (`BuildingUniques`).
- **Happiness, the civilization's stats and unit upkeep** (`economy::{compute_happiness, global_stats_from_uniques, stat_map, compute_civ_stats, gold_per_turn, unit_maintenance}`) are memos per civilization, each with its recorded classes, the local ones read over the civilization's own cities, tiles and units (`civ_level_cond`). They validate against its index with the resource layer, stocks (and the natural wonders it found, written with them), golden age, seat, cities and territory, buildings and religion; the tiles changed since their verification that it (and, for happiness, an allied city-state) owns; which cities exist, alliances, religions, the turn, relations and deals, the settings; happiness against its supply and cities' parts, the stats against its happiness, upkeep and cities' stats and its allied city-states' stats. Unit upkeep reads which units it has (`roster`), its index and seat, which cities exist and the turn, so a move or a heal leaves it. The breakdowns are keyed by a typed `CivSource` (a city's `StatSource` or `HappinessSource`, a unique's `SourceKind`, and the civilization's own lines), whose `name()` is Python's key. `happiness_seen` is committed at S1 and E1 (`commit_happiness_stage`), once per civilization by the setup stage `happiness`, and once after a Python conversion without flags (keeping the cities Python assigned). A commit flags the civilization's cities for a recheck when it moves the ranking's bands (below 0, below -8), or whenever the ruleset's citizen classes read `HAPPINESS_SEEN`. E2 (`end_turn_rates`) writes `last_gold_rate` (flagging the cities when its sign changes) and adds the turn's culture and faith to the totals. E3's bankruptcy reads the stats afresh after each disbanding.
- **Connectivity** (`cities::connections`): one flood fill per medium whose visited set only grows, the railroad from the capital, the road from every city reached and a harbour's water from every harbour reached, alternating until neither reaches a new city (§6.11). `connected_cities_naive` ports Python's walk plainly, one search per city and medium, behind `cfg(any(test, feature = "test-ops"))`. The memo per civilization validates against `routes`, `cities`, `owners`, the changed tiles, `diplo` (borders, war), `turn`, its index with the resource layer (techs), `city_buildings` (harbours), the settings and the classes its `Forests and Jungles are roads` recorded.
- **Citizens** (`cities::citizens`). `RankCtx` hoists what the ranking reads per city: the focus, growth terms, construction and whether it converts food, `happiness_seen` and the sign of `last_gold_rate`. A rank is its food's worth plus the rest, so an assignment weighs each tile's other yields once. `assign(g, c, reset)` reads only, and reads the city's food with its citizens so far from `cities::stats::food_surplus`, the food column of the stats alone over the `CityBase` memo; a settle's passes call it for the flagged cities in id order (`reassign_flagged`, `Pending::take_recheck_from`), and a city whose assignment takes or releases a tile a sibling could work flags the sibling (and, where the ranking reads `MAP`, any city in reach). The writes of `game::mutate` flag what they concern (§6.7): through `Derived::on` the tiles in range, ownership and the blockade (a military unit placed, removed or changing hands, and war and peace), and the cities that may work a neighbour of a tile that changed, whose yield reads it (`citizens-follow-a-neighbour-at-once`); through `recheck_civs` and the touches the civilization-level causes, limited by `StatsDeps::citizens`, the classes the ranking's uniques read: influence for `INFLUENCE`, a treasury for `STOCKS` (its civilization's cities), a golden age beginning or ending, the turn for `TURN` and `CHANCE` and the declared friendships it ended (great person points), a city's stocks for `CITY` (that city). `City::citizens_settled` marks a city this engine has assigned, and `citizens::verify`, the citizen oracle, runs in `verify_caches`: every such city has its citizens where a fresh assignment puts them. A city's worked and locked tiles are sets; a tile a sibling works is not workable (`cities-never-share-a-tile`); a lock on a tile no longer workable is dropped.
- **Tools** `work_tile`, `set_city_focus` and `set_specialists` check, write the city (`CityTouch::WORK`), settle, and render their result from the settled game, its tiles by column and then row as `inspect` lists them (`citizen-tools-list-tiles-sorted`), so it equals what `inspect` shows after the call. A lock past the city's citizens is refused (`work-tile-refuses-a-lock-past-the-citizens`), and a count that is not a number (`specialist-counts-must-be-numbers`).
- **Forward-ported for the scripts:** the scenario operations `found_city` and `set_city` (`set_city`'s `production` is refused as not ported until 1b-07's queue) through `cities::founding` (`found_city`, `add_building`, `remove_building`, `rename_city`, `capital_indicator`), and the city numbers the `city_stats` group reads beside its stats: `production_cost`, `remaining_work`, `turns_to_build`, `max_health`, `city_strength`, `food_to_next_pop`, `maintenance`. Package 1b-07 builds construction on them.
- **Inspect.** `player` gains `happiness`, `happiness_seen` and `gold_rate`; `city` gains `worked`, `locked`, `workable`, `specialists`, `focus`, `avoid_growth`, `food` and `yields`, from both engines (Python's in the test-side `inspect.py`).
- **Kitchen sink.** `[n]% Food consumption by specialists [cities]` in the food a city eats and in its citizens' ranking; `Cost increases by [n] when built` and `[n]% production cost` in `production_cost` (`tests/engine/cities.rs`).
- **Refcheck.** `tile_yields` is enforced whole, `city_stats` but for its religion paths (`followers` and `majority` answered, `pressure_in` missing until 1b-08), and `civs` adds `happiness`, `civ_stats`, `stat_map` and `gold_per_turn`, clean on the 12 committed states and the 250 of the corpus. Intended: `marble-bonus-in-its-own-city` (limited to the 70 fixtures where it explains a difference), `religion-ties-by-founding-order` (the corpus states where a city's followers tie) and `gold-per-turn-rounds-a-sum-at-a-half`. The `city_stats` answer lists as workable the tiles the engine leaves out only because a sibling works them where Python let the city take them too, so any other tile missing is compared; that difference is a script's (`cities_share_tile`). `between-stat-scales-by-speed` moves to the script list. The ratchet holds `city_stats`' 160 missing `pressure_in` answers, and `civs` falls from 848 to 392.
- **Scripts** (intended, skipped on Python): `happiness-seen-committed`, `last-gold-rate-written`, `work-tile-refuses-a-lock-past-the-citizens`, `specialist-counts-must-be-numbers`, `citizens-follow-a-blockade-at-once` (Python took a citizen off a blockaded tile at the end of its city's turn), `citizens-follow-a-neighbour-at-once` (a Moai built beside a tile a city works, beyond its reach), `citizen-tools-list-tiles-sorted` and `cities-never-share-a-tile`. The citizen oracle holds after every step of every script (gate 4).
- **Properties.** Gate 3: on random worlds of the arena (twelve sites, three civilizations, harbours, road and railroad paths with gaps, techs, contact, open borders and war), the floods, the memo and the plain walk agree for every civilization, and still after random route edits. Gate 5: fifty queries, inspections and refusals between each pair of actions leave every digest what it is without them. A unit test with the memo counters (feature `stats`) checks that a move, a heal, another city's stocks and influence recompute no memo, a treasury only its civilization's happiness and stats, and a reassignment neither the city's base, the trade network, nor another civilization's memos.
- **Criterion** (laptop, the late fixture `small-continents-normal-s1025/t280`, `cargo bench -p citar-bench --bench stats`, with another package building and a local model serving): a tile yield hit 5 ns; the first read of each of the 141 tiles a civilization owns after a write no cache reads 41 ns; a recompute 0.31 µs; the stats of the largest city (pop 41, 37 buildings) with its base recomputed 7.1 µs; its citizens at pop 20 assigned in 6.0 µs (12.5 before the fix round's food-only surplus, base memo and split ranking); the connectivity of the civilization with 13 cities 4.0 µs; a settle with nothing pending 7 ns. Report-only: every major's stats and every city's, read after a unit of another civilization moved, 181 µs, all of it validation. `Game::settle_for_bench`, `Game::unrelated_change_for_bench` and `Game::move_unit_for_bench` exist behind `test-ops` for them.
- **Left to later packages:** religious pressure (1b-08: keeping a city's pressures in the order they arrived removes `religion-ties-by-founding-order`); the production queue and construction (1b-07); the golden ages' own writes through `Game::set_golden_age_turns` (1b-08).

**As built in 1b-07** (§6.5, §6.7, §9.2, §9.3, §9.5, §9.7), after its fix round:
- **Files.** Engine: `game/research.rs` (rewritten from 1b-02's scenario subset: costs, the queue, progress and overflow, research agreements' boost, eras entered, free techs), `game/policies.rs`, `game/cities/{construction, purchase, queue, borders, free_buildings, lifecycle}.rs`, `game/derive/buildable.rs`, and `rules::Derived::aircraft` (the base units of the air domain, which the `Air` unit filter of a hangar names); the actions `SetProduction`, `ChangeQueue`, `SetAutoProduction`, `Buy`, `BuyTile`, `RenameCity` (any time), `SetResearch`, `DequeueResearch`, `ChooseFreeTech` and `AdoptPolicy`, with their argument specs. Stages: S2 research, S5 the cities' turns, S9 the reminder to research, E3 culture and policies and science, E4 the cities' turns (razing cities first, then by id). Testkit: `tests/engine/production.rs`, twelve new rule scripts (`production_{queue, completes, lost_wonder}`, `purchase_gold`, `borders_growth_and_purchase`, `cities_{growth_and_starvation, remove_city}`, `research_{progress, queue_tools}`, `policies_{adopt, scenario}`, `costs_follow_the_handicap`), and `RandomAgent`'s moves `research`, `production`, `queues`, `policies`, `purchases` and `names`, which between them use all ten actions (a far research goal, an appended tech, a tech dropped, a free tech; appended items and conversions of production; queue edits and clears; automatic production switched; cities renamed). Refcheck: `answer/buildable.rs` and the `civs` paths `tech_cost`, `policy_cost` and `adoptable_policies`. Bench: `citar-bench/benches/buildable.rs`.
- **Rejections.** `construction::rejection_reasons` ports `cities.rejection_reasons` in Python's order with Python's kind names (`RejectionKind::name`); `rejection_kinds` gives the kinds alone, in the same order, without writing any text, for the rules that read the kind (`validate_progress`, `can_purchase_with`). `is_buildable` answers whether there is any, with the cheap reasons first whatever their place in that order (a tech, obsolescence, another nation's unique, a world or national wonder built, `Unbuildable`) and the rest without writing a reason's text (`Reasons` stops at the first); the queue check, the upgrade of an obsolete unit's progress, free buildings and the era's settler buildings use it. Of a building's own uniques only the types that may reject it are asked whether they apply (`may_reject`), where Python asked every one: the answer is the same, and a memo records only what it read. `purchase_check` hands the reasons it read to `can_purchase_with` rather than reading them twice. `equivalent_unit` and `equivalent_building` ask the nation's short list (`Derived::nation_uniques`) before scanning the ruleset for Python's first match.
- **`Buildable`** (§6.5) is a memo per city (`derive::buildable`), with the classes its computation recorded (`unique::record`; `uq::requirement_problems` notes a requirement's conditionals as `applies` does, which it asks directly). It validates against the city's `core`, `buildings`, `religion` and `tiles` (not `work`: where citizens stand is `TILE`'s, recorded when read), its owner's `index` (with the resource layer through `CivIndexFull` and `CityLocalFull`), `roster`, `cities`, `buildings` and supply, which cities exist, the wonders built, religions, the settings, and the tiles and owners within the reach of the ruleset's placement rules (`reach`: the neighbours for a coastal city; `Must be on`, `Must be next to` and `Must have an owned [] within [n] tiles` at 0, 1 and n, one further for a filter that reads the tiles beside the one it is asked of, `Fresh water` and `Coastal`; the worked tiles when a filter reads `worked`). The civilization-wide conditionals of `Only available` and `Can only be built` are asked once per civilization by a memo of their own (`CivRequirements`, the failing ones by id), which the lists validate against by its `changed` stamp: `if [Monument] is constructed in all [non-[Puppeted]] cities` reads every city's status (`CITY_COUNT`, which any city's `core` moves), and would otherwise have recomputed every list after any heal; a requirement's local conditionals (`in [Holy] cities`) are asked per city. What moves all turn stays out of the memo, and `buildable_items` reads it as it lends the list: the room aircraft need (the memo keeps the hangar's size, `air_room`, from uniques it validates, and aircraft as if there were room), a wonder another city of the owner is building (`WonderBeingBuiltElsewhere`), and the queued items a limit counts (the memo counts what the civilization has and keeps the room left, `limited`; `count_constructed` is `count_made` and `count_queued`). So a sibling's heal, growth or queue edit recomputes no list but its own. `compute_buildable` and both fields are the crate's; `buildable_items` is the only public way to the list (`compute_buildable_for_bench` under `test-ops`). Spaceship parts count with the units they were, whose removal moves `roster`.
- **Production.** `end_turn_production` puts the turn's production into the front of the queue; a conversion to gold or science keeps nothing, since the city's stats already paid it out (`perpetual-production-is-not-banked`: Python banked it as overflow as well, which the next item took whole). `construct_if_enough` finishes the item at the start of the city's next turn when what is stored pays for it, keeping at most its cost or a turn's production as overflow, and counts it for `Cost increases by [n] when built` only when it was finished (`increasing-cost-counts-what-was-built`: Python counted a unit with no room to stand every turn it waited). Both counters saturate. `validate_queue` drops what can no longer be built; `validate_progress` refunds a lost wonder's production as gold and moves an obsolete unit's progress to its upgrade. A finished building is `add_building` with its free buildings; a world wonder is recorded (`WorldTouch::WONDERS`) and told to everyone; a unit is placed in the city or beside it (`construction::placement`, which since the merge with 1c-02 is `units::spawn_spot` and its real `can_stand`; `units::add_unit_in_city` and `units::add_construction_bonuses` make it), gets its construction bonuses and, bought, no moves unless `Can move immediately once bought`.
- **Purchases** (`cities::purchase`): the standard gold price `(30 x cost)^0.75` with the hurry modifier, the specific prices of the city's uniques (the cheapest wins), an item's own `Can be purchased ...`, the era's price for `May buy [] with []`, then the discounts, rounded down to ten; a bought unit that cannot be placed is refused before any payment. `buy_tile` and border growth (`cities::borders`) rank tiles as Python did, ties broken by Python's `within` order (`within_order`).
- **Research and policies.** `tech_cost` and `culture_cost` follow the handicap (a humanlike seat pays the difficulty's research and policy modifiers), the speed, the map size, the cities founded, and the uniques that make techs and policies cheaper, overall and per city; a tech costs at least 1 and a policy at least 5, whatever a ruleset's discounts add up to. `add_science` completes one tech after another in a loop while the science carried over pays (Python's `add_tech`, `update_research_progress` and `add_science` called each other once per tech, which a cheap repeatable tech would have run without end); `add_tech` is `learn` then the carried science. `learn` announces what the tech unlocks, removes obsolete queue items, enters eras (announcing the branches a new era opens) and flags the civilization's cities. The scenario operation `adopt_policy` adopts whatever the era (`scenario-adopt-policy-skips-the-era`, which Python refused on a second check).
- **Cities' turns** (`cities::lifecycle`): S5 finishes what is paid for, starts We Love The King Day for a demanded resource, counts down resistance and the celebration and draws the next demand (keyed RNG, `Purpose::Demand` and `DemandNew`), and tells a human owner of an idle city; E4 production, border growth, razing or growth and starvation, healing. A destroyed city's capital moves to the largest city left; its religion, espionage and elimination wait for 1b-08, 1c-05 and 1c-08. `Game::turn_yields` holds the turn's totals from E2 for E3's science.
- **Scenario and test operations.** `set_city` sets `health`, `attacked`, `food` and `production` (Python's test-side `scenario.py` too); `remove_city` and `adopt_policy` are ported, and the test operation `complete_construction`. Inspect: `buildable` and `costs`; `city` gains `queue`, `progress`, `overflow`, `culture`, `health` and `tiles` (how many it owns); `player`'s `research` gains `progress` and `overflow`.
- **Kitchen sink** (`tests/engine/production.rs`): the tech cost uniques, `Obsolete with [tech]`, `Cannot build [buildings] <when at war>`, the purchase uniques of the Kitchen Sink Faith (`May buy [] units with [] []`, `May buy [] units for [] [] []`, `May buy [] buildings with [] for [] times their normal Production cost`, `[] cost of purchasing [] buildings []%`, `May buy [] buildings for [] [] [] at an increasing price ([])`) and the Works' `Can be purchased for [] [] []`, and `Cost increases by [n] when built` counted as a building is finished. Their triggers (`upon founding a city`, `upon adopting a policy`, a policy's or tech's own triggerable uniques) wait for 1b-08's triggers. Beside them, on overlays of the shipped ruleset: a requirement whose conditionals read a golden age or the turn, `Must be next to [Fresh water]` with a lake two tiles out and nothing else reading that far, a wonder and a limit read from the other cities' queues (every list checked item by item against `is_buildable`), a unit with no room that waits at no extra cost, and discounts of 100% that leave a tech at 1 and a policy at 5 while 5,000 Future Techs are learned in one call.
- **Refcheck.** `buildable` is enforced whole, and `civs` adds `tech_cost`, `policy_cost` and `adoptable_policies`, clean on the committed states and the corpus; `civs`' ratchet falls from 392 to 126 (score, military, victory and the world, 1c-08). The turns `buildable` shows for a wonder are explained by `marble-bonus-in-its-own-city-wonder-turns` (its own case list, Marble's cases checked against Python with the bonus limited to the city that owns the tile) and `religion-ties-by-founding-order`; `marble-bonus-in-its-own-city` keeps 1b-06's cases.
- **Gate 4.** Three `RandomAgent`s on the arena, each with a city, research, build, buy, edit their queues, rename cities and adopt policies for a hundred turns with every check (the cache oracle, the `Buildable` memo's and the requirements' included) clean after every settle, and every city's list agreeing with `is_buildable` item by item at the end.
- **Criterion** (laptop, the late fixture's largest city, pop 41: 14 units, 5 buildings, no wonder buildable; `cargo bench -p citar-bench --bench buildable`, own medians, after the fix round): a memo hit 25 ns; a recompute 12.7 µs (39.5 µs before the hangar's room moved into the memo and the cheap reasons came first, 16.9 before only the uniques that may reject were asked); the first read after a change to another civilization 0.36 µs; the lists of the 12 other cities of the largest civilization after one of its cities changed (`Game::city_change_for_bench`) 5.8 µs, all of it validation (256 µs when every list read its siblings' `core`).
- **Left to later packages:** `auto_pick_production` (1c-07); triggers (1b-08); a camp's ownership when a border takes a tile (1c-06). The units a border's new tile may not hold are sent off by `movement::teleport_to_closest` since the merge with 1c-02. A requirement's local conditional (`in cities without a [Nuclear Plant]`) records `TILE`, which reads where the city's citizens work, so the city's own list recomputes after its citizens move once such a building's tech is known; a class for the city in context alone would remove it.

**As built in 1b-08** (§5.9, §5.12, §6.2, §6.5, §6.11, §6.14, §8.3, §9.2, §9.3, §9.7):
- **Files.** Engine: `game/religion.rs` (a city's followers and majority, pressure and conversions, holy cities) with `game/religion/{found, prophets}.rs` (pantheons, founding and enhancing, the beliefs to choose; what pantheons and prophets cost and the prophets faith brings), `game/derive/religion.rs` (`CityNeighbours` and each city's spread), `game/great_people.rs`, `game/triggers.rs`, `game/ruins.rs`; the actions `FoundPantheon` and `ChooseGreatPerson` with their argument specs; `Known::preferred_great_people` (`great_people.py:214`). Stages S2 (great people, religion, the Maya), S4, E1, E3 (faith, great person points) and E6 (golden-age progress), and the setup stage `starting triggers`: `inspect` `pending` lists 21 turn stages and 4 setup stages, and `cargo xtask check` counts 16 `NotPorted` and 57 `Pending`. Testkit: `tests/engine/religion.rs` and six rule scripts (`religion_pantheon_then_religion`, `great_people_free_choice`, `golden_age_start_and_end`, `ruins_rewards`, `triggers_babylon_writing`, `triggers_timed_strength`). Refcheck: `city_stats`' `pressure_in`. Bench: `citar-bench/benches/religion.rs`.
- **Pressures keep the order they arrived in.** `City::set_pressure` and `add_pressure` keep an entry in its place and put a new religion last, and an entry at 0 keeps its place, as Python's dict kept its keys; the converter keeps Python's order and `validate` refuses a religion listed twice where it refused an unsorted list. So which religion a citizen left over after the division goes to, and which of two religions with as many followers is the majority, follow Python, and `religion-ties-by-founding-order` is gone (the corpus shows no difference without it). The golden `convert.json` was blessed again for the two committed states whose cities list a religion before another founded earlier.
- **Pressure** (`religion::pressures_from_surroundings`, stage E4's `city_end_turn`). `derive::religion` keeps the cities in buckets of ten columns and rows (`CityNeighbours`, valid while `revs.cities` stands) and three memos per city. Its neighbours (`with_near`): the other cities within the farthest any religion could reach, by id, each with its distance, valid while `revs.cities` stands and the reach is the one they were found for. The major religion it follows (`major_religion`): its majority when that is a full religion and religion is in play, on its `religion` and `core` revisions, the religions and the settings, so recomputed once a turn in nearly every city, as pressure arrives in nearly all. Its spread (`with_spread`, `SpreadSource`: that religion, its reach and its natural pressure's multipliers with the cities each holds for), with the classes its computation recorded, which validates on the stamp of the major-religion memo (not on the city's `religion` and `core` revisions: pressure rarely changes which religion a city follows), its `buildings`, its owner's and that religion's founder's `index`, the religions, the settings and its recorded classes; the indexes' resource layers are read only for a ruleset whose resources carry a unique a spread reads. The reach (`reach`: ten, and whatever `Religion naturally spreads to cities [n] tiles away` could add, whatever its conditionals, a memo on the civilizations' `index` revisions; the shipped enhancer Itinerant Preachers adds three) is clamped to the map's width and height together, and ranges and pressures are summed without overflowing whatever a ruleset's amounts. A city reads its neighbours, passes over those that follow no major religion before their spread is read, and adds the pressure of each within its own reach, in one write of the city. The cache oracle checks the grid, the reach and each city's three memos; a test holds the grid to Python's walk over every city on the 12 committed states and the 250 of the corpus.
- **Conversions.** A new majority is announced to the city's owner, and the first time a city adopts a religion its founder has its `[stats] when a city adopts this religion for the first time`. As in Python, the majority is compared around each write of pressure: a city whose growth alone tips it (the citizen is counted before the pressure the growth adds) is not announced.
- **Religion** (`religion.py:331-706`). `found::plan_pantheon`/`apply_pantheon` (the tool `found_pantheon`): with faith for a first pantheon, else with a free pantheon belief; a first pantheon is a religion of its own named by its belief, whose 200 pressure a citizen takes the civilization's cities. `plan_religion`/`apply_religion` and `plan_enhance`/`apply_enhance` read and write apart, and take the tile a great prophet stands on and a closure that spends it, called where Python consumed the unit (before `upon founding a Religion`), which the unit actions of package 1c-04 pass; the checks write nothing, where Python owed a pantheon belief after a refused founding (`refused-founding-owes-nothing`), and refuse a belief listed twice (`belief-listed-twice-refused`), which Python took as filling two slots, and a first pantheon or a religion in a game that holds 256 already (a `ReligionId` is a byte), so that an apply never pays for what it cannot found; a pantheon is made before it is paid for. `beliefs_to_choose` counts a founder (an enhancer), a pantheon belief a civilization without one owes itself, a follower, `May choose [n] additional [kind] beliefs when [founding/enhancing] a religion` and its variant of any kind, and the free beliefs. `prophets`: a pantheon's faith, the religions a game allows and has room and beliefs for, the prophet's cost (`200 + 100 n(n+1)/2`, scaled), and stage S2's prophet, with a chance of `(5 + faith - cost)%` drawn from `Purpose::Prophet` keyed `[turn, player]`, born in the holy city its civilization holds, else in its capital. Stage E3 banks the turn's faith. `ai_choose_beliefs` weighs beliefs by their AI weights, then takes them in the ruleset's order, where Python took them by name (`ai-beliefs-tie-by-ruleset-order`). The spread actions (`religion.py:709-775`) are package 1c-04's, built on `add_pressure`, `remove_all_except` and `protected_by_inquisitor`; conquest (1c-03) calls `remove_unknown_pantheons`.
- **Great people** (`great_people.py`). `city_gpp` in fixed point with the city's bonus and `[great person] is earned [n]% faster`; stage E3 adds each major's cities' points; stage S2 births every great person whose points reach their pool's threshold (100, doubling; combat points 200, rising by 50) in the capital, where `upon gaining a [unit]` fires once, as for any unit made in a city (Python fired it a second time, for every such unique whatever unit it named: `great-person-born-fires-gaining-once`); `add_combat_points` is for package 1c-03, and credits the civilization's own kinds of great person earned through combat (the Mongols' Khan in place of the general, nobody else's), where Python credited every such unit of the ruleset (`combat-points-for-the-civilizations-own-great-people`). `ChooseGreatPerson` (the tool `choose_great_person`) checks the capital has room before anything is written; `ai_choose_free` takes the first of the preferred kinds the Maya calendar allows, where Python picked among every kind and took nothing when the calendar refused it (`ai-free-great-person-within-the-calendar`); the check it shares with the tool takes a unit id, and only the tool's wrapper resolves a name. `maya_long_count` gives a free great person at each b'ak'tun's end, of the kinds not taken. Golden ages: `enter_golden_age` (ten turns, lengthened, told, `upon entering a Golden Age`), and stage E6 counts one down or piles up the turn's happiness toward the next, through `Game::set_golden_age_turns`. The great person actions are `plan_*`/`apply_*` pairs for package 1c-04 (`hurry_research`, `hurry_construction`, `trade_mission`) with `consume_unit`, which fires `upon expending a [unit]` once, where Python fired it twice (`expending-a-unit-fires-once`).
- **Triggers** (`triggers.py`). `triggers::fire` finds what fires for an event at a site and applies each; `triggers::apply` is `apply_one_time`, one arm per kind of `OneTimeEffect`, keyed `Purpose::Trigger` by `[meta.key, civilization, tile, turn]`, with the site found as Python found it (the tile of the city or unit, the city of the tile when the civilization owns it); `on_gain` applies what a tech, era, policy, building, belief or the nation and global uniques at a game's start give once. The sites the earlier packages left are filled: a tech learned and each era entered, a policy adopted, a building added, a city founded (the settler is 1c-04's to pass), a unit made in a city, war declared and peace made, and the turn's start and end. Effects that reach systems ported later do the least of them: a spy recruited or promoted, the next world leader vote, the city-states' first great-person gift; a unit's heal, damage, experience, promotion, movement and destruction. Since the merge with 1c-02 what a one-time effect does to a unit is its `units::health::apply_unit_effect` (a free upgrade included), a promotion given free (`Promote all [units]`, a city's starting promotions) is its `units::promotions::add_promotion` with the promotion's own one-time effects, and a new unit is made by its `units::add_unit_in_city` and `units::place_unit_near`, which give a religious unit its religion (`religion::on_unit_made`). A triggered timed unique fires on its trigger alone (`unique::trigger::fire`), its other conditionals being its effect's, as a timed unique a source gives is granted. One-time effects nest eight deep at most (`triggers::TRIGGER_DEPTH`, a counter on `Game` that `apply` keeps): a ruleset whose effects feed themselves, which Python recursed on until it raised, is stopped there and reported as SETTLE-1 where the checks run (`trigger-chains-stop`). `Adopt [belief]` takes a belief nobody holds when it fits the civilization's progress: a pantheon or follower belief once it has a pantheon, a founder belief only for a religion without one, an enhancer belief only for an enhanced religion without one, so that a pantheon never turns into a religion by a belief. A new spy is the first `Agent n` no spy is called, the numbers taken read once.
- **Ruins** (`ruins::enter`): the ruins are cleared; the rewards possible (not the last two found, not one the game's difficulty excludes, those whose `Unavailable` and `Only available` allow it) each as many times as its `weight` (`RuinDef::weight`: 1 unless the ruleset says, 0 for never, refused at load outside 0 to 65535, as Python and UnCiv weighed them), are shuffled from `Purpose::Ruins` keyed `[tile, player]`, and the first that does anything is found, remembered and announced; a reward that did nothing is not tried again at its other places. Movement (1c-02) calls it; the test operation `enter_ruins` stands in.
- **What differs from Python, on purpose** (in `tests/rules/intended.toml`, cited at each fix): a timed unique is granted whatever its conditionals, which are its effect's (`timed-uniques-granted-whatever-their-conditionals`: the Autocracy finisher's strength was never granted); `Adopt [belief]` adds the belief to the civilization's religion (`adopt-a-belief-joins-the-religion`); `Adopt [policy]` adopts it whatever it requires, as UnCiv does, where Python raised out of the trigger (`adopt-a-policy-whatever-it-requires`); `Can speed up the construction of a wonder` hurries a wonder (`hurry-wonder-construction`); and the eight of the fix round named above (`great-person-born-fires-gaining-once`, `expending-a-unit-fires-once`, `combat-points-for-the-civilizations-own-great-people`, `ai-free-great-person-within-the-calendar`, `refused-founding-owes-nothing`, `belief-listed-twice-refused`, `ai-beliefs-tie-by-ruleset-order`, `trigger-chains-stop`).
- **Inspect and test operations** (both engines): `religion` (a player's religion, beliefs and costs; a city's majority, followers, pressures and holiness) and `great_people` (points, free great people, golden ages, the uniques held for some turns); `found_religion`, `enhance_religion` and `enter_ruins`, which stand in for the unit actions and the moves of packages 1c-04 and 1c-02; the prophet's stand-in lives in `api::testops` alone, so that no engine code calls it by mistake.
- **Kitchen sink** (`tests/engine/religion.rs`): every kind of one-time effect applied on the arena, each unit effect among them, with every check clean after (gate 4); the triggers `upon founding a Pantheon`, `upon founding a Religion`, `upon enhancing a Religion`, `upon gaining a [unit]` (once), `upon entering a Golden Age`, `upon turn start` and `upon turn end` (each exact, the turn's own yields taken out), `upon adopting [policy]`, `upon adopting [belief]`, `upon founding a city`, `upon declaring war on [City-State] Civilizations` (on the arena with a city-state), `upon entering a war`, `upon being declared war on` (its timed `[+10]% Strength <when attacking> <for [10] turns>` granted while nobody attacks) and `upon signing a peace treaty` fired where their events happen; `Adopt [belief]` accepted for a follower belief and refused for a founder's; `May choose [n] additional [kind] beliefs`; `Can speed up the construction of a wonder`, which hurries a wonder and not a building; a ruin weighing 3. On overlays of it: a great person born from points firing `upon gaining` once, a promotion that gives itself stopped at the depth, ruins drawn by weight (and a negative weight refused at load), and a ruleset's largest amounts overflowing nothing (an amount is at most a million, but 1,100 copies of the largest reach and three of the largest multiplier take a range and a pressure past an i32, and a city's growth by `i32::MAX` citizens its pressure and `add_population`).
- **Refcheck.** `city_stats` is enforced whole: `followers`, `majority` and `pressure_in`, clean on the committed states and the corpus; its ratchet falls from 160 to 0. The `civs` group records no religion or great-person path (its missing paths are score, military strength, victory progress and the world, package 1c-08), so there is nothing more of it to enforce; its religious yields (the `Religion` lines of happiness and stats, founder beliefs in the unique index) were enforced already.
- **Criterion** (laptop, other packages building beside it; `cargo bench -p citar-bench --bench religion`, own medians): a gargantuan pangaea (160 by 100) of twelve civilizations with a city on every site, 399 cities, six religions founded, every city given a thousand pressure a citizen toward the religion of the holy city nearest it and five rounds played, so that all 399 follow a religion, as late in a game: one round of pressure, every city's religious turn in id order, 0.23 to 0.30 ms over two runs (Criterion's 0.31 to 0.57 ms; budget 1 ms). The first cut measured a state where 23 cities followed a religion; on this one it took 1.2 ms, each neighbour recomputing the majority of the city it read (twice, the second in the spread's validation) and each city walking the grid's buckets. Every city's surroundings asked at a stable revision, 0.07 to 0.11 ms.
- **Left to later packages:** the spread actions and the great prophet's and great person's unit actions (1c-04); combat's experience and a conquered city's pantheons (1c-03: `units::promotions::add_xp` credits `add_combat_points` since the merge with 1c-02, which also calls `ruins::enter` from `movement::on_enter_tile`); the city-states' great-person gift tick (1c-06).

### 6.7 Settle, and pending work

```rust
fn settle(g: &mut Game) {
    sync_sight(g);                                   // dirty vision sources -> footprints -> transitions -> effects
    let mut passes = 0;
    while g.pending.recheck.any() && passes < SETTLE_PASSES { reassign_flagged(g); passes += 1; }   // SETTLE_PASSES = 8
    if g.pending.recheck.any() { g.report(Violation::SETTLE_1); g.pending.recheck.clear(); }
    debug_assert!(g.pending.is_empty());
    run_checks(g);
}
```

**What flags a city for a citizen recheck** (in `Game.pending`, never persisted):
- a `CityTouch::WORK` or `CORE` touch, or a change to the city's tiles;
- a change in the yield of a tile within the city's range;
- a change in which tiles are workable:
  - ownership;
  - an enemy military unit entering or leaving, which is the blockade (cities.py:170-193);
  - a sibling city taking or releasing a tile;
- `happiness_seen` crossing 0 or -8 at a commit;
- the sign of `last_gold_rate` changing at E2;
- a `Seat` change for the city's owner.

**How a settle reassigns.**
- A pass visits flagged cities in `CityId` order.
- A city whose assignment takes or releases a tile a sibling city could work flags that sibling. A sibling with a higher id is handled in the same pass, one with a lower id in the next.
- Within one settle, nothing else moves: `happiness_seen` and `last_gold_rate` are constants, and sight never depends on citizens. So the passes converge quickly.
- Hitting the 8-pass cap is invariant violation SETTLE-1 in checks builds. In release the flags are cleared and the assignment stands. That is still a pure function of the sequence of successful calls, because nothing is carried over.

**When settle runs:**
- at the end of every successful mutating call: `act`, `end_turn`, `apply_ops`, and host ops that write;
- at the ◆ points of §6.2.

It never runs after a refusal, a query, a view, a snapshot or a save. A settle with nothing pending costs 0.5 µs or less.

`run_checks` runs the invariants and `verify_caches` when `DebugOptions` asks for them (§9.4).

**As built in 1b-01** (§4.8, §5.11, §6.1-6.7, §8.3-8.5, §9.4):
- **Files.** `game/{mod, core, mutate, events, eval, pending, action, invariants, debug, error, query}.rs`, `game/derive/{mod, rev}.rs`, `game/turn/{mod, settle}.rs` and `game/vis/mod.rs`, with unit tests beside them (`game/{action, events, invariants}/tests.rs`, `game/derive/rev/tests.rs`, and `core::testing`, a small two-major game on the embedded ruleset). The fixture gate is `crates/citar-testkit/tests/engine/game.rs`.
- **`Game`** has the fields of §6.1 plus the journal cursor and the violations the checks found (`take_violations`), `pub(crate)` for the rule systems. `Game::load` wraps `save::load` and starts the cursor at the end; `Game::from_state` (states built by hand) and `Game::from_python` start it at 0, since their history is in no journal yet, so the first chunk carries all of it. `from_state` runs `save::validate` first. The chain stays the host's (`DigestChain::resume`). `Game` is `Send` and not `Sync` (asserted in `lib.rs`).
- **Revisions** (`derive::rev::Revs`): per tile (`tile`, `tile_owner`, `tile_height`) with a bounded `TileChangeLog` (4096 entries; asked about an older revision it says so, and the cache rebuilds in full); per civilization `CivRevs { index, stocks, research, happiness_seen, gold_rate, seat, units, cities, buildings, religion, city_state, spies }`; per city `CityRevs { core, buildings, work, stocks, religion }`; per unit `UnitRevs { core, moves, place }`, both kept sparse in a `LookupMap` by id (an id may be as large as `MAX_ENTITY_ID`, and a table grown to it would pass the 64 MB bound of §4.8; a removed entity keeps its entry); and the global `owners, routes, worked, cities, city_core, city_buildings, city_religion, unit_pos, units_core, diplo, influence, talks, alliances, policies, religions, wonders, world, names, turn, clock, config`. Every write takes a new revision (`Revs::next`); `Revs::on_change` maps each `Change`, and the touches map their flags.
  - **What moves a civilization's index** (the inputs of `CivIndex`, `economy.py:91-128`): its techs, era and temporary uniques (`PlayerTouch::INDEX`), policies (`POLICIES`), religion (`RELIGION`, founder beliefs), seat, the buildings of its cities (`CityTouch::BUILDINGS`, a city event), and its city-state bonuses (`city_states.bonus_umaps`): an ally change, contact or war with a city-state, a city-state's death, and influence that flips its friend level (`Game::set_influence`). Nothing that moves every turn moves it: growth, a queue edit or a heal is `CityTouch::CORE`, which moves the city and `city_core` only; influence below or above the level moves `influence` only; a tech moves no other civilization's revisions.
  - **Relations** report what moved (`Diplomacy::update`): `War` when war broke out or ended (it moves `diplo`, and flags the cities a military unit of the other side stands in range of, the blockade), `Diplo` for a friendship, a pact or open borders (`diplo`), `Talks` for bookkeeping no cache reads (treaty terms, research agreements and their science, embassies, denouncements; `talks`). `State::set_clock` reports `Turn` only when the turn number moves, and `Clock` otherwise, which moves only `clock`.
- **`Revs::cond(st, deps, ctx)`** maps every class: `Revs::cond_civ` the civilization-level half for `ctx.civ` (with no civilization in context those classes read nothing that can move), and the local half through the state: `CITY` the revisions of `Ctx::rel_city` (found from the state: the city in context, our side's in a fight, else the territory's city when the civilization owns it) and the tile's owner revision; `UNIT` the unit's; `TILE` the tile's inputs, owner and its city's `work`; `COMBAT` both sides and the attacked tile; `MAP` the tile log, routes, owners and worked tiles; `CHANCE` the turn; `WAR` the relations and influence (the friendly civilization and land leaves, which read influence, read every class); `RELIGION_STATE` and `CITY_COUNT` any city's religion too, since a city filter over the counted cities reads its majority religion and holiness; `GLOBAL_BUILDINGS` any city's buildings; `GLOBAL_POLICIES` any civilization's policies and religion (beliefs count as adopted). `RESOURCES` reads everything the supply is computed from until package 1b-05 makes the supply a memo of its own. A class added to `CondDeps` without a line reads the current revision, which is always correct. A test moves every class with a write that must move it.
- **Memos.** `Memo<T>` holds its value mutably borrowed for the whole validation, so a read of it from inside its own validation panics on the `RefCell`; `CopyMemo<T>` has a busy flag that a drop guard clears, so a panicking compute leaves it usable. A value's first compute always counts as a change. `BitEq` compares floats as bits (`-0.0` differs from `0.0`, a NaN equals itself). Cloning a stamp copies it: a cloned game (`apply_ops`) keeps what was verified, and each moves on with its own revisions. With `stats`, each stamp counts hits, validations and recomputes.
- **`Derived`** holds the revisions, the grid, the event name index (a memo on `names` and `cities`), the visibility sets (empty until 1c-01; `reveal_for_test` under `test-ops`) and an empty index. `Derived::on` returns `Reactions`: the cities to flag (a city may work any tile of its owner in its range that no city stands on, its own city does not work and no enemy military unit blocks, `cities.py:170-193`; so a tile whose yield, owner, city or enemy occupant changed flags the cities of its owner, before and after, whose range reaches it; a seat flags all its player's cities, and war or peace the cities of each side in range of a military unit of the other side in its land; no other term of a relation flags any) and the vision sources to mark (`pending::SightSource`). `flag_near` walks the owners' cities, so the write path allocates nothing. The effect queue holds `Effect::Meet`, drained in `(kind, a, b)` order, once each. Effects are idempotent per key, so one settle applies at most a few per tile per civilization and per pair: `EffectQueue::limit(tiles, players)` (8 per tile and civilization, 8 per pair, twice over, at least 2^16) is the runaway bound, so a whole map revealed on the largest map is far below it.
- **Writes** (`game::mutate`): a wrapper for every setter of `Tiles`, `Units`, `Cities`, `Diplomacy` and `State`, the touches `city_mut`, `player_mut`, `unit_mut` and `edit_world`, `edit_diplo` (`WorldTouch`, `DiploTouch`; not `world_mut` and `diplo_mut`, which `cargo xtask check` reserves for the state's accessors by name), and `edit_config`, which starts every cache cold with every revision past the old ones, and flags every city for a citizen recheck and every player's sight, as a seat does for one player. `CityTouch` adds `BUILDINGS` (the building set; implies `CORE`) to the flags of §6.4, and `PlayerTouch` adds `POLICIES`, `RELIGION` (with the great prophets earned), `CAPITAL` and `OTHER`. Influence changes only through `Game::set_influence`: a `CITY_STATE` touch moves no major's index, and a port that wrote influence through one would leave a flipped friend level stale, which the cache oracle reports once `CivIndex` is a memo (1b-05). `Change` is `Copy`, so no drop check is possible; `_ =`, `let _x =` and `drop` stay review items.
- **Settle** alternates `sync_sight` and the effect queue until both are empty, then runs up to 8 citizen passes; flags left after the eighth are dropped and reported as SETTLE-1 when invariants are on. `sync_sight` (1c-01) and the reassignment (1b-06) are `Pending` no-ops that clear their work. The rule of first contact (`game.py:694-701`) is `Game::make_contact`, which the settle applies for `Effect::Meet`; the name `meet` stays free for the host command of §8.1, which wraps it with `begin_call`, a settle and `take_batch`.
- **Events.** `Game::emit(kind, text, audience, tile, data, mentions)`; `emit_host(kind, text, audience, data)` and `add_thought(pid, text, kind)` count in the host heads only, and take a `PlayerSet`, typed `EventData` and an optional kind (the tool registry, 1d-01, maps the host's JSON onto them). Empty event data is stored as `None`, as the converter does. `event_view` returns `EventOut { event: Cow<Event>, unknown: PlayerSet }`: the UN tally keeps its player ids, and `unknown` names the ones a view must show as unknown; an unknown winner is cleared. `events_for` is `game.py:880-896`.
- **The evaluator's view** (`EvalView`) answers from the state and the ruleset, porting the accessors of `game.py` (`is_friend` with `city_states.is_friend_level`, whose threshold is `city_states::influence::FRIEND_INFLUENCE` since 1c-06), `tiles.fresh_water` (one set test per tile against `rules::Derived::fresh_water`, the terrains carrying `Fresh water`, marked at load), `resource_visible` and `is_friendly_territory`, `movement.is_embarked`, `cities.is_garrisoned` and `has_annex_unhappiness`, `policies.completed_branches`, and `religion.civ_beliefs`, `is_major` and `is_enhanced`. The rest are `pending_or` markers answering what a game without the system would: the four unique indexes (empty) and resource amounts (1b-05), coast and the trade network (1b-06), the era (the starting era; 1b-07), religious majorities (1b-08); and `difficulty` leaves the `monotonic` AI base values to 1b-05, which needs Prince among the known objects.
- **Porting markers.** `game::Porting { Ported, Pending(&str) }` is the type 1b-03's stage tables use; `pending(Porting::Pending("<pkg>"))` and `pending_or(Porting::Pending("<pkg>"), value)` mark a step or a value inside a ported function, and `cargo xtask check` counts them like stages.
- **Actions.** `action::Rule { type Plan; check(&self, &Game, pid); apply(self, &mut Game, pid, plan) -> OutcomeSpec }`; `OutcomeSpec` is a closure rendered on the settled game; `Outcome` is `serde_json::Value`. `Game::guard` checks, in `tools.execute`'s order, a poisoned game, a living major, a game that goes on, and the turn unless the tool is `any_time`, with Python's messages. `Action` is a serde enum tagged `tool`; it has no variants outside tests until the systems add theirs. `act` logs an action that succeeds as an `ActionRecord` (`tools.py:129-130`) through `journal::Record`, after the settle: the turn it was taken on (Python logged the turn after the call), the tool, and the action's fields as compact JSON without the tag. It is logged here and not in `execute` (1d-01), since bots and drivers call `act` directly and Python logged theirs too; the record counts in the host heads, never in the digest.
- **Errors** live in `game::error` (the `api` layer re-exports them): `ErrCode` adds `Poisoned`; `EngineError` adds `State(StateError)`.
- **Invariants** (`game::invariants::check`, `Game::check_invariants`) are always compiled and run at every settle when `DebugOptions::invariants` is on (the default in debug builds and with `checks`); `CACHE-1` reports the cache oracle (`Game::verify_caches`). Two bounds wait for their systems: a unit's maximum moves and the stacking rules (1c-02) and a city's maximum health (1c-03). CITY-2 checks what a city may work (tiles of its owner, in its range, no city centre, worked by no other city) only for cities whose citizens the engine has settled (§6.8): converted cities keep Python's worked tiles, and Python let a city work a sibling's tile, and two cities work a tile neither owned (`cities.py:170-193`).
- **Loading Python states.** `Game::from_python` gives a unit Python left at 0 health 1 (a civilian a ranged attack reached; `combat.py:109, 806`), which state_echo shows as the intended `civilians-at-zero-health`; the happiness commit waits for 1b-06. Every mini, late and corpus fixture loads with no violation and caches equal to a cold rebuild.
- **Refcheck** loads each fixture with `Game::from_python` inside `run::guarded` and hands the game to every group as `Ctx::game`.
- **Python lines left to their systems:** `find_spawn_tile` (`game.py:787-794`) needs `movement.can_stand` and lands with the starting units (1c-02); a new unit's promotions and its moves when made on its owner's turn (`units.on_created`, `movement.max_moves`) are `Pending("1c-02")` inside `Game::create_unit`.

### 6.8 The citizen oracle and converted states

The oracle asserts, for every city, that `worked == greedy(inputs)`. Refcheck, however, needs converted Python states to keep Python's worked tiles, or every `city_stats` answer would differ.

**Decision (a persisted flag).** `City.citizens_settled`:
- the converter sets it to `false`;
- the engine sets it to `true` the first time it assigns that city's citizens.

The oracle checks only cities with `citizens_settled`. When chaos or soak runs start from fixtures, testkit flags every city first.

### 6.9 Incremental visibility

This replaces `visibility.py:96-168`. There, every unit step recomputed every civ; that was 54% of gargantuan time, and 98.5-99.5% of the results were unchanged.

```rust
pub struct Visibility { civ: PlayerVec<CivVis>, sources: LookupMap<SourceKey, VisSource>, los: LosCache }
pub struct CivVis { count: Vec<u16>, visible: BitSet }
pub enum SourceKey { Unit(UnitId), City(CityId), AllyCity(PlayerId, CityId), Spy(PlayerId, u8) }
pub struct VisSource { owner: PlayerId, footprint: Box<[TileIdx]> }   // sorted, an owned copy
```

**Sources and footprints:**
- **A unit** sees the ring-by-ring elevation walk of `visibility.py:34-66`, with its sight radius from `units.sight` (ported here, in 1c-01). `CanSeeOverObstacles` gives the plain radius.
- **A city** sees its owned tiles plus one ring (visibility.py:99-104). Its footprint changes with its borders (`Change::CityTiles`).
- **A city-state's ally** sees the ally's city tiles (without the ring), and a city-state allied to another city-state sees likewise (visibility.py:107-111).
- **A set-up spy** sees its city's tile and one ring (espionage.py:444-451). It is registered or moved by `Change::Spy`.

**Updates.**
- `set_footprint` increments the counts on the new tiles, then decrements the old ones, and collects the 0→1 and 1→0 transitions. Only the owner's counts change.
- Sources are updated in `sync_sight` at settle, from `pending.sight`.

**Effects of a transition, sorted by tile:**
- 0→1 sets the explored bit, checks first contact with the tile's owner and with the owners of units there, and finds natural wonders;
- 1→0 takes a memory snapshot (majors only).

**First contact.** Python rechecks every visible tile on every refresh (visibility.py:151-165). Rust gets the same result from four triggers:
1. a 0→1 transition on tile t: the viewer meets t's owner and the owners of the units on t;
2. a unit arriving on t, or changing owner (`UnitPlaced`, `UnitOwner`): every civ with `visible[t]` meets the unit's owner. That costs 56 bit tests;
3. an owner change on a visible tile (`TileOwner { new }`, `CityAdded`, `CityOwner`): every civ with `visible[t]` meets `new`. This covers border growth, `buy_tile`, a city founded in sight, a culture bomb, a capture and a liberation, which the earlier draft missed;
4. a player revived (`PlayerAlive`): its sources are re-registered, and civs that see its tiles and units are rechecked.

The exclusions are Python's: the viewer itself, barbarians, pairs already met, dead players, and two city-states. A property compares the incremental met sets with a full recompute by Python's rule after random moves, border changes, captures and liberations.

**Enemy spotted.** `move_toward` receives the owner's 0→1 list after each step, and stops if an at-war military unit is now visible (as `movement.py:628-640` does).

**Invalidation.**
- A `TileHeight` change evicts `LosCache` entries within 6 tiles and re-registers the units standing there.
- A `SightMods` change marks the civ's unit sources dirty.
- On load, the counts are rebuilt from scratch.

**Event audiences.** Widening a tile-anchored event's audience (`game.py:872-875`) is one bit test per major. It applies only to non-public events that are not private (§8.4).

**As built in 1c-01** (§6.4, §6.5, §6.7, §6.9, §6.14, §9.2):
- **Files.** `game/vis/{mod, los, visibility, sight, effects}.rs` with the unit tests and the gate-2 property in `game/vis/tests.rs`; the refcheck modules `answer/{visible, fixed_point}.rs`; `crates/citar-testkit/tests/engine/vis.rs` (the kitchen-sink extras); the scripts `vis_first_contact_border_growth`, `vis_first_contact_both_ways`, `vis_line_of_sight`, `vis_natural_wonder` and `vis_city_state_contact`; `crates/citar-bench/benches/vis.rs`.
- **Who sees.** Only a living player that is not the barbarians has sight, as in Python's `refresh`. Every such player explores what comes into sight (city-states too, as §4.3 persists it); only a living major remembers what leaves it, so a player's sight that dies with it leaves no memory.
- **`Visibility`** holds, per player, `count: Vec<u16>` (allocated on its first source) and `visible: BitSet`; the sources in a `BTreeMap<SourceKey, VisSource>` (the oracle, a revival and the ally and spy sources walk it in key order, so it is ordered rather than the `LookupMap` of the sketch above); the tiles' two heights; the `LosCache` in a `RefCell`, so that `has_los` reads through `&Game`; the ruleset's `SightRules`; and the tiles that came into each player's sight since the last settle. `VisSource { owner, at, sight: Option<Sight>, footprint: Arc<[TileIdx]> }`, where `Sight` is `Blind` (`No Sight`), `Clear(r)` (`Can see over obstacles`) or `Walk(r)`. `Visibility::set` counts a new footprint before it takes the old one away and reports each count that left or reached zero.
- **Line of sight** (`vis::los`). `Heights` keeps each tile's standing and blocking heights, read again for one tile at a `TileHeight` change, at once, in `Game::changed` (with the eviction below), so `has_los` and `unit_viewable` read the terrain as it is even between a write and the next settle. The walk keeps, per tile of the map, the generation of the ring that reached it and the height seen on the way (a `LosScratch` the cache owns), where Python kept a dict; a unit test holds it to a direct port of Python's walk on random heights, wrapping maps included. `LosCache` is a `DetMap` keyed `(tile, radius, attack)` that drops an answer when a tile within its radius plus one of its centre changes height (eagerly, at the change), and starts again past 65,536 answers.
- **Dirty marks** (`pending::SightSource`), from `Derived::on` and the touches: `Unit` (placed, changed hands, removed, and the `CORE`, `BASE` and `SIGHT` touches, since promotions, health and status are what sight uniques and their conditionals read), `City` (added, removed, changed hands, its tiles, a tile of it changing hands), `Tile` (a tile changing hands, a city's tile when the city appears or goes, and a tile's inputs when the sight uniques read tiles: the units on it are looked at again, since whether a city stands there decides whether they are embarked), `Area` (`TileHeight`: the heights and the cache are already updated; the sync looks again at the units that walk near the tile and at a natural wonder on it), `Allies` (`Alliance`), `Spies` (`PlayerTouch::SPIES` and `Change::Spy`), `Civ` (`PlayerAlive`, `edit_config`, the test operation `refresh_visibility`, and a game built from a state or converted), and the new `Contact(p)` (a `Met` that leaves the pair unmet: two players who forgot each other meet again at the next settle if they see each other, as Python's next refresh met them; a meeting makes no other pair meet, so it marks nothing).
- **Sync.** `Game::sync_sight` checks the civilizations' sight uniques, takes the marks, works out every new source from the game as it is (the game's own `Visibility` stays in place, so nothing a source reads can see a half-built one), registers them, nets the transitions over the whole sync, writes explored tiles and memories at once (one `PlayerTouch::OTHER` per player), and queues `Effect::Meet` and the new `Effect::Wonder { civ, tile }`. `Game::settle_sight` alternates it with the effects to a fixed point; `settle` is `settle_sight`, then citizens, then it forgets the tiles newly seen, then the checks. `emit` syncs first when it widens a tile's audience and sight is dirty, as Python's `visible_tiles` refreshed. `edit_config` keeps the counts, so the settle compares the new settings' sight with the old.
- **Sight mods.** `SightRules::new(rules)` reads the ruleset's `[n] Sight`, `No Sight` and `Can see over obstacles` once: whether a civilization-wide source (anything but base units, unit types, promotions and terrains) or a resource carries a `[n] Sight`, the civilization-level classes and whether the local ones beyond the unit their conditionals read, and the terrains that carry one. At each sync at a new revision, for each civilization with units, the revision of what it gives its units' sight (its `index`; its supply's stamp when a resource carries one; `Revs::cond_civ` of those classes; the current revision when they read a city, a fight or the map) is compared with its stamp; when it moved, its `[n] Sight` entries of `CivIndexFull` (`SightMods`) are read again, and its units are marked when they differ or a conditional is civilization-level. A unit's sight is then worked out from its profile, those entries and its tiles' terrains (`sight::sight_with`, which reads the entries and the `SightRules` from the `Visibility` it is given: the game's in a sync, the new one in a rebuild), with no civilization memo on a step; `vis::verify` holds it equal to `sight_of`, which asks the index. In the shipped ruleset, the Great Lighthouse, America and Polynesia carry a civilization-wide `[n] Sight`, and every sight conditional reads the unit alone. The property found that a city founded under an embarked unit changes its sight; that is why a city's tile is marked.
- **First contact** is Python's rule (`may_meet`: not itself, the barbarians, the dead, one already met, or two city-states) checked on a tile coming into sight (its owner and its units' owners), on a unit marked (those who see its tile meet its owner), on a tile or city changing hands (those who see it meet the owner), and for `Civ` and `Contact` on everything the player sees and everything of its others see. Each pair found in a sync is queued viewer first, `Effect::Meet { a: viewer, b }` with the lowest viewer that saw the other. Python's refresh met them viewer by viewer in id order, `g.meet(viewer, q)`, so the queue's `(a, b)` order is Python's, and the announcement names the one that saw first. Once 1c-06 ports `city_states.on_meet`, the order also decides which major a city-state greets first. `Effect::meet(a, b)` (the lower id first) stays for a meeting with no viewer.
- **Natural wonders.** A major discovers a wonder on a tile coming into its sight, on a tile whose terrains changed while it sees it, and in a whole-player look; `Game::discover_wonder` applies it (the first discoverer's grant, every discoverer's `[stats] for discovering a Natural Wonder`, Python's text and audience). The count is read by civilization stats (`economy.py:636-641`), so the discovery touches `PlayerTouch::STOCKS`. Stats land through the new `Game::add_stat` (`game.py:636-650`): gold, culture, faith and golden-age points; science is `Pending("1b-07")`, since it needs research's progress rules.
- **Loading and setup.** `Game::load` rebuilds the counts from the sources with no effect (`rebuild_sight`); `from_state` marks every player and settles sight, so a state built by hand explores what its units see and meets whom they see; `from_python` does the same inside its settle. Setup's `visibility` stage is ported: `settle_sight` before `begin`, so first contacts are announced before the game starts, as Python's `refresh` ran before `begin_turn`.
- **For later packages.** `vis::{sight, sight_of, has_sight, unit_viewable, unit_visible_to, has_los, enemy_spotted, verify}` and `Visibility::{sees, visible, seers, count, source, sources, line_of_sight}` are public. Package 1c-02's `move_toward` runs each step through `Game::step_seeing(owner, |g| step)`. It settles sight and drops what came into view earlier (Python built `seen_before` right before the step, after a refresh), runs the step, settles sight again (Python's `refresh`, `movement.py:633`) and returns the tiles the step brought into view, for `enemy_spotted`. Python compared enemy unit ids rather than tiles, which differs only when the step itself changes who is at war. Combat (1c-03) calls `settle_sight` where Python refreshed (`combat.py:792, 870, 1140`), and before `unit_visible_to`, whose `sees` is as of the last sync; `has_los` and `unit_viewable` read the terrain and the unit's place as they are, with no sync. Map trades, ruins and embassies (1b-08, 1c-05) call `Game::reveal_tiles(p, &tiles) -> u32` (`visibility.py:232-243`: explored, and a major remembers what it does not see). `Game::step_unit_for_test` (feature `test-ops`) is the benchmark's step.
- **The oracle.** `Game::verify_caches` adds `vis::verify`: the counts, sources and heights against a rebuild from the state, every answer the line-of-sight cache holds against a walk now, each unit's sight against the index, and Python's rule over what each player sees (every owner it sees met or excluded, every wonder a major sees discovered). `Derived::verify` no longer compares sight. Gate 2 is `game::vis::tests::incremental_sight_is_a_rebuild_by_pythons_rule`: a populated 12x10 game (America, Polynesia, two city-states, the barbarians), then 10 to 50 random writes (units made, moved, removed and promoted, triremes among them; tiles claimed; cities founded, captured and razed; hills, forest, mountains and coast; the Great Lighthouse built and pulled down; alliances; deaths and revivals; forgotten meetings; spies set up and moving), with every check clean, `vis::verify` empty and each player's sight equal to a port of Python's `compute_visible` with Python's walk, after each, and every line-of-sight answer in the cache equal to a walk over the terrain before each settle; 64 cases in CI; it held for 2,000, and after the fix round for 2,000 more, then 1,500 with the check before the settle.
- **Kitchen sink.** Both extras of this subsystem are ported and tested with the kitchen-sink ruleset in `tests/engine/vis.rs`: `No Sight` (the Drone sees its own tile) and `Invisible to others` (the Sub, seen only by a unit that `Can see invisible [Submarine] units`), beside the shipped `Invisible to non-adjacent units`. No shipped terrain carries a `[n] Sight`, so the kitchen sink adds the Kitchen Sink Lookout, a feature with `[+1] Sight` that does not generate naturally: a unit stepping onto it, or standing on a tile it is put on, sees at 3, and at 2 again off it. Deferred: none.
- **Scripts and inspect.** `inspect` gains `tile.visible` (the players who see the tile) and `player.natural_wonders`, on both engines. The five scripts pass on Python and Rust. Two city-states never meeting is a unit test only: the arena has one city-state start, and the others need `maps.prepare` (1c-09).
- **Refcheck.** `visible` (each living major's tiles) and `fixed_point` (each player's explored tiles and meetings in the recorded state against the loaded game's, after the settle built sight from nothing) are enforced, with no difference on the 12 committed states or the 250 of the corpus.
- **Criterion** (laptop, other builds running; after the fix round). A unit's step at sight 2 with its sight brought up to date, against the 1.5 µs budget:
  - 0.74-0.75 µs with both footprints in the line-of-sight cache (`vis_step/sight2`, a unit pacing in place);
  - 1.36-1.40 µs with the cache forgotten before each step (`vis_step/sight2_fresh`: the walk and the new footprint's allocation, as on a step onto a tile no unit saw from lately).

  The walk at sight 3 uncached, 0.65-0.67 µs (budget 1 µs). The reviewer's run on a busier laptop measured 1.42-1.49 µs for the cached step and 1.03-1.09 µs for the walk, under the 3x limit.

### 6.10 Movement and paths

Python's `find_path` (movement.py:388-466) is a Dijkstra over `(turns, -moves_left)` whose passability depends on much more than terrain:
- explored tiles: an unexplored tile is passable at its true cost;
- visible foreign units, which block only in visible tiles;
- the unit's own units: it may not end its turn stacked with one of its kind;
- the target: the capture-a-civilian exception;
- foreign cities;
- territory: entry rights, open borders and war.

Folding these into a cached cost table would invalidate it on almost every step, so the design separates the static part from the dynamic part.

- **Static, cached per move class (`MoveCosts`).** A `MoveClass` is interned per (owner, movement profile flags, double-movement terrains, embark and disembark costs). For each class:
  - `base: Vec<u16>` is the terrain entry cost: double movement, rough penalty, ignored hills, the flat cost of a city tile, `0xFFFF` for impassable;
  - `pass: BitSet` holds `terrain_reason`'s static answer (movement.py:122-160): water, ocean, ice and mountains, by the class's techs and uniques;
  - the embark and disembark transitions.

  Tables are rebuilt incrementally from `tile_log`, or in full (about 60 µs on 16k tiles) when more than an eighth of the map changed. At most 96 tables are kept, least recently used first out.
- **Static, cached per civ (`RouteLayer`).** It holds the effective route per tile:
  - roads and railroads;
  - city centres by their owner's techs, as `route_at` does (movement.py:282-295);
  - forest and jungle roads on the civ's own tiles;
  - whether roads cross rivers.
- **Dynamic, checked per node in O(1).** Each is a bit test or an occupancy lookup:
  - `explored[t]` (barbarians know every tile). An unknown tile is passable at its true cost. **Decision:** Python's optimistic rule is kept, because paths through fog are how exploration happens (movement.py:388-395);
  - territory: `enterable: PlayerSet` per civ from diplomacy and the profile's `foreign_ok`/`cs_ok`, tested against the tile's owner;
  - a foreign city;
  - foreign non-air units in `visible[t]`, with the target exception for capturing a civilian;
  - the own-stacking rule when moves reach 0 (`stack_reason`, movement.py:208-240);
  - zone of control: the civ's `Zoc` bitset, with `zoc_between(a, b)` testing the tiles adjacent to both. Like Python, it counts every enemy military unit, visible or not;
  - the extra cost of entering at-war territory (`EnemyUnitsSpendExtraMovement`).

  Edge cost combines these in the order of `enter_cost` (movement.py:330-377).
- **A\*.**
  - **The key** is a `u64`: `((t as u64) << 32) | (u32::MAX - l as u64)`. It orders states exactly as Python's `(turns, -moves_left)` does, and cannot underflow when a unit's moves exceed its full moves (possible after `max_moves` drops). The earlier `t·(F+1) + (F−l)` could.
  - **The overdraw rule is kept:** a step costs `min(cost, l)` within a turn, and `1 + min(cost, F)` when it crosses a turn boundary.
  - **The heuristic** is the turn-aware lower bound built from the cheapest possible step of the class. It is admissible but not consistent, so a node can be reopened (lazy deletion on the g-score).
  - **Pruning.** A node is pruned when `t + n > max_turns`, where n is the lower bound on further turns.
  - **Scratch arrays** are reset by bumping a generation stamp, and the open set is `MinHeap<(u64 key, u32 tile)>`.
- **Decision (priority queue).** `base::collections::MinHeap` wraps the banned `BinaryHeap`, with the push sequence as the final tie-break.
- **Other searches:**
  - `PathTree` is a bounded Dijkstra from one unit, used by barbarian `_seek` (11 searches become 1) and later the bot;
  - `reachable_this_turn`;
  - `PathCache` keyed on `(unit, from, moves, target, rev.now)`, so reuse is exact.
- **Refcheck** compares turns, final moves left and step validity. It does not compare the tile list (`PathEquivalent`, §9.2).

**As built in 1c-02** (§6.10, §9.2, §9.4, §9.5, §10):
- **Files.** `game/path/{mod, class, memo, node, cost, astar, tree}.rs` with the unit tests and the gate properties in `game/path/tests.rs`; `rules/moves.rs` (the names movement reads, at load); `game/movement.rs`; `game/units/{mod, promotions, health, abilities, upgrades, capture, turn, actions}.rs` (the old `units.rs` is `units/mod.rs`); `game/triggers.rs`; the refcheck module `answer/movement.rs`; `crates/citar-testkit/tests/engine/units.rs` (the kitchen-sink extras); the bench `crates/citar-bench/benches/path.rs`; the scripts `units_move_order`, `units_embark_needs_optics`, `units_zone_of_control`, `units_roads`, `units_promotions`, `units_healing`, `units_upgrade`, `units_carrier`, `units_orders` and `units_carthage_crosses_mountains`, which pass on Python and Rust (the last two end with Rust-only steps, `intended`).
- **No per-class tables.** There is no `MoveClass`, `MoveCosts` or `RouteLayer` table kept per class or civ. A search takes a `Mover` (`path::class`): the unit's profile (`Profile`, `movement.profile`), its civilization's rules (`CivMove`), whom it is at war with, whose land it may enter (`PlayerSet`), its explored and visible sets and, on first use, its zones of control (`Zoc`, absent when none is exerted, so a step with no enemy about reads nothing). A tile is read as it is, a handful of loads (`Mover::look`: whether the mover may route through it, and the `Facts` its steps read: land, rail, connection, river edges, the war extra and the terrain cost), once per search: the scratch keeps what it found. Nothing per tile follows the map's changes, so there is nothing to rebuild or evict. The ruleset's names movement reads are `rules::moves::MoveRules`, resolved at load among the ruleset's derived tables (Python's compares with `All`, `Embarked` and `Air` happen there once).
- **The mover's memos** (`path::memo`, in the cache oracle). What a mover takes of the game is memoised, each memo checked against the revisions of what it reads, so building a mover costs a few reads while nothing it depends on moved (it cost 1 to 1.5 µs of unique queries before): per civilization, its `CivMove` with whose land it may enter (`CivParts`: its index with the resource layer, what the conditionals of the movement uniques read in its context, its roster, which the units it gained grow with, the relations, the cities, the turn and the settings); per unit, its `Profile` (its core revision, its owner's index, what the conditionals of the profile's types read in its context, and its place when they read where it stands; the table is emptied when it outgrows the units twice over); per civilization and for land units or the rest, its zones of control (the relations, the cities, where units stand, their core, and the tile log, since an embarked unit exerts none on land units). A unit's step moves only the last; the bench prints what `reachable` costs right after one, at war (about 3.5 to 4.5 µs).
- **Step cost.** `Mover::cost_from(a, d, fa, fb, zoc)` is `enter_cost` in Python's order (embark and disembark, zone of control, all-1, rail, road with the river rule, ignores-terrain, crossing, terrain), given the direction; `edge_cost(a, b)` wraps it. A zone of control is two bit tests: the tiles next to both ends of a step in direction `d` are the start's neighbours in directions `d±1`.
- **The bound** reads two facts of the map. The least a step off the routes may cost: one movement point when no terrain of the ruleset costs less; the shipped River, a feature that costs nothing, governs no tile the map generator makes but an editor may put it on one, so then the game keeps whether any tile is governed by such a terrain (`TerrainFloorMemo`, from the tile log, looking at the tiles written only). A floor over every terrain of the ruleset would be 0 and blind the bound (searches seven to ten times slower). And `RouteNet`: per tile, the steps to the nearest tile that can start a road step, one with a route and a neighbour with a route, and to the nearest that can start a rail step; a breadth-first search from all of them. It reads the routes, the cities and which civilizations know the road's and the railroad's techs, and nothing else a civilization learns or builds: it is checked against the `routes` and `cities` revisions and those two sets of players (`RouteNetMemo`). A route built or repaired is taken in where it was built (a breadth-first search from the new sources lowers the distances it shortens); a route lost, a city founded, lost or taken, or one of those techs learned builds it afresh (23 to 63 µs); a property test compares it with a cold build after random changes. `Mover::floors()` gives the least an off-route, road and rail step may cost (embarking and disembarking included off the routes). The bound plays the steps a path must still take at those floors: the first `r` off the routes (`r` the tile's distance to the routes), then roads to the nearest railroad, rail, roads, and the target's own `r` off the routes, up to the hex distance; or the same without rail, or all off the routes, whichever is least. Each reading is consistent (a step never lowers it: the property `the_bound_grows_along_every_step`), so, unlike the sketch above, a tile is never reopened. A tile with a route but no neighbour with one starts no road step, so a lone city does not count: without that, the late fixture's isolated railroad cities made the bound nearly useless.
- **Order and the path.** The heap (`astar::Open`, four-ary, of `(bound, label, tile)`) orders equal bounds by label: the bound saturates (an overdrawing step takes 10 left or 30 left to the same place), and the tile that gives a neighbour its best label must come out first for that neighbour to be closed with it. Those three order entries totally, so no push sequence is kept. The search stops once the target is out (and the entries with its bound and label, for a free step), and walks back choosing, at each tile, the least `(key, tile)` among the closed neighbours whose label steps to it: Python's own path, tile for tile (`the_search_finds_pythons_path` against a literal port of Python's Dijkstra). The labels are Python's too, stacking rule included (a unit may not end its turn on its own kind), which is not always the truly best arrival. A step that costs nothing (a terrain of cost 0) keeps the label, and Python's pop order among such tiles is no longer `(key, tile)`: where no earlier tile steps to one, the walk back follows the tile that gave each its label, which the search records (`Cell::parent`). That path arrives with Python's label and turns, and may take other free steps than Python's; the gates compare the label a path arrives with in the worlds that have River features.
- **Scratch and cache.** `PathScratch` is one cell per tile (generation, flags, label, whether passable, the distance to the target, the facts), so a neighbour is one cache line; a nested search takes a fresh one. `PathCache` keeps up to 64 answers keyed by `PathKey` (unit, tile, moves, target, turn limit) at one revision, and a sight update forgets them (`Derived::forget_paths`), since a path reads what the owner sees. `PathTree` is one bounded search from a unit that answers `find_path` with its limit to every tile. The scratch keeps the tiles a search closed, in order, and a tree reads them back with each tile's label and the tile that gave it, instead of walking the map (a one-turn tree cost 10 µs on a gargantuan map).
- **What the searches know.** An unexplored tile is passable at its true cost, as Python's. The barbarians know every tile, in `reachable` too: Python gave them all explored, and the converter drops their explored set.
- **Movement** (`game::movement`). `step` checks (`step_check`, Python's reasons as `Blocked`), pays, captures a civilian, relocates (carried units with their carrier), clears fortify and sleep and runs `on_enter_tile` (terrain promotions; ruins by 1b-08's `ruins::enter` since the merge, camps wait for 1c-06). `move_toward` walks the path through `Game::step_seeing`, stopping as Python did (`Stop`: out of moves, blocked, an enemy spotted, off the route), and keeps or clears the goto with Python's patience of three turns. `teleport_to_closest` and `find_spawn_tile` take a ring's tiles in the grid's ring order. `send_home` runs when peace is made. A step looks at the unit's movement once (`priced_step` checks and prices it, `take_step` takes it); `follow` builds that one mover per step, its stacking check included, and the move tool's first-step check likewise.
- **Units** (`game::units`). Promotions and experience, health and healing, limited-use abilities, upgrades (costs, blockers, placement), capture, and the turn's start and end are ported. Behaviour changed from Python: a paid promotion needs the experience or a pick of its own (Python let any promotion through while a free one was available, and took experience the unit did not have); an upgrade finds where the new unit stands before the old one goes, so one that cannot be placed changes nothing (Python removed the unit and put a copy back); a created unit records its type in `units_gained`, which Python never wrote (Carthage's mountain crossing reads it). Disbanding refunds a twentieth of the unit's `purchase::base_gold_cost` inside its owner's borders (wired at the merge with 1b-07). Each difference is an entry of `tests/rules/intended.toml`, cited where it is made: `promotion-needs-its-experience`, `upgrade-places-before-removing`, `units-gained-recorded` (shown by `units_carthage_crosses_mountains`) and `unit-refusals-change-nothing` (a refused `move_unit` leaves the unit's orders as they were, where Python had woken it: `units_orders`).
- **Triggers** (`game::triggers`). `fire(site, event, include_unit, note)` matches a trigger's conditionals and `apply` applies a one-time effect that targets a unit (`units::health::apply_unit_effect`), `note` the cause its announcement names, as Python's `fire(..., note=...)` passed it (a unit used up: `due to expending our Great Prophet`); the rest are 1b-08's `apply_one_time` since the merge. Promotions (`TriggerUponPromotion`) and units gained in a city (`TriggerUponGainingUnit`, at `add_unit_in_city`) fire them.
- **Invariants** (§9.4). Two bounds do not hold in play, Python's or this engine's, and are left out: a unit's movement may exceed its allowance (a unique's movement, or a carrier's it has since left), so UNIT-1 holds it to a sanity cap of a hundred movement points; and units stack (a move through its own units stops on one when an enemy comes into view, a great person born in a garrisoned city, the editor), which the fixtures showed, so there is no stacking invariant.
- **Wiring.** Stages S0 and E0 (the barbarians' units) and S6 and E6 (units start and end their turn) run; setup's starting units are placed (`units::starting_units`, `find_spawn_tile`). The actions `move_unit`, `unit_order`, `upgrade_unit` and `promote_unit` have their argument specs; the `explore`, `automate` and `pillage` orders are refused as not ported (1c-04). `RandomAgent` promotes, upgrades, gives orders and moves (`agents::MOVES`).
- **Scripts and inspect.** `inspect`'s unit gains `moves`, `max_moves`, `activity`, `goto`, `fortify`, `embarked`, `carried_by` and `set_up`, on both engines; the test operations `set_unit` and `ready_unit` are ported, on both.
- **Refcheck.** `movement` (reachable tiles exactly; paths as `PathEquivalent`, with their turns and step costs) is enforced, with no difference on the 12 committed states or the 250 of the corpus, strict: the tiles agree too.
- **Kitchen sink.** `CanUpgrade`, `ReducedEmbarkCost`, `XPForPromotionModifier`, `CanMoveOnWater`, `CannotEmbark`, `FreePromotion` and `TriggerUponPromotion` (with a unit effect) are ported and tested in `tests/engine/units.rs`; `TriggerUponGainingUnit` is matched, its effects other than a unit's waiting for 1b-08. Every one-time effect on a unit (damage, a promotion, a free upgrade, movement, experience with its cause, being destroyed) is applied once in `a_one_time_unique_does_its_part_to_the_unit`, as ruins (1b-08) and combat's triggers (1c-03) will apply them.
- **Criterion** (laptop, other packages building; the medians of the bench's own check). Each search is timed as `movement::find_path` runs one the path cache does not hold: a mover built from the memos, then the search; `reachable` is `movement::reachable_this_turn`. Against the budgets:
  - `astar_small_30`, the committed small maps, 16 targets 25 to 35 tiles away from the land unit with the most: 26-27 µs on the mid-game `small-pangaea-raging/t50`, 34-35 µs on `scenario-small-continents-s3001/t61` and 63-67 µs on the late `small-continents-normal-s1025/t280`, each over the 20 µs budget. The gate is their mean, 42-43 µs, under the 3x limit. On t280 the worker's paths run 31 to 43 tiles round a bay; the bound, all off-route steps at a point each (the roads are too far to count), says 13 to 15 turns where the path takes 17 to 19, and every tile within that slack is expanded, about 290 a search, half of them the sea behind the unit (which a two-move unit crosses at a point a tile, so it is no dead end). A tie costs as much: a third of the expanded tiles have the target's bound, and the label order that keeps each tile's label Python's expands them all. Python took 2.5 ms.
  - `astar_garg_fog`, a new gargantuan game's settler to 16 land targets 50 to 58 tiles away through fog: 70-73 µs (budget 150 µs).
  - `reachable`, the late fixture's units with moves: 3.9-4.0 µs (budget 5 µs); right after another unit's step at war, zones of control built again, 3.4-4.4 µs.

  A search costs about 200 ns per expanded tile: the tile's look (about 20 ns), the bound, the heap and six neighbours; reading a tile's cell once for all a neighbour asks measured the same. The route-aware bound, over the single cheapest step it replaced, took the mid-game search from 85 µs to 25 µs; ordering the heap by label cost nothing and fixed the paths. What would take the late case under budget is a bound that knows how far round the coast a path must go, which an additive landmark bound cannot say under the overdraw rule (a step that takes the last move may cost any amount, so a turn absorbs any sum of costs): a candidate for 1e-03, with the tie order.

### 6.11 The other hot algorithms

- **Tile yields.** `CityMods` pre-filters the city's tile modifiers (`StatsFromTiles`, `StatsFromObject`, `StatsFromTilesWithout`, `StatPercentFromObject`, `AllStatsPercentFromObject`). Civ-level conditions are evaluated once, and filters become tests against a per-tile `TileTags` bitset. One tile then costs about 0.3-1 µs, against about 43 µs in Python.
- **Citizens.** `RankCtx` is hoisted once per assignment: focus weights, growth flags, the WLTKD and `happiness_seen` bands, `last_gold_rate < 0`, the construction class and food to the next pop. The rank itself is closed-form.
  - Assignment is always the full greedy from locked tiles, with ties broken by `(value, x, y)`.
  - Python sometimes placed only the new citizen; that difference is intended.
  - Cost is about 8 µs for pop 20, against 9.5 ms in Python.
- **Connectivity** is a multi-source flood fill per medium: road from the capital, then harbour cities over water, then road again, repeated until nothing changes; then rail. One visited bitset per medium makes it O(tiles).
- **Worker automation.** Each `JobMap` keeps the best `(ImprovementId, value)` per tile. `worker_jobs` is one linear scan per worker that skips claimed and dangerous tiles. Build options are evaluated with an explicit tile instead of rewriting `u.idx` (`automation.py:334-339`).
- **Combat.** `combat::setup(att, from_tile, def) -> CombatSetup` builds the modifier stacks once, and the damage for any roll is then closed-form. Python's `preview` rebuilt them about 6 times (`combat.py:512-538`). The Great General aura is a per-civ bitset. Barbarian target search passes `from_tile` instead of rewriting `u.idx` (`barbarians.py:596-604`).
- **Religion.** `CityNeighbours` is a spatial grid with buckets the size of the largest spread range, so pressure costs O(C·k). The found check looks only at city centres within 3 tiles.
- **What-if.** `what_if_building(city, b) -> StatsDelta` computes on an overlay of `CityMods`. It never touches `State` or the city's memos, and replaces the bot's `_simulate` cache swap (`bots/basic.py:1491-1505`).

**As built in 1c-03** (§4.4, §5.9, §6.11, §7.2, §9.2-9.4):
- **Files.** Engine: `rules/combat.rs` (`CombatRules`: the `Military` filters of a great general's aura, the great people of the `War` pool, the widest aura's radius, and `Carriers` of the aura, of the malus of adjacent enemies and of a chance to intercept, resolved at load), `game/combat/{mod, combatant, strength, resolve, city, air, nuke, actions}.rs`, `game/conquest.rs` (with `diplomacy.on_city_captured`, which only a capture reads, and `religion.remove_unknown_pantheons` and `is_holy_city`), `units::capture::{capture_civilian_by, plan_return_civilian, return_civilian}`, `movement::is_embarked_at`, `Game::next_combat_seq`, `triggers::find` (what `fire` would apply, found and traced but not applied), and the trace `triggers::take_fired_for_test` (feature `test-ops`: what fired on the thread, for the sites whose effects wait for 1b-08). Testkit: `tests/engine/combat.rs`, thirteen scripts `combat_*`, `agents::fight`. Refcheck: `answer/combat_previews.rs`. Bench: `citar-bench/benches/combat.rs`.
- **A side** is the evaluator's `Combatant` (a unit or a city); `combatant` reads what the rules ask of it. **`combat::setup(g, a, from, d, sweeping) -> CombatSetup`** gathers a fight's numbers once: both sides' `Mods`, the final strengths and the wounded ratios; `damage_to_defender(rnd)` and `damage_to_attacker(rnd)` are then closed-form. Fight contexts are `strength::fight_ctx` and `fight_ctx_at` (`_ctx`: the side's owner, city or unit and tile, the tile under attack). `preview_of` gives the preview's numbers (`Preview`), which `preview` renders for the tool and `inspect`. The wounded uniques are asked only of a wounded unit, the terrain uniques only for the bonus's sign.
- **Where the attacker stands.** Every read `setup` makes of the attacker's position is of `from`: its fight context, its distance to its capital, the enemies beside it and their tiles, the generals near it, whether it is embarked (`movement::is_embarked_at`), landing, boarding or crossing a river, and whether the defender has it beside it (the enemy counts where it fights from, never where it stands). `resolve::{validate_attack_from, preview_of_from, preview_from}` and `contains_attackable_enemy_from` measure the range, the line of sight and embarkation from `from` (its movement and attacks are as they are now): the `from_tile` preview §6.11 promised the barbarians' target search (`barbarians.py:596-604`, package 1c-06), which rewrote the unit's tile. A test holds `setup` and `preview_from` from a tile equal to the same after the unit moves there. Only the conditionals that look through the world for the unit's own position (`when adjacent to a [unit]`, `when stacked with a [unit]`) still see where it stands.
- **Modifiers** are keyed by `strength::ModKey`: the `Source` of a unique, the great general's type (`General`), or one of the fixed rules (`Landing`, `Flanking`, `Tile`, ...); they keep the order first given. Names are only the preview's and refcheck's (`strength::named`: modifiers whose sources share a name are one line with their sum, as Python's dict, keyed by `_src`, held them). A great general's aura is looked for only on units that may carry one (`CombatRules::aura`), within `aura_radius` of the fighter: on those tiles through the occupancy index, or among the side's units when they are fewer; the best bonus wins, the lowest unit id among equals (Python's first in id order). This replaces the per-civilization bitset §6.11 sketched. The adjacent enemies' malus is asked only of its carriers likewise.
- **Randomness** (§7.2). `resolve::stream(g, purpose, a, b)` takes the next `combat_seq` and keys `[turn, seq, a, b]`: `Combat` for a fight (a withdrawal's tile, the two rolls, the blows by `below(pd + pa)`, a capture's chance, in that order), `Intercept` for each interception (its chance, then its roll; only when a candidate is found), `InterceptOrder` for an air sweep that meets a candidate (the candidates shuffled, then sorted by chance, stably; the sweep's blows come from the same stream; a sweep that meets no one takes no number), `Nuke` for a detonation (tile by tile, the grid's order). A city's plunder and lost buildings draw from `CaptureGold` and `CaptureBuildings` keyed `[city, turn]`. `blows` plays the blows on the sides' hit points held apart and writes them once (a unit's `hp` under `UnitTouch::CORE`, a city's `health` and `damaged_turn` under `CityTouch::CORE`); a unit brought to 0 stays until its caller removes it with `kill_unit`.
- **Interception.** `air::can_intercept` asks the unit's carriers (`CombatRules::intercept`), then its movement, then the unique queries (chance, interceptions, range): a side's or the game's units are asked in turn, and nearly none carry a chance. A bound on the range before the queries was not taken: `[n] Air Interception Range` stacks with its copies (one per building of a civilization, say), so no ruleset-wide bound holds.
- **Conquest.** `conquer` (the other side's military units and aircraft in the city lost, its civilians captured, the plunder, the opinions, the city to the conqueror as a puppet, recaptured, or as the seat's automatic decisions have it) returns a typed `conquest::Capture` (name, old owner, `CaptureResult`, gold); `city::handle_city_defeated` returns `CityOutcome` (`Captured`; 1c-06 adds the barbarians' sack), which the attack's result renders (`write`). `move_to_civ`, `plan_fate`/`apply_fate` (annex, puppet, raze, stop razing, liberate) and `liberate` (a founder that was eliminated comes back through `Game::revive_player`). A captured city's locks go (`assign_citizens(reset=True)`); the settle places its citizens. `founding::remove_building` and `move_to_civ` (whose equivalent buildings may hold less, Walls of Babylon becoming Walls) hold a city's health to its new maximum, where Python kept the excess.
- **Deliberate differences:** `civilians-under-fire` (a civilian a ranged attack brings to 0 dies, and a fight reports a capture only when a melee unit took the civilian), `may-not-annex-refuses-annexing`, and in refcheck `combat-modifiers-in-ruleset-order` (a unit's promotions are a set). `vs-units-never-matches-a-city` moves to `tests/rules/intended.toml`: no reference state shows it. `upon being defeated` fires for the unit that dies (§5.9): what fires is found while it stands (`triggers::find`), then applied once it is removed, at its civilization with no unit, so an effect on the unit itself (a free upgrade would have put a new unit on the tile at no health) never acts on the dead; then `upon losing a [unit]` fires for its owner.
- **Waiting for their packages,** marked where Python had them: a barbarian's sack of a city it beat and a camp attacked (1c-06), the city-states' thanks for kills (1c-06), spies fleeing a captured city (1c-05), eliminations and domination after a loss or a capture (1c-08), the effects of triggers other than a unit's and the great general points of combat (1b-08). Merged after 1b-08, those two are real: the triggers combat and conquest fire apply through 1b-08's `triggers::apply`, and a fight's experience earns great general points through `promotions::add_xp` (`great_people::add_combat_points`); the kitchen-sink tests still read what fired from the trace.
- **Tools, operations and views.** The actions `attack` (a nuclear weapon detonates, an aircraft strikes, anything else attacks), `air_sweep`, `city_attack`, `city_status` and `return_civilian`, with their argument specs; `move_unit` rebases an aircraft. The test operations `attack_as` and `capture_civilian` (a unit, or a player: scenarios cannot add barbarian units) are ported on both engines. `inspect` gains `preview` (the preview, or its refusal as `{error}`), a city's `max_health`, `founder`, `previous_owner`, `original_capital`, `puppet`, `razing`, `resistance` and `attacked`, and a unit's `original_owner` and `return_offer`. `RandomAgent` fights (`agents::fight`), before its units move.
- **Invariants.** CITY-1 holds a city's health at most its maximum.
- **Refcheck.** `combat_previews` is enforced whole, clean on the 12 committed states and the 250 of the corpus; `civilians-at-zero-health` explains the corpus's previews of a civilian loaded at 0 (35), and `combat-modifiers-in-ruleset-order` one corpus state (4).
- **Kitchen sink** (`tests/engine/combat.rs`): `[n] Strength`, `May attack when embarked`, `No defensive terrain penalty`, `[n] Air Interception Range` (the kitchen-sink interceptor now has a chance to intercept), `May not annex cities`, `Never destroyed when the city is captured`, and the triggers `upon conquering a city`, `upon losing a city`, `upon losing a [unit] unit`, `upon defeating a [unit] unit` (with its unit effect) and `upon being defeated` (with a free upgrade and `[This Unit] is destroyed`, on a kitchen sink whose warriors carry them: the unit stays dead after a fight and after a blast). Deferred: none.
- **Gates.** 2: damage grows with the attacker's strength: a property over `CombatSetup::for_test` (feature `test-ops`; the strengths, the roll, both sides' wounds, a civilian defender, a ranged attack) that asks the fight's own damage functions, and real fights of a warrior with and without `[+2] Strength` at several wounds. 3: each fight, bombardment, interception, air sweep that meets a candidate and detonation takes one `combat_seq`, a sweep that meets no one none; a game saved and loaded between two attacks fights the second alike. 4: the thirteen scripts pass on Python and Rust (`combat_air` with a fighter that meets a sweep and fights it); the fighter's sweep is also checked in full on Rust (the damage within its sweeping fight's, `[+33]% Strength when performing Air Sweep`, 5 experience each, either side shot down). Liberation reviving a civilization is an engine test (`conquest::tests`), since Rust eliminates at the round's end (1c-08) where Python did at once.
- **Criterion** (laptop, other packages building): `combat/preview` (`preview_of`: the checks, one setup, four damages) 1.45 µs median over the committed fixture with the most fights (budget 1 µs, report-only), its checks 0.4 µs and its setup 1.0 µs; the JSON the tool reports adds about 2 µs. What remains is about ten unique queries per fight, each looking up a unit's profile index; a setup that looked the profiles up once would take it under the budget (a candidate for 1e-03).

**As built in 1c-04** (§6.2, §6.5, §6.11, §8.3, §9.2-9.4, §9.7):
- **Files.** `game/workers.rs` (`workers.py`), `game/actions.rs` (`actions.py` and `tools.py`'s `found_city`), `game/automation.rs` (`automation.py`), `game/religion/spread.rs` (`religion.py:709-775`), `game/derive/{jobs, danger}.rs`; the testkit's `tests/engine/workers.rs` and `data/worker_jobs.json` (from `scripts/refcheck/worker_jobs.py`); the bench `crates/citar-bench/benches/jobs.rs`; the scripts `workers_road_pillage_repair`, `workers_forest_chop_farm`, `workers_automated`, `workers_turn_flow`, `workers_pillage_improvement`, `units_explore`, `units_paradrop`, `great_people_actions`, `religion_spread` and `found_city_refusals`, which pass on Python and Rust.
- **Who builds** is a `workers::Builder`: a unit, whose own uniques and conditionals count (the tools, and the direct port of worker automation), or a civilization's builder class with no unit (the job maps), whose `Can build [...] improvements on tiles` filters stand for the unit's. Every placement rule (`built_here_ok`, `building_problems`, `unit_can_build`, `turns_to_build`, `needed_removals`) takes the builder and an explicit tile, never a unit moved to it. An improvement over unbuildable features it may not stand on is allowed once the civilization knows how to remove each in its way from the top down, and queues every such removal first, top first (fallout, then the forest under it), its time counted in the options and the job maps. Work in progress is the tile's `BuildQueue`, advanced at E6 (`progress_builds`) by a builder that ends its turn on the tile with movement left; what is finished, and what a unit makes at once, is checked against the tile as it stands (`problems_now`: no removal still to come, but for an improvement that removes features itself), so nothing is set on a feature it may not stand on; `set_improvement` runs an improvement's one-time uniques and `upon building a [...] improvement` (the civilization's, with the tile and the unit in context), removals clear their feature and a chopped forest or jungle gives its nearest city production. Pillage (`can_pillage`, `pillage`) takes the improvement, else the route; its random loot draws from `Purpose::Pillage` keyed `[turn, tile]`, two draws of up to each amount as Python drew.
- **Actions.** `actions::unit_actions` lists what a unit can do now with Python's ids (`found_city`, `found_religion`, `enhance_religion`, `spread_religion`, `remove_heresy`, `hurry_research`, `hurry_construction`, `trade_mission`, `political_treatise`, `create:<improvement>`, `paradrop`, `add_to_spaceship`, `trigger:<n>`, numbered over the unit map's uniques as Python numbered them: the base unit's, its type's, each promotion's; the profile index now holds a unit's action uniques). `plan_action` checks on `&Game` and `apply_action` cannot fail: religion's `plan_religion`/`apply_religion` and `plan_enhance`/`apply_enhance`, the spread pair, the great-person pairs of 1b-08 and a one-time effect a unit carries, refused when it would do nothing by `triggers::would_apply`, which answers each kind of effect on `&Game` with no copy of the game (debug builds check it against the effect applied to a copy). `found_city` is the settler's action on `cities::founding::found_city_by`, which fires `upon founding a city` with the settler in context before it leaves the game; a city-state that has a city founds no other (`city-states-found-one-city`), whoever asks its settler. Adding a spaceship part is refused as not ported (1c-08). A paradrop's range is written as Python's float (`5.0`).
- **Automation.** Stage S8 runs `run_unit_orders` for each major, a settle after each unit: a builder with an enemy in reach stops, a move order walks on (`movement::move_toward`, news only when it ended short), an explorer heads for the explored tile within reach with the most unexplored ground around it for its distance, clear of danger (`explore_target`; else the nearest frontier anywhere), keeping its target until reached and giving up those it cannot reach, an automated worker takes its job (`worker_jobs`, claimed tiles kept apart within the turn), a sleeper wakes near enemies. A unit that carries on with its order writes nothing that moves a cache, and the danger tiles are read in place. `city_site_score` and `suggest_city_sites` read the tiles around a site in Python's order, where the first of equal tiles wins or a sum runs over them.
- **The job map** (`derive::jobs`, one per civilization and builder class, in the cache oracle, which compares every tile with `best_job` asked afresh). A tile is recomputed only when what its job reads changed: its `TileSig` (terrain, features, resource, improvement and pillage, river, owner, a repair or an instant build at the front of its queue, which neighbours are fresh water or coast; whether a city works it and its route only when a filter a job reads asks); the civilization (`CivJobs`: known and obsolete improvements, build times, visible resources and owned luxuries, known removals, the amounts of consumed resources, the revision of the classes the relevant uniques' conditionals read), where a change reaches only the tiles it can (the resource's, those whose job it was, those where a changed improvement could stand and now beat the job, and, weighing every improvement, those with a feature whose removal it learned or forgot and those whose standing improvement changed: `Diff::reach`); its territory. A tile with no job keeps a bound on what any improvement is worth there (the highest value weighed, raised by the civilization's changes that do not reach it); when only its improvement changes, the bound moves by the difference in what the two cost, and the tile keeps no job while neither it nor the improvement taken away rises above the threshold (`kept_without_job`). A tile under a great improvement has no job unless it has fallout. While no relevant unique reads beyond the civilization with its conditionals (the shipped ruleset's case), what the civilization settles for every tile is answered once per key (`Plan`: barred, named by a class filter, instant, the border rules, the tile filters, the irremovable improvements), and each tile then runs only the tile's half of `best_job` (`planned_job`); otherwise (local mode) each tile asks `best_job` itself, and any change of the map, an owner, a city (its own revisions, where a relevant unique reads the city) or the civilization recomputes every tile, since what a change of the civilization does to a tile is known only by asking on the tile.
- **The danger map** (`derive::danger`, per civilization, in the cache oracle): the tiles within striking reach of the hostile military units it sees, recomputed on a unit's move, making or loss, war or peace, a sight settle (`CivRevs::sight`, moved by `vis`), a tile change, or the classes the movement uniques' conditionals read.
- **Wiring.** Stages S8 (standing unit orders) and E6 (worker builds) run. The actions `build_improvement`, `found_city` and `unit_action` have their argument specs; the orders `explore`, `automate` and `pillage` run; `RandomAgent` founds cities, builds, takes unit actions and gives the three orders, and a unit at work on a tile mostly stays to finish it. The test operations `automate` and `progress_builds` are ported, on both engines; `inspect` gains `build_options` and `unit_actions` (a unit) and a tile's `builds`, on both.
- **Differences** (`tests/rules/intended.toml`, each cited where it is made): an improvement over a feature the civilization can remove is offered and queues the removal first (`improvements-over-removable-features`); unit results give tiles as `{x, y}` (`unit-results-give-tiles`); a tile's job is judged for the builder class and its civilization, not the first unit of the type to ask (`jobs-by-builder-class`); jobs of equal priority go to the improvement later in the ruleset, not the name that sorts last (`jobs-tie-by-ruleset-order`); fallout is a job where nothing better is, read from the tile's feature (`fallout-removal-is-a-job`); a city-state that has a city founds no other (`city-states-found-one-city`).
- **Gates.** The ten scripts pass on both engines; `found_city_refusals` refuses a settler with no movement left, a tile too close to a city, a tile on water, a unit without `FoundCity`, and a city-state's settler: its seat takes no tools, it trains no settler (1b-07's `NoSettlerForOneCityPlayers`), and once it has a city the settler's action is refused by the engine itself (a Rust step, `city-states-found-one-city`). Worker jobs on the committed fixtures are compared with Python's (`scripts/refcheck/worker_jobs.py`) and with the direct port (each tile asked for the unit itself): the job maps equal the direct port everywhere, and every difference from Python is one of the two intended ones about tiles: 79 workers and 894 tiles on the committed fixtures, 86 differences (`worker_jobs_on_the_fixtures_are_pythons_but_for_the_intended_differences`); on the local corpus (`CITAR_WORKER_JOBS_CORPUS`, recorded by the script's `--corpus`), 5158 workers and 57,756 tiles, 4738 differences from `improvements-over-removable-features` and 27 from `fallout-removal-is-a-job`, none unexplained. An eighty-turn slice of three `RandomAgent`s with automated workers runs every check clean (the job and danger maps' oracles at every settle), and under `stats` 1 of the job maps' 60 recomputes comes out as it was (under 5%; 2 of 132 over 160 turns). Tests in `tests/engine/workers.rs` pin what reaches a map: a removal learned, a luxury's improvement replaced, fallout on a great improvement, an improvement replaced by a cheaper one (kept, not recomputed), and in local mode a tech, a building and a religion that change build times read on the tile or its city; a farm over fallout and forest queuing both removals and standing on bare ground; a farm not finished over a forest grown since; a city-state's second city refused; every one-time effect of the kitchen sink answered by `would_apply` as applying it answers. The kitchen sink's extras of these systems are ported and tested in `tests/engine/workers.rs`: `SpecificImprovementTime`, `PillageYieldFixed`, `DestroyedWhenPillaged` and `ObsoleteWith` on an improvement, `PercentHealthFromPillaging` and `PercentYieldFromPillaging`, `TriggerUponBuildingImprovement`, `CanHurryPolicy` and `CanSpeedupWonderConstruction` (a wonder only).
- **Criterion** (`jobmap_small`, a civilization's whole job map for the Worker's class built cold, the memos a job reads warm; laptop, other packages building): 10-22 µs on `small-pangaea-raging/t50`, 18-41 µs on `scenario-small-continents-s3001/t61`, and 72-160 µs on the gate, the late `small-continents-normal-s1025/t280` (13 cities; the spread is the laptop's load), under the 200 µs budget. Each tile asking `best_job` whole, with the civilization's half answered again for every tile, took 44, 114 and 829 µs: the `Plan` is what brings the late map under budget.

**As built in 1c-05** (§4.6, §6.2, §7.2, §8.1, §8.3, §9.2-9.5):
- **Files.** Engine: `game/diplomacy/{category, relations, deals, negotiation, actions}.rs` and `game/espionage.rs`; `Game::{record_message, next_deal_id, next_negotiation_id}`; `base::text::truncate_chars` (Python's `text[:n]`). The host methods of §8.1 are in `api/game.rs`: `negotiation`, `negotiations`, `negotiation_view`, `end_turn_refusal`, `max_chat_messages`, `deal`, `describe_items`, `validate_items`, `close_negotiation` and `open_negotiation_as`. Testkit: `tests/engine/diplomacy.rs`, 25 scripts (`negotiation_*`, `diplomacy_*`, `espionage_counter_intelligence`), `agents::{diplomacy, spies, converse}`. Refcheck: `answer/deal_checks.rs`. Python: the `negotiation` and `spies` inspect queries and the test operations `add_spy`, `close_negotiation` and `open_negotiation_as`.
- **Deals** (`deals`). `normalize_items` and `make_proposal` read the items a caller writes as `_normalize_items` did, into typed `DealItem`s (Python's `int()`, the ruleset's loose name lookup, the speed's deal duration, turns held to 1..100); mutual agreements go on both sides in `DealItemKind::ALL` order. A caller holding typed items (a bot, the host) goes through `fit_item` and `proposal_of`, which check them as reading the same items written would (`complete_mutual` is the tail both share). `validate_items` keeps every refusal and its order, and adds one: a side may not declare war on a civilization with a defensive pact with the other side, whose pact would put the deal's parties at war (`deal-war-on-a-partners-pact-refused`). `plan_deal` is what accepting checks (both sides, and peace in a deal at war) and `execute_deal` what it writes (peace first, then each side's items, a mutual agreement once, then the deal's record, then the wars agreed to, `WarReason::Deal`: recorded before its wars, a deal would end with any war between its parties). The friendship and pact fire `upon declaring friendship` and `upon declaring a defensive pact` for both parties. `process_round` is stage R1, reading the deals in place (the list keeps every deal ever made) and copying out only the resource trades still running: a resource trade its giver cannot supply is cut, a deal whose recurring parts have run out expires and one with none ends, lapsed open borders become 0, a research agreement in force banks each side's science and the round after its last turn pays both the smaller sum (`TechState::ra_bonus`, which research spends at E3).
- **Negotiations** (`negotiation`). Every step is a `plan_*` that only reads and an apply that cannot fail: `plan_open`/`open`, `plan_respond`/`respond` (`RespondPlan::{Accept, Reject, Deliver, Cap}`: the message that would pass the cap is a successful call that closes the chat; an accept whose deal closed the chat leaves the close as it stands), `plan_close`/`close` (`plan_close` takes a `NegStatus`; `close_status` reads a name for the test operation). `plan_open_terms` and `plan_respond_terms` are the typed twins of the two that read JSON. Deals and negotiations are kept in ascending id order (checked when a state is assembled, and by ID-1), so `Diplomacy::{deal, negotiation, negotiation_mut}` binary-search. `cancel_between` is what a war does (`set_war` calls it), `expire_for` stage E0. `end_turn_refusal` is the chat rule; `negotiation_json` is the kept shape `inspect` gives, and `negotiation_view` the viewer's (§8.1's `NegotiationView` is that JSON, as Python's dict was). Messages are chronicle entries numbered after the last (`Game::record_message`, the heads' count plus one), cut to 4,000 code points.
- **Actions** (`actions`, §8.3): `send_message` and `respond_negotiation` any time; `open_negotiation`, `declare_war`, `denounce`, `move_spy` (`espionage::MoveSpy`) and `end_turn`, whose check refuses while a negotiation of the player's is open (`ErrCode::Negotiation`) or while a driver plays (`ErrCode::Rule`, through `ensure_not_driving`). Arguments that take several JSON types (`to`, `city_id`) or that Python read with `str` stay JSON and are read as Python read them. The specs are in `api::tools::args`.
- **Espionage** (`espionage`): the spy core of `espionage.py` (recruiting, promoting, effective rank, skill, efficiency, moving, the state machine of stage E3, stealing technology, a dead spy replaced, spies fleeing a captured or destroyed city, `remove_all_spies` for 1c-08's eliminations). The theft draws from `Purpose::Spy` keyed `[spy index, owner, city tile, turn]` (§7.2): the tech among those it could take in id order, then the roll; one whose roll less the spy's skill falls below 0 goes unnoticed (Python's `0 <= result < 100`). The progress multiplies the ruleset's unbounded rank bonus in floats. A spy whose efficiency is 0 would wait forever: its countdown is held to `i16::MAX`. `triggers` recruits and promotes spies through it. A spy set to stage a coup waits for package 1c-06 (`SpyAction::Coup` is `Pending("1c-06")`), which also holds elections.
- **Relations** gain `can_declare_war`, `plan_declare_war`/`declare_war` (the declarer's message becomes a message to the target), `plan_denounce`/`denounce`, `denounced`, `shared_embassies`, `meets_embassy_requirement`, and `plan_peace_with_city_state`/`peace_with_city_state` for 1c-06's `city_state_action`. The city-states' reactions to a war stay `Pending("1c-06")`.
- **Host commands.** `close_negotiation` returns the `Negotiation` as it now stands, since it may be closed for nobody; `open_negotiation_as` takes typed `DealItem`s, as §8.1 has it (a host holding items as a caller writes them reads them with `deals::normalize_items`), and lends no turn, since the rule does not read whose turn it is.
- **Deliberate differences** (`tests/rules/intended.toml`): `met-lists-in-player-id-order` (a message to all goes out in player-id order), `deal-items-fit-their-fields` (a player id, city id or amount no game can hold is refused when proposed, where Python stored it) and `deal-war-on-a-partners-pact-refused` (a war on the other side's pact partner is refused, where Python carried it out and put the deal's parties at war with the deal in force). Keys an item does not have are dropped from the stored item, where Python kept them; no reference state or script shows it.
- **Inspect and test operations.** `inspect` gains `negotiation` (kept, or as a `player` sees it) and `spies`; the test operations `add_spy`, `close_negotiation` and `open_negotiation_as` are ported on both engines (tests/rules/README.md).
- **RandomAgent** answers what waits on it, now and then messages, opens a negotiation from a small pool (one time in ten), denounces, and after turn 50 declares war, then withdraws what it opened; its spies move now and then; `respond` answers a negotiation from a stream keyed by it and its length. Until 1c-09's `drive` dispatches `respond`, the side a chat it opens waits on answers at once (`agents::converse`, up to four answers, from the same streams), so drive carries out its deals; an answer the game refuses ends the chat, and it accepts twice as often as it does anything else. `random_agents_strike_deals_that_run_their_course` drives three who have met for a hundred turns.
- **Kitchen sink** (`tests/engine/diplomacy.rs`): `upon declaring friendship` (three tiles for the capital) and `upon declaring a defensive pact` (forty science) fire once each for a deal of both, `[+15]% spy effectiveness [in all cities]` speeds the kitchen sink's spies at home and abroad, and `Spies in [Capital] cities act as though they have [+1] levels for [Counter-intelligence]` raises the rank of a spy guarding the capital and of no other. Deferred: none.
- **Gates.** 1: `deal_checks` is enforced whole, without `bot_value`. 2: the 18 negotiation-chat scripts and the four diplomacy scripts of `test_engine.DiplomacyTests` pass on Python and Rust, with three more (messages, denouncing, spies). 3: the `end_turn` refusal both ways (the opener waiting, the responder answering) while the host's `end_turn` expires the opener's chats (`negotiation_host_end_turn_expires`). 4: `negotiations_keep_diplomacy_consistent`, a property (64 cases) over random negotiations between three civilizations, the second and third with a defensive pact, a cap of three messages, an accept step and wars on both of the pact's partners in the pool, checks every invariant after each step, and that no deal stays in force between two at war.
- **Counts** at this package's head (after its fix round): nextest 880 passed, 1 skipped (`--workspace --all-features`, the corpus on); Python 555 OK (2 skipped); `cargo xtask check` 11 NotPorted, 40 Pending; the ratchet holds `deal_checks` at 0, clean on the 12 committed states and the corpus's 250; `cargo doc` with `-D warnings` clean.

**As built in 1c-06** (§4.5, §6.2, §6.10, §6.11, §6.14, §7.2, §8.3, §9.2-9.4, §9.7):
- **Files.** Engine: `game/barbarians.rs` (with `barbarians/tests.rs`), `game/city_states/{influence, actions, quests, turn, ai}.rs` (with `city_states/tests.rs`), the city-state side of `game/espionage.rs`; `rules::derived::Known::city_state_builds` (the eight buildings a city-state prefers, by name at load). Testkit: `tests/engine/city_states.rs`, fifteen scripts (`barbarians_*`, `city_states_*`), `agents::city_states`. Bench: `benches/barbarians.rs`. Python: the test operations `add_barbarian`, `barbarian_act`, `clear_camps`, `create_camp` and `sack_city`, and the inspect queries `camps` and `city_state`.
- **Barbarians** (`barbarians`). Aggression (the game's `barbarian_aggression`, else its level's) sets every knob, as `barbarians.py:32-95` did. Camps (`place_camps`, `create_camp`, `update_camps`) appear out of every living civilization's sight, on free land beside land, 4 tiles from capitals and 7 from camps (4 from a destroyed one, which lingers 15 turns); the first at setup (`place_initial_camps`, a third of what the fog holds room for), later one on half the turns. A camp spawns on itself when its countdown runs out (beside it from turn 10 while few barbarians are about, at sea from turn 30), a unit type weighted by UnCiv's force evaluation among what the barbarians' techs allow, which follow the techs every living civilization shares. A camp attacked halves its countdown; a civilization's military unit entering it clears it (`clear_camp`: the difficulty's reward, `GainFromEncampment` recruits, `GoldFromEncampmentsAndCities`); borders taking its tile remove it (`remove_camp`). A city a melee barbarian beats is sacked (`sack_city`, `combat::city::CityOutcome::Sacked`): gold, maybe a citizen and a building (never a wonder, the palace or a free building), health to a quarter of its maximum, left alone for 5 to 10 turns. The AI (`take_turn`, stage S0): ranged units, then melee, then captured civilians (to the nearest camp they can reach); a wounded unit pillages first; then up to three attacks or pillages in reach, whichever is worth more; then the best of five targets within the search radius, else a random tile in reach.
- **Where the raider stands.** The attack search (`attack_targets`) asks `resolve::preview_of_from` from each tile the unit could attack from, never moving it (§6.11); `_seek` builds one `PathTree` with its turn bound and reads every candidate's path from it, following the path it found (`movement::follow`). Draws are keyed by ids and the turn (§7.2): the placement by the next camp id, a spawn's side by the camp's tile, its unit by the camp, the countdown by the camp, a sack by the city, a wander by the unit.
- **City-states** (`city_states`). `influence` gains `FRIEND_INFLUENCE` (moved from `core`), `Relationship` (`Unforgivable`, `Enemy`, `Ally`, `Friend`, `Afraid`, `Neutral`), `resting_point`, `degrade`, `recovery`, `is_aggressor` and `is_warmonger`. `turn`: setup (`init_city_state`: a personality drawn unless the nation names one, a unique luxury and a gifted unit where the type's friend or ally bonuses provide them), the end of its turn (stage E3: drift toward the resting point, countdowns, unit gifts, border tension, free techs, quests, war quests of wars that ended, the election tick), the great people allies give (stage S3, `turns_for_gp_gift` moved from `triggers`), first contact (`on_meet`, called from `Game::make_contact` before the `first_contact` event, as `game.py:700-702`), `on_attacked` (from `set_war`), `on_military_unit_killed` and `barbarian_killed_near` (from a kill), and `on_destroyed` (for package 1c-08's eliminations). `actions`: the `city_state_action` tool (`CityStateAction`, plans `plan_gift_gold`, `plan_gift_unit`, `plan_pledge`, `plan_withdraw`, `plan_tribute`, `plan_peace_with_city_state`, `plan_marriage`, each with its apply), `tribute_modifiers` in Python's order, and `military_strength` (`victory.py:46-52`, which a city-state's fear reads and 1c-08's score may reuse). A protector that attacks its city-state breaks its pledge (`withdraw`). `quests`: `QuestTarget` by the row's `QuestKind`; `quest_target` (drawn from `Purpose::Quest` keyed by the city-state, the major, the row and the turn, so asking twice gives the same answer), `quests_end_turn`, `complete_quests`, `quest_event`, `camp_cleared`, `camp_removed`, `quests_for` (for 1d's views). `ai`: stage S8, founding with its settler, bombarding, production from the cached `Buildable` list (Gold when broke, a defender while short of `2 + cities`, a favourite building whose upkeep it meets, else the first such, else Gold), units attacking in reach or holding the capital.
- **Espionage.** `city_state_election_tick` (the first election drawn from `Purpose::ElectionDelay` keyed by the city-state, then every `city_state_election_turns`), `hold_elections` (the riggers in the capital and nobody at 20, weighted by half the civilization's influence and the spy's skill by its efficiency; drawn from `Purpose::Election` keyed by the city-state and the turn), `can_coup`, `coup_chance` and the coup itself at the end of the spy's turn (its roll from the spy's own stream), and the `stage_coup` tool (`StageCoup`).
- **Wiring.** Stages S0 (the barbarians act), S3 (the great-person gift tick), S8 (the city-state's turn) and E3 (the city-state's own end of turn) and the setup stages `city-state init` and `camps` (sight settled first) are ported; no stage or setup stage waits for 1c-06. The actions `city_state_action` and `stage_coup` have their argument specs; `RandomAgent` deals with a city-state it has met one time in five and its spies stage coups now and then.
- **Differences** (`tests/rules/intended.toml`): `city-state-gifts-need-a-common-nation` (a militaristic city-state's faster gifts need a war both fight against a civilization or city-state; Python counted the barbarians, at war with everyone). A marriage leaves the city-state with nothing, and its elimination waits for 1c-08, which calls `on_destroyed`.
- **Scripts.** The bare prelude also clears the camps (`clear_camps`), so a script that turns the barbarians on places its own. Ten barbarian scripts (sack instead of capture, a sacked city left alone, wonders spared, aggression storming cities and hunting from afar, loot before a fight, a captive carried to a camp, camps spawning and raging ones faster, a camp cleared and attacked) and five city-state scripts (influence decay, gifts, tribute, alliance and protection with an attack's fallout, a coup) pass on both engines. Quests, elections, successful coups, unit and great-person gifts and marriage are unit tests, since their draws differ between the engines.
- **Refcheck.** Every path of `civs` a city-state has is enforced and clean; `civs[*].military_strength` is answered and enforced, and the ratchet falls from 126 to 88 on the committed states (4156 to 2854 unexplained on the corpus's 250, clean where enforced).
- **Kitchen sink.** No extra unique type is staged `barbarians` or `city_states`; the espionage extras were 1c-05's. Deferred: none.
- **Criterion** (laptop, `small-continents-raging-s1005` at turn 120 of the corpus, the nearest to turn 100; `cargo bench -p citar-bench --bench barbarians`): `barbarians/round`, stage S0 on a fresh copy (71 barbarian units, 18 standing camps, aggression 85), 4.7 ms median against a budget of 2 ms (report-only: about 60 µs a unit, most of it the units' searches and moves; the camps' turn alone 118 µs, the units' start 47 µs). The reach a unit's attack and loot share is searched once; the multi-turn `PathTree` of `_seek` and the moves are what is left for package 1e-03.
- **Counts** at this package's head: nextest 974 passed, 1 skipped (`--workspace --all-features`, the corpus on); Python 579 OK (2 skipped); `cargo xtask check` 5 NotPorted, 18 Pending; ratchet `civs` 88.

### 6.12 Drivers, the advisor and the bot boundary

```rust
pub trait SeatDriver: Send {
    fn play_turn(&mut self, g: &mut Game, pid: PlayerId, mem: &mut DriverMemory) -> DriverOutcome;
    fn respond(&mut self, g: &mut Game, pid: PlayerId, nid: NegotiationId, mem: &mut DriverMemory) -> DriverOutcome;
}
pub struct Drivers<'a> { pub seats: PlayerVec<Option<&'a mut dyn SeatDriver>> }
pub enum Stop { External(PlayerId), HybridDiplomat(PlayerId), AwaitingReply { pid: PlayerId, nids: SmallVec<[NegotiationId; 2]> },
                SeatLimit, GameOver }
pub fn drive(g: &mut Game, d: &mut Drivers<'_>, opts: DriveOptions) -> (Stop, EventBatch);
```

- **Decision (bot boundary).** The engine owns the driver state machine, `drive`. Drivers are trait objects supplied by the host:
  - `RandomAgent` from testkit in Phase 1;
  - `citar_bot::Bot` in Phase 2.

  So `run_ai` in the facade becomes `drive` plus citar-bot drivers, and the engine never depends on bot code.
- **`Send`.** citar-py calls `drive` inside `py.allow_threads`, whose closure must be `Send`, so the trait requires `Send`.
- **Driver memory.** `drive` takes the seat's `DriverMemory` out of `Seat.driver` (an empty one on first use), passes it to the driver, and puts it back when the driver returns. All of this happens inside one host call, under the lock, so a snapshot can never observe the seat without it. The bytes are saved and digested (§4.5, §4.10). As built in 1b-03, the seat keeps its memory and the driver works on a copy, written back when it changed, and a driver cannot end its own turn ("As built in 1b-03" after §6.14).
- **Stop reasons** cover each case the host must handle:
  - `External` is a human, LLM or MCP seat;
  - `HybridDiplomat` lets Python run the diplomat phase;
  - `AwaitingReply` implements rule T3;
  - `SeatLimit` lets the server release the lock between AI seats on gargantuan maps.
- **The advisor.** `game::advisor` ports the production advisor:
  - `cities.py:1696-1717`;
  - `basic.py:777-875`, the part of `context` it needs;
  - `basic.py:1146-1590`.

  It uses `AdvisorParams` (the live bot's defaults) and `what_if_building`. Engine auto-production uses it for the puppets and `auto_production` cities of majors; city-states choose through their own AI (1c-06), as Python's did. The bot reuses it in Phase 2, which is plan 2.1's "`advise_production` in the engine".
- **The advisor's random draw.** Python built `BasicBot(seed=city.id)` per pick, and its `self.rng.random()` chose ranged or melee (basic.py:1336-1337). Rust draws from `Purpose::Advisor` keyed `[city, turn]`.
- **Tie order.** `max(scored)` over `(value, name)` picks the lexicographically largest name among equal values. Rust breaks ties by `BuildingId` descending, which is an intended difference.

**As built in 1c-07** (§6.11, §6.12, §9.3, §10):
- **Files.** Engine: `game/advisor.rs` (with `advisor/tests.rs`), `game/cities/what_if.rs`, `rules/advisor.rs` (`Derived::advisor`, `AdvisorRules`: the unit types `Scout`, `Siege`, `Mounted` and `Armored` and the `Scientific` victory resolved to ids at load; the units that found cities, build improvements, build on water, may not join an army (`NuclearWeapon`, `SelfDestructs`) or are spaceship units; the victory and space-program buildings; each building's `[n]% Food is carried over`; and what one more copy of each building adds to its city's own index and its owner's), `unique::index::{Extra, building_extra, Csr::plus}`. Testkit: `tests/engine/advisor.rs`, `data/advisor.json`. Refcheck: `scripts/refcheck/advisor_dump.py`. Bench: `citar-bench/benches/advisor.rs`.
- **The advisor** (`game::advisor`) reads the game and writes nothing. `advise_production(g, p, c, &AdvisorParams)` is `BasicBot.advise_production`: `situation` is the part of `context` production reads (the enemies seen and their weight near each city, the army and its target, gold per turn, happiness, the era, wars with met majors, the unit supply, the exposed cities of `GarrisonMode::Exposed`), `counts` is `_counts`, then `choose_unciv` or `choose_classic` by `prod_mode`. `auto_pick(g, c)` is `cities.auto_pick_production`'s choice at automatic production's parameters (`AdvisorParams::auto_production`, the live defaults at aggression 0.25): a puppet's is `puppet_pick` (the building of most value in the classic valuation, never a wonder or a unit, else Gold, else nothing), any other city's `advise_production`. `cities::queue::auto_pick_production` puts it at the front of the queue through `plan_production`, and replaces 1b-07's stub: a major's puppet or `auto_production` city whose queue runs empty (S5) and `set_auto_production` switched on with an empty queue pick through it. City-states do not: Python's never asked the advisor, and their production is their AI's (1c-06). `AdvisorParams` holds every parameter production reads, with the live bot's `DEFAULT_PARAMS` (checked against Python field by field); `bv_cache_turns` is not one, since the advisor keeps nothing between calls (Python's default was 0, no cache). `aggression` is read held to 0 to 1 (`AdvisorParams::aggr`), as `BasicBot.__init__` held it (NaN counting as 1, as Python's `max(0, min(1, x))` gave). A bot that asks only for production has no war preparation, garrisons, escorts, boat turns or cached sites, and the advisor reads them as such.
- **One advisor for a civilization's turn** (`advisor::Advisor`). `Advisor::new(g, p, &pp)` gathers `situation` (a scan of every unit in the game) and `counts` once; `advise(g, c)` is `_choose_production` for one city; `started(g, item)` counts what a city started, as `manage_cities` did after each pick (`basic.py:1166-1179`). It holds no game, so the bot of Phase 2 keeps one for a civilization's turn and sets production between asks, as Python's context lived while the bot acted. `expansion_sites` is asked only when a city could start a settler and the cheap conditions of `_may_build_settler` hold (the caps, the city's size, happiness; Python computed the sites for every choice), and the `Advisor` keeps them for its turn, each read dropping a site `found_check` now refuses, as Python's `_sites_cache` did for `site_cache_turns`. `advise_production` asks a fresh `Advisor`, which is what automatic production does for each pick in S5: a fresh `BasicBot` in Python, whose context and counts saw the cities that had started or finished something earlier in the stage.
- **S5 places the citizens first.** A major's puppet or `auto_production` city whose queue is empty has its citizens placed (`Game::reassign`, what the settle does for a flagged city) before the advisor picks, as Python's `start_turn` placed them (`cities.py:2223-2229`); the settle after S5 places everyone else's. Without it the advisor weighed what the city could build against where its citizens stood before a building finished or a puppet's focus switched to Gold.
- **Units** are read in the ruleset's order (`BaseUnitSet`), where Python iterated a set of names: the first settler, worker, scout or work boat and the best of equally valued military units are the lowest id. Of equally valued choices the largest item wins (`Constructible`'s order: the largest `BuildingId` among buildings), where Python's `max` over `(value, name)` took the name last in code point order; the classic mode keeps the first of the highest, as Python's stable sort did (`advisor-ties-by-id`). The ranged-or-melee draw (`prefers_ranged`) is `Purpose::Advisor` keyed `[city, turn]` (`advisor-draws-by-city-and-turn`). A military unit that costs nothing is valued as costing one (`advisor-counts-a-free-unit-as-costing-one`), where Python divided by zero and the city picked nothing.
- **The what-if** (`cities::what_if`, §6.11). `what_if_building(g, c, b) -> Option<StatsDelta>` (`before`, `after`: the city's stats total with its happiness the sum of its happiness list) and `CityWhatIf` (a city's what-ifs sharing what its memos give: its stats, tile modifiers, and each tile's yield with the classes it recorded). The game is read through `EvalView::what_if(g, &Overlay)`: the city's building set with the building (`FilterFacts::city_buildings`, which every reader of a city's buildings now asks), its own index and its owner's with the building's entries added (`Csr::plus` of `AdvisorRules::adds_local` and `adds_civ`, what a rebuild with it gives, which a test holds for every building), and, only where the building could change them (it needs or provides a resource, adds to the unit supply's or trade network's uniques, or the conditionals those memos recorded read buildings), its owner's resources with the resource layer of its index and cities' indexes, its trade network and its unit supply deficit, each computed in the view (`economy::compute_supply_in`, `connections::connected_cities_in`, `economy::unit_supply_deficit_in`) and kept only when it differs. The city's tile modifiers (when the building could move them), base, parts and stats are computed by the memos' own functions in the view (`tiles::city_mods_in`, `stats::{city_base_in, city_parts_from, city_stats_from_in}`); a tile's yield is its memo's when its recorded classes miss what the building moves. A city filter that reads the buildings of the city it is asked of (`CityLeaf::Has`, `in all cities with a world wonder`; `NonOccupied`, which a Courthouse makes hold) has no class of its own, since a memo keyed by the city validates on the city's revisions: the tile modifiers record `CITY` for one (`Filters::city_deps_here`), and the unit supply notes `CIV_BUILDINGS` for a `[n] Unit Supply per [k] population [cities]` whose filter reads them (`Filters::city_reads_buildings`), so the what-if computes them again (two kitchen-sink tests). A city in We Love The King Day asks whether its owner would be happy with it: `economy::compute_happiness_in` over the owner's cities, each city's parts from its memo when nothing it read moved; every other city is computed again only when the owner's resources change or the building adds to its owner's index a type a city's yields or happiness read (`AdvisorRules::widens`, from `rules::advisor::CITY_STATS_TYPES`, the types `cities::stats`, `tiles`, `economy`, `cities::connections` and `eval` name, which a test holds to their sources): 11 of the 64 shipped buildings that add to their owner's index, where every one of them did (a Monument's and Walls' `Destroyed when the city is captured`, a national wonder's `Cost increases by [n] per owned city`, a Courthouse's own unique, read from the city's buildings, move no other city). The production penalty of units over the supply is `economy::supply_penalty(deficit)`, which city stats and both `unit_supply_penalty` read. Python's `_simulate` left values computed with the building in the caches it did not swap, so its answers depended on what had been asked of the game before (`advisor-what-if-leaves-no-trace`; the dump asks each question from cleared caches).
- **Kitchen sink** (`tests/engine/advisor.rs`, `sink`): `Triggers victory`, the one extra staged `ai`, is worth the victory building's value in a city producing at least the average (the Kitchen Sink Wonder is built first, and not without the unique); a building with `[+4] Unit Supply` lifts a civilization's supply penalty in the what-if, which agrees with the build for every kitchen-sink building. Deferred: none.
- **Gates.** 1: on the 12 committed states the what-if of every building each city lacks, and on the 250 of the corpus every building each city could build, equals adding it (`toggle_building_for_test`, feature `test-ops`) and reading the city's memos, bit for bit, `before` equals the stats after taking it out again, and the digest is unchanged; the reference states reach each part of the overlay but the unit supply (reached by the kitchen sink), We Love The King Day's happiness on the corpus. 2: over the same states the advisor (both modes, and `auto_pick`) gives only items `is_buildable` accepts, the same answer twice and on a fresh load, and moves no digest; an engine test asks a warm and a cold copy alike. 3: a puppet's pick is a building that is no wonder, or Gold, on every city of the reference states, and on a city of the small test game with every tech (Gold once every building is built; a city of its own then picks a unit or a wonder). 4 (informational): against `advisor_dump.py`'s answers, the committed states agree on 93 of 96 cities for automatic production, the UnCiv mode and the classic mode (the three that differ are Python's settlers) and on 96 of 96 puppet picks; the corpus (5,101 cities of majors in 250 states, dumped to a local file and read through `CITAR_ADVISOR_DUMP`) agrees on 4,858 for automatic production (95.2%; 183 of the 243 that differ are Python's settlers), 5,086 puppet picks (99.7%), 4,861 in the UnCiv mode (95.3%; 182 of 240) and 4,893 in the classic mode (95.9%; 103 of 208). Of the rest, the cases looked at are the ties the ids break (the Pyramids against the Mausoleum of Halicarnassus, a Garden against a Constabulary, at equal value), the draw and the order of units (Rifleman against Gatling Gun, Infantry against Machine Gun), and the Marble decision (`marble-bonus-in-its-own-city`: a city building a wonder without the quarry has 15% less production in Rust, so what a building adds to it weighs less). A first dump asked every question of one game in turn and agreed less (4,841 in the UnCiv mode): Python's `_simulate` had left the previous what-ifs in its caches (`advisor-what-if-leaves-no-trace`). 5 (report-only): see Criterion.
- **Sites, since the merge with 1c-04.** `advisor::site_score` is 1c-04's `automation::city_site_score(g, p, t) -> Option<f64>` (`automation.city_site_score`), which scores the sites `expansion_sites` ranks. Before the merge it was `Pending("1c-04")`: no site scored, so no settler was chosen, which was most of gate 4's differences. With it, the committed states agree on 96 of 96 cities in every mode, and the corpus's second dump on 5,041 for automatic production (98.8%), 5,043 in the UnCiv mode (98.9%) and 4,996 in the classic mode (97.9%), none of the differences left where Python founds a city. `expansion_sites` walks the `site_radius` of every city; it runs only when a city could otherwise start a settler, once per `Advisor` (a civilization's turn for the bot; one pick in S5), where Python cached the sites for `site_cache_turns` (4).
- **Criterion** (laptop, other packages building; `cargo bench -p citar-bench --bench advisor`, own medians): on `small-continents-normal-s1025/t200` of the corpus (29 cities of living majors, 314 buildings they could build), `advisor/call_per_city` 250 µs against the 50 µs budget, over it (report-only); `advisor/what_if` 21 µs. The what-ifs are 87% of a call (about 11 a city): the overlay is 0.05 to 3.5 µs, the rest a full recompute of the city's base, parts and stats in the view (6 to 35 µs, as a city's stats memo recomputes). An incremental what-if (the building's own yields and percentages added to the city's) would take a call under budget, bit-exactness permitting: a candidate for 1e-03. After the fix round (the sites asked last), on the same state: Criterion's `advisor/every_city` 5.76 ms for the 29 calls (199 µs a call), `advisor/every_city_kept` (one `Advisor` a civilization) 4.36 ms (150 µs a call), `advisor/every_what_if` 4.41 ms for the 314 (14 µs); the run's own medians, as noisy as the laptop's load, 133 µs a call and 14 µs a what-if. Still over the 50 µs budget (report-only).

### 6.13 Parallelism

**Decision (no parallelism in Phase 1).** The pipeline area proposed `ExecPolicy`, a host-supplied pool and parallel sections behind `par`. The workspace area said no threads in Phase 1, and it wins:
- the budgets in §10 do not need parallelism;
- throughput comes from games side by side;
- it removes rayon from the engine and the 1-vs-4-thread CI job for now.

The compute functions are already pure: `&State` and `&Derived` in, values out. A later `par` feature can therefore add index-merged sections:
- load rebuild;
- `record_stats`;
- tile prefetch;
- what-if batches;
- map-generation noise.

`par` requires memos that are safe to share (for example, per-thread `Derived` overlays), and the 1-vs-N-thread determinism job arrives with it. Citizens, visibility, moves, combat and AI never run in parallel, now or later.

### 6.14 New-game setup

`Game::new` (game.py:145-318) is a stage table too, in `game::setup`. Each stage is ported by the package that owns its system:

| Stage | What it does | Package |
|---|---|---|
| config | `config_from_json` normalisation (game.py:151-196) into a `NewGame`; seed required; map inline, kept as its id | 1b-03 |
| nations | chosen nations, then a keyed shuffle of the rest (`Purpose::NationShuffle`) | 1b-03 |
| map | an editor document: `validate`, `tiles_from_rows`, continents and explicit starts | 1b-03 |
| | a generated map | 1b-04 |
| | start filling and ruins for documents that lack them (`maps.prepare`, `Purpose::MapPrepare`) | 1c-09 |
| players | majors, city-states, barbarians; seats and overrides | 1b-03 |
| starting techs | era techs, AI free techs, `StartsWithTech`, era gold and culture | 1b-07 |
| city-state init | `city_states::init_city_state` | 1c-06 |
| starting units | `units::starting_units`, `find_spawn_tile` in ring order | 1c-02 |
| starting triggers | global and nation uniques with no trigger conditional | 1b-08 |
| relations | a `Relation` for every non-barbarian pair | 1b-03 |
| camps | `barbarians::place_initial_camps` | 1c-06 |
| happiness | commit every civ's `happiness_seen` once | 1b-06 |
| visibility | register every source; first contact | 1c-01 |
| begin | `begin_turn` for player 0; the `game_start` event | 1b-03 |

Python drew the nation shuffle and the map from one `random.Random(seed)`. Rust gives each its own `Purpose`, so tuning map generation never reshuffles nations. The `newgame-*` goldens are blessed in 1c-09, once every setup stage is ported.

**As built in 1b-03** (§6.2, §6.12, §6.14, §8.1, §9.3, §9.6):
- **Files.** Engine: `game/turn/{stages, driver, drive}.rs`, `game/setup.rs`, `mapgen/{document, continents}.rs`; host methods `end_turn` and `force_turn` in `api/game.rs`. Testkit: `src/agents.rs` (`RandomAgent`), `src/golden/turns.rs`, `tests/engine/turns.rs`; `script::setup` is gone, replaced by `script::{new_game, map_doc}` over `Game::config_from_json` and `Game::new`. Scripts: `turns_order`, `turns_city_states_play_themselves`, `turns_not_your_turn`, `turns_counter` and `setup_settings`.
- **Stage tables** (`game::turn::stages`). One row per system step, not one per stage: `PLAYER_START` has 23 rows, `PLAYER_END` 25 and `ROUND_END` 11, each labelled with its stage of §6.2 (`S0`-`S9`, `E0`-`E6`, `R0`-`R6` for the round) and owned by one package, so a stage two packages share (S2: research 1b-07, great people, religion and the Maya 1b-08) is two rows, each flipped by its own package. A row has `who` (majors, city-states, barbarians), `when` (`HasCities`, `Religion`, both, or `EvenIfOver` for the round's close) and a `Step`: `Player(fn)`, `Round(fn)`, `Settle` (the ◆ points), `StopIfDead`, `StopIfBarbarian` (Python's early returns), `SkipIfOver` (R0: a round whose eliminations end the game skips to its close, the R6 rows marked `EvenIfOver`, so it is still settled and digested) or `Pending`, the no-op of a row whose `porting` is `Pending("<pkg>")` (a unit test holds the two together). Ported in 1b-03: the control flow, the settle points, the `turn_start` and `turn_end` events (S9, E6), the lapse of unanswered civilian-return offers (E0), the next turn (R4), the round's optional digest (R6) and the turn limit (R5): a game whose last turn is past ends with no winner and a `game_over` event, the Time victory by score waiting inside it as `Pending("1c-08")`, so drives and games reach an end. Every other row is pending: 40 rows, which `inspect` `pending` lists as `turn_stage` (`player_start S2: research progress`). `stages::waiting()` lists them.
- **Kitchen sink.** `upon turn start` and `upon turn end` (the extras staged `Turn`) are rows S4 and E1, `Pending("1b-08")`: firing them needs the civilization's unique index (1b-05) and `apply_one_time` (1b-08), like every other trigger site; a kitchen-sink game sets up and plays its turns meanwhile. `SpawnRebels` and `CannotAttack` (staged `Turn`) are the revolts of row S3, `Pending("1c-08")`. The `Setup` types (`StartingTech`, `StartsWithTech`, `DisablesReligion`) are read by setup.
- **Turn driver** (`game::turn::driver`). `Game::end_turn` (host) and `end_turn_now` (the rule) refuse a game that is over and a player whose turn it is not ("It is not your turn (it is Rome's turn).", the tools' wording, `end-turn-names-whose-turn-it-is`); then `PLAYER_END`, and the loop of `game.py:1017-1033`. With no living major a call ends the round and returns at the next round's start, player 0's turn not begun, so nobody ends a turn twice in a round (`end-turn-stops-without-majors`; Python's loop never returned); a turn not begun begins before it ends. `begin_turn`, `end_round` and `force_turn_now` are `pub(crate)`; the host's `force_turn` and the test operation refuse a player the game does not have, a dead one (`Eliminated`, "Greece has been eliminated and plays no turns.") and a game that is over, where Python made a dead player's turn current or moved a finished game's turn (`force-turn-only-for-the-living`). `Game::set_chain(Option<DigestChain>)` opts a game into the round digests of §4.10: row R6 folds each round's digest in after the round's settle and before the next turn begins, under the round's own turn number, taken as its end begins (`chain()`, `last_round()`, `last_round_digest()`); the chain is never saved, and a host resumes it with `DigestChain::resume`.
- **Drivers** (`game::turn::drive`). `SeatDriver: Send` has `play_turn` and `respond` (called from 1c-09); `DriverOutcome` has `Done` and `Stop` has `External(pid)` and `GameOver`, both `#[non_exhaustive]` for 1c-09's variants; `DriveOptions` is empty until 1c-09's seat limit. `Game::drive(&mut Drivers, DriveOptions) -> Result<(Stop, EventBatch), ActionError>` (a poisoned game refuses) begins a turn not yet begun, ends the turn of a seat that is no living major (a forced turn), stops at a seat with no driver, and otherwise hands the driver a copy of the seat's `DriverMemory` (an empty memory of kind 0 when it has none), writes it back only when the driver changed it (kind 0 and empty is stored as none, so a driver that keeps nothing leaves the seat unchanged), and then ends the turn; the seat holds its memory throughout, so a digest or snapshot taken while a driver plays sees it, which is §6.12's guarantee kept without taking it out. Ending the turn is `drive`'s alone, as `run_ai` did (`play_turn(end_turn=False)`): while a driver plays (`Game::driving`), `end_turn`, `force_turn` and `drive` refuse (`ErrCode::Rule`), so a round a driver's turn closes is digested with the memory that turn left, and the chain is the same whoever asked; a turn that passed anyway is reported as TURN-1. Package 1c-09 sets `driving` around `respond` too. Its batch holds every event since it began, whatever the driver's own calls took. It stops with `GameOver` when no major is alive. `Drivers::none(n).with(p, &mut d)` builds the seats. `citar_testkit::agents::RandomAgent` draws from `Purpose::TestAgent` keyed `[pid, turn]` and plays the moves in `agents::MOVES`, empty until the system packages add theirs.
- **Setup** (`game::setup`). `Game::config_from_json` (and `config_from_value`, which takes the parsed JSON whole, so the map document and the seats move into the `NewGame` uncopied) normalises `game.py:151-196`: null means absent; the seed is required ("The settings need a seed: ..."), and a map must come as the editor's document ("The map must come inline, ... (got the id 'islands')."); names resolve loosely and an empty one is the default, but a name the ruleset or lobby lacks is refused listing what is valid when the table is short (`config-refuses-unknown-names`); the seats are checked in Python's order (count, then each handicap and `auto`, then controller, nation and difficulty), and kept in `GameConfig::host["players"]` (an empty list becomes the default number of `{}`), with every key the engine does not read; the starting era defaults to era 0, the barbarians to `normal`, a generated map to `small` and `continents` (lobby keys); the lobby's resource options are read as leniently as `MapOptions` read them. `SETUP` is the table of §6.14 with 15 rows: `Draft` rows make the state (config, nations, map, players), then `Game` rows play on the new game (starting techs, relations, begin); 8 are pending, which `inspect` lists as `setup_stage`. `Game::new` moves the map's tiles and continents and the players out of the draft into the state. Ported beyond the table's 1b-03 rows: the starting techs, gold and culture, which the scripts rely on (granted by `research::add_tech_silently`, which 1c-06's city-state catch-up shares; era techs, `Starting tech`, an AI seat's `aiFreeTechs`, `Starts with [tech]` read from the nation's, the known techs', the era's and the global uniques with their conditionals, and the era's stocks scaled by speed), so package 1b-07 need not. A generated map (1b-04) and a document short of starts (1c-09's `maps.prepare`) are refused `NotPorted`. Nations: the seats' choices, then `Purpose::NationShuffle` keyed `[0]` over the unchosen majors (popped from the end, as Python did) and `[1]` over the city-states. Colours follow `unique_colors` (`colors_clash`, redmean under 60). Relations need no stage work: `Diplomacy::new` holds every pair. `begin` begins player 0's turn, then emits `game_start`; `Game::new` returns the game settled, with that batch.
- **Map documents** (`mapgen::document`). `read` ports `maps.validate(fix=True)` and `tiles_from_rows` with Python's warnings (the resource check reads the top feature with hills sorted first, as Python did), `dimensions` the width and height check; which improvements a map may carry, and which are land-only, are rules over `ImprovementKind`, `great`, `Known` and the terrains (`map-documents-read-by-rule`). `mapgen::continents::assign` numbers landmasses from the largest (`_components`, `_assign_continents`).
- **Scenario and test operations.** `add_unit` is ported (it was `Pending("1c-02")`): the turn scripts need a unit per player, since Python eliminates a player with no unit and no city at the round's end; what `create_unit` gives a new unit stays 1c-02's. The test operations `end_turn` (optional `player`), `end_round` and `force_turn` return `{turn, current}`, on both engines.
- **Invariant TURN-1** now allows a game over with no winner (the turn limit with the Time victory off, `victory.py:363-365`); a winner in a game that goes on is still a violation.
- **Golden sets.** `SetReport` has `waiting`, the stages a set depends on that are pending. The new set `turns` (the arena, two bots and a human seat, a city-state and the barbarians, 20 turns, each round's digest chained, one row per round the chain took, under `last_round()`'s turn) depends on every setup and turn stage: `golden check` computes it and reports it `waiting` (not compared; `matches_committed` true, so the determinism workflow still compares the targets), and `golden bless` leaves it out and says why; `golden bless turns` refuses with exit 1 (gate 4). Package 1c-10 blesses it with the rest.

**As built in 1b-04** (§5.10, §6.14, §7.1, §8.1, §9.5, §9.6, §9.7):
- **Files.** Engine: `mapgen/{options, noise, landmass, map, terrain, rivers, spread, starts, wonders, resources, ruins, generate, metrics}.rs` beside 1b-03's `document` and `continents`, `api/maps.rs`, and `rules::derived::KnownMap`. Testkit: `tests/engine/mapgen.rs`, `src/golden/maps.rs` and `golden/maps.json`. Bench: `citar-bench/benches/mapgen.rs`.
- **Entry points.** `mapgen::generate(rules, seed, &GenSpec) -> Result<GeneratedMap, MapError>` ports `generate_map` (`mapgen.py:1662-1721`): `GenSpec { width, height, map_type: MapType, options: MapOptions, players, city_states, nations, ruins }`, and `GeneratedMap { width, height, wrap_x, wrap_y, tiles, starts, cs_starts, continents, attempt }`. `MapType` and `MAP_TYPES` are the five shapes (`MapType::from_key` falls back to continents, as Python did); `MapOptions { edges, rivers, resources }` reuses `state::config`'s `MapEdges` and `ResourceOptions`, and `IceSides::of(edges)` gives the capped sides. The lobby's resource settings are read by `mapgen::options::{option_number, resource_options}`, which `game::setup` now calls instead of its own copies. Starts that do not fit in `ATTEMPTS` (12) tries are `MapError("Could not generate a map with valid start positions: ...")`; a ruleset with no base terrain of land or of water cannot generate at all.
- **Streams.** Each phase draws from `Rng::keyed(seed, Purpose::Map*, [attempt, step])`: `MapIce`, `MapLand` (the land scores, the continents' centres), `MapClimate`, `MapRelief` (mountains, hills, flattened mountains), `MapLakes` (the coast's spread), `MapVegetation` (step 0 vegetation, 1 rare features), `MapRivers`, `MapStarts`, `MapWonders`, `MapResources` (steps 0 strategic, 1 strategic variety, 2 luxuries, 3 luxury variety, 4 bonus, 5 the starts made playable, 6 the lobby's shares, 7 and 8 the variety top-ups again) and `MapRuins`. `_side_rng` is gone: a top-up has its own step. Gate 4 is the unit test `mapgen::generate::tests::tuning_one_phase_leaves_the_other_phases_draws_alone`: with the vegetation's stream keyed apart (`generate_with`, crate-private), the tiles after ice, land, climate, relief and lakes are the same, the vegetation differs, the rivers (which read nothing vegetation writes) are the same, and every other `Map*` stream draws the same numbers.
- **What generation names** is `rules::derived::Known.map` (`KnownMap`): Ocean, Coast and Lakes (base terrains of water), Mountain, Snow, Tundra, Plains, Grassland and Desert (of land), Ice, Marsh, Oasis, Forest and Jungle (features), and Horses, Iron and Cattle; each is `None` unless the ruleset has it as that kind, and a step that needs a missing one is skipped (no lakes without Lakes, no polar ice without Ice, no mountains without Mountain), where Python failed on the name. Land starts as Plains (or the first base terrain of land) and water as Ocean (or the first of water). The rest is read by rule through `gen_tables` and `mapgen::map::Kit`, gathered once per map: hills are the terrains `Occurs in groups`, coast is Coast or any water terrain marked `Coastal Water`, fresh water the terrains marked `Fresh water`, the climate lands, flats, vegetation, rare features and wonders by kind and flags; a resource only a city-state makes (`CityStateOnlyResource`) and the food bonuses by their unique and stats.
- **The map in the making** (`mapgen::map::GenMap`) holds the engine's `Tile`s, the ice band, latitude, temperature, humidity, landmasses and the per-resource counts, and answers map generation's filters through `TileFacts` (`Filters::gen_matches`): a tile's terrains, its river, fresh water on or next to it, and a neighbour that is coast. Features are a `FeatureSet` in layer order, which is Python's list order for every combination the generator makes. `set_terrain` takes the rivers off a tile that becomes water, on both sides of each edge (`mapgen-no-river-along-new-water`).
- **Rivers** walk the grid's corners (`rivers::Corners`: every three mutually adjacent tiles, sorted, with the other corner of each edge): each edge between two land tiles has its cost drawn once, in edge order, and the drainage search (`MinHeap`, ties by push order) runs from the mouths, where Python drew a cost lazily and a random tie-break per push. `metrics::rivers` reports the river edges as a graph over the corners: `edges`, `systems`, `dry` (a system touching no water), `loops`, `along_water` and `one_sided`; `sound()` is gate 1's "every river reaches water, and none cross".
- **Natural wonders.** `Must be on [n] largest landmasses` (the kitchen sink's extra) and `Must not be on [n] ...` read a tile's landmass number, numbered from the largest, below n, which is Python's `continents_by_size[:n]` with n clamped to the number of landmasses (the note of 1a-05b). A neighbour turned by `Neighboring tiles will convert to` must meet all the unique's conditions (`mapgen-wonder-conversions-read-every-condition`). A vegetation or rare feature marked `Doesn't generate naturally` is never placed, and one marked `Doesn't generate naturally <in [...] tiles>` is not placed where its conditions hold (`GenMap::may_generate`; `mapgen-features-that-never-generate`); a land terrain with either form stays out of the climate's choice, as Python's `has_tag` kept it. A wonder's group grows over masks of the group and its neighbours, in time linear in its size (`wonders::grow_group`).
- **Resources.** The share of luxury types a map keeps is `mapgen::luxury_variety(tiles)`, a curve over the tile counts of the shipped lobby sizes (`mapgen-luxury-variety-by-tile-count`), and `luxury_types_wanted(tiles, types)` its count. The rest follows `mapgen.py:1191-1647`: strategic major deposits per terrain and minor ones spread out, luxuries round the starts and the city-states and scattered, bonus resources by frequency (skipping region-only ones), starts normalised, shares rebalanced, and every strategic type and the luxury variety guaranteed. A share takes no normal type's last deposit (`mapgen-shares-keep-every-type`): Python's did, and the variety top-ups after it put the type back as a fresh cluster, so a 25% luxury share came out near 20%; now a share is its part of its kind to within a tile and the kind's total is what the density made.
- **For 1c-09** (`maps.prepare`): `mapgen::fill_starts_on(rules, &MapDocument, have, n, min_gap, avoid, avoid_gap) -> Result<Vec<TileIdx>, MapError>` ports `_fill_starts` over `_start_candidates` and `_start_score`, and `mapgen::ruins_on(rules, &mut MapDocument, rng, starts, cs_starts) -> Result<(), MapError>` spreads the ruins on the document's tiles; the caller draws from `Purpose::MapPrepare`. Both take the document `read_map` already holds, so the grid and the tiles come together, and refuse (`MapDocument::checked_grid`) a document whose tiles do not fill its size, or a given tile off the map, leaving the document as it was.
- **Setup.** The stage `map: a generated map` is a `Draft` stage (`generate_map`): for settings without a document it builds the `GenSpec` from `MapSource::Generated` (the lobby size's width and height or the `dims` set, the type by its lobby key, the edges), the settings' `river_density` and `resources`, the seats (each nation's start bias) and the city-states drawn, and `ruins`, and draws from the game's seed; `read_map` passes when there is no document. A crowded map is refused as `EngineError::Map`. `setup::generated_map` checks the sides as given before an odd height on a map that wraps north-south is made even (as `maps.generated_map` checked first, `maps.py:88-91`), so a height of 65535 is refused, not overflowed, through `Game::config_from_json` and `api::maps::generate_map` alike. `inspect` `pending` lists 7 setup stages; `cargo xtask check` counts 22 `NotPorted` and 99 `Pending`.
- **`api::maps::generate_map(rules, seed, &settings) -> Result<Value, EngineError>`** ports `maps.generated_map`: the settings are the lobby's (`map_size`, `width`, `height`, `map_type`, `map_edges`, `river_density`, `resources`, `players`, `city_states`, `ruins`, `name`), read by setup's `generated_map` so an unknown name is refused alike (`config-refuses-unknown-names`); the result is the editor's document, its rows written by `mapgen::document::tile_row`, its name and description as Python wrote them, and its id `api::maps::slug(name)` (`map` where Python named the empty case after the clock).
- **Properties** (gate 1, `tests/engine/mapgen.rs`): ten tests, duel and small by the five types, 200 seeds each with the edge modes in turn: the starts and sites are on passable land, apart and bare; every feature lies on its base or a feature below it; a wonder stands alone; only strategic deposits have a size; the landmasses match the tiles; the rivers are `sound()`; no ice outside the band (`metrics::ice_outside_band`); no strategic type missing; the luxury variety met; and the worst start scores at least 0.4 of the best (`metrics::start_quality`; the lowest over the 2,000 maps is 0.455, small fractal; the ignored `report_start_quality` prints the distribution). They take about 3 s in the ci profile. Beside them, Python's four resource tests (`tests/test_mapgen.py`): the luxury-variety curve by lobby size, every luxury and strategic type on huge seeds 1 and 7 and gargantuan seed 3 with two players, at least 70% of the luxury types on standard maps and half on duel and small ones, sparse strategic resources and Silk off, and each kind's density at 0; then the lobby's caps (`Cap`, never exceeded by any step) and shares (Iron 60%, Wine 25%); and a ruleset overlay with Forest held back on hills and Jungle everywhere.
- **Timing** (gate 2, `cargo bench -p citar-bench --bench mapgen`, release): a small map 4.8 to 5.5 ms by type, a gargantuan one 38 to 62 ms (archipelago the slowest), against budgets of 50 ms and 1.5 s; the run fails above three times either.
- **Golden set** `maps.json` (gate 3): ten maps, duel to huge, every type and edge mode, the first majors' nations seated so start biases count; each row is `[name, edges, seed, [width, height], blake3(CITAR-MAP, size, wraps, canon tiles, landmasses, starts), attempt, [starts, sites], land tiles, river edges, resources]`.

---

## 7. RNG, maths and determinism

### 7.1 The RNG

```rust
#[repr(u32)] #[non_exhaustive] pub enum Purpose { /* frozen discriminants; see the list below */ }
pub struct Rng { s: [u64; 4] }                                    // xoshiro256++
impl Rng {
    pub fn keyed(seed: u64, purpose: Purpose, keys: &[u64]) -> Rng; // h = mix(seed ^ C0); h = mix(h ^ purpose);
                                                                    // h = mix(h ^ keys.len()); for k in keys { h = mix(h ^ k) };
                                                                    // s = four SplitMix64 outputs from h (never all zero)
    pub fn next_u64(&mut self) -> u64;
    pub fn below(&mut self, n: u64) -> u64;                         // Lemire with rejection: unbiased
    pub fn range(&mut self, lo: i64, hi_inclusive: i64) -> i64;
    pub fn unit(&mut self) -> f64;                                  // (x >> 11) * 2^-53
    pub fn chance(&mut self, p: f64) -> bool;
    pub fn shuffle<T>(&mut self, v: &mut [T]);                      // Fisher-Yates from the end
    pub fn pick<'a, T>(&mut self, v: &'a [T]) -> Option<&'a T>;
    pub fn weighted(&mut self, w: &[f64]) -> Option<usize>;         // cumulative sum in index order
}
pub trait KeyPart { fn key(self) -> u64; }   // ids: the raw value (< 2^32); Option: None = u64::MAX; bool: 0/1
```

**Decision (generator).** The plan's "e.g. ChaCha8 streams" and the pipeline area's `rand_chacha` keyed through `blake3::derive_key` lose to the workspace area's own xoshiro256++:
- it is about 150 lines we own for good, so no crate release can change a game;
- deriving a key is a few nanoseconds, which matters for per-event streams such as chance rolls;
- cryptographic strength is not needed;
- `rand`'s own sampling algorithms are not value-stable across releases anyway.

Committed test vectors pin `keyed` and every distribution (`golden/rng.json`).

**Purposes** replace every Python RNG site (verified by grep): `game.py:557-559` `state_rng` (32 call sites), `g.rng` (combat.py:550, 555, 565, 693, 739, 970-973, 1029, 1153) and mapgen's `m.rng`.
- **Combat:** Combat, Intercept, InterceptOrder, Nuke.
- **Barbarians:** BarbPlace, BarbUnit, BarbSpawn, BarbCountdown, BarbSack, Wander.
- **Cities:** Demand, DemandNew, CaptureGold, CaptureBuildings.
- **City-states:** CsInit, CsUnit, CsGiftUnit, CsGp, CsGpGiver, CsAttacked, Quest, Quests.
- **Espionage:** Spy, Election, ElectionDelay.
- **Religion and ruins:** Prophet, Ruins.
- **Uniques:** Trigger, Chance.
- **Turns:** Revolt, RevoltDelay, UnVote.
- **Workers:** Pillage.
- **Setup:** NationShuffle, MapPrepare.
- **Advisor:** Advisor.
- **Map generation:** MapIce, MapLand, MapClimate, MapRelief, MapLakes, MapVegetation, MapRivers, MapStarts, MapWonders, MapResources, MapRuins.
- **Tests:** TestAgent.
- **Reserved:** `BotBase = 0x1000_0000` and up, one per decision type, for Phase 2. This gives plan 2.1 its per-decision streams, so turning bot diplomacy off leaves the other draws alone.

### 7.2 Keys

Keys are semantic integers only: ids, turn, tile, rule ids and `UniqueMeta.key`. They are never floats, strings or volatile counts.
- **Optional parts** go through `KeyPart`, so `None` is `u64::MAX` and never collides with tile 0 or player 0. Python's string keys kept them apart as `"None"` and `"0"` (game.py:557-559).
- **Unique keys.** `UniqueMeta.key` hashes the source kind, source name, occurrence and text. So the same text on a promotion and on a building rolls independently, and an unrelated ruleset edit does not move the rolls.

**The Python keys that are replaced:**
- `len(g.s.camps)` in `barbarians.py:137` → `(turn, next camp id)`;
- `len(g.s.units)` in `barbarians.py:292` → `(turn, camp id)`;
- `len(g.s.events)` in `barbarians.py:443` → `(turn, city id)`;
- the text of a unique (`triggers.py:94`, `uniques.py:842`) → `meta.key`;
- the spy's name (`espionage.py:147`) → the spy's index;
- the quest name → `QuestKindId`.

**Decision (combat's sequential stream).** Python's combat draws from the saved Mersenne Twister `g.rng`; `combat.py:550` even mixes `g.rng.random()` into a battle's key. Rust has no sequential stream at all. Every combat event, including an interception or a nuke, does two things:
- takes `combat_seq = st.ids.combat_seq; st.ids.combat_seq += 1`;
- draws from `Rng::keyed(seed, Purpose::Combat, &[turn, combat_seq, attacker, defender])`.

`combat_seq` is persisted and digested. `save_rng` (`game.py:995-1002`) goes away.

**Map generation** has one stream per phase, replacing `_side_rng` (`mapgen.py:1476-1490`). Tuning one phase does not shift the others.

### 7.3 Maths and number formatting

`base::num` provides:
- `pow`, `exp`, `ln`, `log10`, `hypot` (used by mapgen), `sin`, `cos` and `atan2` as `libm` wrappers;
- `floor_div` and `floor_mod` for i32 and i64, with Python's semantics for `//` and `%` on negative operands;
- `round_half_even`, which is `f64::round_ties_even`: Python's `round()`, used at 77 `round(` sites in `citar/engine`;
- `round_half_away`, used only where Python wrote `int(x + 0.5)` or `math.floor(x + 0.5)`;
- `round_ndigits(x, n)`, Python's `round(x, n)` (17 sites);
- saturating float-to-int conversions.

`round_ndigits` rounds the exact binary value to n decimals, half to even on exact ties. It is implemented on Rust's exact float formatting with an explicit tie fix-up, and unit-tested against a table recorded from Python, including ties such as 0.125, 2.5 and 2.675 (`scripts/refcheck/pyfmt_vectors.py` → `golden/pyfmt.json`).

`base::fmt::PyFloat(f64)` reproduces Python's `repr`/`str` of a float:
- the shortest round-trip digits;
- a trailing `.0` for whole numbers;
- scientific notation when the exponent is below -4 or at least 16, written as `1e+16` or `1e-05`.

Rust's `Display` prints `2.0` as `2` and never uses an exponent; its `Debug` writes exponents differently. Model-facing text (briefing.py:608 `round(u.moves / sc, 1)`, views.py:82 and 189-197, tool errors, event text) goes through `PyFloat` and `round_ndigits`. Otherwise the refcheck `briefing`, `views` and `tool_errors` string groups would flood with differences.

`sqrt`, `floor`, `ceil`, `trunc`, `abs`, `min`, `max`, `clamp` and `total_cmp` are exact in std and stay allowed; `round` is banned (§2.5). The engine has no FMA and no fast-math. Sums run left to right in a documented order.

### 7.4 Order

- Units and cities are iterated by ascending id.
- Maps are `BTreeMap`, `IndexMap` (insertion order, `shift_remove` only) or dense vectors. Hash maps are only ever looked up (`LookupMap`).
- `order::argmax_first` returns the first of equal elements, as Python's `max` does. Rust's `max_by_key` returns the last.
- Float keys compare with `total_cmp`.
- Every deciding sort key ends in an id or a tile index.
- Only stable sorts are used, since clippy bans unstable ones.
- `MinHeap` breaks ties by push sequence.

### 7.5 Enforcement

| Hole | What closes it |
|---|---|
| Hash-order iteration | The hash types are banned by clippy (verified). `LookupMap` has no iteration methods. `same_process_twice` runs two identical games in one process, where `RandomState` differs between maps. |
| Platform libm | Clippy bans the std transcendental functions (verified). `libm =0.2.16` with default features off. `golden/libm.json` holds about 2,000 inputs as bit patterns, compared on all 5 targets. |
| Rounding mode slips | `f64::round` and `libm::round` banned; `round_half_even` and `round_half_away` named for what they do |
| Insertion-order loss | `IndexMap::{remove, swap_remove}` banned, with every other swap form, the entry APIs' and `serde_json::Map`'s |
| Optional key parts | `KeyPart` (None = `u64::MAX`) |
| Toolchain tie order | Stable sorts only. `rust-toolchain.toml` pins exactly 1.98.1, and a toolchain bump is a pull request that re-blesses the goldens. |
| Build profile | The digest refuses NaN; the `ci` and `release` profiles are compared in CI |
| Signed zero | Hashed as is, so a divergence shows instead of hiding |
| Reads changing a game | Queries take `&self`; property P8 |
| Denormals flushed in the host process | citar-py gets a canary, `black_box(f64::MIN_POSITIVE) / 2.0 != 0.0`. If it fails, results are marked as not verifiable for proof of work. |
| Big-endian or 32-bit targets | `compile_error!` and a `const` assert in `lib.rs` |
| Threads | none in Phase 1 (§6.13) |

---

## 8. Engine API and errors

### 8.1 Host methods (`api::game`, `impl Game`)

```rust
// lifecycle
pub fn new(rules: &'static Ruleset, setup: &NewGame) -> Result<(Game, EventBatch), EngineError>; // settings + editor document
pub fn config_from_json(rules: &'static Ruleset, json: &[u8]) -> Result<NewGame, EngineError>; // requires seed; map inline
pub fn load(rules: &'static Ruleset, state_json: &[u8], chunks: &mut dyn Iterator<Item = &[u8]>) -> Result<(Game, LoadReport), LoadError>;
#[cfg(feature = "legacy")] pub fn from_python(rules: &'static Ruleset, json: &[u8]) -> Result<(Game, ConvertReport), ConvertError>;
pub fn snapshot(&self) -> Snapshot;                      // under the lock; Snapshot::to_json() off it
pub fn take_journal_chunk(&mut self) -> Option<JournalChunk>;
// commands: each checks, applies, settles, then renders its result from the settled state
pub fn execute(&mut self, pid: PlayerId, tool: &str, args: &serde_json::Value) -> Result<Executed, ActionError>; // 1d
pub fn act(&mut self, pid: PlayerId, a: Action) -> Result<(Outcome, EventBatch), ActionError>;              // typed, 1b-1c
pub fn end_turn(&mut self, pid: PlayerId) -> Result<EventBatch, ActionError>;
pub fn drive(&mut self, d: &mut Drivers<'_>, o: DriveOptions) -> (Stop, EventBatch);
pub fn close_negotiation(&mut self, nid: NegotiationId, status: NegStatus, note: &str, by: Option<PlayerId>) -> Result<(NegotiationView, EventBatch), ActionError>;
pub fn open_negotiation_as(&mut self, pid: PlayerId, to: PlayerId, msg: &str, give: &[DealItem], recv: &[DealItem]) -> Result<(NegotiationView, EventBatch), ActionError>;
pub fn emit_host(&mut self, kind: &str, text: &str, players: Option<&[PlayerId]>, data: &serde_json::Value) -> EventBatch; // host heads only
pub fn add_thought(&mut self, pid: PlayerId, text: &str, kind: &str);                                                   // host heads only
pub fn set_controller(&mut self, pid: PlayerId, c: Controller, h: Option<Handicap>, a: Option<AutoDecisions>) -> Result<EventBatch, EngineError>;
pub fn set_difficulty(&mut self, pid: PlayerId, name: &str) -> bool;
pub fn apply_ops(&mut self, ops: &serde_json::Value) -> Result<(Vec<serde_json::Value>, EventBatch), ActionError>; // atomic
pub fn meet(&mut self, a: PlayerId, b: PlayerId) -> EventBatch;
pub fn force_turn(&mut self, pid: PlayerId) -> EventBatch;
pub fn debug(&mut self, a: DebugAction /* MeetAll|Reveal|Gold */) -> EventBatch;
// queries: &self; memos validate lazily; never change State or the digest
pub fn view(&self, pid: Option<PlayerId>, event_limit: u32) -> Vec<u8>;       // JSON bytes, the web client's shape
pub fn briefing(&self, pid: PlayerId) -> String;
pub fn turn_progress(&self, pid: PlayerId) -> String;
pub fn empire_summary(&self, pid: PlayerId) -> EmpireSummary;
pub fn negotiation_view(&self, nid: NegotiationId, pid: PlayerId) -> Result<NegotiationView, ActionError>;
pub fn path_preview(&self, pid: PlayerId, unit: UnitId, x: i32, y: i32) -> PathPreview;
pub fn standings(&self) -> Vec<Standing>;
pub fn summary(&self) -> Summary;  pub fn turn(&self) -> Turn;  pub fn current(&self) -> PlayerId;  pub fn phase(&self) -> Phase;
pub fn events(&self, since: u32, limit: usize) -> &[Event];  pub fn event_view(&self, ev: &Event, pid: Option<PlayerId>) -> EventOut;
pub fn stats(&self, last: Option<usize>) -> &[StatsRow];  pub fn thoughts(&self, pid: Option<PlayerId>, since: u32) -> Vec<&Thought>;
pub fn negotiation(&self, nid: NegotiationId) -> Option<&Negotiation>;  pub fn negotiations(&self) -> &[Negotiation];
pub fn end_turn_refusal(&self, pid: PlayerId) -> Option<String>;  pub fn deal(&self, id: DealId) -> Option<&Deal>;
pub fn replay_data(&self, f: ReplayFormat) -> Vec<u8>;  pub fn rev(&self) -> u64 /* session ETag, not saved */;  pub fn digest(&self) -> Digest;
```

**Free functions:**
- `api::tools::{schemas, schemas_json, kind}`
- `api::maps::{validate_map, blank_map, generate_map(rules, seed: u64, ..), map_summary, export_map}`
- `api::scenario::{ops_help, overview, default_seats, normalize_seats, scenario_summary}`
- `game::diplomacy::category::{CATEGORIES, item_category, proposal_categories}`
- `api::text::{RULES_OVERVIEW, MAP_LEGEND}`
- `save::summary`

**Decision (event delivery).** An `EventBatch` is returned by every call that changes the game:
- it holds the events that call appended;
- Python's `subscribe` fans them out;
- nothing calls back into Python during a turn.

**Decision (seeds and maps from the host).**
- `Game.new` and `maps.generated_map` drew a random seed when none was given (game.py:156-157, maps.py:92), and `Game.new` read editor maps from disk (game.py:165-170).
- The engine has neither randomness nor I/O. So `config_from_json` requires a seed, and requires a custom map as an inline document. `generate_map` takes a seed.
- The Python facade draws seeds with `random.randrange(1, 2**31)` and resolves map ids, as today.

**Decision (atomic `apply_ops`).** Python applied ops until the first failure, left the earlier ones applied, and skipped the visibility refresh (scenario.py:471-487). Rust's behaviour:
- It clones the `Game`, applies the ops in order, and settles.
- On the first failure it restores the clone and returns an error naming the op: "Operation 3 (set_city): …".
- The editor therefore sees all or nothing, and the game is never left half-edited and unsettled. The clone costs a few milliseconds even on large maps.

### 8.2 The facade, mapped

| `EngineGame` (Python) | Rust |
|---|---|
| `new(config)` | `Game::new` with `config_from_json`; the facade supplies the seed and inline map |
| `from_save(data)`, `from_state(state)` | `Game::load` (new-format states and scenarios) |
| `to_save()`, `state_dict()` | `snapshot` + `take_journal_chunk` |
| turn, current, phase, winner, victory, turn_limit, config, player, majors, summary | `summary()` |
| standing, standings | `standings()` |
| stats, events, event_view, thoughts, thought_count, add_thought, emit | chronicle reads, `add_thought`, `emit_host` |
| execute | `execute` |
| view, briefing, turn_progress, empire_summary | same names |
| negotiation, open_negotiations, negotiation_head(s), negotiations, negotiation_view, end_turn_refusal, close_negotiation, max_chat_messages, deal, describe_items, validate_items, open_negotiation_as | diplomacy reads and ops of the same names |
| subscribe, unsubscribe | Python-side fan-out of each `EventBatch` |
| set_controller, set_difficulty | same names |
| play_bot_turn, bot_respond, bot_advice | citar-bot through `drive` and `SeatDriver` (Phase 2) |
| apply_ops, scenario_overview, default_seats, normalize_seats, save_scenario, export_map | `api::scenario`, `api::maps` (the file writes stay in Python) |
| path_preview, has_met, meet, force_turn, debug | same names |
| replay_data | `replay_data(Full)` until replay.js reads `Delta` (Phase 3) |
| inspect, test_ops (new methods, not in `__all__`) | `api::inspect`, `api::testops` (feature `test-ops`) |

### 8.3 Tools and typed actions

**The typed side.** `game::action::Action` is a serde enum tagged by tool name. Its variant and field names are the tools' own names and argument names, checked against the argument specs. Each variant is added by the system package that owns its rule. `Game::act` runs one pipeline:

```rust
pub fn act(&mut self, pid: PlayerId, a: Action) -> Result<(Outcome, EventBatch), ActionError> {
    self.guard(pid, &a)?;                   // poisoned, game over, invalid player, eliminated, not your turn
    let plan = a.check(self, pid)?;         // &self: nothing written; a refusal returns here
    let spec = a.apply(self, pid, plan);    // infallible; returns an OutcomeSpec (which ids and fields to report)
    self.settle();
    Ok((spec.render(self), self.take_batch()))   // rendered from the settled state
}
```

**Why render after the settle.** `set_city_focus`, `work_tile` and `set_specialists` reassign citizens and return the worked tiles, so that "the caller sees the consequence of the choice in the same response" (tools.py:680-757). In Rust the reassignment happens in the settle, so results are rendered after it. The same holds for `move_unit` and unit actions that report what was revealed or who was met. A script asserts that `work_tile`'s returned `worked_tiles` equals `inspect` after the call.

**Argument specs and `normalize`,** from 1b-02:
- `api::tools::args` holds each tool's argument spec: names, JSON types, the required list. Each system package adds the specs with its `Action` variants.
- `api::tools::normalize(tool, args)` ports exactly what tools.py:113-127 does:
  1. check required keys first, and report the missing ones;
  2. drop unknown keys;
  3. coerce integer parameters with Python's `int()`:
     - a JSON float truncates toward zero;
     - a string must be an integer literal, surrounding whitespace allowed;
     - anything else is "Parameter 'k' must be an integer.";
  4. coerce boolean strings: true when they are "true", "1" or "yes", in any case;
  5. split comma strings into arrays.
- There is no `at` → `x, y` coercion. The earlier draft cited one, but Python never had it.
- Both script runners coerce the same way from day one, and `execute` (1d) parses `normalize`'s output into `Action`.

**The JSON side (1d-01).** `api::tools::registry` holds:
- the static `ToolSpec` table of the 61 tools (21 queries, 40 actions), each with its description text, JSON schema, kind, `any_time` and category;
- `schemas_json()`, cached;
- the `Query` tools, which build their answers from the views' info builders.

**Unknown argument keys** are dropped, as today (`tools.py:116-117`). Models often add harmless keys, and failing on them would cost benchmark turns.

**Who calls what.** Bots and testkit call `act` and never touch JSON.

**The `end_turn` chat rule** is part of `Action::EndTurn`'s check (phase0-spec A1.5). `Game::end_turn` keeps the safety net where the initiator's open negotiations expire.

**The action log.** `ActionRecord` has no wall-clock field; hosts add timestamps. It counts in the host heads, never in the digest.

**Decision (when tools land).** The typed `Action`, argument specs and `normalize` land with each system (1b/1c). Only the JSON registry, query tools and model-facing descriptions wait for 1d.

### 8.4 Events and name scrubbing

`game::events::emit(g, kind, text, audience, tile, data, mentions: &[(&str, NameTarget)])` does five things:
1. **Possessives.** It applies the possessive fix (`game.py:17-23`) with a hand-written scanner.
2. **Audience.** It widens the audience with majors that see the tile, one bit test each. This happens only when the event is not public (`audience.is_some()`), has a tile, and is not private (`EngineEvent::is_private()`, ported from `PRIVATE_EVENTS`, game.py:808-812, 872). So rivals are never told about `unit_built`, `great_person_born`, `spy` and the other 21 private kinds.
3. **Name references.** It computes `refs` with the name index. The index is an `aho-corasick` automaton over civilization, leader and city names, rebuilt only when the `names` revision changes; this replaces the regex rebuilt at `game.py:814-840`.
   - Matching reproduces the regex exactly. Candidates are collected with an overlapping search. Then, scanning left to right, the longest candidate at each start whose ends satisfy the word boundary is taken, and matches never overlap. A leftmost-longest automaton alone would drop a shorter valid name when the longest one fails the boundary.
   - The word boundary treats a character as a word character when `char::is_alphanumeric()` or `_` holds, which is Python's Unicode `\w`.
   - `mentions` adds names the index no longer knows, such as a razed city (cities.py:2375) or a civilization's old name (tools.py:924), without overlapping existing refs (game.py:842-858).
4. **Append.** It appends the event to the chronicle.
5. **Hash.** It updates the engine running hash.

**Scrubbing.** `event_view` scrubs by the typed player fields (`game.py:923-990`). It:
- anonymises unmet civilizations and city-states, with Python's capitalisation and possessive rules;
- replaces coordinates such as `(12, -3)` with "an unknown location", using a hand-written matcher for `\(-?\d+,\s*-?\d+\)` (game.py:18, 967);
- drops positions;
- anonymises the UN tally.

It slices text by byte offsets. Golden tests in 1b-01 cover non-ASCII names, a razed city, a renamed civilization, and a private event on a tile others can see.

### 8.5 Errors

```rust
#[derive(Debug, Clone, thiserror::Error)] #[error("{message}")]
pub struct ActionError { pub code: ErrCode, pub message: String }
pub enum ErrCode { UnknownTool, InvalidPlayer, Eliminated, GameOver, NotYourTurn, MissingParam, BadParam, OffMap,
                   NoSuchUnit, NoSuchCity, NoPath, Negotiation, Rule, NotPorted }
pub enum EngineError { Action(ActionError), Config(String), Map(String), Load(LoadError), Rules(RulesetErrors), Poisoned(Box<str>) }
pub enum LoadError { Json(String), Version(u32), UnknownName { path: String, name: String }, UnknownKey(String),
                     Invalid(Vec<ValidationError>) }
pub struct LoadReport { pub rules_changed: Option<(RulesetId, String)>, pub chronicle_incomplete: bool, .. }
pub struct RulesetErrors(pub Vec<RulesetError /* { file, object, text, kind } */>);
pub enum ConvertError { Json(String), Unresolved { path: String, name: String }, UnknownKey(String), NonFinite(String) }
```

- **The message is what models read.** It explains the rule and names what is valid.
  - Existing text is ported verbatim where it is fine and fixed where it is wrong; each fix is listed in `intended.toml`.
  - Numbers in it go through `PyFloat` and `round_ndigits`.
  - The code gives tests and refcheck something stable to match on.
- **Text rules (property P5):**
  - never empty;
  - 600 characters or fewer;
  - ends in `.`, `?` or `)`;
  - no Rust debug artefacts: nothing matching `Some\(|\bNone\b|Idx\(|::`.
- **Atomicity.** An `ActionError` leaves the digest unchanged, emits no event and does not settle (property P2).
- **Panics.** A panic that escapes is caught by the host. The game is marked poisoned: it refuses further commands and can only be snapshotted for debugging.
- **Mapping to Python exceptions:** `Config` → `ValueError`, `Map` → `MapError`, `Action` → `ActionError`.

---

## 9. Testing and validation

### 9.1 Instruments and gates

| Question | Instrument | Gate |
|---|---|---|
| Same rules on the same state? | `citar-refcheck` against 262 recorded states | enforced groups clean in CI; the full corpus `--strict` at the Phase 1 exit |
| Do the behaviours the old tests pinned still hold? | TOML rule scripts over scenario ops, run by both a Rust and a Python runner | all pass on Rust; each validated on Python first |
| Panic or corruption? | invariants, `verify_caches`, properties P1-P8, chaos, soak | zero failures |
| The same game everywhere? | golden digests on 5 targets, `ci` against `release`, `same_process_twice` | all equal, and equal to the committed files |
| Fast enough, and staying fast? | criterion on the laptop; gungraun instruction counts in CI | `thresholds.toml`; +5% at most |
| Whole bot games alike? (Phase 2) | baseline JSONL comparison | every `**` row explained; not a gate |

### 9.2 Reference checks (`citar-refcheck`)

- **Loading.**
  - A fixture parses into `Fixture { name, meta, state: Box<RawValue>, queries }`, and its state loads through `Game::from_python`.
  - Unmapped fields are printed from `ConvertReport`.
  - One load per fixture is enough. Python re-loaded per group because its queries have side effects (`meta.side_effects`). Rust queries take `&self`, and each group asserts the digest before and after. `tool_errors`, which mutates, clones the game per call. `--fresh-per-group` exists for debugging.
  - Fixtures run in parallel with rayon, and results are sorted by name.
- **The groups:**
  - the recorded groups: `tile_yields`, `city_stats`, `civs`, `buildable`, `movement`, `visible`, `combat_previews`, `deal_checks` (`bot_value` only with `--with-bot`, Phase 2), `tool_errors`, `views`, `briefing`;
  - three synthetic groups:
    - `uniques`: Rust compiles every text in `refcheck/uniques.json.gz`, dumped by `scripts/refcheck/uniques_dump.py`;
    - `state_echo`: a projection of the fixture's own state JSON, compared with `inspect` reads after loading;
    - `fixed_point`: the settle on load changes neither explored tiles nor who has met whom.
- **Answer modules.** Each group has one module, `answer/<group>.rs`. It rebuilds the skeleton of Python's answer from the recorded inputs and fills it with Rust calls through `game::query`. A Python group that crashed while recording is reported as `python-crashed`, as information, never as a difference.
- **Comparison:**
  - integers exact;
  - other numbers within `|a−b| ≤ 1e-6·max(1,|a|,|b|)`, with 3 and 3.0 equal;
  - strings exact, with a line diff (`similar`) for text over 200 characters or containing a newline;
  - object keys as a union: a key present on one side only is Missing or Extra, and null is not the same as absent;
  - arrays in order unless the group's `CompareSpec` declares them keyed (by field or tuple position), multiset or custom.
- **`PathEquivalent`.** A different path is accepted when it:
  - starts and ends at the right tiles;
  - moves between adjacent tiles at every step;
  - has the same turns and summed costs as Python's.

  If Rust takes fewer turns, the diff kind is `Better`, and it needs an intended entry with `rule = "rust_le_python"`. Reachability (a path against `null`) must agree.
- **Path grammar**, shared by reports, `intended.toml`, `enforced.toml` and scripts:
  - segments `.key`, `["quoted"]`, `[n]`, `[*]`, `[field=v]`, `[#0=v]`, `.*`, `.**`;
  - reports print concrete selectors, such as `civs[pid=0].happiness.breakdown.Religion`.
- **`intended.toml` v2** is `[[differences]]` with:
  - `id` (kebab-case, unique, cited at the fix site as `// refcheck: <id>`) and `reason`;
  - `where = [{group, path}, …]` and optional `cases` globs;
  - optional `python` and `rust` constraints (exact, `re:`, or bounds), so an entry never masks a later, unrelated change;
  - optional `rule` and `broad`.

  The file holds no entries yet, so it switches straight to v2; the v1 inline form is not supported.
  - An entry that matches nothing in a run covering its cases is **stale**: a warning locally, an error with `--strict`.
  - `citar-refcheck changelog` prints the entries as the CHANGELOG's list of rule fixes.
- **Enforcement.**
  - **Decision.** `refcheck/enforced.toml` plus `refcheck/ratchet.json`, instead of a `gate.toml`.
  - `enforced.toml` lists groups, optionally narrowed by path globs. Every system package adds its paths there.
  - The ratchet stores unexplained counts per group, and they may only fall.
  - Reports print groups in dependency order: uniques, state_echo, fixed_point, tile_yields, city_stats, civs, buildable, movement, visible, combat, deals, tools, views, briefing.
- **The CLI.**
  - Commands: `citar-refcheck run [--fixtures DIR]… [--groups] [--case GLOB] [--json OUT] [--strict] [--with-bot]`, plus `explain`, `suggest` (TOML stubs, never accepted automatically), `ratchet [--update]`, `changelog` and `list`.
  - Exit codes: 0 clean; 1 unexplained differences; 2 a load failure; 3 stale entries under `--strict`.
- **Speed.** About 20 seconds on one thread for the corpus, 3-5 seconds on 8 threads, and under 1 second for the 12 committed states.

### 9.3 Rule scripts

- **Why scripts.** About 118 Python engine tests poke internals (`g.create_unit`, `w.moves = 60`, `mock.patch`), and many depend on the map that seed 21 generates. Scripts run on hand-made **arena maps** (`tests/rules/maps/arena.json`: 24×16) with named anchors: A, B, CS, H (hill), F (forest), R (river), W (water), L (luxury) and M (mountain).
- **Starting a game.** A script's header gives a config with an inline arena map and explicit starts. With `start = "bare"`, both runners add a standard prelude: no city-states, barbarians off, ruins off, then `clear_units` for every player. So a script means the same thing before and after the setup stages for units and camps are ported.
- **Script syntax.** Scripts are TOML: the comments matter, and Python 3.11+ ships `tomllib`. Steps are JSON-shaped:
  - `op`: a scenario op, or a test op;
  - `tool`: with `player`, `args`, and optionally `as`, `id`, and `error = "substring"` or `error = true`;
  - `check`: with the matchers `eq ne gt ge lt le approx contains not_contains len absent is_null matches any none subset`;
  - `repeat`.

  Strings that start with `=` are small arithmetic expressions over bound variables. Tile references are anchors, `(x,y)`, direction walks (`A>e>ne`, computed by the runner in odd-r), or selectors answered by `find_tiles`, whose candidates are sorted by (distance, y, x).
- **Tool arguments** go through `normalize` on both runners (Python's own coercion; Rust's port from 1b-02). So a script behaves the same whether it passes `3` or `"3"`. `_selftest.toml` has a check that scripts themselves never type numbers as strings, so the coercion is tested on purpose, never by accident.
- **Test ops** (feature `test-ops`; `citar/engine/testops.py` on the Python side):
  - unit and position: `clear_units`, `set_unit`, `ready_unit`, `capture_civilian`, `attack_as`;
  - world state: `set_turn`, `unmeet`, `complete_construction`, `add_spy`;
  - barbarians and automation: `barbarian_act`, `sack_city`, `automate`, `progress_builds`;
  - turn flow: `end_turn`, `end_round`, `force_turn`;
  - seats and diplomacy: `set_controller`, `close_negotiation`, `open_negotiation_as`;
  - `refresh_visibility`, and `reload` (a save followed by a fresh load);
  - `bot_turn` and `bot_respond` in Phase 2.

  The existing `set_city` op gains `health`, `attacked` and `food`.
- **`inspect`** returns small documented shapes with sets sorted. It covers game, player, unit, units, city, tile, relation, negotiation, events, view, briefing, `find_tiles`, the pending stages, and calculations shared with refcheck's functions.
- **Runners:**
  - Rust: `citar-testkit/tests/rules.rs`, using libtest-mimic with one trial per test, so nextest lists them.
  - Python: `tests/rulescript.py` plus `tests/test_rule_scripts.py`, built on `engine_api.EngineGame`. In Phase 2 the same Python runner tests the bindings.
  - `tests/rules/_selftest.toml`, which includes must-fail checks, keeps the two interpreters from drifting apart.
- **Where the tests go.** About 110 Phase 1 scripts. About 98 of them come from the existing tests:

  | Module (tests) | Scripts | Native Rust |
  |---|---|---|
  | test_engine (32) | 14 | 16 (hex, rules, map options) |
  | test_mechanics (26) | 26 | |
  | test_barbarians (10) | 8 | 2 |
  | test_events (8) | 8 | |
  | test_single_player (12) | 9 | 3 |
  | test_bots (9) | 5 | |
  | test_negotiation_chat (27) | 18 | |
  | test_controllers (13) | 10 | 1 |

  The other 12 are new scripts for behaviour this design pins down:
  - citizen tool results after the settle;
  - `found_city` refusals;
  - first contact through border growth;
  - private events;
  - atomic `apply_ops`;
  - liberation reviving a civilization.

  What stays out of Phase 1:
  - 15 bot scripts, and the bot parts of test_bot_diplomacy and test_bots, wait for Phase 2;
  - the 5 test_mapgen tests become properties;
  - 14 server tests stay in Python.

  A check whose expected value changes on purpose carries `intended = "<id>"`, and the Python runner skips it.

**As built in 1b-02** (§8.1, §8.3, §9.3):
- **Files.** Engine: `api/{mod, game, scenario, inspect, testops}.rs`, `api/tools/{mod, args, normalize}.rs`, `base/py.rs`, and the first rules of three systems, `game/research.rs`, `game/diplomacy/{mod, relations}.rs` and `game/city_states/{mod, influence}.rs`. Testkit: `src/script/{mod, runner, setup, path, matchers, expr, tiles}.rs`, the harness `tests/rules.rs` (libtest-mimic, `harness = false`, one trial per script) and `tests/engine/{scenario, tools}.rs`. Shared: `tests/rules/README.md` (the language, the `inspect` shapes, the test operations and the arena's anchors), `tests/rules/maps/arena.json`, `_selftest.toml`, `normalize.json`, `intended.toml` and nine scripts. Python: `citar/engine/{inspect, testops}.py`, `EngineGame.inspect` and `EngineGame.test_ops` (not in `__all__`), `tests/rulescript.py` and `tests/test_rule_scripts.py`.
- **Python's reading of values** is `base::py`: truth, `repr`, `str`, `int()` (a float truncates; a string is an integer literal after Python's own transformation: non-ASCII spaces become spaces and decimal digits of any script, Unicode 16's 76 runs, become ASCII, then only the ASCII spaces around it are skipped, so U+001C to U+001F refuse; single underscores between digits) and `float()`. `state::players` uses its `repr` and `truthy` for the seat messages. An integer beyond `i64` is refused where Python made a big integer.
- **`normalize`** (`api::tools::normalize(tool, args)`, `normalize_with(&ToolArgs, args)`) ports `tools.py:113-127`: the missing required parameters are reported first, in the order the spec requires them; unknown keys are dropped; then, in the order the tool declares its parameters, integers go through `int()`, boolean strings are `true`, `1` or `yes` in any case, and array strings split at commas into their `str.strip`ped non-empty parts. Arguments that are neither an object nor false are refused (`BadParam`). `ToolArgs { tool, params: [(name, ArgType)], required }` is `api::tools::args`; `TOOLS` is empty until the system packages add their actions' specs, so `normalize` of any tool is `UnknownTool` today. `tests/rules/normalize.json` holds 43 cases both engines run (gate 4), and two only Rust runs, marked `intended` (a big integer, a list of pairs). Package 1d-01's `execute` should run `Game::guard` before `normalize`: Python refused a caller who may not act before it looked at the arguments.
- **Scenario operations** (`api::scenario`): `OPS` lists all 16 of `scenario.OPS` with their parameters, sorted, each with a `Porting`: `grant_era`, `grant_tech`, `remove_tech`, `set_player`, `set_tile`, `meet`, `set_relation`, `set_influence`, `reveal` and `set_research` are ported; `found_city`, `set_city`, `remove_city` and `adopt_policy` are `Pending("1b-07")` and `add_unit` and `remove_units` `Pending("1c-02")`, and refuse with `ErrCode::NotPorted` (`not_ported("game::...")`). The parameter helpers `pid`, `players`, `tile` and `resolve` keep `_pid`'s, `_players`', `_idx`'s and `_name`'s messages. An unknown operation is named with Python's `!r` (`unknown op 'x'`). The differences are in the module doc, each an entry of `tests/rules/intended.toml` cited at its fix: `set_tile` refuses a terrain, feature or wonder of the wrong kind (`scenario-set-tile-checks-kinds`); numbers must be finite and fit their field (`scenario-numbers-finite-and-in-range`); `set_relation` refuses a war with a friendship, pact or open borders in force (DIPLO-1; `scenario-war-refuses-standing-treaties`); `set_influence` takes a major only (`scenario-influence-majors-only`); a `techs` string is one name (`scenario-techs-string-is-one-name`); a wrong type is refused with a sentence (`scenario-errors-are-sentences`). A scenario's opinion is the holder's own `OpinionKey::Scenario`, clamped to ±100 like every reason, which Python stored under a key its `opinion()` never read (`scenario-opinion-counts`; Python's `inspect` reports `diplomacy.opinion`, so scripts mark the checks that see it). `cargo refcheck changelog` lists `tests/rules/intended.toml` after `refcheck/intended.toml` (an id may be in one only), and a citar-refcheck test fails when an entry of it is cited nowhere in the engine.
- **The rules the operations need** are ported where they belong and marked where they wait: `research::{is_repeatable, is_unresearchable, path_to, plan_research, apply_research, research_result, add_tech, remove_tech}` (setting research is split as the action pipeline runs it: `plan_research(&Game, ..) -> Result<Vec<TechId>, ActionError>` reads and refuses, `apply_research` writes the queue and goal and cannot fail, and `research_result(&Game, p, &path)` renders the tool's result after the settle, so package 1b-07's `set_research` action wraps them as `Rule::check`, `Rule::apply` and `OutcomeSpec::render`; the era, obsolete units, progress and `turns` are `Pending("1b-07")`, the triggers `Pending("1b-08")`; a granted tech is announced "Rome was granted Pottery." where Python wrote "Rome scenario Pottery.", `scenario-tech-announcement-wording`); `diplomacy::relations::{set_war, make_peace, has_pact, is_friends, has_embassy, opinion, add_opinion, set_opinion}` with `WarReason` (cancelled negotiations `Pending("1c-05")`, the city-state reactions and protection `Pending("1c-06")`, units going home `Pending("1c-02")`, the triggers `Pending("1b-08")`); `city_states::influence::{influence, raw_influence, set_influence, add_influence, update_ally}` with `MIN_INFLUENCE` and `ALLY_INFLUENCE` (a new ally's enemies become the city-state's; its marriage cooldown reads the civilization's index, empty until 1b-05). `set_war`, `make_peace`, `set_influence`, `add_influence` and `update_ally` return `Result<(), StateError>`: a write the state refuses is an engine bug, returned instead of leaving a rule half applied, and the scenario operations report it (`The game refused the edit (...)`), after which `apply_ops` puts the game back.
- **Host methods** (`api::game`): `Game::apply_ops(&Value) -> Result<(Vec<Value>, EventBatch), ActionError>` clones the game, applies, settles, and puts the clone back on the first failure (gate 5: the digest, the history and the revision are as before). `meet(a, b)` returns `Result<EventBatch, ActionError>` (a poisoned game refuses it). `set_controller(pid, Controller, Option<Handicap>, AutoOverrides)`, since the seat takes the decisions it names explicitly, and `set_difficulty(pid, name) -> bool`.
- **Test operations** (`api::testops`, feature `test-ops`): `TEST_OPS` lists 22, all or nothing like `apply_ops`. Ported: `clear_units` (`all` includes the barbarians), `set_turn`, `unmeet`, `set_controller`, `set_auto`, `refresh_visibility` (every player's sight flagged for the settle) and `reload` (a copy whose journal starts afresh writes the whole history as one chunk, and the game loads from that save). Pending: `end_turn`, `end_round`, `force_turn` (1b-03), `complete_construction` (1b-07), `set_unit`, `ready_unit` (1c-02), `capture_civilian`, `attack_as` (1c-03), `automate`, `progress_builds` (1c-04), `add_spy`, `close_negotiation`, `open_negotiation_as` (1c-05), `barbarian_act`, `sack_city` (1c-06). Python's `testops.py` has the ported ones (`reload` is `EngineGame.test_ops`' own, since it replaces the game).
- **`inspect`** (`api::inspect`, feature `test-ops`): `game`, `player`, `tile`, `relation`, `unit`, `units`, `city`, `events`, `find_tiles`, `ops` and `pending`, the shapes of `tests/rules/README.md`. `QUERIES` lists them with a `Porting` each: `negotiation` is `Pending("1c-05")`, `view` `Pending("1d-02")` and `briefing` `Pending("1d-03")`, refused with `ErrCode::NotPorted` until those packages fill them in (and add them to `citar/engine/inspect.py`). `pending` lists the queries (kind `inspect`), scenario operations and test operations waiting for a package; package 1b-03 adds its stage tables' pending stages there. Python answers `[]`.
- **Runners.** Both read the same TOML and share the language: step kinds `op`, `ops` (a list applied as one `apply_ops`, added for the all-or-nothing script), `tool`, `check`, `new_game` (a game from the script's settings with keys replaced, or its refusal: the seat-setting refusals), `set` and `repeat`, with `as` (`op`, `ops`, `tool` and `check` only), `error` (`op`, `ops`, `tool` and `new_game` only; elsewhere the runners refuse it, so no check passes unread), `must_fail`, `intended` (cited ids must be in `refcheck/intended.toml` or the new `tests/rules/intended.toml`, for differences no refcheck group shows) and `coerce`. Values starting with `=` are expressions, also in matcher values. The runners' settings are `seed = 1` and two players, then the bare prelude, then the script's `config`, then `nation = "BenchmarkCiv"` for any seat that names none, so no nation's ability leaks into a script and seats are named by number. The Rust runner turns every check on (`DebugOptions::ALL`) and fails a step that leaves a violation. Until `Game::new` (1b-03), `script::setup::new_game` builds bare games from the settings on the inline map: seats and their checks, nations (the first free ones where Python drew at random), city-states and the barbarians, the map's own starts, starting techs, gold and culture, continents; no units, camps or first turn. The Rust runner refuses `start = "full"` until 1c-09. Python's `Game.new` also emits `turn_start` and `game_start` and its units explore around the starts before the prelude clears them, so scripts count events by type and never assume nothing is explored.
- **Scripts.** `_selftest.toml` (every matcher both ways, expressions, tile references and selectors, errors, the numbers-as-strings lint, new games, `repeat`), the four test_controllers ports `controllers_seat_defaults`, `controllers_seat_overrides`, `controllers_set_controller` and `controllers_reload_keeps_seats`, and `scenario_research`, `scenario_players_and_tiles`, `scenario_relations`, `scenario_influence` and `scenario_apply_ops` (whose checks after the failed list are `intended = "atomic-apply-ops"`). Every entry of `tests/rules/intended.toml` has at least one step or `normalize.json` case that shows it, which only Rust runs. test_controllers' handicap-and-cost test waits for research costs (1b-07).
- **Kitchen sink.** No extra unique type names a system of this package: the types staged `api` (`BuildImprovements`, `NuclearWeapon`, `CreateWaterImprovements`, `MustSetUp`, `ReligiousUnit`, `FoundCity`) are all used by the shipped ruleset and read by the tools and views of 1d.

### 9.4 Invariants and the cache oracle

- **`game::invariants::check(&Game) -> Vec<Violation>`:**

  | Code | Invariant |
  |---|---|
  | ID-1 | keys equal ids; ids unique; every live id below its counter; deals and negotiations in ascending id order (1c-05) |
  | OCC-1 | occupancy and `by_owner` match the units; carried units share their carrier's tile |
  | UNIT-1 | owner alive; tile in bounds; hp in 1..=100; 0 ≤ moves ≤ max moves; xp ≥ 0; stacking rules |
  | CITY-1 | `tile.city` and owner consistent; pop ≥ 1; 0 < health ≤ max; queue valid |
  | CITY-2 | worked tiles belong to the city and to no other city; worked + specialists ≤ pop; lists sorted |
  | TILE-1 | `tile.city` refers to a live city with the same owner |
  | PLAYER-1 | every float finite; stocks non-negative except gold, which may go negative under bankruptcy (`economy.py:732-735`) |
  | PLAYER-2 | one capital per living major that has cities; a dead player has no units or cities |
  | DIPLO-1 | war and met symmetric; war excludes open borders, friendship and pacts; `war_mask` and `met_mask` match the matrix |
  | NEG-1 | an open negotiation awaits one of its parties; `seq` has no gaps; history within the cap |
  | VIS-1 | visible ⊆ explored |
  | TURN-1 | `current` alive, or the game over; `winner` set only if the game is over (a game may end with none: the turn limit with the Time victory off, `victory.py:363-365`) |
  | PEND-1 | `pending` is empty at every settle point |
  | SETTLE-1 | settle converged within its pass cap; a chain of one-time effects stayed within its depth (`triggers::TRIGGER_DEPTH`, 1b-08) |

- **When invariants run.** Whenever `DebugOptions.invariants` is set, which is the default in debug builds and in release builds with the `checks` feature:
  - at the end of every `end_round`;
  - after every public call in testkit.
- **`verify_caches`** does three things:
  - it recomputes every memo cold into a scratch `Derived` and compares;
  - it rebuilds visibility and the met sets from scratch;
  - it runs the citizen oracle (§6.8).

  It runs at every settle when `DebugOptions.verify_caches` is on. Testkit turns it on in scripts, properties and chaos.
- **Decision (merging the two checks).** The code is compiled under `cfg(any(debug_assertions, feature = "checks"))`, and `DebugOptions` switches it on at runtime.

### 9.5 Properties, chaos and fuzzing

- **Generating actions.** `ActionSpec { tool: u8, actor: u8, target: u16, coords: (i16, i16), shape }` binds late:
  - targets are indices modulo the current candidate lists;
  - arguments are built from the tool argument specs: 70% valid, 20% with confused types, 10% random JSON.
- **Properties:**

  | # | Property |
  |---|---|
  | P1 | no panic |
  | P2 | a refused call leaves the digest unchanged, emits no event and does not settle |
  | P3 | invariants pass |
  | P4 | `verify_caches` passes (every 10 actions) |
  | P5 | refusal text follows the rules in §8.5 |
  | P6 | save, then load, gives an equal digest (every 25 actions) |
  | P7 | no stall: at most players + 1 `end_turn` calls advance the turn or end the game |
  | P8 | reads are free: any number of queries, views, briefings, snapshots, saves and refused calls inserted between two actions leaves every later digest unchanged |

  Starting states are generated duel and small maps, or committed fixtures. CI runs 64 cases and the nightly run 10,000. Regressions are committed.
- **Pure properties:**
  - hex: symmetry, triangle inequality, wrap distance against BFS;
  - A* cost equals Dijkstra's, with dynamic node checks included;
  - incremental visibility and met sets equal a full rebuild;
  - RNG streams are independent;
  - combat damage is monotonic in strength;
  - `EntityStore` behaves like a `BTreeMap` model;
  - CSR builds are independent of source order;
  - the journal chunk round-trips.
- **`RandomAgent`** plays any seat through `act`, drawing from `Purpose::TestAgent` keyed by `[pid, turn]`.
  - It starts in 1b-03 able only to end its turn. Each system package teaches it its own actions.
  - In the end it researches, builds, founds cities, fights when a preview allows it, adopts policies, negotiates (with p = 0.02), declares war (p = 0.005 after turn 50), and ends its turn.
- **`chaos`** mixes `RandomAgent` turns with `ActionSpec` noise under `catch_unwind`. Each failure writes a replay file to `target/chaos/`, and `chaos --replay` reproduces it.
- **`soak`** plays games on every map size with invariants on.
- **Decision (fuzzing).** Proptest and chaos are the CI gates. cargo-fuzz needs nightly and has weak Windows support, and with `forbid(unsafe_code)` sanitizers add little. `crates/citar-engine/fuzz/` still holds two targets, `load_state` and `fuzz_one(&[u8])` over `arbitrary`, for nightly runs in WSL. They are not a gate.

### 9.6 Determinism matrix and golden sets

Golden sets live in `crates/citar-testkit/golden/` and are staged as the engine grows:

| Package | Golden set |
|---|---|
| 1a-02 | `rng.json` (the first 32 values of 8 streams), `libm.json`, `pyfmt.json` (Python `repr` and `round(x, n)` table) |
| 1a-03 | the `RulesetId` of the embedded ruleset |
| 1a-05 | `uniques.json`: every compiled unique of the embedded ruleset, one row each (source, text, type, role, flags, parameters, modifiers, key); every source object's `all` range, partitions and tags, one row each; and the interned fractions, stats, static and object filters, tags and abilities |
| 1a-06 | `filters.json`: every dynamic filter of the embedded ruleset, compiled, one row each; `gen.json`: the tables map generation, the AI and victory read, and every map-generation unique they hold |
| 1a-09 | 3 known-answer state digests |
| 1a-10 | `convert-*`: the digests of the 12 committed fixtures right after conversion, before any settle |
| 1b-04 | `map-*`: 10 generated maps (duel to huge, every map type), hashed tile arrays |
| 1c-09 | `newgame-*`: 10 new games (duel to huge, every map type), once every setup stage is ported |
| 1c-10 | `load-*`: the 12 fixtures after `from_python`'s settle; `pass-*`: 20 rounds of passing from 3 fixtures, one digest per round; `random-*`: `RandomAgent` games (duel for 200 turns ×2, small 120 ×2, standard 60, large 30) |
| Phase 2 | `bot-*` |

- **The file format.** `determinism.json` is `{format:1, blessed_with:{engine_build, toolchain}, games:[{name, spec, chain_root, digests[]}]}`.
- **Commands.** `cargo golden check|bless|diff` is an alias for the testkit `golden` binary. `bless` refuses while any stage the set depends on is `Pending`.
- **On failure,** the job uploads the canonical state JSON at the first turn where the targets diverge, for `golden diff` to compare with refcheck's comparator.
- **Decision (golden location).** The testing area's layout wins, since the games need `RandomAgent`, which lives in testkit.
- **Budgets.** Pull requests run the short set, under 60 seconds per target. The nightly run does everything.

### 9.7 Benchmarks

**criterion (`citar-bench`)** is pinned to core 0, a P-core, via `core_affinity`, on AC power with the High performance plan.
- **Kernels:**
  - hex; `find_path` over the recorded `movement.paths` pairs; `reachable`;
  - the visibility step and full rebuild;
  - owned tile yields: cold, warm at a stable revision, and first read after one unrelated change;
  - city stats of all cities; citizen assignment; happiness of all civs;
  - combat damage over the recorded pairs;
  - digest, snapshot, `to_json`, load, ruleset load.
- **Macro benchmarks:** `turn/pass_round/<fixture>`, `view/god`, `view/player`, `briefing`.

**Python ratios** come from `scripts/refcheck/turn_timing.py`, which records Python pass rounds on the same corpus states into `refcheck/perf/python-turns.json`.

**`perfgate`** (`cargo xtask perf`) checks `estimates.json` against `thresholds.toml`, and prints each benchmark with its Python ratio. Until 1e-03, thresholds are report-only, except a hard failure above 3x the budget. That catches an algorithmic mistake, such as an interpreted filter, without failing on laptop noise while the Python baseline holds 8 workers. From 1e-03 on, the thresholds are hard.

**Decision (the CI performance gate).** Per-PR instruction counts: gungraun (the renamed iai-callgrind, 0.19) on 8 kernels and 2 macro benchmarks against the base branch, with +5% failing. Shared runners vary 10-20% in wall clock; instruction counts are stable to about 0.1%. The weekly criterion run stays in `nightly.yml` as a trend.

### 9.8 Statistical comparison (Phase 2), and the Python baseline question

**The Phase 2 protocol:**
- `citar-sim baseline` writes `refcheck/baseline/rust-<build>-<bot>.jsonl` in exactly baseline.py's schema, with the same spec rotation and the same aggression formula (`0.25 + 0.5*((pid*37 + seed) % 10)/9`, common.py:292).
- A Rust test round-trips `python-small.jsonl` through a `BaselineLine` type, so the schema cannot drift.
- Rust plays 120 small games against Python's 60.
- `summarize.py` gains three things: a `d` computed from per-game means (civs in one game are correlated), a bootstrap 90% interval, and `--split-half` for the noise floor.
- Every `**` row (|d| ≥ 0.5) is explained or fixed. Rust crashes must be 0.
- Every intended rule fix moves the distributions a little, so this is a coarse sanity check. It exists to catch a gross porting mistake, such as cities that never grow or wars that never end.

**The owner's question: is the Python baseline needed, given a week of Python games on the laptop and server?**

**Decision: finish the small run and the standard/large run; skip gargantuan.** The week of games cannot stand in for the baseline:
- **The lab games are the wrong kind of evidence.** The 586 lab games (archived under `saves/_archive_2026-09-23_pre-0.1.6/saves/lab`) were played by frozen bots and parameter variants, on engine builds from before Phase 0. Phase 0 split the bot's diplomacy random stream and changed its counters (phase0-spec A3.4-A3.5).
- **The lobby and server games mix seats.** They have LLM and human seats.
- **None of them is in baseline.py's schema.** That schema has per-civ stats at turns 100, 200 and 300, wars, captures, and one engine and bot hash. The old games also lack its uniform rotation of sizes, maps and barbarian settings, so `summarize.py` cannot compare them like for like.
- **The reference cannot be recreated later.** The port targets the live bot on engine `5287456aff`, the same engine hash as the fixtures. Only that code gives a like-for-like reference, and once the Python engine is archived (Phase 2.6) it cannot cheaply be run again.
- **It costs almost nothing.** The laptop is free, nothing in Phase 1 waits for it, and the results are first used in Phase 2.2.

**Where the run stands.**
- The small ×60 run started at 04:58. At 06:30 it had 28 of 60 games, at 800-2,250 seconds per game on 8 workers.
- It should finish around 08:15.
- The standard and large ×24 run follows, probably into the afternoon.
- With 60 games × 4 civs and an intra-game correlation of about 0.3, the standard error of d is about 0.13, so `**` sits at about 4 standard errors.

**What to do with each part of the run:**
- **Small ×60:** let it finish.
- **Standard and large ×24:** let it finish. It can only detect gaps of d ≥ 0.5, but it covers late-game code paths with more civs.
- **Gargantuan ×2:** skip it. Two games are not a distribution, and `profile.json` already has the gargantuan timings. It could hold the laptop for up to six more hours (`--max-minutes 360`) while benchmarks run.
  - When `run_phase0.log` shows `start: baseline gargantuan x2`, kill that step's Python process.
  - Do **not** edit `run_phase0.sh` while it runs: bash reads a script as it goes.

**What else follows:**
- Keep `CARGO_BUILD_JOBS=4` until the run ends. Criterion numbers taken meanwhile are indicative (§9.7).
- The archived lab games are still useful for calibration. An optional 30-line adapter measures d between two real bot versions, which is the scale for judging a Rust-vs-Python d.

### 9.9 What each sub-phase makes testable

| Sub-phase | Newly testable |
|---|---|
| 1a | RNG, libm, Python-format and hex vectors on 5 targets; the ruleset compiles; the `uniques` and `state_echo` groups; `convert-*` goldens |
| 1b | The following, plus about 35 scripts:<br>- turn flow and setup skeletons with stage tables;<br>- the `tile_yields`, `city_stats` and `buildable` groups, and the economy paths of `civs`;<br>- mapgen properties and `map-*` goldens. |
| 1c | The following, plus about 70 more scripts and save and load every round:<br>- the `visible`, `fixed_point`, `movement`, `combat_previews` and `deal_checks` groups, and the rest of `civs`;<br>- `newgame-*`, `load-*`, `pass-*` and `random-*` goldens. |
| 1d | `tool_errors`, `views`, `briefing`: every group enforced |
| 1e | P1-P8 at scale, chaos, soak, the full matrix, hard performance gates, the full corpus `--strict` |

---

## 10. Performance budget

Figures are for one P-core of the laptop (i5-13420H), release build, criterion, on corpus fixture states. Until 1e-03 the gate is report-only with a hard failure above 3x the budget; from 1e-03 it fails at 1.5x the budget. Backstops are hard failures, independent of the Python ratio.

| Operation | Budget | Python today |
|---|---|---|
| Ruleset load (embedded) | ≤ 20 ms | ~70 ms |
| Tile yield | hit at a stable revision ≤ 5 ns; first read after one unrelated change ≤ 50 ns; recompute ≤ 1 µs | 43 µs cold; 98.7% of recomputes unchanged |
| City stats recompute | ≤ 10 µs | ~0.7 ms |
| Citizen assignment, pop 20 | ≤ 10 µs | 9.5 ms |
| Happiness recompute, cities cached | ≤ 3 µs | ~12 ms late |
| Civ index | lookup ≤ 20 ns; rebuild ≤ 10 µs | 1-15 µs per `civ_uniques` call |
| Visibility after one step | sight 2 ≤ 1.5 µs; sight 5 ≤ 4 µs | 0.9-8.6 ms per refresh |
| Line of sight, sight 3, uncached | ≤ 1 µs | 195 µs |
| A* | small, 30 tiles ≤ 20 µs; gargantuan through fog, 54 tiles ≤ 150 µs; mean over recorded pairs ≤ 50 µs | 2.5 ms; 67-167 ms |
| `reachable_this_turn` | ≤ 5 µs | — |
| Combat preview | ≤ 1 µs | ~12 stack builds |
| Buildable per city | recompute ≤ 20 µs; hit ≤ 50 ns | 4.5 ms |
| Connectivity per civ | small ≤ 30 µs; gargantuan ≤ 300 µs | 831 BFS searches per round |
| Job map per civ, small, full | ≤ 200 µs | 16% of late game |
| Settle with nothing pending | ≤ 0.5 µs | — |
| `view(pid)`, small T300 | ≤ 1.5 ms | 33 ms |
| God view | large t280 ≤ 20 ms (plan); gargantuan ≤ 12 ms target | 119 ms (huge) |
| Briefing, small T300 | ≤ 1 ms | 33 ms |
| `snapshot()` under the lock, gargantuan T300 | ≤ 10 ms (plan: ≤ 100 ms) | 4.3 s save at T151 |
| `to_json` (off the lock) | small t280 ≤ 20 ms; gargantuan ≤ 100 ms | — |
| Load, small t280 | ≤ 30 ms | — |
| Digest (64 KB block buffer) | gargantuan ≤ 5 ms; small ≤ 0.2 ms | — |
| Map generation | small ≤ 50 ms; gargantuan ≤ 1.5 s | up to 7 s |

**Pass rounds** (engine only, no agent moves):
- at least 20x Python on every corpus state, with backstops of 150 ms for small t280 and 400 ms for large t280;
- target budgets: small ≤ 2 / 8 / 20 ms at turns 50 / 150 / 300, and gargantuan ≤ 40 / 150 / 350 ms.

**Whole games:**
- Phase 1 gate: an engine-only `RandomAgent` small game of 330 turns in 10 seconds or less.
- Phase 2 floors: a small 4-bot game in 25 seconds or less (target 5 s), and gargantuan at least 30x faster than Python (target 90 s or less).

**Enforcement:**
- criterion thresholds on the laptop (`cargo xtask perf`, hard from 1e-03);
- gungraun +5% per pull request in CI;
- the `stats` feature counts memo hits and misses, so a soak can assert that redundant recomputes stay under 5% per memo type. Python measured 74-99.5%.

**`overflow-checks = true`** in release stays unless the bench report shows it costs more than 3%.

---

## 11. Risks

1. **A cache could go stale:** a touch with the wrong flag, or a wrongly classified `CondDeps` on one of the 94 conditionals.
   - Mitigation: `&mut` access only through `game::mutate`, enforced by `xtask check`; `#[must_use] Change` with `unused_must_use` denied.
   - Mitigation: memos that validate themselves on every read.
   - Mitigation: `verify_caches` at every settle in scripts, properties and chaos; the citizen oracle; the `stats` redundancy counters.
2. **A memo dependency cycle.** It panics on the `RefCell`, which is visible in tests and a poisoned game in release. Cycles in the rules are broken by persisted lagged values; the memo graph in §6.5 is acyclic by construction and at most 8 deep.
3. **Refcheck floods with echo differences** when a formula is redesigned.
   - Mitigation: groups are fixed in dependency order; `where` entries can cover several locations; value constraints; the ratchet.
   - Mitigation: `PyFloat` and `round_ndigits` for text.
4. **Behaviour differences refcheck will flag and a person must judge:**
   - the happiness lag and its commit stages;
   - citizens always fully reassigned, ranked on `happiness_seen`;
   - `last_gold_rate` now written;
   - equal-cost A* paths;
   - ring order in `within`;
   - the Marble fix;
   - fallout;
   - scenario opinions;
   - met lists by id;
   - the advisor's tie order.

   Each needs an intended entry. If an entry is written too broadly, it hides bugs.
5. **The performance budget assumes compiled filters** (bitset tests). An interpreted filter would put tiles 5-10x over budget. The 3x backstop in 1b-06 catches it early.
6. **Full unique support** (the owner's decision on open question 2): 125 types the shipped ruleset never uses must still be right, and only the kitchen-sink ruleset exercises them.
   Mitigation: each type names the stages that read it, each 1b and 1c package tests its own with the kitchen sink, and refcheck states from rulesets that use them can be recorded later.
7. **The `EvalWorld` trait churns** while `State` settles. Pin it in 1a-07; `EvalView` is its only production implementation.
8. **Porting a large piece of the bot for the advisor** in Phase 1 (1c-07) may drag bot concepts into the engine. It is kept to production valuation, and the rest of the bot stays out of the engine.
9. **The corpus (44 MB) is git-ignored.** Without the prerelease asset (open question 1), the full refcheck runs only on the laptop, and CI covers 12 states.
10. **`include_bytes!` reaches outside the crate until the Phase 2 data move.** The maturin sdist must carry the data. There is a CI build from the sdist in Phase 2.
11. **Toolchain or dependency bumps change tie order or float bits.**
    Mitigation: exact pins, a committed `Cargo.lock`, Dependabot ignoring `libm`, and re-blessing in a pull request.
12. **The `macos-15-intel` runner may be retired** during the release. Fallback: x86_64 under Rosetta.
13. **The laptop is a noisy benchmark host** (hybrid cores, and the Python baseline running until the afternoon).
    Mitigation: core pinning, report-only thresholds with a 3x backstop until 1e-03, and instruction counts in CI.
14. **OneDrive and parallel agents.** A `target/` inside OneDrive, or one target directory shared by several worktrees, breaks or serialises builds. Mitigation: a `CARGO_TARGET_DIR` per worktree (§2.7).
15. **Recursion on Windows.** The 1 MB main-thread stack aborts the process when overflowed.
    Mitigation: iterative BFS and flood fills, filter depth caps at load, and bounded memo depth.
16. **Refusal atomicity (P2) may force redesigns** of Python tools that mutate before they refuse. This is intended, but it adds work to each system package. Any genuine exception needs an allow-list entry in `props.rs` with a reason.
17. **Stage tables hide missing work.** A `Pending` stage silently does nothing.
    Mitigation: `inspect` lists pending stages; goldens refuse to bless over them; `xtask check` fails on any pending stage from 1c-10.
18. **History moves into journal chunks.** Replays and `events()` depend on the Phase 2 bindings wiring chunks through `citar-store`. An end-to-end replay test is planned for Phase 2, and `replay.js` learns the delta format in Phase 3.
19. **The per-round digest at gargantuan** (about 2-3 ms) is opt-in and has a benchmark. Per-layer running hashes are the fallback if it grows.
20. **Estimates exceed plan §8** (35-45k lines for the engine). The work breakdown totals about 88k lines including unit tests, refcheck, testkit, bench and xtask; non-test engine code is about 48-52k. That is scale risk, contained by the phase gates.

---

## 12. Glossary

| Name | Meaning |
|---|---|
| `AbilityKey` | Rule id of a limited-use unit action unique. Replaces the `"placeholder|params"` keys. |
| `Action` | Typed player action (serde enum, tagged by tool name). Dispatched by `Game::act`. |
| `ActionError`, `ErrCode` | Model-facing refusal message plus a stable code |
| `ActionMods` | Folded unit-action modifiers: consume, times, extra, move cost |
| `AdvisorParams` | Production advisor parameters (the live bot's defaults) |
| `BaseUnitId` / `UnitTypeId` | A row of `units.json` / a row of `unit_types.json` |
| `BitSet` | Dense `Vec<u64>` bitset: explored tiles, visible tiles, zone of control, rechecks |
| `BuildQueue`, `BuildStep` | Work in progress on a tile, in the `Tiles::builds` side table |
| `CANON_V1` | The canonical binary encoding hashed by the digest |
| `CanonSerializer` | serde `Serializer` that writes `CANON_V1` through a 64 KB block buffer |
| `Change` | `#[must_use]` record returned by state setters whose effects need the new state; passed to `Game::changed` |
| `Chronicle`, `ChronicleHeads`, `HostHeads` | In-memory history / engine counts and running hash (digested) / host counts and the event id sequence (saved, not digested) |
| `CivIndex`, `CivIndexFull`, `CityLocal`, `FollowerIndex` | Unique index memos (CSR) |
| `CivSources` | Everything that contributes uniques to a civ, gathered from `State` |
| `Cond`, `CondData`, `CondDeps` | Compiled conditional / its payload / the input classes it reads |
| `Constructible` | Building, Unit or Perpetual (Gold, Science, Nothing) |
| `ConvertReport` | What the strict Python converter dropped |
| `CopyMemo<T>`, `Memo<T>`, `Stamp` | Self-validating cache cells: small `Copy` values in a `Cell`, large values in a `RefCell` / the `verified` and `changed` revisions |
| `Countable` | Compiled countable expression |
| `Csr` | Compressed sparse rows: slot starts plus sorted `(UniqueId, n)` entries |
| `Ctx`, `CombatCtx` | `Copy` evaluation context |
| `DealItem`, `Terms`, `Deal`, `Negotiation`, `NegEntry` | Diplomacy records (the Phase 0 chat model) |
| `DebugOptions` | `{ invariants, verify_caches }`; not saved |
| `Derived` | All caches; a pure function of `State` |
| `DetMap` / `LookupMap` / `MinHeap` | Ordered hash map / hash map that can only be looked up / heap with a push-sequence tie-break |
| `Digest`, `DigestChain` | blake3 of the canonical state / the per-round chain |
| `Drivers`, `SeatDriver`, `Stop`, `drive` | The AI turn state machine and the boundary to the bot |
| `DriverMemory` | Opaque driver state saved and digested on a seat |
| `EffectQueue`, `Effect` | Follow-ups from derived reactions that write state, applied in a fixed order |
| `EngineError`, `LoadError`, `LoadReport`, `RulesetErrors`, `ConvertError`, `ValidationError` | Error and report types |
| `EntityStore<I, T>` | Id-ordered store with tombstones for units and cities |
| `EvalWorld`, `TileFacts`, `EvalView` | Fact trait for the unique evaluator / map-generation subset / its production implementation |
| `Event`, `EventType`, `EventData`, `NameRef`, `EventBatch` | Typed events / the events a call appended |
| `FeatureSet`, `FeatureId` | u16 bitset of terrain features / index among the 10 features |
| `FracId` | Interned fractional unique parameter (`Ruleset.fracs`) |
| `Game` | One game: `rules`, `st`, `dv`, `chron`, pending work, effects, frames, debug options, poison flag |
| `GameConfig`, `HostOnly`, `MapSource` | Typed configuration / values saved but never hashed / generated map, or an editor map by id |
| `HexGrid` | Map geometry derived from `MapInfo` |
| `IdCounters` | Persisted id counters, including `combat_seq` |
| `IdSet<I, W>`, `IdVec<I, T>`, `PlayerSet`, `PlayerVec` | Typed bitsets and vectors |
| `JournalChunk`, `FrameWriter`, `ReplayFormat` | Chronicle delta for the host to append / keyframe and delta encoder / Full or Delta replay output |
| `KeyPart` | Encoding of RNG key parts; `None` is `u64::MAX` |
| `MoveClass`, `MoveCosts`, `RouteLayer`, `Zoc`, `PathTree`, `PathCache`, `PathScratch` | Pathfinding structures: static per-class costs, per-civ routes, zone of control, searches |
| `NewGame`, `MapDoc` | What `Game::new` sets a game up from: the settings and, for an editor map, its inline document, which is never saved |
| `OneTimeEffect`, `TriggerKind`, `TriggerCond`, `TriggerEvent` | Triggered effects |
| `OutcomeSpec`, `Outcome` | What an applied action reports / its rendering after the settle |
| `PairMatrix<T>`, `Relation`, `OpinionBook` | Triangular relation storage and its cell / sparse opinions |
| `Pending` | Work raised during a call (citizen rechecks, dirty vision sources); empty at every settle point |
| `Player`, `Seat`, `Controller`, `Handicap`, `AutoDecisions` | Typed player and seat (the Phase 0 split) |
| `Purpose`, `Rng` | RNG stream purpose (frozen discriminants) / xoshiro256++ keyed generator |
| `PyFloat` | Formats a float as Python's `repr` does |
| `QuestTarget` | Typed quest data |
| `RankCtx` | Hoisted citizen-ranking context |
| `Rev`, `Revs`, `CivRevs` | The clock / per-input revisions |
| `Ruleset`, `RulesetFiles`, `RulesetId`, `BUILD_ID` | Compiled rules / input bytes / canonical hash / build id for proof of work |
| `Snapshot` | Deep clone of `State` plus the ruleset, taken under the lock |
| `SourceUniques` | An object's uniques, partitioned by role |
| `Stage`, `Porting` | An entry in a turn or setup stage table / Ported or Pending(package) |
| `State` | The persisted model |
| `Stats`, `Stat`, `StatsId` | `[f64; 7]` in the order Food, Production, Gold, Science, Culture, Happiness, Faith / one stat / an interned stats payload |
| `Tile`, `Tiles`, `TileMemory`, `TileMemoryLayer`, `RouteBits` | Map storage |
| `Touch` (`CityTouch`, `PlayerTouch`, `UnitTouch`) | Flags naming what an entity edit changes; bumped before `&mut` is handed out |
| `Unique`, `UniqueMeta`, `UniqueType`, `UniqueData`, `UniqueId` | Compiled unique (hot part, 24 bytes) / its cold part / the generated enum / its payload / its id |
| `Visibility`, `CivVis`, `VisSource`, `LosCache` | Incremental vision |

---

## Appendix A: conflict log

Each line is: the conflict, then the choice. The reasons are given where each decision is made in the body.

**Between the area designs:**
1. **Bot location.** Bot in the engine (plan §3, pipeline area) against a separate crate (workspace area) → a separate `citar-bot` crate. The engine gets `SeatDriver` and `drive`; the advisor moves into the engine in 1c-07.
2. **RNG generator:** ChaCha8 via rand_chacha with blake3 keying against our own xoshiro256++ with SplitMix64 keying → xoshiro256++.
3. **RNG state:** `RngState{seed, draws}` against keyed streams only → keyed only, with a persisted `combat_seq`.
4. **Ruleset handle:** `Arc<Ruleset>` against `&'static Ruleset` → `&'static`.
5. **Ruleset identity:** FNV-1a over the bytes against blake3 over a canonical walk → blake3 canonical walk.
6. **Embedding:** `include_str!` against `include_bytes!` → `include_bytes!` feeding `RulesetFiles`.
7. **Generator:** Python `--rust` mode against xtask → `cargo xtask gen-uniques`.
8. **Unique index maintenance:** eager hooks against memo rebuild → memos with early cutoff.
9. **Condition scopes:** `Scope` against `CondDeps` → one `CondDeps` bitflags type.
10. **Change tracking:** change buffers against `#[must_use] Change` → `Change`, plus `Touch` (review).
11. **Tile storage:** bytemuck `Pod` → plain `#[repr(C)]` with `canon_bytes()`.
12. **zstd and journal framing:** in the engine against in the hosts → `citar-store` in Phase 2.
13. **Legacy loader name:** `py-state` against `legacy` → `legacy`, `compat::python`, test-only (review).
14. **Debug check features:** `oracle`, `invariants` or `verify_caches` → a compile feature `checks` plus runtime `DebugOptions`.
15. **Parallelism:** `par` and rayon now against none → none in Phase 1.
16. **Profiles:** `ci` against `release-checked` → `ci`.
17. **Tests** in `citar-engine/tests` against `citar-testkit/tests` → testkit.
18. **Benches** in the engine against `citar-bench` → `citar-bench`.
19. **Golden files:** engine with xtask against testkit with the `golden` binary → testkit.
20. **Refcheck gating:** `gate.toml` against `enforced.toml` with a ratchet → `enforced.toml` with a ratchet.
21. **Fuzzing:** cargo-fuzz in CI against proptest and chaos → proptest and chaos; cargo-fuzz targets for WSL.
22. **CI:** jobs in `test.yml` against separate workflows → separate workflows.
23. **Performance CI:** weekly criterion against gungraun per pull request → gungraun per pull request, criterion weekly for the trend.
24. **Determinism lint:** grep against clippy → clippy only, verified on 1.98.1.
25. **Naming a row of units.json:** `UnitTypeId` against `BaseUnitId` → `BaseUnitId`.
26. **`PromotionSet` width:** 2 words against 4 → 4.
27. **Id placement** → `base::ids`.
28. **`within` order:** Python's distance sort against ring order → ring order, documented.
29. **Event delivery:** `drain_events` against a returned `EventBatch` → `EventBatch`.
30. **When tools land:** step 7 against with each system → typed `Action`, argument specs and `normalize` with each system; the JSON registry in 1d.
31. **Refcheck loading:** a fresh load per group against one load per fixture → one per fixture, safe now that queries are `&self`.
32. **The citizen oracle against converted states** → `City.citizens_settled`.
33. **Unknown tool arguments** → dropped, as today.
34. **Crate versions** → the crates.io versions current on 2026-09-23 (§2.3).
35. **Supported uniques:** all 509 against the 402 used → the used set, until the owner chose every type Python handled on 2026-09-23 (package 1a-05b).
36. **The Marble unique** → LOCAL resource uniques apply only in cities with the improved resource.
37. **The `fixtures-late` pick** → small t280, standard t120, scenario-small t61.

**From the review** (each finding verified against the code):

38. **Work-package order (blocker).**
    - Accepted: `Game.new` calls research, city-state, unit, trigger, barbarian and visibility code and `begin_turn` (game.py:258-315).
    - Turn flow and setup become stage tables in 1b-03. Map generation (1b-04) is split from setup, which is completed in 1c-09, and 1c-08 is split into 1c-08, 1c-09 and 1c-10.
39. **Settle semantics.**
    - Accepted: settle runs to a fixed point, only after successful writes; pending work is never persisted.
    - Changed: `happiness_seen` commits at S1 and E1, not S1 and E5, so that end-of-turn economics see the turn's actions.
40. **Freshness of memos.**
    - Accepted: `Cell`/`RefCell` are `Send`, so the premise for "ensure, then read" was wrong. Memos validate on read through `&Game`.
    - Refined: large values are returned as `Ref` guards rather than `Arc` copies, which is safe because revisions only move under `&mut`.
41. **Unguarded writes.**
    - Accepted: `&mut` state only through `game::mutate` (xtask-enforced), with `Touch` flags for entity edits and `Change::Seat`.
42. **Host activity in the digest.**
    - Accepted: engine heads and host heads are split.
    - Rejected: the alternative of dropping the chronicle hash, since the hash is what pins event wording across platforms.
43. **Bot memory.** Accepted: `Seat.driver: Option<DriverMemory>`, frozen in save format v1, handed to drivers by `drive`.
44. **`found_city`.** Accepted: 39 of the 40 actions had owners. It is now in 1c-04.
45. **Tool results before the settle.** Accepted: `act` runs check → apply → settle → render (tools.py:680-757).
46. **First contact on owner changes.**
    - Accepted: triggers on `TileOwner`, `CityAdded`, `CityOwner` and `PlayerAlive`.
    - Accepted: `State::revive_player` for liberation (conquest.py:262-265).
47. **Pathfinding cache inputs.**
    - Accepted: static `MoveCosts` and `RouteLayer`, dynamic per-node checks, and a `u64` key that cannot underflow.
    - Python's optimistic fog rule is kept.
48. **Event scrubbing.** Accepted: private events, `mentions`, the coordinate scrubber, and a Unicode word boundary. Name matching also reproduces the regex's shorter-match fallback, which the review did not mention.
49. **`Unique` size.** Accepted: `FracId`, 12-byte payloads and `UFlags` in `CondDeps`'s top byte give 24 bytes.
50. **Python number formatting.** Accepted: `PyFloat` and `round_ndigits`, tested against recorded Python tables. There are 77 `round(` sites in `citar/engine`, not 88.
51. **Seeds and maps from the host.** Accepted: `config_from_json` requires a seed and an inline map; `generate_map` takes a seed; `Purpose::MapPrepare`.
52. **Determinism details.**
    - Accepted: `KeyPart`, and unique keys that include the source; `Purpose::Advisor`; the `f64::round` and `IndexMap::swap_remove` bans; hashing -0.0 as is.
    - Accepted with a note: `libm` default features are turned off, although its `arch` paths (sqrt, fma, rint) are correctly rounded and could not change a result.
53. **Optimistic budgets.** Accepted: the digest block buffer; the settle budget restated; the yield budget restated per revision state.
54. **Laptop performance gates.** Accepted: report-only with a 3x backstop until 1e-03; kill the gargantuan baseline step.
55. **Over-engineering.** Accepted: no lenient converter and no citar-py shipping; intended.toml v2 only; no invented `at` coercion.
56. **Package details.**
    - Accepted: sight and spy vision in 1c-01; religious unit actions in 1c-04; the happiness commit in 1b-06; host ops only in 1c-09; `not_met_dump.py` in 1a-07.
    - Additionally, city-state espionage (elections, coups) moves to 1c-06, and `load-*` goldens move to 1c-10 because the settle on load keeps changing until then.
57. **Saves across ruleset changes.** Accepted: a changed `RulesetId` is a warning, and unresolvable names are an error; proof of work checks the exact id separately. Unknown top-level keys are now an error in every build, rather than depending on the profile.
58. **Phase 2 boundary.**
    - Accepted: `SeatDriver: Send`; a `Delta` replay format for `replay.js` in Phase 3; met lists by id (an intended entry); `relocate` cascading to carried units.
59. **Script runners.** Accepted: `normalize` and the argument specs move to 1b-02, and Action field names follow the tool argument names.
60. **Worktree removal.**
    - Partly rejected: the critic listed priceless-turing as removable, but it holds an uncommitted 38-line test in `tests/test_bots.py`, so it is left alone.
    - `inspiring-wu` and `quirky-hugle` are removed.
61. **The PR to merge.** There is no GitHub pull request for `claude/sweet-raman-2ff95b`. Its commits are on `v0.1.6` (PR #5's head), so P1-00 only confirms this.

---

## Appendix B: Phase 1 work breakdown

Each package is sized for one implementation session and ends with its gates passing. Order follows the dependencies.

### P1-00 (1a): Branch and laptop housekeeping before Phase 1

- **Depends on:** nothing
- **Scope:** Orchestrator task, no Rust.
(1) Confirm the as_player fix is on v0.1.6: v0.1.6, origin/v0.1.6 and claude/sweet-raman-2ff95b all point at 4b5a912. No separate GitHub PR exists for the branch; its commits ship in PR #5.
(2) Remove the clean, merged worktrees .claude/worktrees/inspiring-wu-f6c9f3 and .claude/worktrees/quirky-hugle-e9a767. Leave .claude/worktrees/priceless-turing-9d3a63 alone: it holds an uncommitted 38-line test in tests/test_bots.py.
(3) Python baseline (refcheck/baseline/run_phase0.sh):
- let small x60 (28/60 at 06:30, due around 08:15) and standard/large x24 finish;
- when run_phase0.log shows 'start: baseline gargantuan x2', kill that step's python process;
- never edit the script while it runs.
(4) Build setup:
- a CARGO_TARGET_DIR outside OneDrive, one per worktree;
- CARGO_BUILD_JOBS=4 until the baseline ends.
- **Gates:** (1) git merge-base --is-ancestor claude/sweet-raman-2ff95b v0.1.6 exits 0.
(2) git worktree list shows neither inspiring-wu nor quirky-hugle, and priceless-turing still has its diff.
(3) python -m unittest discover -s tests is green.
(4) A trial cargo build writes nothing under the OneDrive folder.

### 1a-01 (1a): Workspace, toolchain, lints, CI skeleton and xtask check

- **Depends on:** P1-00
- **Estimated Rust lines:** 1000
- **Scope:** Workspace setup.
(1) Manifests and toolchain:
- root Cargo.toml as in design section 2.3, with libm default-features = false;
- workspace.lints, including unused_must_use = deny;
- profiles dev, release, ci and profiling;
- rust-toolchain.toml pinned to 1.98.1, and rustfmt.toml.
(2) Lint and tool config:
- the strict root clippy.toml: hash types; transcendental functions; f64/f32::round; IndexMap/IndexSet remove and swap_remove; unstable sorts; fs, env and thread functions; print macros;
- relaxed clippy.toml files for refcheck, bench and xtask;
- .cargo/config.toml aliases (xtask, refcheck, golden);
- .config/nextest.toml, whose ci profile has retries = 0.
(3) Crate stubs:
- citar-engine: lib.rs with forbid(unsafe_code), 64-bit and little-endian asserts, Send (not Sync) asserts, empty layer modules, and the features embedded-ruleset, legacy, test-ops, checks and stats;
- citar-testkit, citar-refcheck, citar-bench.
(4) xtask check:
- a normal-dependency allow-list from cargo metadata;
- the workspace version equals citar/__init__.py __version__;
- layering, by use crate:: paths per directory;
- restricted callers of the State mutable accessors (a rule that is live once 1a-08 adds them);
- generated files fresh (a hook for 1a-05);
- NotPorted and Pending-stage checks, switched on by later packages.
(5) Workflows and repo hygiene: .github/workflows/rust.yml (a lint job, and a test job on ubuntu, windows and macos-arm64); .gitignore and .gitattributes additions.
(6) Docs:
- crates/citar-engine/README.md: layer rules, and the eight rules no lint can see;
- the CONTRIBUTING Rust section: CARGO_TARGET_DIR per worktree, WSL builds in ~/, the dev loop.
- **Gates:** (1) cargo build and cargo clippy --workspace --all-targets --all-features -D warnings are green on Windows and in WSL.
(2) A throwaway branch with one violation of each disallowed type, method and macro (including f64::round and IndexMap::swap_remove), plus a for loop over a HashSet, fails clippy once per violation.
(3) xtask check fails on each seeded violation: num-traits added to the engine; a version bump in one place only; use crate::api inside game/.
(4) rust.yml is green on 3 OS.
(5) scripts/check_links.py --strict passes.

### 1a-02 (1a): base layer: ids, sets, collections, num, fmt, rng, order, hex, text, stats, digest writer

- **Depends on:** 1a-01
- **Estimated Rust lines:** 2600
- **Scope:** The base modules:
- base::ids: define_id!, every entity and rule id including FracId, and IdVec.
- base::sets: IdSet, BitSet, PlayerSet, PlayerVec.
- base::collections: DetMap/DetSet, LookupMap, MinHeap.
- base::num: libm wrappers, floor_div/floor_mod, round_half_even, round_half_away, round_ndigits (Python round(x, n)), saturating casts.
- base::fmt: PyFloat, Python repr of a float.
- base::rng: Rng xoshiro256++; Purpose with frozen discriminants, including Advisor, MapPrepare and NationShuffle; KeyPart (None = u64::MAX); keyed derivation; below/range/unit/chance/shuffle/pick/weighted.
- base::order: argmax_first.
- base::text: rules.py:26-30 normalisation, game.py:17-23 possessives, the Unicode word-boundary scanner, the coordinate scrubber for game.py:18.
- base::stats: Stat, Stats, StatMask.
- base::hex: HexGrid, porting hexmap.py:1-214, with ring-order within.
- base::digest: CanonSerializer implementing CANON_V1 over a 64 KB block buffer (floats as to_bits, NaN and infinity an error).
One-off scripts: scripts/refcheck/hex_vectors.py records Python HexGrid answers; scripts/refcheck/pyfmt_vectors.py records Python repr(float), round() and round(x, n) over about 2,000 inputs, including ties.
Also: determinism.yml (5 targets), and a minimal golden binary in citar-testkit for rng.json, libm.json and pyfmt.json.
- **Gates:** (1) Identical on the 5 targets: committed RNG vectors, a chi-square check of below() and unit(), libm vectors (about 2,000 bit patterns) and the pyfmt table.
(2) PyFloat and round_ndigits match every recorded Python answer, including 0.125, 2.5, 2.675, 1e16 and 1e-5.
(3) Hex vectors for duel, standard and gargantuan, with and without each wrap, match: neighbours, distance and line exactly; within and ring as sets.
(4) Proptests pass: BitSet against BTreeSet; MinHeap pop order against a sorted Vec with ties; floor_div/floor_mod against a table of Python results; KeyPart None differs from 0.
(5) CanonSerializer: known-answer vectors hold, -0.0 and 0.0 hash differently, NaN returns an error, and the block-buffered and unbuffered outputs are identical.

### 1a-03 (1a): Ruleset: raw layer, typed tables, loader, validation, names, constants, client JSON, RulesetId

- **Depends on:** 1a-02
- **Estimated Rust lines:** 2600
- **Scope:** Ports rules.py:37-342 and the ruleset data: the 22 ruleset files, custom/nations.json and game.json.
(1) Loading and ids:
- rules::source: RulesetFiles, embedded() via include_bytes, RulesetId as a blake3 canonical walk of the parsed JSON, BUILD_ID;
- rules::raw: serde with deny_unknown_fields, IndexMap tables, skipping _ keys, the custom nations merge.
(2) Tables:
- rules::defs: every typed Def, Constants, and the fracs table;
- rules::derived: tech_order, unlocks, upgrade_from, unique units and buildings per nation, major and city-state nations, great person units, spaceship parts, max_turns, stat_related, feature layer order, builder classes, feature removals.
(3) Checks and access: cross-reference validation into RulesetErrors, capacity checks against the IdSet widths, rules::names (resolve), and Ruleset::load/leak/shared/version/counts.
(4) Client output: rules::client, client_json in the shape of to_client (rules.py:303-331).
Unique texts are carried raw here; 1a-05 compiles them. A one-off script, scripts/refcheck/rules_dump.py, records Python rules_client() and a resolve table.
- **Gates:** (1) The embedded ruleset loads in under 20 ms in release (report-only; hard above 60 ms).
(2) client_json equals the recorded Python rules_client() when compared as JSON values.
(3) resolve() matches Python for every name and key of every table.
(4) RulesetId is the same across a CRLF-converted, re-indented copy, changes when one number changes, and is identical on the 5 targets (added to the goldens).
(5) Negative tests pass: a dangling reference, an unknown field, a table over capacity, a missing file.

### 1a-04 (1a): citar-refcheck skeleton: fixtures, comparator, intended v2, enforced list, ratchet, reports

- **Depends on:** 1a-01
- **Estimated Rust lines:** 2100
- **Scope:** Work in crates/citar-refcheck (the Rust side of refcheck/README.md), plus the fixture files.
(1) Loading: fixture.rs, with flate2 gzip; meta typed, state as RawValue, queries as Value; rayon across fixtures; results sorted by name.
(2) Comparing: the compare/ module, with:
- the path grammar and the numeric tolerance;
- keyed, multiset and custom lists;
- similar-based line diffs;
- the PathEquivalent and Better kinds;
- a CompareSpec per group.
(3) Accepted differences:
- intended.rs: v2 [[differences]] only, with value constraints, cases globs via globset, and stale detection;
- enforced.rs: refcheck/enforced.toml, with optional path globs;
- the ratchet: refcheck/ratchet.json.
(4) Reporting: human and JSON reports in dependency order.
(5) The CLI: run, explain, suggest, ratchet, changelog and list, with exit codes 0, 1, 2 and 3.
(6) Fixture files:
- copy the three late states into refcheck/fixtures-late/: small-continents-normal-s1025/t280, standard-pangaea-normal-s1031/t120 and scenario-small-continents-s3001/t61;
- rewrite the refcheck/intended.toml header for v2;
- update refcheck/README.md.
No engine groups yet; later packages add the answer modules.
- **Gates:** (1) Differ unit tests cover tolerance, key order, multisets, keyed lists, globs and PathEquivalent.
(2) A fixture compared with itself gives 0 diffs.
(3) A mutated copy gives exactly the expected diffs: one yield +0.5, a set-typed list reordered, a path rerouted at equal cost.
(4) Loader tests pass:
- a constraint mismatch stays unexplained;
- an unused entry is stale only when its cases are in the run;
- duplicate ids, unknown fields and the v1 inline form are rejected.
(5) The ratchet refuses an increase.
(6) cargo refcheck list shows 262 states, and the report JSON is byte-identical across two runs.

### 1a-05 (1a): Unique compiler: UniqueType generation, parsing, parameters, roles, tags, source partitions

- **Depends on:** 1a-03, 1a-04
- **Estimated Rust lines:** 2200
- **Scope:** Ports unique_types.py and uniques.py:28-213 (split_modifiers, placeholder, parameter parsing, UniqueMap construction), plus the unique parts of rules.py _umap/_prepare.
(1) Generation:
- unique_types.tsv: 637 rows, converted once from unique_types.py;
- unique_supported.toml: a role, fields and stages for each of the 402 used types, plus the 20 trigger kinds;
- cargo xtask gen-uniques writes src/unique/gen.rs, and xtask check keeps it fresh.
(2) Parsing: unique::text and unique::params, with about 45 ParamKind compilers, StatsId and FracId interning, and range checks.
(3) unique::compile:
- modifier folding, LOCAL, temp variants, the relevant-promotion fixup;
- the tag rule, including Aircraft;
- UniqueId order following civ_umaps;
- SourceUniques partitions and the hot/cold split: a 24-byte Unique with 12-byte, 4-aligned payloads and UFlags in CondDeps' top byte;
- UniqueMeta.key: FNV-1a of source kind, source name, occurrence and text.
(4) Refcheck: scripts/refcheck/uniques_dump.py writes refcheck/uniques.json.gz, and the refcheck uniques group is added.
Filters are carried as unresolved text handles here; 1a-06 resolves them.
- **Gates:** (1) All 1,615 shipped unique texts compile, and a golden debug dump matches as a snapshot.
(2) Error tests pass: an unknown text; an unknown tag nothing references; a bad stat; an out-of-range amount; two triggers; for every.
(3) size_of::<Unique>() is at most 32 (24 expected).
(4) Two identical texts on different sources get different keys, and adding an unrelated unique leaves every key unchanged.
(5) gen-uniques is idempotent, and CI fails on a stale gen.rs.
(6) The refcheck uniques group is enforced with 0 unexplained.

### 1a-05b (1a): Full unique support: every type the Python engine handled compiles, and the kitchen-sink ruleset

Added by the owner's decision of 2026-09-23.
- **Depends on:** 1a-05, 1a-06
- **Scope:**
(1) Every unique type and conditional Python handled compiles:
- `unique_supported.toml` gets a role, fields and stages for each, marked `(extra)`;
- `gen.rs` is regenerated, and the new `ParamKind`s get compilers;
- the triggers and one-time effects join the tables of §5.9, for 1a-07 to decode.
(2) The kitchen-sink test ruleset in `crates/citar-testkit/testdata/rulesets/kitchen_sink/`:
- it uses every extra type and conditional at least once, each in a plausible place;
- an overlay path in testkit loads it over the shipped ruleset.
(3) DESIGN.md and `docs/MODDING.md` say what is supported and who implements it.
The extra effects are implemented with their systems in 1b and 1c, and the conditionals in 1a-07. A type that needs more than a few minutes' work is deferred, with its reason, in `ops/unused-uniques.md`.
- **Gates:** (a) All shipped and kitchen-sink uniques compile.
(b) A test shows the shipped ruleset and the kitchen sink together cover every type in `unique_supported.toml`.
(c) gen-uniques is idempotent, and `cargo xtask check` passes.
(d) Clippy passes with `-D warnings`, and nextest is green.
(e) The refcheck uniques group is still enforced with 0 unexplained.

### 1a-06 (1a): Filters, and the tables for map generation, AI and milestones

- **Depends on:** 1a-05
- **Estimated Rust lines:** 2000
- **Scope:** Ports uniques.py:294-776 (the multi-filter grammar and every single-term predicate) and the mapgen-unique consumers in mapgen.py (_allowed, _never_generates, resource weighting).
(1) unique::filter:
- the parser and Expr;
- static-domain bitsets for base unit, building, terrain, improvement, resource, tech, era, policy and promotion;
- dynamic pruned trees with UnitLeaf, TileLeaf, CityLeaf and CivLeaf, with HumanPlayer and AiPlayer tagged SEAT;
- tile-terrain-only filters and combatant filters;
- the aliases, dedup by (domain, text), depth caps, and errors for terms that match nothing.
(2) The TileFacts trait.
(3) rules::gen_tables: TerrainGen, ResourceGen and NaturalWonderGen with GenCond; per-object AI weight lists; the victory Milestone enum; the inert list with its reasons.
A one-off script, scripts/refcheck/filters.py, records Python truth tables.
- **Gates:** (1) For every static filter string in the data, the Rust bitset equals the Python truth table over every object.
(2) Dynamic leaves pass unit tests against a mock world.
(3) A proptest shows constant folding preserves meaning on random expressions.
(4) A region conditional on an effect is rejected.
(5) Every map-generation unique lands in a table (snapshot test).

### 1a-07 (1a): Evaluation: conditionals, CondDeps, countables, Ctx, EvalWorld, query API, CSR build, triggers

- **Depends on:** 1a-06
- **Estimated Rust lines:** 2600
- **Scope:** Ports:
- uniques.py:215-293 (Ctx) and 778-1083 (_COND, countables, chance);
- the composition of the civ index in economy.py:64-147;
- the matching part of triggers.py:13-74;
- the requirement texts of cities.py:1169-1193.
(1) Conditionals, in unique::cond:
- every supported conditional (94: the 49 used and the 45 package 1a-05b added), with CondDeps tagging including SEAT;
- applies and applies_scoped;
- describe, with the cities._not_met texts;
- Chance via Rng::keyed with meta.key and KeyPart-encoded civ, tile and unit.
(2) unique::countable.
(3) unique::world: Ctx, CombatCtx, and the EvalWorld and TileFacts traits. EvalWorld exposes index reads (IndexRef) that a production world validates lazily.
(4) unique::query: uq::civ, civ_no_resources, city, unit, unit_and_civ, terrains, object, raw, any, sum_i32, requirement_problems.
(5) unique::index: Csr, CivSources, CivIndex::build, placeholder_counts.
(6) unique::trigger: TriggerKind, TriggerCond, TriggerEvent matching, fire, and OneTimeEffect decoding for every one-time type (39) and the gain effects. Applying them is 1b-08.
(7) A one-off script, scripts/refcheck/not_met_dump.py, records Python _not_met strings for sample uniques and contexts on the fixtures.
- **Gates:** (1) There is a mock-world unit test for every Cond variant, plus a table test that CondDeps are assigned right.
(2) Python edge cases pass: civ None, ignore_conditionals.
(3) The chance key uses meta.key, and a None part differs from 0.
(4) A proptest shows CivIndex::build gives an identical Csr for any permutation of source order.
(5) Every OneTimeEffect kind decodes.
(6) describe() texts match the recorded not_met_dump strings.

### 1a-08 (1a): State model: stores, map, units, cities, typed players, diplomacy, world, chronicle heads, Change

- **Depends on:** 1a-03
- **Estimated Rust lines:** 4300
- **Scope:** Ports state.py:13-405 (Tile, Unit, City, Player, GameState and the Phase 0 seat defaults), the config shape of game.py:32-60, the relation and deal shapes of diplomacy.py:60-96 and 530-900, and the flags mapping.
(1) Entities:
- state::store: EntityStore;
- state::map: MapInfo, the 16-byte Tile with canon_bytes, FeatureSet top(), RouteBits, Tiles setters returning Change, BuildQueue;
- state::memory;
- state::units: occupancy and by_owner; relocate cascading to carried units; despawn clearing carried_by;
- state::cities.
(2) Players and world:
- state::players: every typed flag field, including last_gold_rate, happiness_seen and citizens_settled; Seat with Controller, Handicap, AutoDecisions, SeatOverrides and driver: Option<DriverMemory>; MajorData; CityStateData; QuestTarget; Spy;
- state::diplo: PairMatrix, war_mask, met_mask, OpinionBook, DealItem, Terms, Deal, Negotiation, NegEntry;
- state::world.
(3) History and config:
- state::chronicle: Event; EventType with the 97 engine kinds and the is_private table; EventData; NameRef; Message; Thought; StatsRow with the baseline STAT_KEYS; ActionRecord; Chronicle; ChronicleHeads (engine); HostHeads;
- state::config: GameConfig with a required seed and a MapSource; HostOnly.
(4) Changes and access:
- state::change: the Change enum, including UnitOwner, UnitRemoved, CityOwner, Met, Seat and PlayerAlive;
- the pub(crate) mutable accessors, with the xtask check rule restricting their callers;
- State::{transfer_city, kill_player, revive_player, rebuild_indexes}.
- **Gates:** (1) Proptests pass:
- EntityStore against a BTreeMap model, over random insert, remove and get with compaction;
- random unit spawn, move, board, despawn and owner change, with occupancy and carried-unit checks after every op.
(2) PairMatrix index tests pass for n = 1..64, and war_mask and met_mask agree after random edits.
(3) Seat default derivation matches the Phase 0 Python tests.
(4) DealItem JSON equals the Python item dicts for all 13 kinds.
(5) Size asserts hold: Tile 16 bytes, TileMemory 8 bytes.
(6) A test lists every emit type in citar/engine and checks that EngineEvent covers it, and that is_private matches PRIVATE_EVENTS.
(7) xtask check fails when a file outside game/mutate.rs, save or compat calls a mutable accessor.

### 1a-09 (1a): Save format v1, canonical digest and chain, load and validate, summary, journal chunks

- **Depends on:** 1a-08, 1a-05
- **Estimated Rust lines:** 2700
- **Scope:** Replaces the save path of GameState.to_dict and from_dict, save_rng (game.py:995-1002), and the frame format of victory.record_frame (victory.py:458-485).
(1) Save format:
- save::ctx, the scoped thread-local ruleset context;
- save::json, the human-readable codec: rule ids as names; custom encodings for PairMatrix, OpinionBook, EntityStore and DriverMemory (base64); HostOnly heads saved;
- save::columns, base64 column layers with palettes;
- save::migrate (a scaffold) and save::summary, replacing engine_api.state_summary;
- the test that bans skip_serializing_if, flatten and untagged.
(2) Digest:
- save::canon: the CANON_V1 forms of State, with HostOnly as nothing and floats as to_bits;
- save::chain: the Digest formula and DigestChain.
(3) Loading:
- save::validate: referential integrity, ranges, finite floats, PlayerVec lengths, palettes, DriverMemory sizes;
- a RulesetId mismatch becomes a LoadReport warning;
- an unresolvable name is LoadError::UnknownName, and an unknown top-level key is LoadError::UnknownKey in every build.
(4) Snapshots and journal:
- Snapshot::to_json;
- save::journal: JournalChunk encode and decode, FrameWriter keyframe and delta encoding, and a chronicle rebuild with a chronicle_incomplete report.
Game::load itself is wired in 1b-01; here load returns a State plus a Chronicle.
- **Gates:** (1) Round trips on generated states: load(save(s)) == s bit for bit, including -0.0 and DriverMemory, and the resave is byte-identical.
(2) A missing ruleset context returns an error, not a panic.
(3) A save made under a ruleset copy with one changed number loads with rules_changed set; a save naming a removed building fails with UnknownName.
(4) The attribute-ban test passes.
(5) Known-answer digests for 3 checked-in states are identical on the 5 targets.
(6) The digest does not change with BTreeMap insertion order, and does not change when only HostHeads differ.
(7) NaN is refused.
(8) A property test round-trips random chunk and frame sequences.
(9) summary() matches a fixture.
(10) Criterion: the digest of a synthetic gargantuan state stays at or under 5 ms (report-only; hard above 15 ms).

### 1a-10 (1a): Strict Python-state converter (feature legacy, test-only), the refcheck state_echo group, convert goldens

- **Depends on:** 1a-09, 1a-04
- **Estimated Rust lines:** 1800
- **Scope:** Reads the format of state.py to_dict:
- tiles in Tile._ORDER; base64 explored;
- units, cities and camps keyed by string ids; str(pid)-keyed flags;
- relations as 'a,b'; open_borders and embassies as 'a>b';
- deals and negotiations; the religions dict; UN names; events with refs; config.
(1) compat::python, strict only, with mirror types and the full mapping of design section 4.12:
- flags become typed fields; free_buildings move to cities; quests become QuestTarget; religions become ReligionId;
- scenario opinions move to their holder; code-point refs become byte offsets; fallout is OR-ed into features;
- dead GameState fields are dropped (barbarian_state, capture_ids, first_discovered);
- ConvertReport lists every dropped item, and the fields Python lacks get defaults (citizens_settled false, last_gold_rate 0, combat_seq 0, driver None).
(2) Refcheck: the state_echo answer module, a projection of the fixture state compared against inspect-style reads of the converted State.
(3) Goldens: convert-* digests for the 12 committed fixtures right after conversion, before any settle.
Not enabled in any shipped crate.
- **Gates:** (1) All 262 fixture states (mini, late and corpus) convert with zero unknowns.
(2) Counts match Python: tiles, players, units, cities, events, and explored popcount per player.
(3) Ids are preserved.
(4) Converted -> save -> load gives an equal digest.
(5) The ConvertReport dropped list equals the expected set.
(6) The refcheck state_echo group is enforced with 0 unexplained.
(7) The convert-* goldens are identical on the 5 targets.
(8) cargo tree shows the legacy feature off for every non-test crate.

### 1b-01 (1b): Game core: self-validating memos, game::mutate (Change and Touch), effects, settle, events, invariants, act pipeline

- **Depends on:** 1a-07, 1a-10
- **Estimated Rust lines:** 3400
- **Scope:** Ports:
- game.py:100-145 (construction) and 320-540 (accessors);
- 565-609 (whose invalidate machinery is replaced);
- 656-812 (occupancy, at_war, has_met, meet, unit create, remove and place);
- 842-990 (emit, refs, mentions, events_for, scrubbing).
(1) The game and its caches:
- game::core: Game, with &'static Ruleset, State, Derived, Chronicle, Pending, EffectQueue, FrameWriter, DebugOptions and the poison flag;
- game::derive::rev: Rev, Revs, CivRevs, Stamp, CopyMemo and Memo, validated lazily through &self, with early cutoff, and the CondDeps-to-rev mapping;
- the Derived skeleton and game::eval: EvalView implementing EvalWorld over lazily validated memos.
(2) Writes:
- game::mutate: Game setters wrapping every State setter into Game::changed; city_mut, player_mut and unit_mut with CityTouch, PlayerTouch and UnitTouch;
- Derived::on and the EffectQueue drain order.
(3) Settle and events:
- game::turn::settle: sync_sight and citizen hooks, the fixed-point loop with an 8-pass cap, SETTLE-1 and PEND-1;
- game::events: emit with private events, mentions, and audience widening only for non-public events; the aho-corasick name index with overlapping matches, leftmost-longest boundary-valid selection and a Unicode word boundary; NameRef byte offsets; EventBatch; the engine running hash, which excludes ids; host events and thoughts counted in HostHeads;
- scrubbing: anonymising, possessives, coordinates, the UN tally.
(4) Checks and actions:
- game::debug (DebugOptions) and game::invariants (every code of design section 9.4);
- a game::query skeleton;
- game::action: the Action enum; act() with guard, check (&self), apply (infallible, returning an OutcomeSpec), settle, and render.
(5) Loading: Game::load and Game::from_python (convert, build Derived, commit happiness once from 0 once Happiness exists, then settle).
- **Gates:** (1) Memo unit tests:
- revalidation happens only when inputs change;
- early cutoff stops the cascade;
- a read through &Game after a change sees the new value in release builds;
- a deliberate cycle panics on the RefCell.
(2) A compile_fail doctest proves that a dropped Change fails to compile, and xtask check rejects a direct mutable-accessor call from game/cities.
(3) Invariant corruption tests fire exactly one expected code each.
(4) Emit and scrub golden tests pass:
- possessives and non-ASCII names;
- a razed city via mentions, and a renamed civilization via mentions;
- a private event on a tile others can see, which does not widen;
- coordinates scrubbed;
- a shorter name matched when the longest candidate fails the boundary.
(5) A refused Action leaves the digest unchanged and does not settle.
(6) Game::from_python on all mini and late fixtures passes check_invariants.

### 1b-02 (1b): Scenario ops, inspect, test ops, tool argument specs and normalize, and the rule-script runners (Rust and Python)

- **Depends on:** 1b-01
- **Estimated Rust lines:** 2700
- **Scope:** Ports scenario.py:36-536: the op registry and the ops that need no later systems (grant_era, grant_tech, remove_tech, set_player, set_tile, meet, set_relation, set_influence, reveal, set_research). The ops found_city, set_city, remove_city, add_unit, remove_units and adopt_policy land with their systems.
(1) Rust:
- api::scenario: the framework, with atomic apply_ops (clone, apply, settle; restore on the first failure and name the op);
- api::inspect, including the pending-stage list, and api::testops (feature test-ops).
(2) Tool arguments:
- api::tools::args, the argument specs; each system package adds its own;
- api::tools::normalize, porting tools.py:113-127 exactly: required keys first; unknown keys dropped; Python int() semantics, with floats truncating and strings needing an integer literal; bool strings; comma lists; no at coercion.
(3) Script format:
- tests/rules/README.md, the script language, including start = 'bare';
- tests/rules/maps/arena.json with its anchors and explicit starts;
- tests/rules/_selftest.toml, including must-fail checks and a check that scripts never type numbers as strings.
(4) Rust runner: citar_testkit::script (parser, interpreter, matchers, expressions, tile references and selectors, the bare prelude) and citar-testkit/tests/rules.rs (libtest-mimic).
(5) Python side, about 900 lines of Python: citar/engine/testops.py, citar/engine/inspect.py, the EngineGame.inspect and EngineGame.test_ops methods (not in __all__), tests/rulescript.py, tests/test_rule_scripts.py.
Port the first scripts: config refusals and seat defaults from test_controllers.
- **Gates:** (1) _selftest.toml passes on both runners, including its must-fail checks.
(2) cargo nextest lists each script test separately.
(3) python -m unittest tests.test_rule_scripts is green, and tests/test_engine_boundary.py is still green.
(4) normalize unit tests match a table of Python tools.execute coercions: '3', 3.7, ' 4 ', '3.0' refused, 'Yes', 'a,b', unknown keys, a missing key reported before coercion.
(5) A failing third op leaves the game digest identical to before apply_ops.
(6) The first 4-6 ported scripts pass on Python and on Rust.

### 1b-03 (1b): Turn flow and new-game skeletons: stage tables, end_turn, end_round, Game::new on editor maps, minimal RandomAgent and drive

- **Depends on:** 1b-02
- **Estimated Rust lines:** 2000
- **Scope:** Ports:
- the control flow of turns.py:20-118 and 190-202, and game.py:1004-1037 (begin_turn, end_turn);
- the skeleton of game.py:145-318;
- the map-document half of maps.py (validate, tiles_from_rows, continents).
(1) game::turn::stages: the PLAYER_START (S0-S9), PLAYER_END (E0-E6) and ROUND_END tables with every settle point. System stages are Pending('<package>') no-ops.
(2) game::turn::driver: Game::end_turn (auto-playing city-states and barbarians, stopping at the next major), begin_turn and end_round (turn += 1 and the optional digest).
(3) game::setup: the stage table of design section 6.14, with these stages ported here: config, nations (NationShuffle), map from an inline editor document with explicit starts, players, relations, begin. config_from_json requires a seed and takes a custom map only inline.
(4) Drivers:
- game::turn::drive: the SeatDriver: Send trait, DriverMemory hand-off, and a drive loop with Stop::External and Stop::GameOver (the other Stop variants in 1c-09);
- citar_testkit::agents::RandomAgent, which can only end its turn; later packages add its actions.
(5) Test support: the turn test ops end_turn, end_round and force_turn, and inspect of pending stages.
Port about 4 scripts: turn order, city-states auto-played, not your turn, turn counter.
- **Gates:** (1) A bare arena game with 3 majors plays 50 end_turn calls with invariants clean, and inspect lists the pending stages.
(2) config_from_json refuses a missing seed and a map id, with a model-readable message.
(3) drive with RandomAgent in every major seat reaches the turn limit on a tiny arena map.
(4) golden bless refuses while stages are pending.
(5) About 4 turn-order scripts pass on Python and Rust.

### 1b-04 (1b): Map generation

- **Depends on:** 1b-03, 1a-06
- **Estimated Rust lines:** 3700
- **Scope:** Ports mapgen.py:1-1721:
- MapOptions, ValueNoise, fractal fields, land scores;
- ice band, humidity and temperature, mountains and hills, lakes and coasts, vegetation, rare features, continents;
- rivers, terrain conversion, fertility, start and city-state sites;
- natural wonders, resources and luxury variety, rebalancing, start normalisation, ruins;
- generate_map.
Also the start-filling and ruin helpers that maps.prepare reuses (_fill_starts, _start_candidates, _start_score), for 1c-09.
Each generation phase gets its own Purpose stream, and mapgen reads the gen tables through TileFacts.
Also:
- mapgen::metrics (start quality, resource counts, river mouths, ice band) for the properties;
- api::maps::generate_map taking a seed;
- the generated-map path of the setup map stage.
- **Gates:** (1) Mapgen properties hold over 200 seeds x 5 map types x duel and small:
- every river reaches water, and none cross;
- ice stays in the polar band;
- every strategic resource is present;
- luxury variety holds;
- start-quality spread is within a threshold.
(2) Generation time: small at or under 50 ms, gargantuan at or under 1.5 s in release (report-only; hard above 3x).
(3) The map-* goldens (10 maps, duel to huge, every map type, hashed tile arrays) are identical on the 5 targets.
(4) Tuning one phase's parameters leaves the other phases' draws unchanged (a test that swaps the vegetation stream).

### 1b-05 (1b): Civ-level derived data: unique index memos, unit profiles, resource supply, upkeep, supply, gold

- **Depends on:** 1b-03
- **Estimated Rust lines:** 2600
- **Scope:** Ports:
- economy.py:64-353: civ unique maps, temp uniques, and resource supply, including the staged exclusion of resource uniques and detailed resources;
- economy.py:500-746: unit upkeep, supply, gold processing and bankruptcy;
- cities.py:34-104: city unique composition;
- units.py:21-53: unit unique maps and profiles;
- turns.py:120-131: temp uniques expiring.
(1) Index memos: game::derive::civ (CivSources gathering, and the CivIndex, CityLocal and FollowerIndex memos) and the unit ProfileTable.
(2) Resources: the ResourceSupply and CivIndexFull stages.
(3) game::economy: upkeep, supply, process_gold. Wire stage E3 (gold) and E5 (temp uniques).
(4) The refcheck civs answer module for these paths, and the scenario ops add_unit and remove_units.
- **Gates:** (1) Refcheck civs paths are enforced with 0 unexplained: resource_supply, detailed_resources, unique_index, unit_upkeep, unit_supply.
(2) verify_caches is clean on every fixture after load.
(3) A proptest shows the index memos equal a cold rebuild after random touches and source edits.
(4) Criterion (report-only; hard above 3x): civ index lookup at or under 20 ns; index rebuild at or under 10 us.

### 1b-06 (1b): Tile yields, city stats, happiness, civ stats, connectivity and citizens

- **Depends on:** 1b-05
- **Estimated Rust lines:** 3900
- **Scope:** Ports:
- tiles.py:1-391: tile predicates, tile_stats, improvement yields;
- cities.py:246-926: city stats with breakdowns, city happiness, food to the next pop, specialist stats, rank_stats_for_work, assign_citizens, work_tile and focus;
- cities.py:1967-2072: connected to capital;
- economy.py:404-708: happiness, civ stats, stat maps, gold per turn.
(1) Yields: game::tiles; game::derive::tile (CityMods, TileTags, the TileYield memo, unowned yields).
(2) Stats:
- game::cities::stats: the CityHappiness and CityStats memos;
- the Happiness and CivStats memos;
- the happiness_seen commit at stages S1 and E1 and the setup happiness stage, with threshold flags at 0 and -8;
- the E2 stage: last_gold_rate and totals, with a sign-change flag.
(3) Connectivity: game::cities::connections, a flood fill per medium.
(4) Citizens: game::cities::citizens, with:
- RankCtx reading happiness_seen and last_gold_rate;
- greedy assignment and recheck flags in Pending;
- the fixed-point settle passes, citizens_settled and the citizen oracle;
- the actions work_tile, set_city_focus and set_specialists, whose results render after the settle.
(5) Refcheck answer modules for tile_yields and city_stats.
Port about 6 scripts: citizen results equal inspect after the call; focus; specialists; the blockade; growth under unhappiness; a happiness commit only at S1 and E1.
- **Gates:** (1) Refcheck tile_yields is enforced, and city_stats is enforced except its religion paths.
(2) Refcheck civs paths for happiness, civ_stats, stat_map and gold_per_turn are enforced.
(3) A connectivity proptest agrees with a naive BFS.
(4) The citizen oracle holds in scripts.
(5) P8 in miniature: 50 queries and refusals between two actions leave the digest identical.
(6) About 6 scripts pass on Python and Rust (intended checks skipped on Python).
(7) Criterion (report-only; hard above 3x):
- tile_yield hit at or under 5 ns, first read after an unrelated change at or under 50 ns, recompute at or under 1 us;
- city_stats at or under 10 us;
- assign_pop20 at or under 10 us;
- connectivity on small at or under 30 us;
- settle with nothing pending at or under 0.5 us.

### 1b-07 (1b): Cities (borders, construction, purchase, queue, buildable, free buildings, founding, lifecycle), research and policies

- **Depends on:** 1b-06
- **Estimated Rust lines:** 3900
- **Scope:** Ports:
- cities.py:927-1965: borders and tile purchase; construction and production costs; purchase checks and costs; the queue; the Buildable list; free buildings; founding and city naming (the engine function; the unit action is 1c-04);
- cities.py:2073-2391: growth, starvation, razing, health, resistance, WLTKD demands;
- research.py: costs, progress, eras, free techs, research agreements;
- policies.py: culture costs, adoption, branches.
auto_pick_production is stubbed until 1c-07; it picks nothing for now.
Wiring:
- stages S2 (research), S5 (cities::start_turn), E3 (policies, research) and E4 (cities::end_turn, razing first);
- the setup stage for starting techs and era stocks.
Additions:
- the actions set_production, change_queue, set_auto_production, buy, buy_tile, rename_city, set_research, dequeue_research, choose_free_tech and adopt_policy, with their argument specs and RandomAgent moves;
- the scenario ops found_city, set_city (plus health, attacked, food), remove_city and adopt_policy;
- the Buildable memo;
- the refcheck buildable answer module and the civs paths for tech costs, policy cost, adoptable policies and era.
Port about 12 scripts from test_mechanics.
- **Gates:** (1) Refcheck buildable is enforced.
(2) Refcheck civs paths for tech_cost, policy_cost, adoptable_policies and era are enforced.
(3) About 12 scripts pass on Rust and Python: production, purchase, growth, borders, research, policies.
(4) An arena game with RandomAgent researching and building plays 100 turns with verify_caches clean.
(5) Criterion (report-only; hard above 3x): buildable recompute at or under 20 us, hit at or under 50 ns.

### 1b-08 (1b): Religion (civ and city side), great people, golden ages, one-time effects, ruins

- **Depends on:** 1b-07
- **Estimated Rust lines:** 2900
- **Scope:** Ports:
- religion.py, except the unit spread actions (1c-04): pantheons, founding, enhancing, beliefs, pressure with the CityNeighbours spatial grid, followers, prophets with Purpose::Prophet;
- great_people.py: points, thresholds, births, the Maya long count, golden ages;
- triggers.py:75-374: apply_one_time for all 31 OneTimeEffect kinds, including the timed path through temp variants;
- ruins.py: rewards with Purpose::Ruins.
Wiring: stages S2 (great people, religion, Maya), E3 (religion, great people) and E6 (golden-age progress), and the setup stage for starting triggers.
Additions: the actions found_pantheon and choose_great_person, and the religion and great-person paths of the refcheck city_stats and civs answer modules.
Port about 6 scripts.
- **Gates:** (1) Refcheck city_stats religion paths are enforced: followers, majority religion, pressures.
(2) Refcheck civs religion and great-person paths are enforced.
(3) Scripts pass:
- Babylon gets a free Great Scientist on Writing;
- a timed Strength bonus expires after 50 turns;
- ruins rewards apply;
- a pantheon, then a religion, is founded;
- a golden age starts and ends.
(4) Unit tests cover apply for every OneTimeEffect kind.
(5) Criterion (report-only): religious pressure for one gargantuan round at or under 1 ms.

### 1c-01 (1c): Incremental visibility: sight, spies, line of sight, explored tiles, memory, first contact, revival

- **Depends on:** 1b-05
- **Estimated Rust lines:** 1700
- **Scope:** Ports:
- visibility.py:1-243: elevation walk, visible tiles, explored tiles, memory snapshots, refresh, reveal_tiles;
- units.py sight;
- espionage.py:444-451, spy vision;
- the first-contact part of game.py:694-712;
- the natural-wonder discovery bonuses.
(1) Line of sight: game::vis::los, a LosCache evicted eagerly around height changes.
(2) Visibility: game::vis::visibility, with:
- per-civ u16 counts and bitsets;
- sources for units, cities (owned tiles plus a ring, following CityTiles), allied city-states and set-up spies;
- owned footprints, and set_footprint with transitions that cancel out.
(3) Effects: game::vis::effects:
- explored tiles and memory for majors only;
- first contact on 0-1 transitions, on unit arrival or owner change, on tile, city and owner changes of visible tiles, and on PlayerAlive;
- natural wonders.
(4) Settle and turn wiring: sync_sight in settle, event audiences from bitsets, the enemy-spotted delta for move_toward, and the setup visibility stage.
(5) Refcheck: the visible and fixed_point answer modules.
- **Gates:** (1) Refcheck visible and fixed_point are enforced.
(2) A proptest shows incremental visibility and met sets equal a full rebuild with Python's rule, after random moves, border growth, tile purchase, city founding in sight, captures and a revival.
(3) Tests pass for first contact in both directions, city-states not meeting city-states, natural-wonder discovery and enemy spotted.
(4) About 4 scripts pass, including first contact through border growth.
(5) Criterion (report-only; hard above 3x): vis_step at sight 2 at or under 1.5 us; LOS at sight 3 uncached at or under 1 us.

### 1c-02 (1c): Units and movement: promotions, XP, healing, upgrades, move classes, A*, PathTree, orders

- **Depends on:** 1c-01
- **Estimated Rust lines:** 3700
- **Scope:** Ports:
- units.py: promotions, XP, healing, upgrades and their costs, start_turn and end_turn, place_unit_near, limited-use abilities;
- movement.py:22-702: passability, ZOC, embarkation, costs, find_path, path_turns, reachable_this_turn, move steps, goto, move_toward.
(1) game::path:
- class: MoveClass interning;
- cost: static MoveCosts (terrain entry costs, the static terrain_reason pass bitset, embark transitions), rebuilt incrementally from tile_log with an LRU cap; the per-civ RouteLayer;
- node: the dynamic per-node checks (explored, territory entry, foreign cities, visible foreign units with the capture exception, own stacking at 0 moves, Zoc, war extra cost);
- astar: a u64 key (turns << 32 | u32::MAX - left), the turn-aware heuristic and pruning;
- tree: PathTree; plus PathScratch and PathCache.
(2) game::units and game::movement, with carried units following their carrier.
(3) Wiring: stages S6 and E6 (units), and the setup stage for starting units (units::starting_units, find_spawn_tile in ring order).
(4) The actions move_unit, unit_order, upgrade_unit and promote_unit, with argument specs and RandomAgent moves.
(5) Refcheck: the movement answer module, with PathEquivalent and reachable compared exactly.
Port about 8 scripts.
- **Gates:** (1) Refcheck movement is enforced, with any Better difference listed.
(2) A proptest shows the A* cost equals Dijkstra's on random cost grids, with random fog, units and territory.
(3) edge_cost equals a direct port of enter_cost on random states.
(4) A unit whose moves exceed its full moves gets a correct path (a key-overflow regression test).
(5) About 8 scripts pass: embarkation, ZOC, roads, promotions, healing, upgrades, a carrier moving its aircraft.
(6) Criterion (report-only; hard above 3x): astar_small_30 at or under 20 us; astar_garg_fog at or under 150 us; reachable at or under 5 us.

### 1c-03 (1c): Combat and conquest

- **Depends on:** 1c-02, 1b-07
- **Estimated Rust lines:** 3600
- **Scope:** Ports:
- combat.py:29-1215: combatants, strengths and modifiers, damage for a given roll, preview, resolve with Purpose::Combat keyed by combat_seq, withdraw, city combat and bombard, air sweep and interception (InterceptOrder instead of g.rng.shuffle), nukes with fallout as a feature;
- conquest.py: capture; gold and buildings on capture with Purpose::CaptureGold and CaptureBuildings; annex, puppet, raze; liberate with State::revive_player; auto-conquer following Seat.auto.
(1) game::combat::{combatant, strength, resolve, city, air, nuke}, with combat::setup built once per preview.
(2) game::conquest.
(3) The actions attack, air_sweep, city_attack, city_status and return_civilian, with argument specs and RandomAgent moves.
(4) The refcheck combat_previews answer module.
Port about 14 scripts.
- **Gates:** (1) Refcheck combat_previews is enforced: damage exact at rolls 0, 0.5 and 1; strengths within tolerance; interception candidates.
(2) A property shows damage is monotonic in attacker strength.
(3) combat_seq increases by exactly one per combat event, and save/load between two attacks reproduces the second.
(4) About 14 scripts pass: melee, ranged, city capture, puppet and raze, liberation reviving a civilization, nukes, interception, air sweep, civilian capture.
(5) Criterion (report-only): preview at or under 1 us.

### 1c-04 (1c): Workers, unit actions, found_city and automation

- **Depends on:** 1c-02, 1b-08
- **Estimated Rust lines:** 3300
- **Scope:** Ports:
- workers.py: improvements, build progress, repair, chop, route building, pillage with Purpose::Pillage;
- actions.py: special unit actions following ActionMods, including great person abilities and paradrop;
- the religious spread actions of religion.py;
- tools.py found_city: the settler action, on top of cities::founding from 1b-07;
- automation.py: goto execution, explore, automated workers, city-site scoring, run_unit_orders.
(1) game::workers, game::actions and game::automation. Wire stage S8 (run_unit_orders with a settle after each move order) and E6 (worker builds).
(2) Derived maps: game::derive::jobs (JobMap per civ and builder class) and game::derive::danger (DangerMap).
(3) The actions build_improvement, unit_action and found_city, with argument specs and RandomAgent moves.
Port about 10 scripts.
- **Gates:** (1) About 10 scripts pass:
- worker builds, chop, pillage, automated worker, explore, great improvements, religious spread;
- found_city refused too close to a city, on water, for a unit without FoundCity, and under the city-state settler rule.
(2) Criterion (report-only): jobmap_small full rebuild at or under 200 us.
(3) The job redundancy counter (stats feature) stays under 5% in a soak slice.
(4) Worker job choices on fixture states are logged against a direct port, with intended differences listed.

### 1c-05 (1c): Diplomacy and espionage (spies, theft)

- **Depends on:** 1c-03
- **Estimated Rust lines:** 3000
- **Scope:** Ports diplomacy.py:17-963:
- CATEGORIES, item_category and proposal_categories;
- relations: war, peace, treaties, friendship, pacts, embassies, open borders, denouncements, opinions;
- deals: validate_items, describe_items, research-agreement cost, ongoing items;
- negotiations, in the Phase 0 chat model: seq, a required message, aliases, rejecting empty counters, the message cap, close_negotiation, end_turn_refusal, expiry at E0;
- process_round in ROUND_END.
Also ports the spy core of espionage.py: recruitment, placement, tech theft with Purpose::Spy, and Change::Spy for vision. Elections and coups go to 1c-06.
Additions:
- the actions send_message, open_negotiation, respond_negotiation, declare_war, denounce, move_spy and end_turn (with the chat refusal rule), with argument specs and RandomAgent moves;
- the host ops open_negotiation_as and close_negotiation;
- met lists in player-id order (an intended entry);
- the refcheck deal_checks answer module, without bot_value.
Port about 22 scripts.
- **Gates:** (1) Refcheck deal_checks is enforced, without bot_value.
(2) 18 negotiation-chat scripts and 4 diplomacy scripts pass on Rust and Python.
(3) The end_turn refusal works in both directions while Game::end_turn still works.
(4) The invariants DIPLO-1 and NEG-1 hold under random negotiation sequences.

### 1c-06 (1c): Barbarians, city-states and city-state espionage

- **Depends on:** 1c-03, 1c-04, 1c-05
- **Estimated Rust lines:** 3700
- **Scope:** Ports:
- barbarians.py: camps, spawning, sacking, wandering, and the barbarian AI, whose attack-target search takes an explicit from_tile and whose _seek runs on one PathTree. RNG keys use camp and city ids instead of len(units), len(camps) and len(events).
- city_states.py:15-1320:
  - influence, resting points, allies and protectors;
  - the actions gift, pledge, tribute, buyout, marriage and peace;
  - quests with QuestTarget, and war quests;
  - the city-state AI turn, using the cached Buildable.
- The city-state parts of espionage.py: elections with Election and ElectionDelay, rigging, coups.
Wiring: stages S0 (barbarians), S3 (the great-person gift tick) and S8 (city-state AI), plus the setup stages for city-state init and initial camps.
Additions: the actions city_state_action and stage_coup, with argument specs and RandomAgent moves.
Port about 12 scripts.
- **Gates:** (1) 8 barbarian scripts pass: sack instead of capture, camps, spawn rate, raging.
(2) About 4 city-state scripts pass: influence decay, quests, gifts, bullying, alliance, a coup.
(3) Refcheck civs city-state paths are enforced.
(4) Criterion (report-only): a barbarian round on small T100 at or under 2 ms.
As built: ten barbarian and five city-state scripts pass on both engines; quests, elections and successful coups are unit tests, their draws differing between the engines. See "As built in 1c-06" after §6.11.

### 1c-07 (1c): Production advisor in the engine (game::advisor)

- **Depends on:** 1b-07, 1c-03
- **Estimated Rust lines:** 2000
- **Scope:** Ports:
- cities.py:1696-1717: auto_pick_production, including the puppet rule of buildings or Gold only;
- the production half of the bot:
  - bots/basic.py:777-875, the parts of context production needs;
  - 1146-1590: advise_production, _choose_production_unciv and _choose_production_classic, _building_value_unciv and its raw form, unit values.
AdvisorParams takes the live bot defaults. The ranged-or-melee draw (basic.py:1336-1337) uses Purpose::Advisor keyed by city and turn. Ties break by BuildingId descending (an intended entry).
what_if_building over a CityMods overlay replaces _simulate (basic.py:1491-1505).
This replaces the 1b-07 stub, so city-states, puppets and auto_production pick through the advisor.
As built: majors' puppets and auto_production cities pick through it; city-states never asked Python's advisor and choose through their own AI (1c-06). See "As built in 1c-07" after §6.12.
A one-off script, scripts/refcheck/advisor_dump.py, records Python advise_production on fixture cities (informational).
- **Gates:** (1) what_if_building(c, b) gives the same deltas as applying and undoing the build, and the digest is unchanged afterwards.
(2) A property over the fixtures: the advisor never returns an unbuildable item, and returns the same answer twice.
(3) The puppet rule holds.
(4) The agreement rate with Python's advise_production is reported (informational, not a gate).
(5) Criterion (report-only): an advisor call per city on small T200 at or under 50 us.

### 1c-08 (1c): Victory, score, UN, eliminations, stats rows and frames

- **Depends on:** 1c-05, 1c-06
- **Estimated Rust lines:** 1800
- **Scope:** Ports victory.py:
- score and military strength;
- UN votes with Purpose::UnVote, a tally keyed by player id, and auto-votes following Seat.auto;
- milestones and victory checks;
- eliminations through State::kill_player, closing negotiations and deals;
- record_stats with the baseline STAT_KEYS;
- record_frame into FrameWriter.
Also ports turns.py:135-188 (revolts with Revolt and RevoltDelay) and the victory parts of turns.py:190-202.
Wiring: stages S3 (revolts), S9 and E6 (victory checks), and ROUND_END (eliminations, stats, frame, UN, turn limit).
Additions: the action un_vote with argument spec and RandomAgent move; the refcheck civs paths for score, military strength, victory progress, world era and UN numbers.
Port about 6 scripts: victory types, UN, elimination, the turn limit.
- **Gates:** (1) The refcheck civs group is fully enforced.
(2) About 6 scripts pass.
(3) Stats rows carry all 33 baseline STAT_KEYS, and war and capture events carry attacker/defender and old/new owner (unit tests).
(4) The FrameWriter keyframe-plus-delta sequence reconstructs full frames equal to direct snapshots over a 100-turn arena game.

### 1c-09 (1c): New-game setup completion and the AI driver

- **Depends on:** 1c-06, 1c-07, 1c-08, 1b-04, 1b-08
- **Estimated Rust lines:** 1900
- **Scope:** Completes game::setup:
- every stage of design section 6.14 is Ported;
- maps.prepare (maps.py:323-365) for editor maps lacking starts or ruins, with Purpose::MapPrepare, reusing the 1b-04 start-filling helpers.
Completes game::turn::drive:
- every Stop variant: External, HybridDiplomat, AwaitingReply (rule T3), SeatLimit, GameOver;
- respond() dispatch for negotiations awaiting a driven seat;
- DriverMemory taken from and returned to Seat.driver.
Additions:
- the host ops force_turn, meet and debug (MeetAll, Reveal, Gold), and set_controller and set_difficulty with Change::Seat;
- the actions set_civ_name, write_notes and log_thought.
Bless the newgame-* goldens (10 games, duel to huge, every map type).
Port about 6 scripts: controllers and seat changes, drive stops, setup on an editor map without starts.
- **Gates:** (1) inspect reports no pending setup stage, and xtask check fails on one.
(2) The newgame-* goldens are identical on the 5 targets.
(3) drive stops right for every seat mix, including AwaitingReply, and DriverMemory written by a test driver survives save and load and is digested.
(4) Game::new refuses more than 64 players, and invariants are clean after setup for every map size and type.
(5) About 6 scripts pass on Python and Rust.

### 1c-10 (1c): Whole-game validation: complete RandomAgent, pass, random and load goldens, save and load every round

- **Depends on:** 1c-09
- **Estimated Rust lines:** 1200
- **Scope:** Testkit and integration work:
(1) citar_testkit::agents::RandomAgent, complete across every action.
(2) The golden sets:
- load-*: the 12 fixtures after from_python's settle;
- pass-*: 20 rounds of passing from 3 fixtures;
- random-*: duel 200 x2, small 120 x2, standard 60, large 30.
(3) The save-and-load-every-round digest test.
(4) xtask check fails on any Pending stage from here on.
(5) The early P8 property: queries, views, briefing, snapshots and refusals interleaved do not change digests.
Fix what these find.
- **Gates:** (1) Every fixture plays 5 pass rounds with no panic and no invariant violation.
(2) A RandomAgent small game reaches its turn limit, and its 330 turns take at or under 10 s (report-only; hard above 30 s).
(3) The digest chain with save and load at every round equals the uninterrupted run.
(4) The load-*, pass-* and random-* goldens are identical on the 5 targets.
(5) Every refcheck group except tool_errors, views and briefing is enforced on mini and late.
(6) No Pending stage remains.

### 1d-01 (1d): Tool registry: ToolSpec table, schemas, execute, query dispatch, model-facing errors

- **Depends on:** 1c-10
- **Estimated Rust lines:** 2400
- **Scope:** Ports tools.py:16-201 (registry, execute, tool_list, helpers) and the action-tool wiring of tools.py:416-1109 onto the typed Action variants, argument specs and normalize already in place.
(1) api::tools::registry:
- the static ToolSpec table of 61 tools (21 queries, 40 actions), each with its kind, any_time flag and category;
- descriptions ported as they are where they are fine, and fixed where wrong;
- schemas_json, cached; kind().
(2) Game::execute: guard, then normalize, then parse into Action and act; or dispatch a query.
(3) Errors: an ErrCode on every refusal, the P5 text rules, and numbers through PyFloat and round_ndigits.
(4) Refcheck: the tool_errors answer module.
- **Gates:** (1) Refcheck tool_errors is enforced, and every text fix is listed in intended.toml.
(2) The schemas JSON equals Python tool_list() apart from listed fixes.
(3) Every refusal text satisfies P5.
(4) Execute-level tests pass: missing parameters are reported before coercion; unknown keys are dropped; an action while not your turn is refused with Python's message.

### 1d-02 (1d): Views, info builders, query tools, event scrubbing in views, replay data

- **Depends on:** 1d-01
- **Estimated Rust lines:** 3200
- **Scope:** Ports views.py:
- client_view, for a player and for spectators;
- the *_info builders: empire, city, unit, tile, players, diplomacy, city-states, tech tree, policies, religion, great people, espionage, victory status.
Also ports:
- the query tools of tools.py:202-415 (get_* data, preview_attack, read_notes), which reuse the builders;
- the view-side scrubbing of game.py:891-990 (events_for, event_view);
- replay_data in the Full and Delta formats;
- empire_summary (at_war_with in player-id order), negotiation_view, standings and path_preview.
All are &self. Views serialise typed DTOs straight to JSON bytes in the shapes the web client reads, with numbers through PyFloat.
Adds the refcheck views answer module.
- **Gates:** (1) Refcheck views is enforced.
(2) Golden tests for event scrubbing in views pass: possessives, the UN tally, unmet city-states, coordinates.
(3) replay_data(Full) equals frames rebuilt directly from state over a 100-turn game, and Delta round-trips.
(4) Criterion (report-only; hard above 3x): view(pid) on small T300 at or under 1.5 ms; god view on large t280 at or under 20 ms (plan), with a target of 12 ms or less on gargantuan.

### 1d-03 (1d): Briefing, turn progress, maps API, scenario summaries, text constants

- **Depends on:** 1d-02
- **Estimated Rust lines:** 2600
- **Scope:** Ports:
- briefing.py: briefing, turn_progress, the ASCII map, alerts, MAP_LEGEND;
- the RULES_OVERVIEW text from views.py;
- the get_briefing, get_map and get_rules tools;
- maps.py:30-365: validate, blank_map, generated_map with an explicit seed, map_from_game, map summary. The file I/O stays in Python;
- scenario.py:537-645: ops_help, overview, default_seats, normalize_seats, summary.
Adds the refcheck briefing answer module.
Port about 5 scripts: briefing alerts, turn progress, single-player events.
- **Gates:** (1) Refcheck briefing is enforced, so every group is now enforced on the 12 committed states.
(2) About 5 scripts pass.
(3) Map API tests pass:
- validate fixes and reports problems;
- blank and generated maps validate;
- export, then import, round-trips.
(4) Criterion (report-only): briefing on small T300 at or under 1 ms.

### 1e-01 (1e): Stability: full invariants and cache oracle, properties P1-P8, chaos driver, fuzz targets

- **Depends on:** 1d-01
- **Estimated Rust lines:** 2300
- **Scope:** Completes game::invariants and game::derive::oracle under cfg(any(debug_assertions, feature checks)). The oracle's verify_caches covers:
- every memo in design section 6.5;
- a visibility and met-set rebuild;
- the citizen oracle.
(1) Properties in citar-testkit/tests/props.rs:
- the ActionSpec strategy, with late binding and arguments generated from the specs (70/20/10);
- properties P1-P8 and the pure-function properties;
- regressions committed;
- 64 cases in CI and 10,000 nightly.
(2) The chaos binary: RandomAgent turns mixed with ActionSpec noise, catch_unwind, replay files, --replay, and --from-fixtures (which flags every city for recheck).
(3) crates/citar-engine/fuzz with the load_state and fuzz_one targets, for WSL nightly only.
- **Gates:** (1) Seeded bugs are found and shrunk within the CI budget:
- a tool that mutates before refusing;
- a stale visibility cache;
- a Touch with the wrong flag;
- a query that writes through interior mutability.
(2) chaos --seconds 600 reports zero failures on Windows, Linux and macOS.
(3) A replay file reproduces an injected panic deterministically.
(4) Props at 64 cases are green in rust.yml.

### 1e-02 (1e): Determinism matrix, complete golden sets, soak, nightly workflow

- **Depends on:** 1e-01, 1c-10
- **Estimated Rust lines:** 1300
- **Scope:** (1) Goldens: finish the golden binary (check, bless, diff) and every set: rng, libm, pyfmt, ruleset id, known-answer states, convert, map, newgame, load, pass, random.
(2) Extra determinism checks: the same_process_twice test, and the ci-versus-release run on linux-x64.
(3) The final determinism.yml: 5 targets, a compare job, divergence artifacts.
(4) The soak binary: every map size, invariants on, reporting panics, violations, turn-time outliers and peak memory.
(5) nightly.yml:
- full-corpus refcheck --strict, if the corpus asset is approved; otherwise a documented laptop run;
- props at 10,000 and chaos for 20 minutes per OS;
- a reduced soak;
- the long golden set;
- the criterion trend.
- **Gates:** (1) Every golden set is identical on the 5 targets, under both ci and release.
(2) same_process_twice passes.
(3) A test branch with a HashMap iteration, a platform powf or an f64::round breaks clippy or the matrix.
(4) A nightly run completes in under 2 hours with zero failures.
(5) A laptop soak of 200 games is clean.

### 1e-03 (1e): Benchmarks, hard performance gates and tuning

- **Depends on:** 1c-10, 1d-02
- **Estimated Rust lines:** 1800
- **Scope:** (1) Criterion: the citar-bench suites (the kernels, macro benchmarks and I/O benchmarks of design section 9.7), on corpus fixture states, pinned to one P-core, run after the Python baseline has finished.
(2) CI gate: gungraun suites and the per-PR +5% instruction gate in rust.yml; thresholds.toml; perfgate via cargo xtask perf. Thresholds switch from report-only to hard (1.5x budget).
(3) Python ratios: scripts/refcheck/turn_timing.py (about 100 lines of Python) writes refcheck/perf/python-turns.json.
(4) Measure the overflow-checks cost.
(5) Tune until every budget in design section 10 holds, and write the performance report.
- **Gates:** (1) Every criterion threshold is met with hard gating.
(2) pass_round is at least 20x Python on every corpus state, within the 150 ms (small t280) and 400 ms (large t280) backstops.
(3) Snapshot lock time on gargantuan is at or under 100 ms (target 10 ms).
(4) The god view is at or under 20 ms.
(5) The gungraun gate fails on a synthetic +10% regression.
(6) The redundancy counters stay under 5% per memo type in a soak.

### 1e-04 (1e): Phase 1 exit: full-corpus strict refcheck, intended list, changelog, docs

- **Depends on:** 1d-03, 1e-02, 1e-03
- **Estimated Rust lines:** 500
- **Scope:** (1) Refcheck:
- run cargo refcheck over mini, late and corpus with --strict, and explain or fix every remaining difference;
- cite every intended entry at its fix site (// refcheck: id);
- generate the changelog rule-fix list with citar-refcheck changelog.
(2) Cleanup: remove every NotPorted; xtask check fails if any NotPorted or Pending stage remains.
(3) Docs: update ARCHITECTURE and MODDING for the compiled ruleset, the unique support scope, memos and settle.
(4) Record the exit evidence in the Phase 1 pull request.
- **Gates:** (1) Full corpus, all 262 states, --strict: 0 unexplained, 0 stale, ratchet at 0.
(2) Every Phase 1 exit criterion in design section 1.3 holds: rule scripts, stability (P1-P8), determinism and performance.
(3) cargo xtask check reports zero NotPorted and zero Pending stages.

