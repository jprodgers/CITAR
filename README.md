<div align="center">

<img src="docs/assets/icon.png" alt="" width="96">

# CITAR

**Civ Inspired Tool for AI Research**

A Civilization V-style 4X game whose players can be language models.
Play against them, watch them play each other, and measure how well they do it.

[![tests](https://github.com/jprodgers/CITAR/actions/workflows/test.yml/badge.svg)](https://github.com/jprodgers/CITAR/actions/workflows/test.yml)
[![PyPI](https://img.shields.io/pypi/v/citar.svg)](https://pypi.org/project/citar/)
[![Python](https://img.shields.io/pypi/pyversions/citar.svg)](https://pypi.org/project/citar/)
[![Licence: MPL-2.0](https://img.shields.io/badge/licence-MPL--2.0-blue.svg)](LICENSE)

[Install](#install) · [Quick start](docs/QUICKSTART.md) · [Documentation](docs/) · [Run a server](docs/server/DEPLOY.md)

</div>

---

## What it is

A full Civilization V ruleset — nine eras, 35 civilizations, 40 city-states, religion, social
policies, great people, espionage, the United Nations, nuclear weapons — with an interface built so
that a language model can sit in any seat.

Humans play in the browser. Models join through **MCP** (Claude Code, Claude Desktop, any MCP
client), through the **Anthropic API**, or through any **OpenAI-compatible endpoint** — LM Studio,
Ollama, llama.cpp, vLLM. A scripted bot is included to play against and to measure models against,
so the game is fully playable with no model at all.

The rules and numbers come from [UnCiv](https://github.com/yairm210/Unciv)'s "Civ V – Gods & Kings"
ruleset, which is why they are faithful enough to be worth measuring against.

## Why it exists

Most model evaluations are short. A question, an answer, a score. A game of Civilization is the
opposite: hundreds of turns, imperfect information, an opponent who reacts, and consequences that
arrive forty turns after the decision that caused them. That is a different thing to be good at,
and it is hard to fake.

So CITAR is built as an instrument, not just a game:

- **Benchmarks** run full games across several models on identical maps and score them against the
  scripted bot, with confidence intervals and per-turn timing.
- **Scenario probes** put a model in a prepared position — an offer to accept, a war to decide on —
  and record what it does, one decision at a time, repeatably.
- **Metrics** record every tool call, every rejected order, every loop, and how each turn ended.
- **Reports** price it all: hardware time, electricity, tokens, and what a given result cost to
  produce.

None of it phones home. CITAR has no telemetry and no accounts unless you run a server on purpose.

## Install

### Just want to play

**Windows** — download [`CITAR-setup.exe`](https://github.com/jprodgers/CITAR/releases/latest) and
run it. Nothing else needed; there is no Python to install.

**macOS and Linux**

```bash
curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh | bash
```

**Anywhere with Python 3.11+**

```bash
pipx install "citar[all]"     # or: pip install "citar[all]"
citar setup                   # finds your models and configures CITAR
citar                         # starts the game and opens your browser
```

Also on [Homebrew, Scoop and winget](docs/INSTALL.md).

### Want to run a server

```bash
curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh | bash -s -- --server
```

or with Docker:

```bash
curl -O https://raw.githubusercontent.com/jprodgers/CITAR/main/docker-compose.yml
curl -o .env https://raw.githubusercontent.com/jprodgers/CITAR/main/.env.docker.example
$EDITOR .env                  # domain, secret key
docker compose up -d
```

Either way you get accounts, invitations, single sign-on, per-user budgets, TLS, and a way for
people to lend their own GPUs to the server without opening a port at home. See
[docs/server/DEPLOY.md](docs/server/DEPLOY.md).

## First game in five minutes

```bash
citar                         # opens http://127.0.0.1:8765
```

Create a game, give one seat to yourself and the rest to **scripted bots**, and press Create. That
works with nothing installed.

To give a seat to a model, pick **LLM** and choose a server and model — `citar setup` will have
found LM Studio or Ollama if either is running. To hand a seat to Claude Code instead, choose **MCP
client**, then copy the `claude mcp add …` command from **Join / Seats**.

[The full quick start](docs/QUICKSTART.md) walks through all three.

## What you can do with it

| | |
|---|---|
| **Play** | Full Civ V rules in the browser: tech tree, policies, religion, espionage, city-states, diplomacy with a deal builder, five victory conditions. |
| **Watch** | Games with no human seat can be watched with full vision, paused, slowed down, and viewed as any civilization — including each AI's recorded reasoning. |
| **Benchmark** | Suites of models against the same seeded maps, run sequentially or in parallel, resumable across restarts and reboots. |
| **Probe** | Prepared scenarios replayed case by case: offers, messages, or a whole turn, with expected outcomes and pass rates. |
| **Measure** | Per-seat turn times, tool mix, error and loop rates, token counts, and how turns ended. Exportable as CSV. |
| **Cost** | A usage ledger priced at report time, so correcting an electricity rate corrects every report ever made. |
| **Edit** | A map editor and a scenario editor, so you can build the position you want to test. |
| **Tune** | A parallel bot simulator and a resumable experiment lab, because the bot is the yardstick. |

## Documentation

| | |
|---|---|
| [Quick start](docs/QUICKSTART.md) | First game, first model, first benchmark |
| [Installing](docs/INSTALL.md) | Every install route, and how to remove it |
| [Playing](docs/PLAYING.md) | The browser client, keyboard shortcuts, every screen |
| [AI players](docs/AI_PLAYERS.md) | LLM seats, MCP, providers, prompts, guard rails |
| [Benchmarks](docs/BENCHMARKS.md) | Suites, scheduling, scoring, what the numbers mean |
| [Scenarios and probes](docs/SCENARIOS.md) | The map editor, scenarios, and repeatable decision tests |
| [Servers, costs and reports](docs/REPORTS.md) | The machine registry, the usage ledger, costed reports |
| [Scripted bots](docs/BOTS.md) | How the bot plays, balancing it, the experiment lab |
| [Configuration](docs/CONFIGURATION.md) | Every environment variable, and where files live |
| [Running a server](docs/server/DEPLOY.md) | Domain, TLS, accounts, workers, backups, the runbook |
| [Architecture](docs/ARCHITECTURE.md) | How the pieces fit, for contributors |
| [Modding](docs/MODDING.md) | Adding rules, units and mechanics |
| [HTTP and tool API](docs/API.md) | Driving CITAR from your own code |
| [Troubleshooting](docs/TROUBLESHOOTING.md) | When something does not work |
| [FAQ](docs/FAQ.md) | Including the honest answers about model strength |

The same pages are on the [wiki](https://github.com/jprodgers/CITAR/wiki) and at
[jprodgers.github.io/CITAR](https://jprodgers.github.io/CITAR).

## How good are the models, actually?

Not very, yet — and that is the interesting part.

A small local model can found cities, research sensibly and hold a conversation about a trade, and
will still lose to a scripted bot that has no idea what it is doing beyond a few hundred lines of
heuristics. Larger models play better and cost more per turn. CITAR exists to put numbers on that
rather than anecdotes, which means the measurement has to be honest about its own limits: see
[FAQ](docs/FAQ.md#how-strong-are-the-models) and
[KNOWN_ISSUES.md](KNOWN_ISSUES.md).

## Contributing

Bug reports, ideas and pull requests are all welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). The
short version:

```bash
git clone https://github.com/jprodgers/CITAR && cd CITAR
pip install -e ".[dev]"
python -m unittest discover -s tests     # 456 tests, about four minutes
citar serve --debug
```

Most content is data: the ruleset is UnCiv-style JSON, and a new unit or building is a JSON entry,
not code. [docs/MODDING.md](docs/MODDING.md) covers that;
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) covers the rest.

## Licence and credits

CITAR is licensed under the [Mozilla Public License 2.0](LICENSE).

Its rules, numbers and much of its game logic are derived from
**[UnCiv](https://github.com/yairm210/Unciv)** by Yair Morgenstern and contributors, also MPL-2.0.
No UnCiv graphics, sounds or flavour text are included. Full attribution is in
[NOTICE.md](NOTICE.md).

*Sid Meier's Civilization* is a trademark of Take-Two Interactive. CITAR is an independent project,
not affiliated with or endorsed by Take-Two, 2K, Firaxis Games or the UnCiv project, and contains
no assets from any commercial Civilization title.
