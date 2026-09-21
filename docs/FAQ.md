# Questions

## How strong are the models?

Not very, and that is the finding rather than a disclaimer.

A small local model (4B or so) founds cities, researches sensibly, answers a trade offer coherently
and falls apart somewhere in the mid-game: it stops noticing its own unhappiness, leaves units
where they will die, and loses to a scripted opponent that is a few thousand lines of heuristics.
Larger models play a recognisable game of Civ and take five to ten minutes a turn on a consumer
GPU.

What makes this interesting is *how* they fail. It is rarely the rules — models know what a library
does. It is holding a plan across forty turns, noticing that a plan has stopped working, and
weighing a cost now against a benefit later. Those are the things a long game tests and a question-
and-answer benchmark does not.

CITAR is built to put numbers on that rather than anecdotes, which means being honest about the
measurement too — see [reading a result honestly](BENCHMARKS.md#reading-a-result-honestly).

## Do I need a GPU?

No.

- Against scripted bots, no model is involved at all.
- With an API key, the model runs on somebody else's hardware.
- A small model runs on a CPU, slowly.

A GPU makes local models practical rather than possible. 6 GB of VRAM runs a 4B model at a usable
context length; 24 GB runs a 27B model.

## Which model should I start with?

`citar setup` suggests one for the hardware it finds. Roughly:

| Usable VRAM | Start with |
|---|---|
| Under 4 GB | A 1–2B model, or scripted bots |
| 4–7 GB | A 4B instruct model at 32k context |
| 8–11 GB | An 8B model |
| 12–16 GB | A 14B model |
| 24 GB | A 27B model |

Raise the context length before the model size. A model that cannot see its whole briefing plays
worse than a smaller one that can.

## Does CITAR send anything anywhere?

No. There is no telemetry, no analytics and no phoning home. The only outbound connections are to
the model endpoints you configure — which may be entirely on your own machine — and, if you run a
worker, to the CITAR server you pointed it at.

API keys go to your operating system's credential store and are never written to the project
folder, a save file or a report.

## Is this legal? Is it Civilization?

No, it is not Civilization, and it contains nothing from any commercial Civilization title: no
graphics, no sound, no text. The rules and numbers come from
[UnCiv](https://github.com/yairm210/Unciv), an open-source game under the MPL-2.0, and CITAR is
under the same licence for that reason. Game rules and numeric values are not themselves protected
by copyright; the expression is, and none of it is here.

*Sid Meier's Civilization* is a trademark of Take-Two Interactive. CITAR is unaffiliated. See
[NOTICE.md](https://github.com/jprodgers/CITAR/blob/main/NOTICE.md).

## Can I play against my friends?

On a local network, start with `--host 0.0.0.0` and share the seat links.

Over the internet, run a [server](server/DEPLOY.md): accounts, invitations, single sign-on,
sharing and budgets. A small VPS handles about one concurrent game per core.

## Can several models play each other?

Yes — that is what most of CITAR's tooling is for. Give every seat to an LLM and watch with full
vision, or set up a [benchmark suite](BENCHMARKS.md) and have every model play the same map.

Up to 24 civilizations on a Gargantuan map, though a turn with 24 AI players takes as long as you
would expect.

## How long is a game?

| Speed | Turns | Rough length |
|---|---|---|
| Quick | 330 | Benchmark default |
| Standard | 500 | The default for play |
| Epic | 750 | |
| Marathon | 1500 | |

Against scripted bots on a small map: minutes. With a local model: a turn is one to ten minutes, so
a 330-turn game is hours to days. Benchmark runs survive restarts precisely because of this.

## Why is my benchmark so slow?

Because a full game is a lot of turns and every one of them is a model call or several. That is
inherent, not a bug, and it is why:

- restricted hours exist (a GPU in a bedroom),
- runs resume after a reboot,
- `citar bench --turns 2` exists for smoke tests,
- and [probes](SCENARIOS.md#probes) exist for testing one decision instead of a whole game.

## Can I use my own ruleset or mod?

Yes. Most content is data. A mod entry in `citar/data/custom/` in UnCiv format works as long as the
engine knows the uniques it uses. To move to a newer UnCiv release, point
`scripts/import_unciv.py` at a checkout and regenerate. See [MODDING.md](MODDING.md).

## Can I drive CITAR from my own code?

Yes. Every action is an HTTP call with a seat token:

```bash
curl -X POST http://127.0.0.1:8765/api/games/$GAME/tool \
  -H "Authorization: Bearer $SEAT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"tool": "get_briefing", "args": {}}'
```

`GET /api/tools` lists every tool with its JSON schema. See [API.md](API.md).

## What is the difference between local mode and server mode?

Local mode binds to loopback, signs you in automatically as the owner, and needs no TLS, no
accounts and no configuration. Server mode has real sessions, requires TLS and a public origin, and
refuses to start half-configured.

Every authorisation check runs identically in both. Local mode is a real account with a real
session, not a bypass — which is what stops permission bugs from hiding locally and appearing in
production.

## How do I back it up?

Copy the state directory (`citar where`), and on a server the environment file. That is everything:
saved games, the registry, benchmark runs, reports and the database. A server install has a nightly
backup timer and a documented restore drill — [server/RUNBOOK.md](server/RUNBOOK.md).

## Can I contribute?

Please do — [CONTRIBUTING.md](https://github.com/jprodgers/CITAR/blob/main/CONTRIBUTING.md). Useful things that need no deep knowledge:
running a benchmark and reporting what you find, writing a probe scenario for a decision you care
about, and telling us where the documentation is wrong.
