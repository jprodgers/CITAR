# Servers, costs and reports

Three connected things: a registry of the machines that run models, a ledger of what every piece of
work used, and reports that price the second using the first.

The reason they exist: "this model is better" is a weak claim without "and it cost four times as
much to find out". CITAR tracks the cost side so that comparisons can include it.

---

## Servers

**Servers** in the top navigation (`#/pool`) is the one page of every machine and service that runs
models. Seats, benchmark suites, probe runs and report narratives all pick *server → model → load
profile* from it. There are no ad-hoc endpoints anywhere else in CITAR — this is the one place.

It lists every machine the viewer owns or may use, the ones this server connects to itself
(**direct**: the host, its online APIs, a dry run, an administrator's machines on the network) and
the ones behind a CITAR helper ([Workers](server/WORKERS.md)) alike: for an administrator the
site's machines, their own and the members'; for a member their own and the ones a server group
shares with them. Each machine is a card, and its editors open from it as dialogs: **Connection &
models**, **Hardware & power**, **Costs**, **Quiet hours**, **API key** (a machine the server
dials), **Helper** (a helper machine's tokens), **Sharing** (the group that shares it),
**Disable** and **Remove**. The page's head has **Add a machine** and **New group**, and for
administrators **Electricity plans** and **Settings**. A control shows only when the server would
take it from the viewer, so a member's page never calls an administrator's route; a machine shared
with you shows its models, its quiet hours and **Test**. The page's address before 0.1.6,
`#/servers` (or `#/servers/<id>`), leads here, opening that machine.

### Kinds

| Kind | Costed as |
|---|---|
| **Owned** | Depreciation of its components, plus electricity |
| **Leased / rented** | A fixed monthly amount and/or an hourly rate while in use, plus electricity if you pay it |
| **Online API** | Per-token prices, optionally a subscription |
| **Test** | The dry run. Never costed, never scored |

### Connection

Provider (LM Studio, Ollama, OpenAI-compatible, Anthropic, Dry run), base URL, the LM Link device
for a PC reached through this one's LM Studio, how many requests it takes at once (its slots), and
whether CITAR loads and unloads models on it. A machine the server connects to itself is an
administrator's to point at an address; a member's is a helper machine (whose helper holds the
address and any key) or the dry run, which dials nothing.

### API keys

Pasted once, never shown again, never written to the project folder. Three backends:

- **OS credential store** (default) — Windows Credential Manager, macOS Keychain, Linux Secret
  Service, via `keyring`.
- **Environment variable** — you name it; CITAR reads it.
- **Encrypted file** outside the project — scrypt + Fernet, unlocked with its passphrase once per
  server start or from `CITAR_KEYS_PASSPHRASE`. This is the headless-Linux option.

### Models and load profiles

**Detect models** fills the catalogue of a machine the server dials; a helper machine's models are
the ones its helper reports when it connects. Each model carries inference defaults — tool mode, reasoning
effort, max tokens, temperature, tool-call limit — and, on LM Studio, **load profiles**: context
length, GPU offload, parallel slots, unload-after-idle, exclusivity, speculative decoding.

**Estimate** shows the memory a profile needs before you try it. **Load now** and **Unload now**
act immediately, which is how you free a GPU without leaving the page.

### Hardware

**Collect from this PC** (administrators, on the host) reads the machine CITAR is running on; a
helper's scan replaces a helper machine's details when it connects. For other machines, download
`collect_hardware.py` (any platform with Python 3), `collect_hardware.sh` (Linux or macOS without
Python) or `collect_hardware.ps1` (Windows without Python), run it there, and **Import JSON**.
Everything can also be typed in: CPU, GPUs and VRAM, RAM or unified memory, disks.

### Power

Idle watts, plus the extra watts at full CPU and with the GPU busy. A suggestion is derived from
the hardware. On the machine running CITAR, GPU power (via `nvidia-smi`) and CPU load are also
**sampled live** every five seconds, so measured energy replaces the estimate where it can.

### Costs

**Components** for owned machines — price, purchase date, lifespan, resale value, with a later GPU
as its own line — and **cost periods** that apply from a date. Changing a price means adding a
period, never editing history, so a report of last month still reflects last month.

**Electricity plans** (⚡) are shared between machines: flat, time-of-use or tiered rates, plus a
fixed monthly charge that can be spread over the household's kWh, charged as a set share, or
ignored. Effective-dated like everything else. They are the site's bill, so an administrator keeps
them and names them in machines' costs; a member puts their own power cost in a cost period's
fixed monthly figure.

### Quiet hours

Windows per weekday when nothing runs on the machine, for anyone, its owner included, with optional
unloading (a machine CITAR loads models on) and grace minutes for a turn in progress: games,
benchmarks, probe runs and the lab finish the model turn in progress and pause until the hours end.
A helper machine's hours are in its owner's time zone, a machine the server dials keeps this
server's clock. The page's head lists the machines in their quiet hours right now.

### Settings

Administrators: the currency, and which machine runs CITAR — its CPU runs the engine, the bots and
the lab, so it gets a share of every game's cost.

The machines are rows of the server's database since 0.1.6, edited on the Servers page (a pre-0.1.6
`config/servers.json` is imported once; see [Configuration](CONFIGURATION.md#the-machines-in-the-database-not-a-file)).

---

## The usage ledger

Every game with an AI seat, benchmark game, probe run, lab game and report narrative is written to
`saves/usage/*.jsonl`:

- which servers it held, and for how long
- how long each model spent generating
- tokens: input, output, reasoning, cache reads and cache writes
- CPU time on the host
- one-minute power samples of the host

**The ledger holds no prices.** Reports price it with the settings in force at the time, so
correcting a rate later corrects every report that touches that period. This is the single most
useful property of the design: you do not have to know your electricity tariff before you start.

---

## Reports

**Reports** builds reports on demand. Nothing is generated automatically.

1. Start from a preset — *Cost of a game, run or experiment*, *Model comparison*, *Server
   comparison over time*, *Model behaviour in scenarios*, *Bot lab experiments*, *Everything* — or
   tick sections yourself.
2. Choose the period and scope: activity types, servers, models, or specific benchmark runs, probe
   runs, lab experiments and games. **Data** says what the figures come from: what you may see (the
   default), that plus pooled averages, or pooled averages only (below).
3. Options: count electricity as the full draw or only the extra power the work caused, and the
   lifespan range for the depreciation sensitivity table.
4. Optionally have a model write an analysis — pick server, model and profile, plus instructions.
   The computed findings never need a model, and the narrative's own cost is tracked too. A report
   whose analysis a machine reached through its helper writes waits in the queue while that helper is
   offline (it says so), and rides out a helper that drops while it is built for up to half an hour.
5. **Run report.**

**View** opens it in place; **Open in new tab** and **Download** give a single self-contained HTML
file — inline SVG charts with hover values and a data table under each, light and dark themes,
printable, no internet needed. **Re-run** makes a fresh one with current data; **Edit copy** loads
its choices back into the builder. **Share** (for the report's owner) sets who can see it, as for a
game: members, a link, people by name, or the whole internet (an administrator's permission); a report
shared by link or published has a page for people with no account, `/r/<key>` or `/r/<id>`. The
phone site's Reports tab lists the same reports.

### What a report contains

Building a report needs the `run_reports` capability; the report is yours, private until you share
it. What it gathers depends on whose it is:

- **A member's report** covers what its owner may see: the activities of their own games, benchmark
  runs, probe runs, lab experiments and reports, and of those shared with them, with members or with
  everyone. Another member's private game or run is not in it, not even by name or id.
- **Costs are counted on your own machines only.** An activity that also ran on the server's own
  machines or on another member's (your game's engine on the CITAR host, your seat on a machine shared
  with you) keeps its outcome and its usage, but not what it cost there; a data-quality note says how
  many such activities there were. The servers, hardware, costs-over-time and lifespan sections show
  your machines and no one else's, and the data-quality notes about pricing (a model with no token
  price, say) are about your machines only, never another's.
- **Pooled averages** (Data: "plus pooled averages" or "pooled averages only") add, in the model
  comparison, per-model figures over the games other members pool (each game's own pooling setting
  and its owner's), each model shown only once enough different accounts are behind it, so no
  average is one person's results.
- **An administrator's report** covers everything on the server, every machine priced.

A report is built when it runs, with what its owner may see then: sharing something with you later
does not change a report already built; **Re-run** builds it again. A report built while its owner's
account is suspended (or otherwise not in good standing, an administrator's included) covers only
what is published to everyone, with no machine priced.

**Sharing a report shares the report.** Whoever it reaches (a member it is shared with, a visitor with
its link) reads it, its title and its progress. Of how it was made they get only what they could have
chosen themselves: the items and machines of its scope that they may see, and whether it has a written
analysis, but not the analysis's instructions or the machine, model and profile that wrote it; a
failed build shows them its error, not its traceback. Its owner and administrators see all of it.

### Sections

Summary and key findings · where the money went, by configuration, server and activity type, split
into depreciation, fixed costs, usage rate, idle and work energy, and tokens · cost per unit of
work (per game, model turn, million tokens, performance point, win, probe case) · servers,
including allocated versus unallocated fixed costs, utilisation and metered energy · costs over
time · model comparison with 95% confidence intervals, speed, reliability, tokens, a
cost-efficiency frontier and significance notes · benchmark runs · scenario probes with outcomes
and pass rates · model behaviour: tool mix, how turns ended, common errors · bot lab experiments ·
what-if pricing, the same tokens at each API's prices and the same hours on each machine · hardware
lifespan sensitivity · hardware · data quality: estimated versus measured energy, missing prices,
small samples · all activities · methodology.

Reports are kept in `saves/reports/<id>/` as `report.html` plus the spec and metadata.

### Data quality

The data-quality section is not decoration. It says which energy figures were measured and which
were estimated from nameplate watts, which cost periods have no price set, and which comparisons
rest on too few games. A report that looks precise and rests on two games and a guessed electricity
rate should say so, and this one does.
