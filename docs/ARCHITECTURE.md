# Architecture

What the pieces are, why they are separated the way they are, and where to change things.

```
citar/
  engine/     the rules. No I/O, no network, no database
  engine_api.py  the only door to the engine: everything outside engine/ and bots/ goes through it
  bots/       the scripted opponent
  agents/     adapters that let a model take a seat
  server/     FastAPI app, sessions, turn driver, benchmark scheduler
  auth/       accounts, sessions, permissions
  pool/       shared hardware: groups, grants, budgets, availability
  db/         SQLAlchemy models
  reports/    report building and rendering
  worker/     the outbound agent that serves models from another machine
  web/        the browser client
  wizard/     first-time setup
  data/       the ruleset, as JSON
```

---

## The one rule

**`citar/engine/` does no I/O.** It does not read files at runtime, open sockets, touch the
database or know what a request is. It takes a game state and an action and returns a new state or
an error.

Everything else follows from that:

- The same engine runs a browser game, a benchmark, a headless simulation and a lab experiment.
- A game is a value. Saving it is `json.dumps`; loading it is the reverse; the replay is a list of
  them.
- The bot, the LLM adapter and the HTTP API are all *callers*, none of them privileged. A model
  cannot do anything a human could not, because there is only one set of actions.
- Tests are fast and deterministic. 456 of them run in about four minutes with no fixtures.

The ruleset is loaded once at import, which is the one exception, and it is read-only.

## The one door

**Outside `citar/engine/` and `citar/bots/`, nothing imports the engine except
`citar/engine_api.py`.** The server, the agents, probes, benchmarks, the lab, balance runs and
`citar sim` hold an `EngineGame` and call its methods; they never touch `Game`, the state or a bot's
internals, and they read a saved game's state only through `engine_api.state_summary`. The facade's
`__all__` is its whole public surface. `tests/test_engine_boundary.py` reads every module and fails on
a way round it, including a name taken from the facade that is not in `__all__`.

The reason is the Rust engine that replaces this one: with a single door, the swap is a new backend
behind `engine_api.py` rather than a change to two hundred call sites. So the facade is shaped like
the coarse Rust API — create, load and save a game; execute a tool; views, briefings and
negotiations; bot turns and whole headless games (`run_game`); scenario, map and debug operations;
the tool schemas — and returns plain data, never live engine objects. The module's docstring maps
each method to the Rust call it becomes.

`citar/bots/profiles.py` and `ratings.py` are bookkeeping about bots and may be imported from
anywhere. Tests may import the engine directly; `EngineGame.python_game` exists for them and for
engine-side tools such as `scripts/refcheck`.

---

## The Rust engine (0.1.6)

