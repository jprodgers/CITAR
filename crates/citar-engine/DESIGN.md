# citar-engine: the Rust engine for CITAR 0.1.6 (Phase 1 design)

**Status.** Final design, 2026-09-23, by the lead architect. It merges five area designs:
- workspace and build;
- state, serialisation and digest;
- ruleset and the unique language;
- turn pipeline, caches and API;
- testing and validation.

A critic then reviewed the merged design. I checked every finding against the code at `4b5a912`. The accepted findings are folded into the body. Where I rejected a finding or changed it, Appendix A says why.

**Phase 1 is complete** (package 1e-04, 2026-10-02). Every exit criterion of §1.3 holds; the evidence is in "As built in 1e-04" after §1.3's table. The "As built" notes through the document record what each package built where it differs from or adds to the design.

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

**As built in 1e-04: the Phase 1 exit** (§1.3, §3.4 rule 4, §9.2, §9.3; Appendix B). Every criterion of the table holds. The evidence below is from local runs at this package's head and from CI's runs at its base, `d4e2a94` (v0.1.6). The head differs from the base only in tests, docs, the refcheck tool and one enum variant nothing used, so no game, golden digest or measure moves between the two. The fix round's soak then found a citizen placement that was not a fixed point, and fixed it, which moves one round of the long golden set; every gate ran again at the fix round's head ("Fix round", the last item below).
- **Refcheck** (scope 1, gate 1). `cargo refcheck run --fixtures refcheck/fixtures-mini --fixtures refcheck/fixtures-late --fixtures refcheck/corpus --strict` loads the 262 states (9, 3 and 250) and compares all 14 groups, every one enforced: 0 unexplained, 0 stale, exit 0, in about 19 s on the laptop. `cargo refcheck ratchet` holds, with every group at 0. The 3,701 explained differences come from the 11 entries of `refcheck/intended.toml`, each used: the Marble pair 2,530, `civilians-at-zero-health` 630, `refusals-end-as-sentences` 262, `briefing-names-only-known-players` 149, `city-view-rounds-its-own-sums` 58, `briefing-nearby-reads-what-it-knows` 39, `refusal-lists-capped` 18, `briefing-lists-only-cities-it-could-have-seen` 10, `combat-modifiers-in-ruleset-order` 4, `gold-per-turn-rounds-a-sum-at-a-half` 1.
  - **The two stale entries.** At the base the run had nothing unexplained and two stale entries: 1a-07's `building-conditionals-read-a-filter` and `no-civ-adopted-counts-beliefs`, written before their groups had answer modules, with broad `buildable` paths. No state can show either. The shipped ruleset names a building in every building conditional (`<if [Apollo Program] is constructed>`, `<if [Monument] is constructed in all [non-[Puppeted]] cities>`), and it never uses `<if no Civilization has adopted []>`. Both entries moved to `tests/rules/intended.toml`, beside 1a-07's other conditional fixes, and `tests/engine/eval.rs` shows them:
    - `the_building_conditionals_read_a_building_filter` (new) reads `[Wonder]` and `[Culture]` in four building conditionals;
    - the conditional table's case for `<if no Civilization has adopted [Ancestor Worship]>` fails on another major's belief, and now cites its entry.

    The rule from here on: an entry of `refcheck/intended.toml` must explain something on the recorded states. A deliberate difference no group shows goes in the scripts' list, with the script or test that shows it.
  - **Citations.** All 98 entries of the two lists are cited in the engine as `// refcheck: <id>`. citar-refcheck's repository test `the_repository_lists_are_well_formed_and_cited` replaces the scripts-only one. It covers both lists, reads a citation's id whole (so an id that starts a longer one is not cited by it), and fails on a citation that names no entry. Neither kind existed.
  - **The changelog.** `cargo refcheck changelog --write` writes both lists between two marker lines of `CHANGELOG.md` (`intended::CHANGELOG_BEGIN` and `CHANGELOG_END`; anything but one of each, on lines of their own and in order, is refused). `--check` exits 1 while the file lags, and so does the repository test `the_repository_changelog_lists_the_rule_fixes`. The Unreleased section's Fixed list now holds the 98 rule fixes, after a paragraph saying what they are.
- **Nothing pending** (scope 2, gate 3). No marker was left: 1d-02 and 1d-03 replaced the last two. `ErrCode::NotPorted`, which nothing has returned since, is removed, and `xtask/check.toml` turns `not_ported.forbid` on, which xtask's test of the committed file pins. `cargo xtask check` reports 247 files, 0 `NotPorted` and 0 `Pending`, all clear; any marker, or any path to the variant, now fails it. `game::Porting` and the `pending`/`pending_or` helpers stay: the stage tables use the type, and a later port's marker would be refused by the check anyway.
- **Docs** (scope 3).
  - `docs/ARCHITECTURE.md` gains "The Rust engine (0.1.6)": the crates and layers, the compiled ruleset, the support scope, state, writes and the self-validating memos, the two committed values, the settle, determinism, how the engine is checked and where to change it.
  - `docs/MODDING.md`'s Rust section now covers: every supported type is implemented (fourteen are inert, as in Python); how strictly a mod's JSON and uniques are read; the rules that read differently (now with the Marble and timed-unique fixes); what the committed happiness and the settle mean for a mod; and the steps to add a type, its conditional's dependencies included.
  - `refcheck/README.md`, the engine's README and the scripts' list's header follow.
  - `mkdocs build --strict` and `scripts/check_links.py` pass.
- **The exit criteria** (scope 4, gate 2):
  - *Refcheck clean*: above.
  - *Rule tests*: the 142 scripts and `_selftest` pass on Rust (the 143 trials of `tests/rules.rs`, in nextest on Windows and Linux). They pass on Python too (`tests/test_rule_scripts.py`, 150 tests with `normalize.json`'s cases, skipping the checks tagged `intended`). The plan asked for about 110.
  - *No panics, no corruption.*
    - **Properties at 10,000 cases.** At the head on Windows (nextest's `nightly` profile): P1 to P7 in 435 s, P8 in 944 s, and the other 19 (the pure properties, the seeded-bug hunts, the spec) in 47 s, all passing. At the base, nightly.yml's run 36990822403 passed the same three jobs (329 s, 883 s, 50 s).
    - **Chaos.** At the base, nightly.yml ran 1,200 s on each OS with seed 1000001: ubuntu-latest 2,220 games, windows-latest 2,000, macos-latest 1,850, 0 failures. At the head: 1,200 s on Windows (seed 1605: 1,259 games, 739,337 steps) and on Linux under WSL beside the soak (seed 1604: 1,071 games, 643,403 steps), 0 failures; and 600 s from the corpus states (`--from-fixtures`, refcheck/README's laptop run: 119 games), 0 failures.
    - **The soak.** 2,000 small games on Windows (seed 1604, six shards, 1,547 s): 631,042 rounds, 500 of the games on the kitchen sink (333 of those won before the limit). 0 failures and 0 panics; 2 outliers (rounds of 91 and 76 ms against medians near 10 ms, with the laptop fully loaded); peak heap 28.2 MiB. 200 larger games under WSL (seed 1605, five shards, 1,210 s): 50 each of standard (12 on the kitchen sink), large, huge and gargantuan, 65,263 rounds. 0 failures, 0 panics and 0 outliers; peak heap 126.8 MiB, a gargantuan game. The nightly run at the base also soaked 36 games on each OS, clean.
  - *Determinism.* At the head, `golden check --long` on Windows x64 and Linux x64 (WSL), in the ci and release profiles, finds every set ok, the long one included. `golden_compare.py compare` over the four reports: every set identical and equal to the committed file. `same_process_twice` passes in nextest on both. At the base, determinism.yml's run 36990743395 is green on its six rows, and nightly.yml's golden-long and golden-compare found every set, long included, "identical on 6 targets and equal to the committed file": the five targets (macOS arm64 and x64, and linux-arm64 on real hardware, among them) and linux-x64 in release.
  - *Engine-only speed*: the three citar-bench suites at the head, run one after another at High priority on core 0 with the corpus, once the laptop was quiet (no build, test or game running beside them). `cargo xtask perf --check`: every budget holds within the 1.5x hard limit (329 measures written; the 42 budgets and the 250 pass rounds gated).
    - One measure is over its budget and within the limit, as 1e-03 accepted: `astar_small_30` at 25.8 µs against 20 µs (1.29x).
    - Pass rounds on all 250 corpus states run at least 145x faster than Python's (`refcheck/perf/python-turns.json`; the lowest a duel at turn 1), against the floor of 20x. The slowest t280 rounds are 13.0 ms on small maps (backstop 150 ms) and 34.5 ms on large (backstop 400 ms). One round is over its target budget, within 1.5x: `small-pangaea-raging-s1026/t25` at 2.25 ms against 2 ms.
    - The god view on the gargantuan state takes 10.1 ms (§1.1's 20 ms, target 12 ms), and the snapshot on gargantuan 0.68 ms (100 ms, target 10 ms).
    - A whole small 330-round RandomAgent game takes 0.85 s (budget 10 s).
  - *Nothing left pending*: above.
  - **Also green at the head.**
    - nextest on Windows: 1,141 passed and 2 skipped (the two ignored diagnostics), with the corpus and without it. nextest on Linux (WSL, a fresh clone): 1,141 passed and 2 skipped, without the corpus, as CI runs it.
    - The doctests on both.
    - Python: 608 OK (2 skipped).
    - rust.yml's lint job, step by step: fmt; clippy for the workspace, for the engine in each of the six feature sets, and for the engine as it ships; the fuzz targets; `cargo xtask check`; the docs with `-D warnings`.
- **The pull request** (scope 4). Worktree agents neither push nor write to GitHub, so the exit evidence for PR #5 is drafted for the merge, in the shape of the description's Phase 0 section: "Phase 1: the Rust engine (done)", in the main checkout's git-ignored `ops/pr5-phase1.md`; adding it to the description waits for the owner's approval.
- **Kitchen sink.** No extra unique type is staged for this package; the soak played the kitchen sink in 512 games. Deferred: none.
- **Fix round.** The review's five findings:
  - *Docs.* ARCHITECTURE said the cache oracle ran at every settle in tests, chaos and the soak. Only the invariants do; the oracle runs at every settle in the rule scripts and the tests that set `DebugOptions::ALL`, every 10 steps and after the last in the properties and chaos, and every 50 rounds and at the end of a game in the soak, as both places now say. ARCHITECTURE and MODDING now give the settle's `SETTLE_PASSES` cap: two neighbouring cities whose placements keep moving each other's yields stop there (SETTLE-1 in test builds), so a mod should avoid uniques that do that.
  - *The scripts' list is checked.* Nothing checked that a script or test shows an entry of `tests/rules/intended.toml`, the list `--strict` does not check for staleness. citar-refcheck's new `the_scripts_entries_are_named_where_they_are_shown` requires each entry to be named where it is shown: a script check's `intended = "<id>"` (or a script table's `"intended": "<id>"`), a test's `// refcheck: <id>`, or a test table's `"<id>"`, in the testkit's and refcheck's tests or in the engine's own test modules. Thirteen older entries that no test names yet are spared, in `NAMED_BY_NO_TEST`, a list that only shrinks. To get there, eleven existing tests that show an entry now name it, the two citations of 1a-07's moved entries in `eval.rs` are each on one line, and three new tests show what nothing showed: `vs [All] units` never matches a city, a map summary counts a ruleset's own water, and a deal item that names nothing says what it lacks. The citation check now reads the engine's code without its test modules, so a test's citation cannot stand in for the fix's; a citation in a test must name an entry too.
  - *DESIGN.* §3.4 rule 4 and §8.5 note that `ErrCode::NotPorted` is gone.
  - *The pull request text* is copied to `ops/pr5-phase1.md` (above), out of the build directory a cleanup removes.
  - *A rule fixed* (found by the fix round's soak, not by the review). The 200 larger games of seed 1607 failed once: game 138 (huge) at turn 300, the citizen oracle finding a city's locks where a fresh placement would not leave them. `assign` was not a fixed point of itself for a city with more citizens locked to tiles and set as specialists by hand than it has: it shed one lock, as Python's placement did, and left a lock past its citizens for the next placement to work and shed. It now places the citizens again while a placement sheds a lock or a hand-set specialist (`citizens-shed-locks-at-once`; §6.8's note; `a_city_sheds_the_locks_its_citizens_cannot_keep_in_one_placement` fails without it). The CHANGELOG lists 99 rule fixes. The long golden set moved at round 324 of its gargantuan game, where such a city stood for a round, and was blessed again; its later rounds and its final state did not move.
  - *Every gate again*, at the fix round's head (the WSL clone and the soaks at `716d40b`, which differs from the head in a doc comment and DESIGN.md only):
    - (1) the strict run over the 262 states: 0 unexplained, 0 stale (11 entries, 11 used, 3,701 explained), exit 0; the ratchet holds every group at 0.
    - (2) Rule scripts: the 143 trials pass in nextest on Windows and Linux, and the 150 Python tests of the scripts. Properties at 10,000 cases on Windows: P1 to P7 in 490 s, P8 in 1,110 s, the other 19 in 54 s, all passing. Chaos for 1,200 s on Windows (seed 1608: 1,889 games, 1,131,418 steps) and on Linux under WSL beside the soak (seed 1609: 611 games, 354,485 steps), and 600 s from the corpus states (seed 1610: 145 games), 0 failures. The soak: 2,000 small games on Windows (seed 1606, six shards, 1,569 s; 632,549 rounds, 500 on the kitchen sink) and 200 larger ones under WSL (seed 1607, five shards, 1,746 s; 65,118 rounds, 50 each of standard, 12 of them on the kitchen sink, large, huge and gargantuan, game 138 among them), 0 failures, 0 panics and 0 outliers, the most heap a game held 27.6 MiB (small) and 126.3 MiB (gargantuan). Determinism: `golden check --long` finds every set ok on Windows x64 and Linux x64, each in the ci and release profiles, and on linux-arm64 cross-built and run under qemu-user in both profiles; `golden_compare.py compare` over the six reports finds every set identical and equal to the committed files; `same_process_twice` passes on Windows and Linux. The macOS targets are nightly.yml's once the head is merged. Speed, the three suites at High priority on core 0 with the corpus on the quiet laptop, `cargo xtask perf --check` exit 0, every budget within the 1.5x hard limit: `astar_small_30` 26.4 µs against 20 µs (1.32x, as 1e-03 accepted); every pass round on the 250 corpus states at least 122x faster than Python's (126x when the turns suite ran again; the lowest are duel rounds at turn 1, about 0.2 ms, which move by tens of percent from run to run), the slowest t280 rounds 12.7 ms on small maps and 34.3 ms on large, one round over its target within 1.5x (`small-pangaea-raging-s1026/t25`, 2.04 ms against 2 ms); the god view on gargantuan 9.9 ms, its snapshot 0.72 ms; a whole small 330-round game 0.83 s.
    - (3) `cargo xtask check`: 247 files, 0 `NotPorted`, 0 `Pending`.
    - Also green: nextest 1,146 passed and 2 skipped on Windows (with the corpus and without) and on Linux (a fresh clone, without); the doctests; Python 608 OK (2 skipped); rust.yml's lint job, all twelve steps; `cargo refcheck changelog --check`; `mkdocs build --strict` and the link check.

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
overflow-checks = false            # 1e-03: they cost 5-9% of a pass round (see 10)
[profile.ci]
inherits = "release"
lto = false
codegen-units = 16
incremental = false
debug-assertions = true
overflow-checks = true             # every test, golden and chaos run keeps them
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
   - As built in 1e-04: Phase 1 ported every rule, and `ErrCode::NotPorted` is removed. `xtask/check.toml`'s `not_ported.forbid` is on, so `cargo xtask check` refuses a `NotPorted(..)` marker, the `not_ported(..)` helper and any path to the variant. Since `pending.forbid_all` (1c-10) refuses any `Pending` stage as well, unfinished work stays on its own branch until it is done.
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
  - `StatsRow` must carry baseline.py's `STAT_KEYS` (as built: every key `victory.record_stats` wrote, which include baseline.py's 8 and balance.py's 13).
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
- **Cities' turns** (`cities::lifecycle`): S5 finishes what is paid for, starts We Love The King Day for a demanded resource, counts down resistance and the celebration and draws the next demand (keyed RNG, `Purpose::Demand` and `DemandNew`), and tells a human owner of an idle city; E4 production, border growth, razing or growth and starvation, healing. A destroyed city's capital moves to the largest city left; its religion, espionage and elimination wait for 1b-08, 1c-05 and 1c-08. Since 1c-08, an owner a destroyed city leaves defeated is eliminated through `victory::eliminate_if_defeated`: at once, or as the round ends if it is the player whose turn it is (see "As built in 1c-08"). `Game::turn_yields` holds the turn's totals from E2 for E3's science.
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
- As built in 1e-02: a city's citizens are the one input that can move within a settle, where the ruleset's uniques read them (a conditional on a city's specialists), and then the best assignment may have no fixed point. A city whose reassignment would give it back an assignment the settle has moved it away from takes it only if it stands there, and otherwise keeps the one it has; the citizen oracle accepts a city its reassignments lead back to; see "As built in 1e-02" after §9.6.

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

As built in 1e-04's fix round: the greedy placement (`citizens::assign`) is a fixed point of itself, locks included, which the oracle compares with the rest of the assignment (and its message now names). A city with more citizens locked to tiles and set as specialists by hand than it has sheds the extra ones, a locked tile's lock with them (`_unassign_extra`), and a lock past its citizens is then the next one worked; Python shed one lock a refresh. `assign` now places the citizens again while a placement sheds a lock or a hand-set specialist, so it ends where those refreshes would (`citizens-shed-locks-at-once`). The soak found it (seed 1607, game 138: two citizens locked to three tiles, one lost and a merchant set by hand, a lock left past the one citizen at turn 300); `a_city_sheds_the_locks_its_citizens_cannot_keep_in_one_placement` fails without the change. The long golden set moved at one round of its gargantuan game (324), where such a city stood for a round; its later rounds and final state did not.

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
- **Scripts and inspect.** `inspect` gains `tile.visible` (the players who see the tile) and `player.natural_wonders`, on both engines. The five scripts pass on Python and Rust. Two city-states never meeting is a unit test only: the arena has one city-state start, and the others needed `maps.prepare`, which 1c-09 ported (the arena's second and third sites are (13,12) and (14,5)).
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
- **Waiting for their packages,** marked where Python had them: a barbarian's sack of a city it beat and a camp attacked (1c-06), the city-states' thanks for kills (1c-06), spies fleeing a captured city (1c-05), eliminations and domination after a loss or a capture (1c-08), the effects of triggers other than a unit's and the great general points of combat (1b-08). Merged after 1b-08, those two are real: the triggers combat and conquest fire apply through 1b-08's `triggers::apply`, and a fight's experience earns great general points through `promotions::add_xp` (`great_people::add_combat_points`); the kitchen-sink tests still read what fired from the trace. Since 1c-08, eliminations and domination are real too: a capture eliminates a defeated owner at once (with its conqueror) and then asks for the Domination victory, and a unit killed eliminates its owner if that left it defeated, except the player whose turn it is, which the round's end eliminates (see "As built in 1c-08").
- **Tools, operations and views.** The actions `attack` (a nuclear weapon detonates, an aircraft strikes, anything else attacks), `air_sweep`, `city_attack`, `city_status` and `return_civilian`, with their argument specs; `move_unit` rebases an aircraft. The test operations `attack_as` and `capture_civilian` (a unit, or a player: scenarios cannot add barbarian units) are ported on both engines. `inspect` gains `preview` (the preview, or its refusal as `{error}`), a city's `max_health`, `founder`, `previous_owner`, `original_capital`, `puppet`, `razing`, `resistance` and `attacked`, and a unit's `original_owner` and `return_offer`. `RandomAgent` fights (`agents::fight`), before its units move.
- **Invariants.** CITY-1 holds a city's health at most its maximum.
- **Refcheck.** `combat_previews` is enforced whole, clean on the 12 committed states and the 250 of the corpus; `civilians-at-zero-health` explains the corpus's previews of a civilian loaded at 0 (35), and `combat-modifiers-in-ruleset-order` one corpus state (4).
- **Kitchen sink** (`tests/engine/combat.rs`): `[n] Strength`, `May attack when embarked`, `No defensive terrain penalty`, `[n] Air Interception Range` (the kitchen-sink interceptor now has a chance to intercept), `May not annex cities`, `Never destroyed when the city is captured`, and the triggers `upon conquering a city`, `upon losing a city`, `upon losing a [unit] unit`, `upon defeating a [unit] unit` (with its unit effect) and `upon being defeated` (with a free upgrade and `[This Unit] is destroyed`, on a kitchen sink whose warriors carry them: the unit stays dead after a fight and after a blast). Deferred: none.
- **Gates.** 2: damage grows with the attacker's strength: a property over `CombatSetup::for_test` (feature `test-ops`; the strengths, the roll, both sides' wounds, a civilian defender, a ranged attack) that asks the fight's own damage functions, and real fights of a warrior with and without `[+2] Strength` at several wounds. 3: each fight, bombardment, interception, air sweep that meets a candidate and detonation takes one `combat_seq`, a sweep that meets no one none; a game saved and loaded between two attacks fights the second alike. 4: the thirteen scripts pass on Python and Rust (`combat_air` with a fighter that meets a sweep and fights it); the fighter's sweep is also checked in full on Rust (the damage within its sweeping fight's, `[+33]% Strength when performing Air Sweep`, 5 experience each, either side shot down). Liberation reviving a civilization is an engine test (`conquest::tests`), since Rust eliminates at the round's end (1c-08) where Python did at once. Since 1c-08, Rust eliminates at once too, except the player whose turn it is (see "As built in 1c-08"); the script `combat_liberation` passes on both engines.
- **Criterion** (laptop, other packages building): `combat/preview` (`preview_of`: the checks, one setup, four damages) 1.45 µs median over the committed fixture with the most fights (budget 1 µs, report-only), its checks 0.4 µs and its setup 1.0 µs; the JSON the tool reports adds about 2 µs. What remains is about ten unique queries per fight, each looking up a unit's profile index; a setup that looked the profiles up once would take it under the budget (a candidate for 1e-03).

**As built in 1c-04** (§6.2, §6.5, §6.11, §8.3, §9.2-9.4, §9.7):
- **Files.** `game/workers.rs` (`workers.py`), `game/actions.rs` (`actions.py` and `tools.py`'s `found_city`), `game/automation.rs` (`automation.py`), `game/religion/spread.rs` (`religion.py:709-775`), `game/derive/{jobs, danger}.rs`; the testkit's `tests/engine/workers.rs` and `data/worker_jobs.json` (from `scripts/refcheck/worker_jobs.py`); the bench `crates/citar-bench/benches/jobs.rs`; the scripts `workers_road_pillage_repair`, `workers_forest_chop_farm`, `workers_automated`, `workers_turn_flow`, `workers_pillage_improvement`, `units_explore`, `units_paradrop`, `great_people_actions`, `religion_spread` and `found_city_refusals`, which pass on Python and Rust.
- **Who builds** is a `workers::Builder`: a unit, whose own uniques and conditionals count (the tools, and the direct port of worker automation), or a civilization's builder class with no unit (the job maps), whose `Can build [...] improvements on tiles` filters stand for the unit's. Every placement rule (`built_here_ok`, `building_problems`, `unit_can_build`, `turns_to_build`, `needed_removals`) takes the builder and an explicit tile, never a unit moved to it. An improvement over unbuildable features it may not stand on is allowed once the civilization knows how to remove each in its way from the top down, and queues every such removal first, top first (fallout, then the forest under it), its time counted in the options and the job maps. Work in progress is the tile's `BuildQueue`, advanced at E6 (`progress_builds`) by a builder that ends its turn on the tile with movement left; what is finished, and what a unit makes at once, is checked against the tile as it stands (`problems_now`: no removal still to come, but for an improvement that removes features itself), so nothing is set on a feature it may not stand on; `set_improvement` runs an improvement's one-time uniques and `upon building a [...] improvement` (the civilization's, with the tile and the unit in context), removals clear their feature and a chopped forest or jungle gives its nearest city production. Pillage (`can_pillage`, `pillage`) takes the improvement, else the route; its random loot draws from `Purpose::Pillage` keyed `[turn, tile]`, two draws of up to each amount as Python drew.
- **Actions.** `actions::unit_actions` lists what a unit can do now with Python's ids (`found_city`, `found_religion`, `enhance_religion`, `spread_religion`, `remove_heresy`, `hurry_research`, `hurry_construction`, `trade_mission`, `political_treatise`, `create:<improvement>`, `paradrop`, `add_to_spaceship`, `trigger:<n>`, numbered over the unit map's uniques as Python numbered them: the base unit's, its type's, each promotion's; the profile index now holds a unit's action uniques). `plan_action` checks on `&Game` and `apply_action` cannot fail: religion's `plan_religion`/`apply_religion` and `plan_enhance`/`apply_enhance`, the spread pair, the great-person pairs of 1b-08 and a one-time effect a unit carries, refused when it would do nothing by `triggers::would_apply`, which answers each kind of effect on `&Game` with no copy of the game (debug builds check it against the effect applied to a copy). `found_city` is the settler's action on `cities::founding::found_city_by`, which fires `upon founding a city` with the settler in context before it leaves the game; a city-state that has a city founds no other (`city-states-found-one-city`), whoever asks its settler. Adding a spaceship part is refused as not ported (1c-08). Since 1c-08, `add_to_spaceship` is ported (`victory::milestones::add_to_spaceship`), and adding the last part may win the Scientific victory. A paradrop's range is written as Python's float (`5.0`).
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
- **RandomAgent** answers what waits on it, now and then messages, opens a negotiation from a small pool (one time in ten), denounces, and after turn 50 declares war, then withdraws what it opened; its spies move now and then; `respond` answers a negotiation from a stream keyed by it and its length. Until 1c-09's `drive` dispatches `respond`, the side a chat it opens waits on answers at once (`agents::converse`, up to four answers, from the same streams), so drive carries out its deals; an answer the game refuses ends the chat, and it accepts twice as often as it does anything else. `random_agents_strike_deals_that_run_their_course` drives three who have met for a hundred turns. Since 1c-09 the agent leaves its chats to the drive, which puts them to the other side's driver, and `converse` is gone.
- **Kitchen sink** (`tests/engine/diplomacy.rs`): `upon declaring friendship` (three tiles for the capital) and `upon declaring a defensive pact` (forty science) fire once each for a deal of both, `[+15]% spy effectiveness [in all cities]` speeds the kitchen sink's spies at home and abroad, and `Spies in [Capital] cities act as though they have [+1] levels for [Counter-intelligence]` raises the rank of a spy guarding the capital and of no other. Deferred: none.
- **Gates.** 1: `deal_checks` is enforced whole, without `bot_value`. 2: the 18 negotiation-chat scripts and the four diplomacy scripts of `test_engine.DiplomacyTests` pass on Python and Rust, with three more (messages, denouncing, spies). 3: the `end_turn` refusal both ways (the opener waiting, the responder answering) while the host's `end_turn` expires the opener's chats (`negotiation_host_end_turn_expires`). 4: `negotiations_keep_diplomacy_consistent`, a property (64 cases) over random negotiations between three civilizations, the second and third with a defensive pact, a cap of three messages, an accept step and wars on both of the pact's partners in the pool, checks every invariant after each step, and that no deal stays in force between two at war.
- **Counts** at this package's head (after its fix round): nextest 880 passed, 1 skipped (`--workspace --all-features`, the corpus on); Python 555 OK (2 skipped); `cargo xtask check` 11 NotPorted, 40 Pending; the ratchet holds `deal_checks` at 0, clean on the 12 committed states and the corpus's 250; `cargo doc` with `-D warnings` clean.

**As built in 1c-06** (§4.5, §6.2, §6.10, §6.11, §6.14, §7.2, §8.3, §9.2-9.4, §9.7):
- **Files.** Engine: `game/barbarians.rs` (with `barbarians/tests.rs`), `game/city_states/{influence, actions, quests, turn, ai}.rs` (with `city_states/tests.rs`), the city-state side of `game/espionage.rs`; `rules::derived::Known::city_state_builds` (the eight buildings a city-state prefers, by name at load). Testkit: `tests/engine/city_states.rs`, fifteen scripts (nine `barbarians_*`, six `city_states_*`), `agents::city_states`. Bench: `benches/barbarians.rs`. Python: the test operations `add_barbarian`, `add_quest`, `barbarian_act`, `clear_camps`, `create_camp` and `sack_city`, and the inspect queries `camps` and `city_state`.
- **Barbarians** (`barbarians`). Aggression (the game's `barbarian_aggression`, else its level's) sets every knob, as `barbarians.py:32-95` did. Camps (`place_camps`, `create_camp`, `update_camps`) appear out of every living civilization's sight, on free land beside land, 4 tiles from capitals and 7 from camps (4 from a destroyed one, which lingers 15 turns); the first at setup (`place_initial_camps`, a third of what the fog holds room for), later one on half the turns. A camp spawns on itself when its countdown runs out (beside it from turn 10 while few barbarians are about, at sea from turn 30), a unit type weighted by UnCiv's force evaluation among what the barbarians' techs allow, which follow the techs every living civilization shares. A camp attacked halves its countdown; a civilization's military unit entering it clears it (`clear_camp`: the difficulty's reward, `GainFromEncampment` recruits, `GoldFromEncampmentsAndCities`); borders taking its tile remove it (`remove_camp`). A city a melee barbarian beats is sacked (`sack_city`, `combat::city::CityOutcome::Sacked`): gold, maybe a citizen and a building (never a wonder, the palace or a free building), health to a quarter of its maximum, left alone for 5 to 10 turns. The AI (`take_turn`, stage S0): ranged units, then melee, then captured civilians (to the nearest camp they can reach); a wounded unit pillages first; then up to three attacks or pillages in reach, whichever is worth more; then the best of five targets within the search radius, else a random tile in reach.
- **Where the raider stands.** The attack search (`attack_targets`) asks `resolve::preview_of_from` from each tile the unit could attack from, never moving it (§6.11); `_seek` builds one `PathTree` with its turn bound and reads every candidate's path from it, following the path it found (`movement::follow`). Draws are keyed by ids and the turn (§7.2): the placement by the next camp id, a spawn's side by the camp's tile, its unit by the camp and its tile (a camp with no record keys as none, never as its tile), the countdown by the camp, a sack by the city, a wander by the unit.
- **City-states** (`city_states`). `influence` gains `FRIEND_INFLUENCE` (moved from `core`), `Relationship` (`Unforgivable`, `Enemy`, `Ally`, `Friend`, `Afraid`, `Neutral`), `friendship` (ally, friend or neither, which the end of the turn, unit gifts and border tension read, skipping the tribute test that tells the afraid from the neutral), `resting_point`, `degrade`, `recovery`, `is_aggressor` and `is_warmonger`. `turn`: setup (`init_city_state`: a personality drawn unless the nation names one, a unique luxury and a gifted unit where the type's friend or ally bonuses provide them), the end of its turn (stage E3: drift toward the resting point, countdowns, unit gifts, border tension, free techs, quests, war quests of wars that ended, the election tick), the great people allies give (stage S3, `turns_for_gp_gift` moved from `triggers`), first contact (`on_meet`, called from `Game::make_contact` before the `first_contact` event, as `game.py:700-702`), `on_attacked` (from `set_war`), `on_military_unit_killed` and `barbarian_killed_near` (from a kill), and `on_destroyed` (for package 1c-08's eliminations). `actions`: the `city_state_action` tool (`CityStateAction`, plans `plan_gift_gold`, `plan_gift_unit`, `plan_pledge`, `plan_withdraw`, `plan_tribute`, `plan_peace_with_city_state`, `plan_marriage`, each with its apply), `tribute_modifiers` in Python's order, and `military_strength` (`victory.py:46-52`, which a city-state's fear reads and 1c-08's score may reuse). A protector that attacks its city-state breaks its pledge (`withdraw`). `quests`: `QuestTarget` by the row's `QuestKind`; the route quest reads connections through `path::cost::has_connection`, which combat's connected-tile bonus shares (the top feature but a hill, as a search reads it); `quest_target` (drawn from `Purpose::Quest` keyed by the city-state, the major, the row and the turn, so asking twice gives the same answer), `quests_end_turn`, `complete_quests`, `quest_event`, `camp_cleared`, `camp_removed`, `quests_for` (for 1d's views). `ai`: stage S8, founding with its settler, bombarding, production from the cached `Buildable` list (Gold when broke, a defender while short of `2 + cities`, a favourite building whose upkeep it meets, else the first such, else Gold), units attacking in reach or holding the capital.
- **Espionage.** `city_state_election_tick` (the first election drawn from `Purpose::ElectionDelay` keyed by the city-state, then every `city_state_election_turns`), `hold_elections` (the riggers in the capital and nobody at 20, weighted by half the civilization's influence and the spy's skill by its efficiency; drawn from `Purpose::Election` keyed by the city-state and the turn), `can_coup`, `coup_chance` and the coup itself at the end of the spy's turn (its roll from the spy's own stream), and the `stage_coup` tool (`StageCoup`).
- **Wiring.** Stages S0 (the barbarians act), S3 (the great-person gift tick), S8 (the city-state's turn) and E3 (the city-state's own end of turn) and the setup stages `city-state init` and `camps` (sight settled first) are ported; no stage or setup stage waits for 1c-06. The actions `city_state_action` and `stage_coup` have their argument specs; `RandomAgent` deals with a city-state it has met one time in five and its spies stage coups now and then.
- **Differences** (`tests/rules/intended.toml`): `city-state-gifts-need-a-common-nation` (a militaristic city-state's faster gifts need a war both fight against a civilization or city-state; Python counted the barbarians, at war with everyone). A marriage leaves the city-state with nothing, and its elimination waits for 1c-08, which calls `on_destroyed`. Since 1c-08, the married city-state is eliminated at once through `victory::eliminate_if_defeated`, with no destroyer to answer for it (see "As built in 1c-08").
- **Scripts.** The bare prelude also clears the camps (`clear_camps`), so a script that turns the barbarians on places its own. Nine barbarian scripts (sack instead of capture, a sacked city left alone, wonders spared, aggression storming cities and hunting from afar, loot before a fight, a captive carried to a camp, camps spawning and raging ones faster, a camp cleared and attacked) and six city-state scripts (influence decay, gifts, tribute, alliance and protection with an attack's fallout, a coup, and quests) pass on both engines. `city_states_quests` places quests with the test operation `add_quest` and pins what follows on both: Give Gold and Pledge to Protect done as they happen, a camp cleared paying its clearer and ending the others' quests, a bully quest paid and the bully's individual quests and investment cancelled on tribute, a quest that ran out dropped and a contest decided. Quest assignment, elections, successful coups, unit and great-person gifts and marriage are unit tests, since their draws differ between the engines.
- **Refcheck.** Every path of `civs` a city-state has is enforced and clean; `civs[*].military_strength` is answered and enforced, and the ratchet falls from 126 to 88 on the committed states (4156 to 2854 unexplained on the corpus's 250, clean where enforced).
- **Kitchen sink.** No extra unique type is staged `barbarians` or `city_states`; the espionage extras were 1c-05's. Deferred: none.
- **Criterion** (laptop, `small-continents-raging-s1005` at turn 120 of the corpus, the nearest to turn 100; `cargo bench -p citar-bench --bench barbarians`): `barbarians/round`, stage S0 on a fresh copy (71 barbarian units, 18 standing camps, aggression 85), 4.7 ms median against a budget of 2 ms (report-only: about 60 µs a unit, most of it the units' searches and moves; the camps' turn alone 118 µs, the units' start 47 µs). The reach a unit's attack and loot share is searched once; the multi-turn `PathTree` of `_seek` and the moves are what is left for package 1e-03.
- **Fix round.** The bench passes CI's lint (`clippy --all-targets`). `path::cost::has_connection` is the one connection test outside a search, which combat's connected-tile bonus and the route quest share (the quests' own copy took any forest or jungle on the tile). `influence::friendship` spares the end of the turn, unit gifts and border tension the tribute test. The election and great-person countdowns saturate, since a save may hold any `i16` there. A spawn's unit draw keys the camp as an `Option` beside its tile. The test operation `add_quest` (both engines) and `city_states_quests` pin quest completion, rewards and drops on both.
- **Counts** at this package's head (after its fix round): nextest 978 passed, 1 skipped (`--workspace --all-features`, the corpus on); Python 580 OK (2 skipped); `cargo xtask check` 5 NotPorted, 18 Pending; ratchet `civs` 88 (2854 unexplained on the corpus, clean where enforced).

**As built in 1c-08** (§4.7, §4.11, §5.3, §6.2, §7.2, §8.1, §8.3, §9.2, §9.3):
- **Files.** Engine: `game/victory/{mod, score, milestones, un, records}.rs` (`victory.py`, the round's end of `turns.py:190-202`), `game/revolts.rs` (`turns.py:134-187`), `rules::derived::KnownVictories` (the Scientific, Cultural, Domination, Diplomatic and Time victories by name at load, each optional: `game.py:46` and `victory.py` named them), `save::journal::event_range` (the event ids a frame record covers, read from its last 8 bytes, so a frame after a load starts where the last one saved ended), `diplomacy::negotiation::cancel_for`; `Game::stats` (§8.1) reads a `last` of 0 as all rows, as the facade's `if last` did. Testkit: `tests/engine/victory.rs`, six scripts (`victory_domination`, `victory_elimination`, `victory_turn_limit`, `victory_un_vote`, `victory_spaceship`, `victory_cultural`), `agents::un_vote`. Python: the inspect queries `victory` and `un`.
- **Score and victories.** `score` returns `Score` (Python's parts, each rounded to a tenth, and their truncated sum); `military_strength` moved here from `city_states::actions`, which reads it. `milestones`: `required_parts` (the Scientific victory's, counted in the order first named), `spaceship_status`, `add_to_spaceship` (the `add_to_spaceship` unit action, no longer refused as not ported), `milestone_done` over the compiled `Milestone`, `victory_progress`, `victory_achieved`. `Won` is a victory of the ruleset or `Neutral` (`Triggers victory`, which no victory of the ruleset stands for): the clock keeps a winner with no victory id for it, and `inspect` and `won_by` name it `Neutral` as Python did. `declare_winner`, `check_victory` (a player alone, or every living major in id order), `check_domination` (holding every original capital, or the last of several standing) and `check_turn_limit` (the Time victory to the best score, the first of equals, else a game over with no winner) are Python's.
- **Eliminations.** `is_defeated` and `check_elimination` are Python's: the player dies through `Game::kill_player` (its units go), its open negotiations are cancelled with `"<name> has been eliminated."`, its deals end, its spies come home, a city-state's destroyer answers for it (`city_states::turn::on_destroyed`) and it loses its ally, everyone is told, and a major's fall may leave another the Domination victory. Stage R0 asks every living civilization and city-state; the sites Python asked at once ask through `eliminate_if_defeated`: a capture (with its conqueror, `conquest.py:149`), a unit killed (`combat.py:610`), a city destroyed (`cities.py:2377`) and a marriage (`city_states.py:800`); a capture also asks for the Domination victory (`conquest.py:194`). **Difference** (`eliminated-on-its-own-turn-at-the-round-end`): the player whose turn it is is left to the round's end, where Python removed it mid-turn and then ended the turn of a player it had removed; a player eliminated at R0 whose turn it was hands the turn to the next living one, so invariant TURN-1 always holds. Liberation brings a founder back as before (`conquest::liberate`).
- **The United Nations** (`un`): `schedule_vote` (moved from `triggers`), `un_owner`, `votes_needed`, `vote_open`, the `un_vote` tool (`UnVote`, any time, argument `candidate` of any JSON type as Python read it), `hold_vote` and stage R5's `vote_stage` (while the Diplomatic victory is on). Votes not cast are the seats' (`_ai_vote`): a city-state for its ally, a civilization whose seat votes for it for the one it thinks best of, drawn from `Purpose::UnVote` keyed `[voter, turn]` (§7.2), abstaining when it thinks badly even of that one. **Differences:** the tally lists equals by player id; `un-city-state-votes-for-a-living-ally`; `un-vote-for-no-player-refused` (Python raised an uncaught error, or counted a negative id from the end); `victories-python-named-are-the-rulesets`: a ruleset without a Diplomatic victory holds no vote, and without a Domination or Time victory has no such win (Python acted as if they were on).
- **Records.** Stage R2 appends a `StatsRow` (a `CivStats` per major, by id, through `journal::Record`; the heads keep it as `last_stats`). Each row has every key `victory.record_stats` wrote (18, with `alive`), which include `scripts/refcheck/baseline.py`'s 8 `STAT_KEYS` and `citar/balance.py`'s 13 (the "33" of the gate was no count of either). Stage R3 captures a `FullFrame` and pushes it through the game's `FrameWriter` into the chronicle (host activity, not digested): a keyframe first, after a load and every 64 frames, else a delta. Every `war_declared` names attacker and defender, now also a war agreed in a deal (Python's said neither, `deal-war-names-both-sides`: a Rust baseline counts those wars where Python's did not), and `city_captured` old and new owner.
- **Revolts** (stage S3, majors): while `Rebel units may spawn` holds and the game has barbarians, a countdown of `base_turns_until_revolt` plus a draw below 3 (`Purpose::RevoltDelay`), scaled by a slower speed; when it runs out, rebels (`Purpose::Revolt`, both keyed `[civilization, turn]`): one, or more with more cities, of a land melee unit type the civilization could field, beside a city drawn by size, on its tile where rebels stand best. A countdown loaded at 0 or below runs out at once.
- **Stages.** S3 revolts, S9 and E6 victory, R0 eliminations, R2 statistics, R3 frame, R5 vote, victory and turn limit: no turn stage waits any more (`Stage::later` is gone).
- **Kitchen sink** (`tests/engine/victory.rs`): `Triggers victory`, the one extra staged `victory` (1c-07 read it for the advisor), wins the neutral victory as its holder's turn starts. `SpawnRebels`, `CannotAttack`, `OneTimeTriggerVoting`, `AddInCapital` and `EnablesConstructionOfSpaceshipParts` are the shipped ruleset's. Deferred: none.
- **Gates.** 1: `civs` is enforced whole (`score`, `victory_progress` and the `world`: the world era, the United Nations' owner, the votes needed, whether voting is open), clean on the 12 committed states and the corpus's 250; the ratchet falls from 88 to 0. 2: the six scripts pass on Python and Rust. 3: `each_rounds_statistics_row_carries_every_key_python_wrote` and `every_war_declared_and_city_captured_names_both_sides`. 4: `the_frames_of_a_hundred_turns_decode_to_what_each_round_captured`: three random agents, a city-state and barbarians, a hundred turns on the arena, saved and loaded at turn 50; every frame in the chronicle decodes to the frame captured as its round ended (feature `test-ops`: `records::take_frames_for_test`), the ranges of event ids follow each other, and the last frame is the map as the game ends.
- **Fix round.** `check_elimination`, `check_victory` and `un::cast_vote` are `pub(crate)`: only stage R0 eliminates directly, every other site through `eliminate_if_defeated`, which spares the player whose turn it is, and a host votes through `Action::UnVote`; the read-only functions stay public for 1d. `tests/rules/intended.toml` lists `victories-python-named-are-the-rulesets` (tested with a ruleset without the three: no vote, no last-standing win, a game over with no winner at the turn limit) and `deal-war-names-both-sides`, each cited where it is made, so `cargo refcheck changelog` lists them. The earlier as-built notes of 1b-03, 1b-07, 1c-03, 1c-04 and 1c-06 that left eliminations, the spaceship or the revolts to 1c-08 say what it did.
- **Counts** at this package's head (after its fix round): nextest 995 passed, 1 skipped (`--workspace --all-features`, the corpus on); Python 586 OK (2 skipped); `cargo xtask check` 4 NotPorted, 4 Pending (no turn stage); every ratchet group at 0, `civs` clean on the corpus's 250 too; `cargo doc` with `-D warnings` clean.

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

  As built in 1c-09 ("As built in 1c-09" after §6.14), the drive also puts every negotiation that waits on a driven seat to its driver's `respond`, whoever's turn it is; a driver may leave one to the host (`DriverOutcome::Deferred`, a hybrid seat's bot leaving it to the seat's model), which then holds the turn of a driven seat in it (`AwaitingReply`) as a seat the host plays does; and a stop inside a turn (`HybridDiplomat`, `AwaitingReply`) is kept in the host heads, so the next drive goes on from it.
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
- **Stage tables** (`game::turn::stages`). One row per system step, not one per stage: `PLAYER_START` has 23 rows, `PLAYER_END` 25 and `ROUND_END` 11, each labelled with its stage of §6.2 (`S0`-`S9`, `E0`-`E6`, `R0`-`R6` for the round) and owned by one package, so a stage two packages share (S2: research 1b-07, great people, religion and the Maya 1b-08) is two rows, each flipped by its own package. A row has `who` (majors, city-states, barbarians), `when` (`HasCities`, `Religion`, both, or `EvenIfOver` for the round's close) and a `Step`: `Player(fn)`, `Round(fn)`, `Settle` (the ◆ points), `StopIfDead`, `StopIfBarbarian` (Python's early returns), `SkipIfOver` (R0: a round whose eliminations end the game skips to its close, the R6 rows marked `EvenIfOver`, so it is still settled and digested) or `Pending`, the no-op of a row whose `porting` is `Pending("<pkg>")` (a unit test holds the two together). Ported in 1b-03: the control flow, the settle points, the `turn_start` and `turn_end` events (S9, E6), the lapse of unanswered civilian-return offers (E0), the next turn (R4), the round's optional digest (R6) and the turn limit (R5): a game whose last turn is past ends with no winner and a `game_over` event, the Time victory by score waiting inside it as `Pending("1c-08")` (ported since 1c-08), so drives and games reach an end. Every other row is pending: 40 rows, which `inspect` `pending` lists as `turn_stage` (`player_start S2: research progress`). `stages::waiting()` lists them.
- **Kitchen sink.** `upon turn start` and `upon turn end` (the extras staged `Turn`) are rows S4 and E1, `Pending("1b-08")`: firing them needs the civilization's unique index (1b-05) and `apply_one_time` (1b-08), like every other trigger site; a kitchen-sink game sets up and plays its turns meanwhile. `SpawnRebels` and `CannotAttack` (staged `Turn`) are the revolts of row S3, `Pending("1c-08")` (ported since 1c-08, `game::revolts`). The `Setup` types (`StartingTech`, `StartsWithTech`, `DisablesReligion`) are read by setup.
- **Turn driver** (`game::turn::driver`). `Game::end_turn` (host) and `end_turn_now` (the rule) refuse a game that is over and a player whose turn it is not ("It is not your turn (it is Rome's turn).", the tools' wording, `end-turn-names-whose-turn-it-is`); then `PLAYER_END`, and the loop of `game.py:1017-1033`. With no living major a call ends the round and returns at the next round's start, player 0's turn not begun, so nobody ends a turn twice in a round (`end-turn-stops-without-majors`; Python's loop never returned); a turn not begun begins before it ends. `begin_turn`, `end_round` and `force_turn_now` are `pub(crate)`; the host's `force_turn` and the test operation refuse a player the game does not have, a dead one (`Eliminated`, "Greece has been eliminated and plays no turns.") and a game that is over, where Python made a dead player's turn current or moved a finished game's turn (`force-turn-only-for-the-living`). `Game::set_chain(Option<DigestChain>)` opts a game into the round digests of §4.10: row R6 folds each round's digest in after the round's settle and before the next turn begins, under the round's own turn number, taken as its end begins (`chain()`, `last_round()`, `last_round_digest()`); the chain is never saved, and a host resumes it with `DigestChain::resume`.
- **Drivers** (`game::turn::drive`). `SeatDriver: Send` has `play_turn` and `respond` (called from 1c-09); `DriverOutcome` has `Done` and `Stop` has `External(pid)` and `GameOver`, both `#[non_exhaustive]` for 1c-09's variants; `DriveOptions` is empty until 1c-09's seat limit. `Game::drive(&mut Drivers, DriveOptions) -> Result<(Stop, EventBatch), ActionError>` (a poisoned game refuses) begins a turn not yet begun, ends the turn of a seat that is no living major (a forced turn), stops at a seat with no driver, and otherwise hands the driver a copy of the seat's `DriverMemory` (an empty memory of kind 0 when it has none), writes it back only when the driver changed it (kind 0 and empty is stored as none, so a driver that keeps nothing leaves the seat unchanged), and then ends the turn; the seat holds its memory throughout, so a digest or snapshot taken while a driver plays sees it, which is §6.12's guarantee kept without taking it out. Ending the turn is `drive`'s alone, as `run_ai` did (`play_turn(end_turn=False)`): while a driver plays (`Game::driving`), `end_turn`, `force_turn` and `drive` refuse (`ErrCode::Rule`), so a round a driver's turn closes is digested with the memory that turn left, and the chain is the same whoever asked; a turn that passed anyway is reported as TURN-1. Package 1c-09 sets `driving` around `respond` too. Its batch holds every event since it began, whatever the driver's own calls took. It stops with `GameOver` when no major is alive. `Drivers::none(n).with(p, &mut d)` builds the seats. `citar_testkit::agents::RandomAgent` draws from `Purpose::TestAgent` keyed `[pid, turn]` and plays the moves in `agents::MOVES`, empty until the system packages add theirs. Package 1c-09 completed the drive: the other stops, the answers put to drivers and the seat limit ("As built in 1c-09").
- **Setup** (`game::setup`). `Game::config_from_json` (and `config_from_value`, which takes the parsed JSON whole, so the map document and the seats move into the `NewGame` uncopied) normalises `game.py:151-196`: null means absent; the seed is required ("The settings need a seed: ..."), and a map must come as the editor's document ("The map must come inline, ... (got the id 'islands')."); names resolve loosely and an empty one is the default, but a name the ruleset or lobby lacks is refused listing what is valid when the table is short (`config-refuses-unknown-names`); the seats are checked in Python's order (count, then each handicap and `auto`, then controller, nation and difficulty), and kept in `GameConfig::host["players"]` (an empty list becomes the default number of `{}`), with every key the engine does not read; the starting era defaults to era 0, the barbarians to `normal`, a generated map to `small` and `continents` (lobby keys); the lobby's resource options are read as leniently as `MapOptions` read them. `SETUP` is the table of §6.14 with 15 rows: `Draft` rows make the state (config, nations, map, players), then `Game` rows play on the new game (starting techs, relations, begin); 8 are pending, which `inspect` lists as `setup_stage`. `Game::new` moves the map's tiles and continents and the players out of the draft into the state. Ported beyond the table's 1b-03 rows: the starting techs, gold and culture, which the scripts rely on (granted by `research::add_tech_silently`, which 1c-06's city-state catch-up shares; era techs, `Starting tech`, an AI seat's `aiFreeTechs`, `Starts with [tech]` read from the nation's, the known techs', the era's and the global uniques with their conditionals, and the era's stocks scaled by speed), so package 1b-07 need not. A generated map and a document short of starts were refused `NotPorted` until 1b-04 and 1c-09 (`maps.prepare`) ported them. Nations: the seats' choices, then `Purpose::NationShuffle` keyed `[0]` over the unchosen majors (popped from the end, as Python did) and `[1]` over the city-states. Colours follow `unique_colors` (`colors_clash`, redmean under 60). Relations need no stage work: `Diplomacy::new` holds every pair. `begin` begins player 0's turn, then emits `game_start`; `Game::new` returns the game settled, with that batch.
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

**As built in 1c-09** (§6.12, §6.14, §8.1, §9.3, §9.6):
- **Files.** Engine: `game/setup.rs` (the stage `prepare_map`), `game/turn/drive.rs` (rewritten), `game/meta.rs` (new), `api/game.rs` (`debug`, `DebugAction`), `api/testops.rs` (`debug`, `drive`, `set_difficulty`), `state/chronicle.rs` (`DriveMark` in `HostHeads`). Python: `citar/engine/testops.py` (the same three operations), and `EngineGame.test_ops`, whose `reload` carries the Python drive's mark. Testkit: `tests/engine/{drive, setup}.rs`, `src/golden/newgame.rs`, `golden/{newgame, turns}.json`; `agents::converse` is gone. Scripts: `setup_fills_missing_starts`, `setup_fills_city_state_sites`, `setup_ruins_on_editor_map`, `drive_stops`, `drive_awaiting_reply`, `drive_hybrid_diplomat`, `drive_left_to_the_model`, `controllers_set_difficulty`, `debug_shortcuts` and `meta_name_notes_thoughts`.
- **Setup is complete.** `SETUP` has no pending row: `inspect` `pending` lists no `setup_stage`, and `xtask/check.toml` lists 1c-09 as done, so a `Pending("1c-09")` fails the check (gate 1). `read_map` reads the document and its continents; the new `Draft` stage `prepare_map` (`maps.prepare`, `maps.py:333-345`) takes the document's starts in seat order and fills the rest with `mapgen::fill_starts_on` (gap 7, shrinking to 2), refusing a map with no room for every civilization as Python did ("This map has room for only 2 civilizations (asked for 3)."); takes its city-state sites at least 3 from every start and fills the rest (gap 4, 6 from the starts), a city-state with no site being left out; and, for settings that want ruins on a map with none, spreads them with `mapgen::ruins_on` from `Purpose::MapPrepare` keyed `[0]` (`map-prepare-draws-its-own-stream`: as many as Python's, elsewhere). A generated map has all of these, and the stage passes it by. `make_players` refuses a game of more than 64 players, city-states that found a site and the barbarians counted, before the state is made ("A game holds at most 64 players, city-states and barbarians included; these settings make 65.", `games-hold-at-most-64-players`): 24 civilizations asking for 47 city-states on a gargantuan map, which has sites for 40 of them, with the barbarians. The Rust runner plays `start = "full"` scripts.
- **The drive** (`game::turn::drive`, §6.12). Each step of `Game::drive` first puts every open negotiation that waits on a driven seat to its driver (`SeatDriver::respond`), whoever's turn it is, as Python's `resolve_negotiations` and the session's responders did: `driving` is set around it, the driver works on a copy of the seat's memory as in a turn, and a negotiation is asked once in a drive for each entry it has, so one a driver leaves is not asked again until it moves or the host drives again (the call keeps, by negotiation, the entries and the seat it last asked about and whether the driver deferred, in a `BTreeMap` pruned of closed negotiations at each step, so a call that plays a whole game looks each up in the logarithm of the open chats); the rounds end on their own and are bounded at 64. Then: a seat with no driver stops the drive (`External`); a driven seat whose driver has not played this turn plays (`play_turn`), or, once `DriveOptions::seat_limit` driven turns have ended in this call, the drive returns `SeatLimit` first; a hybrid seat whose driver has played stops once (`HybridDiplomat`) so the host runs its model's diplomacy on the seat's turn, and the next drive ends the turn (or the host does); a seat whose driver has played and that is in a negotiation waiting on the host stops (`AwaitingReply { pid, nids }`, rule T3): waiting on another seat with no driver, or on a driven seat, itself included, whose driver returned `DriverOutcome::Deferred` for it and that has not moved since (a hybrid seat's bot leaving the question to its model, in or out of that seat's turn); the host has each answered by whoever plays the seat it waits on, closes it when its wait runs out, or has the bot decide by driving with a driver that answers (the next drive asks it again); otherwise the turn ends. A driver that leaves a chat with `Done` has answered: the chat expires with its opener's turn, as in Python's headless loop. `GameOver` as before. A stop inside a turn is a `DriveMark { turn, player, diplomat }` in `HostHeads.drive`: saved (`#[serde(default)]`, so older saves load), never digested, and cleared when a turn begins or ends, so the next drive, even on a game loaded from a save taken there, goes on rather than playing the seat again, and no round's digest depends on who drove. `Stop` is no longer `Copy` (`AwaitingReply` holds a `SmallVec`); `DriverOutcome` gains `Deferred` (from `respond`; from `play_turn` it counts as `Done`); `DriveOptions::default().with_seat_limit(n)` (0 for none); `Drivers::drives(p)`.
- **Host commands and tools.** `Game::debug(DebugAction)` ports `engine_api.debug`: `MeetAll` (every living major meets every other, with what a first contact brings), `Reveal` (the whole map explored through `reveal_tiles`, as a map trade reveals) and `Gold` (500 to every living major). `force_turn`, `meet`, `set_controller` and `set_difficulty` (both `Change::Seat`) were already ported (1b-02, 1b-03). The last three tools are in `game::meta`, each `any_time`: `set_civ_name` (`tools.py:900-925`: the name cleaned to 48 characters, unique whatever its case among all players, refused when nothing changes; `civ_renamed` with the old name as a mention), `write_notes` (`replace` or `append`, the last 8,000 characters kept) and `log_thought` (a thought of 4,000 characters at most, host activity as `add_thought` keeps it). Every tool of `tools.py` now has its typed action.
- **Test operations** (both engines): `debug`, `set_difficulty` (the host's, `ok` false for a name that is no level), and `drive`, the host's drive with a test driver at the seats named, which does nothing with its turns and answers what waits on it with `answer`, or defers it for the seats under `defer`; an `answer` present that is none of `reject`, `accept`, `reply` and `none` is refused on both. `citar/engine/testops.py` plays the same machine (Python had no drive), keeping its stop-inside-a-turn mark on the game object, which `EngineGame.test_ops` carries across a `reload` as Rust's save does, and which its `force_turn` forgets when it begins a turn anew, as Rust's `begin_turn` does.
- **RandomAgent** leaves the chats it opens to the drive: `converse`, which answered for the other side at once, is gone, and so is the withdrawal at the end of its turn; another agent answers them before the opener's turn ends (`RandomAgent::respond`, from the same streams), and a chat with a seat the host plays stops the drive (`random_agents_that_leave_chats_to_the_drive_get_their_answers`).
- **Kitchen sink.** No extra type is staged `setup` or for the drive: the `setup` types (`StartingTech`, `StartsWithTech`, `DisablesReligion`) are the shipped ruleset's, read since 1b-01 and 1b-03, and `upon turn start` and `upon turn end` were 1b-08's. Deferred: none.
- **Golden sets.** `newgame.json` (gate 2): ten new games on generated maps, duel to huge, every type and edge mode, a person's seat and the bot's, and two on the arena that `maps.prepare` fills in (six civilizations for its five starts, which leave room for the one city-state site of three asked for; two civilizations and three city-states, two of their sites chosen; ruins on both); each row is `[name, seed, [width, height], digest after Game::new, [majors, city-states, barbarians], units, camps, explored tiles, events]`, and the check also asks every invariant and a cold rebuild of the caches. With no stage pending, `turns.json` is blessed too. Compared on Windows x64 and Linux x64 (WSL) here; the other three targets are the determinism workflow's.
- **Gates.** 1: see above. 2: `newgame.json`. 3: `tests/engine/drive.rs`: every seat driven, the host's seat in the middle, none driven; the seat limit (one call per seat with a limit of one, the same chain and digest whatever the limit); a hybrid seat's stop once a turn; `AwaitingReply` held while nothing moves, the driver answering back, and ended by the host's close; a chat a hybrid seat's driver defers holds the other seat's turn and, in its own turn, the hybrid seat's, until the model answers or the bot decides, while one a driver leaves with `Done` expires with the turn; a negotiation the host's seat opens answered on the next drive, and one a driver leaves asked once per drive; a driver's memory, written in a turn and in an answer, digested and kept through a save and a load, and a stop inside a turn kept too, so the loaded game does not play the seat again. 4: `tests/engine/setup.rs`: a new game on each of the 30 lobby sizes and types keeps every invariant and agrees with a cold rebuild of its caches, with a start and starting units for every civilization and city-state; 65 players refused and 64 set up; a map with room for fewer civilizations than asked refused, and city-states without a site left out; ruins kept, not added to; the golden set's arena games as it has them. 5: the ten scripts pass on Python and Rust (`drive_left_to_the_model`, and a `reload` and a turn forced away and back in `drive_hybrid_diplomat`, added in the fix round, and `refresh_visibility` run in `debug_shortcuts`).

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

As built in 1e-04: `ErrCode` has no `NotPorted`. Nothing returned it once 1d-03 answered the last queries, so 1e-04 removed it, and `cargo xtask check` now refuses the marker, the `not_ported` helper and any path to the variant (§3.4 rule 4).

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

**As built in 1d-01** (§8.1, §8.3, §8.5, §9.2, §9.5):
- **Files.** Engine: `api/tools/{registry, execute, query_tools}.rs` (new), `api/tools/{args, normalize, mod}.rs`, `game/lookup.rs` (new), `game/error.rs` (`text_rule_broken`, `MAX_REFUSAL_CHARS`), `base/text.rs` (`echo`, `echo_bare`), `base/py.rs` (`repr_echo`), and the refusals below. Refcheck: `answer/tool_errors.rs`. Testkit: `src/calls.rs` (new), `src/agents.rs` (`play` checks every refusal), `src/script/runner.rs` (tools go through `execute`), `tests/engine/{execute (new), tools, whole_game}.rs`. Shared: `tests/rules/tool_list.json` and its recorder `scripts/refcheck/tool_list.py`, whose `--check` `tests/test_rule_scripts.py` runs.
- **The registry** (`api::tools::registry`). `TOOLS: [ToolSpec; 61]` in `tools.py`'s order (the 21 queries, then the 40 actions), each `ToolSpec { args: ToolArgs, description, op: Op, any_time, category: Category }`; `Op::Query(Query)` names one of the 21 queries, `Op::Action` reads the arguments into the `Action` the tool's name tags, and `kind()` is the op's. `tool(name)` looks a name up through an index sorted once; `kind(name)`, `schemas(Option<ToolKind>)` (`tool_list`) and `schemas_json()` (built once, in a `OnceLock`) are the facade's `tool_list` and `tool_kind`. The argument specs moved into this one table: `ToolArgs { tool, params: &[Param], required }`, where `Param { name, json: SchemaType, description, choices }` renders the schema (`type`, `items`, `enum`, `description`, in Python's key order) and gives the coercion (`Param::ty()`); `args::TOOLS` is gone, and `args::spec(name)` reads the registry, so `normalize` knows the query tools too. The table was generated from Python's `tool_list()` and is checked against it.
- **Descriptions.** Ported as they were, but one: `city_state_action`'s told models a unit next to a city-state's territory could be gifted, which the rule refused (`gift-unit-described-as-ruled`). Gate 2 (`the_schemas_equal_python_s_tool_list`) compares `schemas_json()` with `tests/rules/tool_list.json` tool by tool and field by field, in order, each field as JSON text so that the order of the keys inside it counts too (`Value`'s equality ignores it); a field that differs must be listed with its intended id, and a listed one must differ.
- **`Game::execute(pid, tool, &Value) -> Result<Executed, ActionError>`** (`api::tools::execute`), `Executed { kind, result, events }`: a poisoned game refuses; an unknown tool is refused first (`Unknown tool '...'.`, the name quoted through `echo`, as `normalize` quotes it too); an action then goes through `Game::guard` (invalid player, eliminated, game over, not your turn), then `normalize_with`, then `Action::deserialize` of the arguments tagged with the tool's name, then `Game::act`, which logs it (execute does not log again); a query checks only that the caller is a major civilization of the game, as Python did, coerces its arguments and answers, with no events. `Game::execute_query(&self, ..)` answers a query under a shared borrow and refuses an action. An argument an action's typed field cannot take (a number for `unit_order`'s order, `promote_unit`'s promotion, `build_improvement`'s improvement, `unit_action`'s action; a non-text topic for `get_rules`) is refused as `Parameter 'order' must be a string.`, naming the first parameter whose value does not fit its schema type (`SchemaType::fits`, `what`), where Python raised or read the number as a name (`tool-arguments-of-the-wrong-type-refused`). The Rust script runner now calls `execute`, so scripts meet the checks in Python's order.
- **The query tools** (`api::tools::query_tools`). Answered: `read_notes` (the notebook or `(empty)`), `get_events` (`events_for(Some(pid), since, 200)`, the last 40, as `{id, turn, type, text}`) and `preview_attack` (`own_unit`, `tile_at`, then `combat::resolve::preview`). The other 18 check what they name first (`get_tile`'s tile, `get_unit`'s unit, `get_city`'s city, `get_map`'s centre if both coordinates are given, `get_rules`' topic against its 19 topics, lower-cased and stripped) and then refuse with `ErrCode::NotPorted`, "`get_empire` is not ported to the new engine yet; the other tools work.", through two markers, `not_ported("api::views")` (1d-02: 15 queries) and `not_ported("api::briefing")` (1d-03: `get_briefing`, `get_map`, `get_rules`). `cargo xtask check` counts 4 NotPorted.
- **Helpers** (`game::lookup`): `tile_at`, `own_unit` and `own_city` (`tools._idx`, `_own_unit`, `_own_city`) replace the two copies that `units::actions` and `cities::citizens` had; every action and query goes through them. A refusal that lists the caller's units or cities keeps within 600 characters: `with_list(head, entries, tail, tool)` keeps the entries that fit and ends `and N more (get_units lists them all)` (`refusal-lists-capped`; 18 corpus states, none committed, so the entry names its cases).
- **Refusal text (P5).** `game::error::text_rule_broken(&str)` (re-exported by `api`) is the rule of §8.5: not empty, at most `MAX_REFUSAL_CHARS` (600) characters, ending in `.`, `?` or `)`, no `Some(`, `Idx(`, `::` or the word `None`. What was fixed: the lists of promotions and beliefs that ended without a full stop, and could run long, now use `with_list` (`refusals-end-as-sentences`); a resource or tech deal item that named nothing said `'None'` (`deal-items-name-what-they-trade`); and every refusal that quotes a caller's own words (an unknown tool, item, tech, policy, belief, improvement, spy, city, specialist, recipient, status, topic or unit action) quotes at most 60 characters of them and `...` (`base::text::echo`, `refusals-quote-at-most-60-characters`), so no input makes a refusal run long; a name Python gave without quotation marks (a specialist the city has no slots for) is put in them once cut (`echo_bare`), so its `...` does not run into the full stop. A refusal that quotes a JSON value (a deal item, a spy's city id that is not a number) uses `base::py::repr_echo`: Python's `repr` with each text in it, keys included, cut as `echo` cuts one, and, once the quote reaches 100 characters, the rest of each list or object given as `...` and the brackets closed, so the quote stays within about 340 characters and its quotes and brackets balanced (cutting the whole `repr` left them open, and put `...` against the full stop). Numbers in refusals are written as Python wrote them: integers (gold and culture already went through `trunc_i64`), but for a paradrop's range, which Python wrote as its unique's float (`Paradrops reach at most 9.0 tiles.`, through `PyFloat`). Gate 3 has three parts: `every_refusal_reads_as_a_sentence_and_changes_nothing` calls every tool with ten argument sets (`citar_testkit::calls::battery`: the caller's units, cities, tiles, spies and chats, the other players and the ruleset's names, mixed with ids no game hands out, tiles off the map, unknown and 720-character names, numbers as text, text as numbers, missing arguments) on each committed fixture, by the player whose turn it is, by another and by one the game lacks, about 22,000 calls with about 490 distinct refusals, each checked against the rule and for leaving the revision and the events as they were; `a_name_too_long_is_quoted_back_in_part` sends a 1,800-character name in every parameter of every tool (as the name, in a list, as an object's key and value, in each place of a deal item, and 40 lists deep), and checks that each refusal also reads well: no cut quote runs into the full stop (`....`) and no bracket is left open; and `agents::play` checks every refusal a `RandomAgent` meets, in every whole-game test.
- **Refcheck** (`answer::tool_errors`, gate 1). Each recorded call runs through `execute` on a copy of the loaded game, copied again after a call that succeeds (Python reloaded after one); a refused call must leave the copy's revision and digest as they were (P2), or the answer fails naming the call, rather than showing differences on the calls after it. This is what §9.2's "clones the game per call" became: a clone per success and a digest per call. Enforced: the 12 committed fixtures are clean with 12 explained differences (the promotion list's full stop), and the corpus's 250 states with 268 (the promotion lists, and the 18 long lists), each call's text otherwise equal to Python's, the three United Nations votes that succeed included. `ratchet.json` has `tool_errors: 0`.
- **Gate 4** (`tests/engine/execute.rs`): the caller is refused before its arguments are read (`move_unit` with nothing, out of turn, is "It is not your turn (it is ...'s turn)."), and the tool before the caller; missing parameters are reported before any is coerced (`{"unit_id": "abc"}` is "Missing required parameter(s): x, y."); unknown keys are dropped and the log records the arguments taken; a query answers any major at any time and changes nothing; every refusal leaves the digest, the events and the revision as they were.
- **Kitchen sink.** No extra unique type is staged for the registry: the tools read the ones their systems read. Deferred: none.

**As built in 1d-02** (§4.11, §8.1, §8.4, §9.2, §9.3, §10):
- **Files.** Engine: `api/views/{mod, units, cities, tiles, empire, players, info, events, alerts, client, replay, tests}.rs` (new), `api/tools/query_tools.rs` (the fifteen view queries answered), `api/inspect.rs` (`view`), `base/num.rs` (`py_sum`, rounding in integers), `game/core.rs` (`testing::state_of`). Refcheck: `answer/views.rs`, the `views` compare spec, `tests/query_tools.rs`. Testkit: `tests/engine/views.rs` (new), `tests/engine/scenario.rs`, `src/agents.rs` (P8's noise asks a player's view and a spectator's). Bench: `benches/views.rs`. Shared: `refcheck/query_tools.json.gz` and its recorder `scripts/refcheck/query_tools.py`, the script `views_what_a_player_sees`. Python: `citar/engine/inspect.py` (`view`).
- **Host methods.** `Game::view_json(viewer, event_limit) -> Vec<u8>` is §8.1's `view`: the name `Game::view` was the unique evaluator's view (`EvalView`) from 1b-01, called in several hundred places. `Game::client_view(viewer, event_limit) -> ClientView` is the same view typed, before it is written. `empire_summary(pid) -> Option<EmpireSummary>` (`to_json()` is the facade's dict: `get_empire`'s keys, `cities`, `at_war_with` by player id, `notes`), `standing(pid)` and `standings() -> Vec<Standing>`, `path_preview(pid, unit, x, y) -> PathPreview` (`{path: null}` for a unit not the caller's, a tile off the map or no path within 40 turns), `replay_data(ReplayFormat) -> Vec<u8>` (and `replay_json`), and `event_json(ev, viewer)`, one event as the viewer may see it in Python's dict. `negotiation_view` was 1c-05's.
- **The builders** (`api::views`), each `&Game` and a viewer (`None` a spectator), returning Python's shapes: `units::{unit_info, unit_view}` (with `detail`, `get_unit`'s actions, city sites, build options, water sites, upgrade, promotions, reach and attack targets), `cities::{city_info, city_view}` (with `detail`, the breakdowns, what the city can build and buy, what only faith buys, its tiles, the tiles for sale and its followers), `tiles::tile_info` and `tiles::Known` (a tile as the viewer knows it: in sight, as last seen, or explored and never watched leave sight), `empire::{empire_info, strategic_resources, luxury_resources, happiness_json}`, `players::{players_overview, trade_options, diplomacy_info, city_states_info}`, `info::{tech_tree, policies_info, religion_info, great_people_info, victory_info, espionage_view}`, `events::{events_json, event_json}` and `alerts::{alert_items, bombard_targets}`. The alerts are `briefing.alert_items` (`briefing.py:168-316`), ported here because the client view carries them: an `Alert` has the people's `text`, the models' `llm` (which 1d-03's briefing lists), a tile and the ids it concerns. The query tools answer from these; `get_tech_tree` filters as Python did (`all`, a status, `available` for none; a filter that is no status leaves none).
- **Following Python where it chose among equals.** A city's tiles, the tiles for sale (sorted by price, then kept to 24), a unit's attack targets and a city's best bombard target come in Python's `within` order (`cities::borders::within_order`). A sum Python took with `sum()` (a city's happiness, a tile's defence) goes through `num::py_sum`, Neumaier's compensated sum of Python 3.12, since adding in turn rounds `-2.95` to `-2.9` where Python gave `-3.0`. Limits read as Python's `xs[-n:]` (`players::py_tail`): 0 keeps everything, a negative `n` drops the first `-n`.
- **Events** (§8.4). `events_json` walks the chronicle as `events_for` does (the newest 200 the viewer hears, then the last `event_limit`) and writes each as `emit` wrote it: `players` (the audience, or null), `idx` (null once scrubbed), `data` with rule objects by name (an era by index, a religion by its key), `refs` in code points (the chronicle keeps bytes), `x` and `y` where there is a tile. A player field the scrubbing cleared is `null`, as Python set it; a `city_sacked` without a building burned has `building: null`, Python's `building=None`. The UN tally is keyed by name, an unmet candidate named `Unknown Civilization` or `Unknown City-State`, from the second of a kind with its number (`game.py:980-988`). Gate 2 is `api::views::tests`: possessives and non-ASCII names, the audience and coordinates, unmet city-states and capitals at a sentence start, the tally, the `[-n:]` limit and the sacked city, on a game of three civilizations and two city-states.
- **The client view** is written straight to bytes: the tiles as typed rows (their features named as they are written, never collected), units as `UnitView` and cities as `CityEntry` (seen, or remembered with `stale`), the rest as values. `NameMap` writes named pairs as an object in order. The bytes of a view and of the replay go through `views::to_py_json`, whose `PyJson` formatter writes each float as Python's `repr` (`2.0`, `1e+16`, `5e-05`, through `PyFloat`) where `serde_json` writes `1e16` and `5e-5`. Each number is an int or a float as Python's was: `views::PyNum` keeps the two kinds that varied, a city's happiness (an int 0 for a city with no happiness, a city-state's, Python's sum of nothing) and the luxuries' line of an empire's happiness (a count times a whole amount, an int); a city's `food_stored` is always a float, where Python's was an int after some resets and a float otherwise, a history no state keeps (`city-food-stored-is-a-float`). A viewer the game lacks sees no tile, unit or city, and nothing panics.
- **Fixes** (each cited where it is made): `get_city_states` lists the quests the city-state gave the caller, where Python matched them on a key its list never had (`city-state-view-lists-its-quests`); `get_great_people` shows each kind's points, where Python read them under the pool's name (`great-people-view-shows-the-points`); the diplomacy and victory panels name only the UN candidates the caller knows (`un-results-name-only-known-candidates`); `empire_summary` lists wars by player id (`empire-summary-wars-by-id`); a city's food rounds as Rust's total is, where Python's carried float noise to a half or across zero on ten corpus states (`city-view-rounds-its-own-sums`); `get_city_states` names a city-state's ally, and `get_diplomacy` (so the client view too) a message's recipient, only to a caller that has met it, `unknown` otherwise, as the player list names an ally, where Python named them to anyone (`views-hide-unmet-allies-and-recipients`: six answers on the corpus). `marble-bonus-in-its-own-city` and `civilians-at-zero-health` now cover the views too.
- **Refcheck** (gate 1). `answer::views` reads `Game::view_json` back; the spec compares tiles by index and as sets what are sets (features, worked and locked tiles, promotions, an event's audience, a message's recipients, the alerts). Enforced: clean on the 12 committed states and on the corpus's 250; `ratchet.json` has `views: 0`. Beyond the group, `crates/citar-refcheck/tests/query_tools.rs` asks what `scripts/refcheck/query_tools.py` recorded (the fifteen view queries for two civilizations of each state, a unit's and a city's detail for up to twelve of each, tiles, a spectator's view, `empire_summary`, `standings`, `path_preview`): 1,191 answers on the committed states, 28,035 on the corpus with `CITAR_QUERY_TOOLS`, every difference explained by an intended id, and no state's digest or revision moved. An explanation may also say which of the differences at its place it explains: `improvements-over-removable-features` explains only a build option Rust offers and Python did not that removes a feature first, so an option Rust lost stays unexplained. Refcheck's comparator takes 3 and 3.0 as equal, so a second test compares the answers again with each whole float marked, and every number must be of Python's kind but for its own list (`food_stored`). `tests/test_rule_scripts.py` re-records it (`--check`, with `PYTHONHASHSEED=0`) and fails if the Python engine's answers moved, as it does for `tool_list.json`.
- **Replay** (§4.11, gate 3). `Full` decodes the chronicle's frames (a frame that does not decode drops the rest until the next keyframe) and writes each in Python's shape, renumbered into the current ruleset's `improvement_ids` and `feature_ids` by name when its palette differs; `unit_ids` is added for a client that decodes `Delta`, whose frames are the records as stored, base64. The test plays a hundred arena turns with a reload: each of `Full`'s frames is the map rebuilt by the test from the state as its round recorded the frame (`records::on_frame_for_test`, feature `test-ops`, calls it with the game at R3, before `FullFrame::capture`), and the frame captured; the last is the state as the game ends; and `Delta`'s records decode back to the captured frames. Unit tests in `api::views::replay` renumber a frame recorded under other palettes, reordered and shorter, name by name, and drop the frames after one that does not decode, cut short or lost, until the next keyframe.
- **Performance** (gate 4, report-only, `benches/views.rs`). A player's view of the small t280 fixture (no corpus state is at turn 300) about 0.6 ms against 1.5 ms; a spectator's of `large-pangaea-normal-s1016/t280` about 2.3 ms against 20 ms; of the synthetic gargantuan state (2,500 units, 400 cities, 16,000 tiles, 2.6 MB of JSON) about 10 to 11 ms against the 12 ms target. Two changes got there: the typed views, and `num::round_ndigits` in integers for up to 22 decimals and a result under 2^53 units (checked against the formatted rounding on a million values), which every rule's rounding shares.
- **Kitchen sink.** No extra unique type is staged for the views: they read the types their systems read. Deferred: none.
- **Fix round.** The review found the ally of `get_city_states` and the recipients of `get_diplomacy`'s messages named to callers that had not met them, the module doc promising numbers as Python wrote them while three kinds differed unseen, an explanation of the query tools wide enough to hide a lost build option, and gate 3 checking only its last frame against the state. All fixed as above, with the ally and the recipients checked on both engines in `views_what_a_player_sees`.
- **Counts** at this package's head (after its fix round): nextest 1067 passed, 1 skipped (`--workspace --all-features`, the corpus on); Python 600 OK (2 skipped); `cargo xtask check` 2 NotPorted (the briefing's), 0 Pending; `views` enforced and clean on the 12 committed states and the corpus's 250, every ratchet group at 0; both query-tool tests pass on the committed and the corpus recordings; the 15 golden sets unchanged; `cargo doc` with `-D warnings` clean. The views bench after the fix round: 0.63 ms, 2.4 ms and 10.5 ms.

**As built in 1d-03** (§3.3, §8.1, §9.2, §9.3, §10):
- **Files.** Engine: `api/briefing/{mod, map, orders}.rs` (new), `api/text.rs` (new), `api/views/rules.rs` (new), `api/scenario/editor.rs` (new, a module of `api::scenario`), `api/maps.rs`, `api/tools/{query_tools, execute}.rs`, `api/inspect.rs`, `rules/{mod, client, load}.rs` (the client JSON kept as a value too, and a nation's row with its cities). Refcheck: `answer/briefing.rs`, the `briefing` compare spec, `tests/query_tools.rs`. Testkit: `tests/engine/maps.rs` (new), `tests/engine/scenario.rs`. Bench: `benches/briefing.rs`. Shared: `scripts/refcheck/query_tools.py` and `refcheck/query_tools.json.gz` (now the briefing's tools, the maps and the scenario editor too), seven scripts (`briefing_alerts`, `briefing_turn_progress`, `briefing_events`, `briefing_map`, `briefing_wrapping_map`, `briefing_diplomacy`, `rules_lookup`) and the map `tests/rules/maps/arena_wrap.json`, the arena wrapping both ways. Python: `citar/engine/inspect.py` (`briefing`).
- **Host methods and free functions.** `Game::briefing(pid) -> String` and `Game::turn_progress(pid) -> String` (§8.1), also `api::briefing::{briefing, turn_progress, alerts, ascii_map, anchor}`; `api::text::{MAP_LEGEND, RULES_OVERVIEW, RULES_COMBAT}`; `api::views::rules::rules_lookup(g, topic, name)`; `api::maps::{validate_map(rules, &doc) -> (clean, warnings), blank_map(rules, w, h, terrain, name), map_summary(rules, &doc), generate_map}` and `Game::export_map(name)` (the facade's `export_map`; §8.1's free `export_map` is a method, since it reads a game); `api::scenario::{overview, default_seats, normalize_seats, scenario_summary, SEAT_TYPES}` and `Game::scenario_overview()`. `map_summary` takes the ruleset, to tell water by rule. A player the game lacks gets an empty briefing and map rather than Python's exception.
- **The briefing** ports `briefing.py` line for line: the head (turn, wraps, treasury and yields, happiness, score, policies, strategic resources, religion), the alerts (`api::views::alerts`' models' text), cities, units (civilians first), the options of the units that need orders and of the idle cities, the techs to choose from, the local map with what it shows and the points of interest, the events since the reader's last turn (`events_for`, scrubbed), diplomacy, the to-do list and the notebook's last 3,000 characters. Numbers go through `PyFloat` (`{:+.0}`, `{:.0}`) and `PyRound` as Python formatted them; Python's `within` order is `within_order`'s. The terrain and feature characters are looked up by name once per map (Python's two tables; a mod's terrain is `?`). The map's entries are kept apart from its rows (`ascii_map_parts`), so the briefing's local map needs no re-parsing.
- **Fixes** (each cited where it is made): the briefing names no player the reader has not met (whose turn it is, a city-state's ally, the owner of a resource on a tile explored long ago), as the events name them, `Unknown Civilization` or `Unknown City-State` (`briefing-names-only-known-players`, 139 heads, 6 city-state lists and 1 map on the corpus); the points of interest leave out the cities of players the reader has not met, which it cannot have seen (`briefing-lists-only-cities-it-could-have-seen`, 8 corpus states); the ruins and camps a unit's options call nearby are those the reader knows of, as its map shows them, where Python named camps raised in its fog and ruins on tiles it had only had revealed (`briefing-nearby-reads-what-it-knows`, 3 committed states and 17 of the corpus's, one of them the same); the civilizations met are listed by player id and the policies branch by branch, a city's specialists and a religion's beliefs in the ruleset's order, where Python kept the order of a history no state keeps (`lists-in-rule-order`, in `tests/rules/intended.toml`, shown by `briefing_diplomacy`; the refcheck group compares those lists as sets). The scenario editor's overview gives a city-state's influence with every civilization (`scenario-overview-lists-every-influence`), and a map's summary counts water by rule (`map-summary-reads-water-by-rule`). The briefing inherits the views' fixes where it shows the same numbers: `marble-bonus-in-its-own-city` (a city's production and turns, an idle city's wonders), `city-view-rounds-its-own-sums` (a city's food and growth, the starving alert) and `civilians-at-zero-health`, each of which now lists the briefing's paths.
- **`get_rules`** reads the client JSON's tables (Python's `to_client` rows are its `_public` rows): a topic whole, units, buildings, techs and nations in brief, one entry by a name resolved loosely (a city-state type exactly), a tech with what it unlocks, a nation with its city names (`Ruleset::nation_source`), a city-state type as its bonuses' texts; `deal_items` from `DealItemKind` with Python's help texts; a name that is not text is read as Python's `str()` of it; a refusal quotes the caller's name through `echo` or `repr_echo`. **`get_map`** checks a centre given before drawing, centres on the capital otherwise, reads a radius of 0 as 8 and holds it to 2..20, and puts the legend first when asked.
- **Maps and scenarios.** `validate_map` is `mapgen::document::read` written back as the editor's document (id from the name's slug, name at most 80 characters, description 2,000, `created`, `modified`, `author`, `recommended` kept); `blank_map` refuses a terrain that is no base terrain by its exact name before the size, as the facade did; `export_map` writes each tile's row (`tile_row`: no city centre, no great improvement) and a start at each player's capital, else where it began. `normalize_seats` checks a seat's handicap and automatic decisions with `SeatOverrides::parse`, the lobby's check, and keeps `auto` as sent; `scenario_summary` reads a scenario whose `state` is the engine's own save (§4.9), from its JSON: a map size, a turn or a player's kind (read as `PlayerKind`) it cannot read refuses the scenario rather than listing it empty.
- **`inspect`**: `briefing` (`player`) gives `text`, `progress` and `alerts` on both engines; `QUERIES` lost its `Answered` column, and `pending` lists no query.
- **Refcheck** (gate 1). `answer::briefing` reads Python's recorded text and Rust's into the same structure (`structure`): the head's lines (the turn's by field: turn, year, name, player, nation, era and whose turn it is), the empire's line (luxuries, score, policies) and the religion's by field, the alerts, each city's line by field (tags, specialists, food and growth, each yield a number, what it builds with its progress, cost and turns), each unit's (idle, moves, orders, health), a unit's options (its orders, and what is nearby apart), an idle city's options, the techs, the map's rows, what the window shows (each entry's owner apart, a unit's health, a city's size, a resource's improvement), the points of interest, the events, the diplomacy's lines (a city-state by field, its ally apart), the to-do line and the notebook; so a difference is reported at its field and an explanation names it, and one that explains a name explains nothing else on its line. Lists whose order is no rule compare as sets (the policies, beliefs, specialists, civilizations met), and so do the alerts and the points of interest. Enforced: clean on the 12 committed states and on the corpus's 250; `ratchet.json` has `briefing: 0`, so every group is now enforced. `query_tools.py` records `get_map` (around the capital, with the legend, around a unit, the widest, one coordinate, off the map), `get_rules` for every topic and 21 names on the first state, the exported map (every 25th tile), its summary and warnings and whether it reads back, and the editor's overview, default seats and two seat lists; `query_tools.rs` compares them on the committed states and the corpus, with explanations for unknown owners and zero health in the map's text, a great improvement (the Citadel) no map carries, checked against the ruleset, and every influence listed.
- **Scripts** (gate 2): seven, each passing on both engines, the Rust-only steps marked `intended`.
- **Map API** (gate 3, `tests/engine/maps.rs`): a document's problems are fixed and reported, and what cannot be a map is refused; blank maps and generated maps of three types check clean, their tiles unchanged; a game's terrain after 40 rounds of random agents, exported, reads back as written and starts a new game on the same tiles, which exports them again, its civilizations where the old capitals stood. A script map's wrapping copy (`arena_wrap.json`) is checked there and in `tests/test_rule_scripts.py` to differ from its map only in its id, name, description and wrapping. `tests/engine/scenario.rs` reads a scenario built from a real save through `scenario_summary`, and its refusals.
- **Host inputs** (`tests/engine/briefing.rs`): `ascii_map` draws the viewer's own window for a centre off the map (as far as `i32` goes) and reads one past a wrapping edge across it, holds any radius to 2..20, and a city whose saved food store is 1e19 still briefs; the camps near a unit are those its civilization knows of, raised in the fog, seen, out of sight and cleared.
- **Performance** (gate 4, report-only, `benches/briefing.rs`): the briefing on `small-continents-normal-s1025/t280` about 0.43 ms against 1 ms, the turn's progress about 16 us, a large map's briefing about 0.47 ms.
- **Kitchen sink.** No extra unique type is staged for the briefing, maps or scenarios: they read the types their systems read. Deferred: none.
- **Fix round.** The review found `ascii_map` overflowing on a centre far off the map (and drawing an empty window with a nonsense header for one merely off it), a city's growth overflowing on a vast saved food store, the unit options' nearby camps read from the true tile where the map reads what the reader knows, the explanation of unknown names wide enough to hide any other change on a head line, a city-state entry or a window entry (and the query tools' Citadel explanation any improvement Rust dropped), `scenario_summary` untested and silent on a save it could not read, a stale comment on the P8 noise, and nothing keeping `arena_wrap.json` the arena's tiles. All fixed as above; the centre, the radius and the food are saturated or held, and the growth's and the messages' turn arithmetic saturates. The bench after the fix round, on a laptop busy with other work: the briefing about 0.67 ms, the turn's progress about 27 us, whose code the round did not touch (16 us before), so the load, not the round, is most of the difference.
- **Counts** at this package's head (after its fix round): nextest 1084 passed, 1 skipped (`--workspace --all-features`, the corpus on); doctests pass; Python 608 OK (2 skipped); `cargo xtask check` 245 files, 0 NotPorted, 0 Pending; refcheck: all 14 groups enforced, every ratchet group at 0, clean on the 12 committed states and the corpus's 250 (the same two stale entries, `building-conditionals-read-a-filter` and `no-civ-adopted-counts-beliefs`); both query-tool tests pass on the committed and the corpus recordings; the 15 golden sets unchanged; clippy clean for the workspace and the engine in each feature set; `cargo doc` with `-D warnings` clean.

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
- **Runners.** Both read the same TOML and share the language: step kinds `op`, `ops` (a list applied as one `apply_ops`, added for the all-or-nothing script), `tool`, `check`, `new_game` (a game from the script's settings with keys replaced, or its refusal: the seat-setting refusals), `set` and `repeat`, with `as` (`op`, `ops`, `tool` and `check` only), `error` (`op`, `ops`, `tool` and `new_game` only; elsewhere the runners refuse it, so no check passes unread), `must_fail`, `intended` (cited ids must be in `refcheck/intended.toml` or the new `tests/rules/intended.toml`, for differences no refcheck group shows) and `coerce`. Values starting with `=` are expressions, also in matcher values. The runners' settings are `seed = 1` and two players, then the bare prelude, then the script's `config`, then `nation = "BenchmarkCiv"` for any seat that names none, so no nation's ability leaks into a script and seats are named by number. The Rust runner turns every check on (`DebugOptions::ALL`) and fails a step that leaves a violation. Until `Game::new` (1b-03), `script::setup::new_game` builds bare games from the settings on the inline map: seats and their checks, nations (the first free ones where Python drew at random), city-states and the barbarians, the map's own starts, starting techs, gold and culture, continents; no units, camps or first turn. The Rust runner refused `start = "full"` until 1c-09, which plays it. Python's `Game.new` also emits `turn_start` and `game_start` and its units explore around the starts before the prelude clears them, so scripts count events by type and never assume nothing is explored.
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

**As built in 1e-01** (§9.4, §9.5; Appendix B):
- **Files.** Engine: `game/derive/oracle.rs` and `game/seeded.rs` (new); `game/{invariants, debug, core}.rs`, `game/turn/settle.rs`; each memo family's `verify` (`derive/{civ, stats, buildable, religion, danger, jobs}.rs`, `path/memo.rs`, `vis/effects.rs`, `cities/citizens.rs`); `path/astar.rs` (`PathCache::at`); the seeded bugs' sites (`game/action.rs`, `vis/effects.rs`, `cities/founding.rs`, `api/tools/execute.rs`, `derive/{rev, stats}.rs`); `Game::assign_every_city_for_test` (`cities/citizens.rs`, `test-ops`). Testkit: `src/{checks, spec, stability, chaos, fuzz}.rs`, `src/bin/chaos.rs`, `src/games.rs` (`kitchen_sink_game`), `tests/props/{games, seeded, pure}.rs` with `tests/props/games.proptest-regressions`, `tests/engine/{chaos, stability}.rs`. `crates/citar-engine/fuzz/` (new, outside the workspace). Bench: `src/lib.rs` (`unchecked`) and every suite's game. CI: `rust.yml`. `.cargo/config.toml`: `cargo chaos`.
- **The checks under their cfg.** `invariants::check`, `Game::check_invariants`, the cache oracle (`Game::verify_caches`, moved from `core.rs` into `derive::oracle`) and every memo family's `verify` are compiled under `cfg(any(test, debug_assertions, feature = "checks"))`: §9.4's decision, with `test` added so that the engine's own unit tests have them in any profile. A shipped release build keeps `DebugOptions`, `Code` and `Violation`, which settle's SETTLE-1 reports use, and compiles nothing that checks (`run_checks` is empty there); `rust.yml` lints that build (`cargo clippy -p citar-engine --release`), which no dev-profile lint reaches. The invariants of §9.4's table were already in place from their packages, with the two bounds 1c-02 left out (a unit's maximum moves, held to a sanity cap, and stacking); this package put them under the cfg and added no code.
- **Testkit's `checks` feature.** Testkit turns on the engine's `checks` through a feature of its own, on by default, so the golden binary, chaos and the tests have the checks in every profile, release included. Where the checks are compiled, `DebugOptions::default()` runs the invariants at every settle, so the workspace's dependency on testkit has no default features, and `cargo bench -p citar-bench` compiles no check. That alone does not hold for a command that also selects testkit (a plain `cargo bench` at the root), since cargo unifies the features of the packages it builds and turns testkit's default `checks` back on: so every game a benchmark times goes through `citar_bench::unchecked`, which sets `DebugOptions::OFF`, and the numbers are the same whichever way the benches run. Testkit asks the checks through `citar_testkit::checks::{invariants, caches}`; a build without them answers with one line saying so, never with an empty list that would pass for a clean game.
- **The oracle** (`derive::oracle`, scope 0). Its module documentation maps each memo of §6.5 to the check that covers it: `civ::verify` (the unique indexes, the supply, the local and follower indexes, the unit profiles, the era and owned tiles, against a cold game), `stats::verify` (tile yields for owners and every other viewer, city mods, base, parts and stats, happiness, civilization stats, upkeep, connectivity, the supply deficit), `vis::verify` (sight rebuilt from the state, each unit's sight against the index, the kept lines of sight, then the met sets and the natural wonders found against Python's rule), `buildable`, `jobs`, `danger`, `religion` (the grid of cities and the spread), `path::memo` (move costs, zones of control, movement profiles), the route net and the terrain floor against a cold look at the map, `Derived::verify` (the name index, the grid) and the citizen oracle. New: every path the path cache keeps at the current revision against a fresh search, and a key whose unit is no longer where it was asked. `Game::verify_caches` builds one cold game (`oracle::cold`, a clone of the state with every cache cold) and hands it to `civ`, `stats`, `buildable` and `religion`'s `verify`, so the state is cloned once per oracle run and each cold memo built once, whichever family asks first.
- **Seeded bugs** (`game::seeded`, gate 1). `SeededBug` has five: `MutatesBeforeRefusing` (`Game::act` takes a gold piece from a player whose `buy` was refused), `StaleVisibility` (the settle's sight update skips the units marked dirty, `vis::effects::Work::gather`), `WrongTouch` (`founding::write_name` touches `CORE`, not `NAME`, so the event name index keeps the old name), `QueryWrites` (after a query tool answers, `api::tools::execute` validates the asker's happiness memo and raises its total by one through its `RefCell`, stamps unmoved, `Memo::poke`), and `Panics` (a purchase the rule carried out panics before it settles). `seed(Option<SeededBug>)` plants one on the calling thread until its guard drops, in `test-ops` builds only; `has(bug)` is a constant `false` elsewhere, so each site reads as the engine without it. The stale visibility first sat in `Derived::on`'s `UnitPlaced` arm, where the properties never saw it: a move's own `CORE` touch marks the unit again.
- **`ActionSpec`** (`citar_testkit::spec`). `ActionSpec { tool: u8, actor: u8, target: u16, coords: (i16, i16), shape: Shape }` with `Shape::{Valid, Confused(u8), Random(u32)}`, bound late by `bind(&Game) -> Call { pid, tool, args }`: the tool modulo the registry's 61; the caller five times in eight the player whose turn it is, then another living major, any player, or one past the game's players; each argument drawn from what the game offers for that parameter (the caller's units and cities, then the named unit's promotions and improvements, the named city's buildable items and specialist slots, the techs it could research or has queued, its adoptable policies, the open beliefs, its spies, its negotiations, the other players, the parameter's `choices`), a tile within three of the named unit or city (wrapped across an edge the map wraps, held at one it does not), or, one spec in sixteen (`spec::off_map`), past one of the map's four edges, the optional parameters kept by the target's bits. `Confused` sends the parameters its mask picks as another type, or drops a required one; `Random` draws JSON from its seed, nulls at any depth. The shapes come 70/20/10 (`Shape::weighted`); `draw(&mut Rng)` is chaos's generator and `Arbitrary` the fuzz target's.
- **The runner** (`citar_testkit::stability`). `Step::{Call(ActionSpec), EndTurn, Agent}` (the host's `end_turn` for the player whose turn it is; a `RandomAgent` in every major's seat playing the current one's turn through the drive). `Run` checks, per step: a refused call leaves the digest, the revision and the event count as they were (P2) and reads as a sentence (P5, `refusal_rule_broken`: `api::text_rule_broken`, except that the word `None` is read as a caller's null, which the engine quotes back as Python would, when the call's arguments hold a null inside a value, `Unknown policy '[None]'.`, or a name the game keeps holds the word, `You have not met [None, 'x'].` after a `set_civ_name` with a list (`holds_none`: the players', leaders', cities', units' and religions' names, a religion's being the name its founder gave, which a refusal to spread it where it is followed already quotes; the state keeps the word of its own elsewhere, a spy's idle action), while `Some(`, `Idx(`, `::` and the other rules still hold; the agents' own check of their refusals reads it the same way); a query that answered left them too; a step that changed the game leaves no settle violation and no broken invariant (P3); the state has a digest before and after every step (P3: a digest error, a NaN anywhere, would otherwise make P2, P6 and P8 compare nothing with nothing; `games::save_and_load` likewise fails on either state's digest error); every `verify_every` steps (10) the oracle (P4); every `save_every` steps (25) `games::save_and_load` (P6), which plays on from the loaded game. `finish` runs the oracle and P7 (`no_stall`: on a copy, at most `stall_limit` host ends of turn, the living majors plus one, move the turn on or end the game; the "players" of §9.5's table read as the seats whose turns the host ends, since `end_turn` plays the city-states' and the barbarians' turns itself). `reads_are_free` plays the steps quietly, then again with `noise` before each step (the agents' `reads_and_refusals`, one to three times; query tools asked with spec-bound arguments; an action asked by a major whose turn it is not, asserted refused; a snapshot and its summary one time in ten) and noisy agents, and compares each step's refusal text and digest (P8). `verify_every: 0` turns the oracle off, finish included.
- **Properties P1 to P8** (`tests/props/games.rs`, scope 1). The starts are the committed fixtures (6 in 9, loaded once per thread, every city's citizens assigned by the engine through `assign_every_city_for_test`, so the citizen oracle and CITY-2 cover them), generated duel and small maps after 0 to 11 rounds of `RandomAgent`s (2 in 9), and the same on the kitchen-sink ruleset with the Kitchen Sink nation in the first seat (1 in 9); the steps are 1 to 47, a call 20 times in 23, an end of turn 2 and an agent's turn 1. `p1_to_p7_hold_along_any_calls` and `p8_reads_are_free` take `PROPTEST_CASES`, 64 when it is unset. Proptest keeps a failure beside the file (`FileFailurePersistence::WithSource`, as its default does after failing to find a `lib.rs` above an integration test, now without the warning; the earlier properties of `props.rs` too). A third property, `a_spec_binds_arguments_of_the_shape_it_names`, holds the shapes to what they say: a valid spec sends every required parameter, each of its schema type, and its tiles on the map exactly when `off_map` says so; `random_arguments_nest_nulls_now_and_then` and `a_spec_puts_a_tile_off_the_map_one_time_in_sixteen` keep those two classes of input in the mix. At 64 cases in the ci profile on the laptop, P1 to P7 take 6 to 12 s and P8 12 to 23 s (the lower figures alone, the higher beside the rest of the suite); on WSL Linux, 14 and 30 s under load.
- **Gate 1** (`tests/props/seeded.rs`): each bug planted and its property run for 64 cases from a fixed seed (`TestRng::deterministic_rng`), with proptest's default shrinking (four times the cases): mutating before refusing is found by P2, the stale visibility and the wrong touch by P4, and the query that writes by P8, each shrunk to one step. With the oracle on, P4 may find the query's write first, since it sees the memo; with the oracle off, P8 alone finds it. A hunt takes 0.3 to 1.3 s alone, 3 to 8 s beside the rest of the suite; the test fails above 120 s.
- **Pure properties** (`tests/props/pure.rs`): hex distance is a metric equal to a breadth-first search over the neighbour table on grids of 8 to 40 tiles a side with every wrap (and once on the widest), and `within` is the search's ball; RNG streams keyed apart are no copy of each other, nor one shifted by up to eight draws, and agree on about half of 64 coin flips. The others of §9.5 live beside their code, as the module lists: A* against Dijkstra (`game::path::tests`), incremental visibility and met sets against a rebuild (the oracle, in every property case), combat damage (`tests/engine/combat.rs`), `EntityStore` (`tests/engine/state.rs`), CSR order (`props.rs`), the journal chunk (`tests/engine/save.rs`).
- **Chaos** (`citar_testkit::chaos`, `cargo chaos`, scope 2). A game starts from `Start::{Generated, KitchenSink, Fixture}` (generated duel and small maps of five map types and three edges, one game in four on the kitchen-sink ruleset; with `--from-fixtures`, the committed fixtures and the corpus's when `CITAR_REFCHECK_CORPUS` names it, every city flagged for a citizen recheck first); before each seat's turn up to `--calls` (12) `ActionSpec` calls drawn from the game's seed, then the seat's turn by a `RandomAgent`, until `--rounds` (30) rounds or the time budget; every step through `stability::Run` under `catch_unwind` (a panic is P1). A failure is a `Replay { version, start, bug, options, steps, failure }`, written as JSON to `--out` (`$CARGO_TARGET_DIR/chaos`); `--replay FILE` plays its steps again with its bug and cadence and says whether it failed the same way (exit 1 if it failed again). `--bug NAME` plants a seeded bug. Gate 2, all in the ci profile with 0 failures, after the fix round (the binary the tests built, as CI runs it): on Windows, 600 s from seed 610 beside the full gate run, 514 games, 14,527 rounds, 304,436 steps and 260,852 calls (222,902 refused); on WSL Linux, 600 s from seed 611, 941 games, 26,585 rounds, 556,713 steps and 477,073 calls; from the fixtures and the corpus on Windows, 300 s from seed 613, 33 games and 31,252 steps. Those two seeds had failed once each way before P5 read a name's `None` (two replays on Windows, one on Linux, all of them a civilization named from a list), and their games now pass. Before the fix round: on Windows, 600 s from seed 602, 956 games, 27,054 rounds, 566,987 steps and 486,180 calls (415,296 refused), and before the kitchen-sink starts 600 s from seed 600 (990 games, 586,134 steps); on WSL Linux, 600 s from seed 601 beside a Windows build, 407 games, 11,463 rounds, 243,421 steps and 208,550 calls; from the fixtures and the corpus on Windows, 300 s, 54 games and 56,439 steps. About six calls in seven are refused: most specs name something the rules do not allow at that moment. macOS is rust.yml's (the chaos step on every push, and 600 s by a manual run with `chaos_seconds=600`), not run from the worktree.
- **Gate 3** (`tests/engine/chaos.rs`): chaos with the seeded panic fails within the first games; its replay, written out and read back, fails again at the same step with the same message, twice, and without the bug the same steps pass that step. Chaos also finds each of the other seeded bugs (mutating by P2; stale visibility, the wrong touch and the query's write by P4, chaos having no second game to compare with), and each replay fails the same way.
- **Fuzz** (`crates/citar-engine/fuzz`, scope 3): a cargo-fuzz crate outside the workspace, with its own lockfile, its targets `load_state` and `fuzz_one` calling `citar_testkit::fuzz` (a save's bytes to `Game::load`, whose loaded state must keep its invariants; a start and up to 64 steps read through `arbitrary`, each checked as the properties check them). libFuzzer's C++ runtime is behind the crate's default `libfuzzer` feature: `rust.yml` checks the targets on stable with `--no-default-features`, and the testkit tests run both entry points on a few inputs. Not run here: WSL has neither a nightly toolchain nor a C++ compiler; the README says how to run them there.
- **CI** (`rust.yml`, gate 4): the tests step already ran the properties at `PROPTEST_CASES=64`. New: the release clippy of the engine as it ships, the fuzz targets' check, and a chaos step after the doctests on all three OSes, a minute on every push, and the seconds given on a manual run (`workflow_dispatch` input `chaos_seconds`; 600 is gate 2), whose replays are kept as an artifact when it fails. The step runs `target/ci/chaos`, the binary the tests step built for testkit's integration tests (a `cargo run` would resolve the features again without the dev-dependencies and rebuild the engine and testkit, about two minutes), with the run's number as its seed, so that each push plays other games.
- **Kitchen sink.** No extra unique type is staged for this package's systems. The properties and chaos play the kitchen-sink ruleset among their starts (a game in nine and a game in four).
- **What these found.** The engine broke no property in any chaos run or property case. What the checks found was testkit's own: the first generator nested JSON nulls in its random arguments, which `set_research`'s refusal quoted back as Python's `None` (P5; Python would have written the same), so P5 now reads that word as the caller's null for such a call (the implementation first dropped nested nulls from the generator instead, which the fix round undid: that left a class of a model's input out of P1 to P3 for good; once they were back, chaos found within 600 s, on Windows and on Linux, the same word quoted later from a civilization's name that `set_civ_name` had taken from a list, so the reading covers the names the game keeps too), and proptest's two saved cases are committed; and the first site of the stale-visibility bug, `Derived::on`, was invisible to the properties, since a move's own `CORE` touch marks the unit again, so the bug sits in the settle's sight update.
- **Fix round.** Benchmarks turn the checks off in their games (`citar_bench::unchecked`), which feature unification would otherwise turn on under a root `cargo bench`; the oracle builds its cold game once; random arguments nest nulls again, with P5's reading of `None` (in the call's arguments, or in a name the game keeps); P7's bound counts the living majors; a digest error is a P3 breach and fails `save_and_load`; a spec's tiles are off the map one time in sixteen (before, one in 65,536) and wrap only where the map does; the properties count the committed fixtures once (a case saved when there were more still starts, modulo the count), the fuzz target its starts once; CI's chaos step runs the binary already built, seeded by the run's number. New tests: `tests/engine/stability.rs` (P5's reading of `None` on its own, after a `set_civ_name` with a list, and on a real `adopt_policy` refusal in a run; P7's bound against a round of each fixture with city-states or barbarians), the two generator tests above, and citar-bench's own. Its final attempt adds the religions' names to `holds_none` (a religion founded under a list that held a null is quoted in every later refusal to spread it where it is followed already), with a test that meets that refusal.

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
| 1e-02 | `long.json`, the nightly run's: `RandomAgent` games of 330 turns on every map size, duel to gargantuan, two on the kitchen-sink ruleset, and the three late fixtures passed on, one digest per round |
| Phase 2 | `bot-*` |

- **The file format.** `determinism.json` is `{format:1, blessed_with:{engine_build, toolchain}, games:[{name, spec, chain_root, digests[]}]}`.
- **Commands.** `cargo golden check|bless|diff` is an alias for the testkit `golden` binary. `bless` refuses while any stage the set depends on is `Pending`.
- **On failure,** the job uploads the canonical state JSON at the first turn where the targets diverge, for `golden diff` to compare with refcheck's comparator.
- **Decision (golden location).** The testing area's layout wins, since the games need `RandomAgent`, which lives in testkit.
- **Budgets.** Pull requests run the short set, under 60 seconds per target. The nightly run does everything.

**As built in 1c-10** (§9.5, §9.6, §10; Appendix B):
- **Files.** Testkit: `src/agents.rs` (completed), `src/games.rs` (new), `src/golden/games.rs` (new), `golden/{load, pass, random}.json`, `tests/engine/whole_game.rs` (new), `tests/determinism.rs`. Engine: `Game::driving` (`game/turn/drive.rs`), and `api/inspect.rs`, whose `view` and `briefing` are no longer `Pending` markers (below). `xtask/check.toml`: 1c-10 done, `forbid_all = true`.
- **`RandomAgent` is complete** (scope 1). The moves no package had taught it come after the others in `agents::MOVES`, so the earlier moves draw as before: `citizens` (`set_city_focus`, `work_tile` locking and releasing, `set_specialists` by hand or back to automatic, now and then past the slots), `free_choices` (a great person for each one owed, a pantheon belief nobody has taken when it may found one), `notes` (`set_civ_name`, now and then another civilization's name, which is refused; `write_notes`; `log_thought`; and the `end_turn` tool while `Game::driving` says it is driven, which the game refuses), and `untimely`, one time in ten an action whose moment may not have come (a free tech or great person, a vote, a fate for one of its cities, an upgrade or a sweep by any unit, a spy it does not have moved or sent to stage a coup), so that a short game tries all 40 action tools. At random among everything a city can build, a civilization stayed at a city or two for 330 turns; it now trains a settler a third of the time it may while its cities and settlers number fewer than three plus one every forty turns (at most eight), and sends a settler three times in four toward one of the best three sites within six (`automation::suggest_city_sites`). Every action goes through one `play`, which counts per thread what the agents tried and what the game took (`agents::take_tally`, read only by tests).
- **Whole games** (`citar_testkit::games`). `play_random` drives a `RandomAgent`, or any other `SeatDriver` (it is generic over the driver), in every major's seat with a seat limit of one and calls a hook after every round the chain takes, with its turn and digest; `pass_rounds` ends every seat's turn with nothing played; both count rounds by the game's chain and refuse a game that keeps none, which they would otherwise play past their count, to its end, without calling the hook; `from_fixture` loads a fixture as refcheck does; `save_and_load` saves as a host does, the journal's chunk before the snapshot (so the snapshot's heads count the chunk), loads the save with every chunk so far, refuses a history that does not rebuild whole or a state that is not the one saved, and resumes the chain (`DigestChain::resume`).
- **Gate 1** (`every_fixture_plays_five_pass_rounds_cleanly`): the twelve committed fixtures pass five rounds with every check at every settle; with `CITAR_REFCHECK_CORPUS`, the 250 corpus states too (told apart by the list each came from, `fixtures::committed` or `fixtures::corpus`, not by its path), with the invariants at every settle and the cache oracle after the rounds. All 262 are clean, with no panic (about 48 s in the ci profile, 22 s of it the committed fixtures).
- **Gate 2** (`a_random_agent_small_game_reaches_its_turn_limit_in_time`): a small continents map, four `RandomAgent`s, 330 turns to the turn limit and the Time victory, checks off: 1.1 to 1.8 s in the ci and dev profiles on the laptop, against the 10 s gate; the test prints the time and fails above the 30 s backstop in any build. The game ends with 27 cities (the city-states' among them) and 72 units.
- **Gate 3.** The chain of a game saved and loaded after every round equals the uninterrupted run's, round by round, with the same final digest and history: a duel of 120 rounds (the straight run with every invariant and every cache checked at every settle, which only read), a small game of 60, the fixture `standard-pangaea-normal-s1031/t120` passed 10 rounds, and the kitchen sink below. Beyond the committed tests, ten more games of 150 rounds saved every round, twelve of 100 rounds with every check at every settle, and ten noisy ones of 120 rounds were clean.
- **Property P8, early form** (`reads_saves_and_refusals_between_actions_change_no_digest`). `RandomAgent::noisy` calls `agents::reads_and_refusals` before every action it takes (from `agents::play`, which every action goes through, so between two actions of one move too), after its turn's last and before every answer, from a stream keyed apart from the agent's, which `play_turn` and `respond` set in a thread-local slot for their length (`NoiseScope`) so that the moves draw as a quiet agent's: two reads (an `inspect` query of any of its 26 kinds about things drawn from the game, `view` and `briefing` among them, refused until 1d; a unit's reach, a preview and `plan_attack`, a city's buildable list and the advisor's pick, stats rows, events, thoughts, `end_turn_refusal`, every open negotiation's view, the digest), half the time a call the game must refuse (a move of no unit or out of turn, research of no tech, a policy as a number, an answer to no negotiation, `end_turn` for another player and `force_turn` on player 200 outside the drive, or, inside it, the driven seat's own turn ended or forced, which the drive's guard refuses before it looks at the player; scenario operations that fail halfway, a build by no unit; the test panics if one is carried out), and one time in fifty a save (the snapshot as JSON, `save::summary`; no journal chunk, whose cursor is the host's: a chunk the noise took would be missing from the host's list, and its next load would find the history incomplete). A small game of 50 rounds with noisy agents and five more rounds of noise after each round chains as the quiet one, and so does the same game saved and loaded after every round (`games::save_and_load`); the test also checks that the noise ran at least once per action taken plus the hook's. The host heads a journal chunk moves are host-only, so a save is never digested.
- **Every action.** `the_agent_tries_every_action_and_the_game_takes_most`: over two plain games of 120 rounds the agent tries all 40 action tools of `api::tools::args::TOOLS` (since 1d-01, the 40 actions of `api::tools::registry::TOOLS`); with a game set up in the industrial era (everyone met, 3,000 gold, a free tech owed, a capital and two spearmen each, Liberty finished by the person's seat, which owes it a great person), the game carries out each but `air_sweep` (a promotion no unit starts with), `city_status` (a conquered city), `end_turn` (always refused a driven seat), `return_civilian` and `un_vote` (the United Nations).
- **Kitchen sink.** No extra unique type is staged for this package's systems. `the_kitchen_sink_plays_random_games_with_every_check_and_a_save_every_round` plays the kitchen-sink ruleset with a Kitchen Sink civilization among `RandomAgent`s (a duel of 60 rounds and a small game of 30) with every check at every settle and a save and a load after every round, all clean; four more such games of 100 rounds were clean too. Deferred: none.
- **Golden sets** (gate 4, `golden::games`). `load.json`: each committed fixture after `Game::from_python`'s settle, `[fixture, digest, canonical length, [players, cities, units], explored tiles, events]`, and the check asks each loaded state's invariants and a cold rebuild of its caches. `pass.json`: twenty rounds passed from `duel-continents-normal/t50`, `small-pangaea-raging/t50` and `small-continents-normal-s1025/t280`, a row per game (`[fixture, first turn, rounds, chain head, events, over]`) and one per round (`[fixture, turn, digest]`). `random.json`: six `RandomAgent` games on generated maps (duel continents and pangaea for 200 turns, small fractal and archipelago for 120, standard continents for 60, large pangaea for 30), a row per game (`[name, seed, [width, height], rounds, chain head, final digest, [cities, units, deals], events, winner]`) and one per round, 730 in all. `pass` and `random` depend on every stage, as `turns` does (`turns::waiting`), so `bless` refuses them while one is pending. The whole `golden check` takes about 4 s per target in the ci profile, so the pull-request run needs no short set. Windows x64 (ci and release profiles) and Linux x64 (WSL) compute the same 15 sets; the other targets are the determinism workflow's. Rows are row lists, not the `determinism.json` of the plan above: each set keeps the shape of the earlier ones, and a diff names the first round that moved.
- **Pending stages** (gate 1 of 1c-09, gate 6 here). No `Pending` marker remains: `cargo xtask check` counts 2 `NotPorted` and 0 `Pending`, and fails on any `Pending` from now on (`forbid_all`, pinned by xtask's own test of the committed file). The `inspect` queries `view` and `briefing`, the last two markers, are no stage: `QUERIES` now says which package answers them with a local `Answered::From("1d-02")`, which `pending` still lists, while their arms refuse them with the `not_ported` markers the check counts and 1e-04 forbids.
- **Refcheck** (gate 5): every group but `tool_errors`, `views` and `briefing` was already enforced on the committed fixtures, and all are at 0 in `ratchet.json`; nothing changed.
- **What these found.** The engine passed everything above without a change. The one bug was the test helper's: a snapshot taken before the journal's chunk names one chunk fewer than the history holds, which `Game::load` rightly reports as `chronicle_incomplete`; hosts take the chunk first (§4.11), as `games::save_and_load` now does.
- **Fix round.** The review found the noise's save taking the journal's chunk and dropping it (a noisy agent could not be saved and loaded with `save_and_load`: the load found the history incomplete at turn 2), the helpers counting rounds by a chain the game might not keep, noise only between moves, not between the actions of one move, the drive's refusals labelled as the wrong-player ones they never reached, and gate 1 telling committed fixtures from corpus ones by path. All fixed as above. `game::mutate`'s impl-wide `allow(dead_code)`, which waited for the rule systems up to 1c-10, is gone: the wrappers `push_build` and `pop_build`, which nothing called (workers replace a tile's queue whole with `set_builds`), are deleted, while `Tiles` keeps its own; `edit_config`, which only tests call (no action, operation or host call edits the settings of a game under way), and the state's `config_mut` under it are compiled for the tests that call them only (`cfg(all(test, feature = "embedded-ruleset"))`); and `set_auto_decision`, which only the test operation `set_auto` calls (a host sets a seat's overrides with `set_seat_controller`), needs `test-ops`. The same holds for the other dead-code allows whose packages have landed: those on the state's restricted accessors (1a-09 to 1b-01) and on `step_seeing` (1c-02) are gone, and the one accessor they hid that nothing calls, `State::chronicle_mut` (`heads_mut` moves both heads), is deleted. The allows on `pending` and `pending_or` stay: no stage waits now, but a later port may mark one again.

**As built in 1e-02** (§2.6, §6.7, §7.5, §9.5, §9.6; Appendix B):
- **Files.** Testkit: `src/soak.rs`, `src/bin/soak.rs`, `src/golden/{divergence, dump}.rs` and `golden/long.json` (new); `src/golden.rs`, `src/golden/{games, turns, newgame}.rs` and every set's report, `src/bin/golden.rs`, `tests/determinism.rs`, `tests/engine/whole_game.rs`, `tests/engine/cities.rs`. Engine: `game/turn/settle.rs`, `game/cities/citizens.rs`, `game/core.rs` (the settle's cycle stop, below). CI: `determinism.yml`, `nightly.yml` (new), `.github/scripts/golden_compare.py` (new), `.config/nextest.toml` (profile `nightly`), `.cargo/config.toml` (`cargo soak`). Docs: CONTRIBUTING.md ("Rust"), refcheck/README.md ("The nightly run and the corpus"). `scripts/determinism_hazards.py` plants gate 3's hazards.
- **Goldens** (scope 1). Every set of the table above was already in place (rng, libm, pyfmt, the ruleset's id, the known-answer states, convert, maps, newgame, load, pass, random, and 1b-03's turns); this package finishes the binary around them and adds the long set.
  - **Reports** carry each set's problems and a 16-digit hash of every row of each of its lists. `golden diff A B` of two reports names, for each set that differs, the first row of each list where they part, with the committed file's row there: a game and its turn for the whole-game sets.
  - **The short set** is every set but `long`: the whole `golden check` takes 4 to 5 s a target in the ci profile, under §9.6's 60 s, so pull requests check everything else.
  - **`long.json`** (`golden check --long`, `golden bless long`): `RandomAgent` games to 330 turns on every map size (duel fractal, small inland sea, standard archipelago, large continents, huge fractal, gargantuan pangaea; seeds 201 to 206), two on the kitchen-sink ruleset (a duel and a small map, both won before the limit), and the late fixtures passed on (`small-continents-normal-s1025/t280` 60 rounds, `standard-pangaea-normal-s1031/t120` and `scenario-small-continents-s3001/t61` 100 each), 2,705 round rows. About 45 to 50 s a target in the ci profile on the laptop, 64 to 78 s under WSL beside other work. It is not in `check_all` or `blessed_files`, so neither the pull-request matrix nor `blessing_reproduces_the_committed_files` plays it; the nightly run checks it on every target. The whole-game sets share `golden::games::Play`.
  - **Divergence artifacts.** `golden check --states DIR` watches every round of each whole-game set's games against the committed digests (`golden::divergence::Watch`) and, at the first round that differs, writes the state as that round ended and as the round before ended (save JSON), listed in `DIR/divergence.jsonl`; `newgame` and `load` write a differing row's state. Each check starts the list afresh: the list a check before it left in the folder, and the states that list names, are removed first. `golden dump SET:GAME [TURN]` plays any golden game to the end of a round on this machine (`golden dump --list` names the 45), and `golden diff` of two states lists every place they differ.
  - **Deviations.** The rows stay the row lists 1c-10 chose, not the `determinism.json` file of the format above: the game sets' rows carry the same things (the name as the spec, the chain head, every round's digest). The state diff is exact (numbers by their round-trip text, so a last bit and `-0.0` count) rather than refcheck's comparator, whose 1e-6 tolerance would hide the last-bit differences a platform `libm` makes.
- **Extra determinism checks** (scope 2). `same_process_twice` (`tests/determinism.rs`) plays two copies of a small game (60 rounds), a kitchen-sink duel (40) and the late fixture `small-continents-normal-s1025/t280` (6) side by side in one process, comparing every round's digest and, every few rounds and at the end, everything a host reads: the canonical state, the save, the spectator's and every major's view, every major's briefing, the standings and the replay; then a third copy on another thread, whose hash maps are seeded from that thread's keys, round by round. About 5 s in the ci profile. The ci-against-release run is determinism.yml's `linux-x64-release` row (since 1a-02), and the nightly run's long set has it too.
- **determinism.yml** (scope 3): the five targets and linux-x64 in the release profile, as before, now with `--states`; each target uploads its report, and its states when its check fails. The compare job (`.github/scripts/golden_compare.py compare`) groups the targets by what each computed for each set; the reference group is the one that matches the committed file, or the largest; for each other group it names the first row of each list where it parts. When targets disagree, the same job builds `golden`, plays each odd target's divergent games to the same rounds on linux-x64 (`golden_compare.py states`), diffs the states and uploads them with the diffs as the `divergence` artifact (if linux-x64 is itself odd, the script says the reference must be dumped on a target that agrees).
- **The soak** (scope 4; `citar_testkit::soak`, `cargo soak`). A run's games are planned from its seed: the six sizes in turn, the five map types and five edge modes rotating under them, every fourth lap the duel, small and standard games on the kitchen-sink ruleset; each plays to its 330-turn limit (`--max-rounds` caps it) under `catch_unwind`, with the invariants at every settle, the violations collected every round, the cache oracle every 50 rounds and a save and a load every 25 (both at the end too), and a game still playing three rounds past its limit is a failure. It reports panics, violations, rounds over 8 times their game's median and over 50 ms, and peak memory: the most heap each game held, counted by the binary's global allocator (a wrapper of the system allocator that keeps a running total and its high-water mark; the one `unsafe` in testkit, `deny`, not `forbid`, in that binary alone), the same measure on every OS, with the process's peak resident set on Linux beside it. `--shard K/N` takes whole laps of the sizes, `--game I` plays one game again. The report counts the games the run (or shard) was to play beside those it played, and a run its time budget (`--seconds`) stopped before its last game exits 3, however clean the games it played: a slow runner must not quietly shrink the run. In the ci profile on the laptop a game takes about 1 s (duel) to 45-65 s (gargantuan).
- **nightly.yml** (scope 5). Jobs: the properties at 10,000 cases (`PROPTEST_CASES`) in three jobs (P1 to P7, P8, the rest) under a nextest profile `nightly` that lets a test run 100 minutes, keeping proptest's saved cases as an artifact on failure; `stability` on Linux, Windows and macOS: chaos for 1,200 s seeded a million past the run's number (rust.yml's seeds are the run's number), then the soak of 36 games (six of each size) capped at 2,700 s, which fails the job if the cap stops it early, its JSON report kept; `golden-long` on the five targets and linux-x64 in release, compared by the same script; `criterion` on Sundays and on a manual run, every suite of citar-bench as a trend kept 90 days, report-only (`continue-on-error`, `--no-fail-fast`: the suites' own limits are the laptop's). A schedule runs on the default branch only, so a pull request labelled `nightly` runs the whole workflow too. The full-corpus refcheck `--strict` is not in it: the corpus stays on the laptop (the owner's decision of 2026-09-23), and refcheck/README.md documents the laptop run instead (with the corpus's pass rounds and chaos from its states).
- **The settle's cycle stop** (found by the soak; §6.7). Two kitchen-sink games of the first 200-game soak ran out of settle passes (SETTLE-1) for several turns: a city's best assignment depended on its own specialists (the Kitchen Sink nation's `[+10]% [Food] <in cities with [2] [Specialists]>`, counted from the specialists the city has while the next citizen's food is weighed) and had no fixed point. Python placed the citizens once per refresh and flipped from one refresh to the next. A city in one of the settle's passes (`Game::reassign_in_settle`) whose new assignment is one the settle under way has already moved it away from (`Game::citizens_held`, emptied at the start and the end of the citizen passes, never saved; a reassignment outside a settle, as a city's end of turn does before it picks what to build, records none) now takes it only if it stands: the city is put there and assigned again, and if that moves it on, the write is undone and the city stops where it was, in its cycle. As deterministic as the passes, and the state a refused return leaves is the one before it (the revisions move; the caches follow them). The citizen oracle accepts a city whose reassignments, followed on a copy of the game, lead back to its own assignment within a settle's passes, which a stale city's fixed point never does. No golden set moved. `a_city_whose_uniques_read_its_own_citizens_settles_where_its_reassignments_come_back` replays the game (it fails at turn 324 without the change, and checks the oracle every round from turn 318).
- **Fix round.** The review found the first form of the stop, which refused every return to a held assignment, too strong: a city whose inputs another city's move changed back may need its earlier assignment again (with `<in tiles adjacent to [worked] tiles>`, a city leaves its plains while the tile beside them is not worked and must come back when a neighbour takes it), and the oracle then failed the city stuck on the worse one. The review proposed dropping a city's held entries whenever another city's move flags it; that makes two neighbouring cities each in its own cycle flag each other at every flip and clear each other's entries, so both flip until the passes run out (SETTLE-1). Trying the return instead covers both: `tests/engine/cities.rs` has a hand-built game for each on a ruleset whose plains yield 3 more food beside a worked tile, `a_city_takes_back_an_assignment_another_citys_move_made_good_again` (it fails under the first form) and `two_neighbouring_cities_each_in_its_own_cycle_both_stop` (it fails under the proposed one). Two cities that move each other's yields with no fixed point between them, each fine alone, still flip until the passes run out, as every cycle did before. The other fixes: the soak's time budget (above), the divergence list started afresh (above), and nightly.yml's note that before the workflow is on the default branch only the `nightly` label runs it (a manual run needs it there too), which the label must first be created for. Every gate again at the fix round's head: (1) every set, the long one included, identical on Windows x64 and Linux x64 (WSL, a fresh clone), each in the ci and release profiles, and equal to the committed files; (2) `same_process_twice` passes on both, in the whole suite (1,124 tests, the corpus's on Windows); (3) the plants again in a throwaway clone: clippy refuses the plain ones with the same four errors, and with the allows the hash order moves `pass` and `random` and fails `same_process_twice` (round 2 of the late fixture), the platform `powf` moves `libm`, and `f64::round` moves `pyfmt`, `turns`, `newgame`, `pass` and `random`; (4) the nightly jobs' commands on the laptop, all clean: the properties at 10,000 cases, 21 of 21 (P8 999 s, P1 to P7 339 s), chaos for 1,200 s on WSL (seed 1000049: 1,577 games, 0 failures) and 600 s on Windows (seed 1000050: 1,030 games), the 36-game soak on WSL in 286 s, the long set, and `cargo bench --no-fail-fast -p citar-bench` on WSL in 919 s (exit 0; the soft budgets it reports over are report-only); (5) the 200-game soak (seed 2026, four shards, 913 s) clean again, the same 64,166 rounds, no outlier, the most heap a game held 17.5 MiB (duel) to 124.7 MiB (gargantuan).
- **Gates.** (1) Every golden set, the long one included, is identical on Windows x64 and Linux x64 (WSL), each in the ci and release profiles, and equal to the committed files (`golden_compare.py compare` over the four reports). linux-arm64 too, emulated: the golden binary cross-built for `aarch64-unknown-linux-gnu` (Ubuntu 24.04's GCC 13 cross toolchain as the linker) and run under qemu-user 8.2 computes every set, the long one included, as the committed files in both profiles (692 s and 338 s), so the compare finds the six reports identical. The macOS targets (arm64 and x64), and linux-arm64 on real hardware, are determinism.yml's and nightly.yml's, which a worktree cannot run; the short sets, whose files this package does not change, were identical on all six of determinism.yml's targets at the merge base and again at v0.1.6's 19c3c7f. (2) `same_process_twice` passes on Windows and Linux x64, and on linux-arm64 under qemu-user, with the rest of `tests/determinism.rs` and the settle's three tests. (3) On a scratch branch never pushed, with `scripts/determinism_hazards.py`: each hazard as plain code fails `cargo clippy -p citar-engine --all-targets -- -D warnings` (`f64::powf`, `f64::round`, `std::collections::HashSet` and iteration over it: four errors); with the allows that silence clippy, the matrix fails on Windows: the hash order moves `pass` and `random` and fails `same_process_twice` (round 2 of the late fixture), the platform `powf` moves `libm` (MSVC's `pow` differs from `libm`'s in the last bit for many of its inputs), and `f64::round` moves `pyfmt`, `turns`, `newgame`, `pass` and `random`. (4) nightly.yml runs only on GitHub, which a worktree cannot reach; each job's commands were run on the laptop instead, all clean: the properties at 10,000 cases on Windows, 21 of 21 (P8 1,028 s, P1 to P7 387 s, the rest under a minute, beside other work); chaos for 1,200 s on WSL Linux (seed 1000047: 1,622 games, 958,409 steps, 0 failures) and 600 s on each OS at the final head; the 36-game soak in 258 s on WSL Linux at the final head (748 s beside other work); the long golden set in 45 to 97 s a target and profile; `cargo bench --no-fail-fast -p citar-bench` in 862 s on WSL (a 5.5-minute build, then 49 measures, none over its limit there). rust.yml's runners played chaos about as fast as the laptop (994 to 1,131 games in 600 s in its last run, against 941 to 1,416 here, 958 on Windows at the final head), so the longest jobs should take about 40 minutes (a stability job) and 20 (P8), well inside the two hours. (5) The laptop soak of 200 games (seed 2026, four shards, 1,105 s beside other work) is clean: every game ended, at its limit or by a victory, 64,166 rounds, 24 games on the kitchen sink, one outlier (a 110 ms round against its game's median of 12.5 ms, under load), the most heap a game held from 17.5 MiB (duel) to 124.7 MiB (gargantuan). The first such run, before the settle's cycle stop, failed 2 of its 200 games (the SETTLE-1 above).
- **Kitchen sink.** No extra unique type is staged for 1e-02's systems. The long set and the soak play the kitchen-sink ruleset, and the soak found the cycle above in it. Deferred: none.

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
- the `stats` feature counts memo hits and misses, so a soak can measure the recomputes that come out as they were against 5% per memo type (Python measured 74-99.5%); for Phase 1 the soak gates each memo's count of recomputes against the count it recorded instead (below).

**`overflow-checks = true`** in release stays unless the bench report shows it costs more than 3%. As measured in 1e-03 it cost 5-9% of a pass round, so release is built without them and the `ci` profile keeps them (see "As built in 1e-03" below).

**Phase 1 decisions on these gates** (1e-03's fix round, on the owner's priority: stability first, then speed; Appendix A, 62):
- *Memo redundancy is guarded against growth, not held under 5%.* The soak records how often each memo recomputes, and a test fails when a memo recomputes more than a quarter above its recorded count, or when a memo not recorded makes 5% of its recomputes to the value it had (`no_memo_recomputes_more_than_the_soak_recorded`, `tests/engine/memos.rs`). The 5% test stays as an ignored diagnostic that prints the table (`--run-ignored all`). The reason: the speed the bound stands for is there by a wide margin (no corpus pass round is under 133 times as fast as Python's engine, 146 times at the fix round's end, where the gate asks 20), and taking each memo under 5% needs revisions finer than the inputs each memo names now, memo by memo, a new way for a cache to go stale that would trade stability for an internal measure. Phase 2 may take it up, with the soak to measure it.
- *A\* is gated cold.* `astar_small_30` times searches that look at every tile afresh, as the first search after a write does (`Game::forget_path_looks_for_bench` before each); a search of the same mover at the same revision reads the looks of the one before it (`LookKey`), and that warm number is report-only (`astar_small_30/warm`). The cold number must hold the hard limit, and does: 25.4 µs against the 30 µs limit (1.27 times the budget).
- *Measures over their budget and within the 1.5x hard limit are accepted for Phase 1*, recorded with what they measured at the end of 1e-03: `astar_small_30` 25.4 µs (each search cold) against 20 µs (1.27x). A measure over the hard limit fails the gate.

**As built in 1e-03** (§2.4, §6.4, §9.7, §10):
- **Files.** Bench (`crates/citar-bench`): `src/lib.rs` (core pinning, `Suite`, the medians, the results each suite writes to `<target>/perf/<suite>.json`), `src/fixtures.rs` (the committed and corpus states, the late fixture, the synthetic gargantuan state, the queries refcheck recorded), `src/thresholds.rs` and `thresholds.toml` (the budgets); `benches/kernels/` (`main.rs` and a part a system: `hex`, `stats`, `index`, `vis`, `path`, `combat`, `buildable`, `jobs`, `religion`, `barbarians`, `advisor`, `mapgen`), `benches/turns.rs`, `benches/io.rs` and `benches/iai.rs` (feature `iai`), which replace the earlier packages' benches and keep their measures (1d-02's views and 1d-03's briefing are in `turns`, 1c-10's digest in `io`); `examples/profile.rs`, one part run in a loop for callgrind or a profiler. `xtask/src/perf.rs` (`cargo xtask perf`). `scripts/refcheck/turn_timing.py` and `refcheck/perf/python-turns.json`. The `perf` job of `rust.yml`. The workspace `Cargo.toml` (`core_affinity`, the profiles). Engine: the tuning below. Testkit: `tests/engine/memos.rs` (feature `stats`).
- **The suites** (§9.7): `cargo bench -p citar-bench --bench kernels|turns|io [-- <part>]`. Each run pins itself to core 0 (`CITAR_BENCH_CORE=<n>` names another, `none` leaves it free) and reads the corpus when `CITAR_REFCHECK_CORPUS` names it, the committed fixtures otherwise. Criterion times each measure for the record; what is gated is the suite's own median of the operation alone (`Suite::put`), since Criterion's estimate of a batched routine counts its setup. Every gated measure has a row in `thresholds.toml` (`id`, `suite`, `budget`, the §10 row it holds, `corpus = true` when it needs the corpus): `Suite::put` of a measure without one panics, and `Suite::note` records a measure report-only.
  - `kernels`: hex; tile yields (a hit at a stable revision, the first read after an unrelated change, a recompute); city stats; citizens at pop 20; happiness; the civ index; the visibility step at sight 2 (with and without the line-of-sight cache) and sight 5; line of sight; A* (30 tiles on small maps, 54 through fog on the gargantuan state, the mean over refcheck's 480 recorded pairs); `reachable`; the combat preview (and, report-only, over the recorded fights); buildable; connectivity small and gargantuan; the job map; a settle with nothing pending; the religion and barbarian rounds; the production advisor; map generation.
  - `turns`: `turn/pass_round/<case>/t<turn>` on every corpus state (the twelve committed ones without the corpus): one round passed untimed so the memos hold what a game under way holds, the next timed on fresh copies, the median kept, as `turn_timing.py` times Python's; `view/player`, `view/god`, `view/god_gargantuan`; `briefing/small`, `briefing/turn_progress` and `briefing/large` (report-only); `game/random_small_330`.
  - `io`: the embedded ruleset's load, `snapshot/gargantuan`, `to_json` small t280 and gargantuan, `load/small_t280` (the state alone; with its history, report-only), the two digests.
- **perfgate** (`cargo xtask perf [--check] [--suite kernels|turns|io] [-- <args>]`) runs the suites, or with `--check` reads what they last wrote, and prints each gated measure against its budget. It fails (exit 1) when a measure is over 1.5 times its budget (`hard`), or a pass round is under 20 times Python's (`[pass_round] ratio`), over its backstop (small t280 150 ms, large t280 400 ms) or over 1.5 times its target budget (the §10 points, interpolated between). A measure over its budget but within the hard limit is listed, and so is a round over its target. It exits 2 when it cannot check this run's numbers: it runs the suites with `--no-fail-fast` and gives each its run's id (`CITAR_PERF_RUN`), which the suite writes into its file, so a suite that failed or did not build, whose file an earlier run left, fails the check rather than passing on stale numbers; and a failed `cargo bench` with no measure over its hard limit to say why exits 2 too. Criterion takes one filter, which perfgate and the suites hold to; a filter runs the parts it names or starts (`advisor` runs `advisor`, not `vis`), and a filtered run keeps what an earlier run measured of the other parts and says so (`merged`), while a whole run writes only its own. A suite's JSON says the core it ran on, whether it had the corpus and whether the build had overflow checks.
- **Python ratios** (`PYTHONHASHSEED=0 python scripts/refcheck/turn_timing.py [--workers n]`): each corpus state loaded in Python's engine, its visibility refreshed, a round passed untimed (`testops._end_round`) and the next timed. `refcheck/perf/python-turns.json` holds the 250 states' milliseconds, a line a state, and the engine's hash; it was recorded with 3 workers on the laptop.
- **The CI gate** (§9.7's decision, gate 5). On a pull request, the `perf` job of `rust.yml` (ubuntu-24.04, valgrind from apt, `gungraun-runner` 0.19.4, which must equal the library's version in `Cargo.lock`) runs the base branch's `iai` suite from a worktree with `--save-baseline=base`, then the pull request's with `--baseline=base --callgrind-limits='ir=5%'`; a base without the suite leaves nothing to compare. The label `perf-accepted` lets an accepted rise through: the step asks GitHub for the pull request's labels when the counts rose, so a label added after a failed run lets the re-run pass (a re-run keeps the first run's event, labels and all). The suite has 8 kernels (hex on the gargantuan grid, tile yields, city stats, citizens, the recorded path pairs of the late fixture, a sight step, the recorded fights, buildable), each repeating uniform work ten times and handing its input back so the drop is not counted, and 2 macros (a pass round and a player's view of the late fixture). Checked in WSL (again at the fix round's head): with `CITAR_IAI_SYNTHETIC=1` each kernel repeats its work 11 times, and against the saved baseline every kernel rose 9.1 to 10.0% and the run failed ("Regressed", 8 of 10, exit 3); unchanged runs repeat within 0.01%.
- **Overflow checks** (§2.4). Measured in WSL on `examples/profile.rs` built as release (fat LTO), the minimum of 8 interleaved runs with and without them: a pass round of the small t280 fixture +9.3% (instructions +11.9%), of `large-pangaea-normal-s1016/t280` +6.8% (+10.4%), a combat preview +8.4%, A* +5.4%, a barbarian round +3.1%, the advisor +2.4%. That is over the 3% rule, so `[profile.release]` has `overflow-checks = false` and `[profile.ci]` turns them back on: every test, golden, refcheck and chaos run keeps them, and an overflow there fails loudly. The goldens are the same built either way.
- **Tuning** (medians on the laptop, before and after):
  - *The production advisor* (`advisor/call_per_city` 123 µs to 43.2 µs; budget 50 µs). A conditional that reads a city's buildings (`CityWithBuilding`, `CityWithoutBuilding`, a city filter that reads buildings) records the class `CondDeps::CITY_BUILDINGS` besides `CITY` (`UniqueTable::cond_deps`), so a what-if that adds a building moves only what reads buildings. A city's tile yields are reused when none of the modifiers the building changes can land on the tile (`mods_changed`, `tiles::mod_may_land`). A city's base keeps each building's yield (`CityBase::each`), and when the base read nothing the building moves (`base_holds`) the what-if sums the kept yields and the new building's (`one_building_stats_in`) in the set's order, instead of working the base out again: bit-exact, as the what-if corpus test checks. Since the fix round "nothing the building moves" does not rest on the classes alone: the base is watched where the view lends a city's buildings to every filter, conditional and count (`record::watch_buildings` at `FilterFacts::city_buildings`, `CityBase::reads_buildings`), its own walk over its buildings the one read left out, so a base that read any city's buildings otherwise is worked out again whatever classes its uniques name; debug builds check every reuse against the base worked out again. A harbour's what-if reads the network as it is and the water its harbours' flood reached, which the connectivity memo keeps beside its value (`connections::with_harbour`, `derive::stats::connectivity_water`), and floods again only when the harbour may link a city the network does not (54 µs to 44 µs).
  - *Barbarians* (`barbarians/round` 3.78 ms to 1.56 ms; budget 2 ms). A civilian a barbarian captured searches its nearest free camp first, a search that stops when it gets there, and builds one `PathTree` of 40 turns for the other camps only after a search finds nothing (the fix round's; first a tree for any two camps); a unit's seek searches its first destination and builds the multi-turn tree only when that search fails.
  - *A\** (`astar_small_30`, each search cold: about 35 µs at the review to 25.4 µs, Criterion's own estimate, which cycles the three maps at every iteration, 30.5 µs; warm 41 µs to 19.2 µs; budget 20 µs). A search reuses the tile looks of the same mover's earlier search at the same revision (`LookKey`: revision, unit, player, base unit; which relies on every write moving the revision), divides by the bound's divisor with a multiplication (`Divisor`, Lemire's), reads a neighbour's cell once, and reads zones of control as a mask. In the fix round, for the cold search: a first look reads a tile's terrain once (`node::Ground`) and is one function; a popped tile's cell is visited once and a neighbour's read in place; each tile is measured to the target's coordinates worked out once (`HexGrid::distance_to_cube`); the heap's entries are 24 bytes, and compared without a branch (`Entry::before`, the derived order exactly). Those took 18% of the instructions and no time, until the last: which of two entries comes first is a coin toss, and a mispredicted compare at every level of a pop cost more than the instructions saved (35.6 µs to 28.3 µs in interleaved runs on a loaded laptop).
  - *Combat* (`combat/preview` 1.33 µs to 732 ns; budget 1 µs). The units of a civilization that may carry a great general's aura are a memo (`derive::civ::aura_units`, on its roster, and on any unit's promotions only when a promotion may carry an aura), so a fighter asks those few instead of every tile within the widest aura; a preview's four damages share the power of the strength ratio (`CombatSetup::damage_range`), each computed as before. In the fix round: a unit asked about a type its profile and its owner's index lack answers without building a query (`uq::unit_candidates`), the profile table keeps its last two keys, which a fight asks a score of times, and a fighter's neighbours are weighed as they are met.
  - *Religion*: a memo of each city's majority religion with its followers (`derive::religion::majority`), which `majority_religion` and `followers_of_majority` read.
  - *Smaller*: Python's `within` order sorts on keys worked out once (`sort_by_cached_key`); a sight settle sizes its vectors up front; `Memo::get` and `CopyMemo::get` are generic over the memo's closure, so the `stats` feature counts each memo where it is read (`derive::rev::tally`).
- **Fix round** (the review's findings):
  - *The what-if gave a wrong value* (the blocker): `[+5 Gold] [in all cities with a world wonder]` and its per-population form read the city's buildings through a filter whose classes leave the city asked out, so the what-if of a wonder reused a base the wonder moves (Gold 3 against 8 built). The base now notes the filter's own classes where it asks them (`cities::stats::uniques_by_source`), and the audit that followed found a count of cities whose filter reads buildings (`[[in all cities with a world wonder] Cities]`) naming only `CITY_COUNT`: `Countable::deps` names `CIV_BUILDINGS` for it. Then the watch above made the reuse rest on what the base read rather than on the classes. The overlay test on the shipped ruleset (`flat_stats_of_cities_with_a_wonder_come_with_the_wonder`) has all three uniques, and fails with the count's class and the watch taken out.
  - *The connectivity memo kept stale water*: its value is equal to a new one by its cities, so a network that came out as it was kept the water an earlier flood walked, which a harbour's what-if reads; the cache oracle found it in the random small game. The water is kept beside the memo, set by every computation.
  - *perfgate passed on stale numbers*: the run ids, `--no-fail-fast`, one filter and whole-run files above.
  - *The A\* budget held only warm*: gated cold, and tuned under the hard limit (above).
  - *The `perf-accepted` label could not take effect on a re-run*: read at run time (above).
  - *A captured civilian built a 40-turn tree for any two camps*: the nearest first (above).
  - Two lints that failed at the reviewed head: clippy's `type_complexity` on the profile table's last keys, and `cargo doc`'s ambiguous link to `uq::unit`.
- **Results** (the fix round's last run of the three suites at high priority on a quiet laptop, the load sampled throughout, then `cargo xtask perf --check`: overflow checks off, the corpus on, core 0; exit 0, every budget within the hard limit). Over the budget and within the hard limit: `astar_small_30` 25.4 µs (each search cold) against 20 µs (1.27x). The rest under: `tile_yield/hit` 5 ns, the first read after a change 42 ns, a recompute 170 ns; `city_stats/recompute` 5.83 µs; `citizens/assign_pop20` 4.87 µs; `happiness/recompute` 1.88 µs; `civ_index` lookup 3 ns, rebuild 2.16 µs; `vis_step/sight2` 855 ns, without the line-of-sight cache 1.40 µs, `sight5` 1.11 µs; `los/sight3_uncached` 554 ns; `astar_garg_fog` 35.5 µs; `astar/recorded_pairs` 10.4 µs; `reachable` 2.93 µs; `combat/preview` 732 ns; buildable 9.53 µs, a hit 26 ns; connectivity 1.72 µs small and 2.60 µs gargantuan; `jobmap_small` 64.8 µs; a settle 5 ns; `religion/round` 188 µs; `barbarians/round` 1.56 ms; `advisor/call_per_city` 43.2 µs; map generation 4.91 ms small and 55.3 ms gargantuan; `view/player` 571 µs; `view/god` 2.28 ms; `view/god_gargantuan` 10.1 ms; `briefing/small` 417 µs, `briefing/turn_progress` 12.9 µs, `briefing/large` 343 µs; `game/random_small_330` 741.4 ms; the ruleset's load 6.57 ms; `snapshot/gargantuan` 979 µs; `to_json` 632 µs small and 23.4 ms gargantuan; `load/small_t280` 6.76 ms; the digests 2.85 ms gargantuan and 174 µs small. Pass rounds on the 250 corpus states: the lowest ratio to Python 146x (`duel-archipelago-raging-s1002/t1`, 0.18 ms), the slowest round 31.2 ms (`large-pangaea-off-s1036/t280`, 1110x), the small t280 rounds at most 15.2 ms, the large at most 31.2 ms, the gargantuan state at turn 25 7.9 ms; no round over its target budget. The small t280 rounds read at most 15.2 ms where the implementation's run read 12.8 ms on the same state (`small-fractal-off-s1009/t280`): its pass round at the fix session's start and at its end counts the same instructions under callgrind (387.1 and 385.8 million) and takes the same time natively (24-25 ms in the profiling build in WSL), so the difference is between the two runs, not the code. A run while another package's soaks and builds loaded the laptop measured the turns and io suites up to twice as slow (the small digest over its hard limit, which a quiet run puts at 0.58 times it): the numbers here are the quiet run's. In that quiet run `tile_yield/hit` read 10 ns, twice its budget, just after Criterion's 5.9 ns for the same read; the `stats` part run again at once read 5 ns, and its numbers are those above: a measure of a few nanoseconds moves with the core's clock, and one reading over the hard limit is a reason to run the part again before believing it.
- **Gates** (at the fix round's head). 1: every threshold holds with the hard gate (above); the measures over their budget within the hard limit are accepted for Phase 1 (§10's decisions), `astar_small_30` timed cold. 2: every corpus state's pass round at least 146x Python's; the small t280 rounds at most 15.2 ms (backstop 150 ms), the large at most 31.2 ms (400 ms). 3: `snapshot/gargantuan` 979 µs (target 10 ms). 4: `view/god` 2.28 ms on the large t280 state (20 ms), 10.1 ms on the gargantuan one (12 ms target). 5: re-run in WSL at the fix round's head: the unchanged suite against its saved baseline passes (10 benchmarks, none regressed, within 0.01%), and the synthetic +10% fails (8 kernels +9.1 to +10.0%, "Regressed", exit 3). 6: replaced, by §10's decision, with the regression guard, which holds (below).
- **Gate 6, memo redundancy.** The soak (`tests/engine/memos.rs`, feature `stats`): `RandomAgent` games on a duel of 200 rounds, a small map of 120 and a standard one of 60, then five rounds passed from each committed fixture, checks off. Of each memo's recomputes, the share that came out as it was: only `owned_tiles` (0% of 381) holds the 5% among the memos the soak recomputes 200 times or more (the religion grid, 0% of 52, is below that); the others do not: `with_spread` 99.6% of 271 (validated 4,130 times; the as-built note of the implementation read the validations as the recomputes and gave 1.4%, wrongly), `with_near` 70.6% of 177, `tile_yield_full` 99.6% of 21,187, `path::memo::civ_parts` 100% of 22,353, `city_base` 98.6% of 15,243, `city_mods` 99.7%, `city_parts` 91.6%, `city_stats` 88.4%, `happiness` 98.5%, `deficit` 100%, `connectivity` 99.0%, `civ_stats` 82.4%, `supply` 98.4%, `majority` 99.7% of 13,859 and `aura_units` 99.5% of 1,242 (both this package's), `major_religion` 99.8%, `upkeep` 99.1%, `zoc` 78.6%, `city_local_full` 97.4%, `era` 97.4%, `civ_requirements` 92.7%, `civ_index` 74.3%, `buildable` 65.2%, `danger` 52.8%, `civ_index_full` 43.8%, `reach` 100%. That is Python's 74-99.5% again, for another reason: a memo recomputes when a revision its inputs name moves, and the revisions are coarse (a conditional on its city validates against all the city's revisions, its stored food and production and its religious pressure among them, which move every turn; a civilization's `units` moves when any of its units moves), while the values rarely change. Early cutoff (§6.4) keeps each recompute from reaching its readers, and every recompute is cheap (the pass rounds above). As §10 records, Phase 1 gates the guard against over-bumping instead: `no_memo_recomputes_more_than_the_soak_recorded` holds each memo to its recorded recomputes plus a quarter, and a memo not recorded to the 5%; it passes at the fix round's head, and the 5% test is kept as an ignored diagnostic (`--run-ignored all` prints the table).
- **Deferred.** The small A* cold is dominated by the late fixture's worker paths round bays (288 tiles closed for a 39-tile path, against 148 and 174 on the other two maps): a bound that knew the coast would close fewer, at the cost of a flood per target. The memos' finer revisions (gate 6) are Phase 2's if it wants them. `nightly.yml`'s weekly criterion trend is 1e-02's.
- **Counts** at this package's head (after its fix round): nextest 1097 passed, 2 skipped (`--workspace --all-features`, the corpus on; the skipped are the ignored 5% soak test and a map-generation report); doctests pass; Python 608 OK (2 skipped); the 15 golden sets unchanged under `ci` and `release`; refcheck clean on the 12 committed states and the corpus's 250, the ratchet holding every group at 0 (the same two stale entries); `cargo xtask check` 245 files, 0 `NotPorted`, 0 `Pending`; clippy clean for the workspace and the engine in each feature set; `cargo doc` with `-D warnings` clean.

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

**Phase 1 decisions taken after the design:**
62. **The performance gates of 1e-03** (its fix round, on the owner's priority: stability first, then speed). Accepted: gate 6's 5% bound on redundant recomputes is replaced by a guard on each memo's recompute count (fails above its recorded count plus a quarter), the 5% test kept as an ignored diagnostic; measures over their budget and within the 1.5x hard limit are accepted for Phase 1 with their numbers; A* is gated on cold searches, the warm one report-only. The reasons and the numbers are in §10 and "As built in 1e-03".

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
As built: nine barbarian and six city-state scripts pass on both engines; quests are placed with the test operation `add_quest` and completed, rewarded and dropped on both, while quest assignment, elections and successful coups are unit tests, their draws differing between the engines. See "As built in 1c-06" after §6.11.

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
As built: the six scripts `victory_*` pass on both engines; eliminations happen at once where Python checked, but the player whose turn it is is eliminated as the round ends; a stats row carries every key Python's rows had (18, with baseline.py's 8 and balance.py's 13). See "As built in 1c-08" after §6.11.

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
As built: every setup stage ported (`maps.prepare` as the stage `prepare_map`); the drive's five stops, the answers put to drivers and a saved mark for a stop inside a turn; `Game::debug`; `set_civ_name`, `write_notes` and `log_thought` in `game::meta`; the test operations `drive`, `debug` and `set_difficulty` on both engines; ten scripts; `newgame.json` (twelve games) and `turns.json` blessed. See "As built in 1c-09" after §6.14.

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
As built: `RandomAgent` takes all 40 action tools (with `untimely` for the actions a short game never reaches, and settlers sent to good sites); `citar_testkit::games` plays whole games with a hook after every round; `tests/engine/whole_game.rs` holds gates 1 to 3, the early P8 with a noisy agent (noise before every action, and in a game saved every round too), and the kitchen sink played with every check and a save every round; `load.json`, `pass.json` and `random.json` blessed; `forbid_all` on, with `view` and `briefing` no longer `Pending` markers. See "As built in 1c-10" after §9.6.

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
As built: `api::tools::registry` holds the 61 tools (the argument specs moved into it), `Game::execute` and `execute_query` run them in Python's order, three queries are answered and the other 18 check their arguments and refuse as not ported until 1d-02 and 1d-03; `game::lookup` is the tools' one set of helpers; refusals keep the P5 rules, checked on every call of a battery over the committed fixtures and on every refusal a `RandomAgent` meets; `tool_errors` is enforced, clean on the corpus too. See "As built in 1d-01" after §8.5.

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
As built: `api::views` holds the builders, the client view (`Game::view_json`, typed as `Game::client_view`), the alerts, `empire_summary`, `standings`, `path_preview` and `replay_data` in both formats; the fifteen view queries answer; `views` is enforced, clean on the corpus, and the query tools match Python's answers on the corpus but for listed fixes; a player's view of the small t280 fixture takes about 0.6 ms and a spectator's of the gargantuan state about 11 ms. See "As built in 1d-02" after §8.5.

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
As built: `api::briefing` (the briefing, the turn's progress, the ASCII map), `api::text`, `api::views::rules`, the map editor's API in `api::maps` and the scenario editor's in `api::scenario`; `get_briefing`, `get_map` and `get_rules` answer, so no `NotPorted` marker remains; `briefing` is enforced, clean on the corpus, so every refcheck group is; seven scripts; the briefing about 0.43 ms. See "As built in 1d-03" after §8.5.

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

As built: the invariants, the cache oracle (now `game::derive::oracle`, which also checks the path cache) and every memo family's check are compiled under `cfg(any(test, debug_assertions, feature = "checks"))`, testkit turning `checks` on by default and bench building testkit without it and turning the checks off in every game it times; `game::seeded` plants five bugs in test builds; `citar_testkit::{spec, stability, chaos, fuzz}` give the `ActionSpec` strategy, the P1 to P8 runner, the chaos driver with its replays and the fuzz entry points; `tests/props/` holds P1 to P8 on games, gate 1 and the pure properties; `cargo chaos` and `crates/citar-engine/fuzz`; rust.yml adds a chaos step on the three OSes. See "As built in 1e-01" after §9.5.

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
As built: reports hash every row and `golden diff` names where two part; `long.json` (whole games to 330 turns on every map size, two on the kitchen sink, the late fixtures passed on) is the nightly run's; `golden check --states`, `golden dump` and an exact state diff give the divergence artifacts; `same_process_twice` compares two games in one process and a third on another thread, digests and every read; determinism.yml's compare job names the rows where targets part and diffs their states; `cargo soak` plays every map size to the turn limit with the invariants on and reports panics, violations, outliers and peak heap; nightly.yml runs the properties at 10,000 cases, chaos and a soak on three OSes, the long set on six targets and the criterion trend; the soak found a citizen settle with no fixed point on the kitchen sink, which now stops where a city comes back. Gates 1 to 3 and 5 hold locally, 1 and 2 on Windows x64, Linux x64 and linux-arm64 under qemu-user; 4 and the macOS targets of 1 are CI's. See "As built in 1e-02" after §9.6.

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

As built: `citar-bench`'s three suites (`kernels`, `turns`, `io`) pinned to core 0, `thresholds.toml` hard at 1.5 times each budget, `cargo xtask perf`, `turn_timing.py` and `python-turns.json`, and the gungraun gate in `rust.yml`; release without overflow checks (5-9% of a pass round), `ci` with them; the advisor, barbarians, A*, combat and religion tuned. The fix round: the what-if's reuse of a city's base made safe (the blocker), the connectivity memo's water kept fresh, perfgate refusing stale numbers, A* gated and tuned cold. Gates 1 to 5 pass, one measure over its budget and within the hard limit (A* small, each search cold, 1.27 times); gate 6 is replaced by the regression guard on the owner's priority (§10's decisions), and the guard holds. See "As built in 1e-03" after §10.

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

As built: the strict run over the 262 states is clean (0 unexplained, 0 stale, every ratchet count 0) once the two stale entries of 1a-07, which no state can show, moved to `tests/rules/intended.toml` with a test that shows each; every entry of both lists is cited and every citation names one, both checked by a test; `cargo refcheck changelog --write` keeps CHANGELOG.md's 98 rule fixes; `ErrCode::NotPorted` is gone and `cargo xtask check` forbids any marker; ARCHITECTURE and MODDING describe the Rust engine. Gates 1 to 3 hold, gate 2 locally at the head (Windows and Linux) and in CI at its base on macOS and real linux-arm64 too; the PR text is drafted for the merge. The fix round: the docs' oracle and settle claims corrected; every entry of the scripts' list named by the script or test that shows it, checked by a test (thirteen older ones spared on a list that only shrinks); a citizen placement the soak found was not a fixed point fixed (`citizens-shed-locks-at-once`, 99 rule fixes), which moved one round of the long golden set; every gate again at its head, the goldens on Windows, Linux and emulated linux-arm64. See "As built in 1e-04" after §1.3.

---

## Phase 2: the bot, the runner, the bindings and the swap

**Status.** Final design, 2026-10-02, by the lead architect, for Phase 2 of `ops/plan-0.1.6.md` §7, after one review round (P2.14 logs what changed and what was rejected). Phase 1 is complete at `8f84421`. Sections are numbered P2.x so they cannot be confused with Phase 1's. P2.13 summarises the work breakdown, which is kept as the package list beside this section.

**Sources:**
- `ops/plan-0.1.6.md` §§1, 3, 4, 5, 6.8 and 7, with the 2026-09-23 revision: no bit parity, stability and speed first, fix Python bugs instead of porting them;
- `ops/STATUS.md` (Phase 1 results and the exit verifier's Phase 2 notes);
- this document's §§1.2, 2.1-2.7, 4.5, 4.9-4.11, 6.12, 7.1, 8, 9.8 and 10, and the as-built notes of 1b-03, 1c-05, 1c-07, 1c-09, 1e-03 and 1e-04;
- `citar/engine_api.py`, `citar/bots/{basic,profiles,headless,idle}.py`, `citar/lab.py`, `citar/sim.py`, `citar/balance.py`, `citar/server/{session,app}.py`, `citar/agents/bot_agent.py`;
- `ops/review-0.1.6/{bots,server-runtime,agents-llm,build-ci-tests}.json`;
- `scripts/refcheck/{baseline,common,summarize}.py` and `refcheck/baseline/*.jsonl`;
- `.github/workflows/*.yml`, `Dockerfile`, `.dockerignore`, `.gitignore`, `pyproject.toml`.

**Facts the design leans on,** checked against the tree at `8f84421`:
- `citar/bots/basic.py` is 2,771 lines. `PARAM_GROUPS` holds 17 `(name, help, specs)` groups and 375 parameters: 222 int, 127 float, 9 choice, 8 bool, 7 order, 2 list. Two defaults are `null`: `small_city_focus` (a choice) and `policy_order_aggressive` (an order meaning "the default order").
- `profiles.schema` returns `{engine, groups}`, the Bots page's shape. `clean_params` (profiles.py:185-225) accepts integral floats and numeric strings for an int and keeps a fractional value as a float.
- The bot draws from two Mersenne Twister streams: `self.rng` (tech noise 906; ranged or melee 1336) and `self.rng_diplo` (spies 1057, peace 2427, friendship 2438, war preparation 2449). Its memory is eleven dicts (684-694), lost on every save.
- In the server every bot action goes through `session.call_tool` (bot_agent.py:27-33), which runs `_after_action` (session.py:200-215): the version, metrics turns, negotiation responders, the `turn` and `update` broadcasts, the autosave.
- `engine_api.__all__` has 38 names. `bot_set_diplomacy` changes the bot in place (test_engine_api.py:151-160); `run_game` honours `raise_errors` (sim.py:48), `labels` and `traceback_limit` (balance.py:86); `lab.play` reads `bots[pid].aggression`.
- `Game::drive`, `SeatDriver`, `DriverMemory` (at most 4 MiB), `DriverOutcome::Deferred` and the five `Stop`s exist (`game/turn/drive.rs`); `with_driver` is private. Nothing asserts `Game: Send`.
- `take_journal_chunk` moves the game's journal cursor when it returns a chunk (core.rs:255-259).
- A poisoned game refuses with `ActionError { code: ErrCode::Poisoned }` (core.rs:308). Nothing in `citar/server` or `citar/agents` handles a crash, and `_drive`'s error handler calls `game.emit`, which a poisoned game refuses.
- `BUILD_ID` is `CITAR_BUILD_ID` at compile time, else `"dev"` (rules/source.rs:60).
- The advisor reads no bot memory: it assumes "a bot that asks only for production" at advisor.rs:444 (war preparation), 728 (sites given up), 925 (garrisons) and 1363 (boat turns), and has no escort override (basic.py:1249, 1415).
- `Purpose::BotBase = 0x1000_0000` is reserved. Refcheck's `deal_checks` holds Python's `bot_value` for every sampled deal, compared only with `--with-bot`.
- The Python baselines are git-ignored and exist only on the laptop: small, 60 games (seeds 5000-5059; 55 to turn 330, 5 Scientific victories); std-large, 24 games (14 standard, 10 large); gargantuan, 2 games (8,364 s and 8,368 s); a smoke file.
- Per-game spreads are small: on small maps techs@100 is 19.24 (sd 0.73), policies@100 8.00 (sd 0.40), score@100 229 (sd 22.5), cities@100 3.26 (sd 0.47). 33 of 240 civilizations had fewer than three cities at turn 100.
- `.dockerignore` excludes `dist`; `.gitignore` covers `*.so` but not `*.pyd`; `docs.yml` runs `pip install -e ".[dev]"`. The Python suite is 608 tests, 2 skipped; `tests/rules/` holds 144 script files.

### P2.1 Goals, non-goals and exit

#### P2.1.1 What Phase 2 delivers

1. **The live bot in Rust** (`citar-bot`). `basic.py` becomes the compiled version `basic-1`, with 373 of its 375 parameters (two are dropped, P2.3.9) and its profiles. It keeps the per-category diplomacy switches and `advice()` that Phase 0 added. `idle.py` becomes the version `idle`.
2. **A headless runner** (`citar-sim`): `run_game` and the baseline JSONL writer.
3. **Saves** (`citar-store`): a zstd container, plus an append-only journal file for replay frames and history, written off the session lock.
4. **Python bindings** (`citar-py`): `citar._engine`, backend 2 of `citar/engine_api.py`, releasing the GIL on every heavy call.
5. **The swap.** The server, agents, MCP, probes, benchmarks, the lab and the Bots page run on Rust, and the Python suite passes on it.
6. **The removal.** The Python engine and the 18 frozen bots are archived on a tag and deleted. Bot versions and a new ladder replace freezing.
7. **The Phase 2 speed floors.** A small 4-bot Quick game in 25 s or less; gargantuan at least 30x faster than Python's 8,364 s, which is 279 s or less.

**Owner decisions that bind this phase:**
- Port only the live bot. Archive the 18 frozen bots and the old ratings.
- Bot versions are named and compiled in. Profiles change parameters only, and are pinned to engine build plus bot version.
- One maturin-built `citar` package with abi3-py311 wheels.
- Saves are zstd, with replay frames in an append-only side file written off the session lock.
- The Python engine is removed after the swap and archived on a tag.
- Hybrid seats come in Phase 3, but the Rust bot must have the switches and `advice()`.
- At most two packages in parallel. Each package fits one agent session and is verifiable when done.

#### P2.1.2 Non-goals

- **Phase 3:** the hybrid agent, its diplomat prompt and triggers; the trade-chat UI and rule T3's lobby timeouts; pooled prompts; `replay.js` reading the `Delta` format (the `Full` replay stays); ETags on views and trade options leaving the view (plan 6.8). The engine already stops for `HybridDiplomat` and honours `Deferred` (1c-09). Phase 2 keeps the bot side ready: switches, deferred answers and advice.
- **Phase 4:** the Rust helper, CPU jobs, signatures and trust, and a Python-free lab job. `citar-sim` and `citar-store` are libraries so the helper can link them.
- **Phase 5:** the release wheel matrix, multi-arch Docker, installers, Scoop, winget, Homebrew and the VPS.
- **Game-for-game parity with the Python bot.** The comparison is statistical (P2.4.4) and by deterministic sub-decisions (P2.3.11).
- **Memo redundancy under 5%.** §10's decision stands: the soak guards each memo's recompute count. It is taken up only if a speed floor fails.
- **Threads inside a game.** Throughput comes from games side by side, now on several cores because the GIL is released.

#### P2.1.3 Exit criteria

| Criterion | Measured by |
|---|---|
| The bot is ported | Every `basic.py` decision has a Rust home (P2.3.10). The bot scripts pass on Rust, and passed on Python while it existed. `cargo refcheck run --with-bot --strict` is clean on the 262 states. The `bot_decisions` agreement floors hold (P2.3.11). |
| Whole bot games alike | Gates G1-G4 of P2.4.4 pass on every stratum: Rust small ×120 and standard/large ×50 against the committed Python baselines. |
| No panics, no corruption | Bot soak: 1,000 small and 100 larger bot games with the invariants on, 0 failures. Mixed bot/random chaos: 20 minutes per OS, 0 failures. The bot fixture sweep: one bot round on each of the 262 states, clean. |
| Determinism | The `bot` golden set is identical on the 6 determinism targets. Save and load at every round of a bot game equals the uninterrupted chain. |
| Speed | `game/bot_small_330` median ≤ 24 s (floor 25 s, target 5 s). `game/bot_gargantuan_330` ≤ 270 s (floor 279 s, target 90 s). God view ≤ 20 ms. Save lock time ≤ 10 ms budget, 100 ms plan limit. |
| The server runs on Rust | `citar/engine` and the frozen bots are deleted and tagged. The full Python suite, ruff, the route audit, the link check and `mkdocs --strict` are green. The server soak in 2-13 is clean: bot turns broadcast, autosave keeps up, a restart resumes, the replay loads. |
| Packaging for now | `pip install -e .` builds the extension. CI builds an abi3 wheel per OS, installs a manylinux wheel in a clean venv, and the Docker job runs from it. |

### P2.2 Crates and what the checks add

| Crate | Kind | Depends on (workspace) | Purpose |
|---|---|---|---|
| `citar-bot` | lib | `citar-engine` | The bot versions (`basic-1`, `idle`) as `SeatDriver`s; parameters, memory, streams, advice and deal valuation |
| `citar-store` | lib | none | zstd, the `.citar` v2 container, journal framing and torn-tail recovery |
| `citar-sim` | lib + bin | `citar-engine`, `citar-bot` | The headless runner, `run_game`, the baseline writer, the CLI |
| `citar-py` | cdylib `citar._engine` | engine, bot, sim, store | PyO3 0.29, abi3-py311; `[lib] test = false, doctest = false` (tested through Python) |

- **Lints.** `citar-bot` uses the strict root `clippy.toml`: its decisions must be as deterministic as the engine's (no hash iteration, no std transcendental functions, stable sorts). `citar-sim`, `citar-store` and `citar-py` are host crates with relaxed files, as testkit's binaries are. If PyO3's macro expansion trips the workspace's `unsafe_code = "deny"`, `citar-py`'s `lib.rs` carries one `#![allow(unsafe_code)]` with its reason, and xtask refuses any hand-written `unsafe` block in the crate.
- **New `cargo xtask check` rules** (written in 2-00a):
  1. **The crate graph.** Each crate depends only on the crates in the table. `citar-testkit`, `citar-refcheck` and `citar-bench` may add `citar-bot`; bench may add `citar-sim`.
  2. **`&mut Game` stays in one file.** In `crates/citar-bot/src/**` the text `&mut Game` may appear only in `src/driver.rs`. Every other bot file reads `&Game` and acts through `Turn` (P2.3.6), so the bot cannot reach the engine's public `&mut Game` functions (`relations::set_war`, `execute_deal`, ...) except through `Game::act`.
  3. **Shipped builds stay clean.** `citar-bot`, `citar-sim` and `citar-store` turn on neither `legacy` nor `test-ops`. `citar-py`'s `test-ops` only forwards to the engine's. `pyproject.toml`'s `[tool.maturin] features` must not list `test-ops` (from 2-06b).
  4. **`crates/citar-bot/src/params/gen.rs` is up to date** with its schema (`cargo xtask gen-params`, from 2-01a), as `gen-uniques` is checked today.
- **`Game: Send`** gets a compile-time assertion beside `SeatDriver`, since the bindings hold games in a `Mutex` inside `Python::detach`.

#### P2.2.1 Build identity

The fingerprints, the lab's results, the baseline writer's "refuse another build's file" and Phase 4's proof of work all need an id that changes exactly when behaviour can. `CITAR_BUILD_ID` is unset in `pip install -e .`, CI wheels, Docker and any sdist, so it reads `dev` almost everywhere; `git describe --dirty` would move on docs-only commits. So the id comes from content:
- **`crates/citar-engine/build.rs`** hashes (blake3) its `src/**/*.rs` in path order with CRLF normalised to LF, the embedded data files, the package version and the `Cargo.lock` versions of its normal dependencies, and sets `CITAR_ENGINE_CODE` (16 hex). `BUILD_ID` becomes that code. The save's `engine` string (`0.1.6+<BUILD_ID>`) is not part of the digest, which covers the ruleset and the state, so no golden digest moves (2-00a's gate checks it).
- **`crates/citar-bot/build.rs`** does the same over its `src/**` and `params/*.json`: `CITAR_BOT_CODE`.
- **`citar_bot::build_id(rules) = hex(blake3("CITAR-BUILD" ‖ ENGINE_CODE ‖ BOT_CODE ‖ RulesetId))[..12]`**, a function because a runtime ruleset (P2.8.4) has its own id.
- **`CITAR_BUILD_ID` becomes `BUILD_LABEL`**, display only (`git describe`, else `unknown`). `build_info()` reports version, build id, label, `RulesetId` and both codes.
- A test computes the codes from an LF and a CRLF copy of the sources and gets the same ids. Any source change in either crate moves the id; a commit outside them does not.

**Changes to the Phase 1 text,** recorded in P2.12:
- §2.1 said the bot "talks only to `game::query` and `Action`". It reads any public function that takes `&Game` and writes only through `Game::act`.
- §7.1 reserved "`BotBase` and up, one per decision type". There is one purpose, `BotBase`, and the decision type is the first key word.
- §4.5 left the bytes of `DriverMemory` open. They are JSON.
- §8.2 mapped `play_bot_turn` and `bot_respond` onto `drive`. They map onto `drive` and a new `Game::answer`.
- `BUILD_ID` is a content hash, not an environment variable.

**As built in 2-00a: the foundation** (P2.2, P2.2.1, P2.3.1, P2.4.1-P2.4.3, P2.5, P2.6.1, P2.6.8; Appendix C). The four crates are in the workspace with their public signatures, the engine has `Game::answer` and its content code, `cargo xtask check` has the new rules, CI covers the new crates, and the Python baselines are committed.
- **Crates and dependencies.** `citar-bot`, `citar-store`, `citar-sim` and `citar-py` are members, with workspace entries for the three libraries. zstd is 0.13 with no default features (0.13 is MIT; 0.14 moved to BSD-3-Clause; neither legacy frames nor dictionaries are used). PyO3 is 0.29 with `abi3-py311` alone: 0.29 links Windows through raw-dylib and deprecated `generate-import-lib`, so the scope's `generate-import-lib` is not turned on. The extension built and imported on Windows under Python 3.11, 3.12 and 3.14 from one abi3 build. PyO3's macro expansion does not trip `unsafe_code = "deny"`, so `citar-py` carries no `#![allow(unsafe_code)]`. citar-sim also uses serde and thiserror. testkit and refcheck depend on citar-bot, bench on citar-bot, citar-sim and citar-store. The fuzz crate's own lock file gains citar-bot (testkit's new dependency).
- **The crate graph and the other rules** (`xtask/src/check/{graph,sources,features}.rs`). Each member is held, in every kind of dependency, to its row: engine none; bot the engine; store none; sim the engine and the bot; py all four; testkit and refcheck the engine and the bot; bench those, testkit, sim and store; xtask none. Bench's store is not in P2.2's table: 2-02 times the container and the journal in bench's io suite, so the graph allows it and bench's manifest has it already. A new member without a row is a finding. In `crates/citar-bot/src` the type `&mut Game` (any lifetime, any path, comments and strings aside) is refused outside `driver.rs`, and so are the spellings that would carry one out of the driver without writing it: a rename of `Game` (`use ...::Game as G`; `<Game as Trait>` is a qualified path and allowed), a type alias or associated type that is `Game` (behind `&` it is a shared reference and allowed), an impl for `Game` (whose `&mut self` is one), and a bound that lends one (`Target = Game`, `AsMut<Game>`, `BorrowMut<Game>`). The bot, the store and the runner never turn on the engine's `test-ops`; citar-py only through its own `test-ops` feature, which must be exactly the forward to the engine's and which no other feature of citar-py may turn on; an engine feature that reaches `legacy` stays the legacy rule's. No `unsafe` token in `crates/citar-py/src`. Findings about manifests name them. Gate 2's planted violations were also run against the real workspace: a citar-store dependency on citar-bot, `test-ops` in citar-bot's engine dependency, a `&mut Game` in `idle.rs` and an `unsafe` block in citar-py each gave one finding naming the file. citar-sim depending on citar-py, the scope's example, is a dependency cycle that cargo itself refuses before xtask runs, so it is a unit test on synthetic metadata.
- **The content codes** (P2.2.1). `crates/citar-engine/content_code.rs` holds the hashing, and the engine's and the bot's `build.rs` each include their own copy (`crates/citar-bot/content_code.rs`; a bot test holds the two equal), so either crate builds from its own folder. (Not `build/code.rs`: the root `.gitignore` ignores every `build/` folder.) The code is the first 8 bytes of blake3 over a domain tag (`citar-engine-code-v1`, `citar-bot-code-v1`), the package version, every input file by name in name order with CRLF turned to LF, and the sorted `name version` of every `Cargo.lock` package the crate's normal dependencies reach. The engine's inputs are `src/**/*.rs` and the ruleset files it embeds (`citar/data/ruleset/*.json`, `custom/*.json`, `game.json`); the bot's are `src/**/*.rs` and `params/*.json`. "The locked versions of its normal dependencies" is taken as everything they reach, not only the direct ones: a ryu or itoa bump under serde_json can change a save's bytes; dev and build dependencies count only where a normal dependency reaches them. A workspace package a crate depends on (the bot's engine) is listed but not followed: the lock lists a workspace member's dev-dependencies beside its normal ones (the engine's proptest and the rand family under it), and its own code covers what it links, so a dev-only bump moves neither code. `BUILD_ID` is `ENGINE_CODE`, so a save's `engine` reads `0.1.5+<16 hex>` until the release bumps the version; no golden digest moved. `BUILD_LABEL` is `CITAR_BUILD_ID` at compile time, else `unknown`. `citar_bot::build_id(rules)` is the first 6 bytes, as 12 hex digits, of blake3 over `CITAR-BUILD`, the two codes as their hex text, then the ruleset's 32 id bytes; `build_info(rules)` gives version, build id, label, ruleset id and both codes, and the extension's `build_info()` returns it as JSON bytes. The engine's build script reruns when `src/`, `content_code.rs`, its manifest, `citar/data/` or `Cargo.lock` change, the bot's when anything in its folder or `Cargo.lock` does, so a schema added under `params/` is seen. `cargo xtask check` allows the engine exactly this build script, with blake3 its only build dependency.
- **`Game::answer`** (`game/turn/drive.rs`) refuses, besides the scope's three cases, a game that is over (`ErrCode::GameOver`), and returns the answer's events. Beside `SeatDriver` are compile-time proofs that a `Game` is `Send` and a `Mutex<Game>` is `Sync`. The facts above said nothing asserted `Game: Send`; `lib.rs` already did, with `!Sync`, since 1b-01.
- **citar-bot.** `BotSpec::new` reads a NaN on either side as no value (a NaN fixed aggression leaves the seat to decide; Python's `max(0.0, min(1.0, nan))` gave 1.0) and keeps the fixed aggression held to 0..1 as it plays, so two profiles that play alike share a fingerprint. Real in the skeleton: `Owners` (with `set_diplomacy`'s messages and `owns_negotiation`), `Memory` and its codec (kind 1, version 1; another kind or version, or bytes that do not decode, start fresh), the five `Stream` words, `versions()` (`basic-1` latest with memory kind 1, `idle` with none; `basic` resolves to the latest), `fingerprint` (blake3 of `CITAR-BOT`, the build id, the version, the overrides' canonical JSON and the fixed aggression to three decimals or `seat`, each length-prefixed; 12 hex digits), `build_id`, `Refusals` and `Turn`. Stubs: `Params` (`src/params/gen.rs`, a placeholder; `gen` is a reserved word in edition 2024, so the module is `generated` with `#[path = "gen.rs"]`), `Resolved`, `clean` (idle ignores everything as Python did; no overrides are none; any `basic-1` override is refused until 2-01a), `schema` (idle's is `{"engine":"idle","groups":[]}`; `basic-1`'s is `BotError::NotYet` until 2-00b's file and 2-01a), `advice` (empty) and `evaluate` (0). The stub `Bot` plays every version as idle (founds its capital, rejects every negotiation with Python's line), and `basic-1` defers a negotiation the seat's model owns. `params/basic-1.json` is 2-00b's and was not created here.
- **citar-store**: the container's `Header`, `SessionRef`, `ChainRef`, `BodyParts`, `Container`, the journal's `JournalRef`, `Recovered`, `JournalWriter`, `RecordKind`, `Codec`, and `StoreError`; every function returns `StoreError::NotYet`.
- **citar-sim**: `Runner::step` is real (one drive with a seat limit of 1; a panic caught with `catch_unwind`, the game poisoned, a `Crash` recorded with the seat's label, or returned with `raise_errors`; a time budget checked between steps). A 3-round duel takes 6 steps, the round hook firing in steps 2, 4 and 6; a recorded crash reads as `GameOver`. `run_game` and `Runner::result` return `SimError::NotYet`. `BaselineLine` (an untagged game-or-crash line, unknown keys refused; a crash line's `seconds` is optional, since `baseline.py` writes a dead or stalled worker's line from the parent without it) reads all 88 committed Python lines back to equal JSON values, which is 2-04's gate 1 already passing. The CLI's `play` and `baseline` exit 2 until 2-04.
- **citar-py**: `citar._engine` with `build_info()`, `HAS_TEST_OPS`, and a frozen `Game` (`Mutex<citar_engine::game::Game>`) with `Game.new(config_json)` and `turn()`, each inside `Python::detach`, as is `build_info()`, whose first call compiles the embedded ruleset; the library is named `_engine`, a cdylib with `test = false, doctest = false`. An extension never links libpython: `.cargo/config.toml` sets `PYO3_BUILD_EXTENSION_MODULE` (PyO3 0.29's replacement for the deprecated `extension-module` feature, and what maturin sets), and the crate's build script adds macOS's `-undefined dynamic_lookup` through `pyo3-build-config`, so a plain `cargo build --workspace` links it on every OS without Python's development package.
- **xtask, bench, testkit.** `cargo xtask gen-params` is registered (`xtask/src/gen_params.rs` says the generator comes in 2-01a and writes nothing). `cargo xtask perf --suite games` runs the new `games` bench, which writes an empty `perf/games.json` until 2-04; a run without `--suite` leaves the games suite out and checks its file only when an earlier run left one; `--suite <name>` checks only that suite's budgets (the pass rounds only for `turns`), so `--suite games` needs no file of the other suites; `--check` alone checks every suite; and `thresholds.toml` budgets may be `report_only`, printed against their budget without failing anything, for 2-04's rows until 2-07. testkit's `tests/bot.rs` holds the skeleton's test: bots found their capitals and reject an offer through `drive`, and a model-owned negotiation is deferred through `Game::answer`.
- **CI** (rust.yml): `actions/setup-python` in the lint and test jobs; nextest and the doctests with `--exclude citar-py`; clippy and `cargo doc` cover it. At the merge, each test job gained a last step, `cargo build --workspace --all-features --locked --profile ci`, so citar-py links on Linux, Windows and macOS (gate 1's `cargo build --workspace` on every job; the tests' profile and features, so it adds little more than PyO3 and the extension). determinism.yml and nightly.yml build single packages and need nothing new; nightly's benchmarks will run the games suite with the others once 2-04 fills it.
- **Data.** The four baselines are under `refcheck/baseline/python/`, committed byte for byte with their CRLF line endings (`-text`), sha256 in `refcheck/README.md`; `refcheck/.gitignore` keeps out every other file under `baseline/`, and `.gitignore` the extension (`citar/_engine*.pyd`, `citar/_engine*.so`). The originals stay in the main checkout, now ignored.
- **Gates, run locally** (the GitHub jobs need a push, which a worktree does not make). (1) Windows: `cargo build --workspace` (citar-py included, dev and ci profiles), nextest `--workspace --exclude citar-py --all-features` in the ci profile with the corpus (1,185 passed, 2 skipped), the doctests, clippy `-D warnings` over the workspace and the engine alone in each of rust.yml's six feature sets and in release, `cargo doc -D warnings`, `cargo fmt --check`, `cargo xtask check` and the fuzz crate's `--locked` check. Linux (WSL Ubuntu 24.04, a fresh clone, no corpus): rust.yml's lint and test steps, the same commands, all green (nextest 1,185 passed, 2 skipped). (Here a plain `cargo build -p citar-py` on Linux failed to find `-lpython3.12` without Python's development library; the fix round below made it link.) rust.yml never links citar-py. (2) The xtask tests above. (3) `engine::drive::answer_*`: an answer through a test driver with its memory written back and the game settled (invariants and caches clean), a deferral, a negotiation waiting on another seat or none, from inside a driver, and on a poisoned game. (4) The content code's tests in both crates (the real tree, an LF and a CRLF copy, a source edit, a docs edit); `cargo golden check` gives all 15 sets `ok` in the ci and release profiles. (5) The blobs' sha256 equal the originals'; `git check-ignore` reports `citar/_engine.cp311-win_amd64.pyd` and `citar/_engine.abi3.so`. (6) The Python suite: 608 tests OK, 2 skipped; ruff and the strict link check clean.
- **Fix round.** The review's seven findings were all valid and are fixed, each with a test that fails without it:
  1. The content code followed the bot's `citar-engine` into its lock entry, which lists the engine's dev-dependencies, so `CITAR_BOT_CODE` covered proptest and the rand family under it, and a dev-only bump would have moved every build id and fingerprint. A workspace package is now a leaf of the walk (above). The tests are a synthetic workspace package whose dev-dependency is bumped, and the real lock with proptest bumped.
  2. The `&mut Game` rule now also refuses a rename, an alias, an impl for `Game` and a lending bound (above). Planted in `idle.rs`, the four forms gave four findings, each naming the file and the line.
  3. A dead or stalled worker's crash line, which has no `seconds`, now reads and writes back to the same value.
  4. A NaN fixed aggression no longer throws away the seat's value, and the fingerprint hashes the fixed value as it plays.
  5. A plain `cargo build` now links citar-py without libpython, on Linux and (by its build script) macOS.
  6. `cargo xtask perf --suite games` checks only the games budgets. In this worktree's target directory, which holds `games.json` alone, it exits 0, and so does `--suite games --check`.
  7. `build_info()` now runs with the GIL released, and another Python thread kept running through its first call.

  Every gate was run again at the head:
  - **Windows, with the corpus:** fmt, `cargo build --workspace` (dev and ci), clippy over the workspace and the engine alone (its six feature sets and release), `cargo doc`, `cargo xtask check`, the fuzz crate's `--locked` check, nextest (1,192 passed, 2 skipped), the doctests, and `cargo golden check` (all 15 sets `ok` in ci and in release, the same hashes).
  - **Linux** (WSL; the branch fetched from a bundle; no corpus and no Python development library): fmt, clippy, doc, xtask check, a plain `cargo build --workspace` and `cargo build -p citar-py`, nextest (1,192 passed, 2 skipped), the doctests and the fuzz check. The extension's `NEEDED` entries are libgcc_s, libc and ld-linux, with no libpython, and it imports under the system Python 3.12.
  - **Build ids:** Windows and Linux give the same build id and the same two codes.
  - **Python:** the suite gives 608 OK with 2 skipped; ruff and the strict link check are clean.
  - **Not run:** the macOS link of citar-py, which needs CI.

### P2.3 citar-bot

#### P2.3.1 Shape

```
crates/citar-bot/
  build.rs                CITAR_BOT_CODE (P2.2.1)
  params/basic-1.json     the parameter schema of version basic-1: the Bots page's, profiles.py's and the bot's
  src/lib.rs              BotSpec, Tuning, Bot, BotError; versions(), schema(), clean(), fingerprint(), build_id(), advice(), evaluate()
  src/versions.rs         VERSIONS: basic-1 (memory kind 1), idle; LATEST = "basic-1"
  src/params/             schema.rs (the JSON), gen.rs (GENERATED: Params), clean.rs, resolve.rs (names -> ids)
  src/owners.rs           Owners: who decides each diplomacy category; owns(&Negotiation)
  src/memory.rs           Memory and its codec in DriverMemory
  src/stream.rs           Stream: the decision types' key words under Purpose::BotBase
  src/driver.rs           impl SeatDriver for Bot; Turn, the only holder of &mut Game
  src/basic1/             context, research, empire, faith, cities, gold, settlers, workers,
                          units/{mod, scouts, military, attack, war_plan, special, air, naval, promote},
                          diplomacy/{mod, war, trade, evaluate, respond, city_states, spies}, advice
  src/idle.rs             founds its capital, rejects every negotiation
```

```rust
pub struct BotSpec { pub version: VersionId, pub tuning: Arc<Tuning>, pub aggression: f64,
                     pub fixed_aggression: Option<f64>, pub owners: Owners }
pub struct Tuning { overrides: Overrides /* cleaned, sorted */, params: Params,
                    resolved: Mutex<SmallVec<[(RulesetId, Arc<Resolved>); 1]>> }
pub struct Bot { spec: Arc<BotSpec>, refusals: Refusals }        // impl SeatDriver; cheap to build
pub fn versions() -> &'static [Version];                 // id, label, description, latest, memory kind
pub fn schema(v: &str) -> Result<&'static str, BotError>; // the JSON, verbatim
pub fn clean(v: &str, overrides: &Value) -> Result<Overrides, ParamError>;
pub fn fingerprint(spec: &BotSpec, build_id: &str) -> String;
pub fn advice(g: &Game, pid: PlayerId, spec: &BotSpec, nid: Option<NegotiationId>) -> Advice;
pub fn evaluate(g: &Game, spec: &BotSpec, pid: PlayerId, other: PlayerId, give: &[DealItem], receive: &[DealItem]) -> f64;
```

- **A `Bot` holds no game state between calls.** What lasts lives in the seat's `DriverMemory`. What lasts a turn (the `Context`, one `Advisor`, caches) is local to `play_turn`. So a `Bot` can be rebuilt from its spec at any time, and any host can use it: the runner, the bindings, later the helper.
- **Ruleset resolution is cached in `Tuning`,** which every spec with the same parameters shares. Hosts build fresh `Bot`s for each drive, so the cache must outlive them; it holds one entry per ruleset the spec has met.
- **`aggression`** is what the seat plays with: the profile's, else the seat's, else 0.4, held to 0..1 as `BasicBot.__init__` held it. **`fixed_aggression`** is the profile's own value, `None` when the seat decides; it is what the fingerprint hashes (P2.8.6).
- **`seed` is gone** (P2.3.5).
- **2-00a writes these signatures with stub bodies,** so the bindings and the runner can be written in parallel with the port. The stub `Bot` plays as `idle`.

#### P2.3.2 Parameters: one JSON schema

- **The file.** `crates/citar-bot/params/basic-1.json` is `{"engine": "basic-1", "groups": [{"name", "help", "params": [spec, ...]}]}`, exactly the shape `/api/bots/schema` serves today, so it is served verbatim. Each spec has the keys of Python's specs: `key`, `type` (`int`, `float`, `bool`, `choice`, `order` or `list`), `default`, `label`, `help`; `min`, `max` and `unit` for numbers; `choices` for a choice (it may list `null`); `options` and `presets` for an order or a list.
- **Where it comes from.** `scripts/bots/export_params.py` (2-00b) writes it once from `PARAM_GROUPS`, without `site_cache_turns` and `bv_cache_turns`. From then on the file is the source of truth; the script goes in 2-12.
- **Types.** Python's `_n` typed a number by its default's literal (basic.py:71-74), so some multipliers and weights became `int` only because their default was written `3`. The file types 41 of them `float` (the exporter's `RETYPED`, defaults `3.0`): the eight with unit `x` (the 26 other multipliers are floats already) and the 37 `AdvisorParams` holds as `f64`. Each is used only in float arithmetic and comparisons, never in `//`, `range` or an index, so the Python bot plays the same either way, and `clean()` accepts `1.5` for them as Python did. The other 179 stay `int`: counts, turns, tiles, eras, health, amounts of gold, faith or influence, and integral scores and priorities (the research values, `settler_prio`, which `AdvisorParams` holds as `i32`, and `c_escort`); four are floor-divided (basic.py:1190, 1216, 1452, 2466). A test holds every `AdvisorParams` field to its schema type.
- **Three readers, one file:** the Bots page and `profiles.py` through `engine_api.bot_schema(version)`, and the bot through `include_str!` and the generated struct.
- **Generation.** `cargo xtask gen-params` writes `src/params/gen.rs`: `pub struct Params` with one field per key (`i32`, `f64`, `bool`, a generated enum per choice, `Option<_>` when `null` is a choice, and `NameList { Default, Preset(Box<str>), Names(Vec<Box<str>>) }` for order and list, where `null` and `"default"` both read as `Default`), `serde(deny_unknown_fields)` and no `default`s. Effective parameters are the schema's defaults overlaid with the overrides, deserialized into `Params`; a key missing from either side fails a test, so struct and schema cannot drift.
- **Cleaning.** `clean()` ports `profiles.clean_params`: unknown keys are refused with Python's message; bools accept `1`, `true`, `yes` and `on`; values equal to the default are dropped; keys are sorted. Two laxities are fixed:
  1. An `int` must be integral. `2`, `2.0` and `"2"` are accepted as 2; `2.5` is refused. Python kept it as a float, which turned `//` into float floor division.
  2. The names in an `order` or `list` must be among the spec's `options`, a preset, `"default"` or `null`. Python accepted any string.

  `min` and `max` stay hints for the editor, as in Python. **The two retired keys are dropped, whatever their value,** not refused: stored profiles carry them (eight archived profiles hold `"bv_cache_turns": 0`, which Python dropped as its default), and they tuned caches that no longer exist, so dropping them cannot change play, while refusing them would refuse the profile. Any other key basic-1 lacks is refused. 2-00b's clean-params table gives basic-1's answer for every case where it differs from Python's.
- **Resolution.** `Resolved::new(&Params, &'static Ruleset)` turns names into ids once per ruleset: policy branches, beliefs per kind, the pantheon order, the free great-person choices and the promotion lines (a `PromotionSet` of the promotions whose name starts with a listed prefix, as `startswith` did). Names a ruleset lacks (a mod) are skipped and counted.
- **The advisor's parameters.** The engine's `AdvisorParams` gains `Deserialize` and is read from the same effective map. A test holds `AdvisorParams` from `basic-1`'s defaults equal to `AdvisorParams::default()`, which 1c-07 checked against Python field by field.

#### P2.3.3 Typed context

`Context` is `BasicBot.context()` (basic.py:777-835) as a struct, built twice a turn as Python did: at the start, and again after the units move. Its fields: `cities`, `units`, `military` (not scouts), `hostile` (visible enemy military); `threat` and `near_enemies` per city; `gpt`, `hap`, `era`, `wars` (met living majors at war), `gold`, `supply`, `army_target`; `offense` (at war or preparing one), `exposed` (with `garrison_mode = exposed`), `lux_owned`, `pending_res`. Beside it, as functions over `&Game` and `&Context`: `city_defense`, `in_danger`, `needs_garrison` and `breaks_space_reserve` (836-871).

#### P2.3.4 Memory

```rust
#[derive(Serialize, Deserialize, Default, PartialEq)] #[serde(deny_unknown_fields)]
pub struct Memory {
    war_prep: Option<WarPrep>,              // _war_prep[pid]: player, since, target, rally
    war_plan: Option<WarPlan>,              // _war_plan[pid]: city tile, since, advance, checked, rally, siege_ready
    escorts: BTreeMap<UnitId, UnitId>,      // settler -> escort
    garrisons: BTreeMap<CityId, UnitId>,
    retreats: BTreeMap<TileIdx, u8>,        // _retreats[(pid, site)]
    bad_sites: BTreeMap<TileIdx, Turn>,     // _bad_sites[(pid, site)]
    need_escort: Option<TileIdx>,           // the city tile where a settler waits
    boat_turns: BTreeMap<CityId, Turn>,
}
```

- **Per seat.** Python keyed most dicts by player because one object could play several seats; a seat's memory needs no key.
- **Encoding.** `serde_json` of the struct (fields in declaration order, map keys in order), so the same memory always gives the same bytes, which matters because the engine digests them (§4.10). Stored as `DriverMemory { kind: 1, version: 1 }`.
- **A memory from elsewhere starts fresh.** A kind or version this driver does not know (a seat that changed bot version mid-game, an older schema) gives an empty memory.
- **Pruning.** At the start of each turn the bot drops entries naming units or cities that are gone, blacklisted sites older than `site_blacklist_turns` and boat turns older than `c_boat_retry_turns`, so memory stays far under `DriverMemory::MAX_LEN`.
- **Not kept:** `_sites_cache`, `_bv_cache` and `_space_res` (P2.3.9). `_space_res` is a ruleset fact and lives in `Resolved`.

#### P2.3.5 Random streams

Every draw is `Rng::keyed(seed, Purpose::BotBase, &[stream, pid, turn, ...])`, the game's seed, keyed by what it decides about.

| Word | Stream | Python draw | Key after `[stream, pid, turn]` |
|---|---|---|---|
| 1 | `Research` | `rng.uniform(1 - tech_noise, 1 + tech_noise)` (basic.py:906) | tech |
| 2 | `Spies` | `rng_diplo.random()` added to the tech lead (1057) | spy index, target player |
| 3 | `Peace` | `peace_offer_chance` (2427) | other player |
| 4 | `Friendship` | `friend_chance` (2438) | other player |
| 5 | `WarPrep` | `war_chance` (2449) | other player |

- **The sixth draw is the advisor's.** The ranged-or-melee pick (1336) is drawn by the advisor from `Purpose::Advisor` keyed `[city, turn]` (1c-07), shared with automatic production.
- **The seed is the game's.** `bot_instance(seed=...)` is accepted and ignored until 2-12 removes the argument. Python seeded each bot `seed * 101 + pid`; keying by the game's seed and the seat does the same job.
- **What this gives:** handing a category to a language model cannot move any other draw (plan 2.1); a game resumed from a save plays as the uninterrupted one; the order in which the bot asks never changes what it draws.
- **Pinned.** `tests/bot/streams.rs` pins the first draws of each stream. Keeping the words in citar-bot keeps bot vocabulary out of the engine and leaves `golden/rng.json` as it is.

#### P2.3.6 Acting: `Turn`

```rust
pub(crate) struct Turn<'g> { g: &'g mut Game, pid: PlayerId, refused: &'g mut Refusals }
impl Turn<'_> {
    pub fn game(&self) -> &Game;
    pub fn act(&mut self, a: Action) -> Option<Outcome>;   // Game::act; a refusal is None, counted by tool
}
```

- **It replaces Python's `ex`** (basic.py:697-707). The bot proposes freely and the rules refuse, so a refusal is normal: never an error, never a panic.
- **Counts.** `Bot::refusals()` gives accepted and refused actions by tool name. Tests read them, the soak prints them (a bot looping on a refused action shows), and the server records them per bot turn (P2.7.1).
- **Typed actions.** The bot uses the typed `Action` variants, never the JSON `execute`, and never ends a turn: `drive` ends it.

#### P2.3.7 Using the engine: the advisor, the what-if and reads

**Production is the advisor's** (`game::advisor`, 1c-07). Phase 2 adds the bot's memory as input:

```rust
pub struct BotFacts<'a> {
    pub preparing_war: bool,                    // `pid in _war_prep`: army target and offense (basic.py:806-810)
    pub garrisons: &'a [UnitId],                // `_is_garrison` in _pick_military (1332)
    pub need_escort: Option<TileIdx>,           // the escort override (1249, 1415)
    pub boat_turns: &'a BTreeMap<CityId, Turn>, // `c_boat_retry_turns` (1455)
    pub blocked_sites: &'a [TileIdx],           // `_bad_sites` within `site_blacklist_turns` (1090)
}
impl Advisor { pub fn with_facts(g: &Game, p: PlayerId, pp: &AdvisorParams, f: &BotFacts) -> Self; /* new() = with NONE */ }
```

- **The four assumptions read the facts** (advisor.rs:444, 728, 925 and 1363), and the escort override the advisor lacks is added in both modes (a defender first in the unciv mode, `c_escort` in the classic one). Automatic production passes `BotFacts::NONE`, so its picks do not move; the advisor's corpus test proves it.
- **The advisor stays read-only.** Python's production popped `_need_escort` when it chose a defender; the bot clears `memory.need_escort` itself when the item the advisor picked for that city is a military unit.
- **Made public:** `Advisor::best_military(g, city, role)` and `Advisor::sites(g)`, for the emergency purchases (basic.py:1604) and the settlers.

**The what-if.** Building values come from `cities::what_if::what_if_building` inside the advisor. The bot never edits a city to simulate one, as `_simulate` did (1491-1505).

**Reads.** The bot reads the game through `&Game`: its accessors and the systems' public read functions, which cannot change the game (property P8). The roughly 70 engine helpers the review counted have Rust equivalents:

| Python | Rust |
|---|---|
| `research.available_techs`, `tech_cost`, `path_to`, `is_unresearchable` | `game::research::*` |
| `policies.adoptable_policies`, `branch_of`, `can_adopt_any` | `game::policies::*` |
| `religion.beliefs_available`, `can_found_pantheon`, `beliefs_to_choose`, `ai_choose_beliefs` | `game::religion::*`, `religion::found::*` |
| `cm.purchase_check`, `buildable_items`, `found_check` | `game::cities::{purchase, construction, founding}` |
| `combat.preview`, `can_attack_now` | `game::combat::resolve::{preview_of, can_attack_now}` (typed `Preview`) |
| `movement.can_stand`, `find_path`; `visibility.has_los` | `game::movement::*`; `game::vis::sight::has_los` |
| `automation._threat_reach`, `city_site_score` | `game::derive::danger::threat_reach`; the advisor |
| `briefing.bombard_targets` | `game::combat::city::bombard_targets` |
| `actions.unit_actions`; `units.check_upgrade` | `game::actions::unit_actions`; `game::units::upgrades::check_upgrade` |
| `CS.influence`, `influence_from_gold` | `game::city_states::*` |
| `victory.military_strength`; `D.opinion`, `ra_cost`, `is_friends`, `has_pact` | `game::victory::score::military_strength`; `game::diplomacy::{relations, deals}` |
| `economy.luxury_resources`, `strategic_resources` | the supply memo through `game::economy` |

- **Hot reads stay typed.** JSON-building functions (`combat::preview`, the view builders) are not used on hot paths; the bot calls the typed function under each.
- **No new read API.** `game::query` is refcheck's API; copying 70 reads into it would add a layer without adding safety. The safety is `&Game`.

#### P2.3.8 Diplomacy: typed deals, the switches and advice

- **`Owners`** is `[Owner; 10]` over `game::diplomacy::category::Category`: trades, agreements, peace, war, denounce, un, city_states, espionage, captured_cities, chat. `set` refuses an unknown category or owner with `set_diplomacy`'s messages. `owns(&Negotiation)` ports `owns_negotiation` through `category::proposal_categories`.
- **The switches** are honoured wherever Python checked `_llm(...)`: espionage, spies (1040); city_states, gifts (1640); war, preparing and declaring war (2406, 2445; the bot still fights every war it is in); peace, peace offers (2426); agreements, embassies, friendship and research agreements (2432); trades, luxury trades and the gold counter (2486, 2699). `respond` returns `DriverOutcome::Deferred` for a negotiation the model owns, so `drive` holds it for the host (1c-09). un and captured_cities are the seat's `auto` flags, as in Phase 0.
- **Valuation is typed.** `evaluate` ports basic.py:2558-2643 over `state::diplo::DealItem`, reading the proposal as the `Negotiation`'s typed `Terms` and the seat's memory for war plans and preparation.
- **Counters are built from typed items.** The gold ask is merged into an existing `DealItem::Gold` and never exceeds what the other side holds less what is on the table. Answers go out as `Action::RespondNegotiation`, offers as `Action::OpenNegotiation`.
- **`advice(g, pid, spec, nid) -> Advice`** ports 2713-2771 with Python's JSON keys (`deal_value`, `war_readiness`, `spare_luxuries`, `wants`). It decodes the seat's memory and reads the game; it writes nothing. The facade's `bot_advice` calls it, and so will Phase 3's hybrid advisor.
- **`_settle_chats` and `handle_negotiations` are not ported** (744, 759-772). Before every step, `drive` puts each negotiation waiting on a driven seat to its driver. A chat waiting on another seat is rule T3's `AwaitingReply`, which the host settles.

#### P2.3.9 Fixes instead of ports

Each fix is listed in the module docs of the code that makes it, and any fix a refcheck group can show gets an `intended.toml` entry.
1. `_simulate`'s cache swapping is gone: the advisor's what-if.
2. `u.goto = None` (1872, 1886) wrote engine state directly. A goto onto a blacklisted site is ignored and `memory.bad_sites` is the record.
3. One sequential stream becomes keyed streams (P2.3.5).
4. Memory survives saves (P2.3.4).
5. **`_sites_cache` and `site_cache_turns` are gone.** The advisor finds sites once per turn, only when a city could start a settler; fresh sites are better than four-turn-old ones and cheap in Rust.
6. **`_bv_cache` and `bv_cache_turns` are gone.** Its default, 0, already meant "no cache". The schema has 373 parameters.
7. Fractional ints and unknown list names are refused (P2.3.2).
8. Ties broken by Python's set order of unit names become ruleset id order, as the advisor does (`advisor-ties-by-id`).
9. Exceptions as control flow (`except ActionError` per unit, `_simulate` returning 0) become `Option`s; a refusal skips one action and never the rest of the turn.

#### P2.3.10 Porting order of `basic.py`

| basic.py | What | Rust | Package |
|---|---|---|---|
| 32-64, 71-628 | constants, `PARAM_GROUPS` | `params/basic-1.json` (2-00b), `params/*` | 2-01a |
| 631-707 | helpers, `__init__`, `ex` | `lib.rs`, `versions.rs`, `driver.rs` (`Turn`) | 2-01a |
| 709-738 | `set_diplomacy`, `_llm`, `owns_negotiation` | `owners.rs` | 2-01a |
| 740-757 | `play_turn` | `driver.rs` (phases filled in by 2-01b, 2-03, 2-05) | 2-01a |
| idle.py | the idle bot | `idle.rs` | 2-01a |
| 777-871 | `context`, defence, danger, space reserve | `basic1/context.rs` | 2-01b |
| 876-998 | research, tech values (both modes) | `basic1/research.rs` | 2-01b |
| 1003-1041 | policy order, policies, free great people, pantheon | `basic1/empire.rs` | 2-01b |
| 1069-1150, 1204-1576 | sites, counts, production | the engine's advisor, with `BotFacts` | 2-01b |
| 1152-1202, 1577-1587 | `manage_cities`, focus, avoid growth, city bombard | `basic1/cities.rs` | 2-01b |
| 1591-1643, 1739-1766 | gold, purchases, upgrades, spare units | `basic1/gold.rs` | 2-01b |
| 1696-1737 | beliefs, spending faith | `basic1/faith.rs` | 2-01b |
| 1771-1805 (dispatch), 1835-1857, 1879-1888, 1927-1949, 2054-2063 | `manage_units`' order and dispatch; settlers (no escort yet), workers, work boats, scouts | `basic1/units/{mod,scouts}.rs`, `basic1/{settlers,workers}.rs` | 2-01b |
| 1806-1833 | promotions, garrisons | `basic1/units/promote.rs` | 2-03 |
| 1858-1925 | settler danger, escorts, retreats | `basic1/settlers.rs` | 2-03 |
| 1951-2052, 2065-2094 | special units, air, naval | `basic1/units/{special,air,naval}.rs` | 2-03 |
| 2096-2389 | attacks, military orders, camps, ruins, war target, rally, approach, military power | `basic1/units/{attack,military,war_plan}.rs` | 2-03 |
| 1043-1064, 1645-1694 | spies, city-state gifts | `basic1/diplomacy/{spies,city_states}.rs` | 2-05 |
| 2394-2503 | `consider_diplomacy`, reachable city | `basic1/diplomacy/{mod,war}.rs` | 2-05 |
| 2505-2556 | luxury trades | `basic1/diplomacy/trade.rs` | 2-05 |
| 2558-2643 | `evaluate`, war items | `basic1/diplomacy/evaluate.rs` | 2-05 |
| 2645-2708 | answers and counters | `basic1/diplomacy/respond.rs` | 2-05 |
| 2713-2771 | `advice` | `basic1/advice.rs` | 2-05 |
| 759-772 | `_settle_chats` | not ported (P2.3.8) | |

After 2-01b the bot grows an economy, explores and expands with barbarians off; after 2-03 it fights the wars it is in; after 2-05 it is whole.

#### P2.3.11 How the port is checked

1. **Every Python recording is made first** (2-00b), while nothing else depends on it: the parameter schema, the bot-decision dumps for all three porting stages, and the bot scripts validated on Python. The Rust packages then only make things pass.
2. **Refcheck `--with-bot`.** `answer/deal_checks.rs` answers `deals[*].bot_value` with `citar_bot::evaluate` (a fresh memory, default parameters, aggression 0.4: the conditions Python recorded). `refcheck/enforced.toml` adds the path in 2-05 and the ratchet holds it at 0. Accepted differences are the engine's intended ones (supply, tech costs); an entry covering a pure bot difference is not accepted.
3. **A refcheck group, `bot_decisions`.** `scripts/refcheck/bot_dump.py` records, on each state and for each living major, the deterministic sub-decisions of a fresh `BasicBot(seed=0)` with `tech_noise = 0`:
   - stage 1 (2-01b): tech values in both modes, the next research and policy, each city's defence, danger and garrison need, the expansion sites, the spare units;
   - stage 2 (2-03): the attack each military unit would make (`ex` patched to record instead of act), the war target and rally point of a civilization at war;
   - stage 3 (2-05): the reachable city per pair, the luxury trade proposal, the advice with and without each open negotiation.

   Values are compared with refcheck's numeric tolerance and enforced. Choices are reported as agreement rates by `cargo refcheck bot-agreement`, with a floor of 95% per kind on the committed states and, locally, the corpus; the rest is attributed to a named cause (ties by id, an intended engine difference). The 12 committed states' recordings are committed (`refcheck/bot_decisions.json.gz`); the corpus's stay local (`CITAR_BOT_DUMP`), as the advisor's did.

   **A choice's agreement counts only the items where either engine's answer says something:** a choice that is not null, a list that is not empty, a flag that is true; the items are civilizations, cities, units or rival pairs, as the kind compares them (`bot_dump.py`'s `CHOICES`). Many answers are empty on most items (a unit with nothing to attack, a civilization with no luxury to trade or no policy it can afford), and a rate over every item would let a port that never answers clear the floor. `bot_dump.py` prints each choice's base rate, the items saying something of those asked, and 2-00b's as-built note lists them. Where a choice is rarely open on a saved state, the dump also asks it of every civilization as if it were: `preferred_free` (the free technology it would take if it held one), `preferred_policy`, `preferred_great_person` and `preferred_pantheon`, so those rankings are checked on every item. A choice with fewer than 20 items saying something on the committed states (cities in danger, attacks, war targets, the luxury trade, advice's wants, the picks open now) is gated on the corpus as well, locally, where each has at least 44 but the free technology, the free great person and the pantheon now (0, 8 and 3), which their dense rankings cover instead.

   **Advice about a negotiation is held by `deal_checks`.** The states are saved at a turn's start and hold no open negotiation. With one, advice differs only in `deal_value`, which is `round(evaluate(..), 1)` of the proposal on the table, and `evaluate` is what `deal_checks`' `bot_value` holds on every sampled deal (point 2). `bot_advice_plain_data` pins the rest: which side gives what, and the rounding.
4. **Bot scripts** (`tests/rules/bot_*.toml`, about 24, written in 2-00b).
   - A step kind `{ bot = "turn" | "respond" | "advice", player, negotiation?, aggression?, params?, diplomacy? }`. `turn` means "play the seat's turn and end it" on both runners: the Rust runner drives that seat alone with a seat limit of 1, the Python runner calls `play_bot_turn(end_turn=True)`; checks after it read the same point.
   - Every step pins the draws through `params`: `tech_noise = 0`, `ranged_chance` 0 or 1, and the war, peace and friendship chances 0 or 1. Scripts assert only outcomes no draw decides.
   - A script header `needs = "2-0x"` names the package that makes it pass on Rust. The Python runner ignores it; the Rust runner reports such a script as ignored. Each package's gate is that no script names it any more.
   - They cover what Phase 1 deferred: the 15 bot scripts, and the bot parts of `test_bot_diplomacy` and `test_bots`.
5. **The fixture sweep.** Every state, 12 committed and 250 in the corpus, loaded through `Game::from_python`; bots drive one round for every major with `DebugOptions::ALL`: no panic, no violation. This puts the bot in late-game positions in minutes.
6. **Whole games** in `crates/citar-testkit/tests/bot/`, with the invariants on, mixing bots and `RandomAgent`s.
7. **Statistics** (P2.4.4).
8. **Determinism.** The `bot` golden set, and a save and load at every round.

**As built in 2-00b** (the Python recordings; P2.3.2, P2.3.11):
- **The schema.** `scripts/bots/export_params.py` writes `crates/citar-bot/params/basic-1.json` (two-space JSON, Python's key order, `--check` compares): 17 groups, 373 parameters (179 int, 168 float, 9 choice, 8 bool, 7 order, 2 list), without `site_cache_turns` and `bv_cache_turns`, each spec Python's own but for 41 `int`s typed `float` with a float default (`RETYPED`, P2.3.2 "Types": the 8 with unit `x` and the 37 `AdvisorParams` holds as `f64`). `tests/test_bot_params.py` holds it equal to `PARAM_GROUPS` spec for spec, as JSON (so `1.0` and `1` differ), `RETYPED` to exactly that criterion, every `AdvisorParams` field to its schema type (`f64` float, the integers int, the enums choice), and the exporter's `--check`.
- **The clean-params table.** `tests/data/clean_params_cases.json` (recorded by `python -m tests.test_bot_params --record`, held current by the test) has 80 cases of `profiles.clean_params("basic", params)`: `out` or `error` (ProfileError's message, which names the engine `basic`, Python's name for basic-1). A case 2-01a's `clean()` answers differently on purpose carries `fix` and `basic1`, basic-1's own answer (`out`, or `error` in Python's message form, the value as Python's repr): `int-integral` (2.5 and "2.5" for an int, refused), `names-known` (an order's and a list's unknown name, refused), `dropped` (a retired cache key, removed whatever its value: `{"site_cache_turns": 8}` and `{"site_cache_turns": "x"}` give `{}`) and `retyped` (an integer for a retyped parameter comes back a float: `{"mil_war_mult": 4}` gives `4.0`). Every other case is answered alike, among them `{"bv_cache_turns": 0}`, which eight archived profiles carry, and `1.5` for a retyped multiplier. Python quirks the table records for the port to keep or to list: a bool from `1.0` is false (its text is not "1"), any other word is false, an order's `"default"` is kept as an override where the default is null, and a list takes `"default"` though it has no presets.
- **The bot step** (`tests/rulescript.py`, `tests/rules/README.md` "bot"): `{ bot, player, negotiation?, version?, aggression?, params?, diplomacy? }`, with `as` and `error`. Beyond P2.3.11: `version` (`basic-1` or `idle`), since "idle rejecting" needs the idle bot; `turn` gives `{turn, current}` after it and is refused off the seat's turn (Python's `play_turn` returned silently); `respond` gives `{outcome: done | deferred}` and is refused for a negotiation that does not wait on the seat, as `Game::answer` is; the Python runner defers through `bot_owns_negotiation`, since `BasicBot.respond` itself answers whatever it is asked. `params` are checked against basic-1.json's keys, and a turn's must pin every draw: `tech_noise` 0; `ranged_chance`, `peace_offer_chance`, `friend_chance` and `war_chance` 0 or 1; both `_aggr` chances 0; no `war_prep_rate` putting the war chance strictly between 0 and 1 (the runner refuses otherwise). A seat keeps its bot from step to step while version, aggression and params stay the same, as the Rust bot keeps its `DriverMemory`.
- **`needs`.** `crates/citar-testkit/src/script` parses it (a package id) and `run` refuses such a script; `tests/rules.rs` lists it as ignored (`script::ignored`, libtest-mimic's ignored flag). That takes two lines of code in `tests/rules.rs`, plus a sentence in its doc comment. They are the package's only Rust change outside `src/script`, so gate 4 is not met to its letter. The harness builds each trial, and libtest-mimic gives no other way to list one as ignored: `src/script` cannot reach the flag. A trial that is ignored only at run time still exits 0, so nextest counts it as passed. Without the flag, gate 2's "reported as ignored" could not hold. `test_rule_scripts` checks that `needs` names 2-01a, 2-01b, 2-03 or 2-05, that every script with a bot step is a `bot_*.toml`, and the pinning rule. `cargo nextest run --workspace --all-features --cargo-profile ci --profile ci`: 1,148 passed, 31 skipped (the 29 bot scripts and the two tests ignored before).
- **29 bot scripts**, each passing on Python under any `PYTHONHASHSEED` (checked with 0 to 7) and ignored by the Rust runner: 2-01a: `bot_selftest`, `bot_idle_rejects`, `bot_model_owned_deferred`; 2-01b: `bot_research_beeline` (Mining toward a quarry for marble, Pottery without), `bot_policies_finish_branches`, `bot_buys_defender_in_danger`, `bot_disbands_in_deficit`, `bot_settler_founds_at_site`, `bot_faith_buildings_first`, `bot_scouts_explore`, `bot_workers_automate`, `bot_pantheon_by_preference`; 2-03: `bot_settler_waits_for_escort`, `bot_garrison_fortifies`, `bot_holds_losing_attack`, `bot_ranged_siege_first`, `bot_great_person_used`, `bot_spaceship_part_to_capital`; 2-05: `bot_switch_{trades,agreements,peace,war,espionage,city_states}`, `bot_counter_merges_gold`, `bot_counter_capped_by_treasury`, `bot_counter_rounds`, `bot_offer_stands_once`, `bot_advice_plain_data`. Scripts of an earlier package do not lean on a later one (2-01a's answers are a rejection until 2-05, 2-01b's units stand still until 2-03). test_bot_diplomacy's 19 tests: the category, switch, trade-switch, war-switch, counter and advice tests became scripts (with 2-01a's owner unit tests); the three BotAgent tests are 2-09's; the two stream tests are replaced by keyed streams (`tests/bot/streams.rs`, 2-01a); `_settle_chats`' test goes with it (not ported). test_bots' bot parts are whole games, which 2-01b's and 2-03's game gates cover.
- **What the Rust runner must do** to read the same point as Python: a negotiation the bot opens in its turn with a seat nobody drives stops a drive (`AwaitingReply`) where Python's `_settle_chats` withdrew it; the runner closes such chats as a host would and drives on, and scripts read only a negotiation's opening entry (README "What a script reads").
- **`scripts/refcheck/bot_dump.py`** (documented in `refcheck/README.md`, "The bot's decisions", which is where the refcheck tools are documented) records for each living major, from a fresh `BasicBot(seed=0)` with `tech_noise` 0, its tool calls recorded, from cleared caches: stage 1 `context`, `tech_values` (both modes), `next_research` (both modes, as if nothing were researched, and `preferred_free`, the free technology it would take if it held one), `empire` (the policy, free great person and pantheon now, and `preferred_policy`, `preferred_great_person`, `preferred_pantheon`, what it would take if it could), `cities`, `sites`, `spare`; stage 2 `attacks` (each military land or sea unit readied with the `ready_unit` test operation, then put back) and `war_target`; stage 3 `reachable`, `lux_trade`, `advice` (none, and each open negotiation). It prints each choice's base rate (`CHOICES`, P2.3.11 point 3). Two runs give the same bytes, and `test_rule_scripts` re-records the committed file (`--check`).
  - **Committed** (`refcheck/bot_decisions.json.gz`, 11 KB): the 38 living majors of the 12 states, every kind for each, with no error: 4,054 tech values; 102 rival pairs; no open negotiation (states are saved at a turn's start). Base rates, the items saying something of those asked: next research 76 of 76 (by civilization and mode), the free technology now 0 of 76 and preferred 38 of 38; policy now 3 and preferred 38, great person now 0 and preferred 38, pantheon now 2 and preferred 38 (of 38); in danger 14 and needing a garrison 96 (of 96 cities); sites 29, spare units 21 (of 38; 132 sites and 46 units); attacks 19 of 143 units; war targets 8 of 38 (the 8 at war); reachable 45 of 102 rival pairs; luxury trade 1 of 38; advice's wants 4 of 38.
  - **Corpus** (`--fixtures refcheck/corpus`, about 11 minutes, to the laptop's `C:/dev/bot_decisions-corpus.json.gz`, `CITAR_BOT_DUMP`; 421 KB, sha256 `3ca4c33d6d60e5f6d5f7d8854891613bbe47051d70d49a6acf541037379a4e4b`): 1,302 civilizations in 250 states, no error, and a second run, made at the same time, gave the same bytes; 155,760 tech values, 7,494 rival pairs, again no open negotiation. Base rates: next research 2,604 of 2,604, the free technology now 0 and preferred 1,302 of 1,302; policy now 44 and preferred 1,302, great person now 8 and preferred 1,302, pantheon now 3 and preferred 1,302 (of 1,302); in danger 291 and needing a garrison 5,101 (of 5,101 cities); sites 975, spare units 719 (of 1,302; 4,566 sites and 2,373 units); attacks 675 of 7,693 units; war targets 151 of 1,302 (the 151 at war); reachable 1,737 of 7,494; luxury trade 57 of 1,302; advice's wants 168 of 1,302.
  - Advice about a negotiation is held by `deal_checks`' `bot_value` and `bot_advice_plain_data`, not by the dump (P2.3.11 point 3).
- **Found for later packages.** `evaluate` counts a peace treaty, which a proposal lists on both sides, on each side (+120 twice; friendship and pacts are counted once); `bot_advice_plain_data` records Python's 190 and refcheck's `bot_value` holds the same, so 2-05 either keeps it or fixes it with an `intended` entry and both. The danger purchase (basic.py:1600-1606) buys the defender `manage_cities` has just queued, and nothing refills the queue that turn (the refill at 1639 follows only a purchase of the queue's head), so the city ends its turn with an empty queue until the bot's next turn; 2-01b may refill it as a bot fix, which no script pins. `scripts/refcheck/README` does not exist; the documentation went to `refcheck/README.md`.
- **The fix round** (review findings): the 41 retyped parameters (P2.3.2 "Types"); the retired cache keys dropped rather than refused, and each fix case's own answer, `basic1`, in the clean-params table; the agreement rule over the items that say something, with each choice's base rate, and the dense rankings `preferred_free`, `preferred_great_person` and `preferred_pantheon` (P2.3.11 point 3, Appendix C's 2-01b, 2-03 and 2-05 gates); `bot_buys_defender_in_danger`'s `about` no longer claims the queued defender, which the purchase takes. After it: `python -m unittest discover -s tests` 652 tests, 2 skipped; the 29 bot scripts under `PYTHONHASHSEED` 0 to 7; nextest 1,148 passed and 31 skipped.

### P2.4 citar-sim and the statistical comparison

#### P2.4.1 The runner

- **`citar_sim::Runner`** owns a `Game` and one driver per major. **`step()` is one `Game::drive` with `seat_limit = 1`**, so control comes back after each driven seat: a host can check a time budget between seats, call `on_round` when the turn changes (as `common.play`'s `on_round` does), and, in the bindings, reacquire the GIL to deliver that step's events.
- **Panics.** A drive panic is caught (`catch_unwind(AssertUnwindSafe(..))`). The runner poisons the game, writes a crash record with the message (and the seat's label, if the spec gave one) and ends the game. There is no `max_errors`: a Rust bot does not raise, and a panic is a bug. With `raise_errors` the binding raises `EngineCrash` instead (`citar sim`'s contract).

**Two outputs, each in the shape its Python reader already reads:**
- **`run_game`.** `engine_api.run_game`'s dict: `{turn, turns, phase, winner, victory, turn_limit, stats, players, errors}`; majors add `difficulty`, `techs`, `future_techs`, `policies`, `religion`, `great_people`, `cities`, `spaceship` and `score` (0 once eliminated), as `headless.result` does.
- **The baseline writer.** `baseline.py`'s line exactly: `IDENTITY` (`i`, `seed`, `size`, `map_type`, `barbarians`, `speed`, `turn_limit`); the size × map × barbarian rotation and seed `--seed + i`; `civs[].at` with `"100"`, `"200"`, `"300"` and `"end"`; the `Tally` semantics (wars by attacker split by the defender's kind, captures and losses, checkpoint totals taken at the start of round T+1, the end-of-game fallback); `engine` = the build id, `bot` = `basic-1`; resuming, refusing to add to another build's file, crash lines, `--workers` on `std::thread`. A `BaselineLine` serde type round-trips every committed Python line, so the schema cannot drift.

**The lab stays in Python.** `lab.play` already plays through `engine_api.run_game` with `on_turn` and `on_event` (lab.py:303-389); on Rust its bots are facade handles and its tallies come from the events the binding delivers per step. A Python-free lab job is Phase 4's, when the helper needs one and its result format is redesigned with signatures.

**The CLI** (`citar-sim`, a developer tool): `play` (`citar sim`'s printout), `baseline` (as `baseline.py`), `--smoke` (two short duel games). The wheel exposes the same runner through `engine_api.run_game`.

#### P2.4.2 The game benchmarks

`crates/citar-bench/benches/games.rs` (`harness = false`) plays, single-threaded, release, pinned to core 0: the small 4-bot Quick game of seeds 5000-5002 to 330 rounds (median), and one gargantuan 24-bot game of seed 5000. It writes `<target>/perf/games.json` for `cargo xtask perf --suite games`:

| Measure | Budget | Hard limit (1.5x) | Floor | Target |
|---|---|---|---|---|
| `game/bot_small_330` | 16 s | 24 s | 25 s | 5 s |
| `game/bot_gargantuan_330` | 180 s | 270 s | 279 s | 90 s |

25 s is the plan's 20x over Python's ≈ 510 s on one core; 279 s is 30x under `python-gargantuan.jsonl`'s faster game. The suite is report-only until 2-07 and hard from then on. Timed runs are taken with the other lane paused (the orchestrator's job) and record the machine's load.

#### P2.4.3 The Python baselines are committed

2-00a moves `refcheck/baseline/python-*.jsonl` to `refcheck/baseline/python/{small,std-large,gargantuan,smoke}.jsonl` (490 KB) and commits them; `refcheck/.gitignore` narrows to Rust runs and logs. They cannot be recreated once the Python engine is gone, and today they exist on one laptop.

#### P2.4.4 Statistical comparison: method and gates

**Data.** Python: small ×60; standard ×14 and large ×10; gargantuan ×2. Rust: small ×120 (seeds 5000-5119, the rotation over the five map types, barbarians normal, Quick, no turn limit); standard/large ×50 (the rotation gives 25 of each); gargantuan ×2, for timing and as a check that both games end.

**Method** (`scripts/refcheck/summarize.py`, pure Python, kept after the removal):
- **The unit is the game.** Civilizations in a game are correlated, so each metric becomes one number per game: the mean over the majors alive at the checkpoint for state metrics (`cities`, `population`, `techs`, `score`, `military`, `policies`, `land`, `era`), the game's total for event metrics (`wars_declared`, `cities_captured`).
- **Strata.** Small, standard and large are compared separately: score@200 is 420 on standard and 349 on large, and the two runs mix the sizes differently.
- **Per metric, checkpoint and stratum:** the ratio of means, the difference with a bootstrap 90% interval (2,000 seeded resamples of games), and `d` from the per-game values for reference. `--split-half` reports the noise of the Rust run against itself.
- **Rates per stratum:** the share of games with a war declared, with a capture, ending before the turn limit, by victory type, and with an elimination.

**Gates,** run by `summarize.py PYTHON RUST --gate refcheck/baseline/explained.toml`, exit 1 on a failure:

| Gate | Small | Standard, large |
|---|---|---|
| G1 crashes | 0 crashed, poisoned or timed-out games; 0 invariant violations in a 20-game subset run with checks on | same |
| G2 gross gaps: cities, population, techs, score at 100/200/300 | Rust's mean within [0.75, 1.33]x of Python's, and Rust's means rising from 100 to 200 to 300 | [0.67, 1.5]x, and rising |
| G3 explained gaps: every metric × checkpoint | a gap of 10% or more whose interval excludes 0 needs an `explained.toml` entry (event metrics only where Python's mean is at least 1 a game) | 15% |
| G4 rates | each rate within ±25 percentage points of Python's, or explained | ±35 points |

- **`explained.toml`** holds `[[gap]]` entries (`metric`, `checkpoint`, `stratum`, `reason`). A reason is an intended engine fix by id, a bot fix from P2.3.9, or a measured cause with its evidence. An entry whose gap no longer meets G3's rule is printed as a warning, not a failure, so a re-run on a later head cannot fail on noise.
- **Why these numbers.** On these metrics the per-game coefficient of variation is 4-20%, so G2's ratio bounds sit many standard deviations out: they catch §9.8's gross mistakes (cities that never grow, wars that never end) and nothing else. Requiring both a material gap (10%) and a significant one makes every real behavioural difference a written decision, without asking for causes of 2% gaps on low-spread metrics. The intended rule fixes (99 of them) and a new map generator move the distributions a little, which is why G3 explains rather than forbids, and why game length is a rate (early endings) rather than a median that sits at 330.
- **Calibration** (report-only, 2-07): the same table for Rust `standard` against Rust `classic-production` (small ×60 each), so a reader can see what a known bot change moves.
- **Bounded fixing.** 2-07 fixes porting mistakes it finds. A gap that needs bot-logic work beyond that becomes a follow-up package (`2-07f`), scheduled by the orchestrator before 2-12; `explained.toml` marks it `pending = "2-07f"`, which 2-12's precondition refuses.

**As built in 2-04: citar-sim** (P2.4.1, P2.4.2; Appendix C). The runner, `run_game`, the baseline writer, the CLI and the game benchmarks are real; `SimError::NotYet` is gone. Every game here is played by 2-00a's stub bot, which plays `basic-1` as `idle`, so the numbers below measure the engine and the runner, not the bot.
- **The runner** (`src/runner.rs`). `Runner::step` stays one `drive` with a seat limit of 1. What `drive` leaves to its host, the runner does as `headless.play` did, so a game nobody sits at cannot stall: a `Stop::External` seat (no driver) passes its turn through `Game::end_turn` in the same step, and the chats of a `Stop::AwaitingReply` are closed as `expired` (the next drive then ends the turn). `Runner::new_with` makes the drivers from the new game, so a host seats a bot in every major without knowing who they are. `RunSpec` gains `debug` (the game's `DebugOptions`, the build's default when `None`) and `traceback_limit` (5). `Seats` names the driver list. `Runner::take_violations` hands a host what the checks found.
- **Crashes.** A caught panic poisons the game, ends it and is a `Crash { turn, player, label, message, trace }` whose `Display` is Python's line (`T4 P1 careless: panic: ...`); `run_game`'s `errors` holds the line and, under it, `trace`: where the panic happened (`at file:line:col`) and, when `RUST_BACKTRACE` asks, the first `traceback_limit` frames counted from the code that panicked. A backtrace taken in a hook starts with the capture, the hook and the runtime's panic handling, ten frames before that code, so `Trace::text` skips them down to the panic's entry point (`core::panicking::panic_fmt`, `std::panicking::begin_panic`) and then the standard library's frames that raised it (`unwrap_failed`, an index's bounds check), starting at the runtime's first frame, whose names std ships (a frame of ours may be misnamed, below), so the `catch_unwind` frames under the code are never taken for the panic's own; the frames keep their numbers in the whole backtrace. Without debug information (the ci and release profiles on Windows) a frame is named after the nearest public symbol, so the names there are a hint and the location line is the fact. A payload carries only the message, so `citar_sim::panics::install` adds a process-wide hook, chained to the one it replaces, that keeps the location in a thread-local for the runner that catches the panic on the same thread; hosts choose to install it (the CLI does; the bindings should). A crashed game's result has phase `playing`, as a Python run stopped by its error limit had. With `raise_errors` the caller gets `SimError::Crashed`.
- **`run_game`** calls `on_round` with turn 1, with each new turn after the step that began it, and once more with phase `over` when the game ended on a new turn; `on_events` gets each step's events before that step's round call, so a tally taken in the round hook has every event of the turn before. A test checks the events delivered are exactly the chronicle's after the game's creation, in order.
- **`RunResult`** (`src/result.rs`) is `headless.result`'s dict, keys in Python's order. Against the skeleton: `turn_limit` is the game's (Python's `Game.new` always filled it in), so it is a `Turn`, not an `Option`; a major's `difficulty` is its seat's or else the game's name (Python's `Player.difficulty`, set the same way at `game.py:247`); `religion` is the progress name; `spaceship` is `null` until a part is added and then `{part: count}`; the counts are typed (`u32`, `i32`). `scripts/bots/run_game_keys.py` records the shape of Python's result on a 15-turn duel (city-states and barbarians at their defaults, so every kind of player has a row): every key at every depth and every value's type, numbers one class, lists and id-keyed objects as the set of their elements' shapes. `tests/run_game.rs` plays the same duel with `basic-1` and compares; the shapes are equal.
- **The baseline writer** (`src/baseline/`): `spec.rs` (the options with `baseline.py`'s defaults, the rotation with the barbarians varying fastest as `itertools.product` gives it, seed `+ i`, `IDENTITY`, `common.game_config`, the budgets: 60 minutes on small scaled by area with a 20-minute floor, the stall limit the longest budget plus ten minutes), `play.rs` (the bots' aggression `0.25 + 0.5 * ((pid * 37 + seed) % 10) / 9`, the `Tally` over typed `war_declared` and `city_captured` events, the checkpoint totals, the end-of-game fallback, the lines). A checkpoint's totals are those of the events of its turn and before: the `Tally` takes them just before the first event of a later turn. Python took them in its round hook as round T+1 began, before any bot acted or answered in it; a runner step that begins round T+1 goes on until the seat limit stops the drive, and a drive first puts waiting chats to their drivers, so a step boundary would count an answer's war of turn T+1 in checkpoint T. Nothing declares a war or takes a city as a turn begins, so the events' turns give Python's moment exactly, and a checkpoint no later event passed reads the totals at the end, which is Python's fallback for a game ended by the checks right after it. A panic outside the runner (making the game and its map, seating the bots, summing the game up) is the game's crash line `panic: ...` with its location and `seconds`, as Python's `play_one` caught every error of its game; the workers' own catch stays as the last resort, `run.rs` (`existing`, the run, the workers). Lines are compact JSON, one per finished or crashed game, flushed as they come. Differences from Python's lines, by design: `engine` is the build id and `bot` is `basic-1` (with `+<fingerprint>` when `--params` overrides any parameter, so a calibration run is its own sample); `bot_errors` is always 0; `cpu_s` is the CPU time of the game's thread (`cpu-time`, a small crate of safe wrappers over `GetThreadTimes` and `clock_gettime`, whose `libc` and `winapi` were already locked); a crash line's `crash` is the `Crash` line, `GameTimeout: still playing at turn T after its N-minute budget` (N as `%.3g`), `ConfigError: ...`, or with `--checks` `InvariantViolation: ...` with the violations as its `trace`. The default file is `refcheck/baseline/rust-<build id>-<bot>.jsonl`, found from the working directory up. Workers are `std::thread`s with 16 MiB stacks taking games from a queue; a thread cannot be interrupted, so the in-turn watchdog of `common.Deadline` has no Rust counterpart: a game past its budget stops between seats, and one stuck inside a turn is caught by the stall limit, written as a crash line, and ends with the process. Exit codes are `baseline.py`'s: 0, 1 when some crashed or the run stopped early, 2 when the file holds other games or an option names nothing (checked against the ruleset before anything is written), and also for a `--max-minutes` that is negative, not a number, infinite or more than a `Duration` holds, or a `--seed` whose games would pass `u64::MAX` (a debug build panicked on both, a release build wrapped the seed to 0).
- **The CLI** (`src/main.rs`, `src/play.rs`): `citar-sim play` takes `citar sim`'s options (and `--bot`, `--smoke` for two 40-turn duels), raises crashes and prints a standings line every ten turns and at the end and the last 25 headlines, as `citar sim` did; `citar-sim baseline` takes `baseline.py`'s options plus `--dir`, `--checks` (refused by a build without the engine's checks; the crate's `checks` feature forwards to the engine's) and `--params` (JSON or `@file`). `summarize.py` reads the Rust smoke file beside the Python one.
- **An engine fix.** A seat's `"nation": null` was refused ("nation must be a name, not None"), and so was a null `difficulty`. `citar sim`, the lab, the balance runner and `common.game_config` all seat bots that way, and Python's `rules.resolve(kind, None)` is None, so `setup::named` now reads a null as none chosen (a nation drawn at random, the game's difficulty), with an engine test. The engine code, and so the build id, moved; no golden did (no golden configuration has a null seat value).
- **The game benchmarks** (`benches/games.rs`): each game plays on a thread of its own (16 MiB stack) pinned to the suite's core, from its creation to its end with the checks off, through `Runner` with `baseline::bot_seats`; the small games are the baseline's games 0-2 (continents, pangaea, archipelago; barbarians normal). Each game's time is noted beside the measure (`game/bot_small_330/5000`, ...) and printed with its turns, its victory and its thread's CPU share, which shows a busy machine. `thresholds.toml` has the two rows, 16 s and 180 s, `report_only` until 2-07. On the laptop (bench profile, core 0), with the stub bot: `game/bot_small_330` 376.6 ms (the three games 389.4, 370.2 and 376.6 ms, each to turn 330 and a Time victory), `game/bot_gargantuan_330` 2.72 s (330 turns). The CPU shares were 0.87 to 0.97, so the other lane was busy at times. These time the engine, map generation and the runner; the bot's own cost arrives with 2-01b and 2-03, and 2-07 makes the rows hard.
- **Gates, run locally** (the GitHub jobs need a push, which a worktree does not make).
  1. All 88 committed Python lines read back to equal JSON values (2-00a's test), and so do the lines of the smoke test's Rust run.
  2. `tests/run_game.rs`: the result's shape equals `run_game_keys.json`; the same spec twice gives equal results, the same round calls and the same events.
  3. A driver that panics on turn 4 gives `T4 P1 careless: panic: the test driver panics on turn 4` with its location, a poisoned game and phase `playing`; with `raise_errors` the caller gets `SimError::Crashed` naming the label; another game then plays to its end in the same process.
  4. `tests/baseline.rs`: the smoke run's two duels; a stopped run resumed (a finished line kept, a crashed game and a torn one played again, each equal to the uninterrupted run's game but for its times); the CLI killed after its first line and run again (exit 0, every finished line kept, nothing played twice); refusals of another build's line, another seed's and a line that is no game, the file left as it was. `citar-sim baseline --smoke` and `play --smoke` from the command line; `summarize.py` reads the smoke file. The crate's tests ran 15 times in a row; one race was found and fixed (a test took the first line for game 0, and two workers finish in either order).
  5. `cargo xtask perf --suite games` built the suite (6 min), wrote `perf/games.json` with the two measures and four notes, printed both rows `report-only: ok` and exited 0, as `--suite games --check` did.
  6. Windows: fmt, clippy over the workspace with `--all-features`, the engine alone in rust.yml's six feature sets and in release, citar-sim in release, the fuzz crate's `--locked` check, `cargo xtask check`, `cargo doc -D warnings`, nextest in the ci profile (1,216 passed, 2 skipped; no corpus in the worktree), the doctests, the dev and ci builds with citar-py, `cargo golden check` in ci and release (all 15 sets `ok`, the hashes unchanged), refcheck on the fixtures with the ratchet holding and `--strict` over all 262 states with the corpus (clean), the Python suite (608 OK, 2 skipped) and ruff. Linux (WSL Ubuntu 24.04, a fresh clone, no corpus): fmt, clippy, doc, `cargo xtask check`, the dev build, nextest (1,216 passed, 2 skipped), the doctests, the fuzz check and both CLI smoke runs; the build id is `40b93ac06046` on both. Not run: macOS and every GitHub job.
- **Fix round** (the review's six findings, each with tests): the crash record's frames start at the code that panicked (a pure test over Linux and Windows backtraces with the `catch_unwind` frames below, and a live one that forces a capture in the hook and panics in a named `#[inline(never)]` function); the `Tally` takes checkpoints by the events' turns (hand-built batches: wars on a major, on a city-state, with no attacker or no defender, captures both ways, a batch spanning turns 10 and 11, turns skipped, the fallback; and a whole 30-turn duel whose test drivers declare wars on turns 10, 11 and 30, the last the turn limit); a panic outside the runner is the game's crash line; `--max-minutes` and `--seed` are bounded, through `run` and the CLI; and the runner's expiry of a chat that waits on the host has two tests, one with a seat nobody plays and one with a driver that defers. Every gate was run again at the fix round's code head: on Windows fmt, clippy (the workspace with every feature, the engine's six feature sets and release, citar-sim in release), the fuzz check, `cargo xtask check`, `cargo doc`, nextest in the ci profile (1,226 passed, 2 skipped), the doctests, the ci and dev builds, `cargo golden check` in ci and release, refcheck on the fixtures with the ratchet and `--strict` over the 262 states with the corpus (clean), `cargo xtask perf --suite games` and its `--check` (`game/bot_small_330` 314.3 ms, `game/bot_gargantuan_330` 2.17 s, both report-only), the Python suite (608 OK, 2 skipped), ruff and the strict link check; on Linux (WSL, a fresh checkout, no corpus) fmt, clippy, doc, `cargo xtask check`, the dev build, nextest in the ci profile (1,226 passed) and citar-sim's tests in the dev profile, the doctests, the fuzz check, both smoke runs and the two refusals (exit 2). The ci profile with every feature on Windows is where a frame of the hook first read `std::rt::lang_start`.

### P2.5 citar-store

#### P2.5.1 The container

```
"CITARSV2" | u32 LE header length | header JSON | one zstd frame: body JSON
header: {"format":"citar-save","version":2,"saved_at", "engine_build", "rules":"<RulesetId hex>",
         "summary": <save::summary of the state>, "session": {id, name, benchmark},
         "journal": {"file", "records", "bytes", "head"}}
body:   {"session": {...}, "metrics": {...}, "state": <the engine's state JSON, spliced raw>, "chain": null | {"head","rounds"}}
```

- **The header is uncompressed and small.** Listing saves (`list_saves`, `scoring`, `save_meta`) reads only headers, never a gargantuan state's 6 MB.
- **The body is zstd level 3,** about 10x on this JSON. The state is spliced in raw and handed to `Game::load` without a second parse.
- **Writes are atomic:** a temporary file, then a rename, retried eight times at 250 ms on `PermissionDenied` (the OneDrive and scanner case `session.save` already retries).

#### P2.5.2 The journal

```
"CITARJNL" | u16 LE version (1) | records...
record: u32 LE payload length | u32 LE seq | u8 kind (1 = engine journal chunk) | u8 codec (0 raw, 1 zstd) | [u8; 8] blake3(payload) | payload
```

- **`JournalWriter::open(path) -> (JournalWriter, Recovered)`** scans the records and takes an exclusive OS lock (`File::try_lock`), refusing a file another writer holds.
  - It truncates only an **incomplete tail**: a last record whose length runs past the end of the file (a crash mid-append).
  - A **complete record that fails its hash, or breaks the seq order,** is corruption (bit rot, a sync conflict). `open` then truncates nothing and refuses to append: `Recovered { records, bytes, torn: Option<u64>, corrupt_at: Option<u64> }` says where the good prefix ends, and the session forks from it (P2.5.3).
- **`append(seq, chunk)`** refuses a seq that is not the next one, so records are exactly the engine's chunks 0, 1, 2, ... **`sync`** is `File::sync_data`. A `PermissionDenied` on append is retried as on the rename.
- **A container names its journal by `JournalRef { file, records, bytes, head }`.** `read_upto(path, &JournalRef)` reads exactly that prefix and refuses when record `records - 1` does not hash to `head`: the journal was rewritten, forked or swapped, and the save is refused rather than loaded with someone else's history.
- **`fork(path, &JournalRef, new)`** copies the prefix's bytes to a new file.
- **Nothing panics on any input.** Every failure is a `StoreError`.

#### P2.5.3 Timelines, and writing off the lock

- **Files.** A game's folder holds containers (`autosave.citar`, `turnNNN.citar`, `benchmark.citar`) and journals (`journal.cjnl`, then `journal-2.cjnl`, ...).
- **A session writes one journal at a time, its timeline:**
  - a new game opens `journal.cjnl`;
  - loading a save continues its journal only if no container in the folder names a longer prefix of that journal and the journal opens clean; records past the save's `bytes` (a chunk appended for a save that never completed) are truncated;
  - otherwise (an older save, or corruption) it forks a new journal from the save's prefix, so every other save that names the old journal stays valid.
- **`delete_save`** also removes journals no container in the folder names.
- **Chunks are never lost.** `take_journal_chunk` moves the game's cursor when it returns, so the session's `Journal` keeps every chunk taken but not yet appended and appends those first on the next write. A failed append (a locked file, a full disk) leaves its chunk pending and writes no container, so no container ever names a record that is not on disk.

**The save writer.** `autosave()` is called from `_after_action`, with the session lock held by its caller, so the write cannot happen in place. Each session has one writer thread:

```
autosave() / save(name), under the session lock:   # microseconds to milliseconds
    snap = game.save_snapshot()                     # Rust: Snapshot (a State clone) + take_journal_chunk()
    writer.submit(snap, session dict copy, metrics copy, name)
writer thread (GIL released inside the write):
    journal.append(pending chunks in seq order) + sync
    to_json, zstd, tmp file, rename                 # the container, naming the journal's new end
```

- Queued autosaves coalesce: every chunk is appended in order, but only the newest autosave container is written. A named save (the save route) waits for its own write, outside the lock, and reports its error.
- **`stop()` drains the writer** (waits for the write in flight) and closes the journal, so a session loaded next never shares a journal with a stopping one; the OS lock enforces it.
- **Crash-safe ordering.** Chunks are appended and synced before the container that names them is renamed into place. A crash between leaves extra records, which the next continuation truncates.
- **What runs under the lock:** `Snapshot::new` (0.98 ms on the synthetic gargantuan state, 1e-03) and taking one round's chunk. `to_json` (23 ms), compression and both writes run off it. This fixes the torn saves `session.save()` produced by serialising live structures outside the lock (plan §2). Budget 10 ms.

#### P2.5.4 Replays and `events()`

- **A live game keeps its chronicle in memory,** frames as deltas (§4.11), so `replay_data(Full)`, `events()`, `stats()` and `thoughts()` read memory.
- **On load,** `Game::load(state, chunks)` rebuilds the chronicle from the records the container names. If counts or the running hash disagree, `LoadReport.chronicle_incomplete` is set: the game plays on, the session logs a warning and serves what it has.
- **One test covers the chain end to end:** play with an autosave every round, reload, and compare `replay_data`, `events()` and the digest with the live game's.

**As built in 2-02: citar-store** (P2.5.1, P2.5.2; Appendix C). The crate is `src/{lib,container,journal,disk}.rs`; each module's docs give its format byte for byte. Its tests are `tests/store.rs` with one file per gate under `tests/store/`. Gate 1 at full size is in `crates/citar-bench/tests/store.rs`, because citar-bench is the one crate the graph lets reach both the testkit's states and the store. The io suite holds the store's bench cases. `StoreError::NotYet` is gone.
- **The container is as designed.**
  - **The header.** It has exactly `Header`'s keys: unknown keys are refused, and every key must be there, `journal` included (it may be null, but serde would read a missing `Option` as `None`, so the field is read through a `deserialize_with` that makes a missing key an error). `format` and `version` are checked before the strict parse, so a newer save reads as "version 3" (`StoreError::Format`) rather than as an unknown key. `rules` must be 64 lower-case hex digits and `summary` an object. The `journal` reference must name a plain file name (no folder, `..` or drive), and its counts must be ones a journal could have.
  - **The frame.** It records its content size and a content checksum, and nothing may follow it.
  - **The body** has exactly the four keys, the first three of them objects; `chain` may be null but must be there, as in the header. Each part is spliced in without its surrounding white space, after a syntax check at write time (a `RawValue` scan, about 0.6 ms a megabyte), so a container that cannot be read is never written.
  - **Errors.** A wrong magic is `Format`, and a gzip file is the new `StoreError::Legacy` (a version 1 save; 2-11 words its message). Past the magic, everything else is `Corrupt`, at the byte where the bad part starts. A bad header or body given to the writer is the new `StoreError::Invalid`, and nothing is written.
  - **Limits.** A header may be at most 16 MiB and a body at most 256 MiB. A frame's buffer grows as its content comes, so a damaged content size cannot make a large allocation.
  - **The `Container` API.** Its fields became private, with the accessors `header()`, `body()`, `state()`, `session()`, `metrics()`, `chain()` and `into_header()`. The body's parts are found once when it is read, so `state()` cannot fail and no longer returns a `Result`. `read_header` also checks that a zstd frame starts after the header.
  - **Writing.** The temporary file is `<name>.<pid>-<tag>.<n>.tmp` beside the target, so no `*.citar` listing sees it. `<tag>` is 8 hex digits of the time the process first named one, because ids are reused: a service restarted at boot, or as pid 1 in a container, is often given the id of the run that crashed and left its temporary file behind. The file is created with `create_new`, and a name that is there already is passed over for the next, up to 64 of them; a left file is not removed, since a process on another machine sharing the folder may own it. It is synced, then renamed. The rename is tried nine times in all, 250 ms apart (the first try and the eight retries). It retries on `PermissionDenied` and also on Windows's sharing (32) and lock (33) violations: std leaves those uncategorised, while Python's `PermissionError` covered them, and a scanner holding the new file gives error 32. On Unix the folder is synced afterwards (best effort). A failed write removes its temporary file, and the old save stays.
- **The journal differs from P2.5.2 in its hash, deliberately.** The 8-byte hash covers more than the payload: it is blake3 over the previous record's hash (zeros before record 0), then the seq, kind, codec and payload.
  - Covering the head's fields means a damaged codec byte (a raw chunk read as zstd) is found at open, as gate 2 requires for every byte.
  - Chaining means a `JournalRef`'s `head` stands for the whole prefix, not only its last chunk.
  - The codec is zstd level 3 for chunks of 128 bytes or more, when that is smaller; otherwise the chunk is stored raw.
- **Recovery.**
  - **A torn tail** is a head cut short, or zeros to the end of the file (a crash that grew the file without writing it). It is also a record whose length runs past the end, *unless* the record can be seen to end earlier: its hash holds over a shorter payload that is followed by record `n + 1`'s head or by the end of the file. That case is a damaged length, which is corruption. Telling the two apart is what lets every single-byte change be corruption rather than a truncation; the journal module's unit test tries all 255 changes of every byte in memory. A file that is a prefix of the 10-byte header (one whose creation a crash cut short) opens as a fresh journal, with `torn` set to its length.
  - **Corruption** is a bad hash, a bad seq, an unknown kind or codec, a length over `MAX_CHUNK` (256 MiB, which `append` also enforces), or a zero head followed by anything but zeros. `corrupt_at` is the bad record's first byte, which is also where the good prefix ends.
- **Appends.** Each record is one write. A write that fails part way is cut back before the next try (and, best effort, before the error returns), so the chunk stays the next to append; it is retried like the rename. A chunk over 256 MiB is `Invalid`.
- **New functions.**
  - `JournalWriter::truncate_to(&JournalRef)` checks that the reference is one of the writer's own prefixes (count, end and head), then truncates and syncs. This is P2.5.3's "records past the save's bytes are truncated", which 2-11 needs. It refuses on a journal that opened corrupt: that one is forked instead. When the truncation or its sync fails, the writer stands at the reference all the same and marks the file to be cut again before the next append writes, so a record is never written past the end of the file (which would leave a gap of zeros, read as corruption at that record).
  - `JournalWriter::path()`.
- **Locks.**
  - `read_upto` and `fork` take a shared lock while they read. Windows's locks are mandatory: on the laptop a read through another handle while a writer holds the lock fails with error 33, so a journal a writer holds cannot be read there at all. The shared lock makes Linux behave the same, so a held journal is `Locked` to every reader on both systems, and a lock violation maps to `Locked`. **2-11 must drop a session's writer before it forks from or reads its own journal.** That includes another session's: loading a save of a game that is running (turn050 while the live session autosaves into the same journal, 2-11's gate 2) reads a journal the live session's writer holds. Python's `SessionManager.load` reads the save and only then stops the live session of the same id; with v2 that order fails with `Locked` on both systems. So `Manager.load` must first stop that session (drain its writer and close its journal), then read the save, and decide what a failed load then does: for example reload that session from its own autosave.
  - `fork` creates the new file with `create_new`, so it never overwrites one. It copies the prefix, syncs it, and removes a partial copy.
  - A file system without locks (`Unsupported`) is written without one, so a game on such a share still saves.
- **Tests.** The property tests follow `PROPTEST_CASES`: 256 when it is unset (proptest's own default) and 64 in CI. On the laptop the virus scanner reads every file a case writes, about 10 to 30 ms a case, so the gate counts are separate runs: `PROPTEST_CASES=1000` for the round trips and `PROPTEST_CASES=10000 ... --profile nightly` for gate 4. `nightly.yml`'s props job now runs citar-store's tests at 10,000 cases with "the rest", and keeps their saved cases on a failure. Gate 3 is tested twice: once with a second handle in the same process, and once from another process (the test binary runs itself as a child that holds the journal). `CITAR_STORE_EVERY_VALUE` makes gate 2's file-level test try all 255 changes of every byte instead of two: half a million files, which takes minutes on Linux and too long under the laptop's scanner.
- **Gates, as run** on Windows (the laptop) and on Linux (WSL Ubuntu 24.04, the branch from a bundle, no corpus):
  1. **Round trips.** The property tests of headers and bodies, and of record sequences (forks, `truncate_to`, and re-appends that write the same bytes again), pass at 1,000 cases: in 19 s and 38 s on Windows, and in 14 s for both on Linux. The gargantuan state (6,422,877 bytes of JSON; a 1,182,981-byte container) and 330 history chunks (688,258 bytes; a 282,404-byte journal) round-trip through the store, with the same sizes on both systems. `Game`'s save loads them back equal, with the history complete.
  2. **Recovery.** The 50-record journal is 1,878 bytes. Every cut, and every byte damaged inside a record, behaves as the gate says. On Windows each byte was changed in files by `^ 0x01` and by `^ 0x80`, and by all 255 values in memory; on Linux all 255 values were also tried in files (`CITAR_STORE_EVERY_VALUE`, 890 s). A cut recovers exactly the whole records before it, and the next append writes back what the cut took. Damage gives corruption at that record, nothing truncated, appends refused, and a fork that opens clean and carries on. `read_upto` reads every prefix within the good records and refuses the rest.
  3. **The lock.** A second writer is refused with `Locked`, both in the same process and from another one, on Windows and on Linux.
  4. **Arbitrary bytes.** They are always an `Err`, never a panic: 10,000 cases each for containers and journals, plus 10,000 each of mangled valid files. Windows took 148 to 350 s per property (its scanner); Linux took 38 s for all four.
  5. **The bench** (`cargo xtask perf --suite io`, bench profile, pinned to core 0, exit 0). Writing the gargantuan container takes a median of 29.1 ms (budget 150 ms) and reading its header 65 µs (budget 1 ms). Noted without budgets: reading the whole container takes 11.6 ms, appending one round's chunk with its sync 1.75 ms, and reading 330 chunks back 1.7 ms. An earlier run, straight after the bench build, measured 104.7 ms and 125 µs (criterion's mean then was 71.7 ms). The write varies this much on the laptop because its virus scanner reads each new file, but it stayed within budget.
  6. **Checks.** clippy `-D warnings` over the workspace, `cargo doc -D warnings`, `cargo fmt --check` and `cargo xtask check` are clean on both systems. As rust.yml runs it, nextest passes 1,239 tests with 2 skipped on Windows and 1,237 with 2 skipped on Linux (the two Windows-only tests of a held save). The doctests, the ci build with citar-py, and the fuzz crate's `--locked` check also pass.
  - **Not run here:** rust.yml on GitHub (macOS above all, where `flock` and the folder sync go untried) and nightly's props job with the store added. Both need a push.
- **Fix round.** The review's five findings were all valid. Four are fixed with tests, and the fifth is a note for 2-11 (under **Locks** above). The statements above are updated where they changed.
  - **Temporary files a crash left.** With `<name>.<pid>.<n>.tmp` and `create_new`, a process given a crashed run's id failed its first write of that save with `AlreadyExists`. The name now carries the process's tag, and a name that is there is passed over (**Writing** above). A unit test makes 10 names and then 65 names exist, and an integration test leaves files of the old and the new patterns beside a save.
  - **A failed `truncate_to`.** It ran `set_len` and then `sync_data`, and it moved the writer only when both succeeded. So a sync that failed after the cut left the writer at the old end, and the next append wrote past the end of the file. The writer's position is now a `Tail` whose `cut` moves first and stays dirty until the cut is made (**New functions** above). A unit test drives it over an in-memory file whose length change is refused, made and then reported as an error, or whose sync is refused, and checks that the next record follows the kept ones directly.
  - **Required keys.** serde read a header without `journal`, or a body without `chain`, as `None`. Both now must be there (**The header** and **The body** above). The tests remove each header key in turn, and a body's `chain`.
  - **The tests' gate 4 command,** joined into one line by a heredoc, is two lines again.
  - **Gates at the fix round's head,** on Windows and on Linux (WSL, from a bundle) as before. Gate 1's round trips at 1,000 cases took 18 s and 39 s on Windows and 11 s for both on Linux. The gargantuan round trip gave the same sizes as before. Gate 2's tests pass, and so does the run of all 255 values in files on Linux (1,355 s, alongside the Windows builds). Gate 3 passes on both. Gate 4 at 10,000 cases took 150 to 238 s per property on Windows and 29 s for all four on Linux. Gate 5: `cargo xtask perf --suite io` exits 0 with a write of 27.9 ms and a `read_header` of 67.8 µs, and notes 11.6 ms, 1.49 ms and 1.74 ms. Gate 6: fmt, clippy, doc and `cargo xtask check` are clean. As rust.yml runs it, nextest passes 1,243 tests with 2 skipped on Windows and 1,241 with 2 skipped on Linux (four new tests). The doctests, the ci build with citar-py and the fuzz crate's `--locked` check also pass.

### P2.6 citar-py and the backend switch

#### P2.6.1 The module

- **`crates/citar-py`** builds `citar._engine`: PyO3 0.29, `abi3-py311`, `generate-import-lib` for Windows. API names follow PyO3 0.26 and later (`Python::detach`, `Python::attach`); a `#[pyclass(frozen)]` must be `Sync`. 2-00a's skeleton compiles these, so the names are settled before 2-06a.
- **Classes:** `Game` (frozen: `Mutex<citar_engine::Game>` plus `Heads`, P2.6.2); `Bot` (frozen: `Mutex<Arc<BotSpec>>`, P2.6.5); from 2-11 `SaveSnapshot` and `Journal`; the exceptions of P2.6.3.
- **Values cross as bytes or small Python values.** Every value that would be a dict comes back as JSON bytes and is decoded by the facade; counts, ids and names come back as ints and strs.

| Facade (`engine_api`) | Binding |
|---|---|
| `rules_version`, `rules_client`, `max_players`, `map_sizes`, `map_types`, `speeds`, `difficulties`, `resolve_name`, `ruleset_counts` | module functions over the process ruleset (`Ruleset::shared()`, or `CITAR_RULESET_DIR`'s from 2-12) |
| `tool_list`, `tool_kind` | `api::tools::{schemas_json, kind}` |
| `state_summary` | `save::summary` |
| `validate_map`, `map_summary`, `blank_map`, `generate_map`; `list_maps`, `load_map`, `save_map`, `delete_map`, the scenario files | `api::maps::*`; the facade draws seeds (`random.randrange(1, 2**31)`) and does the file I/O |
| `scenario_ops_help`, `scenario_summary`, `list_scenarios`, `load_scenario`, `delete_scenario` | `api::scenario::*`, file I/O in the facade |
| `DIPLOMACY_CATEGORIES`, `item_category`, `proposal_categories` | `game::diplomacy::category` |
| `RULES_OVERVIEW`, `MAP_LEGEND`, `DEBUG_ACTIONS` | module constants (`api::text`) |
| `bot_instance`, `bot_set_diplomacy`, `bot_owns_negotiation` | `Bot(version, params_json, aggression, fixed_aggression)`, `.set_diplomacy(owners_json)` (in place), `.owns_negotiation(negotiation_json)`, `.aggression`, `.version` |
| `run_game` | `run_game(spec_json, {pid: Bot}, on_turn, on_event)`: the citar-sim runner; the GIL is released for each step and reacquired to deliver that step's events and the turn hook; `raise_errors`, `labels` and `traceback_limit` honoured |
| `EngineGame.new`, `from_state`, `from_save`, `state_dict`, `to_save` | `Game.new(config_json)`, `Game.load(state_json, chunks)`, the state JSON and the whole history as one chunk; the facade inlines the map document and draws the seed (§8.1) |
| `turn`, `current`, `phase`, `winner`, `victory`, `turn_limit`, `is_alive`, `negotiation_head`, `open_negotiation_heads` | read from `Heads` with the GIL held |
| `config`, `player`, `majors`, `summary`, `player_name`, `standing`, `standings`, `stats`, `events`, `event_view`, `thoughts`, `thought_count`, `add_thought`, `emit` | `api::host`, `api::views::empire`, the chronicle reads, `event_json`, `add_thought`, `emit_host` |
| `execute`, `view`, `briefing`, `turn_progress`, `empire_summary`, `end_turn_refusal` | `execute`, `view_json`, `briefing`, `turn_progress`, `empire_summary` |
| the negotiation reads and `close_negotiation`, `max_chat_messages`, `deal`, `describe_items`, `validate_items`, `open_negotiation_as` | the host methods of 1c-05 |
| `set_controller`, `set_difficulty`, `apply_ops`, `scenario_overview`, `save_scenario`, `default_seats`, `normalize_seats`, `export_map`, `path_preview`, `has_met`, `meet`, `force_turn`, `debug`, `replay_data` | the host methods of 1b-02, 1c-09, 1d-02 and 1d-03 |
| `play_bot_turn`, `bot_respond`, `bot_advice` | one-seat `Game.drive`, `Game.answer(pid, nid, Bot)`, `citar_bot::advice` |
| Phase 2 names: `EngineGame.drive`, `answer`, `view_json`, `replay_json`; `build_info`, `bot_versions`, `bot_schema`, `bot_clean_params`, `bot_fingerprint`; the exceptions `EngineCrash` and `BackendError` (defined on both backends) | `Game.drive({pid: Bot}, seat_limit)`, `Game.answer`, `view_json(pid, extra)`, `replay_json(extra)`; `citar_bot::*` and `build_id` (P2.2.1) |
| `inspect`, `test_ops` (tests) | `api::{inspect, testops}`, present only with `test-ops` (`citar._engine.HAS_TEST_OPS`) |

**Engine additions for hosts (`api::host`, 2-06a),** since §3.2's `api/summary.rs` was never built: `summary` and its player rows (controller, handicap, auto, overrides, difficulty, `founded_city`); the lobby config as JSON and the turn limit; the negotiation heads and the open negotiations; an event by id; the stats and thoughts as Python's rows. They are engine functions with engine tests, so the binding stays a thin layer of calls.

**Two more engine additions:**
- **`Game::answer(pid, nid, &mut dyn SeatDriver) -> Result<(DriverOutcome, EventBatch), ActionError>`** (2-00a). It puts one open negotiation that waits on `pid` to a driver through the same `with_driver` that `drive` uses (`driving` set, memory copied and written back, the game settled). It refuses (`ErrCode::Negotiation`) a negotiation that does not wait on `pid`, which a responder that lost a race ignores. It is the session's responder for bot seats and the Rust runner's `bot = "respond"` step.
- **The test operations `eliminate {player}`, `end_game {winner?, victory?}` and `panic`** (2-06a; `panic` exists for the bindings' tests). The first two replace the eleven places tests poke `python_game`, and the Python `testops` gains them while it exists.

#### P2.6.2 The GIL, the locks and `Heads`

- **Heavy calls run inside `py.detach`, with the game `Mutex` taken inside.** No thread ever waits for a game lock while holding the GIL, so the two cannot deadlock. Heavy means anything that can exceed about 100 µs: `execute`, `drive`, `answer`, views, briefing, replay, load, `apply_ops`, snapshots and writes, `summary`.
- **Cheap reads never detach and never wait for the game.** At the end of every call that changes the game, still under the game lock, the binding publishes a small `Heads` copy (turn, current, phase, winner, victory, turn limit, revision, the alive set, the open negotiations' heads, poisoned or not) into its own `Mutex`, held only for a copy. The getters in the table read it with the GIL held; `negotiation_head` of a negotiation no longer open falls back to a locked read. Many server paths read these without the session lock (`info()` for the lobby, `queue_machine`, `mark_live`, `god_view_allowed`, the pool), and they no longer wait for a bot turn or pay to re-acquire the GIL.
- **Lock order is game, then heads; getters take only heads.** Nothing inside `detach` touches Python. Callbacks (`run_game`'s hooks, event fan-out) run after the game lock is released, with the GIL held.
- **The session's `RLock` still serialises a game's callers.** The `Mutex` is insurance against a caller that skips it, never the protocol.
- **What the server gains:** two games on two cores (plan §3). 2-06a's gate 2 measures it.

#### P2.6.3 Errors

| Engine | Python |
|---|---|
| `ActionError { code, message }` | `citar._engine.ActionError(message)` with `.code` (`"not_your_turn"`, ...), exported as `ActionError`, so `except ActionError` works on both backends |
| `ErrCode::Poisoned`, `EngineError::Poisoned`, a caught panic | `citar._engine.EngineCrash(RuntimeError)`, **never** `ActionError`: the server must not swallow a crash as a refusal. The game refuses commands and still answers reads and snapshots (§8.5). |
| `EngineError::Config` | `ValueError`, as `Game.new` raised |
| `EngineError::Map`, a map refusal | `citar._engine.MapError(ValueError)`, exported as `MapError` |
| `LoadError`, `StoreError` | `citar._engine.LoadError(ValueError)` |

#### P2.6.4 Events, panics and interpreter exit

- **Every mutating call returns its `EventBatch` as JSON,** each event in Python's dict shape (`Game::event_json(ev, None)`, 1d-02).
- **The facade fans events out.** `EngineGame` keeps the subscribers and calls each for each event after the call returns, on the calling thread. That changes `subscribe`'s documented timing from "during the call" to "after it"; the docstring says so.
- **Panics never cross the boundary.** Each entry point runs `catch_unwind` inside the game lock, so the `Mutex` is never poisoned; the game is marked poisoned and the caller gets `EngineCrash`.
- **Interpreter exit.** A daemon thread (a session driver) can be inside `detach` when Python finalizes; re-attaching then can kill the thread in ways that abort the process on some Python versions. The binding counts calls in flight, and the facade's `atexit` hook waits up to 5 s for them. A test on Linux with Python 3.11, 3.12 and 3.13 exits the interpreter while a daemon thread drives a game and asserts exit code 0.

#### P2.6.5 Bots across the boundary

- **A `Bot` handle holds `Mutex<Arc<BotSpec>>`.** `set_diplomacy` swaps in a spec with the new owners, in place, as `bot_set_diplomacy` promises and Phase 3's hybrid seats rely on; the `Tuning` (and its resolution cache) is shared, not rebuilt. `.aggression` and `.version` are read-only properties (`lab.play` reads the first).
- **Each `drive` or `answer` snapshots every handle's `Arc` once at its start** and builds fresh `citar_bot::Bot`s from them, so no Python object is borrowed across the call, two seats may share a handle, and a `set_diplomacy` during a drive applies to the next one.
- **The drive's result carries each bot's action counts** (`{pid: {tool: [ok, refused]}}`, from `Bot::refusals()`), which the server records (P2.7.1).
- **Only compiled bots run on Rust.** A Python bot object is refused with a `TypeError`.

#### P2.6.6 How the facade selects its backend

- **`citar/engine_api.py` becomes the selector.** `BACKEND = os.environ.get("CITAR_ENGINE", "python")`, then `from .engine.facade import *` or `from ._facade_rust import *`. It keeps one `__all__` and checks at import time that the backend defines every name.
- **The Python backend is frozen** at today's 38 names and `EngineGame` methods, moved unchanged into `citar/engine/facade.py` (deleted with the engine). Names added in Phase 2 are **Rust-only**: on the Python backend they raise `BackendError("Rust backend only")`, and tests that use them are marked `@rust_only`. Nobody writes a throwaway Python implementation.
- **`citar/_facade_rust.py`** implements every name over `citar._engine` with the shapes the docstrings promise. Two documented behaviour changes: `apply_ops` is all or nothing on Rust (`atomic-apply-ops`), and `subscribe` fires after the call.
- **The boundary test grows:** `citar._engine` is importable only from the facade's modules, and no module outside the facade and the tests reads `BACKEND`.
- **The parity test** (`tests/test_facade_parity.py`) imports both backend modules, plays the same seeded duel and small game on each, and compares every original name's and method's key sets and value types under type classes: int and float are one number class, `None` matches any type where the docstring says optional, an empty collection matches any element type. Behaviour tests run on both: `bot_set_diplomacy` then `bot_owns_negotiation`, `raise_errors` raising, event order and counts.
- **The default flips to Rust at the end of 2-09,** when the server passes on it. From then CI's Python-backend job runs only `tests/python_reference.txt`: the Python runner of the rule scripts on the Python engine, the parity test and the parameter-schema test. 2-12 deletes the switch.

#### P2.6.7 The build and the dev loop

```toml
[build-system]
requires = ["maturin>=1.9,<2"]
build-backend = "maturin"
[tool.maturin]
python-source = "."
module-name = "citar._engine"
manifest-path = "crates/citar-py/Cargo.toml"
features = []                            # never test-ops (xtask check); maturin sets the extension-module flag itself
include = [ ... web, migrations, data (until the data move), collectors ... ]
exclude = ["**/__pycache__/**", "**/*.pyc"]
```

- **The version is Cargo's** (`[project] dynamic = ["version"]`); `cargo xtask check` already holds the workspace version equal to `citar/__init__.py`'s `__version__`. Dependencies are unchanged, so `requirements.txt` and `scripts/sync_requirements.py` are unaffected.
- **`.gitignore`** gains `citar/_engine*.pyd` and `citar/_engine*.so` (2-00a), so no agent commits a 20 MB binary.
- **The laptop dev loop is `cargo xtask develop [--release]`** (2-06a). `maturin develop` would write the extension into the OneDrive checkout (§2.7's os error 32 and sync churn) and a running server would hold it. Instead develop:
  - refuses unless the active venv belongs to this worktree (one venv per worktree, so two lanes never test each other's build);
  - installs the project's dependencies into it when `pyproject.toml` changed;
  - builds `citar-py` with cargo into the worktree's `CARGO_TARGET_DIR` (outside OneDrive), with `--features test-ops` and the `ci` profile, or `--release` for timing, and the build label from `git describe`;
  - copies the library to `<target>/citar-ext/_engine.<ext>`, renaming a locked old copy aside;
  - writes `citar-dev.pth` into the venv, which sets `CITAR_EXT_DIR`.

  `citar/__init__.py` puts `CITAR_EXT_DIR`, when set, first on the package's `__path__`, so `import citar._engine` finds the built library while the sources come from the checkout. CONTRIBUTING says to stop the dev server and the lab before a rebuild on Windows.
- **`pip install -e .`** also works and builds into the tree, which is fine in CI and on Linux.

#### P2.6.8 CI

- **rust.yml** (2-00a): `actions/setup-python` before the build (PyO3 needs an interpreter config); nextest and the doc tests run with `--exclude citar-py` (its library has no Rust tests and would link libpython); the lint job's clippy and `cargo doc` cover it. determinism.yml and nightly.yml build single packages (`-p citar-testkit`, `-p citar-bench`) and are unaffected. Path filters gain `crates/citar-engine/data/**` after the move.
- **test.yml** (2-06b):
  - a `build-ext` job per OS (ubuntu, windows, macos) builds the abi3 test wheel once (`PyO3/maturin-action`, `--profile ci --features test-ops`, rust-cache, the pinned toolchain) and uploads it;
  - each test job downloads its OS's wheel, installs its dependencies, and unpacks `citar/_engine*` into the checkout (`scripts/ci/unpack_ext.py`), so the checkout's sources are tested with the built extension and the three ubuntu Pythons share one build;
  - the `package` job builds a release-profile wheel without test operations, `manylinux: 2_28` explicit (the Docker base and the VPS have glibc 2.36 or newer), installs it in a clean venv, imports `citar._engine` from site-packages, checks the `abi3` tag and runs `citar doctor`;
  - the `installer` jobs use the build-ext wheels with `install.sh --from`;
  - the `docker` job puts the manylinux wheel in `wheelhouse/` and the Dockerfile installs `wheelhouse/*.whl` (`.dockerignore` keeps excluding `dist`).
- **docs.yml** installs the pinned toolchain with rust-cache before `pip install -e ".[dev]"`.
- **release.yml** is not reworked before Phase 5; 2-06b adds a first step that fails with "release packaging is Phase 5" while `__version__` is `0.1.6`.

**As built in 2-06a: citar-py** (P2.6.1-P2.6.4, P2.6.7; Appendix C). `citar._engine` binds every row of P2.6.1 but the v2 saves (2-11); `citar/_engine.pyi` describes every name, and a test holds the stubs and the module to the same names.
- **The crate** (`crates/citar-py/src/`): `lib.rs` (the module, the `Bytes` return type), `calls.rs` (the GIL, the calls in flight, the shutdown), `errors.rs` (the exceptions, `Failure`, the panic guard), `game.rs` (`Game`), `bot.rs` (`Bot`), `run.rs` (`run_game`), `funcs.rs` (the ruleset, tools, maps, scenarios, categories and bot versions). Every value that would be a dict or a list comes back as JSON bytes written by `to_py_json` (floats as Python writes them); JSON arguments go in as bytes. The negotiation heads come back as small dicts, built without JSON, since the session polls them. Commands return the events they appended as JSON bytes, each as `Game::event_json(ev, None)` gives it.
- **Where the binding differs from the table's wording:** `Game.load(state_json, chunks)` returns the game and the load report (`rules_changed`, `chronicle_incomplete`, `engine`), which the facade logs; `Game.save()` is `to_save` until 2-11 (the state and the whole history as one journal chunk, `Game::save_whole`, which leaves the game's own journal cursor where it was), `state_json()` is `state_dict`, and `digest()` is added for tests; `Game.end_turn(pid)` binds the host's `end_turn`; `drive` returns the stop (`{stop, player, negotiations}`), the events and `{pid: {tool: [taken, refused]}}`, and `answer` returns `done` or `deferred`, the events and the bot's actions; `standings()` is keyed by the ids as text, as JSON keys are; `view_json(pid, event_limit, extra_json)` adds the host's keys to the view's `rest` and refuses one of the view's own (`ClientView::FIELDS`), and `replay_json(extra_json)` puts `id` and `name` first and gives each player its `seat` from `extra_json["seats"]` (a list by player id); `emit` takes the event's data as JSON naming players and objects by id; `take_violations()` hands the checks' findings to a host, and `set_checks(on)` (test-ops) turns every check on. `run_game(spec_json, {pid: Bot}, on_turn, on_event)` reads `config`, `labels`, `raise_errors` and `traceback_limit`, ignores `max_errors`, and with test-ops takes `test_panic: {player, turn}`, a seat whose bot panics from that turn, the crash record's way through Python. A bot for a seat that is no major civilization is a `ValueError`, a Python bot a `TypeError`; `Game.drive` refuses such a bot the same way, before anything moves. `on_event` is a listener, as `headless.play` made it one of the game's: Python's `Game.emit` swallowed a listener's `Exception` unseen, so the lab's `listen`, which indexes each event's data, never ended a game over one event; the binding reports such an exception through `sys.unraisablehook` and delivers the next event. An exception from `on_turn`, which `headless.play` called directly, ends the run and is raised, and so does any `BaseException` that is no `Exception` from either hook. The run checks for signals between steps (`Python::check_signals`), so Ctrl-C stops a run with no hooks (`balance.py`'s, `citar sim`'s) within a step, as it stopped Python's loop, rather than at the game's end.
- **Engine additions** (engine tests in `api/host/tests.rs`): `api::host` with `Heads` and `NegotiationHead` (`Game::heads`, `negotiation_head`), `summary_json`, `player_row` and `majors_json` with Python's keys, `lobby_config` (Python's normalised config dict: `DEFAULT_CONFIG`'s keys in its order, rule objects by name, `resources` as the lobby's object or `null`, an editor map's id, the seats and every host key verbatim, the wraps; the server's `on_disconnect` and `reconnect_seconds` fall back to Python's `pause` and 180, as `DEFAULT_CONFIG` filled them in), `negotiation_record` and `negotiation_records` in the stored form, `event_by_id`, `event_rows`, `stats_rows`, `thought_rows`, and `save_whole`. Besides: `ErrCode::name` (`ActionError.code`), `Stop::name`, `player` and `negotiations` (the drive test operation uses them too), `ClientView::FIELDS`, and in citar-bot `Owners::owns_terms`, `owns_negotiation`'s rule for a negotiation held as a record.
- **The test operations** `eliminate` (its turn ends first if it is its turn, then its cities are destroyed, its units removed, and it is eliminated as a defeat eliminates one, so the last major standing may win by Domination; a game left with no major ends with no winner), `end_game` (won by `winner`, by `victory` or `Neutral`, with the winner's announcement, or with no winner) and `panic` (Rust only). `citar/engine/testops.py` gains the first two, and `tests/rules/hosts_eliminate_and_end_game.toml` holds both runners to the same answers.
- **Heads and the locks** (P2.6.2): a read takes the game's lock with the GIL released; a command does too, refuses a poisoned game with `EngineCrash`, and publishes `Heads` before it lets the lock go. The getters (`turn`, `current`, `phase`, `winner`, `victory`, `turn_limit`, `revision`, `poisoned`, `is_alive`, the open negotiations' heads) read `Heads` with the GIL held; `negotiation_head` of a negotiation no longer open reads it under the game's lock. Heads are published at the end of a call, so during a long drive they show where it began.
- **Panics** (P2.6.3-P2.6.4): every call of a game runs its work in `catch_unwind` inside the game's lock, so a panic poisons the game and never the lock (`Game._lock_poisoned`, test-ops, lets a test see the lock itself, since the binding reads through a poisoned one); every entry point that holds no game (the module functions, which parse saves, scenarios, maps and bot parameters; `Bot`'s constructor, `set_diplomacy`, `owns_negotiation`, `fingerprint`, `owners` and `params`; `build_info`; `run_game`'s first round) runs behind the same guard, `errors::guarded` with the GIL released or `errors::shielded` with it held, so a panic anywhere is an `EngineCrash`, never PyO3's `PanicException`, a `BaseException` that the server's and the lab's `except Exception` would let through. The module installs `citar_sim::panics`' hook at import, so the `EngineCrash` names where it happened. A poisoned game refuses every command, `add_thought`, `emit` and `set_difficulty` included, and still answers reads, views, the replay and saves. The briefing, the progress note and the empire summary are a major's: another player is a `ValueError`, not a question put to the engine.
- **Interpreter exit** (`calls.rs`): a call counts as in flight from releasing the GIL until it has it back, and so do `run_game`'s hooks, which run Python from inside a Rust call. The module registers `shutdown` with `atexit` itself, so every importer is covered and the facade needs no hook of its own: it marks the process as exiting and waits, with the GIL released, up to 5 s for the count to reach zero; from then on a thread other than the caller that starts a call, ends one or returns to a hook parks for good with the GIL released, so no thread re-attaches to a finalizing interpreter (Python 3.14's own behaviour). The thread that called it goes on calling, so later exit hooks may still save games.
  - **The abort P2.6.4 feared does not happen in this build, hook or not.** On Python 3.11 to 3.13 a thread that re-attaches to a finalizing interpreter is ended inside `PyEval_RestoreThread`: on Linux by `pthread_exit`, a forced unwind through the Rust frames below it, on Windows by `_endthreadex`, which unwinds nothing (3.14 hangs the thread instead). PyO3 0.29's `PyEval_RestoreThread` (`pyo3-ffi`'s `ceval.rs`) wraps the call in a guard whose drop parks the thread for good, so the unwind stops there (rust-lang/rust#135929): the thread hangs, as 3.14 makes it. On WSL's 3.12 with the hook unregistered, the three daemon threads of a run were still there a second into the window, asleep with no context switch, and the process exited 0.
  - **The hook stays** for what PyO3's guard does not give: the exit waits for the calls in flight, so a daemon thread's call (a save being written, from 2-11) completes instead of being cut off as the process ends, and the threads stop at the binding's own place, before they re-attach, without relying on a private wrapper of PyO3's. It costs a lock per call.
  - **The check** is `tests/engine_exit_child.py check`, which the suite and CI's interpreter-exit job (2-06b) run. Each mode exits mid-drive and opens a window during finalization: an object that only the interpreter's last collection frees (its own cycle, made with the collector off) waits half a second on a lock with the GIL released, long enough for any call to end, and prints whether the interpreter was finalizing and the calls in flight as the window opened and closed. A guarded run passes only with exit 0, the window opened (`sys.is_finalizing()` true) and no call in flight in it; the `--unguarded` runs are reported, not judged. They exit 0 too, with their calls still counted (the threads hung inside PyO3's guard on Linux, ended on Windows, hung by CPython on 3.14). The check's first form put a sleeping finalizer on a module global, which never ran: a daemon thread's frame keeps `__main__`'s globals alive past the end, so the window it claimed never opened, and the check could not fail. The waiting is on a lock, not `time.sleep`, which raises during finalization on Windows' 3.14.
  - The extension prints a caught panic to stderr through the chained hook, as Python printed its tracebacks.
- **The dev loop** (`xtask/src/develop.rs`): `cargo xtask develop [--release] [--venv DIR]`. A venv belongs to the worktree its `citar-dev.pth` names on its first line (the checkout, put on `sys.path`; the second line sets `CITAR_EXT_DIR` unless the environment does), and a venv without one is claimed; another worktree's venv, a venv inside the checkout, an unset `CARGO_TARGET_DIR` or one inside the checkout are refused (exit 2). The dependencies are installed with pip when `pyproject.toml` differs from the copy the venv keeps (`[project]`'s and the `dev` extra's, the project's own extras expanded). The build goes to `$CARGO_TARGET_DIR/develop`, not the test builds' directory: `CITAR_BUILD_ID` (the label, from `git describe --tags --always --dirty`) is read by the engine at compile time, so one shared directory would rebuild the engine on every switch between `develop` and nextest. Both profiles build with test-ops (`--release` is for timing). The library is copied to `$CARGO_TARGET_DIR/citar-ext/_engine.pyd` (`_engine.abi3.so` elsewhere); a copy a running process holds is renamed aside and removed by a later run. `citar/__init__.py` puts `CITAR_EXT_DIR` first on `__path__`. CONTRIBUTING has the loop.
- **Gates, run locally at the fix round's code head** (the GitHub jobs need a push, which a worktree does not make).
  1. `tests/test_engine_module.py` (31 tests, skipped without the extension): every binding on a seeded duel, results decoded, the errors mapped (`ActionError` with its code, `ValueError`, `MapError`, `LoadError`), every read of every player kind leaving the game unpoisoned, a concluded deal and a real route, `drive` refusing a city-state's bot and an unknown seat's, the stubs equal to the module's names; a poisoned game raises `EngineCrash` from every command and answers `summary`, `view_json`, `replay_json` and `save`.
  2. Two threads each driving 4-bot small games of 60 rounds, a seat per call, stopped together within a call of one second: process CPU over wall time 1.97 to 1.98 in three runs on Windows and 2.00 on Linux (gate: 1.6); a counting thread beside whole-game drives (a tenth of a second each, which a drive holding the GIL would take from it whole) kept 90 to 99% of its solo rate on Windows and 94 to 99% on Linux (gate: half); 1,000 `turn` polls during a whole-game drive took at most 4.7 µs each on Windows and 4.8 µs on Linux (median 0.1 µs), and the drive ended. The test's earlier form let one thread finish a game alone after the second, and once measured 1.56 in the review's run.
  3. The panic test operation raises `EngineCrash` naming `testops.rs`, every command then raises `EngineCrash`, the game's own lock is not poisoned (`Game._lock_poisoned()` is false), reads and saves answer, and another game plays on.
  4. `run_game` hands every event once, in order: the delivered events equal, dict for dict, the chronicle after its creation of the same game played as the runner plays it (one driven seat a step through `Game.drive`), whose stats equal the run's; `on_turn` hears each turn once, the first turn and the one the game ended on included; the same spec gives the same result; a listener that raises on every other event hears every event, each exception reported through `sys.unraisablehook`, and the game ends; a `BaseException` from it ends the run; Ctrl-C (`_thread.interrupt_main`) stops a 500-round run with no hooks within a step; `test_panic` gives `T4 P0 careless: panic: ...` in `errors` with phase `playing`, and with `raise_errors` an `EngineCrash`. `set_diplomacy` changes the handle in place: `owns_negotiation` follows it, and `answer` with the same handle defers the chat the model now owns.
  5. Partly met. `tests/engine_exit_child.py check` passed 3 times out of 3 on each of WSL's Python 3.12.3 and Windows' 3.11, 3.12 and 3.14: `long`, `short` and `barred` exit 0, each `long` and `short` run with the window opened during finalization and no call in flight in it (`in_flight=0->0`); the unguarded runs exited 0 every time with their calls still counted (`1->1`). Not run: Linux with Python 3.11 and 3.13, which WSL lacks, and installing them means downloading an interpreter, which this package did not do unasked. CI's interpreter-exit job (2-06b) runs the check on each version.
  6. `cargo xtask develop` built into `C:/dev/target/citar-p-2-06a/develop` and `citar-ext/` with nothing under `citar/` or anywhere in the checkout (`git status --ignored`); a venv whose `citar-dev.pth` names another worktree, no venv, and a target directory inside the checkout were each refused with exit 2 (in the last case cargo builds the `xtask` binary itself into that directory before develop runs: `cargo xtask` is an alias of `cargo run`); the suite imported `_engine` from `CITAR_EXT_DIR` (a test checks it).
  7. Windows: fmt, clippy over the workspace with every feature, the engine alone in rust.yml's six feature sets and in release, citar-py with and without test-ops and in release, the fuzz crate's check, `cargo xtask check`, `cargo doc -D warnings`, nextest in the ci profile (1,247 passed, 2 skipped; no corpus in the worktree), the doctests, the ci and dev builds, `cargo golden check` in ci and release (all 15 sets `ok`, the hashes unchanged), refcheck on the fixtures with the ratchet holding and `--strict` over the 262 states with the corpus (clean), the Python suite in the venv with the extension (640 OK, 2 skipped), ruff and the strict link check. Linux (WSL Ubuntu 24.04, the branch from a bundle, no corpus): fmt, clippy, doc, `cargo xtask check`, the dev build, nextest (1,247 passed, 2 skipped), the doctests, the fuzz check, citar-py built (its `NEEDED` entries libgcc_s, libc and ld-linux) and `tests/test_engine_module.py` under Python 3.12 (31 OK). The build id is `11cefb3edf0a` on both, unchanged: the round touched neither the engine nor the bot. Not run: macOS and every GitHub job.
- **Fix round** (the review's seven findings). Each fix but the panic guard's has a test, and each new test was seen to fail on a build with its fault put back: the signal check removed, the listener's exceptions propagated, the last step's events withheld, a panic unwinding through the game's lock, a drive holding the GIL (the counting thread kept 27%), `drive` resolving seats with `player_of`. The exit check failed as it should while its finalizer still slept with `time.sleep`, which raised on Windows' 3.14 before the window opened.
  1. The interpreter-exit check could not fail, and the hazard it claimed to show did not occur: as above, the check now opens its window and fails when it did not, and DESIGN, `calls.rs` and the check say why the unguarded runs exit 0 (PyO3's own guard). Gate 5 stays partly met for want of Linux's 3.11 and 3.13.
  2. `run_game` abandoned the game on an exception from `on_event`, where Python's listeners were swallowed: it is a listener again (above).
  3. A run with no hooks never checked for signals, so Ctrl-C waited for the game's end: it checks between steps.
  4. Entry points that hold no game ran without the panic guard, so a panic there would have been PyO3's `PanicException`: every one runs behind it now. No input found makes one panic today, so this has no test of its own.
  5. Gate 4's count was never compared with the chronicle: it is now, event for event, against the same game replayed.
  6. Two gate tests could not fail in their case: the counting thread now runs beside whole-game drives, and the lock's state is read with `Game._lock_poisoned`. The two-core test's threads also stop together now (above).
  7. `Game.drive` accepted a bot for a city-state: it refuses it, as `run_game` does.

**As built in 2-06b: packaging and CI** (P2.6.7, P2.6.8, P2.9; Appendix C).
- **`pyproject.toml` builds with maturin** (`maturin>=1.15,<2`): `python-source = "."`, `module-name = "citar._engine"`, `manifest-path = "crates/citar-py/Cargo.toml"`, `features = []`, `[project] dynamic = ["version"]` (Cargo's), and hatch's data list as `include` entries for the wheel and the sdist. maturin honours `.gitignore` in the package folder, so the wheel holds exactly the files git tracks under `citar/`, the library (`_engine.pyd`, `_engine.abi3.so`) and the dist-info. Where it differs from P2.6.7's block:
  - **The floor is 1.15,** the version tested: `editable-profile` needs 1.10, wheel include patterns resolve against the project root from 1.12, and 1.13 reworked the sdist of a workspace.
  - **`editable-profile = "ci"`:** `pip install -e .` builds the ci profile (2 minutes cold on the laptop), `pip install .` and `maturin build` the release profile (6 minutes cold).
  - **No `locked = true`.** maturin's sdist keeps only the five crates the extension needs and rewrites the workspace's members, so cargo prunes the other members' packages from its `Cargo.lock` (155 packages to 89, every kept version unchanged), which `--locked` refuses. CI passes `--locked` on the command line instead.
  - **The sdist** carries, beside the package and the five crates, `rust-toolchain.toml` (a build from it uses the pinned toolchain, so its games are the goldens' games), the tests, the scripts, refcheck's committed answers (`intended.toml`, `query_tools.json.gz`, both fixture sets; never the corpus), and the docs, deploy and installer files, `CHANGELOG.md`, `DESIGN.md` and `alembic.ini` that hatch's carried. maturin's include globs match the files on disk, not git's index, so `installer/output` and `installer/build` are excluded by name (a laptop's ignored build output was seen to slip in before that).
- **`cargo xtask check`** gains `check/pyproject.rs`: the maturin table must build `crates/citar-py/Cargo.toml`; no listed feature may reach citar-py's `test-ops` or anything in the engine's `legacy` or `test-ops` closure, named as citar-py's own feature (followed through its feature table), `citar-engine/x` or `citar-engine?/x`; `all-features = true` is refused; and `[project]` keeps `version` dynamic and sets none. That citar-py itself never enables `legacy` stays the `features` check's, which refuses it for every crate but the three test crates. From the fix round, `check/triggers.rs` too: rust.yml's `push` and `pull_request` path filters must match every file the checks read (each member's manifest, `Cargo.toml` and `Cargo.lock`, `xtask/check.toml`, `citar/__init__.py`, `pyproject.toml`, the three source trees, the generated file and the inputs its generator reads, which `Generated` now lists, and rust.yml itself), since a file they miss can change in a pull request that never runs the checks. It reads the workflow as text (events at two spaces, `paths:` at four, patterns at six, GitHub's `*` and `**`) and reports `paths-ignore`, negated patterns and inline lists rather than guess at them.
- **`scripts/ci/`.** `unpack_ext.py WHEEL_OR_DIR [--test-ops]` writes the wheel's one `citar/_engine` library into the checkout's `citar/`, removing any other `_engine` library there first; it refuses a directory without exactly one wheel, a wheel without exactly one library, and a library whose suffix this Python does not load (a Windows wheel on Linux and the reverse); then a fresh interpreter, without `CITAR_EXT_DIR`, imports it from the checkout, and with `--test-ops` it fails unless the test operations are there, so a test job can never skip the engine's tests unnoticed. `requirements.py EXTRA...` prints `[project]`'s requirements and the extras', the project's own extras expanded, for the jobs that must not build the package (the test jobs, the image's dependency layer). From the fix round, `check_dist.py wheel WHEEL_OR_DIR [--tag T] [--library L]` and `check_dist.py sdist SDIST_OR_DIR` hold a built file to the checkout it was built from (`git ls-files`): a wheel has one `Tag` (`T`, or any `cp311-abi3` one) that its file name repeats, one `citar/_engine` library (`L`), under `citar/` exactly the files git tracks there besides it, everything else in its one dist-info, and no member twice; a source distribution has one `citar-<version>/` folder holding only tracked files and `PKG-INFO`, every tracked file under `citar/`, every tracked file an sdist `include` names and no `exclude` takes away (the patterns read from `pyproject.toml`), and `pyproject.toml`, `Cargo.toml`, `Cargo.lock` and citar-py's manifest. `tests/test_ci_scripts.py` tests all three.
- **test.yml** (P2.6.8):
  - `build-ext` on ubuntu, windows and macos: the pinned toolchain, rust-cache (`build-ext`), `PyO3/maturin-action` with `--profile ci --features test-ops --locked`, `check_dist.py wheel` with the OS's library (the installer jobs install this wheel), and the upload as `ext-<OS>`. On Linux it builds on the runner (`manylinux: off`, a `linux_x86_64` tag) so the cache serves it; only the package job's wheel is manylinux.
  - The five test jobs install `requirements.py all` and unpack their OS's wheel with `--test-ops`, then run the steps they ran before. They, `interpreter-exit` and `installer` need `build-ext` with `if: ${{ !cancelled() }}` (fix round): a matrix need otherwise waits for every leg, so one OS's failed build skipped the suite on all three; now each job fails on its own OS alone, at the download of a wheel that was never uploaded.
  - Every `maturin-action` step builds with `maturin-version: ${{ env.MATURIN_VERSION }}`, `v1.15.0` (fix round). Without it the action resolves `pyproject.toml`'s `maturin>=1.15,<2` to the newest release in its manifest, so a minor release of maturin, whose reading of `include` and `exclude` has changed before, would change what the files hold unannounced. docs.yml's editable install keeps taking the newest maturin that `pyproject.toml` allows, as a build from source does.
  - `interpreter-exit` moved from rust.yml: the Linux test wheel unpacked once, then `tests/engine_exit_child.py check` under 3.11 to 3.14.
  - `package`: the sdist (maturin, on the runner, before the container leaves `target/` owned by root) and the release wheel in the manylinux_2_28 container; `check_dist.py` on both, the wheel's only tag `cp311-abi3-manylinux_2_28_x86_64` and its library `citar/_engine.abi3.so`; the clean venv, `citar --version`, `where` and `doctor --quiet`, `citar._engine` imported from site-packages with no test operations and the package's version, the data checks it had; `twine check` of both; the wheel uploaded as `wheel-manylinux`.
  - `installer` (ubuntu, macos) installs the build-ext wheel with `install.sh --from`; `docker` downloads `wheel-manylinux` into `wheelhouse/`, builds the image, waits for its health route and checks the image's engine has no test operations.
- **rust.yml** loses the interpreter-exit job, its path filter and the "Build, citar-py included" step: build-ext links the extension on all three OSes on every push and pull request (test.yml has no path filters). Its path filters gain `pyproject.toml` (fix round), whose maturin table `cargo xtask check` holds: before, a pull request that changed only that file never ran the check.
- **docs.yml:** the pinned toolchain and rust-cache before `pip install -e ".[dev]"`. The site is built in one job and deployed in another that runs on main only, the wiki is pushed from main only, and the concurrency group is per branch. Not in the scope's words, but gate 2 needs it: the `github-pages` environment's branch policy refuses a deployment from another branch, so a `workflow_dispatch` of the branch failed at the deploy, and had it not, it would have published the branch's pages and wiki.
- **release.yml:** `verify`'s step after the checkout prints `::error::release packaging is Phase 5` and fails while `__version__` is 0.1.6 or earlier; every other job needs `verify`. The scope says "while `__version__` is 0.1.6", but the branch carries 0.1.5 until the release bumps it (`cargo xtask check` holds Cargo's version equal), so an exact match would let a run of the branch through, and gate 4 asks that it fail there.
- **Docker:** the builder installs `requirements.py server` from `pyproject.toml`, then `citar` with `--no-deps --no-index --find-links` from the wheel in `wheelhouse/`, and imports the extension; it never compiles Rust. `COPY pyproject.toml wheelhouse* ...` lets a build without a wheel fail with a message saying how to get one (the CI artifact, or maturin's manylinux container) rather than Docker's own. `.gitignore` ignores `wheelhouse/`; `.dockerignore` still excludes `dist`.
- **Docs:** CONTRIBUTING's setup says a Rust toolchain is needed and what an editable install builds, and its Rust section how CI builds and unpacks the extension and how to run the suite as a test job does; INSTALL's "From source" and README's contributing lines say the same.
- **Gates, run locally** (a worktree pushes nothing, so no GitHub job ran: gates 1 to 3 are CI's, and what stands in for each is below).
  1. Not run: maturin-action, the manylinux container, macOS, install.sh and Docker. Run instead: the Windows release wheel (`maturin build --release --locked`, 6 minutes cold) is `citar-0.1.5-cp311-abi3-win_amd64.whl`, tag `cp311-abi3-win_amd64`, holding the 213 files git tracks under `citar/`, `_engine.pyd` (10 MB) and the dist-info, with no duplicates; installed with `--no-index` in a clean venv (the laptop's packages for the dependencies), it imports `citar._engine` from site-packages without test operations, build id `11cefb3edf0a`, and `citar doctor --quiet` exits 0. The ci test wheel (`--profile ci --features test-ops`, 2 minutes) unpacked by `unpack_ext.py --test-ops` into a clone, where the module's and the scripts' 38 tests pass under Windows' 3.12 and the exit check under 3.11, 3.12 and 3.14; the release wheel is refused there for lacking the test operations and taken away again. On Linux (WSL, the system 3.12) the ci library, zipped as a `linux_x86_64` wheel, unpacked the same way, and the 40 tests and the exit check pass. Every workflow parses, every downloaded artifact is uploaded by a job its job needs, and the steps the gates name are where they belong (a script read the YAML).
  2. `mkdocs build --strict` passes in the fresh clone of gate 6, its editable install the one docs.yml makes, and the wiki generates. The `workflow_dispatch` run is CI's.
  3. Windows: fmt, clippy over the workspace with every feature and of the engine in rust.yml's six feature sets and in release, the fuzz crate's check, `cargo xtask check`, `cargo doc -D warnings`, nextest in the ci profile (1,305 passed, 2 skipped), the doctests and `cargo golden check` (all 15 sets ok). WSL: fmt, clippy, `cargo xtask check` and xtask's tests, the docs, and citar-py's ci build. The Rust code the package touches is xtask's alone.
  4. The step's `run` block, read out of release.yml and run with `bash -e` in WSL, prints `::error::release packaging is Phase 5` and exits 1 at 0.1.5, also at 0.1.6, and 0 at 0.1.7.
  5. `features = ["test-ops"]` in pyproject.toml: `cargo xtask check` exits 1 with ``[pyproject] pyproject.toml: [tool.maturin] features lists `test-ops`: it turns on citar-py's `test-ops` (the engine's test operations), which no wheel carries ...``. Unit tests cover the other spellings (`citar-engine/test-ops`, `citar-engine?/test-ops`, `citar-engine/legacy`, `citar-py/test-ops`, a feature of citar-py's that reaches one), `all-features`, the manifest path and the version.
  6. A fresh clone outside OneDrive, a new venv, `pip install -e ".[dev]"` with `--no-build-isolation` (maturin 1.15 and every dependency already on the laptop, so nothing was downloaded; with isolation pip would fetch maturin) built the extension in 2 minutes into `citar/_engine.pyd`, which git ignores, and the Python suite passed: 647 tests, 6 skipped (the 3 needing test operations, the dev loop's, and 2 that depend on their seed's map).
  - Besides: the suite with the dev loop's extension (test operations on) passes in the worktree (647, 2 skipped, before the last two tests were added), and the source distribution, extracted and installed editable, builds and passes the suite (649, 6 skipped).
- **Fix round.** The review's three findings, each fixed:
  - *`pyproject.toml` was missing from rust.yml's path filters* (major), though `cargo xtask check`, which runs only there, holds its maturin table: a pull request changing that file alone (a `legacy` feature, a static version, another manifest path) ran no check. Both filters list it now, and `cargo xtask check`'s new `triggers` check (above) reports any file the checks read that a filter misses, so the next such file fails the check rather than slipping by. With the two `pyproject.toml` lines taken out of rust.yml, `cargo xtask check` exits 1 with a `[triggers]` finding for each of `push` and `pull_request`, on Windows and in WSL; `rust_yml_runs_on_every_file_the_checks_read` does the same against the real file.
  - *A matrix need skipped every OS when one build-ext leg failed* (minor): `test`, `interpreter-exit` and `installer` now carry `if: ${{ !cancelled() }}` beside `needs: build-ext`.
  - *maturin floated* (minor): the three `maturin-action` steps take `MATURIN_VERSION` (`v1.15.0`; maturin-action v1 downloads that tag's binary, which exists for the three runners' platforms), and `check_dist.py` (above) checks build-ext's wheel on each OS and the package job's wheel and source distribution, so a change of maturin or of the lists that drops or adds a file fails CI. CONTRIBUTING names the pin and the script.
  - *Every gate again*, at `13540ef` (DESIGN.md alone changes after it). Gates 1 to 3 remain CI's.
    1. With maturin 1.15.0: the sdist, the release wheel and the ci test wheel built from the head, and `check_dist.py` passes each (213 tracked files under `citar/`, the library, the dist-info; the sdist's tracked files, includes and build files) and refuses the release wheel as the manylinux one and the first round's sdist (missing `scripts/ci/check_dist.py`). The release wheel in a clean venv: `citar._engine` from site-packages, no test operations, build id `11cefb3edf0a`, version 0.1.5, the package's data, the state directory outside site-packages, `citar doctor --quiet` exit 0; `twine check` passes both files. A test job as closely as Windows allows: a fresh clone, the ci wheel checked and unpacked with `--test-ops`, the module's and the CI scripts' tests on 3.12 (51, 1 skipped: the dev loop's), the whole suite with the test operations on 3.14 (660 tests, 3 skipped: the dev loop's and the 2 seed-dependent ones), and the exit check on 3.11, 3.12 and 3.14. WSL (3.12): a `linux_x86_64` wheel of the ci library holding every tracked `citar/` file passes `check_dist.py` and is refused as a manylinux one, the Windows-built sdist passes against the Linux clone, the library unpacks with `--test-ops`, and the 51 tests and the exit check pass. The workflow script's earlier checks hold, and the new ones: `pyproject.toml` in both of rust.yml's filters, the three jobs' `needs` and `if`, `MATURIN_VERSION` on all three maturin steps and no maturin-action elsewhere, and the `check_dist.py` calls before the uploads.
    2. In gate 6's fresh clone and its editable install, `mkdocs build --strict` passes and the wiki generates.
    3. Windows: fmt, clippy (the workspace with every feature, the engine in its six feature sets and in release), the fuzz crate's check, `cargo xtask check`, xtask's 90 tests, `cargo doc -D warnings`, nextest in the ci profile (1,311 passed, 2 skipped), the doctests, a 60-second chaos run (109 games, 0 failures), refcheck on the committed fixtures (clean) and its ratchet (holds), and `cargo golden check` (15 sets ok). WSL: fmt, clippy, `cargo xtask check`, xtask's tests, the docs and citar-py's ci build.
    4. In WSL, the step's `run` block read out of release.yml prints `::error::release packaging is Phase 5` and exits 1 at 0.1.5 and at 0.1.6, and 0 at 0.1.7.
    5. `features = ["test-ops"]`: `cargo xtask check` exits 1 with the `[pyproject]` finding.
    6. A fresh clone, a new venv, `pip install --no-build-isolation -e ".[dev]"`: the extension built in 2 minutes into `citar/_engine.pyd` (ignored by git), and the suite passed, 660 tests, 6 skipped (the 3 needing test operations, the dev loop's, 2 seed-dependent); the test jobs' scripts and ruff pass there. In the worktree, the dev loop's extension passes the module's and the CI scripts' 51 tests with none skipped.
  - *Not changed:* docs.yml's `pip install -e ".[dev]"` keeps resolving `maturin>=1.15,<2` as a build from source does, so a maturin release that breaks the editable build shows there first; its wheel contents are never shipped.
- **Left for later.** CI wheels carry the label `unknown` (`CITAR_BUILD_ID` unset): setting it per commit would rebuild the engine on every build-ext run, and the tag's label is Phase 5's. `citar doctor` says nothing about the extension yet: it may only reach it through the facade, whose Rust names are 2-08's. Phase 5's release.yml should build with `MATURIN_VERSION` and run `check_dist.py` on what it publishes.

### P2.7 The swap

#### P2.7.1 The server

- **Bot seats go through `drive`, and every drive goes through the session's side effects.** `BotAgent.play_turn`, under the session lock:
  1. records `before = (turn, current)`, then calls `game.drive({pid: bot for every bot seat}, seat_limit=1)`. Passing every bot seat lets a negotiation one bot opens with another be answered inside the same drive;
  2. sets the metrics record's `end_reason` to `end_turn` if the drive ended the turn, records the drive's action counts on the turn row (`bot_actions`), and calls `session._after_action(*before)`: the version bump, `_track_turn`, the responders for model seats, the `turn` and `update` broadcasts and the autosave;
  3. on `awaiting_reply`, the `_after_action` call has already started the model seat's responder; the agent waits on `cond` as today (90 s; Phase 3E replaces it with rule T3's lobby timeouts), drives again when woken, and calls `_after_action` after every drive. When the wait runs out it closes those chats as `expired`, "(no reply in time)", through `close_negotiation` (which runs `_after_action`) and drives once more, which ends the turn.
- **The rest of `_drive` keeps its shape.** A seat whose turn the drive already ended fails the `turn_marker` check, so the driver's own `end_turn` is skipped.
- **Responders.** `_dispatch_negotiation_interrupts` and `_run_responder` stay. A bot seat's answer is `game.answer(pid, nid, bot)` followed by `_after_action`, so a human's or model's proposal answered by a bot is broadcast and wakes waiters. A refused answer (the chat moved) is ignored.
- **Crashes.** A `GameSession` that catches `EngineCrash` anywhere (`call_tool`, the driver, a responder, a view) calls `_crashed(message)`, which:
  - records `crashed = {message, turn, at}`, pauses, stops the driver and cancels every agent;
  - emits nothing on the game (a poisoned game refuses) and broadcasts `{"type": "crashed"}`;
  - writes the poisoned state once as `crash-<turn>.citar` for debugging, and never autosaves over the last good save;
  - keeps reads working (summary, views and the replay of a poisoned game) and shows the state in `info()` and the lobby.

  `_drive`'s generic error handler no longer calls `game.emit` on a game that may be poisoned. A test drives the `panic` test operation through `call_tool` and through a bot drive, and checks each point.
- **Metrics.** Bot seats get a turn row per turn, with `end_reason` and their action counts, but no per-tool rows (their actions do not pass through `call_tool`); the metrics and reports code tolerates it. LLM seats are unaffected.
- **Views and the replay as bytes.** `/api/games/{gid}/view` uses `EngineGame.view_json(pid, extra)`, which splices `seat`, `session`, `version` and `spectator` into the engine's bytes, and the route returns them as they are: a gargantuan god view otherwise costs a parse and a re-dump of 2.6 MB per refresh. `/replay` does the same with `replay_json(extra)`, which splices `id`, `name` and each player's `seat`.
- **Saves.** Until 2-11 saves keep the gzip JSON file, with Rust's `to_save()` as `{"state": <state dict>, "journal": <the whole history as one chunk>}`, built fresh under the lock (slow on large games, but never torn). From 2-11 saves are the v2 container through the writer (P2.5.3).

#### P2.7.2 Agents, MCP, probes, benchmarks and reports

- **Agents and MCP** reach the game only through the facade (`tool_list`, `tool_kind`, `execute`, `briefing`, `add_thought`, `negotiation_view`, the text constants). They need no change beyond the facade's shapes, which the parity test holds.
- **Probes** create games from scenarios, whose `state` is the engine's own save JSON (§4.9; `scenario_summary` reads it), and use `apply_ops`, `force_turn` and `open_negotiation_as`. Scenarios from 0.1.5 were archived in Phase 0.
- **Benchmarks, scoring and reports** read saves through the facade: scores from `state_summary`, from 2-11 from the container header's `summary`.

#### P2.7.3 The lab, profiles, the Bots page, `sim` and `balance`

- **The lab** (`lab.py`): `normalize` resolves profiles to a version (`basic` becomes `LATEST`) instead of freezing code; `play()` keeps its body over `engine_api.run_game` with facade bots and prints its `PROGRESS` lines from `on_turn`; results record per seat the build id, bot version, profile, revision and fingerprint computed at play time, so a queued experiment that runs on a newer build is labelled with the build that played it; `engine_hash()` becomes the build id; the subprocess-per-game runner stays until Phase 4.
- **Profiles and the Bots page** read versions, schema, cleaning and fingerprints through the facade (P2.8). `bots_api`'s `/engines` and `/schema` serve them unchanged in shape.
- **`sim.py` and `balance.py`** keep `run_game`. Their bots are facade handles and the `seed` arguments are ignored.

#### P2.7.4 The Python suite on the Rust backend

- **Markers** (`tests/backends.py`): `RUST = engine_api.BACKEND == "rust"`; `@python_engine_only("<successor>")` for a test that pokes the Python engine, whose behaviour now lives in the named rule script, Rust test or Python test; `@rust_pending("<package>")` for a test the named package will make pass on Rust; `@rust_only` for a test of a Rust-only name.
- **The meta-test** (`tests/test_backends.py`) checks that every successor exists (a `tests/rules/*.toml`, a test name in `crates/**/*.rs`, or a Python test id). Each package's gate includes `git grep` finding no `rust_pending("<its id>")` and no script `needs` naming it.
- **CI** runs the suite with `CITAR_ENGINE=rust` from 2-08, and the default flips in 2-09 (P2.6.6).

| Modules | What happens |
|---|---|
| `test_engine`, `test_mechanics`, `test_barbarians`, `test_events`, `test_single_player`, `test_negotiation_chat`, `test_controllers`, `test_mapgen` | Their engine tests became Phase 1's rule scripts and native Rust tests (§9.3). They are marked `python_engine_only` with successors and deleted in 2-12. Their server-side tests (`update_seat`, the driver closing chats, about 14) are rewritten onto the facade. |
| `test_bots`, `test_bot_diplomacy` | Ported to bot scripts (2-00b) made to pass by 2-01b, 2-03 and 2-05; the BotAgent tests are rewritten for `drive` in 2-09. |
| `test_session`, `test_benchmarks`, `test_access`, `test_worker` | The `python_game` pokes become the `eliminate` and `end_game` test operations; otherwise backend-neutral. |
| `test_editor`, `test_ui_support`, `test_providers` | Moved from `citar.engine.{maps,scenario,tools,...}` onto facade functions |
| `test_engine_api` | Bots are handles. The frozen-bot and Python `Crasher` cases are replaced: a version id check, and the `panic` test operation reaching `run_game` as a crash record (and as `EngineCrash` with `raise_errors`). |
| `test_rule_scripts` | On Rust the Python runner plays every script through the bindings and runs the `intended` checks. The Python `testops.OPS` check and the `--check` re-recordings of `tool_list.json` and `query_tools.json` stay in the Python reference subset until 2-12 deletes them. |
| `test_bot_profiles`, lab tests | Versions instead of frozen engines (2-10) |
| `test_auth`, `test_pool`, `test_costs`, `test_sharing`, `test_paths`, `test_setup`, `test_metrics` | Unchanged; they pass on both backends |

### P2.8 Removal, bot versions and the new ladder

#### P2.8.1 The archive tag and what must exist first

- **2-12 starts by checking that everything recorded from Python is safe:**
  - committed: the fixtures and their answers, `tool_list.json`, `query_tools.json.gz`, the advisor recording and the bot-decision recordings of the committed states, the four baseline files, `basic-1.json`;
  - archived on the laptop under `saves/_archive_2026-10_python-reference/` (inside the OneDrive-synced checkout, git-ignored) with a committed checksum list `refcheck/corpus.sha256`: the 250-state corpus with its answers and the local `CITAR_ADVISOR_DUMP` and `CITAR_BOT_DUMP` files, which 2-13's full-corpus runs read and which nothing can regenerate;
  - no `rust_pending` marker, no script `needs`, no `explained.toml` entry marked `pending`.
- **The tag.** At 2-12's merge the orchestrator tags the base `python-engine-0.1.6` (annotated: "the Python engine, the live bot basic.py and the 18 frozen bots, as of the swap") and pushes it with the branch. The repository is public and all of it is already on `main` and in the v0.1.5 wheel, so the tag exposes nothing new.
- **The lab history** (586 games) and the laptop's profiles are already archived under `saves/_archive_2026-09-23_pre-0.1.6/` (Phase 0).

#### P2.8.2 What is deleted

- `citar/engine/` (with `facade.py`, `testops.py` and `inspect.py`); `citar/bots/{basic,headless,idle}.py` and the 18 `frozen_*.py`; the `__path__` hack in `citar/bots/__init__.py`; the `CITAR_ENGINE` switch; `seed=` in the bot factories' callers.
- The tests marked `python_engine_only`, `tests/python_reference.txt` and what it lists that only made sense on Python, `tests/test_bot_params.py`.
- The Python recorders under `scripts/refcheck/` (`summarize.py` and the committed data stay), `scripts/check_refs.py`, `scripts/check_uniques.py`, `scripts/bots/`, and anything else importing `citar.engine` (found by `git grep -nE 'citar[./]engine([./]|$)'`, which does not match `engine_api`).
- CI: test.yml's "Ruleset integrity" step and the Python-backend job.

#### P2.8.3 What stays in Python

`citar/bots/profiles.py` (storage, revisions, `resolve`, `make_bot` over the facade); `citar/bots/ratings.py` (the Bradley-Terry fit, unchanged); `citar/lab.py` (queue, runner, `play`, reports, quiet hours); `citar/server/bots_api.py`, `citar/sim.py`, `citar/balance.py`, `citar/bench.py`, `citar/aggregate.py`; the whole server, agents, providers, pool, auth and reports; `scripts/refcheck/summarize.py`.

#### P2.8.4 The data move, and modding

- **What moves.** `citar/data/{ruleset,custom,game.json}` moves with `git mv` to `crates/citar-engine/data/`, and `rules::source::embedded()`'s paths change. `citar/data/collectors/` stays, because the Servers page offers it for download. The contents are unchanged, so `RulesetId` and every golden digest are unchanged; the golden check proves it. The sdist becomes self-contained (§11 risk 10). `doctor`'s message and test.yml's package check move to the module (`citar._engine.ruleset_counts()`).
- **Modding keeps working without a toolchain.** `docs/MODDING.md` tells modders to add a file under `citar/data/custom/`, restart, and run `check_uniques.py` and `check_refs.py`; with an embedded ruleset and those scripts gone that would silently stop working. So:
  - `CITAR_RULESET_DIR`, read once when `citar._engine` loads: a directory in the data layout (`ruleset/`, `custom/`, `game.json`) loaded with `Ruleset::load` and used for every game and ruleset function of the process; `build_info()` reports its `RulesetId`, so its games, saves and fingerprints are told apart from the embedded ruleset's;
  - `python -m citar ruleset check DIR` prints `RulesetErrors` (unknown uniques, broken references), replacing the two scripts;
  - MODDING.md and the CHANGELOG's Compatibility section say how.

#### P2.8.5 Bot versions

- **A version is a code module plus its parameter schema, compiled in:** `basic-1`, the port of today's `basic.py` (memory kind 1); `idle`, no parameters, no memory.
- **A new version is a deliberate copy.** A bot change that should not move existing results copies `src/basic1/` and `params/basic-1.json` to `basic2` and `basic-2`, makes the change there, and adds a `VERSIONS` row. A version is deleted once no queued experiment or saved profile names it.
- **`basic` names the latest version.** The `standard` profile follows it, and the lab pins it at submission. This replaces `profiles.freeze()`, which copied Python source.

#### P2.8.6 Profiles and fingerprints

- **A profile** is `{engine, aggression, params}` with its revision history. `engine` is `basic`, `basic-N` or `idle` (`ENGINE_RE = ^(basic|basic-\d+|idle)$`); a `frozen_*` engine is refused with "archived with 0.1.5".
- **The built-in profiles** are `standard` (`basic`, defaults), `classic-production` (`prod_mode = classic`) and `idle`. `snapshot-0922`, `v1` and `v0` pointed at frozen code and are dropped.
- **Validation is Rust's:** `clean_params` is `engine_api.bot_clean_params`; the Bots page's schema is `bot_schema` (373 parameters in 17 groups).
- **The fingerprint** is `hex(blake3("CITAR-BOT" ‖ build_id ‖ version ‖ canonical overrides ‖ fixed aggression or "seat"))[..12]`. It hashes the profile's fixed aggression, as Python did, never the value a seat plays with: lab seats get aggression by position (`0.25 + 0.5*((pid*37+seed)%10)/9`, ten values), and one profile must stay one entry per build and difficulty. With the content build id (P2.2.1), this is the owner's "pinned to engine build plus bot version": every behaviour change starts new entries, and two builds' results are never mixed in one.

#### P2.8.7 Ratings and the new ladder

- **The fit is unchanged.** Entries are keyed by fingerprint and difficulty.
- **Entries are named from the results themselves.** Each lab result records per seat the profile, revision, fingerprint, bot version and build, so `ratings.collect` no longer re-derives fingerprints by importing bot modules (ratings.py:100-106), and `profiles.engines()` no longer globs frozen files; both broke the moment the frozen bots went.
- **The ladder starts empty.** The first entries come from 2-10's gate experiment, then from the owner's experiments; `best` resolves as before.

### P2.9 Packaging: now and Phase 5

**Needed now:**
- `pyproject.toml` on maturin (P2.6.7) and the CI of P2.6.8 (2-06b).
- `.gitignore` for the extension (2-00a) and the Rust baseline outputs; `refcheck/.gitignore` narrowed (2-00a).
- The data move and the ruleset override (2-12).

**Left for Phase 5:**
- the wheel matrix: win-x64, macOS arm64 and x64, manylinux x64 and aarch64, built natively; an sdist build in CI; attestations;
- multi-arch Docker from wheels;
- install scripts that use wheels only; the Windows bundle carrying `_engine.pyd`; Scoop, winget and the helper-only Homebrew tap;
- the VPS deploy from the release wheel. Its systemd units (the server and `citar-lab`) run `python -m` from a `git archive` tree in `/opt/citar`, whose `citar/` would shadow an installed wheel and import without `_engine`: their `WorkingDirectory` must become the state directory, not a source tree;
- `BUILD_LABEL` from the tag, and release.yml's check of the Cargo version.

### P2.10 Performance

- **Where the time goes.** In Python the bot's own logic was about 10% of a game and its engine calls the rest. In Rust the engine part is already at least 133x faster per pass round, so the bot's own work and its pathfinding are the share to watch.
- **Expected cost.** Per bot turn about 1-5 ms on small maps (an advisor call 43 µs a city, a cold A* 25 µs, a combat preview 0.7 µs): 330 rounds × 4 bots is roughly 2-7 s plus the engine's 0.8 s, inside the 5 s target and well inside the 24 s gate.
- **If 2-07 finds a floor missed, the tuning order is:** (1) bot-side caches for one turn (the `Context`, one `Advisor` per turn, path trees per unit per turn through `PathTree`); (2) fewer previews (only tiles in reach); (3) only then engine memos, measured by the `stats` feature.
- **Seeing where it goes.** `examples/profile.rs` in citar-bench gains a bot game for callgrind and samply.
- **The server's share** is plan §4's: god view ≤ 20 ms (10 ms measured), passed through as bytes; save lock time ≤ 10 ms; cheap reads from `Heads` without waiting for a drive.

### P2.11 Risks

1. **The yardstick moves:** a weaker or stronger Rust bot shifts every model score. Mitigation: the bot-decision refcheck, the statistics gates and `explained.toml`, all before 2-12.
2. **Engine-intended differences look like bot bugs** (worker automation, sites, supply). Mitigation: G3 entries name an intended id; the agreement floors attribute misses.
3. **Bot speed dominates games.** Mitigation: the games suite, the tuning order, the refusal counts.
4. **GIL, lock and interpreter-exit hazards.** Mitigation: the one rule, `Heads`, a concurrency test, the exit test.
5. **Facade drift between backends.** Mitigation: the parity test with type classes and behaviour tests.
6. **Bot turns' server side effects** were carried by `call_tool`. Mitigation: `_after_action` after every drive and answer; 2-09's gates on each effect.
7. **Saves:** mismatches, locks, crashes between writes, failed appends, corruption, two writers. Mitigation: P2.5's ordering, pending chunks, seq and head checks, forks, the OS lock, the drained writer.
8. **Coverage lost with the Python tests.** Mitigation: every deleted test names a successor that a test checks exists.
9. **Nothing can be re-recorded after 2-12.** Mitigation: 2-00a and 2-00b first, 2-12's precondition. Refcheck becomes a frozen regression suite; new reference states can only come from Rust runs.
10. **CI time.** Mitigation: one abi3 build per OS, rust-cache, the `ci` profile.
11. **Fingerprints per build fragment the ladder** (the owner's decision). The content build id limits it to real changes, and entries are named by profile and revision.
12. **A bot panic poisons a game.** Mitigation: the fixture sweep, chaos with bots, P8 with bot drivers, the server's crash handling.
13. **Hybrid paths untested until Phase 3:** `Deferred`, `HybridDiplomat` and `advice` are covered by scripts and drive tests, but no model drives them until 3D.
14. **The in-memory chronicle grows** (tens of MB for a gargantuan 330-round game). Measured in the 2-13 soak; paging frames to the journal is a later option.
15. **The Windows dev loop.** Mitigation: `cargo xtask develop`.

### P2.12 Decisions taken in this design (Appendix A continued)

63. **Bot reads:** `&Game` and any public read; writes only through `Game::act`; `&mut Game` only in `driver.rs`.
64. **Bot streams:** one purpose, `BotBase`, the decision type as first key word; the game's seed.
65. **Bot memory:** typed JSON in `DriverMemory`, pruned each turn; a mismatch starts fresh.
66. **Parameters:** one JSON schema per version in the Bots page's shape, generated into a struct; two cache parameters removed; fractional ints and unknown names refused.
67. **The advisor reads the bot's memory** through `BotFacts`, gains the escort override, stays read-only.
68. **`drive` everywhere, with the session's side effects:** `play_bot_turn` and `bot_respond` become `drive` and `Game::answer`; the server runs `_after_action` after each.
69. **Saves:** header plus zstd body; journal records with seqs and hashes; one timeline per session, forked when in doubt; chunks before containers, pending on failure; one writer thread per session.
70. **The backend switch:** Python backend frozen at its 38 names; Phase 2 names Rust-only; the default flips at the end of 2-09.
71. **Python recordings first:** baselines committed in 2-00a, every recording made in 2-00b.
72. **Statistical gates** G1-G4 per stratum (P2.4.4).
73. **Build identity is content;** `CITAR_BUILD_ID` is a label.
74. **Fingerprints** hash build id, version, overrides and the profile's fixed aggression.
75. **Cheap reads from `Heads`;** heavy calls detach and lock.
76. **A poisoned game is `EngineCrash`, never `ActionError`;** the session stops, keeps its last good save, stays readable.
77. **The lab stays in Python over `run_game`** until Phase 4.
78. **Modding without a toolchain:** `CITAR_RULESET_DIR` and `citar ruleset check`.

### P2.13 Work breakdown at a glance

Seventeen packages in two lanes, at most two at once. The longest chain is **2-00a → 2-04 → 2-06a → 2-06b → 2-08 → 2-09 → 2-11 → 2-12 → 2-13**, and the bot's chain **2-00a → 2-01a → 2-01b → 2-03 → 2-05 → 2-07** joins it at 2-12. Fifteen packages fill eight two-lane slots, then 2-12 and 2-13 run alone: ten slots, the least this package count allows. Slot 7's free lane is kept for a `2-07f` follow-up if 2-07 needs one.

| Slot | Lane A | Lane B |
|---|---|---|
| 0 | 2-00a Foundation: skeletons, rules, `Game::answer`, build ids, baselines committed | 2-00b Python reference: schema export, bot dumps, bot scripts |
| 1 | 2-01a Bot I: parameters, versions, owners, memory, streams, driver, idle | 2-04 citar-sim: runner, `run_game`, baseline writer, game benches |
| 2 | 2-01b Bot II: context, economy, settlers, workers, scouts, `BotFacts` | 2-06a citar-py: the module, `api::host`, `Heads`, test ops, dev loop |
| 3 | 2-03 Bot III: units and fighting | 2-06b Packaging and CI: maturin, wheels, Docker, docs |
| 4 | 2-05 Bot IV: diplomacy, war, switches, advice, valuation | 2-08 The Rust facade and the switch |
| 5 | 2-07 Statistics, speed floors, bot goldens | 2-09 Server, agents, probes, benchmarks on Rust; the default flips |
| 6 | 2-10 Bots, lab and the ladder on Rust | 2-02 citar-store |
| 7 | | 2-11 Saves v2 |
| 8 | 2-12 The swap, the removal, the data move | |
| 9 | 2-13 Phase 2 exit | |

- **Parallel lanes share no file.** 2-00a writes every manifest, `Cargo.lock` entry, xtask rule and command registration (`gen-params`, the `games` perf suite), bench target, testkit test root (`tests/bot.rs`) and rust.yml change the later crates need, and every public signature they meet, so later packages fill in bodies. Every facade name the server and the lab use is added in 2-08, so 2-09 and 2-10 only consume them; 2-11 adds the save names after 2-09, since both edit `session.py`.
- Each package ends with its gates passing, then a review and a fix round, as in Phase 1, and adds an "As built in 2-xx" note to this section.

### P2.14 Review log

Each finding was checked against the code at `8f84421`.
- **Bot turns lose the session's side effects** (blocker): confirmed (session.py:146-215, bot_agent.py:27-33). Fixed in P2.7.1, with five 2-09 gates.
- **G2 too tight, G5 cannot fail** (blocker): confirmed from the files. Fixed in P2.4.4: ratio bounds, material-and-significant G3, strata, early endings as a rate, calibration, staleness a warning, a bounded 2-07.
- **2-01's city gate** would fail for Python itself: confirmed (33 of 240 under three cities). Distribution gates; scouts moved into 2-01b so the bot explores.
- **Crashes invisible to the server:** confirmed. Fixed in P2.6.3 and P2.7.1.
- **Fingerprints by seat position:** confirmed (lab.py:323). The profile's fixed aggression is hashed.
- **Build id `dev`:** confirmed. Content ids (P2.2.1).
- **Order of work, shared files, late baselines:** confirmed. 2-00a and 2-00b, signatures first, the server's and the lab's facade names all in 2-08.
- **2-01 and 2-06 too big:** accepted; split, and the Python recordings moved to 2-00b.
- **Journal gaps:** confirmed (journal.rs:202-226). Fixed in P2.5.2-P2.5.3, with the writer thread that an autosave called under the lock needs.
- **Getters wait for drives:** accepted; `Heads`.
- **CI pitfalls:** confirmed where checkable (.dockerignore, docs.yml); fixed in P2.6.8. "A member cannot override a workspace lint" is not a blocker: an inner `#![allow]` relaxes a `deny`.
- **The Windows dev loop:** accepted; `cargo xtask develop`.
- **Facade behaviour the parity test cannot see:** confirmed; P2.6.5-P2.6.6.
- **Throwaway Python implementations, a Rust `lab_game`:** accepted, with one change: the default flips after 2-09, not at once, so the server never runs on a half-ported backend.
- **Mis-specified gates, bot-script draws, schema details, the local-only corpus, the resolution cache, the Phase 5 items:** accepted, in P2.3, P2.8.1, P2.9 and the packages' gates.
- **Modding:** confirmed (MODDING.md:133-150). Decided: a runtime ruleset and `citar ruleset check`, not "rebuild to mod".
- **The open question on pushing the tag:** its premise was wrong (the frozen bots are on `main` and in the v0.1.5 wheel); withdrawn.

---

## Appendix C: Phase 2 work breakdown

Each package is sized for one implementation session and ends with its gates passing. The corpus and the bot-decision dump stay on the laptop (owner decision, 2026-09-23).

### 2-00a: Foundation: crate skeletons with their public signatures, xtask rules, Game::answer, content build ids, CI for the new crates, the Python baselines committed

- **Depends on:** nothing
- **Estimated size:** ~1,900 Rust (skeletons 900, build ids 250, Game::answer 150, xtask 300, tests 300) + CI and data moves
- **Scope:** Workspace: members crates/citar-bot, citar-store, citar-sim, citar-py; workspace dependencies (zstd 0.13, pyo3 0.29 with abi3-py311 and generate-import-lib, clap); Cargo.lock; testkit, refcheck and bench manifests gain citar-bot (bench also citar-sim). Skeletons with the final public signatures of DESIGN P2.3.1, P2.4.1 and P2.5 and stub bodies: citar-bot (BotSpec, Tuning, Bot implementing SeatDriver and playing as idle, Owners, Memory, Stream, Refusals, versions, schema, clean, fingerprint, build_id, advice and evaluate returning neutral values), citar-store (container and journal types, StoreError, functions returning a NotYet error that 2-02 removes), citar-sim (Runner, RunSpec, RunResult, run_game, BaselineLine), citar-py (module citar._engine with build_info() and a #[pyclass(frozen)] Game holding Mutex<citar_engine::Game> whose method runs inside Python::detach; [lib] test = false, doctest = false; its lint table, with one crate-level #![allow(unsafe_code)] only if PyO3's expansion needs it). Engine: Game::answer(pid, nid, &mut dyn SeatDriver) in game/turn/drive.rs through with_driver (refuses a negotiation not waiting on pid with ErrCode::Negotiation, refuses while driving and when poisoned), with engine tests; a compile-time assertion that Game: Send. Build identity (DESIGN P2.2.1): crates/citar-engine/build.rs and crates/citar-bot/build.rs hash their sources (CRLF normalised), embedded data or params, version and locked dependency versions into CITAR_ENGINE_CODE and CITAR_BOT_CODE; BUILD_ID = the engine code; CITAR_BUILD_ID becomes BUILD_LABEL; citar_bot::build_id(rules). xtask: the `gen-params` subcommand registered (its generator, xtask/src/gen_params.rs, a stub until 2-01a) and the `games` suite registered in `cargo xtask perf`; citar-bench's `games` bench target (harness = false) and testkit's tests/bot.rs root registered, so later packages fill in files rather than share them. xtask check: the crate graph allow-list, `&mut Game` only in crates/citar-bot/src/driver.rs, no legacy or test-ops in citar-bot/citar-sim/citar-store and citar-py's test-ops only forwarding, no hand-written unsafe in citar-py. rust.yml: actions/setup-python before builds; nextest and doc tests with --exclude citar-py; the lint job's clippy and cargo doc cover citar-py. Data: git mv refcheck/baseline/python-*.jsonl to refcheck/baseline/python/{small,std-large,gargantuan,smoke}.jsonl and commit them; refcheck/.gitignore narrowed to Rust runs and logs; .gitignore gains citar/_engine*.pyd and citar/_engine*.so. DESIGN: 'As built in 2-00a'.
- **Gates:** (1) cargo build --workspace (citar-py included), cargo nextest run --workspace --exclude citar-py --all-features, clippy -D warnings for the workspace including citar-py, cargo doc -D warnings and cargo xtask check pass locally on Windows and on every rust.yml job. (2) xtask check's own tests: a planted `&mut Game` in a citar-bot file other than driver.rs, citar-sim depending on citar-py, and citar-bot enabling test-ops each fail the check with a message naming the file. (3) Game::answer engine tests: answers a negotiation waiting on pid through a test driver, writes changed memory back and settles; refuses a negotiation not waiting on pid; refuses inside a driver; a poisoned game refuses. (4) Build ids: the engine and bot codes computed from an LF copy and a CRLF copy of the sources are equal; editing one src file changes the id, editing a file under docs/ does not; cargo golden check is unchanged for all 15 sets under ci and release. (5) The four baseline files under refcheck/baseline/python/ are byte-identical to the laptop's originals (sha256 listed in the package notes); git check-ignore reports citar/_engine.cp311-win_amd64.pyd and citar/_engine.abi3.so as ignored. (6) The Python suite (608 tests) and ruff green.

### 2-00b: Python reference: the parameter schema export, the bot step on the Python runner, the bot scripts and every bot-decision recording

- **Depends on:** nothing
- **Estimated size:** ~1,500 Python + ~1,200 lines of TOML scripts + generated JSON; ~60 Rust in the testkit script parser
- **Scope:** scripts/bots/export_params.py writes crates/citar-bot/params/basic-1.json ({"engine": "basic-1", "groups": [...]}, 373 parameters: PARAM_GROUPS without site_cache_turns and bv_cache_turns, and 41 ints typed float, P2.3.2). tests/test_bot_params.py holds the file equal to PARAM_GROUPS apart from those keys and types, and records tests/data/clean_params_cases.json: at least 40 inputs with Python's clean_params output or error (null order default, null choice, 2, 2.0, "2", 2.5, unknown key, unknown list name, presets, 'default', each bool spelling), which 2-01a's Rust test reads. tests/rulescript.py: the step { bot = "turn" | "respond" | "advice", player, negotiation?, aggression?, params?, diplomacy? } on the facade ('turn' = play_bot_turn(end_turn=True), 'respond' = bot_respond, 'advice' = bot_advice), and the script header needs = "2-0x", which the Python runner ignores. Rust: crates/citar-testkit/src/script parses `needs` and reports such a script as ignored (no other Rust change). About 24 bot scripts tests/rules/bot_*.toml, each with pinned draws (tech_noise 0, ranged_chance 0 or 1, war, peace and friendship chances 0 or 1) and a needs header naming 2-01a, 2-01b, 2-03 or 2-05, covering Phase 1's 15 deferred bot scripts and the bot parts of test_bots and test_bot_diplomacy (19 tests): research beeline, policies finishing branches, a defender bought in danger, disbanding in deficit, a settler founding at its site, faith buildings before missionaries, scouts exploring; a settler waiting for an escort, garrisons fortifying, holding a losing attack, ranged siege first, a great person used, a spaceship part to the capital; each diplomacy switch, counters merging gold and never asking more than held, stopping after counter_rounds, an offer standing once then rejected, advice as plain data, idle rejecting, a model-owned negotiation deferred. scripts/refcheck/bot_dump.py records, for a fresh BasicBot(seed=0) with tech_noise 0 and ex patched to record, on each state and living major, the three stages of DESIGN P2.3.11; writes refcheck/bot_decisions.json.gz for the 12 committed states (committed) and the corpus to the path in CITAR_BOT_DUMP (local), and documents how in scripts/refcheck/README.
- **Gates:** (1) tests/test_bot_params.py passes: 373 parameters in 17 groups, every key, type, default, range, choice, option and preset equal to PARAM_GROUPS apart from the two removed keys and the retyped ints' type and default; the clean-params table has at least 40 cases covering every listed input. (2) Every bot script passes on the Python runner (test_rule_scripts green) and carries pinned draws and a needs header; the Rust runner reports each as ignored and nextest stays green. (3) bot_dump.py runs without error on the 12 committed states and the 250 corpus states; the committed file covers every living major of every committed state for every kind of the three stages (counts in the package notes); two runs give byte-identical files. (4) The Python suite and ruff green; git diff touches no Rust file outside crates/citar-testkit/src/script.

### 2-01a: Bot I: parameters and their generation, versions, owners, memory, streams, the driver and Turn, the idle bot, the Rust bot step

- **Depends on:** 2-00a, 2-00b
- **Estimated size:** ~2,200 Rust + ~1,000 generated (gen.rs) + ~900 tests
- **Scope:** crates/citar-bot: params/schema.rs (include_str! of basic-1.json), cargo xtask gen-params (filling xtask/src/gen_params.rs) writing src/params/gen.rs (Params, an enum per choice, NameList with null and "default" as Default, deny_unknown_fields, no defaults) and xtask check rule 4 (gen.rs up to date); clean.rs (port of clean_params with the two fixes: fractional ints refused with integral floats and numeric strings accepted; order and list names limited to options, presets, "default" and null; the two retired cache keys dropped whatever their value, P2.3.2); resolve.rs (Resolved per RulesetId cached in Tuning, unknown names counted); versions.rs (basic-1 memory kind 1, idle, LATEST); owners.rs (Owners over game::diplomacy::category::Category, set with set_diplomacy's messages, owns(&Negotiation) through proposal_categories); memory.rs (Memory, JSON codec in DriverMemory kind 1 version 1, pruning, foreign kind or version starts fresh); stream.rs (the five words under Purpose::BotBase); driver.rs (impl SeatDriver for Bot; Turn wrapping Game::act with Refusals by tool; play_turn calling the phases in Python's order, each a no-op until its package; respond returning Deferred when the model owns the negotiation and otherwise rejecting with a canned line until 2-05); idle.rs; fingerprint(spec, build_id) hashing the profile's fixed aggression (DESIGN P2.8.6). Engine: AdvisorParams gains Deserialize and a constructor from basic-1's effective map. Testkit: tests/rules.rs plays the bot step (turn = drive with only that seat driven and seat limit 1; respond = Game::answer; advice = citar_bot::advice); tests/bot/{params,memory,streams,owners,idle}.rs.
- **Gates:** (1) Effective defaults deserialize into Params; a key missing from struct or schema fails a test; AdvisorParams from basic-1's defaults equals AdvisorParams::default(); clean() matches 2-00b's clean-params table on every case, giving a case's `basic1` answer where it has one (the documented fixes: int-integral, names-known, dropped, retyped); editing gen.rs by hand fails cargo xtask check. (2) Memory: proptest round trip (1,000 cases) to equal bytes; a 64-player worst case under DriverMemory::MAX_LEN; a memory of another kind or version starts empty. (3) The first draws of each stream are pinned; Owners::set refuses an unknown category or owner with set_diplomacy's messages; owns() agrees with Phase 0's owns_negotiation cases (test_engine_api, test_bot_diplomacy) ported as unit tests. (4) Fingerprint: two lab seats of one profile at different positions share a fingerprint; a different fixed aggression, override, version or build id changes it. (5) An all-idle 4-seat small game of 50 rounds with DebugOptions::ALL: no panic or violation, every seat founds a capital, every negotiation put to idle is rejected. (6) No script names needs = "2-01a"; every script passes or is ignored by its needs; nextest, clippy -D warnings, cargo doc, cargo xtask check (rules 1-4) and the Python suite green.

### 2-01b: Bot II: context, research, empire, faith, cities, gold, settlers, workers, scouts, and the advisor's BotFacts

- **Depends on:** 2-01a
- **Estimated size:** ~2,600 Rust + ~700 tests
- **Scope:** crates/citar-bot/src/basic1: context.rs (basic.py 777-871), research.rs (876-998, both tech modes, the Research stream per tech), empire.rs (1003-1041), faith.rs (1696-1737, religion::found::ai_choose_beliefs for belief_mode unciv), cities.rs (1152-1202 through Advisor::with_facts with started() after each pick; focus; avoid growth; city bombard 1577-1587), gold.rs (1591-1643 without city-state gifts; spare units 1739-1766), settlers.rs (1835-1857, 1879-1888 without danger or escorts; bad sites in memory), workers.rs (automate; work boats 1927-1949), units/mod.rs (manage_units' order, ranged first then id, and the dispatch, the other branches no-ops until 2-03), units/scouts.rs (2054-2063). Engine advisor: BotFacts and Advisor::with_facts (new() = BotFacts::NONE); advisor.rs:444, 728, 925 and 1363 read the facts; the escort override added in both modes; Advisor::best_military and Advisor::sites public. citar-refcheck: the group bot_decisions (stage 1 kinds) reading refcheck/bot_decisions.json.gz and CITAR_BOT_DUMP, values compared with refcheck's tolerance and enforced in refcheck/enforced.toml; `cargo refcheck bot-agreement` printing agreement per kind with misses attributed. Testkit: tests/bot/economy.rs.
- **Gates:** (1) Automatic production unchanged: with BotFacts::NONE the advisor's corpus test (1c-07 gate 4) gives identical picks; one test per fact shows the fact changing the pick it should. (2) bot_decisions stage 1: tech values within tolerance for every major of the 12 committed states (and the 250 corpus states locally), enforced and clean; cargo refcheck bot-agreement at least 95% for next research and the free technology, next policy, free great person and pantheon (each now and preferred), expansion sites (top 3 as a set), spare units, and the danger and garrison flags, each over the items where either engine's answer says something (P2.3.11); every miss attributed to a named cause. (3) Whole games, barbarians off, 4 bots on small, seeds 5000-5004, 150 rounds, DebugOptions::ALL: no panic or violation; at turn 100 the mean cities over the 20 civilizations is at least 2.8, at least 85% have two cities or more, mean techs at least 17 with every civilization at least 12; every civilization built a settler by turn 60; no tool is refused more than 50 times in one bot turn; a save and load at round 75 gives the uninterrupted digest chain. (4) No script names needs = "2-01b" and those scripts pass on both runners. (5) nextest, clippy, cargo doc, cargo xtask check and the Python suite green.

### 2-02: citar-store: zstd, the .citar v2 container, journal framing with seqs, torn-tail and corruption recovery, the OS lock

- **Depends on:** 2-00a
- **Estimated size:** ~1,000 Rust + ~600 tests
- **Scope:** Fill in crates/citar-store (relaxed clippy; zstd 0.13, blake3, serde_json, thiserror; no workspace crate) and remove its NotYet error. Container (DESIGN P2.5.1): 'CITARSV2' + u32 LE header length + header JSON + one zstd frame (level 3) of the body JSON with the state spliced raw; write_container (temporary file and rename, 8 retries at 250 ms on PermissionDenied), read_header (no decompression), read_container. Journal (P2.5.2): 'CITARJNL' + u16 version; records u32 length | u32 seq | u8 kind | u8 codec | 8-byte blake3 | payload; JournalWriter::open (exclusive File::try_lock; truncates only an incomplete tail; a complete record with a bad hash or out-of-order seq is corruption: nothing truncated, appends refused, Recovered { records, bytes, torn, corrupt_at }); append(seq, payload) refusing a seq that is not the next, retried on PermissionDenied; sync (sync_data); JournalRef { file, records, bytes, head }; read_upto (refuses a ref past the intact prefix or whose last record does not hash to head); fork (byte copy of a prefix). StoreError for every failure; the formats specified in the module docs. A container and journal case in citar-bench's io suite (report-only).
- **Gates:** (1) Round trips: proptest (1,000 cases) of headers, bodies and record sequences; the synthetic gargantuan state JSON with 330 chunks. (2) For a 50-record journal, every truncation point: open recovers exactly the complete records before it and truncates only the partial one; every single-byte corruption inside a complete record: open truncates nothing, reports corrupt_at at that record, refuses append, and fork of the good prefix gives a journal that opens clean; read_upto never panics and refuses refs past the good prefix or with another head. (3) A second JournalWriter::open of a file already open fails with a lock error on Windows and Linux. (4) Arbitrary bytes as container or journal: always an Err, never a panic (proptest 10,000 cases). (5) Bench (report-only): writing the gargantuan container (JSON already made) at most 150 ms, read_header at most 1 ms. (6) clippy, cargo doc, cargo xtask check green.

### 2-03: Bot III: units and fighting

- **Depends on:** 2-01b
- **Estimated size:** ~2,800 Rust + ~800 tests
- **Scope:** Ports basic.py 1806-1833 (promotions through Resolved's promo_in_city and promo_lines; garrisons in memory), 1858-1925 (settler danger with threat_reach, escorts, follow, retreats, bad sites and need_escort in memory; the goto write replaced by memory), 1951-2052 (special units: spaceship parts to the capital and clearing its civilian slot, founding and enhancing religions with the bot's beliefs, hurry and trade missions, triggers, missionaries and inquisitors, great improvements near the capital, generals following the front), 2065-2094 (air, naval), 2096-2139 (attack_best on the typed combat::resolve::preview_of), 2141-2389 (handle_military: escort duty, ranged siege moves with siege_move_first, attacks, garrison and fortify, healing, filling empty garrisons, defending threatened cities, the war target, the rally point, advance and siege_ready on memory.war_plan, the war-preparation rally read from memory.war_prep, camps, ruins, seeking rival cities, returning home; military_power). BotFacts gains garrisons and need_escort from memory. citar-refcheck: bot_decisions stage 2 kinds. Testkit: tests/bot/{units,war,sweep}.rs, the sweep loading every state through Game::from_python and driving one round for every major by bots.
- **Gates:** (1) cargo refcheck bot-agreement at least 95% for attack targets and war targets, over the units and civilizations where either engine names a target (P2.3.11), on the 12 committed states and, locally, the 250 corpus states; misses attributed. (2) Fixture sweep: every committed state (and locally every corpus state), one bot-driven round for every major with DebugOptions::ALL: no panic or violation; refusals per bot turn reported. (3) Mixed games: 2 bots against 2 RandomAgents (which declare wars after turn 50) on small, 20 seeds x 200 rounds: no panic or violation; the bots out-score both agents in at least 18 of 20 games; bot attacks in every game and at least one city captured by a bot across the 20. (4) 4-bot small games with barbarians normal, 10 seeds x 330 rounds: no panic or violation, every game ends. (5) No script names needs = "2-03" and those scripts pass on both runners. (6) 2-01b's gates still hold; nextest, clippy, cargo doc, cargo xtask check green.

### 2-04: citar-sim: the headless runner, run_game, the baseline writer, the CLI, the game benchmarks

- **Depends on:** 2-00a
- **Estimated size:** ~1,800 Rust + ~600 tests; ~80 Python
- **Scope:** Fill in crates/citar-sim (lib + bin, relaxed clippy; citar-engine with embedded-ruleset, citar-bot, serde_json, clap). Runner (DESIGN P2.4.1): one Game::drive with seat_limit 1 per step; on_round when the turn changes; a time budget checked between steps; catch_unwind -> poison + a crash record with the seat's label (no max_errors); raise_errors surfaced to the host. RunResult = engine_api.run_game's dict with headless.result's player rows. Baseline writer = baseline.py's lines exactly (IDENTITY, the rotation, seed + i, Tally semantics with checkpoint totals at the start of round T+1 and the end-of-game fallback, engine = build id, bot = basic-1, resume, refusing another build's file, crash lines, --workers on std::thread, --smoke), with the BaselineLine serde type. CLI citar-sim play | baseline. scripts/bots/run_game_keys.py records the key and type shape of a Python run_game result on a duel spec into crates/citar-sim/tests/data/run_game_keys.json. Tests use the idle bot, the skeleton basic-1 and a local test driver (one that panics on demand). citar-bench: benches/games.rs (the target 2-00a registered): the small 4-bot game of seeds 5000-5002 to 330 rounds (median) and one gargantuan 24-bot game of seed 5000; thresholds rows game/bot_small_330 (16 s) and game/bot_gargantuan_330 (180 s), report-only; cargo xtask perf --suite games.
- **Gates:** (1) BaselineLine round-trips every line of the four committed Python files to equal JSON values, and a Rust line validates against it. (2) run_game's result has exactly the recorded keys and value types (run_game_keys.json, numbers as one class); the same spec twice gives identical results. (3) A panicking test driver yields a crash record naming its label and a poisoned game, and the process lives; with raise_errors the runner returns the error to its caller. (4) citar-sim baseline --smoke plays two duel games; a run killed midway resumes (finished lines kept, the unfinished game replayed); a file from another build id is refused. (5) cargo bench -p citar-bench --bench games writes perf/games.json and cargo xtask perf --suite games reports it (report-only). (6) nextest, clippy, cargo doc, cargo xtask check green.

### 2-05: Bot IV: diplomacy, war, the switches, advice and deal valuation

- **Depends on:** 2-03
- **Estimated size:** ~2,200 Rust + ~800 tests
- **Scope:** Ports basic.py 1043-1064 (spies on the Spies stream), 1645-1694 (city-state gifts, classic and typed), 2394-2503 (consider_diplomacy: peace offers on the Peace stream; embassies, friendship on the Friendship stream and research agreements every diplo_every turns; war preparation on the WarPrep stream with reachable_city, gathering at the rally, declaring and creating the war plan, aborting), 2505-2556 (luxury trades and buying), 2558-2643 (evaluate over typed DealItems and Terms; war items with memory), 2651-2708 (respond: talk, an offer standing once, accept, counter merging gold and capped by what they hold, counter_rounds, a refused counter becoming a reject), 2713-2771 (advice as a pure read with Python's keys). Every _llm site honours Owners; respond returns Deferred for model-owned negotiations; _settle_chats and handle_negotiations are not ported. citar-refcheck: --with-bot answers deals[*].bot_value with citar_bot::evaluate (fresh memory, defaults, aggression 0.4), refcheck/enforced.toml adds the path, ratchet at 0; bot_decisions stage 3 kinds.
- **Gates:** (1) cargo refcheck run --with-bot --fixtures refcheck/fixtures-mini --fixtures refcheck/fixtures-late --strict: bot_value clean (and on the corpus locally). (2) cargo refcheck bot-agreement at least 95% for reachable cities, trade proposals and advice wants, over the rival pairs and civilizations where either engine's answer says something (P2.3.11; trade proposals and wants, thin on the committed states, on the corpus locally); advice's deal_value is gate 1's bot_value, with bot_advice_plain_data. (3) No script names needs = "2-05"; every script in tests/rules passes on both runners. (4) Switches: with every category owned by the model, 4 bots on small for 200 rounds open no negotiation, declare no war, gift no city-state and move no spy, still fight wars RandomAgents declare on them, and return Deferred for every negotiation put to them; with city_states = 0 and 30 turns before contact, all-bot and all-model owners give identical digests. (5) All-bot games, 4 bots x 20 seeds x 330 rounds on small: wars declared in at least 30% of games, peace made at least once, a resource trade executed in at least 50% of games, every game ends, drive never returns AwaitingReply. (6) Save and load at every round of a 120-round bot game equals the uninterrupted chain. (7) nextest, clippy, cargo doc, cargo xtask check and the Python suite green.

### 2-06a: citar-py: the module, api::host, Heads, the bot handle, test operations, the dev loop

- **Depends on:** 2-04, 2-00a
- **Estimated size:** ~2,600 Rust + ~700 Python (tests, stubs, xtask develop glue)
- **Scope:** Fill in crates/citar-py (cdylib citar._engine; PyO3 0.29 abi3-py311; engine with embedded-ruleset, bot, sim; feature test-ops forwarding to the engine's). Classes Game (frozen: Mutex<Game> plus Heads) and Bot (frozen: Mutex<Arc<BotSpec>>; set_diplomacy in place; .aggression, .version, owns_negotiation); exceptions ActionError (.code), MapError(ValueError), LoadError(ValueError), EngineCrash(RuntimeError) with ErrCode::Poisoned and every caught panic mapped to EngineCrash. Every row of DESIGN P2.6.1 except saves v2: ruleset, tools, maps, scenario helpers, categories, text constants, bots (versions, schema, clean, fingerprint, build_info), run_game with per-step callbacks and raise_errors, labels and traceback_limit, and the Game methods (new, load, Heads getters, summary/stats/events/thoughts JSON, execute, view_json and replay_json with extra spliced, briefing, turn_progress, empire_summary, negotiation reads and ops, seats, apply_ops, scenario and map ops, path_preview, meet, force_turn, debug, replay_data, drive returning stop, events and per-bot action counts, answer, bot_advice, inspect and test_ops with test-ops). Heavy calls inside Python::detach with the Mutex taken inside and catch_unwind inside the lock; Heads published at the end of each mutating call; a calls-in-flight counter for the facade's atexit wait. Engine additions with engine tests: api::host (summary and player rows, config JSON, turn limit, negotiation heads, open negotiations, event by id, stats and thoughts rows); test operations eliminate, end_game and panic (Python testops gains eliminate and end_game). citar/_engine.pyi stubs. The dev loop (DESIGN P2.6.7): cargo xtask develop [--release] (venv must belong to the worktree, dependencies installed when pyproject changed, cargo build into CARGO_TARGET_DIR, copy to <target>/citar-ext with a locked copy renamed aside, citar-dev.pth setting CITAR_EXT_DIR); citar/__init__.py puts CITAR_EXT_DIR first on __path__ when set; CONTRIBUTING's dev loop section.
- **Gates:** (1) tests/test_engine_module.py (skipped without the module): every binding called on a seeded duel; results decode; errors map (ActionError with code, ValueError, MapError, LoadError); a poisoned game raises EngineCrash, never ActionError, and still answers summary, view_json and replay_json. (2) Parallelism: two threads each driving its own 4-bot small game for 60 rounds reach a process CPU time to wall time ratio of at least 1.6; a Python thread counting in a loop keeps at least half its solo rate while another thread drives; 1,000 turn() polls from a third thread during a drive never wait for it (each under 1 ms) and never deadlock. (3) The panic test operation gives EngineCrash, the game refuses further commands, the Mutex is not poisoned, the process lives. (4) run_game with Python callbacks delivers every event exactly once in order (count equals the chronicle's) and on_turn once per round; raise_errors raises EngineCrash; bot_set_diplomacy changes an existing handle in place and owns_negotiation follows it. (5) Interpreter exit: in WSL with Python 3.11, 3.12 and 3.13, a script that starts a daemon thread driving a game and exits mid-drive returns exit code 0 (2-06b puts it in CI). (6) cargo xtask develop on the laptop builds into the target directory, writes nothing under citar/, and a second worktree's venv is refused; the suite imports the extension through CITAR_EXT_DIR. (7) nextest, clippy, cargo doc, cargo xtask check and the Python suite (Python backend) green.

### 2-06b: Packaging and CI: pyproject on maturin, one abi3 build per OS, the wheel, Docker and docs workflows

- **Depends on:** 2-06a
- **Estimated size:** ~600 (pyproject, workflows, Dockerfile, scripts/ci, xtask) + docs
- **Scope:** pyproject.toml on maturin (DESIGN P2.6.7: mixed layout, python-source '.', module-name citar._engine, manifest-path crates/citar-py/Cargo.toml, features without test-ops, includes for web, migrations, data and collectors, dynamic version from Cargo); xtask check: pyproject's maturin features exclude test-ops and citar-py never enables legacy. test.yml (P2.6.8): a build-ext job per OS (PyO3/maturin-action, --profile ci --features test-ops, rust-cache, the pinned toolchain) uploading the abi3 test wheel; the five test jobs download it, install dependencies and unpack citar/_engine* into the checkout (scripts/ci/unpack_ext.py); the package job builds a release-profile manylinux_2_28 wheel without test operations, installs it in a clean venv, imports citar._engine from site-packages, checks the abi3 tag and runs citar doctor; the installer jobs use the build-ext wheels with install.sh --from; the docker job copies the manylinux wheel to wheelhouse/ and the Dockerfile installs wheelhouse/*.whl (.dockerignore still excludes dist); the interpreter-exit job of 2-06a joins test.yml. docs.yml installs the toolchain with rust-cache before pip install -e ".[dev]". release.yml gains a first step failing with 'release packaging is Phase 5' while __version__ is 0.1.6. CONTRIBUTING and INSTALL's from-source notes (a Rust toolchain is needed to build from source).
- **Gates:** (1) test.yml green on every job at the package's head: build-ext on three OSes, the five test jobs running the suite with the unpacked extension, package (clean-venv import from site-packages, abi3 tag, citar doctor), installer, docker (the container answers its health check). (2) docs.yml green on a workflow_dispatch run of the branch. (3) rust.yml green. (4) release.yml's first step fails with its message when run against the branch (its script run locally in the package notes). (5) cargo xtask check fails on a pyproject listing test-ops. (6) pip install -e . from a fresh clone builds the extension on Windows and the Python suite passes.

### 2-07: Whole-game comparison with the Python baseline, the speed floors, the bot golden set

- **Depends on:** 2-04, 2-05
- **Estimated size:** ~700 (summarize.py, explained.toml, goldens, thresholds) + bot or engine fixes as found
- **Scope:** Run citar-sim baseline in release with 6 workers: Rust small x120 (seeds 5000-5119), standard/large x50 (25 each), gargantuan x2 (timing), and the calibration run (classic-production small x60). scripts/refcheck/summarize.py (DESIGN P2.4.4): per-game values, strata, ratios, bootstrap 90% intervals of the difference (2,000 seeded resamples, pure Python), d for reference, --split-half, rates, and --gate refcheck/baseline/explained.toml implementing G1-G4 (exit 1 on failure, stale entries a warning, entries marked pending refused by 2-12). Write explained.toml. Fix porting mistakes found, each with a script or test, re-running the affected runs; a gap needing bot-logic work beyond that is entered as pending = "2-07f" for a follow-up package. Make the games suite hard (budgets 16 s and 180 s); if over, tune in P2.10's order. Bot golden set crates/citar-testkit/golden/bot.json (a 120-round duel, two 100-round small games, the late fixture passed 10 rounds by bots; per-round digest chains and memory hashes), blessed and compared by determinism.yml; long.json gains one 330-round 4-bot small game for nightly. DESIGN 'As built in 2-07' with the comparison and calibration tables.
- **Gates:** (1) python scripts/refcheck/summarize.py refcheck/baseline/python/small.jsonl <rust small> --gate refcheck/baseline/explained.toml exits 0, and the same for the standard and large strata of std-large. (2) 0 crashed, poisoned or timed-out games among the 172; 0 invariant violations in the 20-game subset with checks on. (3) With the other lane paused: cargo xtask perf --suite games --check exits 0 (bot small median at most 24 s, gargantuan at most 270 s); the ratios to the 5 s and 90 s targets reported. (4) cargo golden check identical on Windows x64 and Linux x64 under ci and release; determinism.yml green on its 6 rows with the bot set. (5) The split-half noise and the calibration table are in the as-built note. (6) nextest, clippy, cargo xtask check green.

### 2-08: The Rust backend of the facade, the backend switch, every Phase 2 facade name the server and lab use, and the core suite on both backends

- **Depends on:** 2-06a, 2-06b, 2-01b
- **Estimated size:** ~1,500 Python + ~500 test changes
- **Scope:** citar/engine_api.py becomes the selector (BACKEND from CITAR_ENGINE, default python; one __all__; an import-time check that the backend defines every name); its current body moves unchanged to citar/engine/facade.py. citar/_facade_rust.py implements the 38 names and every EngineGame method over citar._engine with the docstrings' shapes (subscribe fan-out after each call; seeds drawn and map documents inlined in Python; Bot handles; play_bot_turn as a one-seat drive; bot_respond as answer; state_dict, to_save and from_save with the whole history as one chunk; getters on Heads). Every Phase 2 facade name the server and the lab use, defined on both backends and raising BackendError on Python: EngineGame.drive, answer, view_json, replay_json; build_info, bot_versions, bot_schema, bot_clean_params, bot_fingerprint; EngineCrash, BackendError. Docstrings record the two behaviour changes (atomic apply_ops, subscribe after the call). profiles.make_bot, lab.make_bot and balance.make_bot go through engine_api.bot_instance. tests/backends.py (RUST, python_engine_only(successor), rust_pending(package), rust_only) and tests/test_backends.py (successors exist); test_engine_boundary forbids citar._engine outside the facade modules and BACKEND reads outside the facade and tests; tests/test_facade_parity.py (key sets and value types under type classes, plus behaviour tests: set_diplomacy then owns_negotiation, raise_errors, event order and counts); tests/rulescript.py runs the intended checks on Rust; the eleven python_game pokes replaced by the eliminate and end_game test operations; Python-engine-only tests marked with successors. CI: a test.yml job (ubuntu, 3.12) with CITAR_ENGINE=rust.
- **Gates:** (1) With CITAR_ENGINE=rust: test_engine_api, test_rule_scripts (every script in tests/rules, counted from the directory, through the bindings), test_engine_boundary, test_backends, test_facade_parity, test_setup, test_paths and the facade-level controller tests pass; every other test passes or is marked (python_engine_only with an existing successor, rust_pending naming 2-09, 2-10 or 2-11, or rust_only). (2) The whole suite passes on the Python backend. (3) The parity test finds zero differences under its type classes and its behaviour tests pass on both backends. (4) CI green on both test jobs and rust.yml; ruff green.

### 2-09: The server, agents, MCP, probes, benchmarks and reports on Rust; crash handling; the default flips

- **Depends on:** 2-08, 2-05
- **Estimated size:** ~1,000 Python + test changes
- **Scope:** session.py (DESIGN P2.7.1): BotAgent.play_turn drives every bot seat with seat_limit 1 under the lock, sets end_reason and records the drive's action counts, and calls _after_action after every drive; on awaiting_reply waits on cond (90 s), drives again, and on timeout closes the chats as expired through close_negotiation and drives once more; respond_negotiation answers with EngineGame.answer then _after_action, ignoring a refused answer. Crash handling: _crashed(message) on EngineCrash anywhere (call_tool, the driver, responders, views): crashed state, pause, driver stopped, agents cancelled, no emit, a 'crashed' broadcast, crash-<turn> save written once, autosave never over the last good save, reads still served, info() and the lobby show it; _drive's error handler no longer emits on the game. Metrics and reports tolerate bot turns without per-tool rows and show bot_actions. app.py: /view returns EngineGame.view_json bytes, /replay returns replay_json bytes; replay, path preview, debug, export_map, the scenario editor and summaries on Rust. Agents and MCP on the facade shapes; probes on engine-format scenario states; benchmarks, scoring and reports on interim saves. Tests: test_session, test_benchmarks, test_access, test_worker, test_ui_support, test_editor, test_providers, test_metrics made backend-neutral; the BotAgent tests of test_bot_diplomacy rewritten for drive; every rust_pending('2-09') removed; tests/test_server_rust_smoke.py. The default flips to CITAR_ENGINE=rust; CI's Python-backend job runs only tests/python_reference.txt.
- **Gates:** (1) The full suite passes on Rust (now the default) with no rust_pending('2-09'); the Python reference subset passes on Python. (2) Side effects, in an all-bot 4-seat small lobby game under the session driver (TestClient): the session version rises every round; after 20 rounds autosave is at most one round behind; metrics hold a turn row per bot turn with end_reason; a chat a bot opens with a stub LLM seat starts that seat's responder and the bot's turn ends after the stub answers; a human's proposal answered by a bot produces an 'update' broadcast. (3) Crashes: the panic test operation through call_tool and through a bot drive leaves the session crashed, the driver stopped, no exception from emit, the last good autosave intact, the crash save written, and summary, /view and /replay answering. (4) /view returns exactly view_json's bytes with json.loads never called on them (mocked); the late fixture's god view through TestClient has a median at most 20 ms over 20 requests. (5) 4 bot seats on small play 100 rounds under the session driver with responders active, no round longer than 10 s. (6) The smoke test passes; CI green; ruff and scripts/audit_routes.py --strict green.

### 2-10: Bots, the lab and the ladder on Rust: versions, profiles, fingerprints, lab results, ratings, sim, balance

- **Depends on:** 2-08
- **Estimated size:** ~700 Python + ~250 tests
- **Scope:** profiles.py: engines() from bot_versions plus idle; schema, defaults, clean_params and fingerprint through the facade (fingerprint over the profile's fixed aggression); ENGINE_RE ^(basic|basic-\d+|idle)$; basic resolves to the latest version at use; freeze, module and code_id removed and frozen_* refused as archived with 0.1.5; BUILTIN = standard, classic-production, idle. bots_api /engines and /schema from the facade, same shapes. lab.py: normalize pins versions (no freeze); play() keeps its body over engine_api.run_game with facade bots, printing PROGRESS from on_turn; results record build id, bot version, profile, revision and fingerprint per seat at play time; engine_hash() = build id. ratings.py names entries from each result's own records, no bot-module imports or frozen globbing. sim.py and balance.py on run_game with facade bots, seed arguments ignored. docs/BOTS.md and docs/BOT_TUNING.md: versions, the schema, the new ladder. test_bot_profiles and the lab tests on Rust; every rust_pending('2-10') removed.
- **Gates:** (1) The full suite passes with no rust_pending('2-10') left. (2) A lab experiment (standard against classic-production, 4 duel games of 60 turns) queued and run by python -m citar.lab run writes results with build id, version, profile, revision and fingerprint per seat; standard's seats at different positions share one fingerprint; ratings.rankings() lists exactly two entries, named after the profiles. (3) The Bots page API returns basic-1's schema (373 parameters in 17 groups, {engine, groups}); a saved profile round-trips through bot_clean_params; a frozen_* engine is refused with the archive message. (4) citar sim plays a 4-bot small game and prints standings; balance runs one matchup. (5) CI green.

### 2-11: Saves v2 in the server: the container and the journal, the session's save writer, timelines, crash safety

- **Depends on:** 2-02, 2-09
- **Estimated size:** ~600 Rust + ~700 Python + ~500 tests
- **Scope:** citar-py: Journal (open with recovery and the OS lock, fork, append in seq order under its own Mutex, the pending chunks of DESIGN P2.5.3), Game.save_snapshot() -> SaveSnapshot (engine Snapshot + take_journal_chunk under the game lock), SaveSnapshot.write(path, journal, session_json, metrics_json) with the GIL released (pending and new chunks appended and synced, state JSON, zstd, container temporary file and rename), read_save(path), save_header(path), Game.load_save(doc, journal path) through citar-store. Facade names open_journal, read_save, save_header, EngineGame.save_snapshot and from_save(doc) (Rust-only, BackendError on Python). session.py: one writer thread per session (autosave and named saves snapshot under the lock and submit; queued autosaves coalesce; a named save waits for its own write outside the lock); one journal timeline per session (continue only when no container in the folder names a longer prefix and the journal opens clean, truncating records past the save; fork otherwise, corruption included); stop() drains the writer and closes the journal; from_save via read_save; save_meta and list_saves from headers; delete_save removes unreferenced journals; a crashed session's crash save goes through the same path. scoring.py, reports/data.py, benchmarks.py, probes.py and app.py's scenario-from-save through the facade. A v1 gzip save is refused with 'saved by the Python engine; archived with 0.1.5'. Test hooks to stop between append and container write and to fail appends.
- **Gates:** (1) A 4-bot small game autosaved every round for 100 rounds, reloaded from autosave: replay_data(Full), events() and the digest equal the live game's. (2) Stopped between append and container write: the next load of autosave succeeds with the extra record truncated; loading turn050 after a later autosave forks a new journal, and both timelines play on and load again. (3) Appends failing three times: no container is written, the chunks stay pending, the next save appends them in order, and a reload has the complete chronicle. (4) A corrupt record in the middle of journal.cjnl: loading a save that names it forks from the good prefix and leaves the corrupt file untouched; a save naming records past the corruption is refused with a clear error. (5) A second session opening the same journal is refused; stop() returns only after the write in flight. (6) save_snapshot's time under the lock on the synthetic gargantuan state at most 10 ms (budget), never above 100 ms. (7) Listing 50 gargantuan saves reads headers only (asserted: no zstd decode). (8) The full suite passes with no rust_pending('2-11'); CI green.

### 2-12: The swap and the removal: the Python engine and frozen bots tagged and deleted, the data move, modding without a toolchain

- **Depends on:** 2-07, 2-09, 2-10, 2-11
- **Estimated size:** +700 / -60,000 (deletions)
- **Scope:** Precondition checklist (DESIGN P2.8.1): every Python recording committed (fixtures and answers, tool_list.json, query_tools.json.gz, the advisor and bot-decision recordings of the committed states, the four baselines, basic-1.json); the corpus, its answers and the local advisor and bot dumps archived under saves/_archive_2026-10_python-reference/ with refcheck/corpus.sha256 committed; no rust_pending marker, no script needs, no explained.toml entry marked pending. The orchestrator tags the base python-engine-0.1.6 (annotated) and pushes it with the merge. engine_api imports the Rust facade only (the switch, BACKEND, BackendError and citar/engine/facade.py go). Delete citar/engine/, citar/bots/{basic,headless,idle}.py and the 18 frozen_*.py, the __path__ hack, the python_engine_only tests and emptied modules, tests/python_reference.txt, test_rule_scripts' Python-only checks, tests/test_bot_params.py, the Python recorders under scripts/refcheck except summarize.py, scripts/check_refs.py, scripts/check_uniques.py, scripts/bots/, and anything else importing citar.engine; the seed arguments of the bot factories' callers. Data move: git mv citar/data/{ruleset,custom,game.json} crates/citar-engine/data/, rules::source paths, collectors stay; doctor's message and test.yml's package check to citar._engine.ruleset_counts(); pyproject includes; ruff excludes. Modding (P2.8.4): CITAR_RULESET_DIR read when citar._engine loads (Ruleset::load, used for every game and ruleset function, reported by build_info); python -m citar ruleset check DIR printing RulesetErrors. Docs: MODDING.md, docs/reference.md onto citar.engine_api with a link to the Rust API docs, README, ARCHITECTURE, CONTRIBUTING, CHANGELOG Compatibility (0.1.5 saves, scenarios, frozen bots and profiles do not load; building from source needs Rust; modding through CITAR_RULESET_DIR). CI: test.yml drops the Python-backend job and the Ruleset integrity step; rust.yml path filters gain crates/citar-engine/data/**.
- **Gates:** (1) git grep -nE 'citar[./]engine([./]|$)', and git grep for 'frozen_' and 'python_game', find nothing outside CHANGELOG, DESIGN history and archived docs. (2) The Python suite (Rust only), ruff, scripts/audit_routes.py --strict, scripts/check_links.py --strict and mkdocs build --strict pass. (3) nextest, cargo refcheck run on mini and late --strict --with-bot, and cargo golden check (bot set included) unchanged across the data move, the RulesetId equal before and after. (4) A wheel built by maturin installs in a clean venv and citar doctor passes; pip install -e . from a fresh clone builds and the suite passes. (5) CITAR_RULESET_DIR pointing at a copy of the data with one added nation: a game starts with it, build_info reports a different RulesetId, and its save is refused by a process without it; citar ruleset check reports a planted unknown unique and a broken reference. (6) CI green on every workflow.

### 2-13: Phase 2 exit: soak, chaos, determinism, speed, the server soak, docs

- **Depends on:** 2-12
- **Estimated size:** ~400 (soak and chaos driver options, nightly jobs) + docs
- **Scope:** Testkit soak and chaos binaries gain --drivers bot|random|mixed. Runs: 1,000 small bot games (6 shards, invariants on, the cache oracle every 50 rounds, save and load every 25) and 100 larger (40 standard, 30 large, 20 huge, 10 gargantuan) under WSL; mixed bot/random chaos 20 minutes on Windows and Linux; the bot fixture sweep and cargo refcheck --strict --with-bot on the 250 corpus states from the archive (checksums verified first); props P1-P8 at 10,000 cases with bot drivers in P8's noisy game; nightly.yml gains bot chaos, a bot soak and the long bot golden game. Determinism: cargo golden check --long with the bot sets on Windows x64 and Linux x64 (ci and release) and determinism.yml and nightly.yml on 6 targets. Speed: cargo xtask perf --check on every suite including games, with the machine otherwise idle. Server soak: an 8-bot standard lobby game played to its end on the dev server with autosave, a restart mid-game (restore_live), a model seat's chat with a bot answered, and the finished game's replay. Docs: ARCHITECTURE ('Bots and the bindings'), BOTS.md, CONTRIBUTING; DESIGN 'As built in 2-13' recording each criterion of P2.1.3 with its evidence; ops/STATUS.md.
- **Gates:** Each row of DESIGN P2.1.3 measured and recorded: (1) the bot soak and mixed chaos have 0 failures and 0 panics, peak memory reported; the corpus sweep is clean. (2) The bot golden sets are identical on 6 targets and equal to the committed files. (3) game/bot_small_330 at most 24 s and game/bot_gargantuan_330 at most 270 s; god view at most 20 ms; save lock at most 10 ms; every other budget within its hard limit. (4) The Python suite, nextest, doctests, cargo refcheck --strict --with-bot on all 262 states, cargo xtask check, ruff, the route audit, the link check and mkdocs --strict green. (5) The server soak game ends cleanly, broadcasts every bot turn, survives the restart with autosave at most one round behind, and its replay loads. (6) CI green on every workflow at the head.

