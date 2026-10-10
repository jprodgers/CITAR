# Scripted bots

The bot matters more than it looks like it should. Every model score in CITAR is a comparison
against it, so what the bot does is the unit the whole benchmark is denominated in.

It is deliberately not a neural anything: a few thousand lines of heuristics that can be read,
reasoned about, and changed on purpose. It is compiled into the Rust engine (`crates/citar-bot`) as
**bot versions**: `basic-1` is the port of 0.1.5's Python bot (`basic.py`, which the tag
`python-engine-0.1.6` keeps), and `idle`
founds a capital and does nothing else. A change to the bot that should not move existing results is
a new version, a deliberate copy (`src/basic1/` and `params/basic-1.json` to `basic2` and
`basic-2`) with its own row in the version table; `basic` names the latest version wherever it is
used.

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

`aggression` (0–1, per seat or per profile) controls army size and how readily it starts wars. Every
other number the bot decides with — 373 of them in 17 groups for `basic-1`, from the weight of a
point of food in a building to the power ratio at which it declares war — is a named, documented
**parameter**. Each version has its own parameter schema (`crates/citar-bot/params/basic-1.json`,
from which the bot's parameter struct is generated), which a bot profile can change and which the
Bots page shows.

## Its limits

The bot is currently **capped by happiness**: on a Quick Small map it reaches two to five cities by
turn 150 and stops expanding. That is a real ceiling and it is the main open piece of balance work
— see [KNOWN_ISSUES.md](https://github.com/jprodgers/CITAR/blob/main/KNOWN_ISSUES.md).

What it means for a score: the bot is a competent but limited opponent. Beating it is not the same
as playing Civ well, and losing to it badly is meaningful.

`basic-1` plays as the Python bot did, measured over whole games against that bot's own (cities,
technologies, score and the rest at turns 100, 200 and 300, within the statistical gates of
crates/citar-engine/DESIGN.md P2.4.4). The differences it has are explained: a fixed escort rule
keeps more settlers alive, so it has more cities and units at turn 100 (gone by turn 200); and on
Small maps it declares about 18% fewer wars from about turn 130 on, and so takes fewer cities. That
last gap was accepted for 0.1.6, since the bots are trained again in 0.1.7; Standard and Large maps
do not show it.

---

## Bot profiles and rankings

A **profile** is a named configuration of the bot: which *code* it runs (`basic`, which follows the
latest version; a pinned version such as `basic-1`; or `idle`), an optional fixed *aggression*, and
*parameter overrides* on top of that version's defaults. Profiles are what lab experiments, lobby
seats, benchmark opponents and probe runs play; a seat that names none plays **Standard**, the latest
version with its defaults. In the new-game form a bot seat defaults to **Best bot**: the best-ranked
profile on the server whose rating describes its current settings (its current revision, or the same
parameters and aggression on an earlier build of the latest version; falling back to the best-ranked at
all, then Standard; never Idle). It is resolved when the game is created and recorded in the seat, so a
game keeps its bot when the rankings change. Benchmarks default to Standard.

The frozen snapshots of 0.1.5's Python bot (and the built-in profiles v0, v1 and 22 Sep that played
them) were archived with 0.1.5: a profile, seat or experiment that names one is refused with
a message saying so. A saved profile of 0.1.5 on a snapshot stays listed: its page shows it read-only
with that message, an administrator can delete it, and it is never the Best bot.

The **Bots** page lists them, edits them and ranks them:

- **Profiles.** Built-in profiles (Standard, Classic production, Idle) can't be edited; *Fork* one
  to make your own. The editor shows every parameter of the profile's version in its group with an
  explanation, its default and a sensible range; changed values are highlighted, *changed only*
  shows just the overrides, and lists (policy order, belief preferences) can be reordered or picked
  from named presets. Saving cleans the overrides against the version's schema: unknown names, a
  fraction for a whole number and names a list does not offer are refused, and values equal to the
  default are dropped.
