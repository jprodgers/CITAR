# CITAR 0.1.6, Phase 3: the features (design)

**Status.** Design, 2026-10-11, for Phase 3 of `ops/plan-0.1.6.md` §7, written against `v0.1.6` at `d93345a` (Phase 2 complete: the Rust engine, the Rust bot, the bindings and saves v2 are the only engine; the Python engine is deleted and tagged `python-engine-0.1.6`). Sections are numbered P3.x so they cannot be confused with DESIGN.md's Phase 1 (§1-§12) and Phase 2 (P2.x). Appendix D is the work breakdown: fifteen packages, run strictly one after another, one agent at a time (owner's instruction, 2026-10-11). Each package appends an "As built in 3-xx" note to the section it implements, as Phase 2's packages did, and records its wall-clock time so the owner can compare single-agent runs with Phase 2's two lanes.

This file lives in `design/`, outside `docs/`, so mkdocs and the wiki generator never see it. It names code paths in backticks rather than linking them, so `scripts/check_links.py` (which scans `docs/`, `packaging/`, `crates/` and the root documents) needs no change.

**Sources:**
- `ops/plan-0.1.6.md` §1 (decisions R1-G and the defaults under them), §6.1-6.6 (feature designs), §7 Phase 3; §6.7 for the hooks Phase 4 needs.
- `ops/review-0.1.6/{auth-db-sharing, pool-queue-workers, agents-llm, web-game-client, web-site-pages, engine-diplomacy-interface, jobs-bench-lab-reports, server-runtime}.json`. They were written against `main` at `8290e01`, before Phase 0; every finding used here was checked again against the tree at `d93345a`, and the ones that no longer hold are listed in P3.1.5.
- `ops/phase0-spec.md` Stage A (the engine and bot hooks the features need), `ops/p2-notes.md` (2-11, 2-12 and 2-13 sections), `crates/citar-engine/DESIGN.md` P2.3.8, P2.6, P2.7 and the as-built notes of 2-05, 2-09, 2-11 and 2-13.
- The code each feature touches (P3.0 cites it line by line).

---

## P3.0 Facts the design leans on (checked against the tree at `d93345a`)

**The engine and the bot already carry the hooks** (Phase 0 Stage A, ported in Phases 1-2):
- **Negotiations are the chat model.** Every history entry has a `seq` and a message; `open` requires a message and refuses a second open chat between the same pair and more than the ruleset's opens per pair per turn (`crates/citar-engine/src/game/diplomacy/negotiation.rs:279-326`). Opening is an own-turn action (`any_time: false`, `api/tools/registry.rs:899-916`); answering is any-time. `accept/counter/reply/reject` with `decline/withdraw/end` read as `reject` (`negotiation.rs:400-409`); an empty counter is refused (`:556-564`); the message cap (`max_chat_messages`, the game's `config.diplomacy` or the ruleset's 30) closes a chat as `expired` instead of taking the message (`:568-571, 630-645`). The `end_turn` tool refuses while any negotiation the player is in is open, naming the one to answer first (`end_turn_refusal`, `:800-824`); `Game::end_turn` keeps the safety net that expires the initiator's chats (stage E0, `:776-793`). `close_negotiation` is exposed (`EngineGame.close_negotiation`, `citar/_facade_rust.py:874-879`).
- **The model-facing text was rewritten in Phase 0.** The registry no longer promises an immediate reply (`registry.rs:899-940, 1024-1031`) and `turn_progress` already lists the open negotiations and says `end_turn` waits for them (`api/briefing/mod.rs:742-756`). One gap remains: `respond_negotiation`'s schema lists only `negotiation_id` and `action` as required (`registry.rs:928`) although the engine refuses an entry without a message.
- **Seats.** `Controller` has `Hybrid`; `Handicap` (`human|ai`) and `AutoDecisions` (`un_vote`, `conquest`, `free_picks`) are separate, with defaults derived from the controller (`state/players.rs:59-129`). `EngineGame.set_controller(pid, controller, handicap, auto)` (`_facade_rust.py:951-955`) and the lobby's per-seat `handicap`/`auto` (`citar/server/session.py:1489-1492`) reach them.
- **The drive knows hybrid seats.** `Game::drive` stops with `Stop::HybridDiplomat(pid)` once a hybrid seat's driver has played, and the next drive ends the turn (`game/turn/drive.rs:152-155, 324-329`); a driver's `respond` may return `DriverOutcome::Deferred`, which leaves the chat to the host and makes that seat's end of turn wait (`Stop::AwaitingReply`, `:59-75, 449-468`); "the next drive asks the driver again, so a host whose wait has run out has the bot decide by driving with a driver that answers this time" (`:67-73`). `Game::answer(pid, nid, driver)` puts one chat to a driver (`:352-381`).
- **The bot's switches and advice.** `citar_bot::Owners` maps the ten categories (`trades, agreements, peace, war, denounce, un, city_states, espionage, captured_cities, chat`; `game/diplomacy/category.rs`) to `bot|llm` and decides `owns(negotiation)` (`crates/citar-bot/src/owners.rs`). `citar_bot::advice()` returns `{deal_value, war_readiness, spare_luxuries, wants}` (`lib.rs:334`); `citar_bot::evaluate()` values arbitrary terms (`lib.rs:350`) but is **not bound to Python**. The facade has `bot_instance`, `bot_set_diplomacy`, `bot_owns_negotiation`, `EngineGame.drive`, `.answer`, `.bot_advice` (`_facade_rust.py:344-390, 962-1028`).
- **Scenarios** list `hybrid` among their seat types (`api/scenario/editor.rs:24`), but `normalize_seats` keeps only the `llm` and `bot` sub-objects of a seat (`editor.rs:203-213`).
- **The client view** has no explored-region, era-stage or home-continent block (`api/views/client.rs:118-139`); the map keeps a continent per tile (`state/map.rs:41, 58`).

**The server today:**
- `SEAT_TYPES = ("human", "mcp", "llm", "bot")` (`session.py:23`). `BotAgent.play_turn` drives a `hybrid_diplomat` stop straight on, since no session has hybrid seats yet (`citar/agents/bot_agent.py:70-73`), and waits at most `REPLY_WAIT_SECONDS = 90` per turn for other seats' answers (`:25, 65-91`). `LLMAgent` gives each message it sends `negotiation_wait_seconds = 240` (`citar/agents/llm_agent.py:123, 394-467`).
- **Responders:** after every action, `_dispatch_negotiation_interrupts` starts one daemon thread per `(negotiation, entries)` for bot and LLM seats; the `_responding` set is never pruned (`session.py:362-402`); an unanswered chat is rejected with "(no response)" (`:404-417`). `LLMAgent.respond_negotiation` opens a fresh conversation per exchange, with the full system prompt, 15 tools and no deadline (`llm_agent.py:634-691`).
- **Model calls:** a worker's `busy` refusal makes `_step` sleep and retry forever, metering a request each time (`llm_agent.py:204-222, 259-266`); `WorkerConversation` ignores the helper's `Done.duration` and its `close()` does nothing (`citar/agents/providers/worker_provider.py:91-142`); `WorkerHub.submit` refuses at once when the connection's `in_flight >= max_concurrent` (`citar/server/workers.py:212-247`). Report narratives call the SDKs directly, outside `make_conversation`, and poll a busy worker every 5 s for up to 30 minutes (`citar/reports/__init__.py:387-444`).
- **Occupancy is per activity:** `pool.seats.occupied()` counts every live game with a living LLM seat on a machine for the whole game, plus named claims (`citar/pool/seats.py:36-63`); probe runs and reports claim a machine for their whole run (`citar/probes.py:631`, `reports/__init__.py:309`); benchmarks count `occupied` against `max_parallel` (`citar/server/benchmarks.py:776-826`); quiet hours and make-way pauses look only at `seat.type == "llm"` (`session.py:599-704`).
- **Admission** runs only at game creation and tolerates `concurrency` (`pool/seats.py:200-218`); `update_seat` skips it (`citar/server/app.py:569-582`); `PURPOSES = game, benchmark, probe, scenario, lab` (`citar/pool/admission.py:32`); `budgets.reserve/settle/release/refresh_from_ledger` have no caller outside `tests/test_pool.py`.
- **Two registries:** `config/servers.json` (`citar/servers.py`), dialled directly, edited by admin-only routes (`citar/server/admin_api.py:45-293`) and the `#/servers` page (`citar/web/js/servers.js`); and DB `servers` rows reached through helpers (`citar/server/pool_api.py`, `#/pool`, `citar/web/js/pool.js`). Three concurrency numbers: `Server.max_concurrent`, `config.connection.max_parallel`, the helper's `hello.max_concurrent`. A helper's hello overwrites `config.hardware` and replaces the model list (`workers.py:329-361`).
- **The usage ledger** is JSONL with no user (`citar/usage.py:1-17`); `session_servers` charges every LLM seat's machine as held for the whole game (`usage.py:277-295`).
- **The database** has the 16 tables of revision `f064316a6b34` plus `767c824c2fcb` (`citar/db/models.py`, `citar/migrations/versions/`). Visibility is `private|allowlist|link|public` and the string is compared in `models.py:276`, `auth/access.py:21-37, 164-183, 280-293, 310`, `server/app.py:514`, `pool/__init__.py:43`, `server/ownership.py:166`, `server/share_api.py:60-62`. `User.public()` exposes `role` and `status` (`models.py:171-183`). Users have no bio, links or field visibility. `ownership.sync` is never called and the engine's phase is `playing|over`, so a finished game keeps counting against `max_concurrent_games` (`app.py:185-202`, `ownership.py:58-72`). Nothing writes a `reports` row. Benchmark suites and runs, probes, lab experiments, scenarios and maps have no owner, and their routes need only `require_user` (`app.py:785-1424`); any member can change any queue priority (`app.py:1389-1410`); bot profiles and lab experiments are admin-only (`citar/server/bots_api.py`). Model scores read every save whatever its `data_sharing` (`citar/server/scoring.py:87-115`).
- **The web client:** Diplomacy is a modal rebuilt from `get_diplomacy` on every negotiation event, which wipes the textarea and the deal builder (`citar/web/js/panels.js:704-824`, `game.js:248-257, 315-324`); the minimap is a 200x130 canvas of the whole map, not aspect-correct, with the explored tiles drawn where they are (`game.js:58-72, 998-1030`); the nav has no people or profile pages (`nav.js:6-9`).
- **Tests and checks:** 665 Python tests (141 s); `scripts/audit_routes.py --strict` counts 190 routes, 0 unguarded; test.yml's front-end job parses every module as ESM; `scripts/dev_server.py` defaults to port 8799 (the owner's) and `scripts/server_soak.py` shows how a script refuses it.

---

## P3.1 Goals, non-goals and exit

### P3.1.1 What Phase 3 delivers

1. **3A Accounts.** A fresh Alembic baseline; the visibility ladder `private | allowlist | link | server | global`; an owner and a visibility for every job type; profiles with per-field visibility and avatars; a members-only directory; an admin users page with the audit log; the legacy server registry merged into the Servers page.
2. **3C The prompt scheduler.** One scheduler per machine for every provider: full leases (a model playing every turn) and partial tickets (one prompt), priority, FIFO and round-robin across members, loaded-model-only pooled work, cancellation, generation-time metering. Task classes in admission; the Queue page's pooled prompts.
3. **3B The compute ledger.** `compute_events`, credits per hardware class, hourly rollups, given and used per member with self-use excluded, budgets that finally reserve and settle, the ratio on profiles.
4. **3E Trade chat.** Lobby timeouts enforced by the session, status events for the two parties, one model conversation per chat, the late-reply fix; a docked chat window per civilization with the deal builder inside.
5. **3D Hybrid seats.** A bot plays the turns and a model does the diplomacy categories the seat hands it; triggers, a 3-minute diplomat phase, the bot's veto on mixed deals, fallback to the bot; lobby, scenario, benchmark and probe support; the diplomacy leaderboard.
6. **3F The minimap.** It follows explored land only, by era, keeps its size, never shrinks, and is aspect-correct.
7. **3G Machine settings.** A dialog per machine: capabilities (LLM slots, model swaps, CPU limits for Phase 4), auto/manual hardware, models and power with the last scan kept, the hardware class, the trust tier.

### P3.1.2 Owner decisions that bind this phase

