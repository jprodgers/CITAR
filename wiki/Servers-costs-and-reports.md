# Servers, costs and reports

Three connected things: a registry of the machines that run models, a ledger of what every piece of
work used, and reports that price the second using the first.

The reason they exist: "this model is better" is a weak claim without "and it cost four times as
much to find out". CITAR tracks the cost side so that comparisons can include it.

---

## Servers

**Servers** in the top navigation is the registry of every machine and service that runs models.
Seats, benchmark suites, probe runs and report narratives all pick *server → model → load profile*
from it. There are no ad-hoc endpoints anywhere else in CITAR — this is the one place.

### Kinds

| Kind | Costed as |
|---|---|
| **Owned** | Depreciation of its components, plus electricity |
| **Leased / rented** | A fixed monthly amount and/or an hourly rate while in use, plus electricity if you pay it |
| **Online API** | Per-token prices, optionally a subscription |
| **Test** | The dry run. Never costed, never scored |

### Connection

Provider (LM Studio, Ollama, OpenAI-compatible, Anthropic, Dry run), base URL, the LM Link device
for a PC reached through this one's LM Studio, how many games may use it at once, and whether CITAR
loads and unloads models on it.

### API keys

Pasted once, never shown again, never written to the project folder. Three backends:

- **OS credential store** (default) — Windows Credential Manager, macOS Keychain, Linux Secret
  Service, via `keyring`.
- **Environment variable** — you name it; CITAR reads it.
- **Encrypted file** outside the project — scrypt + Fernet, unlocked with its passphrase once per
  server start or from `CITAR_KEYS_PASSPHRASE`. This is the headless-Linux option.

### Models and load profiles

**Detect models** fills the catalogue. Each model carries inference defaults — tool mode, reasoning
effort, max tokens, temperature, tool-call limit — and, on LM Studio, **load profiles**: context
length, GPU offload, parallel slots, unload-after-idle, exclusivity, speculative decoding.

**Estimate** shows the memory a profile needs before you try it. **Load now** and **Unload now**
act immediately, which is how you free a GPU without leaving the page.

### Hardware

**Collect from this PC** reads the machine CITAR is running on. For other machines, download
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

**Electricity plans** (⚡) are shared between servers: flat, time-of-use or tiered rates, plus a
fixed monthly charge that can be spread over the household's kWh, charged as a set share, or
ignored. Effective-dated like everything else.

### Restricted hours

Windows per weekday, with optional unloading and grace minutes for a turn in progress. Benchmarks,
probe runs and the lab respect them; games you start yourself show a 🌙 warning instead of
refusing. The page header lists servers that are restricted right now.

### Settings

Currency, and which server is the machine running CITAR — its CPU runs the engine, the bots and the
lab, so it gets a share of every game's cost.

The registry lives in `config/servers.json` (`citar where` prints the path).

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
   runs, lab experiments and games.
3. Options: count electricity as the full draw or only the extra power the work caused, and the
   lifespan range for the depreciation sensitivity table.
4. Optionally have a model write an analysis — pick server, model and profile, plus instructions.
   The computed findings never need a model, and the narrative's own cost is tracked too.
5. **Run report.**

**View** opens it in place; **Open in new tab** and **Download** give a single self-contained HTML
file — inline SVG charts with hover values and a data table under each, light and dark themes,
printable, no internet needed. **Re-run** makes a fresh one with current data; **Edit copy** loads
its choices back into the builder.

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


---

*This page is generated from [`docs/REPORTS.md`](https://github.com/jprodgers/CITAR/blob/main/docs/REPORTS.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
