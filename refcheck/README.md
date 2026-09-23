# Reference checks for the Rust engine

The Rust engine does not have to reproduce the Python engine's games bit for bit (plan §4): it has its own
RNG, its own map generator and its own caches, and it fixes Python's bugs rather than porting them. Porting
mistakes are caught in three ways instead:

1. **Reference checks.** The same game states are loaded into both engines and asked the same deterministic
   questions: tile yields, city stats, happiness, resources, costs, paths, visibility, combat odds, deal rules,
   tool errors, views and briefings. None of these depends on the game's RNG, so a different answer is either a
   porting mistake or a deliberate fix, and every deliberate fix is listed in `intended.toml` with its reason.
2. **Statistical comparison.** Hundreds of all-bot games in each engine, compared on distributions (cities,
   population, techs and score at turns 100/200/300, victory types, game length, wars and captures).
3. **Rule tests**, rewritten against the scenario-ops API (Phase 1).

This folder holds the data for the first two. The Python tools that make it are in `scripts/refcheck/`, and the
Rust tool that checks the engine against it is `crates/citar-refcheck` ([Checking the Rust engine](#checking-the-rust-engine)).

| Path | What |
|---|---|
| `scripts/refcheck/record.py` | plays seeded all-bot games and writes the fixtures |
| `scripts/refcheck/queries.py` | the questions, and the Python functions that answer them |
| `scripts/refcheck/scenarios.py` | scenario setups for states ordinary games rarely reach |
| `scripts/refcheck/baseline.py` | the statistical baseline: one JSON line per bot game |
| `scripts/refcheck/summarize.py` | distribution tables, and the comparison of two baselines |
| `scripts/refcheck/common.py` | the game loop, bots, hashing and file helpers they share |
| `crates/citar-refcheck/` | `cargo refcheck`: loads the fixtures, compares the Rust answers, reports |
| `refcheck/fixtures-mini/` | the `--quick` fixtures (committed, under 1 MB) |
| `refcheck/fixtures-late/` | three late corpus states, copied (committed, about 0.7 MB) |
| `refcheck/corpus/` | the `--full` fixtures (git-ignored, generated) |
| `refcheck/baseline/` | baseline runs (git-ignored, generated) |
| `refcheck/intended.toml` | the accepted differences, each with a reason |
| `refcheck/enforced.toml` | the groups (or paths) whose unexplained differences fail CI |
| `refcheck/ratchet.json` | unexplained differences and failed answer modules per group, which may only fall |

The tools use the Python engine directly and run their own game loop. They do not use `citar.sim`, `citar.lab`
or `citar.balance`, so the fixtures don't change when those modules change.

## Recording fixtures

```
python scripts/refcheck/record.py --quick            # 9 small states, about 15 s -> refcheck/fixtures-mini/
python scripts/refcheck/record.py --quick --check    # re-record in a temporary folder and compare
python scripts/refcheck/record.py --list --full      # the full corpus's cases and checkpoints
python scripts/refcheck/record.py --full             # the corpus -> refcheck/corpus/ (hours; see below)
python scripts/refcheck/record.py --full --only large --workers 4
```

The recorder re-runs itself with `PYTHONHASHSEED=0` and writes gzip files with no timestamps and no timings,
so the same code produces the same bytes. Errors go into files as one line each (type, message and the
innermost function of this repository, with no paths or line numbers), and their tracebacks go to the console.
`--check` therefore fails only when the Python engine's behaviour changes. If a change is deliberate, re-record
with `--quick` and commit the new fixtures together with the change.

Cases run in parallel processes (`--workers`, default: all cores but four), and each has a time budget (see
[Time budgets](#time-budgets)).

**The corpus (`--full`).** It covers:

- every size from duel to large, on every map type (continents, pangaea, archipelago, inland_sea, fractal);
- two seeds per combination (`--seeds`);
- barbarians rotating through off, normal and raging, so every size meets all three;
- checkpoints at turns 1, 25, 60, 120, 200 and 280 of a Quick game;
- huge and gargantuan maps at turns 1 and 25 only;
- two scenario cases, described below.

That is 44 games and about 250 states. The large-map games take longest: about 15 to 20 minutes each on the
laptop, so the whole corpus takes a few hours with several workers. A game that ends before a checkpoint
simply has no file for it.

**Scenario states.** `scenarios.world_war` edits a running game at its first checkpoint, using the scenario
editor's operations and ordinary tool calls:

- the player to move (A) and the next major civilization (B) are advanced to the Information era's threshold;
- A founds a pantheon and a religion, and spreads the religion into B's capital;
- each side has a spy in the other's capital;
- A builds the United Nations, with a vote due next turn;
- A declares war on B and captures a town B has just founded;
- A drops an atomic bomb two tiles from B's capital and keeps a nuclear missile;
- A and B each get armies in contact with the other;
- A gets a bomber and a fighter, and B a fighter and an anti-aircraft gun, so there are air strikes and
  interceptions to record.

The bots then play on, and the case records the next checkpoints too. Each step's result or error is logged in
the fixture's `meta.setup`. The quick fixtures include one such case (`scenario-duel-fractal`); the full corpus
has two (small at turn 60, standard at turn 120).

**The late states.** The quick fixtures stop at turn 50, so CI would never see a late game. Three corpus states
are therefore copied, byte for byte, into `refcheck/fixtures-late/`: `small-continents-normal-s1025/t280`,
`standard-pangaea-normal-s1031/t120` and `scenario-small-continents-s3001/t61`. They live in their own folder
because `record.py --quick` deletes every fixture under `fixtures-mini/` before it records. When the corpus is
re-recorded on purpose, copy the three again. With the 9 quick states and the 250 of the corpus, that makes 262.

## The fixture format

One file per checkpoint: `<case>/t<turn>.json.gz`.

```
{"meta":    {"case", "engine", "bot", "seed", "config", "turn", "current", "checkpoints",
             "query_order", "side_effects", "query_crashes", "bot_errors", "setup", "python", "hash_seed"},
 "state":   {...},
 "queries": {"<group>": {"fn": {...}, ...answers...}, ...}}
```

**`state`** is `GameState.to_dict()`, the same format as the `state` of a save file. It is taken at the start
of a round, when the first living major's turn has begun, after a load and a visibility refresh. Loading it and
refreshing visibility again changes nothing. A few details of the format:

- tiles are lists in `Tile._ORDER`;
- a player's `explored` is base64;
- units, cities and camps are keyed by their id as a string;
- the RNG state is Python's Mersenne Twister state. It is not needed for any query.

**`meta`** fields:

- `engine` and `bot` are hashes of `citar/engine` plus the ruleset, and of `citar/bots/basic.py`, at recording
  time. `engine` uses the same recipe as `citar.lab.engine_hash`.
- `config` is the `Game.new` configuration: all seats are `bot`, the speed is Quick and the difficulty is
  Prince. The bots are seeded with `seed * 101 + player id`.
- `side_effects` names, for each query group, the top-level state fields that answering changed in Python.
  Examples: `un`, which is created on first read, and a city's religious `pressures`, which are seeded on first
  read. Rust need not copy these side effects; they are listed so that nobody is surprised by them.
- `query_crashes` names the groups whose Python answer raised. Such a group's answer is `{"crash"}`
  instead, one line naming the error and where it happened (the traceback is printed on the console when
  recording). The other groups are still recorded, and the crash is a Python bug worth knowing about, not
  something to port.
- `bot_errors` lists bot crashes so far in the game, one line each (the game goes on, as in the lab).

**`queries`** hold one object per group, answered in the order of `meta.query_order`. Every group starts from
its own fresh load of `state`, with cold caches, so an answer depends only on the state and on the order of
calls within the group. Each group's `"fn"` names the Python functions (`module.function` in `citar.engine`)
whose results are recorded. Wherever a sample was drawn, the chosen inputs are stored next to each answer, so
the Rust side replays the inputs and never has to copy Python's sampling.

| Group | Answers | Inputs recorded |
|---|---|---|
| `tile_yields` | `tiles.tile_stats` for every owned tile (as its city works it) and for 150 unowned tiles (seen by nobody, then by the first major) | `idx`, `pid`, `city` |
| `city_stats` | the full `cities.city_stats` breakdown per city, plus food to grow, maintenance, health, strength, workable tiles, connection, current build, religion followers and incoming pressure | city `id` |
| `civs` | per civilization: happiness, civ stats and their per-source map, gold per turn, resource supply (net and itemised), unique index (placeholder -> count), upkeep, supply, era, tech costs, policy cost, adoptable policies, score, military strength, victory progress; plus world era and UN numbers | `pid` |
| `buildable` | per major's city: `cities.buildable_items`, production cost, turns, and `purchase_check` in gold and faith for each item | city |
| `movement` | `reachable_this_turn` for up to 40 units with moves left; 40 seeded `find_path` calls with `path_turns` and step costs | `unit`, `from`, `to`, `moves` |
| `visible` | `visibility.visible_tiles` per living major, sorted | `pid` |
| `combat_previews` | every attacker (unit, aircraft or city; not nuclear weapons) with targets, up to 2 targets each and 300 fights: strengths, modifiers, damage at rolls 0, 0.5 and 1, and `combat.preview` or its refusal. Aircraft get `can_attack_now` and their interception instead: every candidate interceptor with its chance, damage factor and damage at the same rolls | attacker, `target` |
| `deal_checks` | seeded proposals between majors who have met: the normalised proposal, whether each side's items pass `validate_items` (else the first refusal), `describe_items`, research-agreement cost, and a fresh default bot's valuation | `a`, `b`, `give`, `receive` |
| `tool_errors` | the refusal text of about 30 invalid tool calls (a call that succeeds is recorded as `ok`) | `pid`, `tool`, `args` |
| `views` | `views.client_view` for the first two living majors | `pid` |
| `briefing` | `briefing.briefing` and `briefing.turn_progress` for the first two living majors | `pid` |

## Checking the Rust engine

`crates/citar-refcheck` is the Rust side (design: `crates/citar-engine/DESIGN.md` section 9.2). For each fixture
it loads the state, asks each group's answer module the recorded questions, and compares the two answers.

```
cargo refcheck run                                   # fixtures-mini and fixtures-late, every group
cargo refcheck run --fixtures refcheck/corpus --groups civs,city_stats --case 'large-*' --json report.json
cargo refcheck run --fixtures refcheck/fixtures-mini --fixtures refcheck/fixtures-late \
                   --fixtures refcheck/corpus --strict   # the Phase 1 exit: all 262 states
cargo refcheck explain tile_yields:owned[*].yields     # the differences at a place, and the Python functions
cargo refcheck explain <intended-id>                   # what an entry explains, and what it just misses
cargo refcheck suggest                                 # [[differences]] stubs for what is unexplained
cargo refcheck ratchet [--update]                      # no count may rise; --update records the rest
cargo refcheck changelog                               # the entries as the CHANGELOG's rule fixes
cargo refcheck list                                    # the fixtures, and each group's state
```

**Answer modules.** Each group has one, `crates/citar-refcheck/src/answer/<group>.rs`, written by the package that
ports what the group checks. It rebuilds the skeleton of Python's answer from the recorded inputs and fills it
with Rust calls, so a difference is never about sampling. A group without a module is reported as `not ported`.
Three groups are synthetic rather than recorded: `uniques` (every unique text compiles, checked once per run),
`state_echo` (the state reads back as it was written) and `fixed_point` (the settle on load changes no explored
tile and no contact). A group whose Python answer crashed while recording is `python-crashed`: information,
never a difference. Groups are reported in dependency order: uniques, state_echo, fixed_point, tile_yields,
city_stats, civs, buildable, movement, visible, combat_previews, deal_checks, tool_errors, views, briefing.

**Comparison.** The two answers are compared as JSON values:

- integers exactly; any other pair of numbers within `|a-b| <= 1e-6 * max(1, |a|, |b|)`, so 3 and 3.0 are equal;
- strings exactly, with a line diff for a text over 200 characters or with a line break;
- object keys as a union: a key on one side only is `missing` (Python has it) or `extra` (Rust has it), and null
  is not the same as absent;
- lists in order, unless the group's compare spec (`compare/spec.rs`) says otherwise: keyed by a field or tuple
  position (`cities[id=9]`, `reachable[#0=412]`), a multiset (lists that are sets in meaning, such as
  `workable`, `detailed_resources`, `adoptable_policies` and the lists in `buildable.items`), or custom;
- `fn`, which names the Python functions, is not compared, and `deal_checks`' `bot_value` only with `--with-bot`.

The custom rule is for routes (`movement.paths`): a different route is `path_equivalent`, accepted without an
entry, when it starts and ends on Python's tiles, steps between adjacent tiles, and has the same turns and summed
step cost. A route with fewer turns is `better`, which needs an intended entry with `rule = "rust_le_python"`.
Anything else is a `route` difference. Reachability (a route against none) must agree.

**Paths.** Reports, `intended.toml` and `enforced.toml` share one grammar: `.key`, `["any key"]`, `[3]`,
`[pid=0]` (keyed list), `[#0=12]` (keyed by tuple position), `[*]` (any element), `.*` (any key) and `.**` (any
depth). A pattern matches a difference's whole path, so a subtree is `prefix.**`. Reports print concrete paths
such as `civs[pid=0].happiness.breakdown.Religion`.

**Explaining a difference.** Each difference is a porting mistake, which gets fixed, or a deliberate fix or
redesign, which gets an entry in `intended.toml` (format v2, described in the file's header): an id cited at the
fix site as `// refcheck: <id>`, a one-line reason written for the changelog, the places it covers, optional
`cases` globs, and optional constraints on the Python and Rust values, so an entry never hides a later,
unrelated change at the same place. Text answers (tool errors, briefings) follow the same rule: model-facing text
is ported as-is where it is fine and fixed where it is wrong (decision G), and each fix is listed. An entry that
explains nothing in a run that covered it is stale: a warning, and an error with `--strict`.

**Enforcement and the ratchet.** `enforced.toml` lists the groups, or paths within them, that are clean: an
unexplained difference there fails the run. So does a difference above an enforced path that hides it: an answer
module that failed or panicked, a missing or extra element or subtree that holds an enforced place, or a keyed
list that could not be keyed. Each system package adds its group once its answer module is clean. Everywhere
else, unexplained differences are reported and counted per group in `ratchet.json`, with failed answer modules
counted apart, since one failure replaces all of a fixture's differences. `cargo refcheck ratchet` fails when a
count rises, and also when the file is out of date: a count fell, or a group is compared for the first time.
`--update` records those (and refuses a rise), and the updated file is committed with the change, so the file
always holds the current counts. CI runs `cargo refcheck run` and `cargo refcheck ratchet` on the committed
fixtures.

**Exit codes:** 0 clean; 1 unexplained differences where `enforced.toml` covers them (with `--strict`, any
unexplained difference, or a selected group without an answer module); 2 a fixture or configuration file that
could not be loaded, or a usage error; 3 stale entries under `--strict`. The JSON report (`--json`) is the same,
byte for byte, on every run over the same inputs.

The refcheck is clean when every difference is fixed or listed, over all 262 states and 14 groups. That is Phase
1's exit condition. Start with the committed fixtures (quick, and run in CI), then run the whole corpus. The
`fn` names, which `explain` prints, tell you which Python function to read when an answer is not obvious.

## The statistical baseline

```
python scripts/refcheck/baseline.py --smoke                     # 2 short duel games, a few seconds
python scripts/refcheck/baseline.py --games 300                 # small maps, the five map types in turn
python scripts/refcheck/baseline.py --games 150 --sizes duel,standard --name python-mixed
python scripts/refcheck/summarize.py refcheck/baseline/python-<hash>.jsonl
python scripts/refcheck/summarize.py refcheck/baseline/python-<hash>.jsonl refcheck/baseline/rust-<hash>.jsonl
```

**How a run is made.** Game *i* uses seed `--seed + i` and the *i*-th combination of `--sizes`, `--maps` and
`--barbarians`. A run is resumable: games already in the output file are skipped, and a crashed game is played
again. One file is one sample:

- the default output name carries the engine and bot hashes;
- a run refuses to add to a file that holds games from other engine or bot code, or a game *i* with another
  seed, size, map type, barbarian setting, speed or turn limit than this run's game *i*. Use another `--name`.

A run stopped mid-write leaves a torn last line. The next run starts on a fresh line, and `summarize.py` skips
the torn one with a warning.

**What a line holds.**

- The game: index, seed, size, map type, barbarians, speed, turn limit, the engine and bot hashes, turns
  played, winner and victory type, bot errors, and seconds and CPU seconds. A game that crashed or ran out of
  time has `crash` and `trace` in place of the results.
- Per civilization: nation, aggression, whether it is alive, the turn it was eliminated, its final score, and
  `at`. `at` holds its stats at turns 100, 200 and 300 and at the end: cities, population, techs, score,
  military, era, policies and land, plus running totals of wars declared (all, on majors, on others), cities
  captured and cities lost.

The stats are the engine's own end-of-turn records (`GameState.stats`), so the Rust runner must write the same
lines from its own records.

**Comparing two baselines.** Each game counts once: its last finished line, or one crash if it never
finished. `summarize.py` prints, per file:

- game length;
- victory shares;
- wars, captures and eliminations per game;
- the distribution of each measure at each checkpoint: n, mean, sd and percentiles.

Given two files, it also prints each row's means, their difference and `d`, the difference in pooled standard
deviations:

- |d| of 0.2 or more is marked `*`;
- |d| of 0.5 or more is marked `**`.

This is a sanity check, not a gate: a big gap is either explained (a deliberate change in behaviour) or fixed.
Compare like with like: the same sizes, maps, barbarians and speed. With a few hundred games per side, `d` below
0.2 is well within what a different map generator produces.

## Time budgets

Both scripts run unattended for hours, so no game may hold a run up:

- **The budget.** Each game (or recorded case) has one: `--max-minutes`, or by default 60 minutes on a small map,
  scaled by map area (at least 20 minutes, about 140 on a large map). That is many times what a game takes; it
  is there to catch a hang.
- **Out of time.** A game past its budget stops at the start of its next round.
- **Stuck inside a turn.** A game stuck in one turn is interrupted wherever it is, five minutes later. Its
  traceback, printed with the crash, shows where it was stuck.
- **Either way,** the game is recorded as a crash (`GameTimeout`) and the other workers carry on. A resumed
  baseline plays it again.
- **Stuck beyond that.** If no game finishes for longer than any game may take, a worker is stuck where it
  cannot be interrupted (inside C code). The same happens if a worker process dies. The run then stops: the
  games caught in it are reported as crashes (the baseline also writes them as crash lines), the workers are
  terminated, and the script exits with 1. Run the same command again to resume.

## Caveats

- **Answers depend on the Python engine at recording time.** Re-record when it changes on purpose;
  `record.py --quick --check` says when that is needed. Once the Python engine is archived (Phase 2), the
  fixtures are the frozen reference.
- **Platforms.** Python's floats are IEEE doubles, but `**` and `math` call the platform's C library, which can
  differ in the last bit between Windows, macOS and Linux. So `--check` on another platform could, in rare
  cases, flag a rounding difference that is not a real change. The committed fixtures were recorded on
  Windows.
- **Deal valuations.** The `bot_value` in `deal_checks` comes from a fresh `BasicBot` with default parameters
  and no memory of past turns, so its war-plan terms are always empty. It is a reference for the Phase 2 bot
  port, not for the engine.