From the plan (§1) and the owner's instructions for Phase 3:
- **Hybrid:** every diplomacy category is a per-seat bot/LLM toggle; the model is prompted on events plus every N turns (default 10); difficulty is chosen per seat; an optional advisor, off in benchmarks; a separate diplomacy leaderboard from paired seeds.
- **Pooling:** full and partial task classes; pooled prompts may run between a lobby game's turns, never while a benchmark game holds the machine; pooled prompts only on loaded models unless the machine's owner allows swaps; speed metrics use generation time, never queue wait.
- **Chat:** a docked, non-modal window per civilization that pops up at once; strict turn-taking with a thinking/queued indicator; End Turn waits for a reply with a timeout (a model's reply 10 min including queue time, an AI waiting on a person 5 min, both lobby settings), then a hybrid seat falls back to its bot and a full model seat declines; a cap of 30 messages; bots answer with canned lines.
- **Minimap:** human seats only; explored land only; at most 44x28 early, the explored home continent from the Renaissance, the full map from the Information era; keeps its size, never zooms back in.
- **Accounts:** visibility `global` means the public web, keeping allowlist and link sharing; profiles with per-field visibility, a 256 KB avatar upload, a members-only directory with an opt-out, an admin users page; future restrictions are capability flags.
- **Ratio:** GPU generation-seconds plus CPU core-seconds plus a credit priced per hardware class; self-use excluded; the member chooses who sees it, admins always can.
- **General:** there are no users yet, so schema and data may break (a fresh Alembic baseline); stability and speed first; fix bugs rather than carry them; one agent at a time; never port 8799, `C:/dev/citar-play`, a push or the VPS.

### P3.1.3 Non-goals

- **Phase 4:** the Rust helper and protocol v2 (raw completions, a real cancel, settings and catalog frames, power reports), CPU jobs and their limits, proof of work, the automatic trust tiers, the Rust hardware scan. Phase 3 must not depend on any of it, and leaves these hooks (named in each section): the machine's `capabilities.cpu` and settings, `compute_events.kind = 'cpu'` with `verification`, hardware classes for CPUs, `trust_tier` on users and machines, scheduler capacity read from one settings function, and the cancel path `Conversation.close() -> hub.cancel()`.
- **Phase 5-6:** packaging, deploy, the release docs pass, the real-model hybrid soak (plan §4.8).
- Federation between CITAR servers; streaming completions (protocol v1's unused `CHUNK`); replays in the `Delta` format; ETags on views (plan §6.8).

### P3.1.4 Exit criteria

| Criterion | Measured by |
|---|---|
| Access is closed by default | `scripts/audit_routes.py --strict`: 0 unguarded. Every route added or changed in Phase 3 has a stranger test (another member's private thing is a 404; an anonymous caller gets 401 or 404) beside its owner and admin tests (P3.9.1). |
| Every job type is owned | Benchmark suites and runs, probes and probe runs, lab experiments, scenarios, maps, bot profiles and reports have rows with an owner and a visibility; a non-owner's control call is refused; listings are filtered by visibility (3-02's gates). |
| Profiles, directory, admin | Field visibility honoured for anonymous visitors, members, the owner and admins; avatars validated; the audit log readable by admins (3-03). |
| One registry | `config/servers.json` imported once and retired; every machine is a row; `#/servers` redirects to the Servers page (3-04). |
| The scheduler is fair and never busy-waits | No sleep-and-retry on `busy` remains (a grep gate); ordering, round-robin, lending, the benchmark exclusion, the loaded-model rule, deadlines and cancellation tested; metrics carry generation time and queue time apart (3-05, 3-06). |
| The ledger adds up | Given and used per member with self-use excluded; rollups equal the raw sums; a hard budget blocks at its limit (3-07). |
| Chat behaves | Timeouts enforced by the session alone; status events reach only the two parties and god-view spectators; drafts and the deal builder survive updates; End Turn shows who you wait for (3-08, 3-09). |
| Hybrid seats honour their categories | For each of the ten categories, with that category the model's, the bot does not act in it and the model can; fallback to the bot on timeout; the veto on mixed deals; paired control games and the leaderboard (3-10 to 3-12). |
| The minimap leaks nothing and never shrinks | `node --test` on the window logic; the engine's home region holds only explored tiles (3-13). |
| A hand-set machine stays hand-set | A helper's reconnect leaves manual hardware, models and power alone and keeps the scan as "last scan" (3-14). |
| It all works together | `python scripts/qa_phase3.py all` exits 0 on a dev server and the browser checklist of P3.9.5 passes with screenshots, recorded in "As built in 3-15". The Python suite, ruff, the route audit, the link check, `mkdocs build --strict`, the front-end checks, and (where Rust changed) fmt, clippy, nextest, refcheck `--strict --with-bot` and `cargo golden check` green. |

### P3.1.5 What changed since the plan was written

Each checked against the code at `d93345a`:
1. **3E's engine half is done.** Sequence numbers, required messages, the cap, the aliases, the empty-counter refusal, the End Turn rule, `close_negotiation`, the tool text and the chat lines in `turn_progress` are all in the Rust engine. What is left in the engine is the schema's `required` list (P3.5.4).
2. **The "late reply" defect has changed shape.** A person's reply to a model's offer is no longer lost at the end of the model's turn (the turn cannot end with the chat open), but a reply that comes after the tool's own wait reaches the model only when it tries `end_turn` or asks `get_diplomacy`. The fix is to put the reply in front of the model before its next step (P3.5.3).
3. **The hybrid turn is the engine's drive.** The plan's "bot plays with LLM-owned categories off, the lock is released, the diplomat runs, the turn ends" is `drive()` → `Stop::HybridDiplomat` → the diplomat outside the lock → `drive()` again (P3.6.3). `BasicBot.diplomacy_owners` is citar-bot's `Owners` on the bot handle.
4. **"Engine changes in Python first" happened, in Rust.** Phase 3 makes four small Rust changes only: `respond_negotiation`'s schema (3-08), a binding for `citar_bot::evaluate` and `normalize_seats` keeping a seat's `hybrid` object (3-10), and the minimap block in seat views (3-13).
5. **The registry merge is a storage move, not only a page merge.** `servers.py` (LLM seats, managed model loading, quiet hours, keys), benchmarks, costing and reports all read `config/servers.json`; it moves into the `servers` table (P3.2.8).
6. **Budgets were in money** (`budgets.currency`, priced by `costing.py`). They become credits, the ledger's unit, so they can reserve and settle against `compute_events` (P3.4.6).
7. **`public` becomes `global`,** and `server` is new; the string sweep covers every place P3.0 lists.
8. **"Helpers report power"** needs protocol v2's `power` frame: Phase 4. Phase 3 keeps the host's own power sampler and the JSONL power rows Phase 4 will write into.
9. **The plan's 'server' pseudo-user** is `provider_id = NULL` (and `consumer_id = NULL` for the server's own data collection), so no fake account appears in the directory or the admin page.
10. **Saves:** seats are saved with the session record (`asdict(seat)`, `session.py:888-893`), so a seat's `hybrid` block needs no save-format change; the engine needs none either.
11. **Report narratives** bypass `make_conversation` today; they move onto it so that the scheduler sees every model call (P3.3.4).

---

## P3.2 3A: accounts, visibility, ownership, profiles, directory, admin, the registry merge

### P3.2.1 Today

See P3.0's database and server bullets. Also: local mode signs in a real admin account called "local" (`citar/auth/accounts.py:432`, `auth/deps.py:76-82`); capabilities are `register_servers, run_reports, publish_public, create_games, invite` with role defaults in `settings_kv` and per-user overrides in `User.caps` (`auth/policy.py:29-49`); probation denies `register_servers`, `invite`, `publish_public`; nothing can edit caps or limits after `create_user` except the database; `audit.recent()` has no route; the game sharing routes exist (`app.py:1557-1636`) and `api.js` wraps them, but no page calls them.

### P3.2.2 The schema baseline

- **One revision replaces both.** `citar/migrations/versions/` loses `f064316a6b34` and `767c824c2fcb` and gains `0016a_baseline` (down-revision none), written by hand from the metadata and checked by a test that compares it with `Base.metadata` (alembic's `compare_metadata` finds nothing). It holds **every table and column Phase 3 needs**, so later packages fill in behaviour rather than migrate, as 2-00a wrote every signature first. A later package that must change the schema adds a revision on top, with an upgrade test from the baseline; nothing re-squashes.
- **Old databases.** `server/boot.py:migrate()` today runs `upgrade head`, or `create_all` plus `stamp head` for tables without `alembic_version`. It gains:
  - a database at `f064316a6b34` or `767c824c2fcb` (a 0.1.5 or Phase 0-2 database): on SQLite, the engine is disposed, the file and its `-wal`/`-shm` are **renamed** (never deleted) to `citar-0.1.5-<YYYYmmdd-HHMMSS>.db` beside it, a warning names the new path, and the server starts on a fresh database; on Postgres it refuses to start with a message saying to create a new database (`CITAR_DB_URL`) or drop the schema, since a server process should not rename someone's database;
  - any other revision the scripts do not know (a newer build's): refuse to start, as today's alembic error, with the message spelled out;
  - tables without `alembic_version` (only tests create those, with `db.create_all()`): `stamp head` only if `users.listed` exists (the baseline's), otherwise treated as an old database.
  - Saves are not in the database: a game loaded from disk gets its `games` row again (`ownership.register_existing`, now called on load).
- **Tables and columns** (`citar/db/models.py`):
  - **`users`** gains `bio` (Text, at most 1,000 characters), `links` (JSON, at most 3 `{label, url}`), `avatar_path` (String(200), relative to the data directory, null), `field_visibility` (JSON, P3.2.6), `listed` (Boolean, default true), `trust_tier` (String(16), default `new`: `new|provisional|trusted|quarantined`, set by admins in Phase 3, automated in Phase 4). `avatar_url` stays for the sign-in provider's picture.
  - **`owned_items`** (new; P3.2.4): `id` (`oi_<hex>`), `kind` (String(24), indexed), `ref` (String(120): the id on disk), `owner_id` (FK users, SET NULL; null for system items), `name`, `visibility` (default `private`), `slug` (unique, as games'), `path` (String(512), relative to the state directory), `meta` (JSON), `collection` (Boolean, default false: the server's own data collection, P3.4.2), `created_at`, `updated_at`, `deleted_at`; unique `(kind, ref)`.
  - **`reports`** is dropped: a report is an owned item of kind `report`, its `scope` and `spec` in `meta`.
  - **`games`**: `status` becomes `setup|playing|finished|closed` (P3.2.5).
  - **`servers`** gains `is_host` (Boolean: the machine running CITAR), `server_resource` (Boolean: the site provides it, so it is the server's in the ledger; true for the host and for online APIs an admin registers), `capabilities` (JSON, P3.8.2), `hardware_mode`, `models_mode`, `power_mode` (String(8): `auto|manual`; defaults auto, auto, manual), `hardware_detected` (JSON, null) and `detected_at`, `gpu_class_id` and `cpu_class_id` (FK `hardware_classes`, null), `class_override` (Boolean), `trust_tier` (as users'). `reach` (`direct|worker`) and `max_concurrent` stay; `max_concurrent` becomes the one site number for LLM slots (P3.2.8).
  - **`hardware_classes`** (new; P3.4.3): `id` (`hc_<hex>`), `kind` (`gpu|cpu|api`), `name`, `rate` (Float: credits per generation-second for `gpu` and `api`, per core-second for `cpu`), `match` (JSON), `priority` (Integer), `is_default` (Boolean), `updated_at`, `updated_by`. Seeded by the baseline: `GPU (unclassified)` 1.0, `CPU core (unclassified)` 0.05, `Online API` 0.0, each the default of its kind.
  - **`compute_events`** and **`compute_rollups`** (new; P3.4.2, P3.4.5).
  - **`budgets`**: `currency` becomes `unit` (String(8), default and only value `credits`); `budget_entries` amounts are credits.
  - **`acl_entries`**: `object_type` takes the item kinds; `OBJECT_TYPES` in `models.py` and `access.py` follow.

### P3.2.3 The visibility ladder

| Level | Who may view | Listed for | Needs |
|---|---|---|---|
| `private` | the owner and admins | the owner and admins | |
| `allowlist` | the owner, admins and the accounts with an ACL entry | the same | |
| `link` | the same, and anyone presenting the slug | the owner, admins and ACL accounts only (unlisted) | |
| `server` | the owner, admins, ACL accounts and every signed-in member whose status is `probation` or `active` (no slug needed) | members | |
| `global` | anyone on the internet | everyone | `publish_global` |

ACL entries (view, play or manage, as today) count at every level, so sharing with a person never needs a wider level.

- **One place.** `access.VISIBILITY` is the tuple and `access.check_visibility(value)` the validator; `app.py`'s game creation, `pool/__init__.py`'s groups, `share_api`, `ownership.to_client`, `access.set_visibility` and every new sharing route call it. A test greps `citar/` for the literal `"public"` used as a visibility (outside the migration) and fails on any.
- **`access.on`**: `server` grants `VIEW` to a signed-in viewer with an active status; `global` grants `VIEW` to anyone. ACL entries add to any level. Moderators get `VIEW` and `MANAGE` on `server` and `global` objects (both are "published") and on a `link` object only with its slug, as today. A suspended or deleted account keeps nothing.
- **`visible_filter`**: `global` rows for everyone; plus `server` rows for members; plus own rows and ACL rows; admins everything; `link` rows never listed.
- **Capability:** `publish_public` is renamed `publish_global` (`CAPABILITIES`, `ROLE_DEFAULTS`, `PROBATION_DENIES`, the console's policy editor); `server` needs no capability. A member's own profile fields may be `global` without it (they are the member's own data) unless the account is on probation (P3.2.6).
- **The share URL** (`ownership.to_client`, the item sharing routes) is offered for `link` (carrying its key), `server` and `global` (without one).

### P3.2.4 Owned items

- **`citar/items.py`**: `register(sdb, kind, ref, owner, name, path=None, visibility="private", meta=None)`, `rename`, `delete` (sets `deleted_at`), `get(sdb, kind, ref)`, `resolve(sdb, kind, ref, viewer, slug=None) -> (row, Access)`, `require(...)` (raises 404/403 as `Access.require` does), `visible(sdb, kind, viewer)`, `adopt_orphans(sdb, owner)`. `access.on` learns `OwnedItem` (its `object_type` is the row's `kind`).
- **Kinds and their producers**, each registering at creation and deleting at deletion:

| Kind | `ref` | Registered by | Files stay at |
|---|---|---|---|
| `benchmark_suite` | suite id | `benchmarks.save_suite` | `BENCH_DIR/suites/` |
| `benchmark_run` | run id | `create_run` (the run's owner owns its games' rows too, kind `benchmark`, the run's visibility) | `BENCH_DIR/runs/` |
| `probe` | probe id | `probes.save_probe` | `saves/probes/` |
| `probe_run` | run id | `ProbeRunner.start` | `saves/probes/runs/` |
| `lab_experiment` | experiment name | `bots_api` POST `/experiments`, `lab submit` | the lab's queue and done folders |
| `scenario` | scenario id | the scenario editor's save | `saves/scenarios/` |
| `map` | map id | `PUT /api/maps/{id}` | the maps folder |
| `bot_profile` | profile id | `bots_api` create and fork | the profiles folder |
| `report` | report id | `ReportRunner.start` (the runner now writes the row) | `saves/reports/<id>/` |

- **System items.** Shipped content (the built-in bot profiles `standard`, `classic-production`, `idle`; example scenarios and maps, if any) is registered at boot with `owner_id = NULL` and `global`; only admins manage them.
- **Adoption.** At boot, files of these kinds without rows are registered to the first admin (in local mode, the local owner), `private`, with one audit row. After the fresh start there are few or none, but a half-written file must not become an unowned, unguarded object.
- **Capabilities** gain `run_benchmarks` (user default true), `run_probes` (true), `run_lab` (false: the lab uses the server's CPU, today admin-only), `edit_bots` (false: today admin-only), `full_llm` and `partial_llm` (true; P3.3.7). `run_reports` stays.
- **Routes and their gates.** Every route below resolves its item through `items.require`; listings use `items.visible`. "Create" means the capability plus, where the work runs on a machine, admission (P3.3.7).

| Routes | Read | Change |
|---|---|---|
| `GET /api/benchmarks/status`, `/runs` | member; runs filtered | |
| `GET/PUT /api/benchmarks/settings` | member | admin (server-wide score weights) |
| `/api/benchmarks/suites[...]` | VIEW the suite | create `run_benchmarks`; update and delete MANAGE |
| `POST /api/benchmarks/runs` | VIEW the suite | `run_benchmarks`; the run is the caller's |
| `/runs/{id}/control`, `/jobs/{job}/control` | | MANAGE the run |
| `/runs/{id}/jobs/{job}/watch` | VIEW the run | |
| `/api/maps[...]`, `/maps/validate`, `/maps/generate` | VIEW the map | create `create_games`; update and delete MANAGE |
| `/api/scenario-editor[...]` | the editing session's creator or an admin | saving creates the scenario item, or needs MANAGE on it |
| `/api/scenarios[...]`, `/launch` | VIEW | delete MANAGE; launch `create_games` and the game limit |
| `/api/probes[...]`, `/probes/{id}/run` | VIEW | create `run_probes`; update and delete MANAGE; run `run_probes` |
| `/api/probe-runs[...]`, `/open/{case}/{rep}` | VIEW | stop and delete MANAGE |
| `GET /api/lab`, `/lab/report/{name}` | member, experiments filtered; VIEW | |
| `/api/bots/profiles[...]`, `/fork` | VIEW | create and fork `edit_bots`; update and delete MANAGE |
| `POST /api/bots/experiments` | | `run_lab` |
| `/api/reports[...]` (from `admin_api.py`) | VIEW | create `run_reports`; rerun and delete MANAGE |
| `GET /r/{slug}` | `global`, or `link` with its slug | |
| `GET /api/queue` | member; others' items show as "someone's <kind>" unless visible | |
| `POST /api/queue/priority` | | MANAGE the item (a game through its `games` row) |
| `GET /api/models/scores`, `/api/metrics/compare` | member; pooled data only (below) | |
| `GET/PUT /api/items/{kind}/{ref}/sharing`, `POST .../sharing/users`, `DELETE .../sharing/users/{handle}`, `POST .../sharing/rotate` (new) | MANAGE | MANAGE |

- **`share_api`'s report routes** become the generic item routes; `/r/{id}` becomes `/r/{slug}` for reports (a `global` report is also reachable by id).
- **A report gathers only what its owner may see.** Report building was admin-only, so `citar/reports/data.py` reads every save, run and ledger row in scope. Opened to members, it takes the report's owner: per-game, per-run and per-activity sections cover the owner's own activities and the items the owner may view; machine costs (configurations, tariffs, depreciation) only the owner's machines; anything wider comes from pooled aggregates (`aggregate.scope_for`, the minimum-contributor threshold). An admin's report sees everything, as today. The report's `scope` (`own|pooled|both`) keeps its meaning within those limits.
- **The pooling opt-out.** `scoring._reports` and `/api/metrics/compare` read only games whose row allows it for the viewer: the viewer's own games, plus games with `data_sharing = pool` whose owners are not opted out, through `aggregate.scope_for` with its minimum-contributor threshold. Benchmark games are rows now, so pooled aggregates see them.
- **The sharing dialog** (`citar/web/js/share.js`): visibility select, the share link with copy and rotate, the people with access (handle, permission, remove), "add by handle"; used by games (the existing routes), reports, benchmark runs, probe runs, scenarios, maps and bot profiles. A member who opens a page they cannot see gets the router's "not found" page, never a stack trace.

### P3.2.5 The games index

`ownership.sync(sdb, session)` runs from the session (off the game lock, through `ownership.sync_quietly`, on the writer's thread after an autosave lands, so it costs no lock time) when the phase or the turn changes and when a session stops: `status = "playing"` while `phase == "playing"`, `"finished"` once it is `over` (with `finished_at`), `"closed"` when a running game is closed without finishing, `"playing"` again when it is loaded. `_game_limit` counts `setup` and `playing` rows only. `GameSession` gains `owner_id` (set by the routes that create or load it), which the scheduler and the ledger read.

### P3.2.6 Profiles and the directory

- **Field visibility.** `users.field_visibility` holds `{profile, display_name, avatar, bio, links, items, ratio}`, each `private|server|global`. Defaults: `profile`, `display_name`, `avatar`, `bio`, `links`, `items` = `server`; `ratio` = `private`. A field shows when the viewer's level reaches both the field's and `profile`'s: anonymous visitors see `global`, members `server` and `global`, the owner and admins everything. The handle is always shown wherever the account appears (seats, sharing, grants). Setting a field to `global` is refused on probation ("New accounts can show their profile to members only").
- **`User.public()`** returns `id`, `handle`, `display_name`, and `avatar` (the upload's route or the sign-in picture) only; `role` and `status` move to `User.account()` (the owner's own view and admins). Every caller is checked: `shared_with`, grant subjects, server owners, seat owners.
- **Avatars.** `POST /api/auth/me/avatar` (multipart, at most 256 KB) accepts PNG, JPEG and WebP, recognised by their magic bytes and with dimensions read from the header (PNG IHDR; JPEG SOFn; WebP VP8, VP8L, VP8X), at most 1024x1024; anything else is refused with the rule. Stored as `<data dir>/avatars/<user id>-<sha256[:12]>.<ext>` (the previous file removed), never under the code or the saves. `GET /api/users/{handle}/avatar` serves it with the type fixed by the validated format, `X-Content-Type-Options: nosniff`, `Content-Security-Policy: default-src 'none'; sandbox` and a cache lifetime, when the `avatar` field is visible to the caller; else 404. `DELETE /api/auth/me/avatar`. No image library is added: nothing is decoded, only the header is read.
- **`PUT /api/auth/me`** also takes `bio` (control characters stripped, 1,000 characters), `links` (at most 3; label up to 40 characters; `http`/`https` URLs up to 300 characters), `field_visibility` (known keys, known values) and `listed`.
- **`GET /api/users`** (members only): accounts with `listed` and an active status, newest first, searchable by handle or display name, each with the fields visible to the caller; paged by 50.
- **`GET /api/users/{handle}`** (anyone): 404 unless `profile` is visible to the caller; the visible fields; the account's items visible to the caller (games rows and owned items, through `visible_filter` with `owner_id = them`, 20 per kind, newest first) when `items` is visible; the ratio block when `ratio` is (3-07). A hidden account (not `listed`) still has a page for whoever may see it; it is only missing from the directory.

### P3.2.7 The admin users page and the audit log

- `GET /api/admin/users` (filters: text, role, status; paged), `GET /api/admin/users/{id}`, `PATCH /api/admin/users/{id}` with any of `role`, `status` (with `reason` for a suspension), `caps` (`{cap: true | false | null}`, null removing the override), `invite_quota`, `max_concurrent_games`, `max_seats_per_game`, `trust_tier`, `listed`. Each change is one audit row naming the fields; the last admin cannot demote or suspend themselves. `GET /api/admin/audit` (filters: actor, action, object type and id, before; at most 200 a page). All `require_role("admin")`.
- **CLI parity** in `citar/server/admin_cli.py`: `create-user` (handle, email, password, role, status active, email verified: the QA harness uses it), `set-caps`, `set-limits`, `set-trust`, `audit`.

### P3.2.8 The registry merge

- **Every machine is a `servers` row.** `servers.py` keeps its public functions (`find`, `get`, `list_servers`, `host`, `resolve_llm`, `seat_ref`, `describe_seat`, `model_entry`, `profile`, `ensure_model`, `restricted_now`, `restricted_text`, `server_name`, `normalize_server`) with their return shapes, and reads and writes rows instead of `config/servers.json`: a row's `config` already holds the `normalize_server()` dict (`pool/__init__.py`), and `reach` says who dials (`direct`: CITAR; `worker`: the helper). `with_pooled()` becomes the identity. The electricity plans move to `settings_kv` (`electricity_plans`); `host_server_id` becomes the row with `is_host`.
- **One-time import.** At boot, when `config/servers.json` exists and `settings_kv.registry_imported` is unset, `pool.import_registry` (it exists) brings every server in under its own id, owned by the first admin (in local mode, the local owner), `reach = direct`, `is_host` for the registry's host, `server_resource` for the host and for `api` kinds; the file is renamed `servers.json.imported-<date>`. API keys stay in the keystore under the same ids.
- **One concurrency number.** `Server.max_concurrent` is the site's LLM slots; `config.connection.max_parallel` is read once on import and dropped from new configs (`normalize_server` keeps reading it as an alias). The helper's `hello.max_concurrent` is its local ceiling, and the scheduler takes the smaller (P3.3.2).
- **Routes.** `admin_api.py`'s `/api/servers*` routes keep their paths and become owner-or-admin on the row (`access.on(Server)`, MANAGE): list (the caller's machines, all for admins), new, detect, state, load, unload, loading, loaded-config, estimate, collect, hardware, rates, keys, restricted. **Creating or switching a machine to `reach = direct` is admin-only** unless its provider is `dryrun`: a direct machine is a URL the CITAR server dials, and letting members name one would let them make the server request addresses on its own network. Electricity plans stay admin-only. For the same reason `POST /api/llm/models` (the lobby's "Detect models", which fetches a caller-supplied base URL; `app.py:321-363`) becomes admin-only except for the `dryrun` provider: a member's machine reports its models through its helper. `/api/pool/servers` remains the sharing view for everyone a group reaches (redacted by `public_server`).
- **The page.** `#/pool` is the one Servers page: every machine the viewer owns or may use, direct and helper machines alike, with the sharing it has today. `servers.js`'s editors become dialogs (connection and models with load profiles; hardware and power; costs; quiet hours; API key for direct machines) opened from a machine's card; `#/servers` and `#/servers/<id>` redirect to `#/pool` (opening that machine). The nav's "Servers" goes to `#/pool`.
- Benchmarks' `resolve_group` and `_resolve_pooled` merge (every group is a row; `reach` decides whether its seats play through the helper).

### P3.2.9 The web pages

- `#/users` (members: the directory, a search box, cards with avatar, display name, handle, bio's first line), `#/u/<handle>` (a profile; open to anonymous visitors when `global`; the router lets this route through without a session), `#/admin/users` (admins: a table with filters, an edit drawer for role, status, caps, limits, trust, and the account's audit rows), and a "Profile" card on `#/account` (bio, links, avatar upload with preview, the field visibility selects, the directory toggle, "view as a member" and "view as a visitor" links).
- The nav gains "People" for members and "Users" beside the admin's "Server".

### P3.2.10 Tests

`tests/test_access.py` (the ladder table: five levels by eight kinds of viewer by view, play and manage), `tests/test_migrations.py` (P3.2.2), `tests/test_items_access.py` (table-driven over every route in P3.2.4: stranger, ACL viewer, owner, admin, anonymous), `tests/test_profiles.py` (field visibility by viewer, avatar validation with a corpus of good and bad headers, links and bio limits, the directory), `tests/test_admin_users.py`, `tests/test_registry_merge.py` (a fixture `servers.json` imports to rows equal to `normalize_server()` of each entry; every `servers.*` function answers as before; a member's direct machine is refused, a dry-run one allowed).

---

## P3.3 3C: the prompt scheduler, task classes and the Queue page

### P3.3.1 Today

Every model call goes straight to its provider. A helper machine answers `busy` when its slots are full and the caller sleeps and retries; a direct machine has no slot accounting at all. Machines are "occupied" per activity for a whole game. There is no per-request queue, fairness or priority; `pool.queue` orders whole jobs (benchmark jobs, probe runs, reports, lobby games' make-way) by priority and age (`citar/pool/queue.py`). See P3.0.

### P3.3.2 Machines and capacity

- **`citar/pool/scheduler.py`** keeps one `Machine` per `servers.id`, made on first use and shared by the whole process: sessions, the benchmark scheduler, the probe runner and the report runner. The lab never calls models.
- **Capacity** is `Server.max_concurrent` (the one site number, P3.2.8), capped by the connected helper's `hello.max_concurrent` for a helper machine. While a helper machine is offline its capacity is unknown: lease requests pass through, so the provider's connection error drives today's reconnect window and disconnect policy, and tickets are refused as temporary.
- **Not scheduled:** a model configuration without a `server_id` (tests' inline configs, a raw provider) passes through, measured by the metrics but not by the ledger.
- Capacity, the loaded models (a helper's reported `loaded` flags; LM Studio's model list for a direct LM Studio machine, cached; every model for an online API) and the quiet hours are cached per machine, re-read at most every 5 s and at once on a helper's connect or disconnect, so no request touches the database.

### P3.3.3 Leases, tickets and the order

- **A lease** (task class *full*) is a model seat that plays every turn. The session makes it with the seat's agent (`GameSession.lease_for(pid)`) and releases it when the agent is cancelled, the seat changes or is eliminated, or the session stops. Kinds: `game` (a lobby game; lends its slot between turns), `benchmark` (never lends), `probe_case` (a probe's turn case, for the case only; never lends). The driver calls `activate()` before the seat's `play_turn` and `idle()` after it. Every request the seat makes, in its turn or answering a chat out of turn, is a request of its lease.
- **A ticket** (task class *partial*) is one pooled prompt: a hybrid seat's diplomat phase or chat answer, a probe's offer or message case, a report's narrative, the Servers page's test. It carries the machine, model, activity, the consumer (a user id), its class (`chat`, `diplomat`, `probe`, `report`, `test`), a priority, a deadline (the latest start), `steps` (how many consecutive calls it may make once started; default 8) and `wait_for_benchmark`.
- **The rules,** which tests hold as invariants:
  1. Requests in flight never exceed capacity.
  2. A lease request takes the first free slot, before any ticket; lease requests are served in arrival order.
  3. A ticket starts only when (a) no benchmark lease is live on the machine (otherwise it is refused at once, "held by a benchmark", unless `wait_for_benchmark`, when it waits); (b) its model is loaded, or the machine allows swaps (`capabilities.llm.allow_swaps`, P3.8), or a live lease uses that model (otherwise refused at once, "the model is not loaded"); (c) the machine is outside its quiet hours (otherwise refused as temporary, with `retry_at`); (d) in flight + 1 ≤ capacity − reserved, where *reserved* counts the leases that are active (their seat mid-turn) or never lend, and have no request in flight.
  4. So a seat mid-turn always finds its slot between steps, a benchmark's slot is never lent, and an idle lobby lease's slot is lent: when that seat's turn comes, it takes the next slot that frees, which a running ticket gives back within its step budget.
- **Order among tickets:** priority, highest first; within a priority, round-robin across consumers (the one served longest ago first); within a consumer, first come first served. Priority = the class's base (`test` 30, `chat` 20, `diplomat` 10, `probe` 0, `report` −10) + the activity's queue priority (−100..100, `pool.queue.priority`, which its owner or an admin sets on the Queue page).
- **A started ticket** holds its slot for up to `steps` calls or until it is closed. States: `queued → running → done`, or `cancelled`, `expired` (not started by its deadline; the caller falls back), `refused` (rule 3, admission or a budget). A running ticket is never cut off by its deadline: its caller's own budget stops further steps.
- **Cancel:** a queued ticket leaves the queue; a running one closes its conversation. `WorkerConversation.close()` now calls `hub.cancel(server_id, request_id)`, which sends `CANCEL` and resolves the pending request with an error. A v1 helper ignores `CANCEL` and keeps generating (Phase 4 fixes that); if it then answers the next request `busy`, the scheduler puts that request back at the head of the queue once, uncounted, rather than failing it.
- **Position and ETA:** each machine keeps an exponentially weighted mean of generation time per (model, class); a ticket's ETA is the expected time of the work ahead of it over the slots it may use. Both are published to the ticket's status callback.

### P3.3.4 The conversation wrapper and metering

- `make_conversation(cfg, system, tools, binding=None)`: with a lease or a ticket and a scheduled machine it returns `ScheduledConversation(inner, binding)`, whose `step()` acquires the binding's slot (blocking; never with a session lock held, which a debug assertion checks in tests), runs the inner step, releases, and records `last_gen_s` and `last_queue_s`. Generation time is the helper's `Done.duration` on a helper machine (`WorkerConversation` now keeps it) and otherwise the wall time of the HTTP call, measured after the slot was granted.
- **Listeners:** `scheduler.add_listener(fn)` receives a `StepEvent` (machine, model, activity, consumer, task class, class, `gen_s`, `queue_s`, the usage delta, request id, time) after every step. The usage tracker's `llm()` is the first listener and replaces `LLMAgent._meter`, so an attempt that never reached the model meters nothing; the ledger is the second (P3.4).
- **One path for every model call.** Report narratives (`reports.complete`, which today calls the SDKs directly) and the Servers page's test go through `make_conversation` with a ticket; `_complete_on_worker`'s polling goes.
- `LLMAgent._step` loses its `busy` branch. `metrics.model_step` records `gen_s` as the step's seconds and a new `queue_s`; the stats screen shows "waited for the machine" beside generation time, and the model score's speed reads generation time only.
- `WorkerHub.submit` keeps its busy check as a safety net only.

### P3.3.5 Occupancy, quiet hours and the job queue

- `pool.seats.occupied()` returns the labels of the leases live on a machine (game, benchmark, probe case). Hybrid seats, chat answers and report narratives are tickets and never occupy a machine. `claim()` and `release()` go.
- `pool.queue` stays the job-level queue for work that needs a lease (benchmark jobs, probe runs, lobby games making way). Reports leave it: their narrative is a ticket. A benchmark job starts when its machine has fewer leases than its limit (`scheduler.leases(server_id)`); a probe run takes a `probe_case` lease for each turn case and tickets for offer and message cases; a lobby game's `_quiet_hours` and `_make_way` apply to its full seats.
- `usage.session_servers` counts a machine as held by a game only while one of the game's leases is live on it.

### P3.3.6 Lending and the benchmark rule

A lobby lease lends its slot between its seat's turns (P3.1.2). A benchmark lease never lends, and while one is live on a machine no ticket starts there, so a benchmark's model has the machine's whole GPU for its timing. A hybrid benchmark game holds no lease: its prompts are tickets with `wait_for_benchmark` and no fallback (P3.6.9), so it never runs beside a full benchmark and never stops pooled work.

### P3.3.7 Admission and task classes

- Grant purposes gain `pooled` (partial prompts); `game` remains full seats; an empty list still means all.
- Capabilities `full_llm` and `partial_llm` (P3.2.4) are the hooks for future restrictions (plan A3). An LLM seat needs `full_llm` and a grant for `game`; a hybrid seat `partial_llm` and `pooled`; a report's narrative `run_reports` and `pooled`; a probe run `run_probes`, with `probe` for its turn cases and `pooled` for the rest.
- `pool_seats.authorize` checks every seat with a model by its class, and `update_seat` runs it again.
- A ticket is admitted when it is made (`admission.check(purpose="pooled")` for the consumer on that machine); a refusal is the ticket's `refused`, with the admission's reason and `retry_at`.

### P3.3.8 The Queue page

- `GET /api/queue` adds, per machine, its capacity, its leases (activity label, kind, lending or active, the member when the caller may see the activity) and its running and waiting tickets (position, class, the activity's label or "someone's <class>" when the caller may not see it, the member's handle under the same rule, model, priority, waited, ETA, deadline).
- `POST /api/queue/priority` takes `kind = game|benchmark|probe|report|lab` and needs MANAGE on the activity (P3.2.4); tickets follow their activity's priority. `POST /api/queue/tickets/{id}/cancel` (new) is for the activity's owner, the machine's owner and admins.
- `citar/web/js/queue.js` shows a "Pooled prompts" section under each machine, with priority buttons and cancel only where the caller may use them.

### P3.3.9 Threading

One lock and one condition per machine. `acquire` waits on the condition in ticks of at most a second, to notice cancellation, a stopped session and its deadline. The callers are threads already (the session driver, responders, the report and probe runners, benchmark sessions). Nothing in the scheduler takes a session lock or reads the database on the request path.

### P3.3.10 Phase 4 hooks

Capacity and the swap rule come from one function, `servers.machine_settings(server_id)`, which Phase 4's settings frame will also feed; every request carries the id the v2 `cancel` frame will name; v2's reported `gen_s` arrives through the same listener.

---

## P3.4 3B: the compute ledger and the ratio

### P3.4.1 Today

The usage ledger (`citar/usage.py`) is JSONL in `saves/usage/`, per activity and machine, with held, busy and CPU seconds and tokens, and no user; `costing.py` prices it by reading every file. Budgets never accrue. Nothing records compute given or used per member.

### P3.4.2 `compute_events`

| Column | Meaning |
|---|---|
| `id` | integer, autoincrement |
| `at` | when the work ended (UTC, indexed) |
| `activity` | `game:<id>`, `benchmark_run:<id>`, `probe_run:<id>`, `report:<id>`, `lab:<name>`, `test:<id>` (indexed) |
| `activity_kind` | `game`, `benchmark`, `probe`, `report`, `lab`, `test` |
| `task_class` | `full`, `partial`, `cpu` |
| `consumer_id` | the member whose activity it is (FK users, SET NULL, indexed); null for the server's own data collection |
| `provider_id` | the machine's owner (FK users, SET NULL, indexed); null when the server provides the machine |
| `machine_id` | `servers.id` (indexed) |
| `kind` | `gpu`, `cpu`, `api` |
| `model` | the model that served it, or null |
| `gen_s`, `cpu_core_s` | generation seconds; CPU core-seconds |
| `tokens_in`, `tokens_out`, `tokens_reasoning` | from the provider's usage |
| `credits` | priced at write time (P3.4.3) |
| `self_use` | true when consumer and provider are the same member, or both are the server |
| `verification` | `unverified` for model work (attributed, never re-run; plan C2), `verified` for the host's CPU; Phase 4 writes `pending` for helpers' CPU jobs |
| `request_id` | the model request, for audit |

- **The consumer** is the activity's owner: the game's `games.owner_id`, the run's, the probe run's, the report's, the experiment's owned item. It is null when the item is marked `collection` (only admins set it: the server's own benchmarks and data gathering), which counts as given by whoever's machine served it.
- **The provider** is the machine's `owner_id`, or null when the machine is `server_resource` (the host; online APIs the site provides).
- **Given(u)** = events with `provider_id = u` and not `self_use`. **Used(u)** = events with `consumer_id = u` and not `self_use`. So a member's own games on their own GPU count on neither side; another member's hybrid prompts on their GPU count as given; their own games on the server's CPU count as used; work on a server machine for the server's own collection counts on neither.

### P3.4.3 Writers, credits and hardware classes

- **Writers.** (1) The scheduler's listener: one `gpu` (or `api`) event per model step. (2) The host's CPU: the usage tracker samples each activity's threads every 5 s (`usage.py:_sample`); at each flush it hands the ledger one `cpu` event per activity for the window, on the `is_host` machine, `provider_id` null. A game's driver thread runs its bots' drives (the engine releases the GIL but runs on that thread) and responders register their threads, so a game's CPU is attributed to it. (3) The lab: each game's `cpu_s` in its result (`lab.py:393`) becomes one `cpu` event, the experiment's owner its consumer. (4) Phase 4: helpers' CPU jobs.
- Events go through a ledger writer thread with a queue, inserted in batches (every 2 s or 200 events), never on a game's lock.
- **Credits** = `gen_s × rate` for `gpu` and `api`, `cpu_core_s × rate` for `cpu`, with the rate of the machine's class of that kind. The default rates are an energy proxy, one credit for roughly 300 W for a second: an unclassified GPU generation-second 1.0, a CPU core-second 0.05 (about 15 W a core), an online API 0 (no compute given; an admin may price it). Admins edit classes and rates (`/api/admin/hardware-classes`); a new rate applies from then on, and `citar admin reprice --since <date>` recomputes the credits of a period when wanted.
- **A machine's class** is the first class of its kind, by priority, whose `match` fits the machine's hardware (`gpu_name` regex, `vram_gb_min`, `cpu_name` regex, `cores_min`), else the kind's default; it is recomputed when the hardware changes, unless an admin pinned it (`class_override`).

### P3.4.4 Who sees what

- `GET /api/ledger/me?range=24h|7d|30d|all`: given and used (GPU seconds, CPU seconds, credits, tokens), the ratio (given credits over used credits; "only given" when nothing was used), by machine and by activity kind.
- `GET /api/users/{handle}/ratio`: the same totals when the account's `ratio` field is visible to the caller (P3.2.6); admins always.
- The admin users page shows each account's 30-day given, used and ratio; the Account page gets a "Compute" card; a profile shows the ratio line under the field's rule.
- Ratio-based restrictions later: an admission rule may read `ledger.ratio(user)` and set capabilities; nothing does in 0.1.6.

### P3.4.5 Rollups

`compute_rollups(user_id, hour, side, kind)` with sums of `gen_s`, `cpu_core_s`, `credits`, `tokens_in`, `tokens_out` and `events`, keyed by all four. A rollup thread recomputes, every 5 minutes, each hour touched since its last pass (delete and insert per hour, one transaction). Reads use the rollups for complete hours and the raw events for the current hour, so a total never waits for the pass.

### P3.4.6 Budgets

- A grant's budget is in credits per period (day, week, month or total, in the owner's time zone as today), hard or soft.
- **Reserve:** when a lease request or a ticket under a grant with a budget starts, `budgets.reserve(budget, activity, estimate)` holds the machine's mean credits per step for that model (at least 1). If a hard budget's spent plus reserved would pass its amount, the request is refused with the period's end as `retry_at`: a lease's game pauses with that reason (as for quiet hours, and resumes by itself when the period turns), a ticket is refused and its caller falls back.
- **Settle:** the ledger's listener settles each step's credits into the entry for (budget, activity, period) and releases the reservation. A soft budget only warns: an event to the consumer and a note on the machine owner's Servers page.
- `budgets.refresh_from_ledger` rebuilds a period's entries from `compute_events`: the admin's repair.

### P3.4.7 What costing reads

`costing.py` keeps pricing each machine by its configuration over time (electricity, depreciation, leases, token prices). Per activity and minute it now reads generation seconds, tokens and the host's CPU seconds from `compute_events`, so queue wait is no longer charged as work; held time and power samples stay in the JSONL ledger, which keeps its activity, `held` and power rows and stops writing `busy`, tokens and requests in spans.

---

## P3.5 3E: trade chat

### P3.5.1 Today

The engine side is done (P3.0, P3.1.5). The server waits per agent (90 s for a bot's turn, 240 s per model message), starts a thread per responder and gives a model's chat answer a fresh conversation with no deadline. The web client's Diplomacy modal is rebuilt on every event, losing what was typed.

### P3.5.2 Timeouts and the chat clock

- **Lobby settings.** The engine keeps any lobby key verbatim in the game's config (`api/host.rs:270-279`), so no engine change is needed: `chat_model_reply_seconds` (default 600: how long a chat waits on a model seat, LLM, MCP or a hybrid seat's model, queue time included), `chat_person_reply_seconds` (default 300: how long a chat waits on a person), and the existing `diplomacy.max_chat_messages` (default 30). The lobby gets a "Chat" group (minutes, and the cap); `GameSession.info()["config"]` lists them; a game without them uses the defaults.
- **`citar/server/chat.py: ChatClock`**, one per session, started with its driver. It tracks every open negotiation as (entries, awaiting, waiting since, deadline, kind of the awaited seat); a change of entries or of the awaited seat restarts the clock; while the game is paused the clock stands still. It wakes on the session's condition, and at least once a second. At a deadline it first cancels any answer still in progress for that chat (the seat's ticket, or its lease request through the agent's `cancel_chat(nid)`), then acts once, by the awaited seat's kind, through the session (each is an engine call that a lost race turns into an `ActionError`, which it ignores):
  - **a model seat** (llm, mcp): the seat declines, `respond_negotiation(reject, "(no reply in time)")` on its behalf;
  - **a hybrid seat whose model has the chat:** its fallback bot answers (`EngineGame.answer(pid, nid, fallback)`, P3.6.5);
  - **a person:** the chat closes as expired, "(no reply in time)", whoever opened it (an AI waiting on a person declines after the per-game timeout; a person waiting on a person ends the same way);
  - **a bot seat:** never; bots answer inside drives and through their responders at once.
- **The clock is the only timer.** `BotAgent.play_turn` waits until the chats its drive stopped on move or close, with no budget of its own (`REPLY_WAIT_SECONDS` goes); `LLMAgent._await_answer` and `_end_turn` wait the same way, with a safety cap of twice the timeout. The driver's force-end of a turn an agent left open (`_close_open_chats`, `session.py:796-799`) stays as the last resort.

### P3.5.3 Responders

- **`citar/agents/responders.py`:** one bounded `ThreadPoolExecutor` per process (`CITAR_RESPONDERS`, default 16) and a queue per (session, seat), so a seat answers one chat at a time, in order. `_dispatch_negotiation_interrupts` submits `(pid, nid, entries)` there instead of starting a thread; `_responding` drops a key when its chat moves or closes.
- Bot seats answer as now (`EngineGame.answer`); hybrid seats as P3.6.5.
- **Model seats get one conversation per chat** (`ChatThread`, held by the agent per negotiation, dropped when it closes, rebuilt from the history after a restart). Its system prompt is the diplomacy prompt of P3.5.4 with the seat's persona; its tools are `get_diplomacy`, `get_empire`, `get_players`, `get_victory_status`, `read_notes`, `write_notes`, `log_thought`, `respond_negotiation`, `send_message`. Each exchange adds what changed since the last (the other side's entries, the proposal from the seat's side, the minutes left) and runs up to `max_negotiation_calls` steps on the seat's lease, stopping when the chat moves. `_reject_if_unanswered` stays for an answer that failed outright; a slow one is the clock's.
- **Late replies.** Before each step of its own turn, `LLMAgent.play_turn` compares the chats it is in with the last step's (`_chat_marks`). When one has come back to it, it adds a note: "NEGOTIATION UPDATE: <name> answered negotiation #n: <their entry>; the proposal from your side: <...>. Answer with respond_negotiation, now or before end_turn." A person's reply reaches the model at once, not only at `end_turn`.

### P3.5.4 The engine and the model-facing text

- **Rust:** `respond_negotiation`'s schema requires `message` (`api/tools/registry.rs`), recorded in the tool list test's fixes and `tests/rules/intended.toml` as `respond-negotiation-message-required`.
- `citar/agents/prompts.py`: SYSTEM_PROMPT's DIPLOMACY section adds that a chat waits for an answer for a time the game sets and then closes or the silent side declines, and that whatever is typed in a chat is answered in it; NEGOTIATION_PROMPT becomes CHAT_PROMPT (who, what changed, the proposal from your side, minutes left).
- MCP (`citar/agents/mcp_server.py`): `wait_for_turn` reports each chat's deadline; INSTRUCTIONS say a chat waiting on you closes when its time runs out.

### P3.5.5 Status events

- `{"type": "negotiation_status", "negotiation": nid, "players": [a, b], "waiting_on": pid, "kind": "model"|"person"|"hybrid"|"bot", "state": "queued"|"thinking"|"waiting"|"closed", "position": n|null, "eta_s": x|null, "deadline": epoch seconds|null}`. The clock sends `waiting` with the deadline; the answering seat's lease or ticket callbacks send `queued` (position, ETA) and `thinking`; the session sends `closed`.
- The game socket (`app.py:1765-1830`) delivers it to a seat named in `players` and to spectators of a game whose god view is allowed, and to nobody else (a filter beside the event filter).

### P3.5.6 The web client

- **`citar/web/js/chatmodel.js`** (pure, no DOM, tested with `node --test`): merges a negotiation's entries by `seq`; derives a window's state (whose move; input enabled or not; the status line from the last `negotiation_status`; the countdown); derives the End Turn button's state from the open chats; keeps each negotiation's draft and builder state in a map that updates never touch.
- **`citar/web/js/chat.js`:** docked windows at the bottom right, one per civilization (the engine allows one open chat per pair), stacked, each minimisable. Bubbles and proposal cards in `seq` order, the game's close notes in grey. The textarea and the deal builder (moved out of `panels.js` into the window, collapsible) keep their state: new entries are appended and nothing is redrawn whole. A window opens straight from the socket's `negotiation` event naming you, reading `GET /api/games/{gid}/negotiations/{nid}` (new: the seat's `negotiation_view`; a seat token, or a spectator of a god-view game), without waiting for a view refresh; play goes on while it is open.
- **Strict turn-taking:** input and buttons are enabled only on your move; otherwise the window says "Rome is thinking…", "queued #3, about 40 s" or "waiting for you, 3:12 left". The quick buttons fill in default lines as today ("Agreed.", "No deal.", "How about this instead?"). Typing in an open chat sends `respond_negotiation(reply)`, which the other side answers; a broadcast `send_message` stays on the Diplomacy screen and gets no reply.
- **End Turn:** "Answer Rome first" (opening that chat) while a chat waits on you; "Waiting for Rome… 4:12" while one waits on the other side, and clicking it then arms "End turn when they answer" (cancellable), which ends the turn once every chat has closed with nothing left waiting on you.
- The Diplomacy screen keeps relations, war and peace, denouncing, the UN vote and broadcast messages; each civilization's row gets "Open chat", which opens the docked window (or starts a chat on your turn).
- The recap (`replay.js`) shows chats as threads, with their statuses and close notes.

### P3.5.7 Metrics

`metrics.negotiation` rows gain `nid`, `gen_s`, `queue_s` and `outcome` (`answered`, `declined_late`, `fallback`, `closed`); the stats screen shows chat answers per seat.

---

## P3.6 3D: hybrid seats

### P3.6.1 Today

The engine stops for a hybrid seat's diplomat and lets its bot defer a chat (P3.0); the bot has its switches, its advice and its valuation; the facade can make a bot with owners and drive it. The server has no hybrid seat type, and `BotAgent` drives a `hybrid_diplomat` stop straight on.

### P3.6.2 The seat

```
{"type": "hybrid", "name": "...", (civ_name, nation, leader, color, difficulty as for any seat)
 "bot": {"profile": "standard", "aggression": 0.4},
 "llm": {"server_id": "...", "model": "...", "persona": "...", ...},          as an LLM seat's
 "hybrid": {"owners": {"trades": "llm", "chat": "llm"} | "trade_only" | "all_but_war" | "everything",
            "advisor": true, "difficulty": "ai" | "human", "prompt_every": 10}}
```
- **Validation** (`citar/agents/hybrid.py: normalize_hybrid`): owners through `engine_api.bot_set_diplomacy` on a scratch bot (its messages are the errors); at least one category the model's ("a hybrid seat with nothing for its model is a bot seat"); `difficulty` `ai` or `human` (default `ai`); `prompt_every` 1..50 (default 10); `advisor` (default true in lobby games; always false in benchmark games).
- **Presets:** trade only = `trades`, `chat`; all but war = every category but `war`; everything = all ten.
- `Seat` gains `hybrid`; `SEAT_TYPES` gains `hybrid`. Saved with the session record.
- **The engine's seat:** `{"controller": "hybrid", "handicap": difficulty, "auto": {"un_vote": owners.un == "bot", "conquest": owners.captured_cities == "bot", "free_picks": true}}` at creation; `update_seat` and scenario launches call `set_controller` the same way. With `un` the model's, the engine no longer votes for the seat: the model votes with `un_vote`, and a vote it does not cast is an abstention, as for a person. With `captured_cities` the model's, a captured city stays a puppet until the model decides with `city_status`.
- **Scenarios:** `normalize_seats` keeps a seat's `hybrid` object as it keeps `llm` and `bot` (Rust, 3-10).

### P3.6.3 The turn

- **`HybridAgent`** (`citar/agents/hybrid_agent.py`) holds the seat's bot handle (its profile, its owners), a **fallback** handle (the same profile, every category the bot's; both use the seat's one memory in the game, so the fallback carries on the bot's plans), the model's configuration (`servers.resolve_llm`), and the diplomat's state (the last phase's turn and standings, the last event id read, the chats' threads).
- **`play_turn`** (on the driver thread):
  1. under the lock, `session.drive_bots(pid)`, which now drives every seat with a bot (bot and hybrid seats): the bot plays and leaves the model's categories alone;
  2. the drive stops at `hybrid_diplomat` and the lock is let go;
  3. if a trigger fired (P3.6.4) and no phase ran this turn, the diplomat phase (P3.6.6) runs, outside the lock, on one ticket;
  4. under the lock, drive again: the turn ends, or the drive stops at `awaiting_reply` for chats that wait on others (waited for as a bot seat waits, P3.5.2) or on the seat's own model (a chat it deferred, which its responder or the clock's fallback answers).
- Bot seats keep `BotAgent`; only hybrid controllers stop at `hybrid_diplomat`.

### P3.6.4 Triggers

From the events since the last phase and the standings:
- first contact with a major civilization; war declared on the seat; peace made; the seat denounced;
- a deal of the seat's expired or cut (`deal_expired`, `deal_cut`), or ending next turn (from `get_diplomacy`'s until-turns);
- a UN vote open, when `un` is the model's; a city the seat captured, when `captured_cities` is; a city-state event about the seat (`cs_ally_lost`, `cs_meet`, `cs_quest`), when `city_states` is; a `spy` event, when `espionage` is;
- the seat's score, or its military strength against a met civilization, changed by more than 25% since the last phase;
- `prompt_every` turns since the last phase.

At most one phase per turn per seat. A chat that comes in is not a trigger: the responder answers it whenever it comes (P3.6.5).

### P3.6.5 Answering chats

- A chat waiting on a hybrid seat goes to its bot first (inside drives, or `EngineGame.answer` from the responder): the bot answers what it owns and defers the rest.
- A deferred chat (its proposal touches a category of the model's, or it is talk and `chat` is the model's) goes to the seat's model through its responder queue (P3.5.3): a `ChatThread` on a `chat` ticket whose deadline is the clock's, with the diplomat prompt and the chat tools of the model's categories.
- When the ticket is refused or expires, the model's answer fails, or the clock runs out, the fallback bot answers (`EngineGame.answer(pid, nid, fallback)`) and the metrics record a fallback. On a machine held by a benchmark, the bot therefore answers at once (P3.3.3).

### P3.6.6 The diplomat phase

- **One `diplomat` ticket:** at most 30 s to start (a phase that cannot start soon is skipped and the bot's defaults stand), a 180 s budget once started (the plan's default; the seat's `hybrid.phase_seconds` overrides), at most 8 steps.
- **The system prompt** (`DIPLOMAT_SYSTEM` in `citar/agents/prompts.py`, the same for every call of a seat, so a provider can cache it): "You are the diplomat of {civ} in CITAR, a turn-based strategy game in the style of Civilization V. A scripted bot runs everything else for your civilization: its cities, research, units and the fighting of any war. You decide only these kinds of diplomacy: {the model's categories, one line each}. The bot decides {the bot's categories}. A deal that includes anything the bot decides must be one the bot would accept as a whole: if it would not, your offer or acceptance is refused and you are told why. You are called when something happens (first contact, a declaration of war, a deal running out, a United Nations vote, a large change in strength), every {n} turns, and whenever another civilization talks to you. Each call is short: read what you are given, act with the tools, then stop by answering without a tool call. Every negotiation entry needs a message. The other side answers in its own time; a chat closes after {minutes} minutes without an answer, or at {cap} messages. Keep your agreements and suspicions in your notebook (write_notes): it is shown at every call." Then the persona, if any.
- **The user message:** the triggers in words; the diplomatic situation (the seat's `get_diplomacy`, compacted: met civilizations with relations, treaties and their until-turns, open chats, recent messages, the UN); the empire at a glance (`empire_summary`); the notebook's last 2,000 characters; with the advisor on, "THE BOT'S VIEW" (`bot_advice`: deal values in gold from your side, war readiness per rival, spare luxuries, what it would ask for).
- **Tools:** `get_diplomacy`, `get_players`, `get_empire`, `get_victory_status`, `read_notes`, `write_notes`, `log_thought`, and per category the model owns: trades, agreements, peace → `open_negotiation`, `respond_negotiation`; war → `declare_war`; denounce → `denounce`; un → `un_vote`; city_states → `get_city_states`, `city_state_action`; espionage → `get_espionage`, `move_spy`, `stage_coup`; captured_cities → `get_cities`, `city_status`; chat → `send_message`, `respond_negotiation`. The session refuses any other tool the seat's model calls ("That is the bot's to decide.").
- **The veto on mixed deals** (plan 6.1). Before the seat's model opens a negotiation, or answers one with `counter` or `accept`, the session works out the proposal's categories. If any is the bot's, it asks `EngineGame.bot_evaluate(pid, bot, other, give, receive)` (a new binding of `citar_bot::evaluate`, 3-10); a value below zero refuses the call: "The bot decides <categories> and values this deal at <v> gold for you, so it will not agree. Leave those items out, or make the deal better for <civ>." What lies in the model's own categories is the model's to give.
- Tool calls go through `session.call_tool`, taking the lock per call; model calls are outside it.

### P3.6.7 Every place that asks "is this an LLM seat?"

`Seat` gains `model_seat` (llm, or hybrid with a model), `task_class` (`full` for llm, `partial` for hybrid, none otherwise) and `has_bot` (bot or hybrid).

| Place | Today | Phase 3 |
|---|---|---|
| `Seat.public`'s `llm_info` (`session.py:48`) | llm | model seats |
| `_track_turn`, `metrics_report` (`:212-221`) | llm's model | model seats; hybrid rows marked |
| `drive_bots` (`:257-261`) | bot seats | `has_bot` |
| `_dispatch_negotiation_interrupts` (`:378-384`) | bot, llm | bot, llm, hybrid, through the responder queues |
| `get_agent` (`:450-462`) | bot, llm | and hybrid: `HybridAgent` |
| `_quiet_hours`, `queue_machine`, `_make_way` (`:606, 651, 664`) | llm | full seats; a hybrid's tickets meet quiet hours themselves |
| `SessionManager.track` (`:1513`) | any llm | any model seat |
| `pool/seats.occupied` (`:58`) | llm | leases only (P3.3.5) |
| `pool/seats.authorize` (`:211`) | llm | by task class (P3.3.7) |
| `probes.py:829` | llm | model seats |
| `reports/data.py:78` | llm | model seats, with the class |
| `app.py:484` | llm's model | model seats, labelled |
| `scoring.py:47, 70, 94, 184` | llm | full seats only: hybrid games stay out of the model ranking (plan H4) |
| `usage.py:293, 303` | llm | leases for held time; every model seat listed with its class |
| `citar/web/js` (22 places: seat labels, lobby, seats dialog, metrics, models) | llm | model seats; "Hybrid: <bot> + <model>" |

### P3.6.8 The lobby and the game screen

- The seat types gain "Hybrid (bot plays, model negotiates)", with both pickers (the bot's profile and aggression; `llmForm` for the model), the category grid (ten bot/model toggles and the three presets), difficulty ("AI bonuses, like the bot" or "a person's values, like a model"), the advisor toggle and "prompt every N turns". The same in the Join/Seats dialog (`lobby.js:172-195`) and the scenario seats (`scenario.js`).
- The game screen's player list shows a hybrid seat's state: "bot playing", "diplomat thinking", "queued #n".

### P3.6.9 Benchmarks, probes and paired control games

- **Suites:** a scenario gains `mode: "full" | "hybrid"`, and for hybrid `owners` (a preset or a map) and `bot_profile` (default the standard profile). A hybrid job seats the model as a hybrid seat with the advisor off. Its prompts (diplomat phases and chat answers) are tickets with `wait_for_benchmark` and no start deadline, so a phase waits for its slot instead of being skipped after 30 s: a benchmark waits for its model rather than let the bot answer. The clock's model timeout still applies to a model that is reachable but silent; that fallback is recorded and counts against the model.
- **The paired control game:** every hybrid job has one, with the same configuration and seed and the hybrid seat played by the same bot profile with every category the bot's. Bot-only, it runs headless on the server's CPU (a Python loop over `EngineGame.drive` with the diplomacy recorder), once per (scenario, seed, build id, the bots' fingerprints), cached in the benchmark directory and shared by every model.
- **Probes:** a probe run gains `mode: "hybrid"` with `owners`. Offer and message cases go to the subject's bot first and to its model when the model owns them, and record which answered; turn cases play the bot's turn, then a diplomat phase with every trigger forced.

### P3.6.10 The diplomacy leaderboard

- **`citar/diplomacy_stats.py: DiplomacyRecorder`** follows one seat through a game's events: proposals made and received, each valued from the seat's side when it is put on the table (`bot_advice(...)["deal_value"]` with the seat's fallback bot; an accepted deal keeps the value of the proposal that was accepted); deals made; the acceptance rate of the seat's own proposals; wars declared by and on it; cities captured and lost; peace made; final score, alive, winner. Hybrid sessions attach it to their hybrid seats; control games to the same seat.
- A hybrid job's result carries the recorder's rows and its control's id. A pair's line: the score difference (hybrid minus control) at the end and at turns 100, 200 and 300 where reached, deals made, total and mean deal value, acceptance rate, wars and their outcomes.
- **`GET /api/models/diplomacy`** (members): per model and owners preset, the pairs, the mean score difference with a 90% bootstrap interval (2,000 seeded resamples), deals per game, mean deal value, acceptance rate, wars declared per game, captures and losses. Only benchmark pairs count (a lobby game has no control), only runs the caller may see, and pooled data under the minimum-contributor rule (P3.2.4).
- The Models page gets a "Diplomacy" tab beside the model scores.

### P3.6.11 Metrics

`metrics.data["diplomat"]`: one row per phase (pid, turn, triggers, steps, `gen_s`, `queue_s`, tokens, outcome `done|timeout|skipped|refused`, the tools called). A hybrid seat's turn rows carry the bot's action counts, as a bot seat's do.

---

## P3.7 3F: the minimap

### P3.7.1 Today

`game.js` draws the whole map into a 200x130 canvas with independent x and y scales (not hex-correct), the explored tiles where they lie, and maps a click to a tile without wrapping (`game.js:58-72, 998-1030`).

### P3.7.2 The engine's block (Rust, 3-13)

A major civilization's own view (`api/views/client.rs`, the seat's part) gains `minimap`:
- `stage`: `early`, `renaissance` or `information`, from the seat's era: at or past the era named "Renaissance era" it is `renaissance`, at or past "Information era" `information`. A ruleset without those names uses positions: the era at index ⌈n/3⌉ for the Renaissance and the second-to-last for the Information era (indexes 3 and 7 of the shipped nine).
- `capital`: `[x, y]`, or null.
- `home`: the box of the explored tiles of the home continent (the capital's; without a capital, that of the seat's first city or settler; none: null), `{x0, y0, x1, y1}` inclusive, with `x0 > x1` (or `y0 > y1`) when the shortest covering interval crosses a wrapping seam. Only tiles the seat has explored count, so nothing leaks.
- One pass over the continent's tiles, in seat views only (about 10k tiles on gargantuan). The god view and spectators are unchanged.
- `refcheck/intended.toml` gains `minimap-view-block` (group `views`, path `civs[*].view.minimap`, python `{ is = "absent" }`). Views are not in the digests, so `cargo golden check` is unchanged; the build id moves.

### P3.7.3 The window (`citar/web/js/minimap.js`, pure)

`computeWindow({explored, home, stage, capital, width, height, wrapX, wrapY, prev})`:
1. `explored` is the explored tiles' box (`exploredBox(tiles, width, height, wrapX, wrapY)`: the shortest circular interval on a wrapping axis), padded by 3 tiles, clamped on an axis that does not wrap;
2. capped by stage: `early`, on a map larger than 44x28, the 44x28 box centred on the capital (or the explored box's centre) intersected with the explored box; `renaissance`, the explored box intersected with `home` padded by 3; `information`, the whole map;
3. united with `prev` (the smallest box covering both, circularly), so it never shrinks. `prev` is kept in `localStorage` per game and seat (`citar:minimap:<gid>:<pid>`, every access in try/catch).

`fit(window, widgetW, widgetH)` gives one scale for both axes (a hex is √3 wide, rows are 1.5 apart) and the letterbox offsets; `toTile` and `toScreen` convert both ways, wrap-aware.

### P3.7.4 Drawing and moving

- The widget keeps its size (200x130) and zooms out as the window grows, never back in. The tile layer is drawn on an offscreen canvas when the view's tiles or the window change; the viewport box is drawn over it when the camera moves.
- A click centres the main map, a drag pans it, both through `toTile`.
- Human seats only: the client uses the bounded window when its seat is a person's (`view.seat.type == "human"`); a model seat's view carries the block too, harmlessly, and spectators and the recap keep today's whole-map minimap.

### P3.7.5 Tests

`citar/web/tests/minimap.test.mjs` (`node --test`): the shortest interval across a seam, the cap of each stage, never shrinking, the letterbox maths, a click mapped to a tile and back on a wrapping map. `citar/web/package.json` (`{"type": "module", "private": true}`) lets Node load `js/*.js` as modules; test.yml's front-end job adds `node --test citar/web/tests/`. Rust: testkit tests for the stage thresholds (the shipped ruleset and a renamed-eras one), the home box on wrapping and non-wrapping maps, a tile explored on another continent left out, a seat with no capital and no city.

---

## P3.8 3G: the machine settings dialog

### P3.8.1 Today

A helper's hello overwrites a machine's hardware and replaces its model list; there is no auto/manual choice, no record of the last scan, no capability model beyond LLM slots (three numbers, P3.0), no trust tier and no hardware class.

### P3.8.2 Data (in the baseline, P3.2.2)

- `servers.capabilities`: `{"llm": {"enabled": true, "allow_swaps": false}, "cpu": {"enabled": false, "threads": 0, "cpu_percent": 0, "memory_mb": 0, "work": []}}`. LLM slots are `max_concurrent`, edited in the same dialog. `cpu.work` is a subset of `lab_game`, `ranking_game`, `reference_game` (plan C1).
- `hardware_mode`, `models_mode`, `power_mode`; `hardware_detected` and `detected_at`; `gpu_class_id`, `cpu_class_id`, `class_override`; `trust_tier`.
- `servers.machine_settings(server_id)` returns what Phase 4's welcome and settings frames will carry (slots, swaps, the CPU limits). Phase 3's v1 hub sends nothing new; the scheduler reads slots and swaps from it.

### P3.8.3 A helper's hello

`record_connection` keeps the scan in `hardware_detected` and `detected_at` every time, then:
- **hardware** `auto`: applied (with `apply_hardware`'s source tag), and the machine is reclassified unless pinned; `manual`: `config.hardware` is left alone;
- **models** `auto`: merged (the reported models' labels, context and `loaded` updated; models not reported kept, marked not reported); `manual`: only the `loaded` flags of listed models are updated;
- **power** `auto`: the scan's suggested figures applied; `manual` (the default): left alone.

### P3.8.4 Routes

`GET/PUT /api/machines/{id}/settings` (owner or admin: capabilities, slots, the three modes), `POST /api/machines/{id}/hardware/apply` (owner or admin: copy the last scan, or one section of it, into the configuration), `PUT /api/admin/machines/{id}/trust` and `PUT /api/admin/machines/{id}/class` (admins).

### P3.8.5 The dialog

"Machine settings…" on each machine's card (P3.2.8), three tabs:
- **Capabilities:** LLM on or off, slots, "allow model swaps for pooled prompts"; CPU sharing with threads, CPU %, memory and the kinds of work, with a note that it takes effect with the 0.1.6 helper, that limits are hard where the OS allows, and that the helper's local settings always win.
- **Hardware, models and power:** an auto/manual toggle each; the last scan with its time and its differences from the configuration; "Apply".
- **Class and trust:** shown to the owner, editable by admins.

---

## P3.9 Cross-cutting rules

### P3.9.1 Access

- Every route added or changed in Phase 3 gets, in the package that touches it, a stranger test (another member on a private object: 404 for reads and writes; an anonymous caller: 401 or 404), an owner test and an admin test; routes of shareable objects add an ACL viewer's test (reads 200, writes 403).
- `scripts/audit_routes.py --strict` passes at the end of every package. A route meant to be public goes in `INTENTIONALLY_PUBLIC` with its reason (Phase 3 adds `/api/users/{handle}`, `/api/users/{handle}/avatar` and `/r/{slug}`); new body gates join `BODY_GATES` (`items.require(`, `profiles.require_visible(`).
- 404, not 403, whenever the caller cannot see the object (`Access.require`).

### P3.9.2 Locks, threads and the database

- No model call, disk write or database write under a session lock. The scheduler is never entered with a session lock held (an assertion in test mode). The ledger, the rollups and `ownership.sync` write from their own threads.
- Every wait has a timeout and notices a stopped session.

### P3.9.3 The standard gates

Every package's gates include, besides its own:
- **S1** the Python suite (`python -m unittest discover -s tests`), with the dev loop's extension built in the worktree's venv (`cargo xtask develop`) when Rust changed; its count recorded.
- **S2** ruff. **S3** `python scripts/audit_routes.py --strict`. **S4** `python scripts/check_links.py --strict`. **S5** when `docs/` changed: `mkdocs build --strict`, and `python scripts/build_wiki.py` with `wiki/` committed.
- **S6** the front-end checks: test.yml's ESM parse run locally (`node --check` on `.mjs` copies of `citar/web/js/*.js`), and from 3-09 on `node --test citar/web/tests/`.
- **S7** when Rust changed: `cargo fmt --check`; clippy `--workspace --all-targets --all-features --locked -D warnings`; nextest `--workspace --all-features --locked`; the doctests; `cargo xtask check`; `cargo refcheck run --fixtures refcheck/fixtures-mini --fixtures refcheck/fixtures-late --strict --with-bot`; `cargo golden check`.
- **S8** the package's part of `scripts/qa_phase3.py` (P3.9.4) exits 0, and its browser checklist items (P3.9.5) pass.
- **CI-only** (a worktree never pushes): test.yml on its three OSes with its package, installer and Docker jobs; rust.yml and determinism.yml when Rust changed. Nobody labels or dispatches nightly.yml or docs.yml without the owner's OK.

### P3.9.4 The QA script

- `scripts/qa_phase3.py` (3-02 writes the harness; each later package adds its part): `python scripts/qa_phase3.py <part>|all [--port N] [--dir D] [--keep]`.
- It starts `scripts/dev_server.py --server --registration invite` as a child on a free port, or `--port`, refused when it is 8799 or in use. The state directory must be new, empty or marked `.qa-phase3`, and `C:/dev/citar-play` is always refused, as `scripts/server_soak.py` does. It makes an admin and two members with `admin_cli create-user`, signs them in through `/api/auth/login` (cookies and CSRF), runs the part's checks over HTTP and the game socket, writes `<dir>/qa-<part>.json` (each check, its result, its numbers), stops the server (unless `--keep`), and exits 1 on any failed check.
- Model seats use the dry-run provider, taught what a part needs (a diplomat that proposes, votes and annexes; scripted chat answers). Helper machines use a real Python helper (`python -m citar.worker`) pointed at a small fake OpenAI-compatible endpoint the script serves with canned completions, so the hub, the hello, `Done.duration` and `CANCEL` are exercised without a GPU.
- Parts: `accounts` (3-01 to 3-04), `queue` (3-05, 3-06), `ledger` (3-07), `chat` (3-08, 3-09), `hybrid` (3-10 to 3-12), `minimap` (3-13), `machines` (3-14), and `all`.

### P3.9.5 The browser checklist

Run in the browser pane against the QA script's server (`--keep`), each item with a screenshot in the QA directory and ticked in the package's as-built note:
- **A1** a member's game shared with members appears for another member and not for a visitor; a private one is "not found" for the other member.
- **A2** the profile card: bio, links, an avatar upload, field visibility; the directory lists the member, and stops after "hide from directory"; a global profile opens signed out.
- **A3** the admin users page changes caps, limits and trust, and the audit log shows each change.
- **A4** one Servers page with a direct and a helper machine; an old `#/servers` link lands there.
- **C1** the Queue page shows leases and pooled prompts with positions; priority buttons only on your own work.
- **B1** the Account page's compute card; the profile's ratio line follows its visibility.
- **E1** an AI's chat pops up docked; a reply shows "thinking…" then the answer; a half-typed message and an open deal builder survive two updates; minimise; End Turn shows the wait and, armed, ends the turn when the answer comes; a timed-out chat shows "(no reply in time)".
- **D1** a hybrid seat set up in the lobby from a preset; the game screen shows "diplomat thinking"; a trade the model owns gets the model's answer, one the bot owns the bot's.
- **D2** the Models page's Diplomacy tab.
- **F1** the minimap: hex-correct proportions, the explored area only, growing and never shrinking, click and drag panning, across a wrapping seam.
- **G1** the machine settings dialog: a manual hardware entry survives a helper's reconnect; "Apply" from the last scan; the trust tier shown.

### P3.9.6 Docs

Each package updates the documents its behaviour touches, named in its scope: `docs/server/ACCOUNTS.md` (3-01 to 3-03), `docs/server/WORKERS.md` and `docs/CONFIGURATION.md` (3-04 to 3-07, 3-14), `docs/PLAYING.md` (3-09, 3-11, 3-13), `docs/AI_PLAYERS.md` (3-08, 3-10), `docs/BENCHMARKS.md` (3-12), `docs/API.md` (every new route), `docs/ARCHITECTURE.md` (3-05, 3-08, 3-10), `docs/REPORTS.md` (3-07); the wiki is regenerated with them.

### P3.9.7 Performance

A 4-bot small game through the session driver (`tests/test_server_drive.py`'s) within 10% of its time at `d93345a`; a seat's `/view` within 10% (the minimap block included); the scheduler's overhead per step under 1 ms with nothing queued; a session's chat clock under 1% of a core.

### P3.9.8 Process

As `ops/phase1-common.md`: a worktree per package (`.claude/worktrees/p-3-xx`, branch `p-3-xx`); `CARGO_TARGET_DIR=C:/dev/target/citar-p-3-xx` before any cargo command; files written with the Write tool (Git Bash mangles backslashes in heredocs); small commits ending with the Co-Authored-By line; never `ops/`, `saves/` or `.claude/launch.json`; never a push. Every package ends with its gates, then a review and a fix round (the owner's standing rule), and its as-built note records its start and end wall-clock times.

---

## P3.10 Decisions taken in this design

1. **Schema first.** One baseline revision holds every Phase 3 table and column; later packages fill in behaviour, and add a revision only if they must.
2. **Old databases are renamed aside** on SQLite (never deleted) and refused on Postgres; newer unknown revisions are refused.
3. **The ladder** is `private | allowlist | link | server | global`; `public` is gone; `server` needs no capability; `global` needs `publish_global` (the renamed `publish_public`). Moderators act on `server` and `global` objects.
4. **One `owned_items` table** for every file-backed job type, with `games` kept for live sessions and `reports` folded in; ACL entries keyed by kind. A report gathers only what its owner may see.
5. **Capabilities** `run_benchmarks`, `run_probes`, `run_lab`, `edit_bots`, `full_llm`, `partial_llm` join the five; lab and bot editing stay admin-only by default, as today.
6. **Profiles:** per-field visibility with `profile` as the page's own gate; members-only defaults; the ratio private by default; any non-probation member may make their own fields global.
7. **Avatars** are validated by header only (no image library), stored under the data directory, served with a fixed type and a sandboxing CSP.
8. **The registry merge** moves `config/servers.json` into the `servers` table behind `servers.py`'s unchanged functions; direct-reach machines and `POST /api/llm/models` are admin-only, except for the dry-run provider (the server must not fetch URLs members name).
9. **One LLM-slot number:** `Server.max_concurrent`, capped by the helper's local ceiling.
10. **The scheduler** is in-process, per machine; leases for full seats, tickets for partial prompts; lease requests before tickets; tickets by priority, then round-robin across members, then FIFO.
11. **Lending:** a lobby lease lends its slot between its seat's turns; a benchmark lease never lends, and no ticket starts on a machine a benchmark holds; tickets fail fast there unless they wait for benchmarks (reports, probes, hybrid benchmark jobs).
12. **Loaded models only** for tickets unless the owner allows swaps, or a live lease uses the model.
13. **Generation time** is the helper's `Done.duration` or the HTTP call's time after the slot was granted; queue time is recorded apart and never counted as work.
14. **Every model call goes through `make_conversation`**, report narratives included.
15. **The ledger:** `compute_events` with consumer and provider, null for the server; self-use (both the same, or both the server) excluded; credits at write time from hardware classes; hourly rollups.
16. **Default rates** are an energy proxy (GPU 1.0 per generation-second, CPU 0.05 per core-second, API 0); admins edit them.
17. **Budgets are in credits** and reserve and settle per request against the ledger.
18. **Costing** reads work from `compute_events` and held time and power from the JSONL ledger.
19. **The chat clock is the only chat timer;** lobby keys `chat_model_reply_seconds` (600) and `chat_person_reply_seconds` (300); at a deadline a model declines, a hybrid's bot answers, a person's chat closes as expired.
20. **Responders** run on one bounded executor with a queue per seat; a model seat keeps one conversation per chat.
21. **Late replies** are put in front of the model before its next step.
22. **`negotiation_status`** reaches the two parties and god-view spectators only.
23. **The chat window** is built on a pure state module tested with `node --test`; drafts and builder state live outside any redraw.
24. **The hybrid turn** is drive, stop, diplomat outside the lock, drive; a fallback bot handle with every category the bot's answers whatever the model cannot.
25. **The veto** on mixed deals is the bot's `evaluate()` of the whole deal through a new binding.
26. **One diplomat phase per turn at most,** on triggers and every N turns; 30 s to start, 180 s to run, 8 steps.
27. **The advisor** is on by default in lobby games and always off in benchmark games.
28. **Hybrid benchmark jobs** use tickets that wait for benchmarks and have no start deadline; each has a cached bot-only control game with the same seed.
29. **The leaderboard** counts benchmark pairs only, values deals with the seat's bot at proposal time, and honours visibility and pooling.
30. **The minimap's home region** is computed in the engine from explored tiles only; the window logic is a pure JS module; the previous window lives in `localStorage`.
31. **Machine modes:** hardware and models auto, power manual by default; the last scan always kept.
32. **QA** is a script on a dev server per feature (never port 8799 or `C:/dev/citar-play`), plus a browser checklist with screenshots.

## P3.11 Risks

1. **Scheduler starvation or deadlock.** Unit tests with a fake clock for every rule; every wait in ticks with a timeout; never entered under a session lock (asserted).
2. **Races between the chat clock and the agents.** The clock is the only timer and acts through engine calls that refuse a chat that has moved; agents only wait.
3. **The registry merge breaking benchmarks, costing or managed loading.** `servers.py` keeps its functions and shapes; an import fixture test; costing tested on a synthetic ledger before and after.
4. **Prompt volume on small GPUs** (KV-cache reuse lost, plan risk 6). A slim, static diplomat prompt; one phase per turn at most; triggers; tickets only on loaded models.
5. **Ledger write volume on SQLite.** A writer thread with batched inserts; rollups for reads.
6. **Avatar uploads as an attack surface.** Header-checked formats only, a size cap, a fixed served type, `nosniff` and a sandboxing CSP, storage outside the code and saves.
7. **Churn in `game.js` and `panels.js`.** The chat and minimap logic in pure modules with Node tests; the browser checklist.
8. **v1 helpers ignore `CANCEL`.** The scheduler puts a request the helper refuses as busy back at the head once; Phase 4 makes cancel real.
9. **The automatic archive of an old database.** Rename only, logged with the new path; Postgres refuses; the owner's port-8799 instance gets a fresh database on its first Phase 3 start (its saves on disk are untouched and re-indexed on load).
10. **Fifteen packages in sequence.** Each sized like 2-11 or smaller, with gates a reviewer can rerun locally, and wall-clock recorded.

## P3.12 Work breakdown at a glance

Fifteen packages, strictly in this order, one agent at a time. Each ends with its gates, a review and a fix round, and its as-built note.

| # | Package | Feature |
|---|---|---|
| 3-01 | The schema baseline, the visibility ladder, the games index | 3A |
| 3-02 | Owned items: every job type owned, gated and shareable; the QA harness | 3A |
| 3-03 | Profiles, the directory, avatars, the admin users page, the audit log | 3A |
| 3-04 | The registry merge: one machine table, one Servers page | 3A |
| 3-05 | The prompt scheduler: leases, tickets, the conversation wrapper, metering | 3C |
| 3-06 | Pooled work: task classes in admission, probes and reports on tickets, the Queue page | 3C |
| 3-07 | The compute ledger: events, credits, rollups, budgets, the ratio | 3B |
| 3-08 | Chat in the server: the clock, responders, status events, late replies | 3E |
| 3-09 | The chat window | 3E |
| 3-10 | Hybrid seats: the agent, the diplomat, the veto, the seat sites | 3D |
| 3-11 | Hybrid seats in the lobby, the seats dialog, scenarios and the game screen | 3D |
| 3-12 | Hybrid benchmarks and probes, control games, the diplomacy leaderboard | 3D |
| 3-13 | The minimap | 3F |
| 3-14 | The machine settings dialog | 3G |
| 3-15 | Phase 3 exit: the QA end to end, docs, evidence | all |

The order follows the plan's dependencies (3B after 3A; 3D and 3E after 3C) and two of its own: the ledger needs the scheduler's generation time, so 3C comes before 3B; the hybrid seat answers chats through the chat responders and clock, so 3E's server half comes before 3D. The minimap depends on nothing else and the machine settings only on the merged registry, the scheduler and the classes, so they come last, where they cannot hold up the rest.

## P3.13 Review log

(Filled in by the design review.)

---

## Appendix D: Phase 3 work breakdown

Each package is sized for one implementation session (2-11's size or smaller), starts from the previous package's merge, and ends with its gates passing, then a review and a fix round. The standard gates S1-S8 (P3.9.3) are named where they apply; every other gate is local and can be rerun by a reviewer. The QA script's parts and the browser checklist items are P3.9.4 and P3.9.5.

### 3-01: The schema baseline, the visibility ladder and the games index

- **Depends on:** nothing (`v0.1.6` at `d93345a`)
- **Estimated size:** ~900 Python (models 350, the baseline revision 450, access and boot 150) + ~500 tests + docs
- **Scope:** `citar/migrations/versions/`: delete `f064316a6b34` and `767c824c2fcb`; write `0016a_baseline` with every table and column of P3.2.2 (users' profile columns, `owned_items`, games' statuses, servers' new columns, `hardware_classes` with its three seeds, `compute_events`, `compute_rollups`, budgets in credits, `reports` dropped) and `citar/db/models.py` to match, including the ORM classes later packages fill in. `server/boot.py:migrate()` with the cases of P3.2.2 (an old revision renamed aside on SQLite with its `-wal`/`-shm`, refused on Postgres; an unknown newer revision refused; a version-less test database stamped). The ladder of P3.2.3: `access.VISIBILITY`, `access.check_visibility`, `server` and `global` in `access.on` and `visible_filter`, the moderator rule, the share URL, every validation site of P3.0 moved onto it; `publish_public` renamed `publish_global` in `CAPABILITIES`, `ROLE_DEFAULTS`, `PROBATION_DENIES` and the console (`console.js`). `User.public()` without `role` and `status`, `User.account()` for the owner and admins, every caller checked. The games index of P3.2.5: `ownership.sync` wired off the lock, statuses `setup|playing|finished|closed`, `_game_limit` on live games only, `register_existing` on load, `GameSession.owner_id`. The visibility choices in the web client where they exist. Docs: `docs/server/ACCOUNTS.md` (the ladder, the fresh baseline and what happens to an old database).
- **Gates:**
  1. `tests/test_migrations.py`: `upgrade head` on an empty SQLite database, then alembic's `compare_metadata` against `Base.metadata` finds nothing; the three hardware class seeds exist. A database made by the two old revisions (a small SQLite file built at `d93345a` before they are deleted, committed as `tests/data/citar-0.1.5.db`) is renamed aside with its `-wal` and `-shm`, and the boot comes up on a fresh one; a database at a made-up newer revision refuses to boot, naming it; a version-less database made by `create_all` is stamped; the Postgres refusal (a unit test on the dialect branch).
  2. `tests/test_access.py`: the ladder table (five levels by anonymous, member, probation member, suspended member, owner, ACL viewer, moderator and admin, by view, play and manage) and `visible_filter` per viewer; `global` needs `publish_global`, `server` needs nothing; `public` is refused as a value; `git grep` for a `"public"` visibility literal in `citar/` finds none outside the migration.
  3. The games index: a game ended with the `end_game` test operation is `finished` and no longer counts (a member with a limit of 1 starts another); closing a running game marks it `closed`; loading it marks it `playing`; a loaded save without a row gets one.
  4. `User.public()` carries no role or status; `/api/auth/me` still gives the owner both.
  5. S1-S5.

### 3-02: Owned items: every job type owned, gated and shareable; the QA harness

- **Depends on:** 3-01
- **Estimated size:** ~1,300 Python + ~250 JavaScript + ~800 tests + ~400 (the QA harness)
- **Scope:** `citar/items.py` and `access.on` for owned items (P3.2.4); registration in every producer of P3.2.4's table; system items; adoption at boot; the capabilities `run_benchmarks`, `run_probes`, `run_lab`, `edit_bots`, `full_llm`, `partial_llm` with their role defaults; every route of P3.2.4 gated and every listing filtered; queue priority gated; benchmark games registered as rows with the run's owner and visibility; the item sharing routes; reports wired (the runner writes rows, `share_api`'s report routes give way to the item routes, `/r/{slug}`, and `reports/data.py` gathers only what the report's owner may see, P3.2.4); model scores and the comparison honour pooling through `aggregate.scope_for`; `citar/web/js/share.js` used by games, reports, benchmark runs, probe runs, scenarios, maps and bot profiles; `admin_cli create-user`; `scripts/qa_phase3.py` with its harness (P3.9.4) and the `accounts` part's sharing checks. Docs: `ACCOUNTS.md` (ownership, sharing), `docs/API.md`.
- **Gates:**
  1. `tests/test_items_access.py`, table-driven over every route of P3.2.4 (the table is checked against the app's route list, so a gated-prefix route missing from it fails the test): another member on a private item gets 404 for reads and writes; an ACL viewer 200 for reads and 403 for writes; the owner and an admin 200; an anonymous caller 401 or 404; a `global` item's reads 200 anonymously where P3.2.4 allows, its writes 401.
  2. Every producer writes its row (per kind: created through its route, the row has the caller as owner; a delete sets `deleted_at`; listings filter); system items are `global` and refuse a member's write; adoption gives stray files to the first admin with one audit row.
  3. Control routes (benchmark run and job control, probe stop and delete, report rerun and delete, queue priority) by a non-owner give 404 or 403, by the owner and an admin 200; a lobby game's priority only by its owner.
  4. Pooling: a game whose owner opted out is absent from `/api/models/scores` and `/api/metrics/compare` for another member and present for its owner; a bucket under `aggregate_min_contributors` is withheld. A member's report built over "everything" contains none of another member's private games, runs or machine costs (their ids and names are absent from the HTML), and an admin's report contains them.
  5. Benchmark games have rows (kind `benchmark`, the run's owner and visibility), visible to the run's owner and not to another member.
  6. `python scripts/qa_phase3.py accounts` exits 0. The harness's refusals: port 8799, a port in use, a non-empty unmarked directory and `C:/dev/citar-play` each exit 2 with nothing touched. Browser A1.
  7. S1-S6.

### 3-03: Profiles, the directory, avatars, the admin users page and the audit log

- **Depends on:** 3-02
- **Estimated size:** ~800 Python + ~700 JavaScript + ~500 tests
- **Scope:** P3.2.6 and P3.2.7: the profile fields and their visibility; `PUT /api/auth/me` extended; avatars (`citar/auth/images.py`, the header validator; upload, serve, delete); `/api/users`, `/api/users/{handle}`; `/api/admin/users*`, `/api/admin/audit`; `admin_cli` `set-caps`, `set-limits`, `set-trust`, `audit`; the pages `#/users`, `#/u/<handle>` (open to visitors), `#/admin/users`, the Account page's Profile card; the nav entries; the route audit's new public routes and body gates. Docs: `ACCOUNTS.md` (profiles, the directory, the admin page), `docs/API.md`.
- **Gates:**
  1. `tests/test_profiles.py`: each field at each level is visible to exactly the viewers P3.2.6 names (anonymous, member, probation member, owner, admin); a hidden `profile` makes the page 404; `listed = false` removes the account from the directory only; global refused on probation; the bio and link limits; a `javascript:` link refused.
  2. Avatars, on a corpus in `tests/data/avatars/` (a valid PNG, JPEG and WebP; a PNG named `.jpg`; an SVG; HTML named `.png`; a truncated PNG; 257 KB; 2000x2000; a GIF): accepted or refused as the rules say; served with the validated type, `nosniff` and the CSP; 404 when the field is hidden; the previous file deleted on replace; nothing written outside `<data dir>/avatars/`.
  3. Admin: a PATCH changes each field and writes one audit row naming it; `null` removes a capability override; the last admin cannot demote or suspend themselves; a member gets 404 on every admin route; the audit route filters and pages.
  4. The CLI commands, each against a temporary database and seen through the API.
  5. `qa_phase3.py accounts` with its profile and admin checks exits 0; browser A2 and A3.
  6. S1-S6.

### 3-04: The registry merge: one machine table, one Servers page

- **Depends on:** 3-03
- **Estimated size:** ~1,000 Python + ~600 JavaScript + ~500 tests
- **Scope:** P3.2.8: `servers.py` on rows with its functions and shapes kept; the one-time import at boot; electricity plans in `settings_kv`; the host row; `max_concurrent` the one slot number; `admin_api`'s machine routes owner-or-admin; direct reach and `POST /api/llm/models` admin-only except dry-run; benchmarks' group resolution merged; `usage` and `costing` on rows; the Servers page with `servers.js`'s editors as dialogs; `#/servers` redirecting. Docs: `docs/server/WORKERS.md`, `docs/CONFIGURATION.md` (no more `config/servers.json`), the wiki.
- **Gates:**
  1. `tests/test_registry_merge.py`: a fixture `servers.json` (the host's LM Studio with models and load profiles, an Anthropic server with a key in the file keystore, a test server, electricity plans) imports at boot to rows whose `normalize_server()` dicts equal the file's entries (`max_parallel` carried into `max_concurrent`); the file is renamed; a second boot imports nothing; keys still resolve; `servers.host()` is the host row.
  2. Every public function of `servers.py` answers as before on the fixture: a table of calls (`find`, `get`, `list_servers`, `host`, `resolve_llm` for a direct seat and a helper seat, `seat_ref`, `describe_seat`, `model_entry`, `profile`, `restricted_now`, `server_name`) whose expected values are recorded from the file-backed code before the change; `costing` prices a synthetic ledger to the same totals.
  3. Routes: a member manages their own machine and gets 404 on another member's; creating or switching to a direct machine is refused for a member (403 with the reason) unless its provider is `dryrun`, and so is `POST /api/llm/models` with a base URL; admins manage every machine; electricity plans are admin-only.
  4. Benchmarks: a suite naming the host and a helper machine resolves both through rows, and a run's jobs start as before (the dry-run benchmark tests).
  5. `qa_phase3.py accounts` with a direct dry-run machine and a helper machine exits 0; browser A4.
  6. S1-S6.

### 3-05: The prompt scheduler: leases, tickets, the conversation wrapper, metering

- **Depends on:** 3-04
- **Estimated size:** ~1,300 Python + ~800 tests
- **Scope:** `citar/pool/scheduler.py` (P3.3.2-P3.3.6, P3.3.9): machines, capacity, leases, tickets, the rules and the order, position and ETA, cancel, listeners, status. `make_conversation(..., binding)` and `ScheduledConversation`; `WorkerConversation` keeping `duration` and its request id, `close()` calling the new `hub.cancel`; the busy requeue. `LLMAgent` on leases (`GameSession.lease_for`, the driver's `activate`/`idle`, chat answers on the lease), its busy branch removed, `_meter` replaced by the usage tracker's listener, `metrics.model_step` with `gen_s` and `queue_s`, the speed score on generation time. Report narratives through `make_conversation` (P3.3.4) on a non-lending `report` lease for now, and probe runs on a non-lending lease for the run, as today's claims; `claim()` and `release()` removed; `occupied()` from the leases; benchmarks starting jobs from `scheduler.leases`; `usage.session_servers` from the leases. Docs: `docs/ARCHITECTURE.md` (the scheduler), `WORKERS.md` (a busy machine queues).
- **Gates:**
  1. `tests/test_scheduler.py` with a fake clock and fake providers: P3.3.3's rules checked as invariants after every event of 500 seeded random schedules of leases, tickets, cancels and step times (in flight never above capacity; a lease request never waits behind a ticket started after it; no ticket starts while a benchmark lease is live; an active lease's slot free between its steps); priority, then round-robin, then FIFO on a constructed queue; an idle lobby lease's slot lent and given back within one ticket's step budget; expiry; cancelling a queued and a running ticket; a model not loaded refused unless swaps are allowed or a lease uses it; quiet hours refused with `retry_at`; the ETA rising with position.
  2. On the dry-run provider through a `servers` row with `max_concurrent = 1`: two lobby games with an LLM seat each play 5 rounds with never more than one request in flight (a listener records it) and no busy refusal. Through a real Python helper and the fake endpoint (the QA harness's): the hub never refuses busy, `gen_s` equals the helper's `duration`, `queue_s` is recorded, a cancelled ticket sends `CANCEL`.
  3. No busy-wait remains: `git grep -nE "busy_wait|refusal.*busy|sleep\(5 if"` in `citar/` finds only the scheduler's requeue and the hub's safety check (the gate lists the lines).
  4. A turn's model rows carry `gen_s` and `queue_s`; a model with long queue times and short generation scores as fast.
  5. `occupied()` lists exactly the live leases; a benchmark job waits for a lease slot; a probe run and a report hold their leases for their runs; `usage.session_servers` lists only leased machines.
  6. The scheduler adds under 1 ms per step with nothing queued (the median of 1,000 acquire-release pairs, in the test); P3.9.7's driver timing holds. `qa_phase3.py queue` (its lease checks) exits 0.
  7. S1-S6.

### 3-06: Pooled work: task classes in admission, probes and reports on tickets, the Queue page

- **Depends on:** 3-05
- **Estimated size:** ~800 Python + ~300 JavaScript + ~500 tests
- **Scope:** P3.3.7 and P3.3.8: the `pooled` purpose (and the Servers page's purpose list); seats authorized by task class (`full_llm` and `game`, `partial_llm` and `pooled`), classified by one function that 3-10's hybrid seats will also use; `update_seat` re-admitted; tickets admitted at creation. Report narratives become `report` tickets (`wait_for_benchmark`, a 30-minute start deadline) and leave `pool.queue`; probe offer and message cases become `probe` tickets and turn cases take `probe_case` leases; the Servers page's test becomes a `test` ticket. `/api/queue` with leases and tickets; `POST /api/queue/tickets/{id}/cancel`; the priority route's kinds; `queue.js`'s "Pooled prompts". Docs: `WORKERS.md` (the pooled purpose, lending), `docs/API.md`.
- **Gates:**
  1. Admission: a member without a `pooled` grant on a machine gets no ticket there (refused with the admission's reason); an LLM seat needs `full_llm` and a `game` grant, a seat classified partial needs `partial_llm` and `pooled` (the classifier tested on seat dicts of every type); `update_seat` to a machine the caller has no grant on is refused with 403 and leaves the seat as it was.
  2. Reports: a narrative on a machine whose lobby game holds an idle lease runs between that game's turns (the listener shows no overlap with the lease's active spans); on a machine a benchmark holds, it waits and runs after; no report holds a lease.
  3. Probes: offer and message cases run as tickets with no lease during them; turn cases hold a `probe_case` lease for the case only; the probe tests' results are unchanged on the dry-run model.
  4. `/api/queue` lists leases and tickets with positions, and labels activities the caller cannot see as "someone's <class>"; priority changes by owner or admin only; cancel by the activity's owner, the machine's owner and admins (a stranger gets 404).
  5. `qa_phase3.py queue` exits 0: two members' report narratives and probe cases on one machine beside a lobby game, served by priority then round-robin, none during the game's turns, in the evidence; browser C1.
  6. S1-S6.

### 3-07: The compute ledger: events, credits, rollups, budgets, the ratio

- **Depends on:** 3-06
- **Estimated size:** ~1,100 Python + ~300 JavaScript + ~600 tests
- **Scope:** P3.4: `citar/ledger.py` (the writer thread, owners of activities, credits, the classifier, the rollup thread, totals, the ratio); the scheduler listener; the usage tracker's CPU windows; the lab's results; hardware classes (admin API, the classifier, `class_override`); budgets in credits reserved and settled per request, the lease's pause and the ticket's refusal, `refresh_from_ledger`; `costing.py` reading `compute_events`, `usage.py` no longer writing busy, tokens and requests in spans; `/api/ledger/me`, `/api/users/{handle}/ratio`, `/api/admin/hardware-classes`; the admin users page's ratio column, the Account page's Compute card, the profile's ratio line; `citar admin reprice`. Docs: `CONFIGURATION.md` (credits and classes), `docs/REPORTS.md` (where costing's work figures come from), `ACCOUNTS.md` (the ratio), `docs/API.md`.
- **Gates:**
  1. `tests/test_ledger.py`: consumer and provider for each activity kind (a lobby game, a benchmark run, a probe run, a report, a lab experiment, a `collection` run) on a member's machine, the host and an online API; given and used per member equal a hand-computed table; self-use (the same member; the server on its own machine) on neither side; credits are seconds times the class rate at write time; a rate change applies from then on; `reprice` recomputes a period.
  2. Rollups: after random events over three days, every rollup equals the sum of its raw events; the 24h, 7d, 30d and all totals equal raw sums; a read in the current hour includes events not yet rolled up.
  3. Budgets (fake clock): with a hard budget of N credits on a grant, requests are admitted until spent plus reserved would pass N, then refused with the period's end; a lobby game on that grant pauses with the budget's reason and resumes when the period turns; a soft budget only warns; `refresh_from_ledger` rebuilds entries equal to the settled ones.
  4. Costing: a synthetic ledger priced before and after the switch gives equal totals with no queue time, and less busy energy with queue time; the report tests pass.
  5. The ratio's visibility: `private` gives another member 404 and an admin the totals; `server` gives members; `global` gives anonymous callers.
  6. 10,000 events written from 8 threads in under 2 s with no session lock held; the writer drains on shutdown.
  7. `qa_phase3.py ledger` exits 0: member B's helper machine serves member A's report narrative and lobby game, B's given and A's used match the events, and A's own machine's work counts on neither side; browser B1.
  8. S1-S6.

### 3-08: Chat in the server: the clock, responders, status events, late replies

- **Depends on:** 3-07
- **Estimated size:** ~900 Python + ~30 Rust + ~150 JavaScript + ~600 tests
- **Scope:** P3.5.2-P3.5.5 and P3.5.7: the lobby keys and `info()`; `ChatClock`; `citar/agents/responders.py`; `ChatThread` for model seats on their leases; `BotAgent` and `LLMAgent` waiting without budgets of their own; the late-reply note; `negotiation_status` from the clock, the scheduler's callbacks and the session, and the socket's filter; `GET /api/games/{gid}/negotiations/{nid}`; Rust: `respond_negotiation` requires `message`, with the tool list test's fix and the intended entry; the prompts; MCP; the probes on the new responder API; the recap's chat threads; the negotiation metrics. Docs: `docs/AI_PLAYERS.md` (chats and their timeouts), `docs/PLAYING.md` (the lobby's chat settings), `ARCHITECTURE.md` (the chat clock), `docs/API.md`.
- **Gates:**
  1. `tests/test_chat_clock.py` (fake clock): a chat waiting on a model seat past `chat_model_reply_seconds` is declined on its behalf with "(no reply in time)"; on a person past `chat_person_reply_seconds`, closed as expired; a 10-minute pause does not expire a 5-minute chat; a reply restarts the clock; an answer landing as the deadline fires leaves exactly one outcome; a game without the keys uses the defaults.
  2. A bot's turn that opened a chat with a person waits until the person answers or the clock closes the chat (an answer after 200 s is taken: no 90 s budget), then ends; a model seat's turn waits the same way; a session with a scripted person who never answers ends the turn at the timeout.
  3. Responders: 40 chats opened at once with 8 model seats run on at most `CITAR_RESPONDERS` threads, one chat at a time per seat, each answered; `_responding` is empty once all have closed; a model seat's second exchange in a chat reuses its conversation (one `make_conversation` per chat, counted by a fake provider).
  4. A person's reply during an LLM seat's turn, after the tool's wait, is in the model's next user message (a scripted conversation records it).
  5. Each party's socket receives `negotiation_status` (queued with a position, thinking, waiting with a deadline, closed) for its chat; a third seat's socket and a spectator of a game with a person in it receive none; a god-view spectator does.
  6. Rust: a `respond_negotiation` without `message` is refused by the schema check; S7 (refcheck clean, the golden sets unchanged, the tool list test passing with its new fix).
  7. MCP: `wait_for_turn`'s results carry deadlines; the probes' offer cases still pass.
  8. `qa_phase3.py chat` (the server half: chats between a person, a dry-run model and a bot, both timeouts set to 20 s) exits 0, with every status event seen in the evidence.
  9. S1-S7.

### 3-09: The chat window

- **Depends on:** 3-08
- **Estimated size:** ~1,200 JavaScript + ~300 Node tests + ~100 CSS
- **Scope:** P3.5.6: `chatmodel.js`, `chat.js`, the deal builder moved into the window, the socket's events opening and updating windows, End Turn's states and the armed end of turn, the Diplomacy screen trimmed with "Open chat"; `citar/web/package.json`; `citar/web/tests/chatmodel.test.mjs`; test.yml's front-end job running `node --test citar/web/tests/`. Docs: `PLAYING.md` (chatting, with a screenshot in `docs/assets/`).
- **Gates:**
  1. `node --test citar/web/tests/`: entries merged by `seq` with duplicates and out-of-order arrivals; drafts and builder state intact through 100 random updates; window state for your move, their move and closed, and for each status; End Turn's states (answer first, waiting with its countdown, armed, clear) from constructed sets of chats.
  2. S6's ESM parse of every module.
  3. Browser E1, step by step with screenshots, on `qa_phase3.py chat --keep`; `qa_phase3.py chat` still exits 0.
  4. S1-S6. CI-only: the test.yml change runs on GitHub at the merge push.

### 3-10: Hybrid seats: the agent, the diplomat, the veto, the seat sites

- **Depends on:** 3-09
- **Estimated size:** ~1,300 Python + ~120 Rust + ~700 tests
- **Scope:** P3.6.2-P3.6.7 and P3.6.11: `Seat.hybrid`, `SEAT_TYPES`, `normalize_hybrid` and the presets; the engine's controller, handicap and automatic decisions at creation, seat update and scenario launch; `HybridAgent` (the turn, the triggers, the diplomat phase, chats through the responders, the fallback); `DIPLOMAT_SYSTEM` and the diplomat's message; the tools per category, the session's tool filter and the veto; Rust: `Game.bot_evaluate` in citar-py, `EngineGame.bot_evaluate` in the facade, `normalize_seats` keeping `hybrid`; `Seat.model_seat`, `task_class`, `has_bot` and the Python half of P3.6.7's table; the diplomat metrics; the dry-run diplomat (proposes a trade to a met civilization, votes when a vote is open, annexes a captured city, answers chats as configured). Docs: `AI_PLAYERS.md` (hybrid seats), `ARCHITECTURE.md`, `docs/API.md` (the seat).
- **Gates:**
  1. For each of the ten categories, a seeded 4-seat small game with one hybrid seat whose model owns only that category (the dry-run diplomat acting in it where it can), 60 rounds: the bot takes no action of that category (from the drives' action counts and the events: no trade offer, no war declared, no spy moved, no city-state gift, no engine UN vote, no automatic annexation, as the category goes) and the model's actions in it happen. With every category the bot's and no phase triggered, the hybrid seat's events, turn by turn for 60 rounds, equal a bot seat's in the same game.
  2. Fallback: a diplomat or chat ticket refused (a benchmark holds the machine) or expired, or a model that fails: the fallback bot answers within the same drive or clock tick, and the metrics record it; a 100-round game whose model never answers ends every turn.
  3. Veto: the model's offer of a trade item plus an embassy (agreements the bot's), valued below zero by the bot, is refused with the message; valued above zero it goes through; a deal wholly in the model's categories is never vetoed.
  4. Triggers: each trigger of P3.6.4 fires one phase (a scripted game per trigger, `prompt_every` large); never two phases in a turn; `prompt_every` fires on schedule.
  5. Tools: a diplomat call outside its categories is refused with the message and counted; the system prompt is byte-identical across a seat's calls.
  6. Rust: `bot_evaluate` equals `citar_bot::evaluate` on the deals of refcheck's `deal_checks` for the committed states; `normalize_seats` keeps `hybrid`; S7.
  7. Every Python row of P3.6.7 has a test (`occupied` ignores hybrid seats; scoring leaves them out; usage lists them with their class; the summary labels them).
  8. `qa_phase3.py hybrid` (the server half: a hybrid seat on a dry-run machine shared with `pooled`, two bots and a person, 30 rounds, `prompt_every = 3`) exits 0, with phases, chat answers by the model and by the bot, and a fallback in the evidence.
  9. S1-S7.

### 3-11: Hybrid seats in the lobby, the seats dialog, scenarios and the game screen

- **Depends on:** 3-10
- **Estimated size:** ~700 JavaScript + ~150 Python + ~200 tests
- **Scope:** P3.6.8 and the JavaScript half of P3.6.7: the lobby's hybrid seat form with the category grid and presets, difficulty, the advisor and "prompt every"; the Join/Seats dialog; the scenario editor's seats; the 22 seat-label places; the game screen's hybrid states; the stats screen's diplomat rows; the seat route accepting hybrid. Docs: `PLAYING.md` (setting up a hybrid seat).
- **Gates:**
  1. A game created from the lobby's payload for a hybrid seat (each preset and a custom grid) starts with the right controller, handicap, automatic decisions and owners; a seat changed to hybrid and back to a bot through the route works, re-admitted each time.
  2. A scenario saved with a hybrid seat launches with it.
  3. A Node test of the seat label function ("Hybrid: <bot> + <model>" for each seat shape); browser D1 on `qa_phase3.py hybrid --keep`.
  4. S1-S6.

### 3-12: Hybrid benchmarks and probes, control games, the diplomacy leaderboard

- **Depends on:** 3-11
- **Estimated size:** ~1,000 Python + ~300 JavaScript + ~500 tests
- **Scope:** P3.6.9 and P3.6.10: suites' `mode: "hybrid"`; hybrid jobs (the advisor off, tickets that wait for benchmarks with no start deadline); the control games' runner and cache; `DiplomacyRecorder`; the jobs' results; `/api/models/diplomacy`; the Models page's Diplomacy tab; probes' hybrid mode. Docs: `docs/BENCHMARKS.md` (hybrid suites and the leaderboard), `docs/API.md`.
- **Gates:**
  1. A hybrid suite (one scenario, the dry-run diplomat, 3 seeds, 60 turns) runs 3 hybrid jobs and 3 control games; a second model's run of the suite reuses the 3 cached controls; a control game run twice gives the same event list.
  2. The recorder on a scripted game: proposals, deals, values, acceptance, wars, captures and peace equal a hand-made table; an accepted deal's value is its proposal's when made.
  3. The leaderboard on fixture results: pair lines and per-model aggregates (the mean score difference, the bootstrap interval with a fixed seed, deals per game) equal hand-computed values; a run the caller cannot see is left out; pooled data under the minimum is withheld.
  4. A hybrid job's tickets wait while a full benchmark holds the machine and run after it, never falling back for a start deadline (fake clock).
  5. A hybrid probe run records which side answered each case.
  6. `qa_phase3.py hybrid` with its benchmark checks exits 0; browser D2.
  7. S1-S6.

### 3-13: The minimap

- **Depends on:** 3-12
- **Estimated size:** ~150 Rust + ~600 JavaScript + ~300 Node tests + ~100 Python tests
- **Scope:** P3.7: the engine's `minimap` block with its testkit tests and the `minimap-view-block` intended entry; `minimap.js`; the game screen's minimap (offscreen tile layer, the viewport box, click and drag, `localStorage`); `citar/web/tests/minimap.test.mjs`. Docs: `PLAYING.md` (the minimap).
- **Gates:**
  1. Rust: the testkit tests of P3.7.5; S7, with refcheck `--strict` clean and the new intended entry used, and the golden sets unchanged.
  2. `node --test citar/web/tests/` with the minimap cases of P3.7.5.
  3. Python: a seat's view has `minimap` whose `home` covers only explored tiles of the home continent (checked against the view's own tiles); the god view and a spectator's view have none; a seat's `/view` within 10% of its time before, on the late fixture.
  4. `qa_phase3.py minimap` exits 0: a person's seat on a standard wrapping map, its block's stage moving from early to renaissance to information under scenario operations (`EngineGame.apply_ops`) that give the seat each era's techs, its home region only growing, each step in the evidence; browser F1.
  5. S1-S7.

### 3-14: The machine settings dialog

- **Depends on:** 3-13
- **Estimated size:** ~600 Python + ~500 JavaScript + ~400 tests
- **Scope:** P3.8: `record_connection` honouring the modes and keeping the last scan; `servers.machine_settings`; the settings, apply, trust and class routes; the dialog; the scheduler reading slots and swaps from `machine_settings`. Docs: `WORKERS.md` (what the helper's scan changes and what it does not), `CONFIGURATION.md`.
- **Gates:**
  1. `tests/test_machine_settings.py`, on `tests/test_worker.py`'s end-to-end harness: for each section and mode, a helper's hello with different hardware, models and power applies them in `auto` and leaves the hand-set values in `manual`, always keeping the scan and its time; models in `auto` merge and keep unreported ones; "apply" copies the scan, whole or per section.
  2. Classification follows the hardware unless pinned; the trust and class routes are admin-only (a member gets 404); the settings routes are for the owner or an admin (a stranger gets 404).
  3. Slots and swaps changed in the dialog change the scheduler's capacity and its loaded-model rule at once.
  4. `qa_phase3.py machines` (a real Python helper reconnecting with changed hardware) exits 0; browser G1.
  5. S1-S6.

### 3-15: Phase 3 exit: the QA end to end, docs, evidence

- **Depends on:** 3-14
- **Estimated size:** ~400 (the `all` part and its evidence) + docs
- **Scope:** `qa_phase3.py all` on one dev server: an admin and two members; a direct dry-run machine and a helper machine (a real Python helper and the fake endpoint) shared by member B with `pooled`; one game of member A's, shared with members, on a standard wrapping map: A's person seat (played by the script through the API and the socket), a hybrid seat (the standard bot and B's helper machine's model, `all_but_war`, `prompt_every = 3`) and two bots, 40 rounds, with A opening chats with the hybrid and a bot and answering the hybrid's offers, and the timeouts exercised; member B's report narrative and A's probe on the same machine interleaving with the game's prompts; the ledger's given and used checked against the events; the minimap's stages through scenario operations; a manual hardware entry surviving a helper restart; the server killed mid-game and the game brought back by `restore_live` and played on; the evidence JSON. The browser checklist A1-G1 in one pass with screenshots. The docs pass: `ARCHITECTURE.md` (Phase 3 in one section), `AI_PLAYERS.md`, `PLAYING.md`, `ACCOUNTS.md`, `WORKERS.md`, `BENCHMARKS.md`, `CONFIGURATION.md`, `API.md`, `REPORTS.md`, `KNOWN_ISSUES.md`, `CHANGELOG.md` (the Phase 3 features and the fresh database), `CONTRIBUTING.md` (the QA script), the wiki regenerated. "As built in 3-15" with each criterion of P3.1.4 and its evidence.
- **Gates:**
  1. `python scripts/qa_phase3.py all` exits 0, its evidence listing every check of every part.
  2. The browser checklist A1-G1 passes, with the screenshots listed in the as-built note.
  3. Each row of P3.1.4 is measured and recorded in "As built in 3-15".
  4. S1-S7 at the head; `mkdocs build --strict`; the wiki regenerated and committed.
  5. CI-only: test.yml, rust.yml and determinism.yml green at the merge push (the orchestrator's).