- **Revisions.** Saving a change to what plays (code, aggression or parameters) makes a new
  revision, with a note, and keeps the old one. Results are recorded against a revision.
- **A/B test.** Pick two or more profiles and queue a lab experiment: they take the seats in turn
  (A, B, A, B), the seat order rotates every game, and every profile plays every start position on
  the same maps. The profiles are resolved into the experiment when it is queued, and `basic` is
  pinned to the version it names then.
- **Rankings.** Every lab game counts. An *entry* is one exact configuration at one difficulty.
  Its **fingerprint** hashes the build id (the engine's and the bot's code and the ruleset), the
  version, the overrides and the profile's *fixed* aggression ("seat" when the seat decides) — never
  the aggression a lab seat gets from its start position, so one profile is one entry however it is
  seated. A Deity bot and a Prince bot of the same configuration are separate entries (the ladder
  experiments become a handicap scale), and so are two builds: a build that changes what the bot does
  starts new entries, and two builds' results are never mixed in one. Each lab result records per seat
  the build, the version, the profile and its revision, the overrides and the fingerprint as they were
  when the game was played, and the entries are named from those records (the profile's name, its
  revision past the first, and the version and build where one profile has entries on several).
  Each game's finishing order is split into pairwise results (weighted 1/(players−1) so a seat counts
  about one game) and fitted with a Bradley–Terry model on the Elo scale: 400 points is 10:1 odds of
  finishing ahead. Two drawn games against a 1500 anchor keep thin entries near 1500. The standard
  error ignores the correlation between pairs from one game, so read it as a lower bound.
- **The ladder of 0.1.6 starts empty.** 0.1.5's lab history played Python bots that no longer run and
  was archived; the first entries come from new experiments. A game is rated only when every seat's
  result records its fingerprint, build and version, so 0.1.5's results, which record no build or
  version, are never rated, even where an install upgraded in place still keeps them under
  `saves/lab/results`.
- **Over time.** The chart refits the ratings on the games finished by the end of each day. A
  profile on `basic` gets a new entry whenever a build changes the bot (the fingerprint changes), so
  the Standard line is the history of the bot itself.

From the command line, a profile id works anywhere a bot name does: `citar balance --bots
standard,my-profile`, or `{"profile": "my-profile"}` as a lab seat (with optional `"params"` layered
on top), or `"profile"` as the base of a factorial experiment. `"best"` works there too, and is
queued as the profile it stands for at submission, so its games are rated as that profile's.

