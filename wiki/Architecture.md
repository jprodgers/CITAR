# Architecture

What the pieces are, why they are separated the way they are, and where to change things.

```
crates/       the engine, in Rust: the rules (no I/O, no network, no database), the bots, the
              headless runner, saves, and citar._engine, the extension Python reaches it through
citar/
  engine_api.py  the only door to the engine: everything goes through it
  bots/       bookkeeping about the scripted opponent: its profiles and ratings
  agents/     adapters that let a model take a seat
  server/     FastAPI app, sessions, turn driver, benchmark scheduler
  auth/       accounts, sessions, permissions
  pool/       shared hardware: groups, grants, budgets, availability
  db/         SQLAlchemy models
  reports/    report building and rendering
  worker/     the outbound agent that serves models from another machine
  web/        the browser client
  wizard/     first-time setup
  data/       the hardware collectors the Servers page offers
```

---

## The one rule

**The engine does no I/O.** `crates/citar-engine` does not read files, open sockets, touch the
database, read the clock or know what a request is. It takes a game state and an action and returns
a new state or an error.

Everything else follows from that:

- The same engine runs a browser game, a benchmark, a headless simulation and a lab experiment.
- A game is a value. Saving it is writing its state; loading it is the reverse; the replay is its
  history.
- The bot, the LLM adapter and the HTTP API are all *callers*, none of them privileged. A model
  cannot do anything a human could not, because there is only one set of actions.
- Tests are fast and deterministic.

The ruleset is compiled into the engine and loaded once, read-only; a host may give it another
(`CITAR_RULESET_DIR`, read by the extension when it loads, which does the reading).

## The one door

**Nothing imports the engine except `citar/engine_api.py`.** The server, the agents, probes,
benchmarks, the lab, balance runs and `citar sim` hold an `EngineGame` and call its methods; they
never touch the extension `citar._engine` or the module behind the facade, and they read a saved
game's state only through `engine_api.state_summary` and `save_header`. The facade's `__all__` is its
whole public surface. `tests/test_engine_boundary.py` reads every module and fails on a way round
it, including a name taken from the facade that is not in `__all__`.

The facade is shaped like the engine's coarse API — create, load and save a game; execute a tool;
views, briefings and negotiations; drives of bot seats and whole headless games (`run_game`);
scenario, map and debug operations; the tool schemas; bot versions, schemas and fingerprints — and
returns plain data, never live engine objects. It was the seam the Rust engine replaced the Python
engine of 0.1.5 through: the swap was a new backend behind `engine_api.py` rather than a change to
two hundred call sites, and the Python engine was then removed (the tag `python-engine-0.1.6` keeps
it).

`citar/bots/profiles.py` and `ratings.py` are bookkeeping about bots and may be imported from
anywhere.

---

## The engine

