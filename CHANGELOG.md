# Changelog

Notable changes to CITAR. The format follows [Keep a Changelog](https://keepachangelog.com), and
versions follow [semantic versioning](https://semver.org) — with the pre-1.0 caveat that tool names
and arguments may still change between minor versions, and the release notes will say when they do.

## [Unreleased]

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
how hard it can push a model. See [KNOWN_ISSUES.md](KNOWN_ISSUES.md).

[Unreleased]: https://github.com/jprodgers/CITAR/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/jprodgers/CITAR/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/jprodgers/CITAR/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/jprodgers/CITAR/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/jprodgers/CITAR/releases/tag/v0.1.0
