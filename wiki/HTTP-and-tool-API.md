# HTTP and tool API

Everything a player can do is a **tool**. Tools are registered once, in the engine's tool registry
(`crates/citar-engine/src/api/tools/`), and reach three interfaces automatically: the browser client, MCP clients, and the LLM adapter.
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
| `GET /api/games/{id}/wait` | Long-poll until it is your turn or a negotiation needs an answer. Every answer while the game plays lists the chats your seat is in (`chats`: `negotiation_id`, `with`, `waiting_on` `you` or `them`, `deadline` in epoch seconds, `seconds_left`, `paused`) |
| `GET /api/games/{id}/negotiations/{nid}` | One chat as a side of it sees it (its history, the proposal from that side, `your_move`), with where its clock stands (`clock`: `waiting_on`, `kind` of side, `deadline`, `left_s`, `paused`). A seat token reads the chats its seat is in; anyone who may watch the game reads any while no person plays (or it is over), from `as_player`'s side when that civilization is in it, else the side that opened it. 404 for anyone else, and for a chat the game does not have |
| `GET /api/games/{id}/view` | The game as this seat sees it, as JSON. With no seat token, a spectator's view for an account (or a `link` key) the game's visibility lets see it, while no person is playing; `can_manage` says whether the caller may pause, slow down or save it. Every answer has an `ETag` (and `Cache-Control: private, no-cache`); send it back as `If-None-Match` and an unchanged view is `304 Not Modified`, with nothing built or sent. Its `diplomacy` lists the civilizations met without what could be traded with each (the next route) |
| `GET /api/games/{id}/trade-options/{other}` | What this seat and civilization `other` could trade, as the deal builder suggests it: `{"player", "with", "trade_options": {"their_gold", "they_could_give", "you_could_give", "agreements_possible_now", "research_agreement_cost_each"}}`, each side's luxuries, strategic resources and technologies. A seat token reads its own seat's; a spectator of a game no person plays (or that is over) reads any civilization's through `as_player`. 404 unless `other` is a living major civilization that side has met, and for anyone else. The `get_diplomacy` tool still lists them for every civilization met |
| `GET /api/games` | Games visible to the caller |
| `GET /api/rules` | The whole ruleset |
| `GET /api/games/{id}/metrics` | Per-seat, per-turn metrics |
| `GET /api/games/{id}/metrics.csv` | The same, as CSV |
| `GET /api/games/{id}/replay` | Every turn's state, for the recap: once the game is over, or live for whoever may watch an AI-only game (its spectator token, or an account or key that may see it). `format` is `delta`: `frames` are the turns as the save's journal keeps them (`{turn, keyframe, bytes}`, the bytes in base64: a keyframe now and then, deltas between), which `citar/web/js/replaydecode.js` decodes; [ARCHITECTURE.md](Architecture#the-recaps-frames) describes them |
| `WS /ws/games/{id}` | Live updates: `?token=` a seat or spectator token, or none for the account signed in (its cookie), and `&k=` a `link` game's key. Besides the game's events, a chat's `negotiation_status` (below) |
| `GET /api/models/scores`, `GET /api/metrics/compare` | Per-model scores and metrics. A member's are made from their own games and the games other members pool, a model's pooled part only once enough accounts are behind it (`pooled_withheld`, or a row with `withheld` and the reason); other members' games are counted (`pooled_games`), never named. `scores` also says what the member's scope holds (`scope`). An administrator's cover every game |
| `GET /r/{slug}` | A report's page for people with no account: a `link` report by its key, a `global` one by its id or key; 404 for anything else |

A tool call returns the tool's result, or an error describing what the rules refused and why. Rule
refusals are 400 with a readable message — `"That tile is not adjacent"` — because the message is
read by a model as often as by a person.

Full schemas: `GET /api/tools`, or `/docs` for the generated OpenAPI browser.

### A chat's status on the socket

`{"type": "negotiation_status", "negotiation": 4, "players": [0, 2], "waiting_on": 2, "kind": "model",
"state": "queued", "position": 1, "eta_s": 9.9, "deadline": 1791840929.6, "left_s": 583.0, "paused": false,
"entries": 1}`

- `state`: `waiting` (the chat waits on `waiting_on`, until `deadline`, in epoch seconds, `left_s` from now; no
  deadline while the game is paused or for a bot, which is never timed), `queued` (the answering model's request
  waits for its machine: `position` in its line and `eta_s`), `thinking` (the model is generating its answer), or
  `closed` (with the chat's `status`: `accepted`, `rejected` or `expired`).
- `kind` is the side it waits on: `model` (an LLM or MCP seat), `person`, `bot` (or `hybrid`).
- `entries` (every state but `closed`) is how many entries the chat had when this was said. A chat's
  history only grows, so a client that has read more entries than that knows the status came late,
  and one that has read fewer knows to read the chat (`GET /api/games/{id}/negotiations/{nid}`).
- It goes to the sockets of the two seats in `players` and to spectators while the god view is allowed (no person
  plays, or the game is over); to nobody else.

### Owning and sharing a game

These take a signed-in account (a seat token is not one); see
[the accounts design](Design-accounts-and-sharing#3-authorization) for who may do what.

| Method and path | |
|---|---|
| `POST /api/games` | Create a game: `{"name", "config", "seats", "visibility", "data_sharing"}`. Counts against your game limit. A model seat names a machine (`{"llm": {"server_id", "model", ...}}`); a member's seat that describes one (a base URL, a key, a key variable) is refused with 400 |
| `POST /api/games/{id}/seats/{player}` | Change a seat's type, model or name. A new model block, or a seat made an LLM seat, passes the same rule; a machine you may not use is 403, and the seat stays as it was. A new name alone checks nothing |
| `DELETE /api/games/{id}` | Close the game (the lobby's End game): it stops, its saves stay yours, and it no longer counts against your limit |
| `GET /api/games/{id}/sharing` | Its visibility (`private`, `allowlist`, `link`, `server`, `global`), its share link, who it is shared with, and its last unpublish (`at`, `reason`, and `by`: "you", "its owner", "an administrator", "a moderator" or "someone who manages it", never a handle) |
| `PUT /api/games/{id}/sharing` | `{"visibility", "data_sharing", "rotate_slug"}`; `global` needs the `publish_global` permission |
| `POST /api/games/{id}/sharing/users`, `DELETE .../sharing/users/{handle}` | Share with one account (`view`, `play` or `manage`), or stop |
| `POST /api/games/{id}/unpublish` | `{"reason"}`: make it private and kill its links, for a moderator on a published game or for whoever manages it. The owner sees the reason |
| `POST /api/scenarios/{id}/launch` | Start a game from a scenario you may see: your game, under the same seat rule and game limit |

### Owned items

Benchmark suites and runs, probes and probe runs, lab experiments, scenarios, maps, bot profiles and
reports each have an owner and a visibility, like a game ([the rules](Design-accounts-and-sharing#owned-items)).
Their own routes (`/api/benchmarks/...`, `/api/probes/...`, `/api/probe-runs/...`, `/api/maps/...`,
`/api/scenarios/...`, `/api/scenario-editor/...`, `/api/bots/...`, `/api/lab/...`, `/api/reports/...`)
answer 404 for an item the caller may not see and 403 for one they may see but not change; listings
carry `can_view`, `can_manage` and `can_moderate` per item. A `link` item opens with its key as `?k=`
(or the `x-citar-share-key` header), and a listing asked with a key shows the one item it opens to
whoever holds it. A report's sharing block also gives `public_url`, its `/r/` page, when it is shared
by `link` or published. (`/api/shared/reports...`, the old report sharing routes, are gone: a report
is shared through the routes below like every other item.) What a report gathers follows its owner
([Reports](Servers-costs-and-reports#what-a-report-contains)); its specification takes `"pooling": "own" | "both" |
"pooled"`. `GET /api/reports/{id}` gives a report's managers its whole metadata and specification;
anyone else it reaches gets neither its traceback nor its owner, and a specification cut to what they
could have chosen (the scope's items and machines they may see; of the narrative only `enabled`).
A body of the wrong shape (an item named by anything but its id, a report specification whose parts
are not objects or lists) is a 400. A report's narrative that names a pooled machine the caller is not
admitted to (a grant for `pooled` prompts, P3.3.7) is a 403, as a run's machine is. A probe run's subject
(`POST /api/probes/{id}/run`) is admitted for what its chosen cases are: `probe` for a turn case,
`pooled` (and `partial_llm`) for an offer or message case; one it is not admitted for is a 403 naming the
purpose. A report's record carries `narrative_written` and `narrative_note` (who wrote its analysis, or
why it was skipped or failed); `DELETE /api/reports/{id}` deletes a report being built too (its
narrative's prompt leaves the machine's line). What queued work waits for (a benchmark job's `waiting` in
`GET /api/benchmarks/runs`, a probe run's in `GET /api/probe-runs/{id}`, a report's `waiting` and
`progress`, a Queue page entry's `waiting`) names only the games and runs the caller may see; the rest
are "someone's game", "someone's benchmark run" and so on.
An item is addressed here by its kind and its id on disk (`ref`), as its own routes name it:

| Method and path | |
|---|---|
| `GET /api/items/{kind}/{ref}/sharing` | Its owner, visibility, share link, who it is shared with and its last unpublish (as a game's) |
| `PUT /api/items/{kind}/{ref}/sharing` | `{"visibility"}`; `global` needs `publish_global` |
| `POST /api/items/{kind}/{ref}/sharing/users`, `DELETE .../sharing/users/{handle}` | Share with one account (`view`, `play` or `manage`), or stop |
| `POST /api/items/{kind}/{ref}/sharing/rotate` | A new share link; every copy of the old one stops working |
| `POST /api/items/{kind}/{ref}/unpublish` | `{"reason"}`: make it private and kill its links, for a moderator on a published item or for whoever manages it |
| `GET /api/queue` | The Queue page: the machines the caller may see (`others_busy` counts the rest that are in use, naming none), each with `capacity`, `in_flight`, `busy_with`, its `leases` (`kind`, `label`, `member`, `state`: `active`, `lending` or `held`), its jobs (`waiting`, `running`) and its pooled `prompts` (`running` and `waiting`: `class`, `label`, `member`, `model`, `priority`, `position`, `waited_s`, `eta_s`, `starts_within_s`, `can_manage` with the work's `ref`, `can_cancel` with the prompt's `id`); work the caller may not see is "someone's <kind>" with no member |
| `POST /api/queue/priority` | `{"kind", "id", "priority"}`: move your own work (a benchmark run, a probe run, a report, a lobby game, a lab experiment) up or down the queue; its pooled prompts follow it |
| `POST /api/queue/tickets/{id}/cancel` | Cancel a pooled prompt: for its work's owner (for a prompt of no work, its member), the machine's owner and administrators; 403 to anyone else who may see it, 404 to the rest. A running prompt's request drains on a helper that keeps generating it |

`kind` is one of `benchmark_suite`, `benchmark_run`, `probe`, `probe_run`, `lab_experiment`,
`scenario`, `map`, `bot_profile` and `report`. These take a signed-in account.

### Profiles, the directory and the admin users page

An account's profile fields each show to the account and administrators (`private`), members (`server`) or
anyone (`global`), and the profile page's own field gates the rest
([the rules](Design-accounts-and-sharing#profiles-and-the-directory)). Wherever an account appears in any answer
(`owner`, `shared_with`, a grant's `subject`), it is `{"id", "handle", "display_name", "avatar"}` as the caller
may see it: a hidden display name is the handle, a hidden picture `null`.

| Method and path | |
|---|---|
| `GET /api/auth/me` | The caller's own account, its profile included (`bio`, `links`, `field_visibility`, `listed`, `avatar`, `avatar_uploaded`, and `avatar_url`, the sign-in provider's picture) |
| `PUT /api/auth/me` | Also `{"bio", "links": [{"label", "url"}], "field_visibility": {field: level}, "listed"}`: every part checked before any changes; a bio over 1,000 characters, a fourth link, a link that is not `http` or `https`, an unknown field or level is 400, a `global` field on probation 403 |
| `POST /api/auth/me/avatar` | A multipart form with one file: PNG, JPEG or WebP, at most 256 KB and 1024x1024, checked by its header (400 with the rule otherwise, or for a body that is not a well-formed form; 413 when larger). Stored without its metadata: Exif, XMP, text chunks and comments are left out, the pixels untouched |
| `DELETE /api/auth/me/avatar` | Remove the uploaded picture |
| `GET /api/users` | Members only: the directory, listed accounts in good standing, newest first; `?q=` (a handle, or a display name the caller may see), `?offset=` (pages of 50, `next`; 0 to 1,000,000, 422 otherwise) |
| `GET /api/users/{handle}` | A profile as the caller may see it (`fields` lists what shows; `bio`, `links` and `items` when they do); `?as=member` or `?as=visitor` narrows the view, never widens it. 404 when the page does not show to the caller, alike for an account that does not exist. Open to visitors |
| `GET /api/users/{handle}/avatar` | The uploaded picture, to whoever its field shows to; 404 otherwise. Its address in `avatar` carries the file's hash |
| `GET /api/admin/users` | Administrators: every account, newest first; `?q=` (handle, name or email), `?role=`, `?status=`, `?offset=` (pages of 50; 0 to 1,000,000, 422 otherwise) |
| `GET /api/admin/users/{id}` | One account (its capabilities resolved, `capabilities`, beside its overrides, `caps`), the role's defaults and its audit rows |
| `PATCH /api/admin/users/{id}` | Any of `role`, `status` (with `reason` for a suspension), `caps` (each capability `true`, `false` or `null`, null removing the override), `invite_quota` (0 to 1,000; 0 is none), `max_concurrent_games` (1 to 100), `max_seats_per_game` (0 to 64; 0 is the ruleset's most), `trust_tier`, `listed`: all checked before any changes, one audit row naming the fields; 409 for the last active administrator demoting or suspending themselves |
| `GET /api/admin/audit` | The audit log, newest first: `?actor=` (handle or id, or `admin_cli` for the command line's changes), `?action=` (or a prefix ending in `*`), `?object_type=`, `?object_id=`, `?before=` (an entry's id, from `next_before`; 422 past a 64-bit integer), `?limit=` (at most 200) |

The admin routes answer 404 to any account that is not an administrator, and 401 to a visitor.

### The compute ledger

What each account's machines gave other members and what its work used, in credits
([the rules](Design-accounts-and-sharing#the-compute-ledger-and-the-ratio)). A side's figures are `{"credits", "gpu_s",
"cpu_s", "tokens_in", "tokens_out", "events", "by_kind": {"gpu"|"cpu"|"api": {"seconds", "credits", "events"}}}`, and
the ratio `{"value", "text"}` (`value` null when nothing was used: `text` "only given" or "nothing yet"). `range` is
`24h`, `7d`, `30d` (the default) or `all`; another is a 400.

| Method and path | |
|---|---|
| `GET /api/ledger/me?range=` | The caller's own: `given`, `used`, `ratio`, `by_machine` (`{"id", "name", "given", "used", "gpu_s", "cpu_s", "events"}`; a machine the caller may not see now has no `id` and is named only as whose it is), `by_activity` (by kind of work: `game`, `benchmark`, `probe`, `report`, `lab`, `test`) and `own_use` (the caller's work on the caller's own machines, counted on neither side). 401 to a visitor |
| `GET /api/users/{handle}/ratio?range=` | `{"handle", "range", "since", "given", "used", "ratio"}` when the account's `ratio` profile field shows to the caller (private by default: the account and administrators); 404 otherwise, alike for no such account. Open to visitors for a `global` ratio. A profile (`GET /api/users/{handle}`) carries the same as `ratio` (30 days) under the same rule, and `GET /api/admin/users` each account's 30 days as `compute_30d` (`{"given", "used", "ratio"}`) |
| `GET /api/admin/hardware-classes` | Administrators: every class (`{"id", "kind", "name", "rate", "match", "priority", "is_default", "updated_at", "updated_by", "machines"}`), each with the machines it prices now |
| `POST /api/admin/hardware-classes` | A class: `{"kind": "gpu"|"cpu"|"api", "name", "rate", "match": {"gpu_name", "vram_gb_min", "cpu_name", "cores_min"}, "priority", "is_default"}` (a regular expression that does not compile, an unknown key, a rate outside 0 to 10⁹, a priority outside -1000 to 1000: 400). One marked the default replaces its kind's |
| `PUT /api/admin/hardware-classes/{id}` | Change any field but its kind (400). A kind always keeps a default: unmarking the default is a 409. The new rate prices the events written after it ([reprice](Configuration#credits-and-hardware-classes) a period) |
| `DELETE /api/admin/hardware-classes/{id}` | Remove a class that is not its kind's default (409 for the default) |

The class routes answer 404 to any signed-in account that is not an administrator and 401 to a visitor; every change
is an audit row, and every machine not pinned is classified again.

### Machines

Every machine is a row (the host, online APIs and LAN machines the server dials itself, `reach: "direct"`, and
machines behind a CITAR helper, `reach: "worker"`); who may do what is in
[Workers](Workers#who-may-do-what-to-a-machine). Another member's machine is a 404, a visitor gets 401.

| Method and path | |
|---|---|
| `GET /api/servers` | The machines the caller manages (every one for an administrator), each with `reach`, `enabled`, `is_host`, `max_concurrent` (its slots) and its configuration; the electricity plans (a member sees only the names of those an administrator put in the member's machines, and no issue asking for a plan) and the key backends the caller may choose |
| `GET /api/servers/new` | A blank machine to start from: `reach` is `worker` for a member unless the provider is `dryrun`. Needs `register_servers` |
| `POST /api/servers` | Create a machine, or save one the caller manages (by its `id`). A machine the server dials (`reach: "direct"`, the default for a new one) is an administrator's unless its provider is `dryrun`, and so are the `env` key backend, `is_host` and an electricity plan in its costs: 403 with the reason. A member's new machine gets an id from the server (an `id` in the body names only a machine to save). A body of the wrong shape (a list where an object belongs) is a 400, here and on the pool's routes |
| `DELETE /api/servers/{id}` | Remove it (its tokens, and the key its row stores) and disconnect its helper |
| `POST /api/servers/{id}/hardware`, `GET .../rates` | Import a hardware file; what an hour on it costs (for a member, without the price of the site's electricity) |
| `POST .../detect`, `GET .../state`, `POST .../load`, `.../unload`, `GET .../loading`, `POST .../loaded-config`, `.../estimate` | Model detection and LM Studio control: on a machine the server dials, or the host; for a member only a dry run. 404 for a helper machine, with nothing dialled or started |
| `POST /api/servers/{id}/collect`, `GET /api/lmstudio/devices` | Administrators, on the host: they read the computer CITAR runs on |
| `POST`, `DELETE /api/servers/{id}/key` | Store or remove the API key of a machine the server dials; the `env` backend is an administrator's (403). 404 on a helper machine: the server holds no key of one |
| `PUT /api/servers-settings`, `POST /api/keys/unlock`, `POST /api/keys/lock`, `POST /api/electricity-plans`, `DELETE /api/electricity-plans/{id}` | Administrators (404 to anyone else signed in) |
| `GET /api/servers/restricted` | The machines in their quiet hours that the caller may see; so is `restricted` in `GET /api/benchmarks/status` and in `GET /api/status`'s `benchmarks` |
| `GET /api/pool/availability` | What the caller could run now, and when the rest opens: only machines the caller may see, usable or not |
| `POST /api/llm/models` | List the models at a base URL ("Detect models"): an administrator's, the `dryrun` provider excepted (403 before anything is dialled) |
| `PUT /api/pool/servers/{id}` | `{"enabled": false}` disconnects its helper at once; switching to `reach: "direct"` follows the rule above |
| `DELETE /api/pool/servers/{id}/workers/tokens/{token}` | Revoke a worker token: a helper connected with it is disconnected at once, and refused if it says hello again |
| `GET /api/pool/groups` | The server groups the caller may see, with their machines; a group's manager gets every grant, anyone else their own terms. Its grants are what share it: no route takes a group's key, so a `link` group admits nobody by it yet and the listing hands out no share link |
| `POST /api/pool/servers/{id}/test?model=&wait=` | The Servers page's test: one short completion through the machine's helper, as a `test` prompt of the [prompt scheduler](Workers#when-a-machine-is-busy) (it waits for a slot, on a model the helper has loaded unless its owner allows swaps). `wait` (1 to 120 seconds, default 120) is how long the caller waits for the reply: then the test is cancelled (`"cancelled": true`, `code` `cancelled`; `benchmark` when a benchmark game took the machine meanwhile), and the request the helper is still generating keeps its slot until it finishes. A reply carries `seconds`, the generation time `gen_s` and the time it waited for the machine, `queue_s`; a refusal names its `stage` (`admission`, `connection`, `model`, `queue`, `completion`), its reason and, from the queue, its `code` (`not_loaded`, `quiet_hours`, `benchmark`, ...). A caller has one test of a machine waiting at a time (another answers `code` `duplicate`) and two in all (`too_many`), at most 20 in ten minutes (429 past that, with `Retry-After`). The test is a compute-ledger activity of its own (`test:<id>`): used by the caller, given by the machine's owner, counted against the grant's budget; no cost report lists it. Anyone who may use the machine |
| `GET /api/status` | A check-in for the phone site. An administrator's also lists the scheduler's `machines`: each one's slots, what is in flight and draining, what waits, its leases (each with its `kind`, `label`, the `game` session holding it and the work it serves as `ref`, `[kind, id]`) and pooled prompts, and its counters (`busy_refusals`, `late_frames`, `drain_timeouts`, `drains_dropped`, ...); a member's and a moderator's do not |

The pool's routes that change something (a machine, its costs or quiet hours, a group, a grant, a worker token)
commit before they answer, so a page that reads the change back at once (the Servers page redrawing a card after its
dialog saves) sees it.

`POST /api/pool/import-registry` is gone: a pre-0.1.6 `config/servers.json` is imported once by the server itself
([Configuration](Configuration#the-machines-in-the-database-not-a-file)). The first-run setup wizard's
`POST /api/setup/scan` and `POST /api/setup/apply` (they read this computer and register machines the server dials)
are an administrator's, and so are `POST /api/setup/dismiss` and `POST /api/setup/reopen` (a site-wide setting); a
member's `GET /api/setup/state` has no `paths`.

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
answers in its own time — bots at once, models and humans within the game's chat times.
`respond_negotiation` accepts, rejects, counters or replies, and every entry carries a message (its schema
requires `message`). Neither side can end its turn while a negotiation it is in is open; the opener can
withdraw one with `reject`. A negotiation closes by itself at its message cap (the game's `diplomacy.max_chat_messages`, else its
ruleset's: 30 in the standard rules), and when
the side it waits on runs out of time: a model seat (an LLM or MCP seat) past `chat_model_reply_seconds`
(10 minutes by default) is taken to decline, "(no reply in time)", and a chat waiting on a person past
`chat_person_reply_seconds` (5 minutes) closes as expired. This is the part of the game most worth
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
5. Repeat until `wait` says `game_over` or `eliminated`, or `crashed`: the engine stopped the game after an
   internal error, and it takes no more moves.

Worth knowing:

- **Answer negotiations.** `wait` returns for a negotiation as well as for a turn. An agent that
  ignores one is taken to decline it once the chat's time runs out (`chats`: `seconds_left`). On your
  own turn, after you open one, `wait` waits for the other side's answer (`negotiation_update`, or
  `waiting_for_reply` at the timeout): `end_turn` is refused until the negotiation is settled or
  withdrawn, and the game's chat clock settles it when the other side's time runs out.
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
`wait_for_turn`, which blocks rather than polling. See [AI_PLAYERS.md](AI-players#mcp-clients).

---

## Stability

Before 1.0, tool names and arguments may change between minor versions; when they do, the release
notes say so. `GET /api/tools` is always the truth for the version you are running, and an agent
that reads the schema rather than hard-coding it will survive most changes.


---

*This page is generated from [`docs/API.md`](https://github.com/jprodgers/CITAR/blob/main/docs/API.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