`crates/` holds the engine. Its design is
[crates/citar-engine/DESIGN.md](https://github.com/jprodgers/CITAR/blob/main/crates/citar-engine/DESIGN.md),
and the crate's README lists the rules a reviewer checks. Building it needs a Rust toolchain
(rustup reads `rust-toolchain.toml`); a wheel carries it compiled.

| Crate | |
|---|---|
| `citar-engine` | The game. A library with no I/O, no threads, no clock, no C code and no `unsafe` |
| `citar-bot` | The bots as compiled versions (`basic-1`, the port of 0.1.5's Python bot, and `idle`) that play seats through the engine's `SeatDriver` |
| `citar-sim` | The headless runner: whole games with a bot in every seat, `run_game`, the statistical baseline, the `citar-sim` CLI |
| `citar-store` | Save files: the `.citar` v2 container and the append-only journal beside it |
| `citar-py` | `citar._engine`, the Python extension the facade backs onto (PyO3, one abi3 build per OS) |
| `citar-testkit` | Every integration test, the rule-script runner, `RandomAgent`, and the `golden`, `chaos` and `soak` tools |
| `citar-refcheck` | Compares the engine's answers with the Python engine's, recorded on 262 game states |
| `citar-bench` | Benchmarks: wall clock on the laptop against hard budgets, instruction counts on every pull request |
| `xtask` | `cargo xtask check`: allowed dependencies, the crate graph, layering, generated files up to date, nothing left unported |

Inside the engine each top-level module is a layer, which may use only the layers below it:
`base` (ids, sets, the keyed RNG, maths, hex geometry), `rules` and `unique` (the compiled
ruleset), `state` and `save` (the saved game and its format), `mapgen`, `game` (every rule
system, the caches and the turn) and `api` (tools, views, the briefing, the scenario and map
editors). `cargo xtask check` enforces it.

### The compiled ruleset

The ruleset is JSON, `crates/citar-engine/data/`, embedded in the binary. Loading it compiles every
unique once: the text is matched to its UnCiv type, each parameter becomes a typed value (a stat,
an amount, an id, a compiled filter), and each conditional a typed condition. While a game runs,
no rule parses or compares a string; it asks an index for the uniques of a type and reads their
typed values.

This is why most content is data. A new building with a known unique is a JSON entry; only a new
*kind* of rule needs code.

Loading is all or nothing. A misspelt unique or field, a parameter that does not read, a name
nothing has, or a unique type the engine does not support stops the load, with every error found,
each naming its file, object and text. The Python engine's failure mode, a unique that silently
does nothing, cannot happen. `citar ruleset check DIR` loads a modded copy the same way, and
`CITAR_RULESET_DIR` plays one without a Rust toolchain ([MODDING.md](Modding#playing-a-modded-ruleset)).

A ruleset has an id, a hash of the parsed files (so line endings and formatting do not change
it), which every save, every digest of a game's state, the build id and every bot fingerprint
include.

The engine supports every unique type and conditional the Python engine handled: the 402 types
the shipped ruleset uses and 125 more from other UnCiv rulesets, 527 of UnCiv's 637.
[MODDING.md](Modding#what-the-engine-supports) lists what that means for a mod.

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
| Reference checks (`cargo refcheck`) | 262 game states recorded from the Python engine before it was removed, each with its answers, loaded into the engine and asked the same questions in 14 groups (yields, city stats, paths, combat odds, tool errors, views, briefings and more), every group enforced. A deliberate difference is listed in `refcheck/intended.toml` with its reason, and the code that makes it cites the entry |
| Rule scripts (`tests/rules/`) | TOML scripts over scenario operations, run by a Rust runner and, through the bindings, a Python one. Differences only a script or test shows are in `tests/rules/intended.toml` |
| Invariants and the cache oracle | The invariants at every settle in every test build. The oracle at every settle in the rule scripts and the tests that set `DebugOptions::ALL`, every 10 steps and after the last in the properties and chaos, every 50 rounds and at the end in the soak |
| Properties P1-P8, chaos, fuzzing and the soak | Random actions and whole games, with `RandomAgent`s, bots or both in the seats, looking for panics, broken invariants, refusals that write, reads that change a game and bots that loop on a refusal |
| Golden sets (`cargo golden`) | Digests on five targets, in two build profiles |
| Benchmarks (`cargo xtask perf`) | Hard budgets on the laptop; instruction counts on every pull request |

The two intended lists are the changelog's list of rule fixes (`cargo refcheck changelog
--write`).

### Where to change things in the engine

| To change | Go to |
|---|---|
| A rule | Its system's module under `crates/citar-engine/src/game/`; each names the Python lines it replaced |
| A new kind of unique | `unique_supported.toml`, `cargo xtask gen-uniques`, then the systems that read it ([MODDING.md](Modding#adding-a-new-kind-of-rule)) |
| A player action | Its system's `Action` and rule, then `src/api/tools/` for the tool's schema and text |
| A cache | `src/game/derive/`: a memo, the revisions it reads, and its check in the oracle |
| Something that follows from a write | The settle (`src/game/turn/settle.rs`), never the write itself |
| A rule that should answer otherwise than the Python engine's recorded answers | The fix, cited `// refcheck: <id>`, and its entry in one of the two intended lists |

---

## Bots and the bindings

### A bot is a seat driver

The engine plays seats through one trait, `SeatDriver`: `Game::drive` asks a seat's driver to play
its turn, or to answer a negotiation that waits on it, and stops when a seat it has no driver for
must act, a reply is awaited, or the game ends. A bot (`crates/citar-bot`) is such a driver, and so
is the tests' `RandomAgent`; a model plays through tools instead, outside the drive.

- **It reads, and acts only through tools.** A bot reads the game through `&Game`: its accessors,
  the systems' read functions and the advisor (what a city would make with each building, the
  sites worth settling, how a fight would go). It changes the game only through `Turn::act`, the
  same tool calls a model makes, refused for the same reasons. `cargo xtask check` keeps
  `&mut Game` to the bot's one driver file, so no bot code can reach a write any other way.
- **It holds nothing between calls.** What it remembers (war plans, escorts, garrisons, sites given
  up) is typed JSON in its seat's `DriverMemory`, at most 4 MiB, pruned every turn and saved with the
  game. A bot built afresh for every call plays a loaded game as the one before the save would have.
- **Its randomness is the game's.** Each kind of decision draws from its own stream, keyed by the
  game's seed, the seat and the turn (`Purpose::BotBase`), so a bot game is as reproducible as any
  other: the `bot` golden set holds four of them, round by round, on six targets.
- **Its numbers are parameters.** Each version has one JSON schema (`params/basic-1.json`, 373
  parameters for `basic-1`) generated into a struct; a profile overrides some of them, and its
  fingerprint pins the build, the version, the overrides and the aggression, so a rating always
  describes the bot that earned it ([BOTS.md](Scripted-bots)).
- **It leaves room for a model.** Per-category diplomacy switches let a model own the seat's chats
  while the bot plays the rest (the bot then answers `Deferred`), and `advice` tells that model
  what the bot would do. Phase 3's hybrid seats build on both.

### The extension

`citar._engine` (`crates/citar-py`) is the engine as Python sees it: a PyO3 extension, one abi3
build per OS for every Python from 3.11. `engine_api.py` is the only module that reaches it (the
one door), through `citar/_facade_rust.py`.

- **The GIL is released for every call that does work.** A game is a `Mutex` around the engine's
  `Game`, and each heavy call (a tool, a drive, a view, a save) runs with the interpreter's lock
  released, so two games play on two cores at once, and a slow turn never stops the server's other
  requests. Cheap reads (the turn, whose move it is, the phase) come from heads the extension
  updates after each call, without waiting for a drive under way.
- **Data crosses as bytes.** Views, replays and saves come back as the engine's own JSON (or a save
  file's bytes) and go to the client as they are: a gargantuan god view is never parsed and dumped
  again in Python.
- **A panic is a crashed game, not a dead server.** An engine panic poisons that game: every call on
  it raises `EngineCrash`; the session pauses, keeps its last good autosave, writes a `crash-NNN`
  save and stays readable, and the other games play on. A refused action is an `ActionError` with
  the text a model reads.
- **Saves are written off the lock** (crates/citar-store): the session takes a snapshot under its
  lock, and its writer thread writes the journal's new records and then the `.citar` container
  that names them (see [The turn driver](#the-turn-driver)).

### Bots in the server, and without one

A bot seat's `BotAgent` asks its session for a drive of the bot seats on its turn
(`GameSession.drive_bots`): one seat's turn at most, every bot seat passed so a chat one bot opens
with another is answered inside the drive. After the drive the session does what it does after any
tool call: bumps the version, records the turn's metrics (the bot's actions, taken and refused),
wakes the seats a chat now waits on, broadcasts the turn and autosaves at a new round. Headless
games (`citar sim`, `citar balance`, the lab) run whole games through `engine_api.run_game`, the
runner of `crates/citar-sim`, with the GIL released, so a lab runner plays one game a core.

### How the bots are checked

`basic-1` is a port, so it was first checked decision by decision against what the Python bot
decided on the 262 recorded states (`cargo refcheck bot-agreement`: every kind of decision, every
item), its deal values by `cargo refcheck run --with-bot`, and its play by the bot rule scripts.
Then as a player: one bot round on each of the 262 states with every check on (the fixture sweep),
whole bot games against the Python bot's statistics (`scripts/refcheck/summarize.py`, gates G1 to
G4), the long runs with bots in the seats (the soak and chaos take `--drivers bot` or `mixed`, and
property P8 runs with bot drivers: no read may change what a bot decides), and the server soak
(`scripts/server_soak.py`): a lobby game of bots and a model seat played to its end through a
restart. The numbers of the last full run are in DESIGN.md ("As built in 2-13").

---

## Tools: one registry, three interfaces

Every action a player can take is a tool in one registry, `crates/citar-engine/src/api/tools/`: its
name, the description a model reads, its parameters and JSON schema, whether it is a query or an
action, and when it may be used. A tool reaches:

- **the browser**, over `POST /api/games/{id}/tool`
- **MCP clients**, through the bridge
- **the LLM adapter**, as a tool definition with its JSON schema

There is no second place to add an action, and no interface can drift from another. `GET /api/tools`
returns the whole registry with schemas (`engine_api.tool_list`).

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

A save takes a snapshot of the game under the session's lock (a copy of the state, and the history
since the last save, which goes into the game's journal), and the session's writer thread writes it
off the lock: the journal's new records first, synced, then the save that names them. Autosaves the
writer has not begun when a newer one comes are passed over, and the turn that ends a round waits
for the round's autosave to be written, so the autosave on the disk is never more than a round
behind the game: a restart costs the round in progress and, while that round's own autosave is
still being written, the one before it. A session writes one journal, its timeline; loading an older save of a game that went on forks a new one, so every save stays
loadable (crates/citar-engine/DESIGN.md P2.5.3).

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
| `bot_agent.py` | Plays a bot seat: a drive of the engine's bots on its turn, and their answers to chats ([Bots and the bindings](#bots-in-the-server-and-without-one)) |
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
| Ruleset | `crates/citar-engine/data/ruleset/`, compiled in | UnCiv-derived JSON, generated |
| CITAR additions | `crates/citar-engine/data/custom/` | Same format |
| Game settings | `crates/citar-engine/data/game.json` | Map sizes, lobby defaults, AI limits |
| A modded ruleset | the folder `CITAR_RULESET_DIR` names | The same layout |
| Saved games | `saves/<id>/*.citar`, beside the game's journals `saves/<id>/journal*.cjnl` | A small JSON header and the state, zstd; the journal holds the history, one record per save |
| Maps, scenarios, probes | `saves/maps`, `saves/scenarios`, `saves/probes` | JSON |
| Server registry | `config/servers.json` | JSON |
| Usage ledger | `saves/usage/*.jsonl` | One line per activity, no prices |
| Accounts | the database | SQLite or PostgreSQL |

`citar/paths.py` resolves all of it. Nothing else computes a path from `__file__`.

---

## Where to change things

| To change | Go to |
|---|---|
| A number, a unit, a building | `crates/citar-engine/data/` — it is data (or a modded copy: [MODDING.md](Modding)) |
| A rule that has a unique | `crates/citar-engine/data/` — the compiled ruleset handles it |
| A new kind of rule | `unique_supported.toml` and the engine module that owns the system ([MODDING.md](Modding#adding-a-new-kind-of-rule)) |
| A new player action | `crates/citar-engine/src/api/tools/` — it reaches all three interfaces at once |
| Something the server needs from a game | `engine_api.py`, then the engine behind it (`crates/citar-py`, `crates/citar-engine/src/api/`) |
| How the bot plays | `crates/citar-bot`, as a new version (`basic-N`) when existing results must not move, or a profile's parameters; A/B it ([BOTS.md](Scripted-bots)) |
| What a model is told | `agents/prompts.py` and the briefing, `crates/citar-engine/src/api/briefing/` |
| A screen in the browser | `web/js/`, no build step |
| Who may do what | `auth/access.py`, and only there |


---

*This page is generated from [`docs/ARCHITECTURE.md`](https://github.com/jprodgers/CITAR/blob/main/docs/ARCHITECTURE.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
