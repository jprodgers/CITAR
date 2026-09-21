# Scripted bots

The bot matters more than it looks like it should. Every model score in CITAR is a comparison
against it, so what the bot does is the unit the whole benchmark is denominated in.

It is in `citar/bots/basic.py`, and it is deliberately not a neural anything: a few thousand lines
of heuristics that can be read, reasoned about, and changed on purpose.

## What it does

- **Research by need.** It beelines the most valuable technology for its situation, including
  luxuries it cannot improve yet.
- **Buildings by simulation.** For each candidate it simulates the city with that building built
  and picks by the result, rather than from a static priority list.
- **Everything else in the ruleset.** Adopts policies, founds a pantheon and a religion and spreads
  it, uses great people, sends spies, courts city-states.
- **Defence.** Bombards with its cities, answers threats, clears barbarian camps, keeps settlers
  and workers out of danger.
- **Economy.** Expands while happiness allows, spends surplus gold, trades luxuries.
- **War.** Against weaker neighbours it builds an army with siege units, gathers at a rally point
  near the target, then sieges and captures cities.

`aggression` (0–1, per seat) controls army size and how readily it starts wars.

## Its limits

The bot is currently **capped by happiness**: on a Quick Small map it reaches two to five cities by
turn 150 and stops expanding. That is a real ceiling and it is the main open piece of balance work
— see [KNOWN_ISSUES.md](https://github.com/jprodgers/CITAR/blob/main/KNOWN_ISSUES.md).

What it means for a score: the bot is a competent but limited opponent. Beating it is not the same
as playing Civ well, and losing to it badly is meaningful.

---

## The balance simulator

Plays many bot games in parallel and reports on pacing (techs, eras, cities, population by turn),
economy (unhappiness, bankruptcy, starvation), conflict (wars, captures, eliminations, barbarians),
what gets built, victory types and win rates per bot type. It writes a JSON report to
`saves/balance/`.

```bash
citar balance --games 12 --players 4 --size small --nation BenchmarkCiv --label check
citar balance --games 22 --bots basic,snapshot_old --label ab          # head to head
citar balance --games 22 --players 2 --size duel --bots basic,idle     # can it beat a passive player?
```

To A/B test a change, copy `basic.py` to `citar/bots/snapshot_<name>.py` *before* editing, then run
with `--bots basic,snapshot_<name>`. Both play in the same games, on the same maps, which removes
map luck from the comparison — the single most important thing about measuring a bot change.

A 330-turn Quick game with four bots takes five to six minutes on one core, and the simulator uses
all of them.

---

## The lab

For changes that need more than a few dozen games, the lab is a resumable queue of experiments that
runs for days.

```bash
citar lab submit experiment.json     # queue it (bot code is frozen at submit time)
citar lab run --workers 11           # the runner; safe to restart
citar lab status                     # what is running, progress per experiment
citar lab report NAME                # results per seat label, and head to head
citar lab stop                       # ask it to exit; running games are redone later
```

An experiment is a JSON object:

```json
{
  "name": "prince-baseline", "priority": 5, "games": 24, "seed": 1000,
  "size": "small", "maps": ["continents", "pangaea"], "speed": "Quick", "turns": 0,
  "difficulty": "Prince", "nation": "BenchmarkCiv",
  "seats": [{"label": "new", "bot": "basic", "params": {}},
            {"label": "old", "bot": "frozen_ab12cd34"}],
  "rotate": true
}
```

Game *i* uses `seed + i` and `maps[i % len(maps)]`. With `rotate`, the seat list is rotated by *i*
so every label plays every start position — the same reason as above, removing position advantage
from the comparison.

**Bot code is frozen when an experiment is submitted** (`citar/bots/frozen_<hash>.py`), so editing
`basic.py` afterwards cannot contaminate a running experiment. Use `"bot": "live"` to opt out.

**Factorial experiments** (`"factors"`) screen many parameters at once: each seat plays with its own
mix of factor levels, spread evenly across seats and shuffled per game, so each factor's effect can
be measured within games rather than between them.

Progress is visible on the **Lab** page in the web client — runner health, per-experiment progress
and ETA, per-game turn progress, and the runner log. A long run you cannot see the progress of is a
long run you will restart unnecessarily.

Every finished lab game is written to the usage ledger, so experiments can be costed like anything
else.

---

## Changing the bot

The workflow that works:

1. Snapshot: `cp citar/bots/basic.py citar/bots/snapshot_before.py`
2. Change `basic.py`.
3. `citar balance --games 22 --bots basic,snapshot_before --label whatever`
4. Read the report. Twenty-two games is enough to see a large effect and nowhere near enough to see
   a small one.
5. For anything subtle, queue a lab experiment and leave it running.

The working log of this campaign — what has been tried, what worked, what the current numbers are —
is in [research/BOT_TUNING.md](research/BOT_TUNING.md).
