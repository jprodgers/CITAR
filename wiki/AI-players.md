# AI players

Five kinds of player can hold a seat. They all use the same interface, which is the point: a
scripted bot, a local 4B model, Claude through the API and a human in the browser are issuing the
same orders through the same rules.

| Seat | Who plays | Setup |
|---|---|---|
| **Human** | You, in the browser | Nothing |
| **Scripted bot** | The built-in rule-based AI | Pick an aggression level |
| **LLM** | A model the server drives | Pick a machine, model and load profile |
| **Hybrid** | The bot plays the turns; a model decides the diplomacy you hand it | A bot, a machine and model, and the kinds of diplomacy that are the model's ([below](#hybrid-seats)) |
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
[API.md](HTTP-and-tool-API).

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
  GPU. See the [FAQ](Questions#how-strong-are-the-models).

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
([Playing](Playing-in-the-browser#chat-settings)):

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
a time, in the order they came. An answer that fails outright (the model could not be reached, used
its steps without answering, or answered three times running without calling a tool, which a model
that writes its answer as text keeps doing) is declined, "(no response)", so the other side is not
kept waiting;
a slow one is the chat clock's. Pausing the game, quiet hours, a seat change or a server restart in
the middle of an answer never declines the chat: the seat answers it when it comes back. (A restart
brings a game back from its last save, which is taken as each round begins and at a seat change, so
a chat opened later in the round is gone with the rest of that round's moves.) A game
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

## Hybrid seats

A hybrid seat is a scripted bot that plays the turns (cities, research, units, every war's fighting)
with a language model that decides the kinds of diplomacy the seat hands it. The lobby, the Join/Seats
dialog and a scenario's seats set one up ([Playing](Playing-in-the-browser#hybrid-seats)). Its seat has the bot's
settings, the model's machine (as an LLM seat names one), and `hybrid`:

```json
{"type": "hybrid", "handicap": "ai", "difficulty": "Prince",
 "bot": {"profile": "standard"},
 "llm": {"server_id": "...", "model": "..."},
 "hybrid": {"owners": "all_but_war", "advisor": true, "prompt_every": 10}}
```

**The ten kinds of diplomacy**, each the bot's or the model's (`owners`: a map such as
`{"trades": "llm", "chat": "llm"}`, every kind it does not name the bot's, or a preset):

| Kind | The model's tools | What the bot no longer does |
|---|---|---|
| `trades` | `open_negotiation`, `respond_negotiation` with gold, gold per turn, resources, techs, maps, cities | trade luxuries, answer trades |
| `agreements` | the same, with embassies, open borders, friendship, research agreements, defensive pacts | offer agreements |
| `peace` | the same, with a peace treaty | offer or accept peace |
| `war` | `declare_war` | prepare and declare wars (it still fights every war) |
| `denounce` | `denounce` | (the bot never denounces) |
| `un` | `un_vote` | the engine's vote for the seat: a vote the model does not cast is an abstention |
| `city_states` | `get_city_states`, `city_state_action` | gifts of gold to city-states |
| `espionage` | `get_espionage`, `move_spy`, `stage_coup` | place spies |
| `captured_cities` | `get_cities`, `city_status` | the engine's annexation: a captured city stays a puppet until the model decides |
| `chat` | `send_message`, and the chats that are only talk | canned replies |

The presets: **trade only** (trades and chat), **all but war** (every kind but war) and
**everything**. A seat must give its model at least one kind: one with none is a bot seat.

The model always has `get_diplomacy`, `get_players`, `get_empire`, `get_victory_status`, its
notebook (`read_notes`, `write_notes`) and `log_thought`. Any other tool it calls is refused, "That is
the bot's to decide.", and counted.

**Difficulty and handicap** are the keys every seat has: `difficulty` is its level, and `handicap`
whose numbers it plays with: `ai` (the default: "AI bonuses, like the bot") or `human` ("a person's
values, like a model").

**When the model is asked.** After the bot has played its turn, the seat's diplomat is called when
something happened since its last call: first contact with a civilization, a war declared on it,
peace made, a denunciation of it, a deal of its that expired, was cut or ends next turn, a United
Nations vote open (when the vote is the model's), a city it captured (when captured cities are), a
city-state's news about it (when city-states are), a spy's (when espionage is), its score or its
strength against a civilization it has met changed by more than a quarter, or `prompt_every` turns
(1 to 50, 10 by default) since its last call. At most once a turn. Each call is one pooled prompt
(a `diplomat` ticket on the model's machine, see [Workers](Workers)): it gets 30 seconds
to start (or it is skipped and the bot's choices stand), three minutes once started
(`phase_seconds`), and at most eight model calls. Its message gives what called it, the diplomatic
situation, the empire at a glance and the notebook; with the **advisor** on (the default, never in a
benchmark), the bot's view too: what each deal on the table is worth to the seat in gold, its war
readiness, its spare luxuries and what it would ask for. The system prompt is the same text for every
call of the seat, so a provider can cache it, and it names both chat timeouts. Pausing the game holds
the diplomat before its next model call or action, as it holds an LLM seat's turn; its prompt gives
the machine back meanwhile, and the time paused does not count against `phase_seconds`.

**Mixed deals.** A deal the model offers, counters with or accepts that includes anything the bot
decides must be one the bot would accept as a whole: the bot values the whole deal from the seat's
side, and below zero the call is refused with the reason ("The bot decides agreements and values this
deal at -120 gold for you, so it will not agree. Leave those items out, or make the deal better for
Rome."). What lies wholly in the model's own kinds is the model's to give.

**Chats.** A chat that waits on a hybrid seat goes to its bot first, which answers what it owns and
leaves the rest to the model: a chat whose proposal touches a kind the model decides, or that is only
talk when chat is the model's. The model answers it in a conversation of its own, as an LLM seat
does, with the diplomat's prompt and the chat tools of its kinds, on a `chat` ticket that must start
before the chat's time runs out. When that ticket is refused (a benchmark holds the machine, its quiet
hours, a budget), does not start in time, or the model fails, the **fallback bot** (the same bot with
every kind its own) answers at once, and the stats screen counts a fallback with its cause; when the
chat's time runs out, the chat clock has the fallback bot answer it rather than decline. While the
seat's diplomat is called, the seat answers no chat on the side: one that comes to it then is told to
the diplomat, which answers it.

A hybrid seat's prompts are pooled prompts, never a lease: it does not occupy its machine, it needs a
grant for `pooled` (not `game`) on another member's machine, and quiet hours or a make-way pause
never pause its game: its prompt is refused instead, and its bot answers. Its turns are its bot's, so
it stays out of the model ranking; its own is the Models page's **Diplomacy** tab, from hybrid
benchmark suites, each job against a bot-only control game ([Benchmarks](Benchmarks#hybrid-suites)). The game's metrics (`GET /api/games/{id}/metrics`, see
[the API](HTTP-and-tool-API)) carry its diplomat's calls: each call's row (`diplomat`), and in the seat's
summary their count by outcome (done, timeout, skipped, refused, failed, cancelled, with the cause's
code), their generation and queue time, the tools they called and the calls the session refused. The
stats screen (**📊 Stats**) shows them for the seat, and the game screen's player list says what the seat
is doing: "bot playing", "diplomat thinking", "queued #n" or "diplomat paused".

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
([Accounts](Design-accounts-and-sharing#budgets)). The seat is held to the budget its grant has at each
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
[REPORTS.md](Servers-costs-and-reports); each model step and the host's CPU it used is in the compute ledger, as given by the
machine's owner and used by the game's ([Accounts](Design-accounts-and-sharing#the-compute-ledger-and-the-ratio)).


---

*This page is generated from [`docs/AI_PLAYERS.md`](https://github.com/jprodgers/CITAR/blob/main/docs/AI_PLAYERS.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
