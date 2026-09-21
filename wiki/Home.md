# CITAR documentation

**Civ Inspired Tool for AI Research** — a Civilization V-style 4X game whose players can be
language models.

New here? [Quick start](Quick-start) gets you a game, a model and a benchmark in that order.

---

## Playing

| | |
|---|---|
| [Quick start](Quick-start) | First game, first model, first benchmark |
| [Installing](Installing) | Every install route, and how to remove it |
| [Playing in the browser](Playing-in-the-browser) | Every screen, every shortcut |
| [AI players](AI-players) | LLM seats, MCP, providers, guard rails |

## Research

| | |
|---|---|
| [Benchmarks](Benchmarks) | Suites, scheduling, scoring, reading a result honestly |
| [Scenarios and probes](Scenarios-and-probes) | Testing one decision instead of a whole game |
| [Servers, costs and reports](Servers-costs-and-reports) | The machine registry, the usage ledger, costed reports |
| [Scripted bots](Scripted-bots) | The yardstick: how it plays, and how to change it |
| [Bot tuning log](Bot-tuning-log) | The working record of the balance campaign |

## Running it for other people

| | |
|---|---|
| [Deploying a server](Deploying-a-server) | Installer, Docker, or by hand |
| [Preparing a VPS](Preparing-a-VPS) | SSH, firewall, swap, DNS — before CITAR |
| [Single sign-on](Single-sign-on) | Registering an app with each provider |
| [Workers](Workers) | Lending a GPU to a server without opening a port |
| [Runbook](Runbook) | Health checks, failures, backups, restores, upgrades |
| [Design: accounts and sharing](Design-accounts-and-sharing) | How the multi-user side works, and why |

## Building on it

| | |
|---|---|
| [Architecture](Architecture) | How the pieces fit, and where to change things |
| [HTTP and tool API](HTTP-and-tool-API) | Driving CITAR from your own code |
| [Modding](Modding) | Adding rules, units and mechanics |
| [Configuration](Configuration) | Every variable, and where files live |
| [Contributing](https://github.com/jprodgers/CITAR/blob/main/CONTRIBUTING.md) | Tests, conventions, how to send a change |

## When it goes wrong

| | |
|---|---|
| [Troubleshooting](Troubleshooting) | Start with `citar doctor` |
| [Questions](Questions) | Including the honest answer about model strength |
| [Known issues](https://github.com/jprodgers/CITAR/blob/main/KNOWN_ISSUES.md) | What is broken or missing in this release |
| [Changelog](https://github.com/jprodgers/CITAR/blob/main/CHANGELOG.md) | What changed, and what breaks |

---

## The shape of the thing, in one paragraph

`citar/engine/` holds the rules and does no I/O — it turns a state and an action into a new state
or an error. Everything else calls it: the browser client over HTTP, a scripted bot in-process, a
language model through an adapter, an external agent over MCP. Every action any of them can take is
registered once in `citar/engine/tools.py`, which is why the interfaces cannot drift apart and why
a model has exactly the powers a human has. Around that sit the measurement parts — metrics,
benchmarks, probes, a usage ledger and costed reports — because the point is not only to play the
game but to know what happened.


---

*This page is generated from [`docs/index.md`](https://github.com/jprodgers/CITAR/blob/main/docs/index.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