`crates/` holds the engine that replaces `citar/engine/` in this release. It was built beside
the Python engine, which runs every game until the facade switches to the Rust backend; both sit
behind `engine_api.py`. Its design is
[crates/citar-engine/DESIGN.md](https://github.com/jprodgers/CITAR/blob/main/crates/citar-engine/DESIGN.md),
and the crate's README lists the rules a reviewer checks.

| Crate | |
|---|---|
| `citar-engine` | The game. A library with no I/O, no threads, no clock, no C code and no `unsafe` |
| `citar-bot` | The bots as compiled versions (`basic-1`, the port of `basic.py`, and `idle`) that play seats through the engine's `SeatDriver` |
| `citar-sim` | The headless runner: whole games with a bot in every seat, `run_game`, the statistical baseline, the `citar-sim` CLI |
| `citar-store` | Save files: the `.citar` v2 container and the append-only journal beside it |
| `citar-py` | `citar._engine`, the Python extension the facade backs onto (PyO3, one abi3 build per OS) |
| `citar-testkit` | Every integration test, the rule-script runner, `RandomAgent`, and the `golden`, `chaos` and `soak` tools |
| `citar-refcheck` | Compares the engine's answers with the Python engine's on 262 recorded game states |
| `citar-bench` | Benchmarks: wall clock on the laptop against hard budgets, instruction counts on every pull request |
| `xtask` | `cargo xtask check`: allowed dependencies, the crate graph, layering, generated files up to date, nothing left unported |

The Phase 2 crates (bot, sim, store, py) are being filled in during 0.1.6; their design is
DESIGN.md's "Phase 2".

Inside the engine each top-level module is a layer, which may use only the layers below it:
`base` (ids, sets, the keyed RNG, maths, hex geometry), `rules` and `unique` (the compiled
ruleset), `state` and `save` (the saved game and its format), `mapgen`, `game` (every rule
system, the caches and the turn) and `api` (tools, views, the briefing, the scenario and map
editors). `cargo xtask check` enforces it.

### The compiled ruleset

The ruleset is the same JSON, `citar/data/`, embedded in the binary. Loading it compiles every
unique once: the text is matched to its UnCiv type, each parameter becomes a typed value (a stat,
an amount, an id, a compiled filter), and each conditional a typed condition. While a game runs,
no rule parses or compares a string; it asks an index for the uniques of a type and reads their
typed values.

Loading is all or nothing. A misspelt unique or field, a parameter that does not read, a name
nothing has, or a unique type the engine does not support stops the load, with every error at
once, each naming its file, object and text. The Python engine's failure mode, a unique that
silently does nothing, cannot happen.

A ruleset has an id, a hash of the parsed files (so line endings and formatting do not change
it), which every save and every digest of a game's state includes.

The engine supports every unique type and conditional the Python engine handled: the 402 types
the shipped ruleset uses and 125 more from other UnCiv rulesets, 527 of UnCiv's 637.
[MODDING.md](MODDING.md#in-the-rust-engine-016) lists what that means for a mod.

### State, writes and caches

A game is its `State`, which is saved, and its caches, which never are. Four rules hold them
together:

- **Reads take `&self`.** A query, a view, a briefing or a tool's own check cannot change the
  game, so how often a host reads can never change how a game goes.
- **Writes go through `game::mutate`.** A write either returns a `Change` (a tile, an owner, a
  unit placed or removed: writes whose consequences need the new state) or takes a `Touch` naming
  what it edits. Both move revision counters before anything can read the new state.
- **Caches are self-validating memos.** Each remembers the revisions it was computed from and
  checks them when it is read. Only if one moved does it compute again, and if the answer is the
  same as before, bit for bit, what depends on it stays valid. So a tech that changes no tile's
  yield recomputes no tile, and no cache can be stale, since no write can skip its revision. The
  unique indexes are memos too, rebuilt from their sources (techs, policies, buildings, beliefs,
  resources) instead of kept up to date by hooks.
- **Two values are committed, not live**: a civilization's happiness as conditionals and citizens
  see it (at the start and the end of its turn) and its gold rate (at the end). Citizens depend on
  both and both depend on citizens; committing them breaks the cycle, so placing citizens twice
  gives the same answer.

The cache oracle rebuilds every cache cold and compares; a write with the wrong `Touch` fails it.
It runs at every settle in the rule scripts and in the tests that turn every check on
(`DebugOptions::ALL`), every 10 steps and after the last in the properties and chaos, and every
50 rounds and at the end of each game in the soak.

### Settle

A write only records what it made stale: a city whose citizens must be placed again, a unit whose
sight changed. The consequences, citizens placed, tiles revealed, civilizations meeting, happen in
the **settle**, which runs at the end of every successful call and at fixed points of a turn, and
never after a refusal or a read. It brings sight up to date and applies what that reveals, then
places the citizens of the cities flagged, pass after pass, until none is flagged. A city whose
best placement depends on the placement itself (a ruleset can say so) stops at the first one it
comes back to. A settle takes at most `SETTLE_PASSES` (8) citizen passes: two cities whose
placements keep changing each other's yields, with no placement that suits both, stop there as
they stand, and a test build reports it as invariant SETTLE-1. Nothing is pending after a settle,
so saves, digests and snapshots are taken only there, and pending work is never saved.

So every action runs in three steps: it checks, on `&self`, and refuses before anything is
written; it applies; it settles. A refused action changes nothing: no event, the same digest.

### Determinism

The same seed and the same actions give the same game, digest for digest, on Windows, Linux and
macOS, on x64 and arm64. Each random draw is keyed by what it is for (the seed, a purpose, the
turn, the unit) rather than taken from one stream, so a new draw in one place moves no other.
Maths goes through `libm`, collections iterate in insertion or id order, and clippy bans the
hash-ordered and platform-dependent alternatives. Golden files hold the digests, and CI compares
them on all five targets.

### How it is checked

| | |
|---|---|
| Reference checks (`cargo refcheck`) | 262 game states recorded from the Python engine, loaded into the Rust engine and asked the same questions in 14 groups (yields, city stats, paths, combat odds, tool errors, views, briefings and more), every group enforced. A deliberate difference is listed in `refcheck/intended.toml` with its reason, and the code that makes it cites the entry |
| Rule scripts (`tests/rules/`) | 142 TOML scripts over scenario operations, run by a Rust and a Python runner. Differences only a script or test shows are in `tests/rules/intended.toml` |
| Invariants and the cache oracle | The invariants at every settle in every test build. The oracle at every settle in the rule scripts and the tests that set `DebugOptions::ALL`, every 10 steps and after the last in the properties and chaos, every 50 rounds and at the end in the soak |
| Properties P1-P8, chaos, fuzzing and the soak | Random actions and whole random games, looking for panics, broken invariants, refusals that write and reads that change a game |
| Golden sets (`cargo golden`) | Digests on five targets, in two build profiles |
| Benchmarks (`cargo xtask perf`) | Hard budgets on the laptop; instruction counts on every pull request |

The two intended lists are the changelog's list of rule fixes (`cargo refcheck changelog
--write`).

### Where to change things in the Rust engine

| To change | Go to |
|---|---|
| A rule | Its system's module under `crates/citar-engine/src/game/`; each names the Python lines it replaces |
| A new kind of unique | `unique_supported.toml`, `cargo xtask gen-uniques`, then the systems that read it ([MODDING.md](MODDING.md#in-the-rust-engine-016)) |
| A player action | Its system's `Action` and rule, then `src/api/tools/` for the tool's schema and text |
| A cache | `src/game/derive/`: a memo, the revisions it reads, and its check in the oracle |
| Something that follows from a write | The settle (`src/game/turn/settle.rs`), never the write itself |
| A rule that should differ from the Python engine's | The fix, cited `// refcheck: <id>`, and its entry in one of the two intended lists |

---

## Engine modules

| Module | |
|---|---|
| `state.py` | The data. `GameState`, `Player`, `City`, `Unit`, `Tile` — plain dataclasses that serialise |
| `game.py` | `Game`: the state plus the operations on it. `ActionError` is how a rule says no |
| `rules.py` | Loads the ruleset JSON and answers questions about it |
| `uniques.py`, `unique_types.py` | The rule interpreter. See below |
| `hexmap.py`, `tiles.py`, `mapgen.py`, `maps.py` | Geometry, terrain, generation, saved maps |
| `movement.py`, `combat.py`, `units.py`, `workers.py` | Units and fighting |
| `cities.py`, `economy.py`, `research.py` | Cities, yields, growth, technology |
| `policies.py`, `religion.py`, `great_people.py`, `espionage.py` | The social systems |
| `diplomacy.py`, `city_states.py`, `conquest.py`, `victory.py` | Other players |
| `visibility.py`, `views.py`, `briefing.py` | What a player can see, as data and as prose |
| `tools.py` | The single registry of actions. Everything a player can do is here |
| `turns.py`, `triggers.py`, `barbarians.py`, `ruins.py` | The turn cycle |

### The unique interpreter

UnCiv expresses rules as text on objects — `[+15]% Strength <when attacking>`,
`[+2] [Food] from every [Lake]`. Rather than hard-coding each one, CITAR parses them into
`Unique` objects with typed parameters and conditionals, and the systems that care ask
`UniqueMap` what applies in a given context.

This is why most content is data. A new building with a known unique is a JSON entry; only a new
*kind* of rule needs code.

`unique_types.py` is generated from UnCiv's `UniqueType` enum by `scripts/gen_unique_types.py`.
`scripts/check_uniques.py` flags unique text matching no known type. Of the 402 unique types the
ruleset uses, 19 are unreferenced in code, mostly map-generation region hints.

---

## Tools: one registry, three interfaces

```python
@tool("found_city", "Found a city with a settler", ...)
def found_city(game, player, unit_id, name=None):
    ...
```

Registering an action makes it available to:

- **the browser**, over `POST /api/games/{id}/tool`
- **MCP clients**, through the bridge
- **the LLM adapter**, as a tool definition with its JSON schema

There is no second place to add an action, and no interface can drift from another. `GET /api/tools`
returns the whole registry with schemas.

---

## The server

| Module | |
|---|---|
| `app.py` | The FastAPI app: routes, WebSockets, static files |
| `session.py` | `SessionManager` and `GameSession`: seats, the turn driver, negotiation interrupts, saving |
| `benchmarks.py` | The benchmark scheduler: suites, runs, restricted hours, resuming after a restart |
| `scoring.py` | Model scores |
| `metrics.py` | Per-seat, per-turn measurement |
| `auth_api.py`, `admin_api.py`, `pool_api.py`, `share_api.py`, `setup_api.py` | HTTP surfaces |
| `boot.py` | Migrations at startup, and the configuration checks that refuse to start a broken server |
| `workers.py` | The WebSocket endpoint worker agents connect to |
| `admin_cli.py` | `citar admin` |

### The turn driver

A `GameSession` owns one game and drives it: when the current player is an AI, it runs that seat's
agent to completion, applies the orders, records metrics, autosaves, and moves on. Human seats wait
for HTTP. MCP seats are woken by `wait_for_turn`.

Negotiations interrupt: an AI that proposes a deal blocks until the other side answers, which is
why `wait_for_turn` returns for a negotiation as well as for a turn.

### Authorisation

**`citar/auth/access.py` is the only place that answers "what may this viewer do with this
object".** Nothing hand-rolls a permission check. Two consequences worth knowing:

- Refusals are **404 when the caller has no view permission**, not 403. A 403 confirms the object
  exists, which turns id-guessing into enumeration.
- Watching and playing are separate. A public spectator link never implies the right to play.

`scripts/audit_routes.py` walks every route — including those inside included routers — and reports
any that neither declares a gate nor appears in its list of deliberately public routes, each with a
reason. Run it after adding a route; CI runs it with `--strict`.

---

## Agents

| | |
|---|---|
| `llm_agent.py` | The loop: briefing, tool calls, guard rails, limits, metrics |
| `prompts.py` | What the model is told |
| `bot_agent.py` | Wraps the scripted bot in the same interface |
| `mcp_server.py` | The MCP bridge |
| `providers/anthropic_provider.py` | Thinking, prompt caching, refusal fallbacks |
| `providers/openai_provider.py` | Native and JSON tool modes, for every OpenAI-compatible endpoint |
| `providers/worker_provider.py` | Sends the request down a worker's WebSocket instead |
| `providers/dryrun.py` | Answers plausibly without a model, for testing everything else |

A provider's only job is turning a request into a completion. The turn loop, the guard rails and
the metrics are provider-independent, which is what makes a comparison across providers meaningful.

---

## The browser client

Plain ES modules, no build step, no framework. `citar/web/js/` is served as-is, which means editing
a file and reloading is the whole development loop.

| | |
|---|---|
| `app.js` | The router |
| `api.js` | Every HTTP call, CSRF, share keys |
| `game.js`, `render.js`, `hex.js`, `panels.js` | The game screen and the canvas renderer |
| `lobby.js`, `benchmarks.js`, `models.js`, `lab.js`, `probes.js`, `scenario.js`, `editor.js` | The other screens |
| `servers.js`, `pool.js`, `reports.js`, `console.js`, `setup.js` | Machines, sharing, reports, operator console, first-run wizard |
| `auth.js`, `account.js` | Sign-in and account settings |

Check a change with `node --check` on a **`.mjs` copy** of the file: on a `.js` file Node parses it
as CommonJS, where some module-level syntax errors do not error. A broken template literal inside a
ternary once passed that check and broke the entire module graph in the browser. CI does it the
right way.

---

## Data and state

| Kind | Where | Format |
|---|---|---|
| Ruleset | `citar/data/ruleset/` | UnCiv-derived JSON, generated |
| CITAR additions | `citar/data/custom/` | Same format |
| Game settings | `citar/data/game.json` | Map sizes, lobby defaults, AI limits |
| Saved games | `saves/<id>/*.citar` | Gzipped JSON |
| Maps, scenarios, probes | `saves/maps`, `saves/scenarios`, `saves/probes` | JSON |
| Server registry | `config/servers.json` | JSON |
| Usage ledger | `saves/usage/*.jsonl` | One line per activity, no prices |
| Accounts | the database | SQLite or PostgreSQL |

`citar/paths.py` resolves all of it. Nothing else computes a path from `__file__`.

---

## Where to change things

| To change | Go to |
|---|---|
| A number, a unit, a building | `citar/data/` — it is data |
| A rule that has a unique | `citar/data/` — the interpreter handles it |
| A new kind of rule | The engine module that owns the system, plus `unique_types.py` |
| A new player action | `engine/tools.py` — it reaches all three interfaces at once |
| Something the server needs from a game | `engine_api.py`, then the engine behind it |
| How the bot plays | `crates/citar-bot`, as a new version (`basic-N`) when existing results must not move, or a profile's parameters; A/B it ([BOTS.md](BOTS.md)) |
| What a model is told | `agents/prompts.py` and `engine/briefing.py` |
| A screen in the browser | `web/js/`, no build step |
| Who may do what | `auth/access.py`, and only there |
