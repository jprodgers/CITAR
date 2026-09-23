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
- Tests are fast and deterministic. 451 of them run in about four minutes with no fixtures.

The ruleset is loaded once at import, which is the one exception, and it is read-only.

## The one door

**Outside `citar/engine/` and `citar/bots/`, nothing imports the engine except
`citar/engine_api.py`.** The server, the agents, probes, benchmarks, the lab, balance runs and
`citar sim` hold an `EngineGame` and call its methods; they never touch `Game`, the state or a bot's
internals. `tests/test_engine_boundary.py` reads every module and fails on a way round it.

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
| How the bot plays | `bots/basic.py`, and A/B it ([BOTS.md](Scripted-bots)) |
| What a model is told | `agents/prompts.py` and `engine/briefing.py` |
| A screen in the browser | `web/js/`, no build step |
| Who may do what | `auth/access.py`, and only there |


---

*This page is generated from [`docs/ARCHITECTURE.md`](https://github.com/jprodgers/CITAR/blob/main/docs/ARCHITECTURE.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
