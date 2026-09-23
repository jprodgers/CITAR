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
- **Enforcement.** `xtask check` scans `use crate::…` paths per directory. It also restricts calls to `State::{tiles_mut, units_mut, cities_mut, players_mut, diplo_mut, world_mut}` to `game/mutate.rs`, `save/` and `compat/`.
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
- `map: MapSource`, either `Generated { size, type }` or `Document(Box<MapDoc>)`, an inline editor map. The host resolves a map id to its document; Python's `maps.load_map` call inside `Game.new` (game.py:165-170) stays in Python.
- `host: HostOnly<BTreeMap<String, Value>>` keeps keys the engine does not know verbatim. It is saved but never digested.

Seats live only in `Player.seat`.

### 4.9 Save format v1 (JSON)

The top-level keys are written in this order:

```json
{"format":"citar-state","version":1,"engine":"0.1.6+<BUILD_ID>","rules":{"id":"<RulesetId hex>"},
 "config":{...},"map":{"width":...,"continents":"<b64 u16le>"},
 "tiles":{"palette":{"terrain":[...],"feature":[...],"resource":[...],"improvement":[...]},
          "terrain":"<b64>","wonder":"<b64>",...,"features":"<b64 u16le>","city":"<b64 u32le>","builds":[[idx,[["Farm",5]]]]},
 "clock":{...},"seed":123,"ids":{...},"chronicle":{...},"host":{...},"players":[...],"units":[...],"cities":[...],
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
- **Banned attributes.** `skip_serializing_if`, `flatten` and `untagged` are banned in state types, because they would make the canonical encoding ambiguous. A test scans `src/state/**` for them.
- **Versioning.** `save::migrate::upgrade(&mut Value, from)` applies migrations at the JSON level. An unknown top-level key is a `LoadError` in every build: behaviour never depends on the build profile.
- **`Snapshot`** is a deep clone of `State` plus `&'static Ruleset`. It costs about 1-3 ms at gargantuan and is taken under the session lock, which fixes the torn saves. `Snapshot::to_json()` runs off the lock.
- **Loading.**
  - `Game::load(rules, state_json, chunks)` deserialises, then calls `rebuild_indexes()` and `validate()`.
  - `validate()` checks referential integrity, ranges, finite floats, `PlayerVec` lengths, palettes and `DriverMemory` sizes.
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
  - influence and a civilization's uniques get no class of their own. `Friendly`, friendly land and foreign land read another civilization's state, which no class of the civilization in context can name, so they keep `CondDeps::all()`;
  - the city leaves that read beyond the city got classes: `Capital` reads `CITY_COUNT`, `Garrisoned` `UNIT_SET`, and `ConnectedToCapital` every class. Besides roads, harbours, borders and techs, the connection reads the owner's own `Forests and Jungles are roads` (`cities.py:1985`), and a civilization's uniques have no class.
- **Checks.** `scripts/refcheck/filters.py` records Python's truth table of every filter text the ruleset writes (205) in each of the ten domains into `crates/citar-testkit/data/filters.json`, and the 2,050 Rust sets equal them, as does every static filter of the loaded ruleset. Mock worlds test every dynamic leaf. Proptests keep folding's meaning, on abstract leaves (`tests/props.rs`) and on the engine's own: random pairs of real unit, tile, city and civilization leaves merge exactly (`Leaf::and`, `Leaf::or` and `Leaf::constant` agree with evaluation on random mock worlds), and random trees over them answer the same folded as not (`tests/engine/filters.rs`). The golden `filters.json` snapshots every compiled tree.

### 5.8 Conditionals and `CondDeps`

- **The variants.** `Cond { data: CondData, deps: CondDeps, text: TextId }` has one variant per supported conditional (94: the 49 the shipped ruleset uses and the 45 package 1a-05b added), grouped as game, civ, city, unit, combat and tile.
- **Map-generation only.** `InRegionOfType` and `InRegionExceptOfType` exist only in `GenCond`; on an effect they are a load error. As built in 1a-06, they compile as conditionals and the compiler refuses them (`UniqueModifier`) on any unique but a map-generation or an inert one (the start-quality uniques carry them).
- **Decision (condition scopes).** One `bitflags` `CondDeps` (24 bits, leaving the top 8 bits of the `u32` for `UFlags`):
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
  - `in cities connected to the capital` reads every class, since the trade network reads the civilization's own uniques.

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
  - `upon gaining a [unit]` reads its filter wherever it fires; `great_people.py:140` fired it for every great person. `upon being defeated` is a kind like the others, for the combat port to fire. Both are rule differences no refcheck group can show, since the groups ask questions of a standing state and triggers fire only while a turn is played, so they are recorded here and not in `intended.toml`.
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

Mutable access to `State` is restricted to `game::mutate`. `State::{tiles_mut, units_mut, cities_mut, players_mut, diplo_mut, world_mut}` are `pub(crate)`, and `xtask check` allows calls to them only from `game/mutate.rs`, `save/` (loading) and `compat/` (conversion). Rule code writes through `Game` in exactly two ways.

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
pub(crate) fn unit_mut(&mut self, u: UnitId, t: UnitTouch) -> &mut Unit;         // CORE | MOVES | SIGHT
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
- **Driver memory.** `drive` takes the seat's `DriverMemory` out of `Seat.driver` (an empty one on first use), passes it to the driver, and puts it back when the driver returns. All of this happens inside one host call, under the lock, so a snapshot can never observe the seat without it. The bytes are saved and digested (§4.5, §4.10).
- **Stop reasons** cover each case the host must handle:
  - `External` is a human, LLM or MCP seat;
  - `HybridDiplomat` lets Python run the diplomat phase;
  - `AwaitingReply` implements rule T3;
  - `SeatLimit` lets the server release the lock between AI seats on gargantuan maps.
- **The advisor.** `game::advisor` ports the production advisor:
  - `cities.py:1696-1717`;
  - `basic.py:777-875`, the part of `context` it needs;
  - `basic.py:1146-1590`.

  It uses `AdvisorParams` (the live bot's defaults) and `what_if_building`. Engine auto-production (city-states, puppets, `auto_production`) uses it. The bot reuses it in Phase 2, which is plan 2.1's "`advise_production` in the engine".
- **The advisor's random draw.** Python built `BasicBot(seed=city.id)` per pick, and its `self.rng.random()` chose ranged or melee (basic.py:1336-1337). Rust draws from `Purpose::Advisor` keyed `[city, turn]`.
- **Tie order.** `max(scored)` over `(value, name)` picks the lexicographically largest name among equal values. Rust breaks ties by `BuildingId` descending, which is an intended difference.

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
| config | `config_from_json` normalisation (game.py:151-196); seed required; map inline | 1b-03 |
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
pub fn new(rules: &'static Ruleset, cfg: &GameConfig) -> Result<(Game, EventBatch), EngineError>;
pub fn config_from_json(rules: &'static Ruleset, json: &[u8]) -> Result<GameConfig, EngineError>; // requires seed; map inline
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

### 9.4 Invariants and the cache oracle

- **`game::invariants::check(&Game) -> Vec<Violation>`:**

  | Code | Invariant |
  |---|---|
  | ID-1 | keys equal ids; ids unique; every live id below its counter |
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
  | TURN-1 | `current` alive, or the game over; `winner` set if and only if the game is over |
  | PEND-1 | `pending` is empty at every settle point |
  | SETTLE-1 | settle converged within its pass cap |

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
| `GameConfig`, `HostOnly`, `MapSource` | Typed configuration / values saved but never hashed / generated map or inline editor document |
| `HexGrid` | Map geometry derived from `MapInfo` |
| `IdCounters` | Persisted id counters, including `combat_seq` |
| `IdSet<I, W>`, `IdVec<I, T>`, `PlayerSet`, `PlayerVec` | Typed bitsets and vectors |
| `JournalChunk`, `FrameWriter`, `ReplayFormat` | Chronicle delta for the host to append / keyframe and delta encoder / Full or Delta replay output |
| `KeyPart` | Encoding of RNG key parts; `None` is `u64::MAX` |
| `MoveClass`, `MoveCosts`, `RouteLayer`, `Zoc`, `PathTree`, `PathCache`, `PathScratch` | Pathfinding structures: static per-class costs, per-civ routes, zone of control, searches |
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

