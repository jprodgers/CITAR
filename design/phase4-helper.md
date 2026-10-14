# CITAR 0.1.6, Phase 4: the Rust helper, CPU sharing, proof of work (design)

**Status.** Design, 2026-10-14, for Phase 4 of `ops/plan-0.1.6.md` §7, written against `v0.1.6` at `1d5ca71` (Phase 3 complete and merged at `733345f`, with the CI fix `914abb8` and the side task "a closed connection ends a helper request quietly" on top). Sections are numbered P4.x, so they cannot be confused with DESIGN.md's Phase 1 (§1-§12), Phase 2 (P2.x) or `design/phase3-features.md` (P3.x). Appendix E is the work breakdown: thirteen packages, run strictly one after another, one agent at a time on this laptop (the owner's instruction, kept from Phase 3: "overall faster and had fewer problems"). Each package appends an "As built in 4-xx" note to the section it implements, as Phase 3's did, and records its wall-clock start and end.

This file lives in `design/`, outside `docs/`, so mkdocs and the wiki generator never see it. It names code paths in backticks rather than linking them, so `scripts/check_links.py` needs no change.

**Sources:**
- `ops/plan-0.1.6.md` §1 (R1, R4, C1-C4 and the trust-tier defaults under the table), §2 (the defects list), §4.5-4.8 (the faulty-helper QA), §6.6 (the ledger), §6.7 (protocol v2, the helper binary, CPU jobs and limits, proof of work), §7 Phase 4 (five parts and the exit) and Phase 5 (what packaging builds).
- `ops/review-0.1.6/pool-queue-workers.json` (the helper, the hub, packaging, the CPU-sharing and proof-of-work questions and their suggested defaults), `jobs-bench-lab-reports.json` (the lab as the job unit, verification, the Windows-only runner), `build-ci-tests.json`. Every finding used here was checked again against `1d5ca71`; the ones that no longer hold are in P4.1.5.
- `design/phase3-features.md`: P3.1.3 (Phase 4's hooks), P3.3.3 (draining), P3.3.4 (generation time bounded by observation, `flags.overreports`), P3.3.10, P3.4.2-P3.4.3 (`compute_events.verification`, the lab's CPU events), P3.8 and the as-built note of 3-18 (capabilities, modes, the last scan, `machine_settings`), P3.9 (the cross-cutting rules this design keeps), and 3-19's "Left for later".
- `crates/citar-engine/DESIGN.md` §2.1 (the `citar-helper` row), §4.10 (the digest and the chain), §9.6 (the determinism matrix), P2.2 (the crate graph, the build id), P2.4.1 ("a Python-free lab job is Phase 4's").
- `ops/p3-notes.md` (the CI and timing conventions: `perf_counter` for durations, `TZ=UTC0`, the Python 3.12 venv `C:/dev/venv/py312-check`, CI slack x3 for speed budgets, `fsutil.newest_first`, never sleep-then-assert) and `ops/phase1-common.md` (the build environment).
- The code each part touches (P4.0 cites it line by line), and a read-only probe of this laptop's WSL (Ubuntu 24.04, systemd 255 as PID 1, `systemctl --user` reachable for `claude`, cgroup v2 with `cpu memory pids` delegated to the user manager, NAT networking, Rust 1.98.1 in `~/.cargo/bin`, Python 3.12.3).

---

## P4.0 Facts the design leans on (checked against the tree at `1d5ca71`)

**The helper today is the Python worker** (`citar/worker/`, about 950 lines):
- **Protocol v1** (`citar/worker/protocol.py`): JSON text frames with `t`, `PROTOCOL_VERSION = 1` (`:39`) matched exactly by the server; worker frames `hello catalog done chunk error pong status`, server frames `welcome request cancel ping shutdown` (`:42-55`); refusals `busy closed unknown_model stopping` (`:59`); an 8 MB frame cap (`:61`). A request carries the whole message list, tools, `tool_mode`, `params`, a timeout and the activity (`:133-144`); `Done` carries text, thinking, parsed tool calls, `stop_reason`, `malformed`, usage and `duration` (`:148-158`). `CHUNK` and `STATUS` are never sent.
- **The agent** (`citar/worker/agent.py`): parses tool calls itself by reusing `OpenAIConversation` with `system=""` and the server's message list pasted in (`:301-341`), so a `json` tool mode's protocol prompt, which only `OpenAIConversation.__init__` appends (`openai_provider.py:44-47`), never reaches the model: the plan's "JSON tool mode is broken through helpers" still holds. Server `params` are whitelisted to `temperature max_tokens reasoning_effort top_p seed` (`:55`) and a configured model list is an enforced allowlist (`:234-243`). `CANCEL` is only logged (`:414-415`). Models are discovered once per connection (`:160-190`) and no catalog frame is ever sent. The hardware scan runs synchronously inside the async session before connecting (`:192-220, 348-353`). Reconnects back off from 2 s to 60 s with 30% jitter (`:47-50, 423-456`); a refused token stops it for good (`:434-440`).
- **Configuration** (`citar/worker/__main__.py`): `--config FILE`, else `helper.json` in the per-user base (`paths._user_base()`: `%LOCALAPPDATA%\CITAR`, `~/Library/Application Support/CITAR`, `$XDG_DATA_HOME/citar` or `~/.local/share/citar`; `CITAR_DATA_DIR` overrides; `:74-77`), a first-run prompt on a TTY that offers to save `helper.json` (`:80-104`), `CITAR_WORKER_*` environment variables, and flags `--server --token --config --base-url --provider --api-key-env --max-concurrent --name --model --quiet --insecure --no-hardware --hardware FILE -v` (`:156-186`); quiet hours as `22:00-06:00` or `Sat 00:00-09:00`, split at midnight (`:38-71`). `citar setup --worker` writes `worker.json` (mode 0600) under the configuration directory and, as root, a system unit `/etc/systemd/system/citar-worker.service` (`citar/wizard/worker.py:150-198`).

**The hub** (`citar/server/workers.py`, the `/ws/worker` route in `citar/server/app.py:2560-2650`):
- One `WorkerConnection` per machine, the newest replacing the older (`workers.py:258-272`); a request bridges from a game thread to the event loop and waits on a `threading.Event` (`:373-413`); `cancel` sends `CANCEL` and ends the caller's wait at once (`:215-229`); frames for resolved ids go to the scheduler as late frames (`:194-213`); `drop` on revoke, disable, delete or suspension sends `shutdown` and closes (`:326-356`); the ping loop checks every connection's token, machine and owner again each round (`:434-468`).
- **Defects found here.** (1) The `worker_auth` rate limit (`ratelimit.py:57`, 20 in 300 s) is recorded only after a failed lookup and its `RateLimited` is swallowed (`app.py:2658-2671`): it never refuses anything, and every attempt costs a token lookup. (2) `request_ip` takes the last `X-Forwarded-For` entry whatever `CITAR_TRUSTED_PROXY_HOPS` and `behind_proxy` say (`app.py:2652-2655`), where the HTTP routes use `ratelimit.client_ip` (`ratelimit.py:174-193`): a client with no proxy in front can choose its own address. (3) `WorkerToken.last_ip` (`models.py:519`) is never written. (4) The version check is exact and comes before authentication (`app.py:2580-2586`), which is right for a refusal that names the version.
- `record_connection` keeps each report as the machine's last report and applies it by its modes (`machine_settings.record_report`, 3-18; `workers.py:554-574`).

**The server's model calls** (`citar/agents/providers/worker_provider.py`): a `WorkerConversation` ships messages and unpacks the helper's parsed `Done` (`:92-161`); `close()` asks the hub to cancel (`:176-184`); the provider's tool-call parsing (native calls, `repair_tool_calls`, XML, GLM, plain text including Gemma 4's own syntax, the JSON protocol) is `openai_provider.py:83-336`.

**The scheduler** (`citar/pool/scheduler.py`) already has every hook P3.3.10 promised: capacity and swaps from `servers.machine_settings` (`:229-244`), the helper's state pushed by the hub (`helper_state`, `:247-262`), every request carrying its own id, and **draining** for a cancelled v1 request until its late frame or its timeout plus 15 s (`:46-53, 1025-1040`); the over-report rule (`:128`, `flag_overreports`).

**Machine settings** (`citar/machine_settings.py`, 3-18): `capabilities.cpu = {enabled, threads, cpu_percent, memory_mb, work}` with `work` a subset of `CPU_WORK = ("lab_game", "ranking_game", "reference_game")` (`:54`), 0 meaning "left to the helper" (`machine_settings.js:108-110`); hardware, models and power modes with the last report kept; `servers.machine_settings(id)` returns slots, swaps and the CPU limits "Phase 4's welcome and settings frames will carry" (`servers.py:773-792`). `servers.trust_tier` and `users.trust_tier` exist, admin-set (`models.py:99, 190-191, 496`); `servers.flags.overreports` is set by the scheduler.

**The ledger** (`citar/ledger.py`, 3-10): `compute_events.verification` is one of `unverified verified pending` (`models.py:720`); model work is `unverified`; `ingest_lab` reads the lab's results files and writes one `cpu` event per game **on the host**, `verified`, whoever played it (`ledger.py:1132-1236`).

**The lab** (`citar/lab.py`): experiments are files (`saves/lab/queue`, `results/NAME.jsonl`, `done/`); `game_spec(exp, i)` is the job unit (`:277-297`); `play()` drives `engine_api.run_game` and builds the result line in Python from the events it hears (`:314-406`); the runner is a separate process (`python -m citar.lab run`) that starts one Python subprocess per game (`:554-670`), adopts orphans through PowerShell and kills them with `taskkill`, Windows only (`:496-538`), honours the host's restricted hours (`:424-430`) and retries a crash three times. Results feed the ratings (`citar/bots/ratings.py`, Bradley-Terry over every result, cached on the files' signature, `:290-335`). The plan's `ranking_game` and `reference_game` have no runner: the dialog names them (`machine_settings.js:17-18`) and nothing else does.

**The engine side is ready for proof of work:**
- **The digest and the chain** exist (DESIGN.md §4.10; `crates/citar-engine/src/save/chain.rs`): `Digest = blake3("CITAR-DIGEST" ‖ CANON_V1 ‖ RulesetId ‖ canon(State))`; `chain_0 = blake3("CITAR-CHAIN" ‖ spec_hash)`, `chain_t = blake3(chain_{t-1} ‖ turn_le ‖ digest_t)`, taken at the end of each round, opt-in through `Game::set_chain` (`game/turn/driver.rs:163-172`), about 3 ms a round at gargantuan.
- **The build id** (P2.2.1): `citar_bot::build_id(rules)`, 12 hex digits over the engine's and the bot's content codes and the ruleset's id (`crates/citar-bot/src/lib.rs:245-260`); the same on every target, since it hashes sources, data and locked dependency versions.
- **The determinism matrix** already runs every golden set on five targets, macOS x64 included (`macos-15-intel`; `.github/workflows/determinism.yml:83-91`); the `bot` set holds each round's digest of bot games, so a target that plays differently shows there.
- **The runner** (`citar-sim`): `Runner` and `run_game` (P2.4.1), the baseline writer's `play_one(cx, spec) -> BaselineLine` (`baseline/play.rs:212`); citar-sim and citar-store are libraries "so the helper can link them" (DESIGN.md P2.1.2).
- DESIGN.md §2.1 reserves `citar-helper | bin | 4 | Links the engine and the bot`; P2.4.1: "A Python-free lab job is Phase 4's, when the helper needs one and its result format is redesigned with signatures"; §7.5: "Denormals flushed in the host process: citar-py gets a canary; if it fails, results are marked as not verifiable for proof of work".
- `cargo xtask check` holds each workspace crate to its row of the crate graph (`xtask/src/check/graph.rs:18-28`), the engine to its dependency allow-list (`deps.rs:17-29`), the shipped crates away from `test-ops` and `legacy`, and `crates/citar-py/src` free of `unsafe`; the workspace denies `unsafe_code` (`Cargo.toml`, `[workspace.lints.rust]`).

**Hardware and power:** `citar/hwinfo.py` (standard library, psutil when present; `collect()` `:354-395`, the `citar-hardware` format, `COLLECTOR_VERSION = 1`, `guess_power` `:327-344`), the no-Python collectors `citar/data/collectors/collect_hardware.{ps1,sh}`, and the host's `usage.PowerSampler` (`usage.py:405`), which writes one-minute power rows per machine that costing reads. Helpers send no power.

**Packaging:** the release workflow fails at its first step for any version up to 0.1.6 ("Release packaging is Phase 5", `release.yml:31-56`); its `build-helper` job bundles `citar.worker` with PyInstaller into four unversioned assets (`release.yml:186-243`); `citar/web/js/helper.js` links the same four from `releases/latest/download/<name>` and tells Intel Mac owners to use pipx (`:10-19, 67`); `pyproject.toml` has the `worker` extra and the `citar-worker` script (`:62, 93`); `install.sh --worker` and `install.ps1 -Worker` install the Python worker; the Homebrew formula installs the whole game (R4 says the tap ships only the helper; Phase 5).

**Docs:** `docs/server/WORKERS.md` ("What the server can and cannot do", `:299-311`), `SECURITY.md` (worker tokens in scope; no threat model for CPU jobs or updates), `KNOWN_ISSUES.md` ("A cancelled request still runs on a helper", "CPU sharing waits for the next helper", "There is no helper build for Intel Macs", "Energy on helper machines is estimated").

---

## P4.1 Goals, non-goals and exit

### P4.1.1 What Phase 4 delivers

1. **Protocol v2** (P4.2): one normative specification, typed frames on both sides, and conformance fixtures both sides run.
2. **The Rust helper** (`citar-helper`, P4.3): the same asset names plus Intel Mac, the same configuration files, flags and first-run prompt, a streaming LLM proxy whose cancel really stops generation and which returns raw completions, the hardware scan and power samples, CPU jobs under hard limits where the operating system allows, a machine key, signed results, signed self-update, and service installation per OS.
3. **Jobs in Rust** (`citar-jobs`, P4.4): a closed, typed vocabulary of bot-only work (lab games, ranking games, reference games), a Python-free lab game, the state-hash chain, canonical result hashes, signed claims, and one job host process that runs jobs under limits for both the helper and the server.
4. **The server side** (P4.5): the hub on v2 with tool-call parsing on the server, the job dispatcher with local and remote executors replacing the lab's Windows-only subprocess runner, results into the lab, the ratings and the ledger, trust tiers with replication, sampling, promotion, quarantine and withdrawal, the update relay, and the admin UI.
5. **The Python worker retired**, and a 0.1.5 helper told plainly to upgrade (P4.2.8).
6. **A threat model** (P4.6) written into `SECURITY.md`, and `WORKERS.md` rewritten.

### P4.1.2 Owner decisions that bind this phase

