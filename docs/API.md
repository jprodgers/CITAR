# HTTP and tool API

Everything a player can do is a **tool**. Tools are registered once, in `citar/engine/tools.py`,
and reach three interfaces automatically: the browser client, MCP clients, and the LLM adapter.
There is no fourth set of actions hiding anywhere, and no interface can do something another
cannot.

This page is for driving CITAR from your own code — a custom agent, a harness, a script.

---

## Authentication

A **seat token** is issued per seat when a game is created, and grants play on that one seat. It is
what MCP clients and external agents use, and it works with no account at all.

```bash
curl -X POST http://127.0.0.1:8765/api/games/$GAME/tool \
  -H "Authorization: Bearer $SEAT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"tool": "get_briefing", "args": {}}'
```

Find the token in **Join / Seats** in the browser, which also prints the ready-made `claude mcp add`
command for that seat.

On a public server, browser requests use a session cookie plus a CSRF header instead; seat tokens
still work and still grant exactly one seat.

---

## The endpoints

| Method and path | |
|---|---|
| `GET /api/tools` | Every tool with its JSON schema. The authoritative reference |
| `POST /api/games/{id}/tool` | Call a tool: `{"tool": "...", "args": {...}}` |
| `GET /api/games/{id}/wait` | Long-poll until it is your turn or a negotiation needs an answer |
| `GET /api/games/{id}/view` | The game as this seat sees it, as JSON |
| `GET /api/games` | Games visible to the caller |
| `GET /api/rules` | The whole ruleset |
| `GET /api/games/{id}/metrics` | Per-seat, per-turn metrics |
| `GET /api/games/{id}/metrics.csv` | The same, as CSV |
| `GET /api/games/{id}/replay` | Every turn's state, for the recap |
| `WS /ws/games/{id}` | Live updates |

A tool call returns the tool's result, or an error describing what the rules refused and why. Rule
refusals are 400 with a readable message — `"That tile is not adjacent"` — because the message is
read by a model as often as by a person.

Full schemas: `GET /api/tools`, or `/docs` for the generated OpenAPI browser.

---

## The tools

59 of them. Anything that reads is safe to call as often as you like; anything that acts is subject
to the same rules a human is.

### Looking

`get_briefing` · `get_map` · `get_tile` · `get_unit` · `get_units` · `get_city` · `get_cities` ·
`get_empire` · `get_players` · `get_diplomacy` · `get_city_states` · `get_tech_tree` ·
`get_policies` · `get_religion` · `get_great_people` · `get_espionage` · `get_rules` ·
`get_events` · `get_victory_status` · `preview_attack` · `read_notes`

**`get_briefing` is the one that matters.** It is written to be self-sufficient: empire status,
cities, units with idle ones marked, what needs a decision, available techs and policies,
diplomatic messages, and an ALERTS section for problems that need action. An agent that reads the
briefing properly needs few other queries, which is most of the difference between a one-minute
turn and a ten-minute one.

`preview_attack` predicts a fight without starting one — strengths, modifiers and damage range.

### Units

`move_unit` · `attack` · `air_sweep` · `unit_action` · `found_city` · `build_improvement` ·
`unit_order` · `upgrade_unit` · `promote_unit`

`move_unit` takes a destination and paths there, spending what movement it can this turn and
continuing next turn. `unit_order` gives standing orders: fortify, sleep, explore, automate.

### Cities

`set_production` · `change_queue` · `set_auto_production` · `buy` · `set_city_focus` ·
`work_tile` · `set_specialists` · `buy_tile` · `city_attack` · `rename_city` · `city_status`

`city_attack` is the one agents most often forget: a city with an enemy in range can bombard once
per turn, free. The TURN PROGRESS note reminds them.

### Empire

`set_research` · `choose_free_tech` · `adopt_policy` · `found_pantheon` · `choose_great_person` ·
`un_vote` · `set_civ_name`

### Diplomacy

`send_message` · `open_negotiation` · `respond_negotiation` · `declare_war` · `denounce` ·
`city_state_action` · `move_spy` · `stage_coup`

A negotiation is a chat with a deal on the table: `open_negotiation` proposes, and the other side
answers in its own time — AIs in seconds, humans when they see the popup. `respond_negotiation`
accepts, rejects, counters or replies, and every entry carries a message. Neither side can end its
turn while a negotiation it is in is open; the opener can withdraw one with `reject`. A negotiation
closes by itself at its message cap (30 by default). This is the part of the game most worth
probing, because it is where a model's judgement shows.

### Turn

`write_notes` · `log_thought` · `end_turn`

`write_notes` is a private notebook that persists across turns and saves — the only memory an agent
has that is not in the briefing, and the difference between a model that has a plan and one that
improvises every turn. `log_thought` records reasoning for spectators and the replay.

---

## Writing an agent

The loop:

1. `GET /api/games/{id}/wait` — blocks until it is your turn.
2. `get_briefing`.
3. Act. Read the result of each call; the guard rails refuse an identical successful action twice
   in one turn, and repeating a query that has not changed returns "unchanged".
4. `end_turn`.
5. Repeat.

Worth knowing:

- **Answer negotiations.** `wait` returns for a negotiation as well as for a turn. An agent that
  ignores them stalls the game for everybody.
- **The turn ends without you** if you hit a limit — 10 steps with no progress, 60 steps, 150 tool
  calls, or 1800 seconds. All four are per-seat settings, and which one you hit is recorded.
- **Errors are information.** A refusal explains the rule. Retrying an identical call unchanged is
  the single most common failure of weak agents, and CITAR counts it.

The built-in adapter (`citar/agents/llm_agent.py`) is a working reference for all of this.

---

## MCP

```bash
citar-mcp --server http://127.0.0.1:8765 --game <id> --token <seat token>
```

Or copy the ready-made command from **Join / Seats**. The bridge exposes every tool above plus
`wait_for_turn`, which blocks rather than polling. See [AI_PLAYERS.md](AI_PLAYERS.md#mcp-clients).

---

## Stability

Before 1.0, tool names and arguments may change between minor versions; when they do, the release
notes say so. `GET /api/tools` is always the truth for the version you are running, and an agent
that reads the schema rather than hard-coding it will survive most changes.
