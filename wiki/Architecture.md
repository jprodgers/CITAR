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
  pool/       shared hardware: groups, grants, budgets, availability, the prompt scheduler
  ledger.py   the compute ledger: who gave and who used compute, in credits; rollups, the ratio
  usage.py, costing.py   what each activity held, and its price
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
- **A view is built only when it changed.** Every view answers with an `ETag` made from everything
  its body is made of: the game object (`EngineGame.instance`, random, since another object, a loaded
  game, counts again from 0), how many commands it has taken (`EngineGame.writes`, counted by the
  extension under the game's lock on every call that may change the game or its history; a query
  tool is a read and never counts, or the selected unit's details, which the browser asks for after
  every refresh, would make the next refresh a full view), the
  perspective, and the keys the server splices in (the seat, `info()` with its pause, agent
  statuses and seats, the session's version, `can_manage`), hashed as they are written. The browser
  sends it back on every refresh (`If-None-Match`), and an unchanged view is a 304 with nothing
  built. The engine's own revision (`Game::rev`) is not enough: it moves on writes to the state, and
  a message, a thought or a host's event is history. A view built while a command came in carries
  no tag, since its body may already be the game after it.
- **A panic is a crashed game, not a dead server.** An engine panic poisons that game: every call on
  it raises `EngineCrash`; the session pauses, keeps its last good autosave, writes a `crash-NNN`
  save and stays readable, and the other games play on. A refused action is an `ActionError` with
  the text a model reads.
- **Saves are written off the lock** (crates/citar-store): the session takes a snapshot under its
  lock, and its writer thread writes the journal's new records and then the `.citar` container
  that names them (see [The turn driver](#the-turn-driver)).

### The recap's frames

The recap gets the game's frames (the dynamic map at the end of each round: owners, improvements,
routes, features, each major's explored tiles, the cities, the units and the round's events) as
the journal stores them, never expanded on the server: a keyframe on the first round after a load
and every 64 rounds, deltas between (the tiles that changed as 8-byte records, the land newly
explored as LEB128 gaps), each record's bytes in base64. The layout is in `save::journal`'s
module documentation. A keyframe carries the improvement, feature and unit names it numbers, so a
frame recorded before a change of ruleset still names what it shows.

`citar/web/js/replaydecode.js` decodes them with no DOM or network (Node tests it against a
recorded game's frames in both formats, `citar/web/tests/fixtures/replay-delta.json.gz`).
`ReplayFrames` walks the records once to find those that decode, then serves any round by walking at
most 63 deltas from the nearest keyframe, updating one frame in place: memory stays at one frame
however long the game. The server sends only the records that decode, in order (a round after a
lost journal chunk is left out until the next keyframe): each delta names its base by a hash the
browser does not compute. The same module reads the replay's negotiations as the threads a round
shows, and leaves their words out of the round's messages (the engine records each chat entry as a
message to the other side too).

Until 0.1.6 the server expanded every frame into Python's shape, each layer of each round whole;
that format (`replay_data("full")`) was retired once the browser decoded the deltas.

### Bots in the server, and without one

A bot seat's `BotAgent` asks its session for a drive of the bot seats on its turn
(`GameSession.drive_bots`): one seat's turn at most, every bot seat passed so a chat one bot opens
with another is answered inside the drive. After the drive the session does what it does after any
tool call: bumps the version, records the turn's metrics (the bot's actions, taken and refused),
wakes the seats a chat now waits on, broadcasts the turn and autosaves at a new round. Headless
games (`citar sim`, `citar balance`, the lab) run whole games through `engine_api.run_game`, the
runner of `crates/citar-sim`, with the GIL released, so a lab runner plays one game a core.

### Hybrid seats: the bot's turn, the model's diplomacy

A hybrid seat (`design/phase3-features.md` P3.6) is the engine's `hybrid` controller: its bot plays
the turn with the model's kinds of diplomacy switched off (`citar_bot::Owners`, set on the handle
from the seat's `hybrid.owners`), and the engine's drive stops at `hybrid_diplomat` once the bot has
played. `HybridAgent` (`citar/agents/hybrid_agent.py`) is the seat's agent, on the driver thread:

1. under the lock, `drive_bots` (every seat with a bot, bot and hybrid seats alike), which stops at
   `hybrid_diplomat`;
2. off the lock, when a trigger fired and no phase ran this turn, the **diplomat phase**: one
   `diplomat` ticket on the model's machine (30 s to start, `phase_seconds` once started, eight steps),
   the static diplomat prompt (`prompts.DIPLOMAT_SYSTEM`), and the tools of the model's kinds, each
   call through `GameSession.diplomat_call`: a tool none of its kinds gives it is refused, and a deal
   it offers, counters with or accepts that includes anything the bot decides goes through only when
   `EngineGame.bot_evaluate_call` (`citar_bot::evaluate_call`) values it at zero or more from the
   seat's side. That deal is the engine's own reading of the call (`Game::offer_of_call`: the
   arguments coerced and checked as `execute` does), so nothing the model writes reaches the engine
   as a deal the veto did not value. A pause from the game screen holds the phase before its next
   model call or tool call, its ticket given back until the resume;
3. under the lock, the drive again: the turn ends, or the drive stops at `awaiting_reply` for chats
   that wait on another seat or on this seat's model, waited for as a bot seat waits.

A chat that waits on a hybrid seat goes to its line twice: its bot's pass on the bot pool
(`EngineGame.answer`, which answers what the bot owns and defers the rest), then, for what was
deferred, its model on the model pool, in a `ChatThread` on a `chat` ticket whose start deadline is
the chat clock's. When the ticket is refused or expires, or the model fails, the **fallback bot** (the
same profile with every kind its own, on the seat's one memory) answers (`GameSession.fallback_answer`);
the chat clock's deadline does the same (`chat_fallback`). While a phase runs the seat's model answers
no chat on its own ticket: what comes to it is told to the phase, and the line is dispatched again
when the phase ends. The seat holds no lease: its prompts are tickets, admitted for `pooled`, so it
never occupies its machine and quiet hours or a make-way pause never pause its game. The phases are
`metrics.data["diplomat"]`, the refused calls `metrics.data["diplomat_refusals"]`.

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

## Phase 3: people, machines and shared work

0.1.6's server side (`design/phase3-features.md`) in one place; each part has its own section or page.

- **Who may see and do what.** Every game and every kind of job (benchmark suites and runs, probes and probe runs,
  lab experiments, scenarios, maps, bot profiles, reports) has an owner and a visibility, `private | allowlist | link
  | server | global` (`citar/auth/access.py`, `citar/items.py`, `owned_items`); [Authorisation](#authorisation)
  answers every question of it, a caller who may not see a thing gets 404, and a moderator may only unpublish.
  Profiles have a visibility per field, a members-only directory and an avatar checked by its header; administrators
  have a users page and an audit log ([ACCOUNTS.md](Design-accounts-and-sharing)). The schema is one fresh Alembic baseline:
  a database of 0.1.5 is renamed aside at the first start, never changed. A request has one database session
  (`citar/auth/deps.py`'s `get_db`, taken function-scoped), committed when its route returns and before its answer
  is sent, so the caller's next request reads what this one changed.
- **Machines are rows.** The old `config/servers.json` is imported once into the `servers` table under its own ids
  and renamed; every machine, the server's own, one it dials directly or one a member's helper connects from, is a
  row on one Servers page. A member's seat names a machine and never describes one (no base URL, key or key
  variable from a member reaches a provider), the routes that touch the host (its hardware scan, LM Studio, its
  environment) are an administrator's, and a revoked token, a disabled or a deleted machine drops its helper at once
  ([WORKERS.md](Workers)). Each machine has its settings (slots, model swaps, LLM on or off, CPU limits for
  the next helper; hardware, models and power each `auto` or `manual`, the helper's last report always kept), a
  hardware class and a trust tier (`citar/machine_settings.py`).
- **Every model call is scheduled.** [The prompt scheduler](#the-prompt-scheduler) gives each machine one queue:
  leases for seats that play every turn, tickets for pooled prompts (chat answers, a hybrid seat's diplomat, probe
  answers, report narratives, a machine's test), never more requests than slots, no busy-waiting, a benchmark kept
  apart, and generation time measured apart from queue time. Admission decides who may use a machine, by task class
  and purpose (`citar/pool/admission.py`); the scheduler decides when; the Queue page shows it.
- **The compute ledger** (`citar/ledger.py`) records each model step and each unit of host CPU as an event with a
  consumer and a provider, priced in credits by the machine's hardware class; a member's given and used, self-use on
  neither side, roll up by the hour; budgets reserve and settle in credits; costing reads its work from it.
- **Chat and hybrid seats.** A negotiation is a chat timed by [the chat clock](#chats-the-clock-and-the-responders) and
  answered by the responders; a [hybrid seat](#hybrid-seats-the-bots-turn-the-models-diplomacy) is a bot that hands the
  diplomacy categories its seat names to a model, with the bot's veto and fallback. The client docks a chat window
  per civilization left of End Turn.
- **The client's payloads.** The recap plays from the journal's `Delta` frames ([The recap's
  frames](#the-recaps-frames)), a view answers with an ETag (304 when it has not moved), and trade options are fetched
  when the deal builder opens; a seat's view carries the minimap's block (explored land only, by era).
- **Checked end to end.** `scripts/qa_phase3.py` runs each feature on a dev server of its own, with real helpers in
  front of a scripted endpoint, and its `exit` part plays all of it at once (one game of a person, a hybrid seat, an
  LLM seat, an MCP seat and bots on a shared helper machine, with pooled work, a benchmark beside it and a server
  killed mid-chat); its `real` part does the same against a real model in LM Studio
  ([CONTRIBUTING.md](https://github.com/jprodgers/CITAR/blob/main/CONTRIBUTING.md)).

## The server

| Module | |
|---|---|
| `app.py` | The FastAPI app: routes, WebSockets, static files |
| `session.py` | `SessionManager` and `GameSession`: seats, the turn driver, the chats' responders, saving |
| `chat.py` | The chat clock: how long a chat waits on each side, and its status events (below) |
| `benchmarks.py` | The benchmark scheduler: suites, runs, restricted hours, resuming after a restart |
| `scoring.py` | Model scores |
| `metrics.py` | Per-seat, per-turn measurement |
| `auth_api.py`, `admin_api.py`, `pool_api.py`, `share_api.py`, `setup_api.py` | HTTP surfaces |
| `boot.py` | Migrations at startup, and the configuration checks that refuse to start a broken server |
| `workers.py` | The WebSocket endpoint worker agents connect to |
| `citar/pool/scheduler.py` | The prompt scheduler: one queue per machine for every model call (below) |
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

### Chats: the clock and the responders

A negotiation is a chat (`design/phase3-features.md` P3.5). Three pieces of the session play it:

- **The chat clock** (`citar/server/chat.py`, `GameSession.chat`) times every open chat by the side it waits on,
  from the game's `chat_model_reply_seconds` and `chat_person_reply_seconds`: past its time a model seat (LLM, MCP)
  declines, "(no reply in time)" (`decline_late`), and a chat waiting on a person closes as expired (`expire_chat`); a
  bot is never timed. It first cancels the answer still in progress for that chat (`cancel_chat_answer`). Each action
  is an engine call that a chat which moved meanwhile refuses, so a late answer and the deadline leave one outcome. It
  stands still while the game is paused (`settle` before a pause or a resume changes it), restarts a chat's time when
  its entries or awaited seat change (the session's event hook, `note`), starts afresh in a loaded session, and, in a
  benchmark game, holds a chat whose answer is a prompt queued behind a benchmark (`answer_status`: the ticket's or the
  conversation's status says so) until that prompt starts. Its thread wakes on the session's condition and at least
  once a second, records and survives every exception, and the driver starts it again if it dies (`ensure`); an
  action that failed on a chat that has not moved is tried again after 1 s, doubling up to a minute (a chat between
  two people has no agent's safety cap). Its time is the session's `clock`, which tests replace with a fake one
  (`tests/chatfake.py`).
- **The responders** (`citar/agents/responders.py`): a chat that comes to wait on a bot or a model seat is handed to that
  seat's line, which answers one chat at a time in order, on a pool of `CITAR_RESPONDERS` (16) threads for model answers
  and one of `CITAR_BOT_RESPONDERS` (2) for bot answers, so a bot is never behind models waiting for a busy GPU; each
  answer names its pool, so a line whose next answer is the other kind's moves there. A machine's answers take no
  more model threads than it has slots (and at most half the pool): a line whose machine has that many in flight is
  parked, holding no thread, its chat told its place (`queued`, as the machine tells its own line), and takes the
  place of the next of its answers to end, so a queue on one busy machine never starves another machine's seats. A
  halt (`cancel_agent`: a suspension, a seat change, a crash, a stop) never declines: the chat waits for the seat's
  next responder (a parked line leaves its machine's queue), dispatched again on resume, `restore_live` and a seat
  change (`dispatch_waiting`); a loaded game's models answer nothing until it is first resumed (`held_from_load`),
  its bots at once. Only an answer that ran and failed outright is declined, "(no response)".
- **The model seat's conversations.** `LLMAgent` keeps one `ChatThread` per open chat: a slim fixed system prompt
  (`prompts.chat_system_prompt`), the chat's history once, then only what changed, on the seat's lease, stopping when
  the chat moves. On its own turn the agent's waits for the other side (`_await_answer`, the tool's own minute;
  `_end_turn`, until every chat it opened is settled) end when the clock settles the chat, with a safety cap of twice
  the game's longest timeout; an answer that comes later is put in front of the model before its next step
  (`_chat_news`). `BotAgent.play_turn` waits the same way.

**Status events.** `{"type": "negotiation_status", "negotiation", "players", "waiting_on", "kind", "state", "position",
"eta_s", "deadline", "left_s", "paused", "entries"}` goes out on the game's socket: `waiting` (with the deadline) from
the clock, `queued` (with the place and ETA) and `thinking` from the answering conversation's requests in the prompt
scheduler (`ScheduledConversation.on_status`, delivered by the notifier thread), `closed` (with the chat's `status`)
from the session. The clock tells a chat `waiting` when it begins to wait, never over an answer's `queued` or
`thinking`, which stand until the answer ends. Since a `queued` stands, the notifier's late ones are dropped: a
conversation passes its place on only while a step still waits for its slot, not once it started or its wait was
cancelled (a halted answer, whose end already said `waiting`). A pause or a resume tells each chat again where it stands, with its
new deadline: an answer that goes on through a person's pause keeps its `queued` or `thinking`, and a suspension,
which halts the answers until the resume, says `waiting`. `entries` is the chat's
length when it was said, so a client tells a late status from a current one. The socket gives it to the seats in
`players` and to spectators while the god view is allowed, and to nobody else.

### The prompt scheduler

Every model call of a machine (a row of `servers`, whoever dials it) goes through `citar/pool/scheduler.py`
(`design/phase3-features.md` P3.3), so that no machine is sent more than it has slots for and nobody waits by
sleeping. `make_conversation(cfg, system, tools, binding)` wraps the provider's conversation in a
`ScheduledConversation`, whose every step takes a slot of its binding's machine first:

- A **lease** is a model seat that plays every turn. It belongs to the session's seat (`GameSession.lease_for`), not to
  its agent, so it outlives agents made again after a seat change or a resume. `GameSession.open_leases` makes one for
  every living model seat with a machine when the session is registered (`start`: created, loaded paused, brought
  back by `restore_live`, reloaded by a benchmark, launched from a scenario), so a game that will want a machine holds
  it before its seat is asked anything; it is released when the seat changes machine, model or type (`update_seat`
  makes the new one at once) or is eliminated (the engine's `eliminated` event), and for good when the game is over,
  stopped or crashed. A make-way pause (`pause_reason.kind == "queue"`) releases the leases and the resume makes them
  again; every other pause keeps them. While a game makes way the driver gets no agent for a model seat and
  `lease_for` makes no lease, except for a chat's answer (`lease_for(answering=True)`), whose lease and agent go when
  the seat's last answer ends (`_answered`). `get_agent` keeps a model seat's agent only if, under the game's lock,
  the seat still plays the block the agent was made from and its lease is live (`suspend`, `update_seat` and
  `_crashed` change, cancel and release in one hold of that lock), and makes again one whose lease was released. A
  lease is capped by the owner's grant for the work its game was admitted for (`benchmark`, `probe`, else `game`). `pool.seats.occupied()` lists the leases live on a machine, which is what
  benchmark jobs wait for and `usage.session_servers` counts as held. A probe's turn case holds a non-lending
  `probe_case` lease for the case (`GameSession.lease_ref`/`lease_label` name it for its run), made once the
  machine's leases leave a slot (`ProbeRunner._wait_machine`, the job rule). It is *active* while its
  seat is
  mid-turn (the driver calls `activate()` and `idle()` around `play_turn`) and *idle* otherwise; the agent also idles
  it while it waits on a chat reply or a pause, so a seat waiting on its counterpart never keeps the slot the
  counterpart needs. A lobby game's idle lease lends its slot; a benchmark's and a probe case's never do. A
  benchmark lease's first request waits for the machine's pooled requests to drain; the session adds the wait to the
  job's `drain_s` and a drain cut short to `contaminated` (`benchmark` in its saves), whose timings
  `scoring.compare_models` leaves out of the speed score.
- A **ticket** is one pooled prompt of a few steps (a chat answer, a diplomat phase, an answer of a probe's offer or
  message case, a report's narrative, the Servers page's test), with a class, a priority (its class's base plus the
  queue priority of the work it is part of, its `ref`, which it follows: `Scheduler.set_priority`) and a deadline to
  start by. It is admitted when it is made (`pool.seats.ticket`: `partial_llm`, the work's own capability and a grant
  for `pooled`, with `in_flight=None`, so a busy machine queues it; the grant's concurrency is the consumer's cap), and
  a refusal is its `refused` with the admission's reason. Model seats are admitted by their task class
  (`pool.seats.seat_class`: `llm` full, `hybrid` partial). `Machine.view` is what the Queue page lists
  (`/api/queue`, told per viewer by `server/work_access.py`). A probe's answer is asked again when the machine cut it
  off (`yielded`, `borrow_cap`, `benchmark`), and its ticket does not wait for benchmarks: the probe run waits for one
  as a run (`ProbeRunner._wait_machine`, with its waiting note). The report runner builds each report on a thread of
  its own; a build whose narrative waits for its machine is not counted against `MAX_BUILDS`, and each member has at
  most `NARRATIVES_PER_OWNER` narratives under way.
- **The rules**: in flight (draining included) never above capacity; lease requests first and in arrival order; a
  ticket only on a slot no active or non-lending lease needs, on a loaded model, outside quiet hours, under its
  consumer's cap and never beside a benchmark; tickets by priority, then round-robin across members, then first come.
  `tests/test_scheduler.py` checks them as invariants at every grant over 500 random schedules.
- **Threads.** One lock and one condition per machine. Only `acquire` blocks (in ticks of a second, noticing
  cancellation and deadlines), never with a game's lock held: in test mode it asserts so. Cancelling, releasing,
  `idle` and `activate` only change counters, so `suspend`, `update_seat` and a crash can cancel a waiting or running
  call under the game's lock. Status callbacks run on the scheduler's notifier thread, after the machine's lock is let
  go.
- **Draining and metering.** A request abandoned on a helper keeps its slot until the hub hands the scheduler the
  helper's late frame (`late_frame`), or the connection it went out on goes (`helper_changed`: each connection has its
  own `serial`). After each step the listeners get a `StepEvent` with the generation time (bounded by what the server
  observed), the queue time and the tokens; the compute ledger (`citar.ledger.listener`) is the listener, which
  prices each step, settles its budget hold and queues its event for the ledger's writer. The agent adds queue
  and chat waits to the turn's waited time, so they never count against `max_turn_seconds`, and shows its seat as
  `queued` (with its place) while a step waits (`ScheduledConversation.on_wait`).

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
| `hybrid.py`, `hybrid_agent.py` | A hybrid seat's settings, and its agent: the bot's drive, the diplomat's phases, its chats and its fallback ([Hybrid seats](#hybrid-seats-the-bots-turn-the-models-diplomacy)) |
| `mcp_server.py` | The MCP bridge |
| `providers/anthropic_provider.py` | Thinking, prompt caching, refusal fallbacks |
| `providers/openai_provider.py` | Native and JSON tool modes, for every OpenAI-compatible endpoint |
| `providers/completion.py` | The one parser of a model's reply: tool calls native or written as text, repairs, the JSON protocol ([One parser](#one-parser-for-every-completion)) |
| `providers/worker_provider.py` | Sends the request down a worker's WebSocket instead |
| `providers/dryrun.py` | Answers plausibly without a model, for testing everything else |

A provider's only job is turning a request into a completion. The turn loop, the guard rails and
the metrics are provider-independent, which is what makes a comparison across providers meaningful.

### One parser for every completion

Every OpenAI-format reply the server receives goes through one function, `parse_completion` in
`providers/completion.py`: native tool calls, arguments that are not JSON, XML calls a local server
merged into one call's arguments, calls a model wrote as text (Qwen/Hermes XML, GLM tags, plain tool
names, Gemma 4's own syntax), and the JSON protocol of tool mode `json`. `OpenAIConversation` calls it
on what an endpoint the server reaches itself returned; from protocol 2 on, a helper sends the
completion exactly as its endpoint returned it and the server calls the same function, so a model's
quirk fixed once is fixed for every machine, and nothing about parsing ever ships to a helper. The
JSON protocol's prompt is built here too (`system_prompt`), so it reaches a model however the request
goes out. `tests/data/completions/` holds raw completions with what the parser makes of each.

---

## The browser client

Plain ES modules, no build step, no framework. `citar/web/js/` is served as-is, which means editing
a file and reloading is the whole development loop.

| | |
|---|---|
| `app.js` | The router |
| `api.js` | Every HTTP call, CSRF, share keys |
| `game.js`, `render.js`, `hex.js`, `panels.js` | The game screen and the canvas renderer |
| `replay.js`, `replaydecode.js` | The recap: `replaydecode.js` decodes the frames (see [The recap's frames](#the-recaps-frames)) and reads the chats as threads, with no DOM or network, and Node tests it |
| `chat.js`, `chatmodel.js` | The chat windows docked left of End Turn: one per civilization, opened from the socket (a `negotiation` event reads `GET /api/games/{id}/negotiations/{nid}`; `negotiation_status` moves the status line), with the deal builder inside, which fetches its trade options (`GET /api/games/{id}/trade-options/{other}`) while it is open: when it opens, after each entry in its chats and at each turn. The view carries no chat clock, so an open chat the client holds no clock for is read once at each version. End Turn's click is decided in `ChatDock.endTurnClick` (`GameScreen.onEndTurn` only calls it); an armed End Turn ends the turn on a view fetched after the last chat closed (`GameScreen.syncView`; a 304 saying the view in hand still is counts, `GameScreen.viewSeq`), so a decision a deal brought stops it. `chatmodel.js` decides, with no DOM or network: entries merged by `seq`, each window's state and message count, End Turn's states (answer first, waiting, armed), the dock's layout (clear of End Turn and of the unit, city and side panels shown), and the drafts no update touches. Node tests both (`citar/web/tests/`, `chat.js` over stand-ins for the DOM) |
| `seats.js` | What the pages say and send about seats, with no DOM or network: every seat's label ("Hybrid: standard + dry-run"), what an AI seat is doing (a hybrid seat's "bot playing", "diplomat thinking", "queued #n"), and a hybrid seat's kinds of diplomacy, presets and handicap with the payload the lobby, the Join/Seats dialog and the scenario seats send. Node tests it, against the payloads `tests/test_hybrid_lobby.py` creates games from (`citar/web/tests/fixtures/hybrid-seats.json`) |
| `lobby.js`, `benchmarks.js`, `models.js`, `lab.js`, `probes.js`, `scenario.js`, `editor.js` | The other screens (the lobby's, the Join/Seats dialog's and the scenario seats' hybrid seat form is `lobby.js`'s `hybridForm`) |
| `pool.js`, `servers.js`, `machines.js` | The Servers page (`#/pool`): every machine the viewer owns or may use and the groups that share them; the machines' editors as dialogs; the page's rules (its old `#/servers` addresses, and which routes a viewer may call for a machine), kept apart so Node tests them (`citar/web/tests/`) |
| `reports.js`, `console.js`, `setup.js` | Reports, operator console, first-run wizard |
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
| Machines (the server registry) | the database's `servers` table; a pre-0.1.6 `config/servers.json` is imported once and renamed | rows, each machine's configuration as JSON |
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