From the plan (§1) and the owner's instructions for Phase 4:
- **R1:** one workspace; `citar-helper` links the same engine and bot. **R4:** the Homebrew tap ships only the prebuilt helper (Phase 5).
- **C1:** CPU sharing is bot-only work in 0.1.6: lab games, bot rankings, reference games.
- **C2:** trust tiers: re-run until N = 5 match, then about 10% sampled, promotion to trusted after 50 verified jobs with no mismatch, a mismatch quarantines; results signed with ed25519 with a per-turn state-hash chain; LLM work attributed, not re-verified.
- **C3:** limits set on the site per machine, enforced by the helper, capped by a local ceiling (local always wins), hard where the OS allows.
- **C4:** a Rust helper replaces the Python worker with the same asset names plus an Intel Mac build; protocol v2 with raw completions parsed on the server; real cancel; job frames; the hardware scan with auto/manual per section; signed self-update; one machine = one row with LLM and CPU capabilities.
- **Priorities:** stability and speed first; fix bugs rather than carry them; no users yet, so protocol, schema and data may break freely; strictly one agent at a time; packages sized for one sitting with local, re-runnable gates; CI-only gates named.
- **Safety** (the owner's rules, P4.7.1): no system service, scheduled task, startup entry, system or security setting, or elevation on this laptop; never the owner's real helper configuration or data directory; never a private signing key in the repository; never a test helper against `citar.jimmieprodgers.com` or any non-local server; small, short CPU-limit tests; never port 8799 or `C:/dev/citar-play`; never load, unload or swap LM Studio's models or change its settings; never push; never the VPS; the main checkout is only read.

### P4.1.3 Non-goals

- **Phase 5:** the release workflow's gate removal, the signed manifest's real key and who holds it (an owner question), attestations and the dry run; the Homebrew tap formula for the helper; Scoop and winget; install scripts that download the helper; code signing and notarisation of the binaries. Phase 4 leaves the hooks (P4.10).
- **Phase 6:** the VPS, the owner's Framework Desktop switching to the new helper (the runbook is P4.2.8's).
- **Not in 0.1.6:** games with LLM seats on helpers (C1), model calls relayed from CPU jobs, GPU compute jobs, streaming completions to the server (v1's unused `CHUNK`), canary prompts for LLM work (optional in plan §6.7; the hook is a ticket class), several helpers on one machine row (the newest connection still wins), protocol v1 (P4.2.8).

### P4.1.4 Exit criteria

| Criterion | Measured by |
|---|---|
| The helper passes conformance on every target | The shared transcripts (P4.2.9) from the helper's side (`cargo nextest -p citar-helper -p citar-jobs`) and the hub's side (Python), and the end-to-end tests of the real binary against the real hub, on Windows x64 and on Linux x64 in WSL locally; `helper.yml` on macOS arm64, macOS x64, Linux arm64 (and again on Windows and Linux x64) CI-only; the `jobs` golden set on all five targets in `determinism.yml`, CI-only. |
| A remote lab experiment is verified end to end | `qa_phase4.py exit`: a server in WSL, a Windows helper and a WSL helper (each in its own home, under limits), a member's experiment granted `lab` plus ranking and reference games; every accepted result signed, its chain consistent, replicated or sampled as its machine's tier says, written once to the lab's results, rated, and in the ledger as given by the machine's owner with the right verification; the Windows and Linux results of replicated jobs equal byte for byte. |
| A faulty helper gets quarantined | The same run's third helper, built with `test-faults`: caught at its first altered result while `new` (evidence "same chain, different result"), and in a second pass caught by sampling after reaching `provisional`, its unverified results withdrawn from the results file, re-run elsewhere, their ledger events `rejected` with no credits, and the ratings recomputed without them. |
| The LLM proxy is real, and so is cancel | `qa_phase4.py real`: LM Studio's `google/gemma-4-e2b` (checked loaded before and after, never loaded, unloaded or swapped) through the Windows helper: a short game with an LLM seat and a hybrid seat, chats answered by the models' own calls parsed on the server; a long generation cancelled at 2 s is acknowledged within 1 s and the next short request takes no longer than the baseline measured in the same run; no slot drains. |
| CPU limits hold | Per OS (P4.3.6, P4.4.3; 4-07's and 4-09's gates): Windows' Job Object and Linux's cgroup scope cap CPU and memory hard; the fallbacks and macOS report themselves soft, and the dialog says so. |
| Updates are signed | `qa_phase4.py update`: a throwaway key generated at run time; an old build updates to a new one; every tampering is refused with the binary unchanged. |
| Services install per OS | WSL: `systemd --user` install, start, connect, stop, uninstall; the other OSes' generated definitions match expected files; Windows' real service and macOS launchd CI-only. |
| The Python worker is gone | No `citar.worker`, no `citar-worker`, no `worker` extra; a v1 hello gets the upgrade message and close code 4426. |
| Access is closed by default | `audit_routes.py --strict` 0 unguarded; every new route's stranger, owner, admin, moderator and visitor tests. |
| Nothing of the owner's was touched | S10 (P4.7.5) before and after every package: the fingerprints of the owner's data, no helper or job host left running, no service, task or startup entry anywhere on Windows, no `citar` unit left in WSL. |
| It all works together | `qa_phase4.py all` exits 0; the browser checklist (P4.7.7) passes with screenshots; S1-S10 green, recorded in "As built in 4-13". |

### P4.1.5 What changed since the plan was written

Each checked against `1d5ca71`:
1. **Much of §6.7's site side exists** (3-18): `capabilities.cpu`, the three modes, the last scan, classes, `trust_tier` on users and machines, `flags.overreports`, `machine_settings()`. Phase 4 adds the frames that carry them and the automation that acts on them.
2. **The chain and the cross-target matrix exist** (Phases 1-2), macOS x64 included. Proof of work needs a job runner that turns the chain on and a hash of the result, not new engine machinery.
3. **The lab's game is still played in Python** around `run_game` (P2.4.1 left it to Phase 4): 4-06 ports its result building to Rust, which the helper and the server's own executor then share.
4. **The lab runner is a separate process** with Windows-only orphan handling; the plan's "dispatcher replacing the lab's subprocess manager" becomes a dispatcher inside the server and a job host process per executor (P4.5.5-P4.5.6).
5. **The VPS starts Phase 6 on a fresh database** (plan §7 Phase 6), so the owner's 0.1.5 helper's token will not exist on the 0.1.6 server whatever the protocol: keeping v1 alive would buy that machine nothing (P4.2.8).
6. **Defects found on the way**, fixed in Phase 4: the worker-auth rate limit that never refuses, the socket address that trusts `X-Forwarded-For` unconditionally, `last_ip` never written (P4.0), the JSON tool mode through helpers (the plan's §2 item, still open), the catalog never re-sent, `ingest_lab` charging all lab CPU to the host as `verified`.
7. **`verification` has no `rejected`** (plan §6.6 lists it): added (P4.5.7).
8. **Python's standard library has neither ed25519 nor blake3**, and canonical JSON is easy to get subtly wrong across languages: hashing, claims, signatures and manifests are checked by the same Rust code on both sides, the server through `citar._engine` (P4.4.4).
9. **Ranking and reference games had no definition:** P4.4.1 gives them one.

---

## P4.2 Protocol v2

`crates/citar-helper/PROTOCOL.md` is the normative text (4-01); this section is its summary. The fixtures (P4.2.9) are its executable form.

### P4.2.1 Transport, framing and versioning

- **Transport:** the helper dials out to `wss://<server>/ws/worker` (`ws://` for loopback and plain-HTTP servers), as v1 did, so a v1 helper's attempt still reaches a server that can tell it to upgrade. One WebSocket per machine row; the newest connection wins (`shutdown`, code `replaced`, to the older).
- **One machine is one row** (C4): one helper process, one connection, one token and one key carry both capabilities, LLM and CPU; a machine with no model endpoint runs the helper with `--no-llm` and lends CPU only, and one that lends no CPU says so locally or on the site.
- **Frames:** UTF-8 JSON text frames, each an object with `t` (the type) and `ts` (the sender's Unix time, informational only). At most 8 MB a frame (v1's cap); a `job_result` is well under 1 MB.
- **Versioning:** `hello.protocol = 2`; the server accepts the protocols in `HUB_PROTOCOLS = (2,)`. Inside v2 a change must be additive: a new frame type or a new optional field. Both sides ignore frame types they do not know (logged once per type at debug) and fields they do not know. A breaking change is v3; a server may then accept `(2, 3)` for a release, and refuses older ones with `refused` code `protocol`, which a v2 helper handles by waiting for its own update (P4.3.8). **Job specs are the exception:** they are typed and closed (unknown keys refused), because they decide what code runs and what it computes (P4.4.1).
- **Ids:** request ids (16 hex, the scheduler's), job ids `cj_…` and run ids `cr_…` (the server's), all opaque to the helper.

### P4.2.2 The handshake

**`hello`** (helper → server, first frame, within 30 s of the connection):

| Field | Meaning |
|---|---|
| `protocol` | 2 |
| `token` | the worker token (never logged; v1's redaction) |
| `helper` | `{version, build, target, label}`: the helper's semver version, its engine build id (`citar_bot::build_id`), the Rust target triple, `git describe` or `unknown` |
| `machine` | `{hostname, name, os, arch, public_key}`: `name` the owner's `--name`; `public_key` the machine key, base64 of 32 bytes (P4.3.7) |
| `llm` | `{enabled, provider, slots, stream, models: [{key, label, context, loaded}]}`; `enabled` false for a CPU-only helper (`--no-llm`) |
| `cpu` | `{enabled, ceiling: {threads, cpu_percent, memory_mb, work}, limits: {cpu, memory}, verifiable, bots, ruleset}`: the local ceiling (P4.3.6), how hard this OS can hold each limit (`"hard"` or `"soft"` each), the denormal canary's answer, the bot versions compiled in, the ruleset id |
| `report` | `{hardware, hardware_source}`: the scan (P4.3.5) or the owner's file, as 3-18's report; `hardware_source` is `"scan"`, `"file"` or `"none"` |
| `running` | the run ids the helper is still playing from before a reconnect |
| `updates` | `auto`, `notify` or `off`: the helper's local policy |
| `quiet_until` | when the helper's own quiet hours in force end, or null |

**The server's answer** is `welcome` or `refused`, then nothing else until the helper has its welcome (3-05's ordering, kept).

`welcome`: `{protocol: 2, machine: {id, name}, settings, ping_s, server: {version, build}, update, jobs: {keep: [...], drop: [...]}}`: `settings` as P4.2.3; `update` the signed manifest the server relays, or null (P4.3.8); `jobs` says which of the helper's `running` runs are still assigned to it (the others it aborts).

`refused`: `{code, message, retry_after_s?}`; the server closes the socket after it. The helper's reaction is part of the contract:

| `code` | Why | The helper |
|---|---|---|
| `token` | unknown or revoked token, machine disabled or deleted, owner not active | stops for good, saying to issue a new token (`citar-helper setup` asks for one) |
| `key` | the machine already has another key registered | stops, saying the owner must reset the machine's key on the Servers page (P4.5.1) |
| `protocol` | the server does not speak v2 (a 0.1.5 server answers with a v1 `error` frame and close code 4426, which the helper reads the same way) | waits: retries every 10 minutes, logging that the server must be upgraded or its own update applied; never exits on it |
| `rate` | too many refused tokens from this address | retries after `retry_after_s` |
| `build` | reserved for v3 | as `protocol` |

**Server-side checks, in order** (P4.5.1): the address's rate limit (before any lookup), the protocol, the token, then the key.

### P4.2.3 Settings, catalog, hardware, power and status

- **`settings`** (server → helper, in `welcome` and pushed on every change of the machine's settings): `{llm: {enabled, slots, allow_swaps}, cpu: {enabled, threads, cpu_percent, memory_mb, work, yield_to_llm}}`, from `servers.machine_settings(id)`. `0` for a CPU figure means "the helper's ceiling".
- **`settings_ack`** (helper → server, after each `settings`): `{effective: {llm: {enabled, slots}, cpu: {enabled, threads, cpu_percent, memory_mb, work, yield_to_llm}}, capped: [field, ...], limits: {cpu, memory}}`: what the helper will actually do, each figure `min(site, ceiling)`, `capped` naming where its ceiling won. The scheduler's capacity and the dispatcher's threads read `effective`, never the site's figures alone (C3: local always wins).
- **`catalog`** (helper → server, whenever the endpoint's model list or loaded set changes; the helper polls it every 30 s): `{models: [...]}` as in `hello.llm.models`. 3-18's `record_report` applies it by the models mode, and the scheduler's loaded-model rule follows it at once.
- **`hardware`** (helper → server, when the scan changed, and after a `rescan`): `{scan, source}`. Kept as the last report and applied by the hardware and power modes (3-18, unchanged).
- **`rescan`** (server → helper): scan now (the dialog's "Rescan now", P4.5.11).
- **`power`** (helper → server, once a minute when it can measure anything): `{samples: [{t, gpu_w, cpu_util}]}`, from 10-second readings. Written as the machine's power rows (`usage`'s `power-<machine>-YYYY-MM.jsonl` format, P4.5.7); they are measurements, so the power mode (which governs the static figures) does not gate them.
- **`status`** (helper → server, on change, at most every 5 s): `{quiet_until, llm_in_flight, jobs_running, jobs_paused}`. The scheduler treats a machine whose helper is in its own quiet hours like one in the site's quiet hours (tickets refused as temporary with `retry_at`), instead of learning it from `closed` refusals as v1 forced it to.

### P4.2.4 Completions

- **`request`** (server → helper): `{id, model, messages, tools, params, timeout_s}`. `messages` is the whole OpenAI-format list, system prompt included (and, in `json` tool mode, the JSON protocol appended by the server: the fix of the plan's defect); `tools` OpenAI function definitions, or null; `params` at most `temperature max_tokens top_p seed reasoning_effort` (anything else is dropped by the helper, logged once). The helper decides the endpoint, the key and whether to stream; nothing in a request can change them.
- **`done`** (helper → server): `{id, message: {content, reasoning, tool_calls: [{id, type, function: {name, arguments}}]}, finish_reason, usage: {prompt_tokens, completion_tokens, reasoning_tokens, cached_tokens} | null, gen_s, ttft_s, streamed}`. The completion as the endpoint returned it, assembled from the stream when streaming: **nothing is parsed or repaired on the helper** (P4.5.2). `gen_s` is from sending the HTTP request to its last byte; `ttft_s` to the first token; `usage` null when the endpoint gave none.
- **`error`** (helper → server): `{id, code, message, retryable}` with `code` one of `busy` (at its local slots), `closed` (local quiet hours), `unknown_model` (outside its allowlist), `stopping`, `endpoint` (the local endpoint failed or refused), `timeout`, `bad_request`.
- **`cancel`** (server → helper): `{id}`. The helper drops the HTTP response stream, which closes the connection to the endpoint (LM Studio, Ollama, llama.cpp and vLLM stop a streaming generation when its client goes), and only then answers **`cancelled`** `{id, gen_s, tokens_out}`. A request that finished before the cancel arrived has had its `done`; the cancel is then ignored and nothing more is sent.

The server frees the request's slot when `cancelled` (or a crossing `done` or `error`) arrives. Draining stays only as the safety net for a helper that does not acknowledge within `CANCEL_ACK_SECONDS` (5 s): the slot drains as in P3.3.3 and the machine gets `flags.cancel_unacked` (P4.5.3).

### P4.2.5 Jobs

| Frame | Direction | Fields |
|---|---|---|
| `job_offer` | server → helper | `job, run, kind, spec, spec_hash, build, deadline_s, priority` |
| `job_accept` | helper → server | `run` |
| `job_reject` | helper → server | `run, reason`: `busy`, `build` (not its engine build), `kind` (not in its local work kinds), `quiet`, `disabled`, `limits` (the job host could not be started under the limits), `bad_spec` (refused by validation, or its hash differs), `unverifiable` (the canary failed) |
| `job_progress` | helper → server | `run, round, of, elapsed_s, cpu_s`: every 10 s or 25 rounds; it is also the run's heartbeat |
| `job_result` | helper → server | `run, outcome: {result, result_hash, chain: {head, rounds, digests}, cpu_s, wall_s, peak_mb}, claim, signature` (P4.4.4) |
| `job_failed` | helper → server | `run, code, message, retryable`: `code` `memory` (the memory limit ended it), `deadline`, `host` (the job host died), `aborted_locally` (the owner stopped CPU sharing), `internal` |
| `job_abort` | server → helper | `run, reason` |
| `job_aborted` | helper → server | `run` |

A game that panics is a **result**, not a failure: its outcome is the crash record (P4.4.2), which a replica must reproduce. `job_failed` is for the infrastructure. The same frames, without `claim` and `signature`, are the job host's own protocol on its standard input and output (P4.4.3), so the helper relays them and signs results, and the server's local executor speaks them directly.

### P4.2.6 Updates

- **`update`** (server → helper, when the server learns of a manifest while the helper is connected): `{manifest, signature}`, as `welcome.update`.
- **`update_status`** (helper → server): `{state: "available"|"downloading"|"verified"|"applied"|"failed"|"refused", version, message}`. Shown on the Servers page.

### P4.2.7 Keep-alive and shutdown

- `ping`/`pong` every `ping_s` (20 s); a connection is stale after 70 s without a frame (v1's figures). The helper sends its own ping when it has heard nothing for 45 s, so it notices a dead link without waiting for the server.
- **`shutdown`** (server → helper): `{reason, code}` with `code` `revoked`, `disabled`, `deleted`, `suspended`, `replaced`. The helper stops and does not reconnect, aborting its jobs and requests (v1's behaviour for the first four; `replaced` means another process holds the machine's token).

### P4.2.8 The v1 transition

**A 0.1.6 server refuses a v1 helper; it does not serve it.** On a hello whose `version` is 1 (or absent), the hub answers with a v1-shaped `error` frame and closes with code 4426, without looking up the token (nothing to rate-limit, nothing to write):

> This server runs CITAR 0.1.6, which needs the new CITAR helper (protocol 2). Download it from the Servers page of this server, or from https://github.com/jprodgers/CITAR/releases/latest, and run it with a token issued there.

The 0.1.5 helper already handles this well: its `Rejected` path logs "The server rejected this worker: <message>" and stops instead of retrying (`agent.py:434-440`), so it does not hammer the server. **The other order works too:** a v2 helper whose server is still 0.1.5 receives v1's "This server speaks worker protocol v1..." error and close 4426, reads it as `refused` code `protocol`, and waits, retrying every 10 minutes, so it connects by itself once the server is upgraded. Either side can move first; in between, that machine is simply offline, and nothing half-works.

**Why not accept v1 for a transition window:**
1. **It would buy the owner's machine nothing.** Phase 6 starts the VPS on a fresh database (plan §7), so the Framework Desktop's 0.1.5 token does not exist on the 0.1.6 server; the machine must be registered again and given a new token whatever the protocol, and downloading the new helper is one more step in the same sitting.
2. **It would keep two protocols alive in the hub:** parsing on the helper beside parsing on the server, draining as the normal cancel, `busy` as a routine answer, and a second set of tests, all to serve one machine for a few minutes.
3. **There are no other users** (plan §1, revised), and the plan's own rule is to fix rather than carry.

**The runbook for Phase 6** (into `docs/server/WORKERS.md` in 4-12): deploy the server; register the machine (or keep its imported row) and issue a token; on the machine, stop the 0.1.5 helper, download `citar-helper-<target>` from the Servers page, run `citar-helper setup` (it reads the old `helper.json`'s endpoint, models and quiet hours, and asks only for the new token); optionally `citar-helper service install`.

### P4.2.9 Conformance fixtures

- **Where:** `crates/citar-helper/protocol/v2/`: `frames/valid/*.json` and `frames/invalid/*.json` (one frame each, an invalid one with the reason it must be refused), and `transcripts/*.json`.
- **A transcript** is a named sequence of steps: `{"s2h": frame}` (the server sends), `{"h2s": pattern}` (the helper must send a frame matching it next), `{"endpoint": {...}}` (the fake model endpoint's next behaviour: a scripted response, a stream of deltas with delays, or an expectation such as `expect_disconnect_within_ms`), `{"host": {...}}` (the fake or real job host's), and `{"state": {...}}` (an assertion on the receiving side's state, read by that side only: for the hub, "the slot is free and nothing drains"; for the helper, "no request in flight"). Patterns use a small matcher language: `"<any>"`, `"<int>"`, `"<number>"`, `"<string>"`, `"<hex64>"`, `"<base64>"`, `"<id:NAME>"` (binds a value and requires it equal wherever the name recurs), `"<golden:SET/NAME>"` (a value from a committed golden file).
- **Both sides run every transcript.** The Rust side (`crates/citar-helper/tests/conformance.rs`) plays the server's frames and the endpoint from an in-process fake server and checks every frame the real helper code sends. The Python side (`tests/test_helper_conformance.py`) plays the helper's frames from a `FakeHelper` against the real hub (uvicorn, a real WebSocket) and checks the hub's frames and its scheduler's and dispatcher's state. A transcript is therefore checked from both ends; a change to the protocol changes a fixture, and both suites must agree with it.
- **Both sides hold the same frame table:** PROTOCOL.md's table of frame types is parsed by a test on each side and compared with the types each side's code knows.

---

## P4.3 The `citar-helper` crate

### P4.3.1 Structure and dependencies

- **Crate:** `crates/citar-helper`, a library (`citar_helper`, so tests drive it in-process) and the binary `citar-helper`. Modules: `cli`, `config`, `home` (the directory layout), `log` (redacting), `session` (the WebSocket, handshake, reconnect, dispatch of frames), `protocol` (every frame as a serde type), `llm` (discovery, catalog, the proxy), `scan` (hardware and power), `jobs` (the job host's parent side, signing), `key`, `update`, `service`, `quiet`.
- **The crate graph** (`xtask/src/check/graph.rs`): `citar-jobs: [citar-engine, citar-bot, citar-sim]`; `citar-helper: [citar-engine, citar-bot, citar-sim, citar-jobs]`; `citar-py` gains `citar-jobs`. `citar-store` is not needed by the helper (jobs keep no saves) and stays off its row until something needs it.
- **Dependencies** (workspace entries, each with its reason in the manifest): `tokio` (rt-multi-thread, net, time, process, io-util, sync, signal, fs), `tokio-tungstenite` with rustls, `reqwest` with rustls and streaming, `rustls-platform-verifier` (the OS's own trust store: no CA bundle hunt as in `agent.py:58-87`, no OpenSSL), `serde`, `serde_json`, `clap`, `ed25519-dalek` and `getrandom` (in citar-jobs), `blake3` (workspace), `sysinfo`, `base64`, `semver`, `chrono` (local quiet hours), `thiserror`, `self-replace` (the update's swap on every OS), and per OS `windows-service` and `windows-sys` (Windows), `libc` (Unix). No C toolchain beyond what rustls' crypto backend builds with.
- **`unsafe`:** the workspace keeps `unsafe_code = "deny"`. The only `unsafe` in Phase 4 is in `crates/citar-jobs/src/limits/sys/{windows,unix}.rs` (Job Objects; `setpriority`, `setrlimit` where no safe wrapper does it), each block with a `// SAFETY:` comment, allowed for that module alone; `cargo xtask check` gains a rule refusing an `unsafe` token anywhere else in `citar-jobs` and anywhere in `citar-helper`.
- **Features** (none shipped): `test-faults` (P4.4.6), `test-update-key` (P4.3.8), `test-jobs` in citar-jobs (the `burn` and `alloc` job kinds the limit tests use). `cargo xtask check` refuses them in citar-py's dependencies except as a forward of citar-py's own `test-ops`, in `pyproject.toml`'s maturin features, and on any `cargo build` line of `release.yml` and `helper.yml`'s artifact job. **A helper built with any test feature dials loopback addresses only** (the server and the update URL; compile-time), so no test build can reach a real server.

### P4.3.2 Configuration, compatibility and isolation

- **The home.** `--home DIR` (or `CITAR_DATA_DIR`, as Python) is the per-user base: `helper.json` at its root (v1's place), and the helper's own state under `helper/`: `machine-key.json`, `logs/helper.log` (rotated at 5 MB, three kept), `update/` (staging), `scan.json` (the last scan and its hash). Without either, the platform's per-user base (P4.0). The helper writes nothing outside its home (and the service definitions P4.3.9 writes on request).
- **Compatibility.** Every v1 way of configuring keeps working with the same meaning: `helper.json`, `--config worker.json` (any path), the `CITAR_WORKER_*` variables, every v1 flag, the quiet-hours syntax, the first-run prompt on a TTY (server, token, "remember these in …?"), "Press Enter to close" when a double-clicked binary fails. 4-03's gate 1 holds this against a table recorded from `citar/worker/__main__.py:build_config` before 4-04 deletes it.
- **New settings** (flags and `helper.json` keys): `--no-llm`; `cpu: {enabled, threads, cpu_percent, memory_mb, work}` (the local ceiling: defaults enabled, threads = logical CPUs − 1, 100%, half the RAM, every kind; `"cpu": {"enabled": false}` refuses CPU work outright); `updates: auto|notify|off` (default `auto`); `stream: true|false` (default true); `--ca-file FILE` (trust a server's own CA, the right answer to a self-signed server); `--no-power`. `--insecure` is kept for compatibility but honoured only for loopback and private (RFC 1918, ULA, `.local`) hosts, with a warning.
- **Subcommands:** `run` (the default), `setup` (the first-run questions, also to replace a token), `scan [FILE]` (print or write the hardware report: the standalone collector), `service install|uninstall|status|run` (P4.3.9), `update check|apply`, `job-host` (internal: the job host, P4.4.3), `--version` (version, build id, target, protocol).
- **Isolation in tests** is a rule (P4.7.1): every test and QA run passes `--home` to a fresh directory under its own scratch or QA directory, and sets `CITAR_DATA_DIR` to it as well.

### P4.3.3 The connection

- A single tokio runtime. The session task owns the socket: it reads frames, routes each to its handler (requests to the proxy, jobs to the job side, settings to the limits), and writes through one channel, so a slow handler never blocks pings. Scans, key generation and file reads run on `spawn_blocking`.
- **Reconnect:** 2 s doubling to 60 s with 30% jitter (v1's), reset by a welcome; the `refused` reactions of P4.2.2; a TLS failure is logged with the host and the verifier's reason, never relaxed.
- **On each connect:** discover models (P4.3.4), read or scan hardware (cached; P4.3.5), send `hello` with `running`, then follow the welcome's `jobs.drop`.
- **Local policy, enforced here whatever the server asks** (v1's principle, kept): the model allowlist, the local slots, the local quiet hours (no new request or job starts; running jobs finish), the CPU ceiling, the update policy.

### P4.3.4 The LLM proxy

- **Endpoints:** `lmstudio` (`/api/v0/models` for load state and context, else `/v1/models`), `ollama` (`/v1/models`, and `/api/ps` for what is loaded), `openai_compatible`, `openai` (a key from `api_key_env`, read locally, sent only to the configured base URL). The configured base URL and key are the only ones the helper ever dials for completions.
- **Streaming by default:** `stream: true` with `stream_options.include_usage`; the helper assembles content, reasoning (`reasoning_content` or `reasoning`, whichever the endpoint uses), and tool calls from their deltas (by index: id, name, arguments concatenated), and keeps `finish_reason` and the final usage. `stream: false` (or an endpoint that refuses streaming with tools, detected once and remembered for the connection) sends one non-streaming request; its cancel then closes the connection as well, but whether the endpoint stops is its own business (logged). Streaming is chosen for cancel, not for the server: the server still receives one `done` (P4.1.3).
- **Real cancel:** each request is a task holding its response stream; `cancel` aborts the task, which drops the stream and closes the TCP connection (reqwest never reuses a connection whose body was not read to the end); `cancelled` is sent after that, with the tokens received so far.
- **Slots and refusals:** `busy` beyond the local slots, `closed` in local quiet hours, `unknown_model` outside the allowlist, `timeout` when `timeout_s` (capped at 900 s locally) passes, `endpoint` for a connection or HTTP error from the endpoint (with its status and the first 300 characters of its body).
- **Timing:** `gen_s` and `ttft_s` from `Instant`; the server bounds them by what it observed (P3.3.4, unchanged).

### P4.3.5 The hardware scan and power samples

- **`scan`** produces the `citar-hardware` format `hwinfo.collect()` produces (same keys; `collector_version` 2, `collector: "citar-helper"`), so 3-18's `valid_scan`, `clean_hardware`, classes and the dialog read it unchanged: `sysinfo` for the OS, CPU model, cores, threads, RAM, disks and volumes; GPUs from `nvidia-smi --query-gpu` (names, memory, power limit, whether power is readable), on Windows `Get-CimInstance Win32_VideoController` through PowerShell (as `hwinfo.py` does, so the logic and the answers match), on Linux `lspci` and `rocm-smi`, on macOS `system_profiler SPDisplaysDataType SPHardwareDataType -json` and unified memory; the form (a battery means a laptop); runtimes (LM Studio and Ollama answering locally); `power.suggested` from a port of `guess_power`. Every probe is best-effort with a timeout, as in `hwinfo.py`: less information, never an error.
- **Auto or manual, per section, is the site's choice** (3-18, unchanged): the helper always reports, the machine's hardware, models and power modes decide what a report may change, and the last report is always kept for "Apply". The helper's own manual is to report a file or nothing.
- **When:** at connect (a cached scan younger than 6 hours is reused), every 6 hours, and on `rescan`; a `hardware` frame only when the scan's hash changed. `--hardware FILE` reports the file instead (read again at each connect, UTF-16 accepted, v1's rule); `--no-hardware` reports nothing.
- **Power:** every 10 s, `nvidia-smi --query-gpu=power.draw` where power is readable and the CPU utilisation from `sysinfo`; one `power` frame a minute. `--no-power` turns it off. Machines without a readable GPU power report CPU utilisation only, and costing keeps using their figures for the rest.

### P4.3.6 CPU jobs on the helper

- **The job host is a child process,** `citar-helper job-host` (P4.4.3), started when CPU work is enabled (site and ceiling both), with the effective limits (P4.2.3), and restarted between jobs when the limits change. Its standard input and output carry the job frames; the helper relays them to and from the server, checks them, and signs results (P4.3.7). Running the games in a child keeps a crash, an out-of-memory kill or a hung game away from the connection, and gives every OS one process to put its limits on.
- **How each OS holds the limits** (C3: "hard where the OS allows"):

| OS | CPU (`cpu_percent` of the whole machine) | Memory (`memory_mb`, all jobs together) | Reported |
|---|---|---|---|
| Windows | the job host puts itself in a Job Object with `JOB_OBJECT_CPU_RATE_CONTROL_ENABLE` and `HARD_CAP`, `CpuRate = percent x 100`, at `BELOW_NORMAL_PRIORITY_CLASS` | the same Job Object's `JobMemoryLimit`, with `KILL_ON_JOB_CLOSE` | hard, hard |
| Linux with a reachable user manager | the parent starts it under `systemd-run --user --scope --quiet --collect -p CPUQuota=<percent x CPUs>% -p MemoryMax=<MB>M -p TasksMax=<threads + 16>`; the job host reads its own cgroup's `cpu.max` and `memory.max` to confirm | as left | hard, hard |
| Linux otherwise | `nice 10` and the thread cap only | `RLIMIT_DATA` (private writable memory: `RLIMIT_AS` would count the address space glibc reserves per thread) | soft, hard |
| macOS | background priority (`PRIO_DARWIN_BG`, efficiency cores on Apple silicon) and the thread cap | a watchdog in the job host reads its resident size every 500 ms and ends the jobs past the limit | soft, soft |

- **`threads`** is the most games at once (one thread each); the server never offers more, and the host refuses more (`busy`).
- **`yield_to_llm`** (a site setting, default on): while any model request of this helper is in flight, the job host is told `pause` and stops every game at its next seat step (each well under a second), then `resume`. A GPU machine lending its spare CPU does not slow its own model, and benchmark timing stays clean (the server also offers no CPU job to a machine while a benchmark lease is live on it, P4.5.5).
- **Deadlines:** the helper aborts a run past its `deadline_s` itself (`job_failed` `deadline`), so the server's own timeout is only the backstop.
- **Reconnects:** a run continues across a lost connection; the next `hello` lists it in `running`, and the welcome says whether to keep it. A result produced while disconnected is kept (in memory, and in `helper/results/` until acknowledged) and sent after the reconnect.

### P4.3.7 Keys and signing

- **The machine key** is an ed25519 key generated on first run from the OS's random source (`getrandom`), stored in `helper/machine-key.json` (`{"v": 1, "public": base64, "secret": base64}`), written with mode 0600 on Unix; a key file readable by group or others is refused at start, as SSH does. On Windows the per-user base's ACL already keeps it to its owner. The secret never leaves the machine: it is not in any frame, log or scan.
- **Registration:** the first v2 hello of a machine registers its public key on the row (trust on first use, bound to a valid token); a later hello with another key is refused (`key`) until the owner or an admin resets the machine's key (audited, P4.5.1). A token issued again keeps the key. The Servers page shows the key's fingerprint (the first 16 hex of blake3 of the public key), which `citar-helper --version` also prints, so an owner can check it is their machine.
- **Signing:** the helper signs each claim the job host's outcome yields (P4.4.4) before relaying the result. Signing in the parent keeps the key away from the job host, which runs the games.

### P4.3.8 Self-update

- **The manifest** (`citar-helper-manifest.json`): `{v: 1, version, protocol, build, released_at, assets: {<target>: {name, url, sha256, size}}}`, the five targets of P4.10. It is signed as a whole: `citar-helper-manifest.json.sig` = ed25519 over `"CITAR-HELPER-MANIFEST-V1\0" ‖ the manifest's bytes as published`. Both files are release assets beside the binaries.
- **Trusted keys:** the helper embeds the public keys in `crates/citar-jobs/update-keys.pub` (one base64 key per line, up to two, for rotation). **In Phase 4 that file is empty:** no real key exists yet, and a build that trusts no key reports updates `unavailable` and never downloads anything. Phase 5 adds the owner's public key (the owner question on custody, P4.10). Only a build with `test-update-key` also trusts a key given at run time (`--update-key FILE`), and only such a build reads `CITAR_HELPER_VERSION` at compile time to call itself another version.
- **Where the manifest comes from:** the server relays it (`welcome.update` and `update`); the helper does not poll GitHub. The server fetches the manifest **of its own version** (`helper_manifest_url`, default `https://github.com/jprodgers/CITAR/releases/download/v{version}/citar-helper-manifest.json` and its `.sig`; or `helper_manifest_file` for tests), at most hourly, off any request path, and relays it only if it verifies against the same keys (P4.5.9). Helpers therefore follow the server they serve: same engine build, same protocol.
- **The helper's steps,** under its local policy (`off`: ignore; `notify`: report `available` and stop there; `auto`):
  1. verify the signature against the trusted keys; refuse otherwise (`update_status refused`);
  2. refuse unless `version` is newer than its own (no downgrade, so a server cannot push an old signed build with a known flaw), and unless the manifest has its target;
  3. refuse a URL that is not `https`, or `http` to a loopback host (tests);
  4. download into `helper/update/` and check `size` and `sha256`;
  5. run the staged binary with `--self-test` (it starts as far as loading its embedded ruleset and running the job canary, then prints its version, build and target, which must equal the manifest's): the guard against a binary that cannot run on this machine;
  6. wait until idle (no request or job in flight; at most an hour, then between two jobs) and swap the binary with `self-replace`, keeping the old one as `<name>.old`;
  7. **a console run** starts the new binary and stays until it reports itself healthy (its first welcome, or 60 s of running while the server cannot be reached); if it exits or hangs first, the old process swaps itself back from `.old`, reports `failed` and keeps serving. **A service run** exits with code 75 and the service manager starts the new binary (the self-test of step 5 is its guard; `<name>.old update rollback` puts the old one back by hand). The new process reports `applied`.
- **Who signs** is Phase 5's (P4.10): `cargo xtask keygen --out DIR` (refuses a directory inside the repository), `cargo xtask helper-manifest --assets DIR --version V` and `cargo xtask sign-manifest --key FILE MANIFEST` (refuses a key file inside the repository or known to git) exist from 4-11, so whichever custody the owner chooses has its tools. (xtask signs with `ed25519-dalek` directly; its row of the crate graph stays empty, and the bytes it signs are the manifest file as published, so no shared type is needed.)

### P4.3.9 Service installation

`citar-helper service install|uninstall|status|run [--user|--system] [--dry-run]`. Every definition runs the helper with an explicit `--home` and the absolute path of the binary; `--dry-run` prints what would be written and run, and changes nothing.

| OS | `--user` (default where possible) | `--system` |
|---|---|---|
| Linux | `~/.config/systemd/user/citar-helper.service` (`Restart=always`, `RestartSec=15`, `NoNewPrivileges=yes`), then `systemctl --user daemon-reload` and `enable --now`; it prints the `loginctl enable-linger` line for running without a login, and never runs it | `/etc/systemd/system/citar-helper.service` with a `User=`; needs root, and says how rather than elevating |
| macOS | `~/Library/LaunchAgents/org.citar.helper.plist` (`RunAtLoad`, `KeepAlive`, logs into the home), `launchctl bootstrap gui/<uid>` | not offered |
| Windows | a value `CITAR helper` under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` running `citar-helper run --detach --home …` (no window; logs to the file) | a Windows service (`windows-service`; `service run` is its entry), started automatically, home in `%ProgramData%\CITAR\helper`; installing it needs an elevated prompt, and an unelevated `install --system` prints the elevated command and exits 2 |

The service's restart is the update's restart (exit 75, P4.3.8). On this laptop only `--dry-run` runs on Windows, and real installs run in WSL with `systemd --user` (P4.7.1); the Windows service and launchd install for real in CI only (P4.7.10).

### P4.3.10 What the helper never does

It never runs a command, a script or code it was sent; never reads or writes a file outside its home (and the service files it was asked to write); never dials anything but its server, its configured model endpoint and, for an update, the signed manifest's asset URL; never loads, unloads or swaps a model; never changes a system setting. It runs only the engine and bots compiled into it, on specs that are data (P4.4.1). These are the claims `WORKERS.md` makes to a lender and `SECURITY.md` defends (P4.6).

---

## P4.4 Jobs and proof of work (`citar-jobs`)

A library crate shared by the helper and the server's bindings: the job vocabulary, the Rust lab game, the job host and its limits, hashes, claims and signatures, and the update manifest's verification. Everything that must give the same bytes on both sides of the wire lives here, once.

### P4.4.1 Job kinds and specs

| Kind | What it is | Spec | Result |
|---|---|---|---|
| `lab_game` | one game of a member's (or an admin's) lab experiment | `LabSpec`: `lab.game_spec(exp, i)`'s keys, typed (`exp, i, seed, map_type, size, speed, turns, difficulty, barbarians, barbarian_difficulty, nation, city_states, ai_base_values, map_edges, river_density, resources, seats[{label, bot, difficulty, params, aggression, profile, profile_rev, profile_name, levels, nation}]`) | `LabResult`: today's result line, P4.4.2 |
| `ranking_game` | a game of a ranking experiment: the site's own ladder (an experiment with `kind: "ranking"`, admins only, always `collection`) | `LabSpec` | `LabResult` |
| `reference_game` | a game of a reference run: the statistical baseline the bots are compared against (an experiment with `kind: "reference"`, admins only, `collection`) | `ReferenceSpec`: citar-sim's baseline `Options` and the index `i` | `BaselineLine`, the line `citar-sim baseline` writes and `summarize.py` reads |

- **Closed and typed:** specs deserialize with `deny_unknown_fields`; bounds are checked (players within the ruleset's limits, a known size, map type, speed and difficulty, `turns` at most the speed's time-victory turn, bot versions compiled in, overrides through `citar_bot`'s own `clean`, strings at most 200 characters, `i` below 1,000,000). A spec is data: no path, URL, code or ruleset text. The kinds differ only in who may queue them and which machines opt in (`capabilities.cpu.work`), so an owner may lend CPU to the site's ladder without lending it to members' experiments.
- **`spec_hash = blake3("CITAR-SPEC-V1\0" ‖ kind ‖ "\0" ‖ canonical bytes)`**, where the canonical bytes are the typed spec re-serialized by serde (fixed field order, maps as `BTreeMap`). The server computes it when it creates a job (through the binding); the helper and the job host recompute it and reject a mismatch (`bad_spec`).

### P4.4.2 The lab result in Rust

- `citar_jobs::lab::play(spec, control) -> LabOutcome` ports `lab.play()`'s result building (`lab.py:314-406`): the players by `str(pid)` with label, bot, difficulty, alive, levels, profile and revision, build, version, fingerprint, clean params, fixed and played aggression, score and share, rank, techs, cities, policies, religion, great people, spaceship parts, unhappy share, checkpoints at 50…300 with the twelve stat keys, era turns, the event counts and the 25 most built items; the winner, its label, the victory and the turns. Integer maps are `BTreeMap`, so the JSON is the same bytes on every target.
- **The deterministic part** is everything above plus `exp`, `i`, `seed`, `map` and `engine` (the build id). `seconds`, `started_ts`, `cpu_s` and `finished` are measurements, outside the hash, added by whoever writes the line.
- **Crashes:** a panic caught by the runner is the outcome `{crash: {turn, player, label, message}}` (the message only; the backtrace is logged, not hashed: frame addresses differ between builds of one source). It is final: recorded in `crashes.log`, never retried (a Rust panic is a bug, not bad luck; 0.1.5's three retries were for a Python process dying), and a replica must reproduce it.
- `lab.play()` in Python becomes a call of `engine_api.run_job("lab_game", spec)` (4-06); the Python result building and its event listener go.

### P4.4.3 The job host and its limits

- **One program, two parents:** `citar_jobs::host::main(stdin, stdout)` is the job host; the helper runs it as `citar-helper job-host`, the server as `python -m citar.jobs host` (which calls it through the extension). Its protocol is the job frames of P4.2.5 as JSON lines, plus `pause`, `resume` and `limits` (sent once at start) from the parent, and `ready {limits: {cpu, memory}, verifiable}` and `heartbeat` to it.
- **Inside:** at most `threads` runs, each on its own thread with its own `Runner`; between seat steps each run checks its abort flag, the pause flag and its deadline, so abort and pause take effect within one seat step; progress every 10 s or 25 rounds; the canary (`black_box(f64::MIN_POSITIVE) / 2.0 != 0.0`) at start, and `verifiable: false` refuses every job (`unverifiable`) rather than producing results nobody could check.
- **Limits** (`citar_jobs::limits::apply`) are applied by the host to itself at start, per the table of P4.3.6 (Windows: a Job Object it creates and joins, legal since Windows 8's nested jobs; Linux: confirms the scope its parent made, else `RLIMIT_DATA` and `nice`; macOS: background priority and the watchdog). The parent side (`citar_jobs::host::spawn` for the helper, `citar/jobs/host.py` for the server) adds the `systemd-run --user --scope` wrapper on Linux when `systemctl --user` answers, and reads `ready` to learn what holds.
- **Ending:** standard input closing (the parent died) aborts every run and exits within 2 s, so no game outlives its parent: this replaces 0.1.5's orphan adoption. A run the memory limit ends shows as the host dying (Windows' Job Object and Linux's OOM kill end the whole host): the parent reports `job_failed memory` for the run that was allocating most (the host's heartbeat carries each run's peak) and `host` for the others, which are retried.

### P4.4.4 Hashes, the chain, the claim, the signature

- **`result_hash = blake3("CITAR-RESULT-V1\0" ‖ kind ‖ "\0" ‖ serde_json bytes of the typed deterministic result)`.** The server never hashes Python JSON: it passes the received result through the binding, which parses it into the typed result (refusing unknown keys), re-serializes it and hashes, and compares with the claim.
- **The chain** (the plan's "per-turn state-hash chain": one digest per round, after every seat's turn of it): every job game runs with `set_chain(DigestChain::new(spec_hash))`; the outcome carries the chain's head, its round count and every round's digest (64 hex each: about 21 KB for a 330-round game). The server checks that the digests fold to the head from `chain_0` (`chain_check` in the binding); replicas compare heads, and on a mismatch the first differing digest names the round where two machines parted.
- **The claim** (typed, fixed field order): `{v: 1, job, run, machine, kind, spec_hash, build, bots, result_hash, chain_head, rounds, key}`, `bots` the sorted bot versions the spec names, `machine` the id the welcome gave, `key` the public key. **`signature = ed25519(secret, "CITAR-CLAIM-V1\0" ‖ the claim's bytes)`.** The run and machine ids inside it make a signature useless for any other run; the spec hash and build bind it to exactly what was asked for.
- **Server-side verification** (`engine_api.verify_claim(claim, signature, public_key, result, chain)`): the signature against the registered key, the claim's job, run, machine, kind, spec hash and build against the server's records, the result's hash, the chain's fold. Any failure rejects the run and quarantines its machine (P4.5.8): a correct helper never produces one.
- The server's own executor's results are not signed: it is the server (P4.5.6).

### P4.4.5 Determinism the proof stands on

- Every job's game is the engine's ordinary game under `set_chain`; the engine's determinism rules (the keyed RNG, `libm`, defined iteration orders, the lint against hash iteration) and the five-target matrix already hold it.
- **A `jobs` golden set** (4-06; `crates/citar-testkit/golden/jobs.json`): four short jobs (two lab duels of 30 rounds, a small four-bot lab game capped at 40 rounds, one reference game capped at 40), each with its spec hash, chain head, rounds and result hash. `cargo golden check` checks it with every other set; `determinism.yml` compares it across the five targets; WSL checks it locally (S9). A target that computes another result fails there before any volunteer's machine does.
- Results are only compared within one build id; a job offered to a helper of another build is rejected (`build`), and the dispatcher never compares across builds.

### P4.4.6 Faults for testing

`citar-helper --features test-faults` adds `--test-fault MODE`: `result:N` (from the Nth job, alter one score in the result and recompute its hash, so the claim is validly signed and the content wrong: the cheater), `digest:R` (alter round R's digest and fold the chain from it: broken hardware), `sig` (sign with another key), `cpu:F` (report F times the CPU seconds), `abandon:N` (accept the Nth job and never finish it). Each mode's detection is a gate of 4-10 and the exit.

---

## P4.5 The server side

### P4.5.1 The hub on v2

- **`/ws/worker`** keeps its path (where v1 helpers dial, so they can be told to upgrade). Its order: (1) the address (`ratelimit.client_ip`, honouring the trusted hops: defect P4.0 (2)) is checked against `worker_auth` **before** any lookup, and an address over it is `refused rate` with `retry_after_s`; (2) the protocol: v1 gets P4.2.8's message and 4426; (3) the token: a failure counts toward the limit and is audited (as today); (4) the key (P4.3.7); (5) registration (`admit`, 3-05's double check, unchanged), `welcome`, then `record_connection` off the loop. `WorkerToken.last_ip` and `last_seen_at` are written on success.
- **Frames from the helper** are handled on the event loop without blocking it: completions resolve their pending requests (as today); `cancelled` frees the slot (P4.5.3); `catalog`, `hardware` and `power` go to `asyncio.to_thread` writers (`record_report`, the power rows); `settings_ack` and `status` update the connection and call `scheduler.helper_changed`; job frames are put on the dispatcher's queue (P4.5.5), never handled on the loop.
- **Settings are pushed:** the settings route (`PUT /api/machines/{id}/settings`) already refreshes the scheduler; it now also calls `hub.push_settings(id)`.
- **The machine row gains** `public_key`, `key_at`, `helper` (the last hello's helper block, limits and update state) and `trust` (P4.5.8), in revision `0016c_helper_jobs` (P4.5.4).
- `WorkerConnection` keeps one protocol; v1's frame definitions (`citar/worker/protocol.py`) move to `citar/server/helper_protocol.py` as v2's.

### P4.5.2 Tool-call parsing on the server

- `citar/agents/providers/completion.py` (new) holds `parse_completion(message, finish_reason, mode, tools) -> (StepResult, assistant_entry)`: the post-processing `OpenAIConversation.step` does today (native calls, `__invalid_json__`, `repair_tool_calls`, text calls in XML, GLM, plain and Gemma 4 syntax, the JSON protocol), moved without change of behaviour.
- `OpenAIConversation.step` converts the SDK's message to the same dict and calls it; `WorkerConversation.step` sends `request` frames (OpenAI-format tools; in `json` mode the system prompt with `JSON_PROTOCOL` and the listing, and no tools) and calls it on `done.message`. One parser, so a model's quirk fixed once is fixed for helpers too: the 3-19 Gemma 4 fix needed shipping to helpers; from 4-02 it never will.
- Usage maps `prompt_tokens`, `completion_tokens`, `completion_tokens_details.reasoning_tokens` and `prompt_tokens_details.cached_tokens` onto the server's usage keys; tokens are capped as P3.3.4 says.

### P4.5.3 The scheduler on v2

- **Capacity** = the site's slots capped by the helper's `settings_ack.effective.llm.slots` (it was `hello.max_concurrent`).
- **Cancel** (`WorkerConversation.close` → `hub.cancel`): the caller's wait ends at once, as today; the slot is freed by `cancelled` (or a crossing `done`/`error`). If neither comes within `CANCEL_ACK_SECONDS` (5 s), the slot drains as P3.3.3 describes and the machine's `flags.cancel_unacked` counts it (shown to admins). A v2 helper that drains is misbehaving; draining is no longer the normal path, which is what 3-19's "Left for later" asked.
- `busy` from a v2 helper means its local slots are below the site's and its `settings_ack` was missed: the existing lower-and-requeue rule (P3.3.3) stays as the net.
- A helper's `status.quiet_until` is read like the site's quiet hours.

### P4.5.4 The job store (revision `0016c_helper_jobs`)

One Alembic revision (4-02, "schema first" as in P3.10.1) with every Phase 4 table and column; later packages fill in behaviour.

- **`cpu_jobs`:** `id` (`cj_…`), `kind`, `ref` (`lab:<exp>:<i>`, the ledger's activity id of 3-10), `item_kind`/`item_ref` (the owned experiment, for access), `owner_id` (the consumer; null for `collection`), `spec` (JSON), `spec_hash`, `build`, `priority`, `state` (`queued running verifying accepted crashed failed withdrawn cancelled`), `replicas_needed` (1, 2 or 3), `accepted_run`, `result_hash`, `attempts`, `created_at`, `updated_at`; indexed on (`state`, `priority`, `created_at`) and on `ref`.
- **`cpu_job_runs`:** `id` (`cr_…`), `job_id`, `machine_id`, `executor` (`local`/`remote`), `role` (`primary`, `replica`, `tiebreak`), `state` (`offered running done failed aborted expired`), `offered_at`, `started_at`, `ended_at`, `deadline_at`, `cpu_s`, `wall_s`, `peak_mb`, `result` (JSON, pruned once the job is settled), `result_hash`, `chain_head`, `rounds`, `digests` (JSON, pruned once matched; kept as evidence on a mismatch), `claim`, `signature`, `verdict` (`reference match mismatch unchecked rejected`), `error`.
- **`servers`:** `public_key`, `key_at`, `helper` (JSON), `trust` (JSON: `{verified, streak, mismatches, last_verified_at, last_mismatch, abandons, since}`).
- **`compute_events.verification`** gains `rejected` (a string column: no DDL); `settings_kv.verification` holds P4.5.8's settings.

### P4.5.5 The dispatcher

- **`citar/jobs/dispatcher.py`:** one thread per server process (like the scheduler and the ledger writer), restarted if it dies, surviving an exception in its loop; it wakes every 2 s and on events (a job frame, an executor change, a new experiment). Its state is the database; its in-memory view is rebuilt at start, which re-queues every run that was `offered` or `running` on the local executor (whose job host died with the old process) and keeps remote runs until their helpers reconnect or their deadlines pass.
- **Jobs come from the lab's queue:** experiments by priority then age (`lab.load_queue`, unchanged files), each game without an accepted result and without a live job becomes a `cpu_jobs` row, at most 4 x the executors' total threads queued ahead (so a 10,000-game experiment does not become 10,000 rows at once). An experiment whose games are all accepted moves to `done/` (as today).
- **Matching** a queued job to an executor with a free thread, highest priority first, round-robin across owners within a priority. An executor is eligible when: online; CPU enabled with the job's kind in its effective `work`; the same build id; `cpu.verifiable`; not quarantined, nor its owner (`users.trust_tier = quarantined`); outside the site's restricted hours and its helper's own quiet hours; no benchmark lease live on it (P3.3.6's rule extended to CPU); and the job's owner may use it: their own machine, a `collection` job on a machine whose owner opted into that kind, or a grant with the purpose `lab` on its group checked by `admission.check(..., purpose="lab")` (3-09's admission, its budget included).
- **Deadlines:** `deadline_s = clamp(10 x the expected wall time, 300 s, 6 h)`, the expected time from the executors' running mean per (map size, rounds); a run past its deadline plus 60 s is `expired` and its job re-queued; a machine with three expired or abandoned runs in an hour gets no offer for an hour (`trust.abandons`).
- **Failures:** `job_failed` with `retryable` re-queues the job (at most 3 attempts, then `failed`, shown on the Lab page); a crash outcome is accepted as the game's outcome (P4.4.2) once verified like any result.
- **Pause:** `citar lab stop` and the Lab page's Stop pause the dispatcher's offers (running runs finish); `citar lab resume` resumes.

### P4.5.6 Executors

- **The local executor** (`citar/jobs/local.py`) is the host's machine row: it runs a job host child (`python -m citar.jobs host`) with the host row's CPU settings, restarted if it dies, ended with the server. It is **trusted by definition** (it is the server): its runs' verdict is `reference`, and it is the default verifier (P4.5.8). Defaults for the host row: local mode, CPU on with threads = half the logical CPUs; server mode, on with 1 thread, 50% and 1,024 MB (the VPS has one vCPU; an admin raises or lowers them in the host's machine settings). These replace the lab runner's `workers`/`night_workers` (`saves/lab/config.json`); the host's restricted hours replace `quiet_now`. On the VPS, a system service with no user manager, the local job host's CPU share is soft (`nice` and its one thread are the real cap) and its memory hard (`RLIMIT_DATA`); the host's card says so like any machine's.
- **Remote executors** are connected helpers: the dispatcher sends `job_offer` through the hub (`send_threadsafe`), and job frames come back through its queue. Threads are the helper's `settings_ack.effective.cpu.threads`.
- **Replacing the lab runner:** `lab.run`, `_runner_step`, `_Adopted`, `_find_running_games`, `_collect_orphans`, `cmd_play`, the job files and the status file go (4-08); `python -m citar.lab run` and `citar lab run` say the lab now runs inside the CITAR server (local or server mode) and exit 2; `submit`, `status`, `report`, `stop` and `resume` stay. `lab.overview()` reads the dispatcher.

### P4.5.7 Results into the lab, the ratings and the ledger

- **The lab:** an accepted job appends one line to `saves/lab/results/<exp>.jsonl` (`reference` experiments to `saves/lab/reference/<name>.jsonl`), as `_record` does today, plus `executor: {machine, run, verification}`. Writes go through `fsutil` (replace on rewrite) on the dispatcher's thread. A **withdrawal** rewrites the file without the withdrawn lines (P4.5.8).
- **The ratings** read the results files as before (only accepted lines are there), and their cache, keyed on the files' signature, recomputes after any append or rewrite.
- **The ledger:** each completed run writes one `cpu` event when it completes, through the ledger's writer: `activity = lab:<exp>:<i>`, `activity_kind = lab`, `task_class = cpu`, `machine_id` the executor's row, `provider_id` its owner (null for the host), `consumer_id` the job's owner for a primary run and null for a replica or tiebreak (verification is the server's own work, given by the machine's owner and used by nobody), `cpu_core_s = min(reported cpu_s, observed wall x 1.05 + 1)` (a game is one thread; `flags.overreported_cpu` past it), `verification` `pending` until the job settles, then `verified` (matched, or the host's), `unverified` (accepted without a replica) or `rejected` (credits set to 0, the hour's rollups recomputed). `ingest_lab` and its reading of the results files go. A job admitted under a grant with a budget settles its primary run's credits into that budget as a model step does (3-10's keeper, through the ledger's writer), and admission checks the budget before each offer, so a hard budget stops CPU work at its limit.
- **Power rows** from helpers' `power` frames are written as the host's sampler writes its own (`usage`), so costing prices a helper machine's measured energy; the KNOWN_ISSUES item narrows to machines whose power cannot be read.

### P4.5.8 Trust tiers: replication, sampling, promotion, quarantine, withdrawal

**Settings** (`settings_kv.verification`, admins; defaults from plan §1): `new_streak` 5, `provisional_rate` 0.10, `trusted_rate` 0.02, `promote_verified` 50, `overreport_days` 30.

**The rules,** held by `citar/jobs/trust.py` and its table-driven tests:
1. **When a primary run goes to a remote machine, the dispatcher decides at once whether the job is replicated:** always on a `new` machine; with probability `provisional_rate` on a `provisional` one and `trusted_rate` on a `trusted` one, decided by `blake3(server secret ‖ job id) < rate`, so it is reproducible for the server and unpredictable for the helper, whose offers look the same either way. Runs on the local executor are never replicated.
2. **A replica runs on a verifier:** the local executor, or a `trusted` machine of another owner than the primary's (an owner never verifies their own machine). If no verifier is free, the replica waits; a `new` machine is offered nothing while no verifier exists at all (the Lab page says "waiting for a verifier").
3. **Settling a job:** with no replica, the primary's result is accepted (`unverified`). With one, the job is accepted when both are done and match (spec hash, build, result hash, chain head and rounds): `verified`. If the primary fails, the verifier's result alone is accepted.
4. **A mismatch** against the local executor: the primary's machine is quarantined. Against a trusted machine: a tiebreak runs on the local executor, and every machine that disagrees with it is quarantined (a trusted machine can be wrong too). The evidence kept on the run and the machine: the job, both runs, the verdict (`result`: same chain, different result, which a correct engine cannot produce, so tampering; `chain`: the first differing round; `signature`; `chain_inconsistent`; `claim`: a claim naming another job, run, machine, spec or build).
5. **Promotion:** each verified match adds to `trust.verified` and `trust.streak`. `new` with `streak >= new_streak` becomes `provisional`; `provisional` with `verified >= promote_verified`, no mismatch, and no `flags.overreports` in the last `overreport_days` becomes `trusted`. Each change writes an audit row (`server.trust`, actor `system`).
6. **Quarantine:** the machine gets no CPU job, and admission refuses it for any purpose but its owner's own use (code `quarantined`): a machine that returned a wrong game is not relied on for anyone else, its LLM work included. Its running runs are aborted and re-queued.
7. **Withdrawal:** every job whose accepted run was on the quarantined machine and `unverified`, accepted after the machine's last verified match, is withdrawn: its line removed from the results file, its run's event `rejected`, the job re-queued (it will run elsewhere). Withdrawn jobs are listed in the evidence.
8. **Admins** can set any tier (the existing `PUT /api/admin/machines/{id}/trust`, now with `withdraw: bool` when quarantining by hand), release a machine from quarantine (back to `new`, counters reset), and ask that a machine's next N jobs be replicated (`POST /api/admin/machines/{id}/replicate`).
9. **LLM work** stays attributed, not re-verified (C2): its events carry `flags.tier`, the machine's tier when the step ran; over-reporting blocks promotion (rule 5) but does not quarantine.

**Reading plan §6.7's "only verified or trusted results feed lab results, ratings and pooled data":** this design counts a `provisional` machine's unsampled results (rule 3), with rule 7 taking them back if the machine is ever caught. The strict reading would keep 90% of a provisional machine's work out of the lab until it reached 50 verified jobs, which at a 10% sample is about 450 jobs: the work would be wasted or held for hours. With withdrawal, a cheater gains nothing: everything it contributed since its last check leaves the lab, the ratings and its credits. This is listed as an owner question for confirmation.

### P4.5.9 The update relay

`citar/server/helper_updates.py`: fetches the manifest and signature of the server's own version (P4.3.8) at most hourly on a background thread (never on a request path; a failure keeps the last good manifest and is logged once an hour), verifies it with `engine_api.verify_manifest` against the keys the extension embeds (the same `update-keys.pub`; a test-ops build of the extension also trusts `CITAR_HELPER_UPDATE_KEY`, so the Python suite can use a throwaway key while no shipped wheel can be fooled by the environment), puts it in every welcome and sends `update` to connected helpers whose version is older. The Servers page shows each helper's version, the latest signed one, and its `update_status`.

### P4.5.10 Routes

Every route below is gated, gets P3.9.1's tests (stranger 404, visitor 401, owner, admin, moderator, a grantee where a group lends the machine), and joins `audit_routes.py --strict`.

| Route | Who | What |
|---|---|---|
| `POST /api/machines/{id}/key/reset` | MANAGE (owner, admin) | forget the registered key; the next hello registers its own; audited |
| `POST /api/machines/{id}/rescan` | MANAGE | send `rescan` to its helper; 409 when offline |
| `GET /api/machines/{id}/jobs` | MANAGE | its recent runs (state, kind, verdict, times), experiment names only where the caller may view the experiment, its trust counters and last mismatch |
| `GET /api/lab/jobs?exp=NAME` | VIEW on the experiment | its games' jobs and runs, executors named where the caller may view the machine ("a member's machine" otherwise) |
| `GET /api/admin/jobs` | admin | the dispatcher: queued by experiment, running per executor, verifiers, paused or not |
| `GET /api/admin/verification` | admin | the settings, quarantined machines with evidence, recent mismatches and withdrawals |
| `PUT /api/admin/verification` | admin | the settings (bounded: rates 0..1, counts 1..10,000) |
| `POST /api/admin/machines/{id}/replicate` | admin | replicate its next N jobs (1..100) |
| `PUT /api/admin/machines/{id}/trust` | admin (exists) | gains `withdraw` |

`GET /api/lab` and `GET /api/queue` change shape (the dispatcher's health; a "CPU jobs" section per machine the caller may view, others as "someone's lab game"), keeping their gates.

### P4.5.11 Pages

- **Servers page, a helper machine's card:** helper version, build, target and protocol; "update available" or its `update_status`; the key's fingerprint and "Reset key…"; CPU in effect (threads, CPU %, memory, each marked hard or soft for this OS); CPU runs now and today; the trust tier with verified count and streak; for admins, the last mismatch with its evidence and Quarantine / Release.
- **Machine settings dialog** (3-18): the CPU section's note "takes effect with the 0.1.6 helper" goes; `yield_to_llm` joins it; what the helper's ceiling capped is shown; "Rescan now" on the hardware tab.
- **Queue page:** "CPU jobs" per machine.
- **Lab page:** per experiment, games accepted (verified, unverified, reference), running, waiting, withdrawn; per game its executor and a verification badge; "waiting for a verifier" when that is why nothing starts.
- **Admin:** `#/admin/verification` (settings, quarantined machines with evidence and Release, mismatches, withdrawals) linked from the admin users page; a member gets the router's "not found".
- **The helper card** (`helper.js`): five platforms, Intel Mac included; the pipx sentence goes.

---

## P4.6 Threat model

**Assets:** a lender's machine (its CPU, memory, GPU, files, network, the API key a helper may front, its uptime); the server's data (lab results, ratings, the ledger and credits, model scores); members' accounts and the machines they own; the update channel.

**Actors:** (A) a malicious or compromised CITAR server, or its operator, against lenders; (B) a malicious lender or a modified helper against the server's data; (C) a broken but honest machine (bad RAM, an overclock, a denormal-flushing environment); (D) a network attacker between the two; (E) an attacker on the release channel (a compromised GitHub account or workflow); (F) another member against a machine they were not lent.

| Threat | Mitigation | Left over |
|---|---|---|
| A makes the helper run code, read files or reach the lender's network | No command, script or code is ever accepted; jobs are a closed vocabulary of typed, bounded specs running only compiled-in engine and bots (P4.4.1); completions go only to the configured endpoint with whitelisted parameters and the model allowlist; nothing reads outside the home | A bug in the engine or a spec parser on hostile input: specs are fuzzed with the frames (4-01), and the job host runs under limits |
| A exhausts the lender's machine | The local ceiling caps every figure (local always wins); hard limits where the OS allows, soft and labelled where not (P4.3.6); local quiet hours; `yield_to_llm`; spec bounds and deadlines | macOS limits are soft; a lender who needs hard limits there sets fewer threads |
| A pushes malicious or old code as an update | Manifests are verified against public keys compiled into the helper, whose secrets the server never holds; no downgrade; sha256 and size checked; https only; the staged binary self-tests; policy `off`/`notify` locally | The release key itself (E) |
| A learns more about the lender than it should | Hardware reports can be a file or nothing; power off locally; the key's secret never leaves | The prompts a lender's model serves are seen by the lender, as in 0.1.5: lending a GPU means seeing what it computes |
| B fakes results or poisons the ratings | Every result is a signed claim bound to its run, machine, spec and build; chains and result hashes recomputed by the server; replication (all jobs of `new` machines, samples after); quarantine on any mismatch; withdrawal of everything unverified since the last check; quarantined machines serve nobody else | A patient cheater on a `trusted` machine contaminates at most about 50 jobs (2% sampling) before it is caught, and those are withdrawn when it is |
| B inflates credits | CPU seconds bounded by observed wall time; generation time by observed time (P3.3.4); rejected runs earn nothing; replicas earn the provider credit but no consumer pays | A helper can be slow on purpose; it then earns the same per CPU-second and is abandoned past deadlines |
| B serves a smaller model than claimed | Not verifiable (C2); events carry the tier; admins see `flags.overreports` | Canary prompts are a later option (P4.1.3) |
| B steals another machine's identity | Tokens hashed at rest and revocable; keys bound on first use and resettable only by the owner or an admin; the rate limit before any lookup | A token stolen before the real helper's first hello registers the thief's key; the fingerprint on the Servers page shows it, and a reset fixes it |
| C produces honest wrong results | The same machinery as B: quarantine with evidence naming the round; the canary refuses jobs where denormals are flushed | — |
| D reads or alters traffic | TLS with the OS verifier; `--insecure` only for loopback and private hosts; `--ca-file` for self-signed servers | — |
| E signs a malicious release | Phase 5's custody decision (offline key, or a protected environment with a required reviewer); two key slots for rotation; attestations | The owner's key management |
| F uses a machine they were not lent | Admission by grant and purpose (`lab`) for every offer; owners' own experiments only on their own machines otherwise; `collection` kinds only on machines that opted in | — |

`SECURITY.md` gains this as its "Helpers and shared machines" section (4-10, extended in 4-11): what a worker token allows (serving completions and running the closed job vocabulary under the owner's limits), the claims a lender can rely on (P4.3.10), and the verification scheme. `WORKERS.md` "What the server can and cannot do" is rewritten from the same table.

---

## P4.7 Cross-cutting rules

### P4.7.1 Safety on this laptop (binding on every package)

1. **Never** install a system service, a scheduled task or a startup entry on Windows, change a system or security setting (firewall, Defender, UAC, any registry value), or run anything elevated. Service code is tested by `--dry-run` and unit tests of the generated definitions; real installs run only in WSL with `systemd --user` (per-user), and in CI.
2. **Never** touch the owner's real helper configuration or data: `helper.json`, `worker.json` and `machine-key.json` in their real places (`%LOCALAPPDATA%\CITAR`, the repository's `config/`), `%LOCALAPPDATA%\CITAR\citar.db`, `secret_key`, `config/servers.json`. Every helper run uses `--home` (and `CITAR_DATA_DIR`) in its own scratch or QA directory. S10 fingerprints them before and after.
3. **Never** generate a release signing private key into the repository or commit any private key. Tests generate throwaway keys at run time in their temporary directory (`cargo xtask keygen` refuses a path inside the repository; a gate greps the tree for key files and PEM headers). The real key's custody is the owner's question for Phase 5.
4. **Never** connect a test helper to `citar.jimmieprodgers.com` or any non-local server: the QA harness starts its own servers on loopback (or inside WSL), helpers built with test features dial loopback only (P4.3.1), and no package hands a helper a URL from anywhere else.
5. **CPU-limit tests** use small limits and short runs: at most 2 cores' worth and 10 s each, at most 512 MB.
6. **Never** port 8799 or `C:/dev/citar-play` (the harness refuses them, as `qa_phase3.py` does).
7. **LM Studio** (`http://localhost:1234`, `google/gemma-4-e2b`): read its model list and ask it for completions of that model only; never load, unload or swap a model or change its settings; check that the model is loaded before and after.
8. **WSL** is `wsl -d Ubuntu-24.04 -u claude` (root via `-u root` only inside WSL, and only where a package says why); never handle the WSL password; build in `~/` (never under `/mnt/c`), in a copy of the branch made by `git bundle` or `git clone`, with `CARGO_TARGET_DIR=~/target/citar-p-4-xx`.
9. Never push, never touch the VPS, never write in the main checkout (it is only read).

### P4.7.2 Conformance fixtures

P4.2.9: every frame type has valid and invalid fixtures; every behaviour the protocol promises has a transcript; the helper's Rust tests and the hub's Python tests both run all of them; a package that changes the protocol changes PROTOCOL.md, the fixtures and both sides in the same commit series. A behaviour that is only tested on one side is a review finding.

### P4.7.3 Access and the route audit

P3.9.1 stands: every new or changed route gets stranger, visitor, owner, admin and moderator tests (and a grantee's where groups lend the machine), 404 rather than 403 where the caller cannot see the object, `Depends(get_db, scope="function")` (3-19's rule, enforced by `test_access`), and `audit_routes.py --strict` with 0 unguarded. Job frames are authorised by the connection (a helper only ever acts on runs offered to it: a frame naming another machine's run is dropped and logged).

### P4.7.4 Locks, threads, the event loop, the database

- P3.9.2 stands (no model call, disk write or database write under a session lock; every wait has a timeout; every timer thread survives its own exceptions and is restarted).
- **The event loop never blocks:** database writes, scans' records, power rows and token lookups go through `asyncio.to_thread`; job frames go onto the dispatcher's queue.
- **The dispatcher** is the only writer of `cpu_jobs` and `cpu_job_runs` (one thread); routes read them. Results files are written only by it, through `fsutil` (atomic replace on a rewrite); a listing of files newest first uses `fsutil.newest_first` (`p3-notes.md`).
- **The helper:** one session task owns the socket; requests and jobs are tasks; blocking work on `spawn_blocking`; the job host is a separate process.

### P4.7.5 The standard gates

Every package's gates include, besides its own (P3.9.3's, renumbered where Phase 4 adds):
- **S1** the Python suite (`python -m unittest discover -s tests`), with `cargo xtask develop` (which from 4-03 also builds the helper into `citar-ext/`, where the end-to-end tests find it) when Rust changed; its count recorded. **S1b** the whole suite on Python 3.12 under `TZ=UTC0` (`C:/dev/venv/py312-check` with `CITAR_EXT_DIR` pointing at the worktree's extension), as Phase 3's merges ran it.
- **S2** ruff. **S3** `audit_routes.py --strict`. **S4** `check_links.py --strict`. **S5** when `docs/` changed: `mkdocs build --strict` and `build_wiki.py` with `wiki/` committed.
- **S6** the front-end checks (ESM parse of every module, `node --test citar/web/tests/`).
- **S7** when Rust changed: `cargo fmt --check`; clippy `--workspace --all-targets --all-features --locked -D warnings`; nextest `--workspace --all-features --locked`; the doctests; `cargo xtask check`; the refcheck run (`--fixtures refcheck/fixtures-mini --fixtures refcheck/fixtures-late --strict --with-bot`); `cargo golden check`.
- **S8** the package's part of `scripts/qa_phase4.py` (P4.7.6) exits 0, and its browser items (P4.7.7).
- **S9** when `citar-helper` or `citar-jobs` changed: in WSL, `cargo nextest run -p citar-helper -p citar-jobs --locked` and `cargo golden check` in the branch's copy (Linux x64, CI's stand-in for ubuntu).
- **S10** the owner's data and the machine are as they were: before and after the package's runs, the sha256 of `config/servers.json` (starts `586888cbb61aa9c7`), the mtimes of `%LOCALAPPDATA%/CITAR/citar.db` and `secret_key`, the presence, size and mtime of `%LOCALAPPDATA%/CITAR/helper.json`, `machine-key.json` and the repository's `config/worker.json`; no `citar-helper`, `job-host` or QA server process left; `sc query citar-helper` reports no such service; no `CITAR` value under `HKCU\...\Run`; no `CITAR` scheduled task; in WSL no `citar` unit under `~/.config/systemd/user` and none listed by `systemctl --user list-unit-files`; no `CITAR_*` variable left set; the main checkout shows only `.claude/launch.json`.
- **CI-only** (a worktree never pushes): test.yml on its three OSes; rust.yml; determinism.yml (the `jobs` set on five targets); `helper.yml` from 4-12 (P4.7.10). Nobody labels or dispatches nightly.yml, docs.yml or release.yml without the owner's OK.

### P4.7.6 The QA script

- `scripts/qa_phase4.py <part>|all [--port N] [--dir D] [--keep] [--helper-bin PATH] [--wsl]`, reusing `qa_phase3.py`'s harness (the dev server child, the refusals of port 8799, `C:/dev/citar-play` and non-empty foreign directories, the accounts, the evidence files, the scripted endpoint), imported rather than copied.
- **Helpers** are the real Rust binary (`--helper-bin`, else `CITAR_EXT_DIR/citar-helper[.exe]`), each started with `--home <dir>/helper-<n>`, `--server http://127.0.0.1:<port>` (or the WSL server's forwarded `localhost`), and its own token; the harness stops them all at its end and checks none is left.
- **The scripted endpoint** (3-19's) gains streaming (SSE with per-delta delays, reasoning deltas, tool-call deltas) and records, for each request, the tokens it sent and when the client disconnected, so cancel is checked at the endpoint.
- **Parts:** `helper` (4-03, 4-04), `llm-real` (4-04, LM Studio), `scan` (4-05), `lab` (4-08), `remote` (4-09), `verify` (4-10), `update` (4-11), `service` (4-12, WSL), `exit` and `real` (4-13), and `all` (the others in that order; `real` and `llm-real` need LM Studio and can be skipped with `--skip` for development runs, never at the exit).
- **Topology for `exit` and `remote --wsl`:** the dev server runs **in WSL** (bound to `0.0.0.0` inside WSL only, in its own directory, with a venv and extension built there by `cargo xtask develop`), the Windows helper dials it through WSL's localhost forwarding (`http://localhost:<port>` from Windows), and the WSL helpers dial `127.0.0.1` inside WSL; the server's local executor (Linux) is the verifier. This makes "a remote lab experiment" cross-OS (Windows results replicated against Linux ones) without a server on Windows listening beyond loopback, which could raise a firewall prompt. If Windows cannot reach the WSL server through localhost forwarding, the part records it and runs the fallback: the server on Windows bound to `127.0.0.1` with two Windows helper processes and the local executor, and the WSL helper's conformance against a WSL server separately.
- `qa_phase3.py`'s parts keep working with the Rust helper (4-04 switches its helper processes), so Phase 3's QA stays a regression gate.

### P4.7.7 The browser checklist

Run in the browser pane against the QA script's server (`--keep`), each item with screenshots in the QA directory, ticked in the package's as-built note:
- **H1** a helper machine's card: helper version, build, target, protocol 2; the key's fingerprint and "Reset key…"; CPU in effect, hard and soft marked; the latest signed helper and an "update available" state.
- **H2** the machine settings dialog: the CPU section live with what the helper's ceiling capped; "Rescan now" updates the last report's time.
- **H3** the Queue page's CPU jobs per machine: the owner's own experiment named, another member's "someone's lab game".
- **H4** the Lab page: per game its executor and verification badge; a withdrawn game shown re-queued; "waiting for a verifier".
- **H5** `#/admin/verification`: settings, a quarantined machine with its evidence (the round where it parted, or "same chain, different result"), Release; a member's "not found".
- **H6** the helper card: five downloads, Intel Mac among them, no pipx sentence.

### P4.7.8 Docs

Each package updates what its behaviour touches, named in its scope: `docs/server/WORKERS.md` (4-03, 4-04, 4-05, 4-09, 4-10, 4-11, 4-12: rewritten section by section into "the CITAR helper"), `SECURITY.md` (4-10, 4-11), `docs/API.md` (every new route), `docs/CONFIGURATION.md` (the helper's settings, the host's CPU defaults, `helper_manifest_url`), `docs/ARCHITECTURE.md` (4-02, 4-08, 4-10), `docs/BOTS.md` and `docs/research/BOT_TUNING.md` (the lab runs in the server, 4-08), `CONTRIBUTING.md` (the helper's dev loop, WSL, `qa_phase4.py`), `KNOWN_ISSUES.md`, `CHANGELOG.md` (each package's lines; 4-13's pass), `packaging/README.md` (4-12); the wiki regenerated with them.

### P4.7.9 Performance

Measured as P3.9.7 says: the baseline in the same run, alternating, five times each, medians compared; never a number from another day; CI-run timing assertions get three times their local bound (`p3-notes.md`); durations by `time.perf_counter` in Python and `Instant` in Rust; never sleep-then-assert (wait on the condition with a deadline).
- **The session's overhead** (`scripts/session_overhead.py`) at most 2.87 where the scheduler or hub changed (4-02, 4-04).
- **The scheduler:** under 1 ms a step with nothing queued (P3.9.7), unchanged by v2.
- **The hub:** 1,000 request and done round trips through a `FakeHelper` that answers at once: the hub's own time per round trip (observed minus the FakeHelper's) median under 3 ms.
- **The dispatcher:** a tick with 2,000 queued jobs and 10 executors under 50 ms (median of 100 ticks).
- **Job overhead:** a 30-round duel through the local job host against `engine_api.run_job` in-process, same run: at most 10% plus 0.2 s more; the same duel through a helper on the same machine against the local job host: at most 15% plus 0.5 s more. The Rust lab game against the Python `lab.play` it replaces (4-06): no slower.

### P4.7.10 CI parity and CI-only gates

- Locally: Windows x64 natively, Linux x64 in WSL (S9), Python 3.12 under `TZ=UTC0` (S1b), `--locked` everywhere, the `ci` profile's checks in nextest.
- **CI-only, named in each package that relies on them:** macOS arm64 and x64 and Linux arm64 builds and conformance (`helper.yml`, from 4-12; until then rust.yml's three OSes run the helper's tests on Windows, Linux and macOS arm64); the `jobs` golden set on all five targets (determinism.yml); the Windows service installed, started, stopped and removed on `windows-latest`; the launchd agent on macOS (plist lint, bootstrap where the runner allows); Linux `systemd --user` on ubuntu (after `sudo loginctl enable-linger runner`); macOS's soft limits reported; `test.yml`'s end-to-end helper tests on its three OSes.

### P4.7.11 Process

As Phase 3: a worktree per package (`.claude/worktrees/p-4-xx`, branch `p-4-xx`); `CARGO_TARGET_DIR=C:/dev/target/citar-p-4-xx` before every cargo command (outside OneDrive); files written with the Write tool (Git Bash mangles backslashes in heredocs); small commits ending with the Co-Authored-By line; never `ops/`, `saves/` or `.claude/launch.json`; never a push. Every package ends with its gates, then a review and a fix round (the owner's standing rule), and its as-built note records its start and end times. Handoff notes for the next package go to `ops/p4-notes.md` (gitignored), as `p3-notes.md` did.

---

## P4.8 Decisions taken in this design

1. **v1 is refused, with a message a 0.1.5 helper prints, and a v2 helper waits for an old server** (P4.2.8); no transition window.
2. **One WebSocket path**, `/ws/worker`, for both the refusal and v2.
3. **Additive within v2; job specs closed** (P4.2.1).
4. **Raw completions; one parser on the server** (P4.5.2); `json` mode's protocol prompt built by the server.
5. **Streaming between the helper and its endpoint, for cancel;** one `done` to the server.
6. **Cancel is acknowledged;** draining only for an unacknowledged cancel, flagged.
7. **Settings are acknowledged with what is in effect;** the scheduler and dispatcher read the acknowledgement.
8. **Rate limit before lookup, hop-aware address, `last_ip` written** (defects of P4.0).
9. **Machine keys are trust-on-first-use per row,** resettable by the owner or an admin; signing in the helper's parent process, never in the job host.
10. **A shared `citar-jobs` crate** holds everything both sides must compute identically: specs and hashes, the lab result, the chain, claims and signatures, the update manifest, the job host and its limits.
11. **The kinds:** lab, ranking and reference games; ranking and reference are admins' `collection` experiments; reference results are baseline lines.
12. **Crashes are results:** final, verified like any other, never retried.
13. **One job host process per executor,** applying its limits to itself, used by the helper and by the server alike; no orphans (stdin EOF ends it).
14. **Limits per OS** as P4.3.6's table, each reported hard or soft and shown.
15. **`yield_to_llm`** pauses a helper's games while its model works; no CPU job offered while a benchmark lease is live on the machine.
16. **The dispatcher lives in the server process;** `citar lab run` is retired; the host's machine row is the local executor and its settings replace the lab's worker counts.
17. **The local executor is trusted by definition** and the default verifier; trusted machines of another owner verify too.
18. **Sampling is keyed by the server's secret;** trusted machines are still sampled at 2%.
19. **Provisional results count, with withdrawal on quarantine** (P4.5.8; an owner question confirms the reading).
20. **Quarantine stops a machine's work for anyone but its owner,** CPU and LLM alike; release returns it to `new`.
21. **Ledger events are written per run** with `pending` until settled; replicas are given by their machine's owner and used by nobody; rejected runs earn nothing; `ingest_lab` goes.
22. **Updates follow the server's own version,** relayed and verified by the server, verified again by the helper, never downgraded, applied when idle; no key ships in Phase 4.
23. **Services:** per-user by default (systemd user unit, LaunchAgent, HKCU Run entry), a real Windows service and a system unit on request with elevation done by the owner.
24. **Test features** (`test-faults`, `test-update-key`, `test-jobs`) never ship (xtask) and confine a build to loopback.
25. **The Linux release builds stay glibc-based** (on ubuntu-22.04, as today's helper), with rustls and the platform verifier, so TLS no longer depends on where a distribution keeps its CA bundle.
26. **The Python worker is deleted,** with its extra, its script, the wizard's flow and the install scripts' paths replaced by a pointer to the helper.
27. **Release names are unchanged** plus `citar-helper-macos-x64`; 4-12 switches release.yml's helper job to cargo with those names and a test holds helper.js to them; the rest of the release is Phase 5's.
28. **QA runs the server in WSL** for the cross-OS remote experiment, so no Windows listener ever leaves loopback.

## P4.9 Risks

1. **A target computes another game.** The engine's matrix holds today; the `jobs` set adds the job runner to it on five targets, WSL checks it locally, and a mismatch between honest machines would show as a quarantine with "chain parted at round R", which the evidence makes diagnosable. Mitigation for an outbreak: an admin can release machines and set the rates to 1.0 while it is investigated.
2. **OS limit APIs** (Job Objects, cgroup delegation, macOS's lack of hard caps). One small `unsafe` module with SAFETY comments; each limit measured by a test; soft limits reported as soft, never claimed hard.
3. **The update channel.** Signatures, no downgrade, self-test, rollback; no key until the owner decides custody.
4. **Verification load on a one-vCPU VPS.** New machines double their work until five matches; the host verifies at 1 thread by default; admins can mark a machine trusted to verify; "waiting for a verifier" is visible rather than silent.
5. **WSL networking.** Localhost forwarding from Windows into WSL is assumed and probed; the fallback topology is defined (P4.7.6).
6. **LM Studio's behaviour on a closed stream.** Measured, not assumed: the cancel check compares a following request's latency with the same run's baseline.
7. **A helper newer than its server** (updated elsewhere): LLM work goes on (same protocol); CPU jobs are rejected for the build until the server catches up, and the Servers page says so.
8. **This laptop throttles** (P3.9.7): timings within a run only; limits checked as upper bounds.
9. **Thirteen packages in sequence.** Each sized like a Phase 3 package or smaller; the three at the upper bound (4-02, 4-08, 4-10) name where to split.

## P4.10 What Phase 5 builds (the hooks Phase 4 leaves)

- **The helper's release job** (rewritten in 4-12, enabled when Phase 5 removes the gate): `cargo build --release --locked -p citar-helper` on `windows-latest` (`x86_64-pc-windows-msvc` → `citar-helper-windows-x64.exe`), `macos-15` (`aarch64-apple-darwin` → `citar-helper-macos-arm64`), `macos-15-intel` (`x86_64-apple-darwin` → `citar-helper-macos-x64`), `ubuntu-22.04` (`x86_64-unknown-linux-gnu` → `citar-helper-linux-x64`) and `ubuntu-22.04-arm` (`aarch64-unknown-linux-gnu` → `citar-helper-linux-arm64`), each with its `.sha256`, each run once with `--version` and `--self-test`; no features.
- **The manifest:** `cargo xtask helper-manifest` over the five assets (versioned tag URLs) → `citar-helper-manifest.json`; signed by `cargo xtask sign-manifest` → `.sig`; both attached to the release beside the binaries.
- **The key:** `crates/citar-jobs/update-keys.pub` gets the owner's public key in a release commit; where the secret lives and who signs is the owner's decision (owner question 1). Until then helpers report updates `unavailable`.
- **Packaging:** the Homebrew tap formula for the helper only (R4: macOS arm64 and x64, Linux), Scoop and winget, install scripts downloading the helper for `--worker`, attestations, code signing if ever, the release dry run.

## P4.11 Work breakdown at a glance

Thirteen packages, strictly in this order, one agent at a time. Each ends with its gates, a review and a fix round, and its as-built note.

| # | Package | Plan §7 Phase 4 part |
|---|---|---|
| 4-01 | Protocol v2: the specification, the frame types on both sides, the shared fixtures | 1 |
| 4-02 | The hub on v2: the handshake, keys, settings, raw completions parsed on the server, acknowledged cancel, the schema | 1 |
| 4-03 | The Rust helper: configuration, the connection, keys, settings, catalog | 2 |
| 4-04 | The LLM proxy and real cancel; the Python worker retired | 2 |
| 4-05 | The hardware scan and power samples in Rust | 2 |
| 4-06 | Jobs in Rust: the vocabulary, the lab game, reference games, the chain and the result hash | 3 |
| 4-07 | The job host and CPU limits per OS | 3 |
| 4-08 | The dispatcher and the local executor; results into the lab, the ratings and the ledger | 3 |
| 4-09 | Remote executors: CPU jobs on helpers | 3 |
| 4-10 | Proof of work: signed claims, verification, trust tiers, quarantine, withdrawal, the admin UI | 4 |
| 4-11 | Signed self-update | 5 |
| 4-12 | Service installation, the release names, helper.yml, the helper docs | 5 |
| 4-13 | Phase 4 exit: conformance, the remote experiment verified, the faulty helper, the real model, the evidence | exit |

The order follows the plan's five parts and three dependencies of its own: the hub speaks v2 before the helper exists (tested from fixtures), so the Rust helper meets a finished server; the Python worker is retired only in the package that gives the Rust helper its proxy, so the build is never without a working helper (the hub accepts v1 and v2 for exactly 4-02 and 4-03); and the Rust lab game (4-06) and the job host (4-07) come before the dispatcher, so the dispatcher's first executor is the same code its remote executors run.

---

## Appendix E: Phase 4 work breakdown

Each package is sized for one implementation session (a Phase 3 package or smaller: about 1,800 lines of code at most besides its tests, as 2-11 was), starts from the previous package's merge, and ends with its gates passing, then a review and a fix round. The standard gates S1-S10 (P4.7.5) are named where they apply; every other gate is local and can be rerun by a reviewer, and CI-only gates are named as such. Every route a package adds or changes gets the tests of P4.7.3 whether or not its gates name them, and every package keeps the safety rules of P4.7.1.

### 4-01: Protocol v2: the specification, the frame types on both sides, the shared fixtures

- **Depends on:** nothing (`v0.1.6` at `1d5ca71`)
- **Estimated size:** ~700 Rust (the crate's skeleton, `protocol.rs`, the matcher, the fixture runner) + ~450 Python (`helper_protocol.py`, the matcher, the fixture loader) + ~60 fixture files + PROTOCOL.md
- **Scope:** `crates/citar-helper` as a workspace member (library `citar_helper` and a binary that prints `--version` only), its row in the crate graph (and `citar-jobs`'s, reserved for 4-06), the xtask rules of P4.3.1 (no `unsafe` in the helper; its test features never in shipped builds; the loopback guard itself is 4-03's code), `crates/citar-helper/PROTOCOL.md` (P4.2 in full: every frame, its fields and types, who sends it and when, the reactions to `refused`, the matcher language), `protocol.rs` (every frame as a serde type; unknown fields ignored; the 8 MB cap), fixtures under `crates/citar-helper/protocol/v2/` (valid and invalid frames for every type; transcripts for the handshake, settings, catalog, completions and cancel, jobs and updates, written now, played by each side as its package arrives), `citar/server/helper_protocol.py` (builders and parsers for every frame, with per-type checks; `ProtocolError`), `tests/test_helper_protocol.py`, and the frame table parsed from PROTOCOL.md on both sides. Docs: none beyond PROTOCOL.md.
- **Gates:**
  1. Every valid frame parses on both sides and re-serializes to an equal JSON value; every invalid frame is refused on both sides with the reason its fixture names.
  2. The frame types each side knows equal PROTOCOL.md's table (a test on each side).
  3. Robustness: 2,000 random mutations of valid frames (keys dropped, types swapped, deep nesting, huge numbers, NaN-like strings) never panic the Rust parser (proptest) and raise nothing but `ProtocolError` in Python.
  4. The transcripts parse on both sides and their matchers are exercised by unit tests (each matcher accepting and refusing).
  5. S1-S4, S7, S9.

### 4-02: The hub on v2: the handshake, keys, settings, raw completions parsed on the server, acknowledged cancel, the schema

- **Depends on:** 4-01
- **Estimated size:** ~1,300 Python (revision 200, hub 450, `completion.py` and `WorkerConversation` 300, scheduler 100, routes and records 150, JS 100) + ~800 tests (the `FakeHelper` 250 among them). At the upper bound: if it runs long, split after the schema and the parser (4-02a: revision, `completion.py`, parsing parity) and before the hub (4-02b).
- **Scope:** revision `0016c_helper_jobs` (P4.5.4, every Phase 4 table and column) and `models.py` to match; `VERIFICATION` gains `rejected`. The hub (P4.5.1): v2 beside v1 (v1 unchanged, until 4-04), the rate limit before lookup with `ratelimit.client_ip`, `refused` frames, key registration and its refusal, `welcome` with settings, `hub.push_settings` from the settings route, `settings_ack` into the scheduler's capacity, `catalog`, `hardware`, `power` (the power rows), `status` (quiet hours into the scheduler), `request`/`done`/`error`/`cancel`/`cancelled`, `last_ip`. `completion.parse_completion` extracted from `OpenAIConversation.step` (P4.5.2) and used by both providers; `WorkerConversation` on v2 (OpenAI-format tools, the JSON protocol in `json` mode, usage mapping) and on v1 as before. The scheduler's acknowledged cancel and `flags.cancel_unacked` (P4.5.3). `POST /api/machines/{id}/key/reset` and `/rescan`. The card shows protocol, helper version and key fingerprint. `tests/helperfake.py` (`FakeHelper`: plays transcripts against the real hub over a real socket) and `tests/test_helper_conformance.py` for the handshake, settings, catalog, completion and cancel transcripts. Docs: `API.md`, `ARCHITECTURE.md` (the hub on v2), `WORKERS.md` (revocation, key reset).
- **Gates:**
  1. The transcripts for the handshake (including every `refused` code), settings, catalog, hardware, power, status, completions and cancel pass from the hub's side.
  2. **Parsing parity:** a corpus of raw completions (native calls; invalid JSON arguments; merged XML repaired; XML, GLM, plain and Gemma 4 text calls; the JSON protocol; reasoning; no tool call) gives equal `StepResult`s and stored assistant entries through `OpenAIConversation` (a fake SDK response) and through `WorkerConversation` (a `FakeHelper`'s raw message); a `json`-mode seat's request through a helper carries `JSON_PROTOCOL` and the tool listing (the plan's defect, failing on the code before).
  3. **Cancel:** a `FakeHelper` acknowledging within 50 ms frees the slot without draining (a ticket waiting behind it starts within 200 ms; the scheduler's `draining` counter stays 0); one that never acknowledges drains as in P3.3.3 and sets `flags.cancel_unacked`.
  4. **Keys:** the first hello registers its key; the same key reconnects; another key is `refused key` until the owner's reset (one audit row), after which it registers; the routes' access tests (stranger 404, visitor 401, grantee 403, moderator 404, owner and admin 200).
  5. **The rate limit:** 20 bad tokens from one address, then the 21st attempt with a good token is `refused rate` and `authenticate` is not called (patched); with `behind_proxy` off, a forged `X-Forwarded-For` does not change the address the limit counts.
  6. v1 still serves: `tests/test_worker.py`'s end-to-end tests pass unchanged; `qa_phase3.py queue machines` green.
  7. `tests/test_migrations.py`: `upgrade head` from `0016b`, `compare_metadata` clean; one line of revisions from the baseline.
  8. The hub's overhead (P4.7.9) under 3 ms; the session's overhead under 2.87.
  9. S1, S1b, S2-S6.

### 4-03: The Rust helper: configuration, the connection, keys, settings, catalog

- **Depends on:** 4-02
- **Estimated size:** ~1,300 Rust (cli and config 350, home and log 150, session and reconnect 400, key 100, discovery and catalog 200, quiet 100) + ~500 Rust tests + ~300 Python end-to-end tests
- **Scope:** P4.3.2, P4.3.3, P4.3.7 (the key; signing comes with 4-10) and the discovery half of P4.3.4: `helper.json`/`worker.json`/`CITAR_WORKER_*`/flags compatibility (first recording `build_config`'s answers for a table of configurations into `tests/data/helper_config_cases.json` from the Python worker, which still exists), `--home`, the first-run prompt and `setup`, quiet hours with v1's parser, `--ca-file` and the restricted `--insecure`, the redacting log, the session with tokio-tungstenite and rustls' platform verifier, the handshake and every `refused` reaction, reconnect with jitter, the helper's own ping, `shutdown`, the machine key (generated, 0600, refused when too open), `settings`/`settings_ack` with the local ceiling, model discovery for the four endpoint kinds and `catalog` on change, `status` for local quiet hours, `--hardware FILE` and `--no-hardware` (the Rust scan is 4-05's: until then a helper without a file reports none). Until 4-04 the helper's hello says `llm.enabled: false` and `cpu.enabled: false`, so the server sends it nothing it cannot do. The loopback guard for test builds. `cargo xtask develop` builds the helper into `citar-ext/`, and so does test.yml's `build-ext` job, so the end-to-end tests run on CI's three OSes too (CI-only). `tests/test_helper_e2e.py` (the real binary against the real hub). Docs: `WORKERS.md` (the helper's configuration and first run), `CONTRIBUTING.md` (building the helper, WSL).
- **Gates:**
  1. **Compatibility:** every case of `helper_config_cases.json` (helper.json alone; worker.json by `--config`; environment only; flags over file over environment; quiet strings across midnight and by weekday; the model list; `--no-hardware`; a missing token or server) gives the helper the same effective settings as the Python worker gave.
  2. The session transcripts pass from the helper's side (Rust), with `refused token` stopping it, `protocol` waiting (10 minutes, checked on a fake clock), `rate` retrying after its delay, `key` stopping with the message, `shutdown` stopping, a dropped socket reconnecting with backoff.
  3. **End to end** (Windows, then WSL): the binary connects to a dev server, registers its key (the fingerprint the card shows equals `--version`'s), acknowledges settings with its ceiling (a site figure above it is capped and listed), reports a fake endpoint's models and a change of the loaded set within 35 s, is dropped within a second by a revoke and does not reconnect, reconnects by itself after a server restart.
  4. **Isolation:** a run with `--home <tmp>` leaves every file outside `<tmp>` as it was (the S10 fingerprints, taken by the test itself); a test-feature build refuses `--server https://example.com` before dialling.
  5. S1, S1b, S7, S9, S10.

### 4-04: The LLM proxy and real cancel; the Python worker retired

- **Depends on:** 4-03
- **Estimated size:** ~900 Rust + ~700 Python (the endpoint's streaming, `qa_phase3.py`'s helper switch, the retirement, the v1 refusal) + ~600 tests; deletes ~1,600 lines of the Python worker and its tests
- **Scope:** the proxy of P4.3.4 (streaming with delta assembly, the non-streaming path, whitelisted params, the allowlist, local slots and quiet hours, timeouts, error codes, `done` with the raw message, `gen_s` and `ttft_s`) and real cancel with `cancelled`; the hello's `llm.enabled`. The scripted endpoint streams and records disconnects (P4.7.6). **Retirement:** `citar/worker/` and `installer/helper_entry.py` deleted; the `worker` extra and `citar-worker` script removed from `pyproject.toml`; `citar worker` and `citar setup --worker` print where to download the helper and exit 2; `install.sh --worker` and `install.ps1 -Worker` do the same (downloading it is Phase 5's); the hub's v1 path replaced by P4.2.8's refusal; `tests/test_worker.py` split into `tests/test_hub.py` (what remains true of the hub) and the end-to-end tests of the Rust helper; `qa_phase3.py`'s helpers become the Rust binary with `--home` in the QA directory. `qa_phase4.py` with its parts `helper` and `llm-real`. KNOWN_ISSUES: "A cancelled request still runs on a helper" goes. Docs: `WORKERS.md` (the helper replaces the worker; the runbook of P4.2.8), `CONFIGURATION.md`, `CHANGELOG.md`.
- **Gates:**
  1. The completion and cancel transcripts pass from the helper's side: streamed and not, tool-call and reasoning deltas, usage present and absent, `busy`, `closed`, `unknown_model`, `timeout`, an endpoint error, cancel before the first token, mid-stream and after `done`.
  2. **End to end through the scheduler:** a lease's steps and a ticket's through the Rust helper are parsed on the server (a seat plays three turns with tool calls from the scripted endpoint); a cancelled ticket's slot is free within 1 s with no draining; the endpoint's log shows the client gone within 1 s of the cancel and no token sent after it.
  3. **v1:** a v1 hello (a fixture played by a `FakeHelper`) gets P4.2.8's message in a v1 `error` frame and close 4426, with no token lookup and no audit row.
  4. `qa_phase3.py queue machines exit` pass with the Rust helper.
  5. `qa_phase4.py llm-real`: LM Studio lists `google/gemma-4-e2b` loaded before and after; a seat's prompt with tools gets a call the server parses; a 1,500-token generation cancelled at 2 s is acknowledged within 1 s, and three short requests after it take no longer than 1.5 x the median of three before it plus 1 s.
  6. No `citar.worker`, `citar-worker` or `PROTOCOL_VERSION = 1` left in the tree outside the archive notes (a grep gate).
  7. S1, S1b, S2-S7, S9, S10; the session's overhead under 2.87.

### 4-05: The hardware scan and power samples in Rust

- **Depends on:** 4-04
- **Estimated size:** ~900 Rust + ~250 Python + ~400 tests
- **Scope:** P4.3.5 (the scan in the `citar-hardware` format, the power guess ported, runtimes, caching, rescans, `hardware` frames on change, `scan [FILE]`, power sampling and `power` frames, `--no-power`); the hub's power rows and costing reading them for helper machines (P4.5.7); the dialog's "Rescan now" (the route is 4-02's). KNOWN_ISSUES' energy item narrowed. Docs: `WORKERS.md` (what a scan reports and how to keep it private), `CONFIGURATION.md`.
- **Gates:**
  1. On Windows and in WSL, a scan kept by `valid_scan` loses no field (its cleaned copy equals it), and its cores, threads and RAM (within 1%) equal `hwinfo.collect()`'s on the same machine; the power guess equals `guess_power` on a table of twelve recorded `hwinfo` outputs (committed).
  2. A `rescan` brings a `hardware` frame within 5 s; an unchanged scan sends none; `--hardware FILE` (a UTF-16 file too) and `--no-hardware` behave as in v1.
  3. With a fake `nvidia-smi` first on `PATH` (a test script printing fixed watts), a minute of samples lands as the machine's power rows and a helper machine's costing uses them.
  4. CI-only: the scan on macOS arm64 and x64 and Linux arm64 is kept whole (rust.yml's macOS leg now; helper.yml from 4-12).
  5. S1, S1b, S7, S9, S10.

### 4-06: Jobs in Rust: the vocabulary, the lab game, reference games, the chain and the result hash

- **Depends on:** 4-05
- **Estimated size:** ~1,200 Rust + ~300 Python + ~500 tests
- **Scope:** the `citar-jobs` crate (P4.4.1, P4.4.2, P4.4.4 without signing, P4.4.5): `LabSpec`, `ReferenceSpec`, validation and bounds, canonical bytes and `spec_hash`; `lab::play` with the chain on; reference games through citar-sim's `play_one`; crash outcomes; the canary; `result_hash`, `chain_check`; bindings in citar-py (`citar._engine.jobs`: `check_spec`, `run`, `result_hash`, `chain_check`) and `engine_api` wrappers; `lab.play()` on `engine_api.run_job` (first recording the Python results of six seeds as `tests/data/lab_results_*.json`, then deleting the Python result building); the `jobs` golden set. Docs: `ARCHITECTURE.md` (jobs), `BOTS.md` (what a lab result records, unchanged keys).
- **Gates:**
  1. **Parity:** for six seeds (duels and small games, 40 rounds), the Rust `LabResult` equals the recorded Python result on every deterministic key.
  2. **Determinism:** one job twice in one process, in two processes, and in the `ci` and `release` profiles gives one chain head and one result hash; `cargo golden check` passes with the new `jobs` set (blessed once, the bless commit saying why); WSL gives the same (S9); CI-only: determinism.yml on five targets.
  3. Specs with an unknown key, a value out of bounds, an unknown bot version or a hash that does not match are refused with their reasons, through Rust and through the binding.
  4. A result altered in one score, or a digest list altered in one round, fails `result_hash`/`chain_check` through the binding.
  5. Speed (P4.7.9): a lab duel through `run_job` no slower than through the old `lab.play`, same run.
  6. S1, S1b, S7, S9.

### 4-07: The job host and CPU limits per OS

- **Depends on:** 4-06
- **Estimated size:** ~1,000 Rust (the host 450, limits 350 with the `unsafe` module about 150, the parent side 200) + ~250 Python + ~500 tests
- **Scope:** P4.4.3 and the limits table of P4.3.6: `citar_jobs::host` (the JSON-lines loop, threads, abort, pause and resume, deadlines, progress, heartbeat, `ready`, the canary's refusal, stdin EOF), `limits::apply` per OS with its `unsafe` module and the xtask rule confining it, `host::spawn` (with the `systemd-run --user --scope` wrapper where available), `citar/jobs/host.py` and `python -m citar.jobs host`, the helper's `job-host` subcommand, the `test-jobs` kinds (`burn`: spin for N seconds; `alloc`: allocate in steps up to N MB). Docs: `WORKERS.md` (limits per OS, hard and soft).
- **Gates:**
  1. **Windows** (local): a host with `cpu_percent` set to 10% of the machine and two `burn` jobs for 8 s uses, by its own process CPU time, at most that share x 1.15 + 0.05 core-seconds per second; with `memory_mb` 200, an `alloc` job to 400 ends as `job_failed memory`, the parent restarts the host and the next job runs; `ready` reports hard, hard.
  2. **WSL:** under the scope the host reports hard, hard and the same two checks pass; with the wrapper turned off (a test switch) it reports soft CPU and hard memory (`RLIMIT_DATA`), and the `alloc` job still ends cleanly.
  3. Pause: a paused run's round does not move for 2 s, then resumes; abort ends a run within 1 s; a host whose parent dies exits within 2 s, leaving no process (checked by pid).
  4. Every limit test stays within P4.7.1's bounds (at most two cores' worth, 10 s, 512 MB).
  5. CI-only: macOS reports soft, soft and its background priority (rust.yml's macOS leg).
  6. S1, S1b, S7, S9, S10.

### 4-08: The dispatcher and the local executor; results into the lab, the ratings and the ledger

- **Depends on:** 4-07
- **Estimated size:** ~1,400 Python (store 250, dispatcher 500, local executor 200, lab changes 150 and deletions, ledger 100, routes 100, QA part 100) + ~200 JS + ~800 tests. At the upper bound: if it runs long, split after the store, the dispatcher and the local executor with results and the ledger (4-08a) and before the routes, pages and QA part (4-08b).
- **Scope:** P4.5.5, P4.5.6 (local), P4.5.7: `citar/jobs/{store,dispatcher,local}.py`; jobs from the lab's queue with the experiment `kind` (`lab`, `ranking`, `reference`; the last two admins' and `collection`); the host's CPU defaults; quiet hours; benchmark leases; deadlines and retries; crashes; results appended with `executor`; reference lines; experiment completion; ledger events per run (P4.5.7) and `ingest_lab` removed; the old runner deleted (P4.5.6) and `citar lab run` retired; `stop`/`resume`; `lab.overview()` from the dispatcher; `GET /api/lab/jobs`, `GET /api/admin/jobs`; the Lab page's per-game executor; the Queue page's CPU jobs for the host; `qa_phase4.py lab`. Docs: `BOTS.md`, `BOT_TUNING.md` (how to run the lab now), `API.md`, `CONFIGURATION.md` (the host's CPU settings), `ARCHITECTURE.md`.
- **Gates:**
  1. An experiment of six 30-round duels runs to its end on the local executor: six result lines, each with `executor.machine` the host and `verification: reference`; six `cpu` events with the experiment's owner as consumer and no provider; the ratings include its games; the experiment moves to `done/`.
  2. A ranking and a reference experiment (an administrator's) run too: `collection` (no consumer), the reference lines in `saves/lab/reference/` read by `summarize.py`.
  3. **Restart:** the server killed with two runs in flight comes back and re-runs exactly those two; no duplicate line in the results file.
  4. Priority, round-robin across owners, `stop` (running runs finish, nothing new starts) and `resume`.
  5. The old runner is gone: no `taskkill`, `Get-CimInstance`, `_Adopted` or `lab/jobs` paths left (a grep gate); the lab's tests updated.
  6. Access tests for the two routes; the dispatcher's tick under 50 ms (P4.7.9).
  7. `qa_phase4.py lab` passes; browser H3 (the host part) and H4 (executors).
  8. S1, S1b, S2-S6, S10.

### 4-09: Remote executors: CPU jobs on helpers

- **Depends on:** 4-08
- **Estimated size:** ~700 Rust + ~800 Python + ~500 tests
- **Scope:** P4.3.6 on the helper (the job host as its child under the effective limits; relaying job frames; `yield_to_llm`; deadlines; results kept across reconnects; rejects), the hello's `cpu` block; the hub's job frames to the dispatcher; the dispatcher's remote executor adapter and eligibility (P4.5.5: build, kinds, quiet hours, benchmark leases, grants with the purpose `lab`, owners' own machines, `collection` opt-in), abandons. **Until 4-10, remote work is offered only to machines an administrator set `trusted`** (the verification machinery lands next), and their results are accepted `unverified`. Ledger events with the machine's owner as provider. The card's and dialog's CPU in effect, hard and soft; the dialog's note goes; `yield_to_llm`; `GET /api/machines/{id}/jobs`. `qa_phase4.py remote` (and `--wsl`). Docs: `WORKERS.md` (lending CPU), `API.md`, KNOWN_ISSUES ("CPU sharing waits" goes; macOS's soft limits added).
- **Gates:**
  1. The job transcripts pass from both sides: offer, accept, progress, result; reject for build, kind, quiet, busy and a bad spec hash; abort; a deadline; a reconnect that keeps a running run and one that drops it.
  2. **End to end on Windows:** a trusted helper machine with 2 threads, 20% CPU and 512 MB runs eight 30-round games of another member's experiment (that member given `run_lab` by an administrator, and a grant with the purpose `lab` on the machine's group): eight lines with `executor.machine` that machine, eight events given by its owner and used by the experiment's owner; the dialog shows the effective limits, hard; the helper's job host's CPU over the run stays under its cap plus slack.
  3. **Yield:** while a request streams at the scripted endpoint, the running game's round stops moving, and moves again within 2 s of the `done`.
  4. Eligibility: one test per refusal reason (offline, CPU off, kind not opted in, another build, quarantined owner, quiet hours, a live benchmark lease, no grant, a grant without `lab`), each leaving the job queued for another executor.
  5. `qa_phase4.py remote --wsl`: the same experiment with the server in WSL and the Windows helper (topology of P4.7.6), or the fallback, recorded.
  6. S1, S1b, S2-S7, S9, S10.

### 4-10: Proof of work: signed claims, verification, trust tiers, quarantine, withdrawal, the admin UI

- **Depends on:** 4-09
- **Estimated size:** ~500 Rust (signing, `verify_claim`, the faults) + ~1,100 Python (trust 400, dispatcher's verification 300, ledger 100, routes 150, withdrawal 150) + ~400 JS + ~800 tests. At the upper bound: if it runs long, split after the claims, the verification and the tiers with quarantine and withdrawal (4-10a) and before the admin page, the card's trust display and the docs (4-10b).
- **Scope:** P4.3.7 (signing), P4.4.4 (claims, `verify_claim`), P4.4.6 (`test-faults`), P4.5.8 entirely (replication, keyed sampling, verifiers, settling, mismatches with evidence and the tiebreak, promotion, quarantine, withdrawal, the ledger's `pending`/`verified`/`unverified`/`rejected` with rejected credits at 0 and rollups recomputed), the end of 4-09's "trusted only" rule, the routes of P4.5.10 for verification and replication, `#/admin/verification`, the card's trust, evidence and admin controls. Docs: `SECURITY.md` (P4.6), `WORKERS.md` (verification, what a lender's tier means), `ARCHITECTURE.md`, `API.md`, KNOWN_ISSUES.
- **Gates:**
  1. **Unit:** the tier machine's table (each transition, each threshold, release, overreport blocking promotion); sampling within ±3% of each rate over 10,000 job ids and different under another secret; claims verify, and a changed byte in any claim field, the result, a digest or the signature fails with its reason.
  2. **Caught while new:** a helper with `--test-fault result:1` is quarantined at its first job, with the verdict "same chain, different result"; its result never reaches the results file; its event is `rejected` with no credits. `digest:12`'s evidence names round 12; `sig`'s is a bad signature; `abandon:1` re-queues the job and counts an abandon.
  3. **Caught by sampling, with withdrawal:** with `provisional_rate` 0, an honest-then-faulty helper (`result:6`) reaches `provisional` after five matches and has jobs 6-8 accepted `unverified`; with the rate set to 1.0, job 9 is replicated and mismatches; it is quarantined, lines 6-8 leave the results file, those jobs run again elsewhere and are accepted, their old events are `rejected`, and the ratings no longer count them.
  4. **Promotion:** an honest helper with the thresholds lowered to 3 and 6 goes `new` → `provisional` → `trusted`, with an audit row each.
  5. Quarantine refuses the machine's work for other members (a pooled ticket from another member refused `quarantined`; its owner's own still served); release returns it to `new`.
  6. Access tests for every new route; `qa_phase4.py verify`; browser H5, H1 (trust and key).
  7. S1, S1b, S2-S7, S9, S10.

### 4-11: Signed self-update

- **Depends on:** 4-10
- **Estimated size:** ~800 Rust (the updater 500, xtask commands 300) + ~300 Python (the relay) + ~400 tests
- **Scope:** P4.3.8 and P4.5.9: the manifest and its signature, `update-keys.pub` (empty), `test-update-key` (`--update-key`, `CITAR_HELPER_VERSION`), the helper's update steps with `--self-test`, `self-replace`, re-execution or exit 75, rollback, `update_status`; `cargo xtask keygen`, `helper-manifest`, `sign-manifest` with their refusals; the server's relay with `helper_manifest_url` and `helper_manifest_file`, `engine_api.verify_manifest` and the test-ops key; welcome's and `update` frames; the card's update state; `qa_phase4.py update`. Docs: `WORKERS.md` (updates and how to turn them off), `SECURITY.md` (the update channel), `CONFIGURATION.md`.
- **Gates:**
  1. **An update with a throwaway key** generated in the test's temporary directory: a helper built as `0.1.6-test.1` and copied into a temporary install directory (never the build output) is offered `0.1.6-test.2` (a second build) by a local server whose manifest file was signed with that key and whose asset is served on loopback; it downloads, checks, self-tests, swaps, re-executes and reconnects reporting the new version and `applied`.
  2. **Refusals,** each leaving the installed binary's sha256 unchanged and reported `refused` with its reason: a bad signature, an unknown key, a wrong sha256, a wrong size, a downgrade, the same version, a non-loopback `http` URL, a missing target, policy `off`; `notify` reports `available` and downloads nothing; a build with no trusted key reports `unavailable`.
  3. A staged binary that fails `--self-test` is not swapped; in a console run, a swapped-in binary that exits before reporting healthy (a test build made to exit at start) is swapped back, the old process keeps serving and reports `failed`; `update rollback` restores `.old` by hand.
  4. The relay: fetched at most hourly, a failed fetch keeps the last good one, an unverifiable manifest is never relayed; welcomes carry it.
  5. `xtask keygen` and `sign-manifest` refuse paths inside the repository; after the gates, `git ls-files` and a scan of the tree find no private key (no `*.key`, no ed25519 secret JSON, no `PRIVATE KEY` header).
  6. S1, S1b, S7, S9, S10.

### 4-12: Service installation, the release names, helper.yml, the helper docs

- **Depends on:** 4-11
- **Estimated size:** ~700 Rust + ~250 workflow YAML + ~100 JS and Python + docs
- **Scope:** P4.3.9 (`service install|uninstall|status|run` with `--user`, `--system` and `--dry-run` per OS; `run --detach` on Windows; exit 75 as the restart); release.yml's `build-helper` rewritten as P4.10's cargo matrix with the five unversioned names and their `.sha256` (it stays behind the Phase 5 gate); `helper.js` with five entries and no pipx sentence; `citar/web/tests/helper.test.mjs` holding `helper.js`'s names equal to the `asset:` entries of release.yml's helper matrix (read by pattern: the project has no YAML dependency, and the matrix lines are one per target); `.github/workflows/helper.yml` (build and run the helper's and citar-jobs' tests, the conformance transcripts and the `jobs` golden set on the five targets; Windows service install, start, connect to a loopback fake server, stop, uninstall; Linux `systemd --user` after linger; macOS launchd); `packaging/README.md` (what Phase 5 does for the helper, R4); KNOWN_ISSUES (Intel Mac item goes); `WORKERS.md` assembled into its final shape (the helper, CPU sharing, limits, verification, updates, services, troubleshooting, the Phase 6 runbook). Docs: `INSTALL.md` and `QUICKSTART.md` where they name the worker.
- **Gates:**
  1. `--dry-run` on each OS prints exactly the committed expected definitions (unit file, plist, `sc.exe` arguments, the Run value), checked by unit tests that run on every OS.
  2. **WSL:** `service install --user` starts the helper against a WSL dev server; it connects; `service status` says running; `uninstall` stops and removes it; `systemctl --user list-unit-files` shows no citar unit after.
  3. **Windows, nothing installed:** `service install --system` unelevated prints the elevated command and exits 2; `--user` only ever with `--dry-run`; S10's service, Run-key and task checks clean.
  4. The names test passes; `helper.yml` and `release.yml` parse; no feature on any build line of either (xtask).
  5. CI-only: `helper.yml` green on the five targets, its service legs included (run at the merge push).
  6. Browser H6; S1, S1b, S4-S7, S9, S10.

### 4-13: Phase 4 exit: conformance, the remote experiment verified, the faulty helper, the real model, the evidence

- **Depends on:** 4-12
- **Estimated size:** ~800 Python (the `exit` and `real` parts, `all`) + docs
- **Scope:** `qa_phase4.py all` with `exit` and `real` (P4.7.6); the browser checklist; the P4.1.4 table measured; the docs pass: `CHANGELOG.md` (Compatibility: protocol v2 and the v1 refusal, the Python worker removed, the lab inside the server, new routes; Added and Fixed by area, including P4.0's defects), `KNOWN_ISSUES.md`, `ARCHITECTURE.md` (Phase 4 in one section), `CONTRIBUTING.md` (`qa_phase4.py`, WSL, the helper's dev loop), `API.md`, the wiki regenerated; "As built in 4-13" with the evidence.
- **Gates:**
  1. **Conformance on Windows x64 and Linux x64:** S7 and S9 green at the head; the transcripts from both sides; `tests/test_helper_e2e.py` on Windows and in WSL; CI-only: `helper.yml` and determinism.yml on all five targets at the merge push.
  2. **A remote lab experiment verified end to end** (`exit`, P4.7.6's topology): two members, bob given `run_lab` by an administrator and alice granting him `lab` on her Windows helper machine (2 threads, 25%, 512 MB) and her WSL helper machine (2 threads, 50%, 512 MB); bob's experiment of 24 games of 40 rounds, an administrator's ranking experiment of 6 and reference run of 4, all dispatched across the two helpers and the local executor. Every accepted line is signed by its machine (or the host's), its chain consistent, replicated while its machine was new and sampled after; the replicated games' Windows and Linux results equal byte for byte; each game is in the results file once; the ratings include exactly the accepted lab and ranking games; the ledger's events by machine and verification equal the runs, alice's given and bob's used equal their sums, replicas used by nobody; the limits held (hard on both) and were shown so.
  3. **A faulty helper quarantined:** a third helper (WSL, `--test-fault result:3`) joins as new and is quarantined at its third job with the evidence shown on `#/admin/verification`; in a second pass (P4.5.8 rule 7, `provisional_rate` 0 then 1.0) a fresh faulty helper reaching provisional is caught by sampling and its unverified results withdrawn, re-run and accepted elsewhere, with the ledger and ratings following.
  4. **The real model** (`real`): LM Studio's `google/gemma-4-e2b` (loaded before and after; never loaded, unloaded or swapped) through the Windows helper: a game of 6 rounds with alice's person seat, an LLM seat and a hybrid seat (prompted every 2 turns) and two bots; the LLM seat's turns end by its own `end_turn`; at least two chats answered by the models' own calls, parsed on the server; the ledger's generation seconds equal the scheduler's; a long generation cancelled at 2 s acknowledged within 1 s, the next requests within the same-run baseline (4-04's rule), no slot drained.
  5. **Self-update** with a throwaway key (`update`) and **the service** in WSL (`service`) pass inside `all`.
  6. `qa_phase4.py all` exits 0; `qa_phase3.py all --skip real` exits 0 with the Rust helper (Phase 3's regression).
  7. The browser checklist H1-H6, every item with its screenshots.
  8. The P4.1.4 table, each row measured, in the as-built note; S1, S1b, S2-S7, S9, S10 at the head; the session's overhead; CI-only: test.yml, rust.yml, determinism.yml, helper.yml at the merge push.
