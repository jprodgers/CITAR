# AI players

Four kinds of player can hold a seat. They all use the same interface, which is the point: a
scripted bot, a local 4B model, Claude through the API and a human in the browser are issuing the
same orders through the same rules.

| Seat | Who plays | Setup |
|---|---|---|
| **Human** | You, in the browser | Nothing |
| **Scripted bot** | The built-in rule-based AI | Pick an aggression level |
| **LLM** | A model the server drives | Pick a machine, model and load profile |
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
once, in the engine's tool registry (`crates/citar-engine/src/api/tools/`), and reach all three
interfaces automatically. See
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

### A seat names a machine

A seat's model settings name one of the machines on the Servers page (its `server_id`), a model and
a load profile, plus the seat's own overrides (persona, effort, limits). They never describe a
machine: on a server with accounts, a member's seat that carries a base URL, an API key or the name of
an environment variable to read one from is refused ("Pick a machine for this seat"), when a game is
created, when a seat is changed, when a scenario is saved and when one is launched. Otherwise
anybody could make the server dial any address with any of its keys. A machine the member may not
use is refused too: every machine is a row on the Servers page, the site's own included (the host, the
online APIs), and a member seats one only if it is theirs or its owner shared it with them in a server
group. An inline dry run is the exception: it dials nothing, so anyone may use it.

Only the seats that play a model are held to this. A bot, human or MCP seat may carry model settings
left over from when it was an LLM seat; nothing plays them, so they are never a reason to refuse the
game (a member's are dropped, or kept only as far as they name a machine). Making such a seat an LLM
seat later checks the settings it then has. Renaming an LLM seat checks nothing: its settings were
checked when they were set.

Administrators, and the owner of a local install (who is one), can still give a seat a model
configuration of its own, as tests and scripts do. A seat in a member's game that somehow has one
plays no model at all: its turns are skipped and the game says the seat has no machine.

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
for the other side's answer, since `end_turn` is refused until the negotiation is settled. Every
answer of `wait_for_turn` lists the chats the seat is in, each with its deadline (`chats`: whose move,
`deadline` in epoch seconds, `seconds_left`): an MCP seat is a model seat, so a chat it leaves
unanswered past the game's time is declined for it ([Chats](#chats)).

Any seat's controller can be switched mid-game from **Join / Seats**, including handing a human
seat to a model or the reverse.

---

## Chats

A negotiation is a chat: strictly turn-taking, each entry with a message, at most the game's message
cap (30 by default). Whoever's move it is answers in it, on their turn or not, and the other side waits.
The server keeps the time, by the side a chat waits on, from two lobby settings
([Playing](PLAYING.md#chat-settings)):

| A chat waits on | For (default) | Then |
|---|---|---|
| A model (an LLM or MCP seat) | `chat_model_reply_seconds` (10 minutes), its time waiting for its machine included | the seat declines, "(no reply in time)" |
| A person | `chat_person_reply_seconds` (5 minutes) | the chat closes as expired, "(no reply in time)", whoever opened it |
| A bot | no limit | the bot answers at once |

The clock stands still while the game is paused, starts again whenever the chat moves, and starts
afresh for every open chat when a game is loaded or the server restarts. In a benchmark game, a chat
whose answer waits behind another benchmark on its machine is not timed until the answer starts.

**How a model answers.** A model seat answers each chat in a conversation of its own, kept for as long
as the chat is open: a short, fixed system prompt about chats with the seat's persona, the chat's
history and the proposal from its side the first time, and after that only what changed (the other
side's new message, the proposal as it stands, the minutes left). It has a small set of tools (look up
diplomacy, the empire, the players and the victories, notes, a message, and `respond_negotiation`), and
up to `max_negotiation_calls` steps (20) per answer, on the seat's own machine. A seat answers one chat at
a time, in the order they came. An answer that fails outright (the model could not be reached, or
used its steps without answering) is declined, "(no response)", so the other side is not kept waiting;
a slow one is the chat clock's. Pausing the game, quiet hours, a seat change or a server restart in
the middle of an answer never declines the chat: the seat answers it when it comes back. A game
loaded from a save comes back paused, and its models answer what waits on them when it resumes (a
game opened only to be looked at, such as a probe case's end state, answers nothing). A chat that
comes while the game is paused because of the model's machine (its quiet hours, a disconnect, a spent
budget) waits for the resume; during a pause of yours, or while the game makes way for other work, the
model still answers.

**On its own turn.** When the model sends a message in a chat, the tool waits a minute for the answer
(`negotiation_wait_seconds`, set per seat) and shows it if it comes. An answer that comes later is put
in front of the model before its next step, worded by where the chat stands: "answered negotiation #n:
...; the proposal from your side: ... Answer with respond_negotiation", or, for a chat that was
accepted, declined, withdrawn or that expired, a note that asks for no answer. `end_turn` waits for
every chat the seat opened to be settled, as the game's clock allows.

What the stats screen shows for each seat's chats: how many answers it gave and how each ended
(answered, declined late by the clock, a fallback, closed before it answered), with the model's time
and the time it waited for its machine apart.

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

A seat on a member's machine whose grant has a **hard budget** stops when its next request would pass
the budget: the turn ends (`budget`) and the whole game pauses with the budget's reason, resuming by
itself when the budget's period begins again, or when you press Resume after its owner raised it
([Accounts](server/ACCOUNTS.md#budgets)). The seat is held to the budget its grant has at each
request, so a budget the owner sets while the game runs applies from the seat's next request; a game
paused for its budget when the server restarts comes back running and pauses again at once if the
budget is still spent.

---

## What gets recorded

Per seat, per turn: average, median and maximum turn time; model steps; seconds per model call;
tool calls, successful actions and queries; errors; identical repeated calls; repeats the guard
rail refused; malformed calls that were repaired; stall nudges; input, output and reasoning tokens;
output tokens per second; and how the turn ended.

**📊 AI stats** shows all of it live, with a per-tool table, the most repeated calls, the most
common errors and recent turns. **Download per-turn CSV** gets you the raw numbers.

Every game with an AI seat is also written to the usage ledger, so it can be costed later:
[REPORTS.md](REPORTS.md); each model step and the host's CPU it used is in the compute ledger, as given by the
machine's owner and used by the game's ([Accounts](server/ACCOUNTS.md#the-compute-ledger-and-the-ratio)).