Versions, schemas, cleaning, fingerprints and the build id come from the engine
(`citar.engine_api`). A process that plays a modded ruleset (`CITAR_RULESET_DIR`,
[MODDING.md](Modding#playing-a-modded-ruleset)) has its own build id, so its fingerprints and
ratings are its own.

HTTP (signed in; changes need an administrator): `GET /api/bots/profiles`, `GET|PUT|DELETE
/api/bots/profiles/{id}`, `POST /api/bots/profiles`, `POST /api/bots/profiles/{id}/fork`,
`GET /api/bots/schema?engine=basic` (`{engine, groups}`: the version's schema), `GET
/api/bots/engines`, `GET /api/bots/rankings`, and `POST /api/bots/experiments` to queue an A/B
experiment.

Saved profiles live in `saves/bots/profiles/`.

---

## The balance simulator

Plays many bot games in parallel and reports on pacing (techs, eras, cities, population by turn),
economy (unhappiness, bankruptcy, starvation), conflict (wars, captures, eliminations, barbarians),
what gets built, victory types and win rates per bot type. It writes a JSON report to
`saves/balance/`.

```bash
citar balance --games 12 --players 4 --size small --nation BenchmarkCiv --label check
citar balance --games 22 --bots standard,my-profile --label ab        # head to head
citar balance --games 22 --players 2 --size duel --bots basic,idle    # can it beat a passive player?
```

`--bots` takes profile ids and bot versions (`basic`, `basic-N`, `idle`); a name that is neither,
or a frozen snapshot of 0.1.5, stops the run before it starts. One seed plays one game: the bots draw
from the game's seed.
To A/B test a change of parameters, make a profile with it and run `--bots standard,<profile>`; to
A/B test a change of code, make it in a new version and run `--bots basic-1,basic-2`. Both sides
play in the same games, on the same maps, which removes map luck from the comparison — the single
most important thing about measuring a bot change.

The simulator runs one game per core, and uses all of them.

---

## The lab

For changes that need more than a few dozen games, the lab is a resumable queue of experiments that
runs for days.

```bash
citar lab submit experiment.json     # queue it (bot versions are pinned at submit time)
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
  "seats": [{"label": "tuned", "bot": "basic", "params": {"war_prep_rate": 2.0}},
            {"label": "standard", "profile": "standard"}],
  "rotate": true
}
```

Game *i* uses `seed + i` and `maps[i % len(maps)]`. With `rotate`, the seat list is rotated by *i*
so every label plays every start position — the same reason as above, removing position advantage
from the comparison.

A seat names a bot version (`"bot"`: `basic`, `basic-N` or `idle`, with optional `"params"`) or a
profile (`"profile"`). **Versions are pinned when an experiment is submitted**: `basic` becomes the
version it names then, so a version added later cannot contaminate a queued experiment (use
`"bot": "live"` to opt out and play the latest version at play time). A seat's overrides are
checked against its version's schema at submission, so a misspelt parameter is refused then, not
after a day of games; so is an `"aggression"` that is no number, and one outside 0 to 1 is held to
it. Every result records per seat the build that played it, the version, the profile and revision,
the overrides, the fixed aggression its bot played and the fingerprint, so a queued experiment that
a newer build finishes is labelled with that build.

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

1. If a parameter can say it, make a profile: fork Standard on the Bots page and change it.
2. Otherwise make a new version: copy `crates/citar-bot/src/basic1/` and `params/basic-1.json` to
   `basic2/` and `basic-2.json`, add its row to the version table, make the change there, and
   rebuild the extension (`cargo xtask develop`).
3. `citar balance --games 22 --bots standard,<profile> --label whatever` (or `--bots
   basic-1,basic-2`).
4. Read the report. Twenty-two games is enough to see a large effect and nowhere near enough to see
   a small one.
5. For anything subtle, queue a lab experiment (the Bots page's *A/B test*) and leave it running.

The working log of this campaign — what has been tried, what worked, what the current numbers are —
is in [research/BOT_TUNING.md](Bot-tuning-log).

### Checking a change to the code

A new version, or any change under `crates/citar-bot`, has to keep the bot sound as well as make it
better. With a Rust toolchain (CONTRIBUTING.md, "Rust"):

```bash
cargo nextest run -p citar-testkit --test bot         # the bot's tests: one turn's effects, whole
                                                      # games, the fixture sweep, the stability runs
cargo nextest run -p citar-testkit --test rules       # the rule scripts, the bot's (bot_*.toml) among them
cargo golden check                                    # the bot set: four bot games, round by round
cargo refcheck run --with-bot --strict                # deal values against the recorded Python bot's
cargo soak --drivers bot --games 60 --sizes small     # whole bot games, every check on
cargo chaos --drivers mixed --seconds 600             # bots and random agents among random calls
```

A change that plays differently moves the `bot` golden set (and the long set's bot game): bless it
(`cargo golden bless bot`, `cargo golden bless long`) and say why in the commit. The soak fails a
game in which a bot loops on a refused action (more than 200 refusals of one tool in one turn) and
reports every bot's actions taken and refused. `cargo refcheck bot-agreement` compares `basic-1`'s
decisions with the Python bot's on the recorded states; a new version is not held to it, which is
the point of a new version, but its statistics are: `citar-sim baseline` and
`scripts/refcheck/summarize.py` compare whole games ([refcheck/README.md](https://github.com/jprodgers/CITAR/blob/main/refcheck/README.md),
"The statistical baseline").


---

*This page is generated from [`docs/BOTS.md`](https://github.com/jprodgers/CITAR/blob/main/docs/BOTS.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
