# Changelog

Notable changes to CITAR. The format follows [Keep a Changelog](https://keepachangelog.com), and
versions follow [semantic versioning](https://semver.org) — with the pre-1.0 caveat that tool names
and arguments may still change between minor versions, and the release notes will say when they do.

## [Unreleased]

### Compatibility

**The game engine is new.** The rules, the scripted bot and the headless runner are Rust
(`crates/`), reached from Python through the extension `citar._engine`; the Python engine, the
Python bot and its frozen snapshots are removed (the tag `python-engine-0.1.6` keeps them). What
that means for an existing install:

- **Saved games and scenarios of 0.1.5 and earlier do not load.** They are the Python engine's
  format; loading one is refused with "saved by the Python engine; archived with 0.1.5", and a
  game left running by 0.1.5 is not brought back on the first start. Saved maps (the editor's
  documents) still load.
- **Frozen bot snapshots are gone.** The `frozen_<hash>` copies of the Python bot, and the
  built-in profiles v0, v1 and 22 Sep that played them, were archived with 0.1.5. The bot is now
  a compiled version (`basic-1`, `idle`; `basic` is the latest), which the lab pins when an
  experiment is queued. A saved profile on a snapshot is still listed, read-only, can be deleted,
  and is never the Best bot; a seat or experiment that names a snapshot is refused as archived.
- **The ladder starts empty.** 0.1.5's lab history does not carry over: results left in
  `saves/lab/results` record no build or bot version, so they are never rated.
- **Building from source needs a Rust toolchain** (rustup; `rust-toolchain.toml` pins the
  release), since installing from a checkout builds the extension. Wheels and the Docker image
  carry it compiled and need none.
- **Modding moved.** The ruleset is compiled into the engine from `crates/citar-engine/data/`
  (it was `citar/data/`). A modded copy plays without a Rust toolchain: check it with `citar
  ruleset check DIR` (which replaces `scripts/check_uniques.py` and `scripts/check_refs.py`) and
  point `CITAR_RULESET_DIR` at it. Its games, saves, build id and bot fingerprints carry its own
  ruleset id; a save that names something another ruleset lacks is refused there.
  [docs/MODDING.md](Modding#playing-a-modded-ruleset).
- **For code on `citar.engine_api`:** `CITAR_ENGINE`, `BACKEND`, `BackendError` and
  `EngineGame.python_game` are gone, and `check_ruleset` is new. A bot has no seed of its own: its
  draws are keyed by its game's seed, and `bot_instance` and `profiles.make_bot` no longer take a
  `seed`.
- **The database starts afresh.** 0.1.6's schema is one new baseline. On the first start a SQLite
  database of 0.1.5 (or of an earlier 0.1.6 build) is renamed aside, with its `-wal` and `-shm`, to
  `citar-0.1.5-<date>-<time>.db` in the same folder, and a new one is made: nothing is changed or
  deleted, and a rename that fails (another program has the file open) refuses the start, naming the
  file. A PostgreSQL database of an older version, or a database of a newer one, refuses the start.
  Accounts, sessions, machines, groups, grants, budgets and sharing start empty: make the first
  administrator again (`citar admin create-user`, or the setup page). Saved games stay on disk and get
  their place in the games list back as they are loaded.
  [docs/server/ACCOUNTS.md](Design-accounts-and-sharing).
- **`config/servers.json` is imported once.** The machines, electricity plans and keys it lists
  become rows of the database, under their own ids, when the first administrator exists; the file is
  then renamed `servers.json.imported-<date>-<time>` and never read again. Every machine, the server's
  own, one it dials and a member's helper machine alike, is on the one Servers page; `#/servers` opens
  it. `POST /api/pool/import-registry` is gone. [docs/CONFIGURATION.md](Configuration).
- **Visibility is `private | allowlist | link | server | global`.** `public` is now `global` (the
  public web: anyone, signed in or not), and its capability `publish_public` is `publish_global`;
  `server` is new (every member of this server). A moderator may unpublish what is global or shared by
  link, and nothing else.
- **A member's model seat names a machine.** A seat, a scenario's seat, a suite's model or a probe's
  subject from a member that carries a base URL, a key or a key variable is refused (400); an
  administrator's, and the dry run, may still describe one. `POST /api/llm/models` with a base URL is
  an administrator's. Machine routes that act on the server's own computer (its hardware scan, LM
  Studio's control, a key held in its environment, host-wide settings) are administrators' alone.
- **Grants name what they cover.** A grant for "Ordinary games" covers a model seat that plays every
  turn and nothing pooled: a report's narrative, a hybrid seat's diplomacy, a probe's offer or message
  case and a machine's test need "Pooled prompts". Budgets are in credits, not money.
- **The Python worker is replaced by the CITAR helper.** A machine serves its models through
  `citar-helper`, a single Rust program (the same download names), speaking helper protocol 2
  ([crates/citar-helper/PROTOCOL.md](https://github.com/jprodgers/CITAR/blob/main/crates/citar-helper/PROTOCOL.md)). A 0.1.6 server refuses a
  0.1.5 worker with one message telling its owner to download the new helper (close code 4426), and a
  0.1.6 helper waits for a 0.1.5 server to be upgraded. The helper reads a 0.1.5 `helper.json` or
  `worker.json` with the same meaning, lends no CPU unless its owner turns that on, and registers a
  machine key with its first connection. `citar worker`, `citar setup --worker`, `install.sh --worker`
  and `install.ps1 -Worker` now say where to download the helper and exit 2; the `worker` extra and the
  `citar-worker` command are gone. A 0.1.5 worker installed as a system service is restarted into the
  refusal every 15 s until it is disabled (`sudo systemctl disable --now citar-worker`):
  [docs/server/WORKERS.md](Workers#moving-from-015).
- **The lab plays each game as a job,** built in Rust by the jobs crate (`crates/citar-jobs`, the code
  a helper lending its CPU will run): the result line holds what 0.1.5's did, with `seconds`,
  `started_ts`, `cpu_s` and `finished` added outside the result's hash, and `executor` (the machine
  and run that played it). A bot that panics now ends its game as a crash: a crash line in the results
  file and in `crashes.log` (the turn, the seat, its label and the message), where 0.1.5 wrote a result
  whose `errors` carried the trace, so `errors` is always empty; and a crash is final, never played
  again. An experiment
  is checked game by game as jobs when it is queued (`citar lab submit`, the A/B form): a turn limit
  past the speed's last turn, a seed past 2^53 − 1, a label or a name of more than 200 characters or
  an override the bot does not take is refused, saying which game and why (`citar lab submit` exits
  2; the A/B form's turn limit stops at the chosen speed's last turn). One queued before that a job
  refuses is shown on the Lab page as one that cannot be played, and none of its games starts.
  [docs/BOTS.md](Scripted-bots).
- **The lab runs inside the CITAR server.** Its job dispatcher plays the queued experiments on the
  server's own job host, under the host machine's CPU settings (on by default: in local mode half the
  logical CPUs, in server mode one thread and min(1,024 MB, a quarter of the memory)); `citar lab run`,
  0.1.5's separate Windows-only runner with its process per game and its orphan adoption, says so and
  exits 2, and `deploy/citar-lab.service` is gone (disable a 0.1.5 one: `sudo systemctl disable --now
  citar-lab`). `citar lab stop` now pauses the lab (the games playing finish, nothing new starts) and
  `citar lab resume` ends the pause; `saves/lab/config.json`'s `workers` and `night_workers`, the job
  files and `status.json` are no longer read. The lab's CPU is in the compute ledger as one event a
  game, used by the experiment's owner (it was charged to the host as `verified`, whoever played it, by
  reading the results files). [docs/BOTS.md](Scripted-bots#the-lab).
- **API changes.** The report sharing routes under `/api/shared/reports` moved to the owned items'
  routes (`/api/items/report/{id}/sharing`) and `/r/{slug}`; `replay_data("full")` and a replay asked
  for in it are refused (the recap reads the `delta` frames); a seat's view no longer carries
  `trade_options` (`GET /api/games/{id}/trade-options/{other}` does); `respond_negotiation` requires a
  `message`; an LLM seat's wait for a chat's answer inside its turn is 60 s (it was 240 s), the chat's
  own time being the lobby's.

### Added

**Accounts, sharing and administration** ([docs/server/ACCOUNTS.md](Design-accounts-and-sharing)):

- Every kind of job has an owner and a visibility: benchmark suites and runs, probes and probe runs,
  lab experiments, scenarios, maps, bot profiles and reports, as games had. Listings show what the
  caller may see; only the owner, those they share it with to manage it, and administrators control
  it. One sharing dialog for all of them (members, by link, by account, published), with Unpublish
  for moderators; a published report has a public page at `/r/{slug}`.
- What a member's report gathers is what they may see, and model scores and comparisons honour each
  owner's pooling choice.
- Profiles: a bio, links and a 256 KB avatar (PNG, JPEG or WebP, checked by its header and served
  without its metadata), each field visible to whom its owner chooses; a members-only directory with
  a way out of it; public profiles at `#/u/<handle>`.
- An administrators' users page (roles, status, capabilities, limits, trust) and an audit log of
  every change, also from `citar admin` (`set-caps`, `set-limits`, `set-trust`, `audit`).

**Machines** ([docs/server/WORKERS.md](Workers)):

- One Servers page for every machine the viewer owns or may use, with its editors as dialogs.
- A machine settings dialog: LLM on or off, slots, model swaps for pooled prompts, CPU sharing limits
  for the next helper; hardware, models and power each `auto` (a helper's report applied when it
  connects) or `manual` (kept as typed), with the last report always kept and its differences shown,
  "Apply" per section or all; the hardware class and, for administrators, a pin and the trust tier.
  Power is manual by default, so a helper's guess no longer replaces measured figures, and a model a
  helper stops reporting is kept, marked. The helper takes `--hardware FILE` (a collector's file).
- Revoking a helper's token, disabling or deleting its machine disconnects the helper at once.
- The hub speaks helper protocol 2: a helper returns each completion raw and the server parses it with
  the parser it uses for every model, so a `json`-mode seat on a helper machine now gets the JSON
  protocol in its prompt (it never did through the 0.1.5 worker). The machine's settings go to its
  helper at once, from the settings dialog or the machine editor (and from another process at the
  next keep-alive), and are acknowledged with what it will do, its own ceiling always winning; a
  machine key is registered by the first connection, shown as a fingerprint in the Helper dialog,
  and reset there (`POST /api/machines/{id}/key/reset`, which makes a connected helper reconnect and
  register its key at once); "Rescan" asks the helper for a new hardware scan
  (`POST /api/machines/{id}/rescan`); the helper's own quiet hours reach the scheduler.
- **The CITAR helper scans the machine itself and samples its power.** Its hardware report is the
  collectors' format, made in Rust (the CPU, memory, GPUs, disks and volumes, the form, its model
  server's models, and the power guess the Python collector makes, with the same answers): at start (a
  scan younger than 6 hours is reused, unless the machine restarted since), every 6 hours, sent only
  when the machine changed, and at once when the machine settings dialog's **Rescan now** asks.
  `citar-helper scan [FILE]` prints or writes it, as the helper with its settings would send it, to see
  what is sent or to import it by hand; `--hardware FILE` and `--no-hardware` work as before. Every
  10 s it reads the GPUs' draw and the CPU's utilisation, and once a minute sends them: the server
  writes them as the machine's power rows (one a minute per machine at most, never overlapping), so a
  helper machine's energy is measured, not estimated, except a GPU's whose draw does not read
  (`--no-power`, or the machine's Live sampling off, sends or keeps none).

**Shared compute** ([docs/server/WORKERS.md](Workers),
[docs/CONFIGURATION.md](Configuration)):

- The prompt scheduler: every model call of a machine waits its turn on that machine, so no machine
  is sent more requests than it has slots, and a busy one queues instead of failing or being
  retried in a loop. A seat that plays every turn holds a lease; pooled prompts (chat answers, a
  hybrid seat's diplomacy, probe answers, report narratives, a machine's test) take tickets, by
  priority, then in turn between members, then in order, on loaded models only unless the owner
  allows swaps, and never while a benchmark holds the machine. A cancelled request on a helper stops
  its generation: the helper closes its stream from the model server and acknowledges, which frees
  the slot at once; a helper that does not acknowledge within 5 s leaves the slot draining and its
  machine flagged. Speed figures use generation time, never time spent queued.
- The Queue page lists each machine's leases and pooled prompts with their places, naming only what
  the viewer may see, with priority and cancel controls for one's own work.
- The compute ledger: every model step and unit of host CPU is an event with who used it and whose
  machine gave it, priced in credits by the machine's hardware class (administrators set the
  classes and their rates); use of one's own machine counts on neither side. The Account page's
  Compute card, a ratio of given to used on the profile (private unless its owner shows it), and the
  administrators' users page's 30 days of compute. Budgets are in credits, reserved and settled per
  request, and a hard budget stops at its limit, an online API's too (a model with no price is
  refused under one).
- **CPU jobs in Rust** (`crates/citar-jobs`, design/phase4-helper.md P4.4), what a helper lending its
  CPU will run: lab, ranking and reference games as closed, typed and bounded specs with their
  hashes, a chain of round digests, the result hash, whether a result answers its spec, and the jobs
  build, the compatibility key; in Python `engine_api.check_job_spec`, `run_job`, `job_result_hash`,
  `job_result_fits`, `job_chain_check` and `jobs_build`. A `jobs` golden set holds every target to
  the same results (`determinism.yml`).
- **The job dispatcher** (design/phase4-helper.md P4.5.5-P4.5.7): the lab's games as CPU jobs inside
  the server, by priority and round-robin across owners, with deadlines from a cost table seeded from a
  committed measurement (`citar/jobs/costs.json`) and learned from every run, retries, memory failures
  offered only to an executor with more room, a benchmark on the host pausing its games, restricted hours
  starting nothing new, and each result committed before its line is written and the two reconciled at
  the start (a server stopped with games playing plays exactly those again, each game written once).
  Experiments gain a `kind`: `lab`, `ranking` (the site's ladder) and `reference` (the statistical
  baseline, its lines in `saves/lab/reference/` for `summarize.py`), the last two an administrator's and
  the server's own collection. The Lab page shows the dispatcher, its executors and each game's machine
  and verification, and administrators' Stop, Resume and Retry (a game that failed three times is
  played again; the server's next start does that too); the Queue page's CPU card each executor's
  games. A finished game wakes the dispatcher, so short games follow each other at once; a run is timed
  by its frames' arrival, so its CPU is never cut to a few milliseconds; a game held past its deadline
  by a benchmark costs no attempt; a game no executor would take (more memory than any has) waits
  without holding up the games behind it. New routes: `GET /api/lab/jobs?exp=NAME`,
  `GET /api/admin/jobs`, `POST /api/admin/jobs/pause`, `/resume` and `/retry`.
  `scripts/qa_phase4.py lab`.
- **The job host** (design/phase4-helper.md P4.4.3): one program that plays CPU jobs under limits,
  for a helper (`citar-helper job-host`, its child) and for the server (`python -m citar.jobs host`,
  started by `citar.jobs.host`). It holds its CPU share and its memory hard on Windows (a Job
  Object) and on Linux under a `systemd --user` scope, its memory hard and its CPU soft on other
  Linux machines (`RLIMIT_DATA`, `nice`), and both soft on macOS (the background band and a
  watchdog), and says which; it counts each game's peak memory, pauses and resumes its games,
  extends their deadlines by the time paused, refuses work when the canary or its self-check
  fails, and ends its games within two seconds of its parent going. It measures its own memory
  when it starts and offers each game what its memory limit leaves (`memory_per_run_mb`), which
  is what games are matched to. A game that runs out of memory ends the host and is classified
  `memory`, having reported its peak as it grew. `citar-helper --self-test` runs the checks.
  [docs/server/WORKERS.md](Workers#cpu-limits-per-operating-system).
- **Helpers play the lab's games** (design/phase4-helper.md P4.3.6, P4.5.5-P4.5.7): a machine whose owner
  lends its CPU in the helper's own settings (off until they say so) plays, in its helper's job host
  under the CPU settings in effect, its owner's experiments, those of members a grant on its group lends
  it to for **Lab (CPU work)** (a grant with no purposes never lends the CPU; an administrator's own
  experiments need such a grant too) and, where its settings list them, the server's ranking and
  reference games. Each game's event names the machine's owner as the provider and counts against the
  grant's budget. The helper holds its games while its own model works (`yield_to_llm`, on by default)
  and while a benchmark holds the machine, signs each result with the machine key and keeps it in
  `helper/results/` until the server acknowledges it, sending it again after a reconnect; the server
  takes back an offer unanswered in 30 s and a game silent for 5 minutes, backs off a machine that
  refuses or abandons games, and checks eligibility (jobs build, kinds, memory a game, quiet hours,
  benchmarks, grants). No helper can keep a game from the other machines: a spec it refuses or a game
  it fails as one no machine could play is played elsewhere and counted against its machine, a stopped
  helper is offered nothing until it reconnects, and a game its own pauses held past its deadline goes
  elsewhere for an hour. A grant's budget with no room holds its member's games back without charging
  the lender. When the machine's CPU settings change while games play, the helper restarts its job
  host once they end and tells the server the memory a game now has. Every machine takes CPU work, its
  results verified as its trust tier says (below). The machine settings
  dialog and the helper card show the CPU in effect, each limit hard or soft, "not lent by this
  machine's owner", and the grants that lend it; the dialog gains "Pause its games while its own model
  works". New route: `GET /api/machines/{id}/jobs`. `scripts/qa_phase4.py remote` (and `--wsl`, the
  server in WSL). [docs/server/WORKERS.md](Workers#lending-cpu-what-plays-on-it-and-for-whom).
- **Helpers' results are verified** (design/phase4-helper.md P4.4.4, P4.5.8): the server checks each
  result's signed claim (the machine's registered key, its own records, the result's hash, the chain's
  fold) with the same Rust as the helper's, and replays results on a verifier (this server, or a
  trusted machine an administrator marked one): every result of a `new` machine, about 10% of a
  `provisional` one's and 2% of a `trusted` one's, chosen by the server's secret once each result is
  in (a replayed job shows as done to its owners until it settles). Matches promote a machine
  (5 make it provisional, 50 with no mismatch trusted); a mismatch or a claim that does not hold
  quarantines it (its owner too, but for a chain that parted: hardware), refuses its work to anyone
  but its owner, and withdraws every result it gave unverified since its registration or release:
  lines removed, games played again, ledger events rejected with their budget settlements reversed,
  the ratings recomputed. A quarantined verifier's matches are re-opened, and the promotions they made
  (not a tier an administrator set) taken back. A result sent just as its machine's key is reset is no
  verdict: its game is played again. A member's third new machine
  waits while two have work in hand. `PUT /api/admin/machines/{id}/trust` gains `withdraw` and
  `verifier`; new route `POST /api/admin/machines/{id}/replicate`. `citar-helper-faults` (the
  `test-faults` feature, `--test-fault`) cheats on purpose for the tests; `scripts/qa_phase4.py verify`.
  [docs/server/WORKERS.md](Workers#verification-what-a-lenders-tier-means).
- **Credits for CPU work bounded by the job's reference cost** (design/phase4-helper.md P4.5.7): a helper's CPU
  seconds count at most what the server observed (plus 5% and a second) and 1.5 times the job's reference cost (the
  cost table's figure, then the replay's own CPU once a replay reproduces it, its budget settlement moved by the
  difference). Past the first bound the machine is flagged `overreported_cpu`, which keeps it from `trusted`; past the
  second `capped_cpu`, informational. A machine's measured speed (its replays' CPU over its own) is shown to
  administrators beside its CPU class, and one under half its class's median is flagged `class_suspect`.
  [docs/server/WORKERS.md](Workers#credits-for-cpu-work).
- **Model results by their machine's tier** (P4.5.8 rule 11): a member's machine's model results (benchmark games,
  probes, the diplomacy leaderboard's pairs, its games' metrics) count on the global boards only once the machine is
  trusted (its earlier results with it); until then they are left out of every board but their owner's own figures,
  badged "unverified machine", and counted. A model step's ledger entry records its machine's tier.
  [docs/server/WORKERS.md](Workers#model-results-and-the-global-boards).
- **`#/admin/verification`** (linked from the admin users page): the verification settings, bounded
  (`GET`/`PUT /api/admin/verification`), the quarantined machines and owners with their evidence and Release, the
  recent mismatches and withdrawals, the verifiers and the flagged machines. A helper machine's card and machine
  settings dialog show its trust (tier, verified count, streak; for administrators the evidence, the measured speed,
  the verifier mark, Quarantine…, Release, Mark as verifier, Replicate…). `GET /api/admin/jobs` lists the verifiers'
  replays and the caps on new machines. The Lab page counts each experiment's games by verification and the withdrawn
  ones, and says when games wait for a verifier; helpers whose connection went are counted rather than listed.
- **Security:** SECURITY.md's "Helpers and shared machines": what a worker token allows, what a lender can rely on, how
  the server checks a helper, and the threat model.
- **Verification, hardened (4-12's review):** deleting a machine while a result waits for its replay no longer escapes
  quarantine (its owner and its unverified results follow the mismatch); after an upgrade only a sampled result that
  waited is accepted unverified (a new machine's, an asked one and a disputed one are played again, no credits, no
  verdict); a measured speed is held between 0.05 and 20 (a deadline's between 0.25 and 4) and the class check's bar
  is set by machines that did not over-report, trusted ones first; a game knows each machine every model seat
  played on, and a standing that cannot be read counts as unverified; the runs listing's badge is no longer saved into
  the run; `PUT /api/admin/verification` answers 400, not 500, to infinity and huge counts; `GET /api/admin/jobs`
  waits at most 2 s for a tick (503 past that).
- **The helper updates itself, signed** (design/phase4-helper.md P4.3.8, P4.5.9): the server fetches the signed
  manifest of its own version's helpers (GitHub's release in server mode, `CITAR_HELPER_MANIFEST_URL`, or a file,
  `CITAR_HELPER_MANIFEST_FILE`) at most hourly, relays it only if it verifies, and pushes a new one to older helpers;
  each helper checks it again and, under its owner's `updates` (`auto`, `notify`, `off`; `notify` in a package
  manager's build, which refuses `auto`), downloads over `https` only, checks the size and SHA-256, self-tests the new
  binary, waits until idle, closes its connection and swaps, keeping `<name>.old`. A console run supervises the new
  binary and puts the old one back if it exits before its first welcome; under a service manager the helper exits 75,
  and a new binary that does not come up in three starts restores the old one. `citar-helper update check`, `apply`
  and `rollback`. The Servers page shows each helper's version beside the latest signed helper and where its update
  is. No key ships yet: until the owner's first signed release every helper reports updates unavailable; releases are
  signed in CI by the protected `helper-signing` environment's secret (the owner's one-time setup is in
  [docs/server/WORKERS.md](Workers#signing-helper-updates-the-owners-one-time-setup)), with
  `cargo xtask keygen`, `helper-manifest` and `sign-manifest` refusing key paths inside any repository. SECURITY.md
  describes the update channel. [docs/server/WORKERS.md](Workers#updates).
- **Self-update, hardened (4-13's review):** `citar-helper update rollback` holds the release it rolls away from back,
  so an automatic helper no longer installs it again at its next connection; a download the network breaks off (and a
  failed swap) is tried again after 5 minutes, doubling up to 6 hours, instead of never while the helper runs; a service
  start the new binary refuses early (a setting, a command line) counts toward the rollback; a new binary the console
  supervisor cannot even start is rolled back; the update folder is its user's alone and the binary put in place is
  checked against the signed SHA-256 once more; `cargo xtask keygen` writes the key owner-only on Windows too; a server
  that trusts no update key fetches nothing (no hourly GitHub request and warning), relays only a manifest of its own
  engine build, and logs a helper's update status once per change; committing the owner's public key keeps the test
  suite green.
- **The helper runs as a service** (design/phase4-helper.md P4.3.9): `citar-helper service install|uninstall|status
  [--user|--system] [--dry-run]`. Linux: a systemd user unit (or, as root, a system unit with `User=`) that restarts
  on failure and never on a permanent refusal (`RestartPreventExitStatus=78`); macOS: a LaunchAgent (`KeepAlive` with
  `SuccessfulExit` false); Windows: a Run value at sign-in that starts `citar-helper run --detach`, a supervisor with no
  window that restarts the helper at once after an update and with backoff after a crash, or, from an elevated prompt,
  a Windows service running as `NT SERVICE\citar-helper` (not LocalSystem) from `%ProgramData%\CITAR\helper`, whose
  access list leaves out the machine's other users, restarted after every failure. `--dry-run` prints exactly what
  would be written and run. In service mode nothing permanent ends the helper: a refused token or a setting it cannot
  take idles and tries again hourly. [docs/server/WORKERS.md](Workers#running-the-helper-as-a-service).
- **Five helper downloads:** an Intel Mac build (`citar-helper-macos-x64`) joins Windows x64, macOS on Apple silicon
  and Linux x64 and ARM64, all built in Rust by the release workflow under the same unversioned names, each with its
  `.sha256`, run once with `--version` and `--self-test`. The Servers page offers a Mac whose browser does not say its
  processor (Safari, Firefox) both macOS downloads. `helper.yml` tests the helper on the five targets and installs it
  as a service on each OS's runner. [packaging/README.md](https://github.com/jprodgers/CITAR/blob/main/packaging/README.md#the-citar-helper).

**Chat** ([docs/PLAYING.md](Playing-in-the-browser), [docs/AI_PLAYERS.md](AI-players)):

- A docked chat window per civilization, left of End Turn, opening as a chat comes; the deal builder
  is inside it, and a draft or an open builder survives every update. It shows whose move it is, a
  model "queued" (with its place) or "thinking", the time left, and the number of messages. End Turn
  says who you wait for and, armed, ends the turn when the last answer comes. At 700 px and below one
  window shows at a time, with tabs.
- The lobby's Chat row: how long a model (10 minutes) and a person (5 minutes) have to answer, and
  the cap on messages. The session keeps the time: a model that runs out declines, "(no reply in
  time)", and a chat waiting on a person closes as expired. A pause stops the clock; a suspension, a
  seat change or a restart never declines a chat.
- One conversation per chat for a model seat, and an answer that comes after the tool's wait is put
  in front of the model before its next step.

**Hybrid seats** ([docs/AI_PLAYERS.md](AI-players#hybrid-seats)):

- A seat played by a bot that hands the diplomacy categories you choose (trades, agreements, peace,
  war, denunciations, the United Nations vote, city-states, spies, captured cities, talk) to a model,
  with presets ("Trade only", "All but war", ...), the bot's handicap, an advisor, and how often the
  diplomat is called. The bot vetoes a deal that mixes its categories with the model's when it values
  it below zero; whatever the model cannot answer in time, its bot answers. In the lobby, the
  Join/Seats dialog and scenarios; the game screen shows "bot playing", "diplomat thinking" and
  "queued #n", and the stats screen the diplomat's phases and answers.
- Hybrid benchmark suites, each job paired with a bot-only control game of the same seed (cached and
  shared between models), the Models page's Diplomacy tab (`GET /api/models/diplomacy`) and hybrid
  probe runs that record which side answered each case.

**The game screen** ([docs/PLAYING.md](Playing-in-the-browser)):

- A person's minimap shows what their civilization explored, opening out to the home continent at
  the Renaissance and to the whole map at the Information era, never shrinking, remembered per game
  and seat; it is hex-correct and letterboxed for everyone, with click and drag to move the map.
- The recap plays from the game's journal frames (`delta`) and shows the chats as threads. A view
  answers with an ETag and a refresh that has nothing new gets 304; trade options are fetched when the
  deal builder opens.

**Development:** `POST /api/games/{id}/debug/ops` (scenario operations on a live game, on debug
servers only); `scripts/qa_phase3.py`, which checks each feature end to end on a dev server of its
own, and all of them at once in one game, with a real model in LM Studio last
([CONTRIBUTING.md](Contributing)).

### Fixed

- A helper's result that waited for its replay when the server was upgraded to another jobs build waited for ever (no
  verifier plays an old build): it is accepted unverified at the next start.
- The evidence of a machine caught at its first mismatch did not list the job its mismatch withdrew.
- The limit on refused worker tokens never refused anything (it was counted after the lookup and its
  refusal dropped), and it took the client's address from `X-Forwarded-For` whatever the proxy
  settings: an address is now refused before any lookup once 20 different tokens failed from it in
  five minutes (a revoked token retried counts once, and hellos sent together get no more lookups),
  read as every other limit reads it. A token's last address is written.
- A helper's first frame was read on the server's event loop, up to 8 MiB of it before any token was
  checked, holding up every game and helper meanwhile: a first frame over 2 MiB is now refused
  unread and long frames are read on a thread. A database error while checking a token no longer
  counts as a refused token (the helper is asked to try again).
- A cancelled request kept running on its helper until it finished, holding its slot (draining); the
  helper now stops it.
- A Mac whose browser does not say its processor (Safari and Firefox, on Apple silicon and Intel
  alike) was offered the Apple silicon helper only; it is now offered both macOS builds.
- A helper run as a service with a setting it cannot take exited with code 2, which a service
  manager restarted every 15 s for ever; it now idles, says why once in its log and reads its
  settings again hourly (except while an update's new binary must have its starts counted).

**Rules the Rust engine fixes.** The new engine in `crates/citar-engine`, which replaces the Python
engine in this release, plays by the same rules except where the Python engine was wrong or
inconsistent. Each fix below is deliberate, and each is checked: against the answers the Python
engine gave for 262 recorded game states (`refcheck/intended.toml`), or by a rule script or test
(`tests/rules/intended.toml`). The name at the end of each line is the entry's id, which the code
that makes the fix cites. `cargo refcheck changelog --write` keeps this list in step with the two
files.

<!-- rule fixes: written by `cargo refcheck changelog --write` from the intended lists -->
- Python left a civilian that a ranged attack brought to 0 health on the map at 0, neither dead nor captured (combat.py:109, 806); no Rust unit is ever at 0, and one loaded at 0 keeps 1 (`civilians-at-zero-health`)
- Python gave a resource's `[in this city]` uniques to every city of its owner, so Marble's +15% production toward wonders held in every city; Rust gives them to the city whose improved tile provides the resource (DESIGN.md 5.12) (`marble-bonus-in-its-own-city`)
- Python gave a resource's `[in this city]` uniques to every city of its owner, so Marble's +15% production toward wonders held in every city, and every city's list showed its wonders taking fewer turns; Rust gives them to the city whose improved tile provides the resource (DESIGN.md 5.12) (`marble-bonus-in-its-own-city-wonder-turns`)
- Gold per turn is shown rounded to one place from a sum of floats taken in the order of the cities' worked tiles, which Python kept in the order it placed the citizens and Rust keeps sorted; a sum a hair from a half rounds either way (`gold-per-turn-rounds-a-sum-at-a-half`)
- The attack preview listed a unit's promotions' modifiers in the order the unit gained them; Rust keeps a unit's promotions as a set and lists their modifiers in the ruleset's order (the same modifiers, with the same values) (`combat-modifiers-in-ruleset-order`)
- A refusal of a unit or city id that is not the caller's listed every unit or city the caller had, past 600 characters for a large empire; Rust keeps the ones that fit and says how many more get_units or get_cities lists (`refusal-lists-capped`)
- Refusals that ended with a list (a promotion the unit cannot take, a belief that is not available) had no full stop, and a long one ran past 600 characters; Rust ends them as sentences and keeps them within 600 characters (`refusals-end-as-sentences`)
- A city's food in the client view is its stats' total rounded to a tenth, and Python's total carried the rounding error of each addition (12.349999999999998 for 12.35, 4.4e-16 for 0), so a total a hair below a half rounded down, a surplus of 4e-16 food showed as 0.0 food growing the city in 5e16 turns, and one of -2e-16 showed the city as starving, with a starving alert; Rust's totals agree with Python's to 1e-6, and the view rounds them as they are (`city-view-rounds-its-own-sums`)
- Python's briefing named whoever it mentioned: whose turn it is, a city-state's ally, the owner of a resource on a tile explored long ago, though the reader had never met them; Rust names them as the events do, Unknown Civilization or Unknown City-State (`briefing-names-only-known-players`)
- Python's briefing listed, among the points of interest, every foreign city on a tile the reader had explored, with its owner, including those of civilizations it had not met: cities founded there after it explored the tile, which it cannot have seen, since seeing a city's tile makes contact with its owner; Rust leaves them out (`briefing-lists-only-cities-it-could-have-seen`)
- Python's briefing told a unit needing orders of the ruins and camps near it as they stood, on any tile the reader had explored, so it named camps raised in the fog since the reader looked and ruins on tiles it had only had revealed, which its own map and points of interest did not show, and dropped a camp it had seen as soon as the camp was gone, though its map still showed it; Rust names those the reader knows of, as its map shows them (`briefing-nearby-reads-what-it-knows`)
- Python's client view carried, for every civilization a player had met, what the two could trade (gold, spare luxuries and strategic resources, technologies, the agreements possible now), built again on every refresh though only the deal builder reads it; Rust's view leaves it out, the chat window's deal builder fetches it while it is open (GET /api/games/{gid}/trade-options/{other}), and the get_diplomacy tool keeps it (`client-view-trade-options-on-demand`)
- Python's client view had nothing for the minimap, which drew the whole map whatever a player had seen of it; a major civilization's own view in Rust carries a minimap block (its stage from its era, its capital, and the box of the tiles of its home continent it has explored and seen joined to its home), from which the browser bounds a person's minimap to the explored world and opens it out with the eras (`minimap-view-block`)
- Python applied scenario operations until the first failure and left the earlier ones applied; Rust applies a list all or nothing (`atomic-apply-ops`)
- The scenario operation set_tile stored any terrain name as a tile's terrain, feature or natural wonder; Rust refuses a name of the wrong kind (`scenario-set-tile-checks-kinds`)
- Scenario operations stored any number they were given; Rust refuses one that is not finite, and a whole number that does not fit its field (a resource amount above 255) (`scenario-numbers-finite-and-in-range`)
- The scenario operation set_relation could leave two players at war with a friendship, defensive pact or open borders in force; Rust refuses a relation no game can reach (`scenario-war-refuses-standing-treaties`)
- The scenario operation set_relation stored an opinion under a key nothing read; in Rust it is the holder's own opinion of the other, within the -100 to 100 every reason keeps (`scenario-opinion-counts`)
- The scenario operation set_influence took a city-state as the player whose influence it set; Rust takes only a major civilization (`scenario-influence-majors-only`)
- The scenario operation grant_tech read a techs string letter by letter; Rust reads it as one tech's name (`scenario-techs-string-is-one-name`)
- A tech a scenario granted was announced as 'Rome scenario Pottery.'; Rust announces 'Rome was granted Pottery.' (`scenario-tech-announcement-wording`)
- A scenario parameter of the wrong type was refused with Python's own exception ('bad parameters (ValueError: ...)'); Rust refuses it with a sentence naming the parameter and what it takes (`scenario-errors-are-sentences`)
- Tool arguments took integers of any size, and arguments sent as a list of pairs; Rust refuses an integer beyond 64 bits and arguments that are not an object (`normalize-refuses-big-ints-and-non-objects`)
- A new game's settings fell back to a default for a speed, difficulty, era, map size, map type, edges, barbarian level, victory or AI base values Python did not know, drew a random nation for an unknown one or a city-state, and took any controller; Rust refuses the settings, naming what is valid (`config-refuses-unknown-names`)
- A map document kept the improvements of a list of 19 names (a Citadel among them) and removed four named ones on water; Rust keeps any improvement proper that is neither a city centre nor a great improvement, and on water removes every improvement built only on land (`map-documents-read-by-rule`)
- Ending another player's turn was refused with 'It is not your turn.'; Rust names whose turn it is, as the tools' refusal does (`end-turn-names-whose-turn-it-is`)
- With no major civilization alive, ending a turn played round after round and never returned; Rust ends the round and returns before anyone plays in the next one (`end-turn-stops-without-majors`)
- Forcing a turn (EngineGame.force_turn) made an eliminated player's turn current, and moved the turn of a game that was over; Rust refuses both (`force-turn-only-for-the-living`)
- A natural wonder's 'Neighboring tiles will convert to [terrain]' read only its <in tiles without [...]> conditions; Rust converts a neighbour only where every condition holds (`mapgen-wonder-conversions-read-every-condition`)
- A natural wonder that turned the land beside it into coast (Krakatoa, the Rock of Gibraltar) left rivers running along the sea; Rust takes a river off a tile that becomes water, on both sides of the edge (`mapgen-no-river-along-new-water`)
- Map generation drew vegetation and rare features even when the ruleset marked them 'Doesn't generate naturally'; Rust never places one marked so, and one marked 'Doesn't generate naturally <in [...] tiles>' only where its conditions do not hold (no shipped feature is either) (`mapgen-features-that-never-generate`)
- A resource's share of its kind took over normal resources' tiles down to a type's last deposit, which the variety top-ups after it put back as a fresh cluster of two or three, so a 25% luxury share came out near 20%; Rust takes no normal type's last deposit, so the share holds to a tile and the variety needs no top-up (`mapgen-shares-keep-every-type`)
- The share of luxury types a generated map keeps was interpolated over the areas of the ruleset's lobby sizes, looked up by their keys; Rust interpolates over the tile counts of the shipped sizes, the same for the shipped ruleset and for any other (`mapgen-luxury-variety-by-tile-count`)
- Python scaled `when above/below [n] [stat]` by game speed on `(modified by game speed)` uniques but not the two bounds of `when between [a] and [b] [stat]`; Rust scales all three alike (`between-stat-scales-by-speed`)
- Python's building conditionals (`if [x] is constructed`, `in cities with a [x]` and the rest) compared the text with building names, so `[Wonder]` or `[Culture]` never held; Rust reads the text as a building filter (no shipped unique uses a filter there) (`building-conditionals-read-a-filter`)
- Python's `if no Civilization has adopted [x]` read policies only, so a belief never counted as adopted; Rust counts beliefs too (no shipped unique uses the conditional) (`no-civ-adopted-counts-beliefs`)
- Python's conditionals and citizen ranking read a civilization's happiness live; Rust commits it at the start and the end of the civilization's turn and reads what was committed, so no call can make citizens flip back and forth (`happiness-seen-committed`)
- Python's citizen ranking read a gold rate nothing wrote, so a shrinking treasury never made gold count double; Rust writes the rate at the end of each turn (`last-gold-rate-written`)
- work_tile silently dropped a city's oldest lock when every citizen was already locked; Rust keeps locks as a set and refuses another lock until one is released (`work-tile-refuses-a-lock-past-the-citizens`)
- set_specialists let a count that is not a number raise Python's ValueError; Rust refuses it with a sentence (`specialist-counts-must-be-numbers`)
- Python took a citizen off a blockaded tile, and its lock, at the end of its city's turn; Rust places the city's citizens again as soon as a blockade begins or ends, as after any change to what they can work (`citizens-follow-a-blockade-at-once`)
- Python placed a city's citizens again at the end of its turn when a tile next to one it could work changed, as a Moai beside a Moai does; Rust places them at once, as after any change to what its tiles yield (`citizens-follow-a-neighbour-at-once`)
- work_tile, set_city_focus and set_specialists listed a city's worked and locked tiles in the order its citizens took them; Rust lists them by column and then row, as the city's own view does (`citizen-tools-list-tiles-sorted`)
- A city with more citizens locked to tiles and set as specialists by hand than it had shed one lock each time its citizens were placed, so they moved at every placement until enough locks were gone; Rust sheds them in one placement, where those placements would have ended (`citizens-shed-locks-at-once`)
- Python let a city work a tile a sibling city of the same owner worked, unless the tile was that sibling's own territory, so two cities could work one tile; Rust gives a worked tile to one city, and no other may work it (`cities-never-share-a-tile`)
- The scenario operation adopt_policy skipped the era check, then checked it again inside the adoption and refused a branch of a later era; Rust adopts it whatever the era, as the operation promises (`scenario-adopt-policy-skips-the-era`)
- A city converting its production to gold or science also banked that production as overflow, which the next item it built took whole, so turns of Gold finished a wonder at once; Rust banks nothing, as UnCiv does, since the conversion already paid out (`perpetual-production-is-not-banked`)
- An item whose cost increases each time it is built counted as built at every start of turn its production was ready, even when the unit could not be placed and waited, so each blocked turn raised its price; Rust counts it when it is finished (`increasing-cost-counts-what-was-built`)
- A promotion bought while a free one was on offer went through without the experience, which Python then took anyway, leaving the unit's experience below zero; Rust refuses a paid promotion without the experience or a free pick of its own, and a unit's experience never goes below zero (`promotion-needs-its-experience`)
- An upgrade whose new unit had nowhere to stand removed the unit and put a copy back (a new id, without its name, religion, used abilities or original owner), then refused; Rust finds where the new unit will stand before the old one goes, so such an upgrade, bought or free, changes nothing (`upgrade-places-before-removing`)
- Python never recorded the units a civilization gained, so Carthage's `Land units may cross [Mountain] tiles after the first [Great General] is earned` never held; Rust records each unit a civilization gains, and Carthage's land units cross mountains once it has had a Great General (`units-gained-recorded`)
- A move_unit that was refused (no path, or its first step refused) had already woken the unit from its sleep, fortification or other order, and cleared its standing move order when the step was refused; Rust refuses the move with the unit as it was (`unit-refusals-change-nothing`)
- Python asked a timed unique's own conditionals when it was granted, so the Autocracy finisher's `[+25]% Strength <when attacking> ... <for [50] turns>` was never granted, nor a triggered one such as `[+10]% Strength <when attacking> <for [10] turns> <upon being declared war on by [Major] Civilizations>`; Rust grants a timed unique whatever its conditionals, which are its effect's, whether its source is gained or its trigger fires, as UnCiv does (`timed-uniques-granted-whatever-their-conditionals`)
- Python's `Adopt [belief]` did nothing for a belief; in Rust a belief nobody has taken joins the civilization's religion or pantheon when it fits it (a pantheon or follower belief once there is a pantheon, a founder belief only a religion without one, an enhancer belief only an enhanced religion without one) and does what a belief does when taken (`adopt-a-belief-joins-the-religion`)
- Python's `Adopt [policy]` raised out of the trigger for a policy the civilization could not adopt yet (its branch closed, its era not reached), failing whatever caused it; Rust adopts the policy whatever it requires, as UnCiv does (`adopt-a-policy-whatever-it-requires`)
- Python offered `Can speed up the construction of a wonder` the hurry action and then refused any unit without `Can speed up construction of a building`; in Rust either hurries construction, the first only while the city builds a wonder (`hurry-wonder-construction`)
- A great person born from points fired `upon gaining a [unit]` twice, the second time for every such unique of the civilization whatever unit it named, so `Gain [10] [Faith] <upon gaining a [Great Prophet] unit>` paid 20 for a prophet and 10 for a scientist; Rust fires it once, for the unit it names, as for any unit made in a city (`great-person-born-fires-gaining-once`)
- A great person spent on an action (hurrying research or construction, a trade mission) fired `upon expending a [unit]` twice, once for the action and once as the unit was consumed; Rust fires it once (`expending-a-unit-fires-once`)
- Experience earned in combat credited every great person of the ruleset earned through combat, so any civilization earned a Mongol Khan beside its Great General and had both; Rust credits the civilization's own kinds only (a Khan for the Mongols in place of the general), as UnCiv does (`combat-points-for-the-civilizations-own-great-people`)
- A civilization that let the engine pick its free great person picked among every kind, and took nothing when the Maya long count allowed only the kinds it had not taken yet; Rust picks the first preferred kind the calendar allows (`ai-free-great-person-within-the-calendar`)
- Founding a religion without a pantheon marked the civilization as owing a pantheon belief before the beliefs chosen were checked, so a founding refused for its beliefs still left the mark; Rust's checks write nothing (`refused-founding-owes-nothing`)
- A belief listed twice when founding or enhancing a religion filled two slots and joined the religion once, founding it with one belief fewer than it was owed; Rust refuses the choice (`belief-listed-twice-refused`)
- An AI choosing beliefs broke a tie between beliefs it weighed the same by their names; Rust breaks it by the ruleset's order, so no game rule compares text (`ai-beliefs-tie-by-ruleset-order`)
- One-time effects that fed themselves (`Free [Warrior] appears <upon gaining a [Warrior] unit>`, a promotion that is never kept and gives itself) recursed until Python raised, failing whatever caused the first; Rust stops the chain eight effects deep and reports it where the checks run (`trigger-chains-stop`)
- Python's `vs [x] units` shared `vs [x]`'s test, so a city matched `[All]`, `[City]` or a city filter's word there; Rust asks it about units only (no shipped unique shows it, so no reference state does) (`vs-units-never-matches-a-city`)
- A ranged attack that brought a civilian to 0 health left it on the map at 0, neither dead nor captured, and every attack on a civilian reported it captured; Rust kills a civilian brought to 0 health, and reports a capture only when a melee unit took the civilian (`civilians-under-fire`)
- A civilization that `May not annex cities` had the cities it captured made puppets, then could annex them at will; Rust refuses it annex (`may-not-annex-refuses-annexing`)
- A message to all a civilization had met went to them in the order they were met; Rust sends it to them in player-id order, as every other list of players is kept (`met-lists-in-player-id-order`)
- A deal item naming a player id or a city id no game can have, or an amount beyond 32 bits, was stored in a proposal and refused only when the deal was accepted, or never for an amount; Rust refuses it when it is proposed (`deal-items-fit-their-fields`)
- A deal in which one side declared war on a civilization with a defensive pact with the other side was carried out, and the pact then put the deal's two parties at war with the deal still in force and its chat both closed and accepted; Rust refuses such an item when it is proposed or accepted (`deal-war-on-a-partners-pact-refused`)
- An improvement was refused on a tile whose feature it may not stand on (a farm on a forest) unless it removed features itself, so the removal it would have queued first never was; Rust offers it once the civilization knows how to remove each feature in its way, and queues the removals first, top down (fallout, then the forest under it), as the tool says and UnCiv does (`improvements-over-removable-features`)
- A city-state's settler founded cities as a major's did, so a city-state that kept its starting settler, was given one or captured one grew past its single city; Rust refuses the settler's action to a city-state that has a city, as city-states train no settlers either (`city-states-found-one-city`)
- The results of founding a city with a unit and of a paradrop gave each tile as the keys of its coordinates (['x', 'y']); Rust gives where it is, as {x, y} (`unit-results-give-tiles`)
- An automated worker's job on a tile was judged for the first worker of its type to ask in a turn, with that unit's promotions and conditionals, and kept for every other; Rust judges it for the builder class and its civilization, whichever unit asks (`jobs-by-builder-class`)
- Two worker jobs of the same priority on the same tile went to the improvement whose name sorted last; Rust takes the one later in the ruleset (`jobs-tie-by-ruleset-order`)
- An automated worker cleared fallout only where a tile's fallout flag was set, which no nuke did, so it never cleared any; Rust reads the tile's Fallout feature, and clearing it is a job where nothing better is (`fallout-removal-is-a-job`)
- The production advisor read the units a city could build from a set of names, so the first settler, worker or scout and the best of equally strong units were whichever the set gave first, and of equally valued choices it took the name last in code point order; Rust reads them in the ruleset's order and takes the largest id of equally valued choices (`advisor-ties-by-id`)
- The production advisor's one random choice, whether a city prefers a ranged unit, drew from a bot seeded with the city's id and made afresh for each pick, so a city drew the same every turn; Rust draws from a stream of its own keyed by the city and the turn (`advisor-draws-by-city-and-turn`)
- The production advisor divided a military unit's strength by a power of its cost, so a unit costing nothing raised a division by zero and the city picked nothing; Rust values it as if it cost one (`advisor-counts-a-free-unit-as-costing-one`)
- The production advisor valued a building by adding it to the city, swapping two of the game's caches away and restoring them; what was computed with the building stayed in the caches it did not swap, so an answer depended on what had been asked before (a city advised a Monument after its puppet's pick was weighed, a Stadium on a fresh load). Rust's what-if reads the game with the building and writes nothing, so the same game always gives the same answer (`advisor-what-if-leaves-no-trace`)
- A militaristic city-state gave units faster to a civilization with 'Militaristic City-States grant units [n] times as fast when you are at war with a common nation' in every game with barbarians, since everyone is at war with them; Rust counts only a civilization or city-state both fight (`city-state-gifts-need-a-common-nation`)
- A civilization that lost its last city or unit on its own turn was eliminated at once and its turn ended for a player the game had removed; Rust eliminates the player whose turn it is when the round ends, and anyone else at once as Python did (`eliminated-on-its-own-turn-at-the-round-end`)
- A world leader vote for a player id the game did not have raised an error the un_vote tool did not catch, or, for a negative id, counted from the end of the list of players; Rust refuses it as it refuses any vote for what is not a living major civilization (`un-vote-for-no-player-refused`)
- A city-state voted in the world leader election for its ally even after that civilization was eliminated, a vote that could elect no one; Rust's city-state votes for its ally only while it lives (`un-city-state-votes-for-a-living-ally`)
- The United Nations' world leader vote, the Domination win and the Time victory at the turn limit were on unless a game turned them off, even with a ruleset that had no Diplomatic, Domination or Time victory; Rust has each only when the ruleset has that victory and the game leaves it on, so such a ruleset holds no vote and ends at its turn limit with no winner (`victories-python-named-are-the-rulesets`)
- A war declared as agreed in a deal was announced without saying who declared war on whom, so statistics counting wars by their attacker (the baseline's wars declared) missed it; Rust's names the attacker and the defender, as every other declaration of war does (`deal-war-names-both-sides`)
- The ancient ruins spread on an editor map that had none were drawn from the game's one random stream, after the nations were shuffled; Rust draws them from a stream of their own, so where they lie differs while how many does not (`map-prepare-draws-its-own-stream`)
- A game could hold any number of players; Rust holds at most 64, city-states and barbarians included (a set of players is one machine word), and refuses settings that would make more, such as 24 civilizations with 40 city-states and the barbarians (`games-hold-at-most-64-players`)
- The city_state_action tool told models that a unit next to a city-state's territory could be gifted, which the rule refused; its description now says the unit must stand in the territory with movement left (`gift-unit-described-as-ruled`)
- The respond_negotiation tool's schema listed only the negotiation and the action as required, although every answer needs a message and the engine refused one without; the schema now requires the message too, so a model is told before it calls and a call without one is refused by the schema check (`respond-negotiation-message-required`)
- A tool given something other than text where it reads a name failed with a Python exception (unit_order's order, get_rules' topic) or read a number as the name '5' (promote_unit's promotion, build_improvement's improvement, unit_action's action); Rust refuses it with a sentence naming the parameter and what it takes (`tool-arguments-of-the-wrong-type-refused`)
- A resource or tech deal item that named no resource or tech was refused as "'None' is not a tradeable strategic or luxury resource" or "Unknown tech 'None'"; Rust says what the item lacks, with an example of one written right (`deal-items-name-what-they-trade`)
- A refusal that quoted a caller's words (an unknown tool, item, tech, policy, belief, improvement, spy, city, specialist, recipient, status, topic or unit action) quoted all of them, so a long name made it run past 600 characters; Rust quotes the first 60 characters and '...', and puts a name it gave without quotation marks (a specialist the city has no slots for) in them once it is cut. A deal item is quoted as Python's repr with each text in it cut so and, past 100 characters, its remaining entries given as '...' and its brackets closed; a spy's city id that is not a number, on which Python raised, is quoted the same way, while a number is given as the whole number it reads as (12 for 12.5) (`refusals-quote-at-most-60-characters`)
- get_city_states listed each city-state's quests by matching them on a key the quest list never had, so it listed none; Rust lists the quests the city-state has given the caller (`city-state-view-lists-its-quests`)
- get_great_people read each great person's points under its pool's name, which the points were never kept under, so every kind showed 0 points; Rust shows the points each kind has earned, from cities or battles (`great-people-view-shows-the-points`)
- The empire summary a negotiation answer reads listed the civilizations it was at war with in the order it had met them; Rust lists them by player id (`empire-summary-wars-by-id`)
- get_diplomacy and get_victory_status gave the United Nations' last result with every candidate's name, those of civilizations the caller had not met included; Rust names them as a scrubbed event does, Unknown Civilization or Unknown City-State (numbered from the second), and gives the winner only to a caller who knows it (`un-results-name-only-known-candidates`)
- get_city_states named each city-state's ally, and get_diplomacy each recipient of a message the caller was on, even a civilization the caller had not met, which the player list hid as unknown; Rust names them only to a caller that has met them, and unknown otherwise (`views-hide-unmet-allies-and-recipients`)
- A city's food stored was written as a whole number after a rule set it (a starving city's box emptied, a city avoiding growth held at a full box, a city being razed) and with a decimal point otherwise; Rust always writes it with the decimal point (`city-food-stored-is-a-float`)
- Python listed a civilization's policies, the civilizations it had met, a city's specialists and buildings and a religion's beliefs in the order they were adopted, met, assigned, built and chosen, a history no state keeps; Rust lists them in the ruleset's order, branch by branch for the policies, and by player id, in the briefing and the scenario editor's overview (`lists-in-rule-order`)
- A map's summary counted as land every tile whose terrain was not called Ocean, Coast or Lakes; Rust counts the tiles whose terrain the ruleset makes water, so a ruleset's own water terrains count as water (`map-summary-reads-water-by-rule`)
- The scenario editor's overview gave a city-state's influence with the civilizations whose influence had ever been set; Rust gives its influence with every civilization, 0 where there is none (`scenario-overview-lists-every-influence`)
- A civilization's great people were born at the start of its turn for as long as its points covered the threshold, all at once, and for ever on points no game earns (a save's 1e300, which paying a threshold leaves where it was), hanging the game; Rust makes one great person of each kind a turn from its points, and one from its combat points, and the rest wait for the turns after (`one-great-person-of-a-kind-a-turn`)
- A scenario kept only a seat's model and bot settings, so a hybrid seat saved in one lost the kinds of diplomacy its model decides; Rust keeps the seat's hybrid settings too (`scenario-seats-keep-hybrid`)
<!-- rule fixes: end -->

**Fixed in the server and the browser client:**

- A member's seat could make the server dial any address with any of its environment variables as the
  key, or spend the site's own API key; a member's machine could make it scan its own hardware or
  drive its LM Studio. Members now name machines (see Compatibility), and those routes are
  administrators'.
- Revoking a helper's token left the helper connected until it reconnected; so did disabling or
  deleting its machine.
- A busy helper made a model seat sleep and retry for ever, metering a request each time; a request
  that timed out freed its slot while the helper was still generating it.
- A finished game kept counting against its owner's game limit, and benchmark and probe games counted
  as lobby games.
- Moderators could control members' published games; now they may only unpublish.
- A chat a model was answering was declined, "(no response)", when the game was suspended or a seat
  changed; a bot waited at most 90 s for a person's answer; a model's answer after the tool's wait
  reached it only when it tried to end its turn.
- Report narratives bypassed the machine's queue and polled a busy helper every 5 s for half an hour;
  a narrative waiting for its machine held up every other member's report.
- A probe answer cut off by the machine was scored as the model's; a benchmark that took the machine
  mid-run failed the rest of a probe run.
- Reading a probe or a run while it was being saved could fail on Windows (the file is briefly
  locked as it is replaced); it is read again.
- Hardware details with figures no JSON answer carries (NaN, infinities, integers of hundreds of
  digits) or parts of other shapes broke the Servers page and the pool's listing; they are dropped as
  the details are kept. A body with such a number is a 400.
- A scenario seat the engine cannot read, or a seat's bot that is not an object or whose aggression is
  not a number, is a 400 (it was a 500, or taken); the scenario editor refuses seat blocks that are
  not objects instead of saving defaults; a scenario's hybrid seat launched as a bot no longer keeps
  the hybrid handicap.
- "Best bot" is pinned when a seat is set, and its "(best)" mark is kept only while the seat plays the
  profile it was pinned to.
- The seat forms, the probe run dialog and the report's narrator show a machine's refusal for the work
  they ask for; the game screen's refreshes get 304s while a unit or city is selected; a closed chat
  window comes back with the other side's news; chat windows keep clear of the unit, city and side
  panels.
- Debug routes are 404 on servers that are not in debug mode, whatever the body.
- A route's changes were committed after its answer was sent, so a request made at once after it could
  read things as they were before (a share link just rotated still opened the game), and a commit that
  failed did so after a success. Every route now commits before it answers.
- The lobby's Saved games list offered Load and Delete on another member's game shared with you,
  which the server refuses; it lists them without those buttons (`GET /api/saves` says `can_manage`
  for each save).
- A person's minimap counts far land as home only once land they explored joins it.
- In hybrid benchmarks: a chat whose model never started falls back as `not_started` and is not
  counted against the model; a control game whose record does not read is played again, and one no
  job can pair with is not played; a control plays the bots pinned at its job's launch; a hybrid job
  loads its model beside the machine's other work; a diplomacy record that fails stops once and its
  job pairs nothing; a hybrid probe case whose model never ran says "answered by nobody"; reports show
  a hybrid job's pair line and keep hybrid seats out of the model's game figures.
- A model's tool call that the hybrid seat's bot vetoes is read as the engine reads it, so no spelling
  slips a deal past the veto; pausing a game holds a hybrid seat's diplomat and frees its machine.
- A model seat's chats asked for the answer as "Answer with respond_negotiation: accept ...", which a
  small model (gemma-4-e2b in the exit QA) copied as text in every chat instead of calling the tool, so
  every chat with it was declined "(no response)"; every chat message now asks for a call of the tool,
  and it answers by calling it.
- A tool call a Gemma 4 model wrote in its own syntax (`call:name{key:<|"|>text<|"|>}`), which LM
  Studio sometimes leaves in the reply's text, is read as the call, as other formats written as text
  already were.
- A model that answers a chat in text without calling a tool is asked three times running, as on its
  turn, not twenty, before the chat is declined "(no response)": a small model that does it held its
  machine about two minutes an answer. A step cut off at its token limit does not count, as on its
  turn.
- A minute of live power samples that read the CPU but not the GPU (a machine without an NVIDIA card
  whose driver reports its draw) was costed as if the GPU drew nothing while the model worked, and
  counted as measured; the GPU's share is now estimated from the machine's power figures as in a
  minute without samples, the minute counts as estimated, and the machine's metered total leaves such
  samples out.

## [0.1.5] - 2026-09-22

Single-player fixes and more dangerous barbarians.

### Added

- **A research queue.** Shift+click a technology to add it (and whatever it still needs) to the end of
  the queue, or Shift+click a queued one to take it off, along with anything queued that depends on it.
  The tech tree numbers the queue on the technologies themselves and lists it along the top with the turn
  each will finish. A plain click still replaces the queue. The `set_research` tool takes `append`, and a
  new `dequeue_research` tool removes a queued technology.
- **Returning recaptured civilians.** Freeing a worker or settler that barbarians took from another
  civilization asks whether to return it (a better opinion with a major civilization, +45 influence with
  a city-state) or keep it, as in Civilization V; left unanswered, you keep it at the end of the turn.
  New `return_civilian` tool; a civilian that was yours simply comes back.
- **Barbarian aggression**, a 0-100 slider next to the barbarian setting in the new-game form (Normal
  defaults to 50, Raging to 85; the `barbarian_aggression` config key). It sets how far barbarians look
  for targets, what odds they accept, how many gather before storming a city, how fast camps spawn, and
  how hard a sack hits.

### Changed

- **Barbarians are a threat.** Barbarian units could not plan a path across their own unexplored map, so
  they only ever attacked what was already next to them. They now hunt cities, units, workers and
  settlers, and the most valuable improvements (luxury and strategic resources first). As in
  Civilization V they never capture or raze a city: one they bring down is *sacked* instead, losing
  gold, possibly a citizen and a building (never a wonder or the palace), and is then left alone for 5 to
  10 turns. Over 100 turns of a four-bot Quick Small game, units killed went from about 50 to about 240
  on Normal and from about 90 to about 650 on Raging.
- **Events no longer name civilizations you have not met.** Every player, human or AI, reads "Unknown
  Civilization has built The Pyramids in an unknown city." until the two civilizations meet; unmet
  city-states are "Unknown City-State", and such events drop their location. Spectators and replays
  still see everything.
- **End Turn waits for decisions.** The button is greyed out while research, a policy, a free
  technology or great person, a pantheon, a promotion, a city with nothing to build, a unit without
  orders, a negotiation or a UN vote is waiting; clicking it lists them and goes to the first.
  Ctrl+click (or Ctrl+Shift+Enter) ends the turn anyway.
- **Player colours are unique.** A new 24-colour palette, picked for contrast on the map and against
  the city-state and barbarian colours. The seat editor offers it as swatches, greys out colours other
  seats hold and accepts a custom hex value that is not too close to one of them; the server enforces
  the same rule, first come first served.
- **The tech tree is easier to read.** More room between technologies, right-angled links that never
  share a vertical run, and hovering a technology highlights everything it needs and what it leads to.
  The tree reopens where it was left, or at the current era.
- **More luxury variety on big maps.** Huge and gargantuan maps now carry every luxury type, large at
  least 90%, standard 75% and small maps half; every map has every strategic resource.
- The wonder-built event names the civilization first ("Rome has built The Pyramids in Rome.").
- Bot seats in live games are seeded from the game's seed, as lab games already were, so the same seed
  and seats play the same game. Before, each live bot seeded itself from the clock.

### Fixed

- The Bombard button stayed after a city had fired, because the city's own view never said whether it
  still could.
- Accepting or rejecting a proposal left the diplomacy window open while play went on; it now closes.
- The interface was close to unusable while AIs played fast: every update rebuilt the top bar, the End
  Turn box, the side panel and the unit and city panels, so clicks landed on buttons that had just been
  replaced and dropdowns closed as soon as they opened (pausing a fast bot game could take a dozen
  tries). Each of them now waits to redraw while the pointer is over it or one of its dropdowns is open,
  and redraws with the first update after a click. Pause/Resume shows the new state at once.
- Notifications covered the diplomacy window when a proposal arrived. Negotiation notices are no longer
  toasted while the window shows them, and any toast moves to the bottom of the screen while a window is
  open.
- A rating test passed its message as `assertAlmostEqual`'s `places` argument, so it raised a `TypeError`
  on Python 3.11 whenever the weighted pair count was not exactly 2.0. Tests only; the shipped code is
  unaffected.

### Known issues

Model scores are **not comparable across 0.1.4 and 0.1.5**: barbarians fight far harder, and agents no
longer learn about civilizations they have not met from the event feed. See
[KNOWN_ISSUES.md](Known-issues).

## [0.1.4] - 2026-09-22

### Added

- **Bot profiles and a Bots page.** Every number the scripted bot decides with (about 360) is now a
  named parameter with a label, an explanation and a range. A *profile* chooses the bot's code (the
  live bot or a frozen snapshot), its aggression and any parameter overrides. The new **Bots** page
  lists the profiles, forks and edits them (grouped parameters, search, "changed only", reorderable
  preference lists), keeps a revision history with notes, and queues an **A/B test** between
  profiles as a lab experiment. Lobby seats, benchmark scenarios and probe runs can pick a profile.
- **"Best bot" for new games.** A bot seat in the new-game form defaults to **Best bot**: the highest-rated
  profile on the server (one whose current settings have been rated), fixed into the seat when the game is created
  so the game keeps that bot. The dropdown lists every profile in ranking order with its rating. Benchmark
  scenarios and probes offer it too but keep Standard as their default, so benchmarks stay comparable.
- **Bot rankings.** Every lab game feeds an Elo-scale rating (a Bradley–Terry fit on each game's
  finishing order) per exact configuration and difficulty, with standard errors, head-to-head
  records, score index, win rate and a rating-over-time chart. Older results are rated too.

### Fixed

- "Enhance religion" was offered as available away from a city, and "Spread religion" for a unit carrying no
  religion; both were then refused. The scripted bot's Great Prophets could retry the refused enhance for the rest
  of a game instead of walking to a city. Both actions now say what is missing, and the bot moves its prophet.
- Lab reports and `citar bench` left eliminated civilizations out of their results (see Added).
- **A spaceship could never be finished, so there were no science victories.** Items "Limited to [n] per
  Civilization" counted their own place in the build queue against the limit, so the last one allowed was
  accepted and then silently dropped from the queue the next turn. Parts limited to 1 (Cockpit, Engine, Stasis
  Chamber) could never be built, the third Booster neither, nor a civilization's fifth Recycling Center.
- The per-turn **production** statistic (graphs, replays, lab checkpoints) was always 0: it read a city attribute
  that doesn't exist. It now records the civilization's production.

### Changed

- **A stronger scripted bot (v2 defaults).** Nine of the bot's parameters changed, each one measured in the
  lab over roughly 200 full-length games: wonders are no longer gated to high-production cities and are worth
  more, building values are no longer cached between turns, great-person points count for three times as much,
  the field army gathers before a war is declared, and spaceship parts, Apollo and victory buildings are finally
  valued as what they are — the things that win the game — with Aluminum kept in reserve for them. Against the
  old defaults this is worth about +0.09 score share (roughly 140 rating points), with more cities, more wonders
  and fewer unhappy turns. **Science victories now happen** (3 to 6 games in 24 to 40, where the bot had never
  achieved one). The trade-off: this bot expands rather than fights, and captures far fewer cities than before —
  the first thing being worked on for the next version. Every old configuration is still available as a bot
  profile, and existing profiles keep playing what they played.
- **Running and finished work no longer share a colour.** Anything in progress — a running experiment or
  benchmark job, a connected helper, a live runner — is blue; anything finished or ready is green, with a ✓.
  Finished saves in the lobby and finished side runs in the lab say so in green instead of grey, so a glance
  at a list tells you what is still working and what is ready to read.
- **Long moves keep their route.** A move order's route is planned once, when the order is given, and a standing
  order follows that route every turn instead of re-planning it. When a unit steps onto the route - your own or
  anyone else's - the moving unit waits with its order intact and carries on when the way clears; it used to lose
  its order and need re-directing. An order still ends on arrival, when a new enemy comes into view, when the route
  turns out to be impassable, or after three turns without getting any further (with a notice). Only a new move
  order plans a new route.
- The bot's code was reorganised so its constants are parameters. With default parameters it plays
  exactly as before: seeded 250-turn games with both production modes and every optional behaviour
  switched on give bit-identical results to the previous version.
- Lab results record each seat's profile, revision, fingerprint and actual aggression.

### Known issues

The scripted bot now expands well but rarely fights: about 0.08 captured cities per game against the
old defaults' 0.42, and a bot of middling aggression may never declare war. Model scores are
comparable within a release and **not across 0.1.3 and 0.1.4**, because the bot changed.
See [KNOWN_ISSUES.md](Known-issues).

## [0.1.3] - 2026-09-22

### Added

- **One prioritised queue per model machine.** Games, benchmark jobs, probe runs and reports whose
  analysis a model writes share a single ranked list for each machine, on a new **Queue** page.
  Higher priority runs first, then whatever has waited longest. Running work is in the same list:
  put something above it ("⤒ top") and the running work makes way at its next safe point, then
  carries on by itself when it is back on top. Equal priorities never interrupt each other. The
  page also shows the lab's experiments, with their own priorities, and this server's CPU load.
- **Quiet hours for Servers-page machines.** Each machine has a **Quiet hours** setting, read in its
  owner's time zone, that applies to everyone including its owner: games, benchmark jobs and probe
  runs finish the turn in progress, pause, and resume by themselves when the hours end. Lobby games
  now pause in quiet hours too (they used to keep running).
- **Costs for Servers-page machines, and energy per task.** Machines registered through the helper
  were recorded in the usage ledger but never priced. Each machine card now has **Power & costs**
  (watts, hardware price and lifespan, electricity plan), reports include these machines, and a new
  report section, **Energy and efficiency**, compares each model on each machine: kWh, electricity
  cost, Wh per game, per model turn and per probe case, output tokens per Wh and per second,
  performance per kWh, and cost per task. A machine registered again keeps its earlier usage.
- **Reports can be written on a helper machine** and wait for it in the queue like other work.
- **Benchmark scenarios take the map options** (edges, rivers, resources).
- **`deploy/citar-lab.service`** runs the bot-vs-bot lab as a low-priority service beside the server.

### Changed

- **Pausing a game stops its clock.** Paused time no longer counts toward a turn's duration or its
  time limit, and an AI paused mid-turn waits before its next model call instead of playing on.
- **Finished games leave the current-games list by themselves**, ten minutes after the end once
  nobody is watching. Their saves and replay are kept, as with Close.
- **Probe runs on different machines run side by side**, and a run waiting for its machine no
  longer holds up runs for other machines.
- **A machine is freed as soon as its model is eliminated**; a benchmark job whose model is out ends
  there instead of watching the bots play on.
- **The Servers page says which hours are which:** a group's window is now "Allowed hours" (when the
  people you share with may use a machine, never limiting you), distinct from the machine's quiet hours.

### Fixed

- **Lobby games survive a server restart.** Open games used to disappear until reloaded by hand.
- **Reports show on the Reports page.** The site's security policy blocked the report frame, so the
  page looked blank; reports are now served with their own strict policy (no scripts, framable only
  by the site).
- **A closed game stays closed.** Closing a game during an autosave could bring it back after the
  next restart.
- **Benchmark jobs pause with the machine their game really uses**, even after its seat was moved to
  a re-registered machine.
- **Queued work waits for a machine a game is using** instead of fighting it for the slot.

## [0.1.2] - 2026-09-21

### Fixed

- **Machines on the Servers page can play.** A game seat could only use a server from the admin
  registry, so machines registered on the Servers page and connected through the CITAR helper never
  appeared in the new-game form. They now do, with the models their helper reports (the loaded
  one first), and a seat on one plays through the helper. Creating a game checks that you may use
  the machine for games right now, and says why not if you can't.
- **The Linux helper connects on any distribution.** It carried its own OpenSSL, which looked for
  CA certificates only where the Ubuntu build machine keeps them, so on Fedora, Arch and others
  every connection failed with `CERTIFICATE_VERIFY_FAILED`. The helper now also uses the
  certificates it ships with and the usual system bundles; verification is as strict as before.
  The release build checks the Linux helper's handshake inside a Fedora container.

## [0.1.1] - 2026-09-21

### Compatibility

- **The same seed now makes a different map.** The generator changed (ice, rivers, noise, and the
  luxuries that used to be missing), so a benchmark suite run on 0.1.0 and on 0.1.1 did not play
  the same worlds, and scores across the two are not directly comparable. The scripted bot is
  unchanged.
- Saves from 0.1.0 load as they were: an old game keeps its map and simply does not wrap.

### Maps

- **Map edges** are a lobby option: ice caps north and south (the default), wrap east-west, wrap
  north-south, wrap both ways, or boxed in with ice on all four sides. Wrapping is real, not a
  picture: movement, distances, borders, sight lines, paths and the LLM briefing all go the short
  way round, the map scrolls without end, and the noise that shapes the land repeats across the
  seam so no coastline is cut off.
- **Polar ice** is now a band one to four tiles deep that drifts slowly along the edge, instead of
  scattered blobs of sea ice.
- **Rivers** always reach the sea and never cross. Every hex corner learns its way downhill to the
  coast first, and rivers follow that drainage, so two that meet merge into one. A **river
  density** option (0–300%) sets how many there are.
- **Resource controls**: overall density, a density for each of strategic, luxury and bonus
  resources, and per-resource rules for strategic and luxury resources — off, at most N tiles, or a
  percentage share of their kind. The map editor's generator has the same options.

### Fixed

- Fourteen luxuries (Cotton, Dyes, Gems, Gold Ore, Silver, Ivory, Silk, Spices, Sugar, Marble,
  Citrus, Copper, Salt, Truffles) never appeared on generated maps: their "doesn't generate
  naturally *on hills*" rule was read as "doesn't generate naturally". Maps now carry the whole
  luxury set.
- A free technology (the Great Library, Liberty, ruins) can be chosen by a human player again: the
  tech tree now says a free pick is waiting, highlights what can be taken, and a click learns it
  instead of changing the current research.
- An LLM whose server dropped for a few seconds no longer loses dozens of turns. A worker's
  disconnection was reported as a model error, which skipped the turn at once, and the next one,
  and every one after, while the bots played on.
- The worker (helper) gave up for good when the server refused its connection — which is what it
  sees while CITAR restarts — so every worker stayed offline after a server deploy. It now retries;
  only a refused token stops it.

### AI players

- **Reconnect wait and disconnect rule**: a seat that cannot reach its model server keeps retrying
  for a set time (180 s by default, per game or per seat), without that time counting against the
  turn. If the server is still gone, the game either **pauses** — everyone, until the server answers
  again, then resumes by itself — or **skips** that seat's turn, as chosen in the lobby.

### Phones and the helper

- **A phone site**: phones get a check-in view of the server — running games, standings, whose
  turn it is, AIs thinking or reconnecting, benchmarks, reports and machines — with Pause/Resume.
  The full site is a tap away and the choice is remembered.
- **The CITAR helper**: the worker as a single download for Windows, macOS (Apple silicon) and
  Linux (x64, ARM64), built with every release. The Servers and Models pages offer the right file
  for the visitor's computer. Started with no arguments it asks for the server and token once and
  remembers them.

## [0.1.0] - 2026-09-20

The first public release. CITAR has existed for a while as a private project; this is the version
somebody else can install.

### The game

- A Civilization V-style 4X game with the rules, numbers and much of the logic derived from
  [UnCiv](https://github.com/yairm210/Unciv)'s "Civ V – Gods & Kings" ruleset: all nine eras, 35
  civilizations, 40 city-states, religion, social policies, wonders, great people and golden ages,
  espionage, city-state and diplomatic victory, and nuclear weapons.
- Map sizes from Duel to Gargantuan (160×100, 24 civilizations), four game speeds, eight
  difficulty levels, five victory conditions.
- A browser client with no build step: canvas map, city management, tech tree, policies, religion,
  espionage, a diplomacy deal builder, and a turn-by-turn recap with every AI's recorded reasoning.

### AI players

- **LLM seats** driven by the server, through the Anthropic API or any OpenAI-compatible endpoint
  (LM Studio, Ollama, llama.cpp, vLLM).
- **MCP seats** for external agents — Claude Code, Claude Desktop, anything that speaks MCP —
  including `wait_for_turn`, which blocks instead of polling.
- **Scripted bots** that research by need, pick buildings by simulating the city with each
  candidate, run religion and espionage, and wage war with siege units and rally points.
- Guard rails for weaker models: repeated actions refused, unchanged queries collapsed, tool calls
  written as text recovered, per-turn progress notes, and an ALERTS section in every briefing.

### Research tooling

- **Benchmarks**: suites of models against seeded maps, sequential or parallel, with restricted
  hours per server, and runs that survive a restart by reloading games from their autosaves.
- **Model scoring**: performance against the strongest bot, reliability and speed, with adjustable
  weights.
- **Scenarios and probes**: a map editor, a scenario editor, and repeatable per-case decision tests
  with expected outcomes and pass rates.
- **Metrics**: per-seat, per-turn timing, tool mix, errors, loops, tokens and turn endings,
  exportable as CSV.
- **Servers, ledger and reports**: a registry of every machine that runs models, a usage ledger
  that records work without prices, and self-contained HTML reports that price it at report time —
  so correcting a rate corrects every report.
- **The lab**: a resumable queue of bot experiments, with bot code frozen at submit time and
  factorial screening of parameters.

### Multi-user

- Accounts, invitations, single sign-on with Google, GitHub, Discord and Microsoft, e-mail
  verification and password reset.
- Sharing with per-object visibility, public spectator links that never imply the right to play,
  and seat tokens that grant exactly one seat.
- Pooled hardware: groups, grants, availability windows and per-account budgets, which are
  accounting and admission control only — there are no payments anywhere.
- **Workers**: a machine at home serves its models to a remote CITAR over an outbound WebSocket, so
  nothing needs to be opened on a router.

### Packaging and setup — new in this release

- `pip install citar`, with extras per role (`all`, `server`, `worker`, and one per provider).
- A **Windows installer** that needs no Python, an `install.sh` for macOS and Linux, an
  `install.ps1` for Windows, a **Docker image** with a compose file that terminates TLS, and
  Homebrew, Scoop and winget manifests.
- A unified **`citar` command**: `serve`, `setup`, `doctor`, `where`, `admin`, `worker`, `mcp`,
  `bench`, `sim`, `balance`, `lab`.
- **`citar setup`**, an interactive wizard with three flows — this computer, a public server, or a
  worker — each of which collects a plan, shows it, and asks once before writing anything. Every
  question has a flag, so installers run the same code unattended.
- **A first-run wizard in the browser** that finds the model servers already running, reads the
  machine's GPU, and suggests a model that will fit — or explains what to install for the hardware
  it found.
- **An operator console** (`#/console`) showing what is configured, what is missing and what each
  gap costs, with runtime policy editable in place and a test-e-mail button.
- **A front door.** On a public server, the root now explains what CITAR is to signed-out visitors
  and offers the install command for their platform, instead of showing them a login box and
  nothing else.
- **`citar doctor`**: versions, dependencies, directories and their permissions, configuration,
  database, ruleset, every model endpoint, and whether the port is free.

### Fixed

- **State no longer lands in `site-packages`.** Every module used to resolve its own directory from
  `__file__`, which worked in a checkout and wrote saved games into the installed package
  otherwise. `citar/paths.py` is now the single answer, and it distinguishes a writable source
  checkout from an installed copy. The database and secret key never land in a checkout at all,
  because a synced project folder corrupts a live SQLite file.
- **The route audit was checking about half of what it claimed.** Recent FastAPI wraps each
  `include_router` call in an object with no `.path`, so a loop over `app.routes` walked past every
  route on every included router — which is the whole authenticated API. It reported 83 routes and
  a clean bill of health; there are 172. It now recurses into included routers, carries their
  prefixes and router-level dependencies, and recognises the gates that are inner functions. It
  also no longer prints "OK" underneath a list of unguarded routes.
- **The lab runner could not start a game.** A refactor removed the module-level `ROOT` that the
  subprocess launch used, leaving an undefined name on a path only the runner takes.
- **Three closures captured a loop variable** in the scenario editor, natural-wonder discovery and
  the lab's queue mover. Each was safe as written and would have broken the moment the call became
  lazy; all three now bind explicitly.
- Database migrations are packaged with the wheel. They were being dropped by a rule that collects
  data files and skips `.py`, which would have produced an installed copy with a migration
  environment and no migrations in it.

### Known issues

The scripted bot is limited by happiness and stalls at two to five cities by turn 150, which caps
how hard it can push a model. See [KNOWN_ISSUES.md](Known-issues).

[Unreleased]: https://github.com/jprodgers/CITAR/compare/v0.1.5...HEAD
[0.1.5]: https://github.com/jprodgers/CITAR/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/jprodgers/CITAR/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/jprodgers/CITAR/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/jprodgers/CITAR/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/jprodgers/CITAR/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/jprodgers/CITAR/releases/tag/v0.1.0


---

*This page is generated from [`CHANGELOG.md`](https://github.com/jprodgers/CITAR/blob/main/CHANGELOG.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
