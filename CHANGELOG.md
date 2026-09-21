# Changelog

Notable changes to CITAR. The format follows [Keep a Changelog](https://keepachangelog.com), and
versions follow [semantic versioning](https://semver.org) — with the pre-1.0 caveat that tool names
and arguments may still change between minor versions, and the release notes will say when they do.

## [Unreleased]

Nothing yet.

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

[Unreleased]: https://github.com/jprodgers/CITAR/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jprodgers/CITAR/releases/tag/v0.1.0
