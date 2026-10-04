# AI players

Four kinds of player can hold a seat. They all use the same interface, which is the point: a
scripted bot, a local 4B model, Claude through the API and a human in the browser are issuing the
same orders through the same rules.

| Seat | Who plays | Setup |
|---|---|---|
| **Human** | You, in the browser | Nothing |
| **Scripted bot** | The built-in rule-based AI | Pick an aggression level |
| **LLM** | A model the server drives | Pick a server, model and load profile |
| **MCP client** | An external agent — Claude Code, Claude Desktop | Copy the command from Join / Seats |

---

## How a model takes a turn

When it is an LLM seat's turn, the server:

1. Builds a **briefing** — a written description of the visible map, the civilization's cities and
   units, what needs a decision, available techs and policies, diplomatic standing, and any
   messages received.
2. Sends it to the model with the tool schema.
3. Runs whatever tools the model calls, feeding results back, until the model calls `end_turn` or
   hits a limit.
4. Records everything: each call, its result, timing, tokens, and the model's reasoning if it
   exposes any.

A typical turn is one to four model calls, because the briefing is written to be self-sufficient —
an earlier version made the model ask a dozen questions before it could act, and turns took seven
to fifteen minutes.

Every tool a model can call is also available over HTTP and to MCP clients. They are registered
once, in `citar/engine/tools.py`, and reach all three interfaces automatically. See
[API.md](API.md).

---

## Local models

LM Studio, Ollama, llama.cpp, vLLM, text-generation-webui — anything that speaks the OpenAI
protocol.

`citar setup` finds the ones already running. To add one later, open **Servers**, add a machine,
and press **Detect models**. For llama.cpp or vLLM, choose *OpenAI-compatible* and give the base
URL (`http://localhost:8080/v1`).

A PC reached through LM Studio's **LM Link** is its own server — pick the device under *Where the
models run*.

### Settings that matter

From testing with `qwen3-27b` and similar:

- **Reasoning effort `low`** — the default for local presets. About four times faster, with no
  measurable loss in play quality. The gains from high effort go into deliberating about things the
  briefing already answered.
- **Context length** — the briefing plus a turn's tool calls runs to a few thousand tokens. 32k is
  comfortable; 8k starts truncating in the late game, which looks like the model forgetting its own
  cities.
- **Tool mode `json`** — for models that struggle with native tool calling. The model replies with
  a JSON list of calls and the adapter parses it.
- **Model size** — a 4B model plays a coherent early game and falls apart around the mid-game. A
  27B model plays a recognisable game of Civ and takes five to ten minutes a turn on a consumer
  GPU. See the [FAQ](FAQ.md#how-strong-are-the-models).

### Changing a model mid-game

**Join / Seats** → change the settings → **Apply**. It takes effect on that civilization's next
turn. This is how you rescue a game where a model is thrashing.

---

## The Anthropic API

On the **Servers** page, open *Anthropic API*, paste your key under **Connection → API key**, and
pick a model for an LLM seat.

The adapter uses adaptive thinking (summaries go to the replay's "AI thoughts"), prompt caching —
which matters, because the briefing repeats most of its content turn to turn — and server-side
refusal fallbacks. Effort is set per model on the Servers page and can be overridden per seat.

**Keys are never stored in the project folder.** They go to your OS credential store (Windows
Credential Manager, macOS Keychain, the Linux Secret Service), or an environment variable you name,
or an encrypted file outside the project for headless servers. They are never returned to the
browser, never written to a save, and never appear in a report.

---

## MCP clients

1. Create a game with an **MCP client** seat.
2. **Join / Seats** → copy the `claude mcp add …` command.
3. Run it, start your client, and tell it to play.

A prompt that works:

> You are a player in the CITAR game via the citar MCP tools. Call wait_for_turn, and whenever it
> is your turn, read get_briefing, play the turn well and call end_turn. Answer negotiations with
> respond_negotiation. Keep your long-term strategy in write_notes. Continue until the game is
> over.

The bridge is `citar-mcp` (`citar_mcp.py` in a checkout). It exposes every game tool plus
`wait_for_turn`, which blocks until it is that seat's turn or a negotiation needs an answer — so
the agent waits rather than polling. On the seat's own turn, after it opens a negotiation, it waits
for the other side's answer, since `end_turn` is refused until the negotiation is settled.

Any seat's controller can be switched mid-game from **Join / Seats**, including handing a human
seat to a model or the reverse.

---

## Guard rails

Weaker models fail in characteristic ways, and CITAR handles the common ones rather than recording
a bad score for a fixable problem. All of these are per-seat settings.

- **No repeated successful actions.** An identical successful order is not carried out twice in the
  same turn.
- **Unchanged queries.** A repeated identical query returns "unchanged" rather than a wall of the
  same text.
- **Text that should have been a tool call is recovered** — Qwen and Hermes XML, GLM-style
  `<arg_key>` tags, and bare `tool_name{...}` lines.
- **TURN PROGRESS.** After each batch of actions the model is told what is still unhandled,
  including cities that can still bombard.
- **ALERTS.** The briefing opens with problems that need action and the usual fix for each: falling
  gold, unhappiness, threatened cities (with the exact `city_attack` call), civilians near enemies,
  starving cities.

### When a turn ends

A turn ends when the model calls `end_turn`, or when one of these is hit:

| Limit | Default | Meaning |
|---|---|---|
| `stall_steps` | 10 | Model steps with no new successful order |
| `max_steps_per_turn` | 60 | Model steps |
| `max_tool_calls_per_turn` | 150 | Tool calls |
| `max_turn_seconds` | 1800 | Wall clock, including a request still generating |

A request cut off by the time limit is not retried. Every one of these is recorded in the metrics
as the reason the turn ended, which is how you tell "played well" from "ran out of budget".

### When the model server goes away

A model server that cannot be reached — LM Studio restarting, a worker reconnecting, Wi-Fi
dropping, an API returning 503 — is waited out rather than punished. The seat keeps retrying with
backoff for the **reconnect wait** (180 seconds by default; set per game in the lobby, or per seat),
and the time spent waiting does not count against `max_turn_seconds`. While it waits the game
shows the seat as *reconnecting*. A drop shorter than the wait costs nothing but the wait: the
turn carries on where it was.

If the server is still unreachable when the wait runs out, the game's **disconnect rule** applies:

| Rule | What happens |
|---|---|
| **Pause** (default) | The whole game pauses — bots included, so nobody gets free turns — with a banner saying which server is down. It resumes by itself as soon as the server answers again (checked every ten seconds), or when you press Resume, and the interrupted turn is replayed. |
| **Skip** | That seat's turn ends and play moves on. The next turn tries again, waiting out another reconnect wait first. |

A model that answers with an error — a bad request, a context overflow — is not a disconnect: the
turn ends straight away and the error appears beside the seat in the lobby.

---

## What gets recorded

Per seat, per turn: average, median and maximum turn time; model steps; seconds per model call;
tool calls, successful actions and queries; errors; identical repeated calls; repeats the guard
rail refused; malformed calls that were repaired; stall nudges; input, output and reasoning tokens;
output tokens per second; and how the turn ended.

**📊 AI stats** shows all of it live, with a per-tool table, the most repeated calls, the most
common errors and recent turns. **Download per-turn CSV** gets you the raw numbers.

Every game with an AI seat is also written to the usage ledger, so it can be costed later:
[REPORTS.md](REPORTS.md).
