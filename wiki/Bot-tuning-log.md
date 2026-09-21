# Bot tuning log

The scripted bot (`citar/bots/basic.py`) is the yardstick that humans and AI models are measured against, so it
has to play competently: expand, grow, research, fight, and win games in all the ways UnCiv allows. This file is
the working log of that effort. It records the goal, the method, every experiment and what was decided. **Any
session that continues this work starts here.**

## Goal and success criteria

The user asked for this on 2026-09-18: per-seat difficulty (done), then test runs for as long as needed (days are
fine) so the bots can "actually compete". Benchmark settings: Quick speed, 330 turns, small maps, BenchmarkCiv.

Targets for four equal **Prince** bots on a Small map at Quick speed:

- [ ] Games are decided before the time limit in most cases, by a mix of Scientific, Cultural, Diplomatic and
  Domination victories. Baseline: Time victories 6/6.
- [ ] The leader reaches the end of the tech tree by about turn 250–300. Baseline: 57/80 techs at turn 330, and only
  8% of civs reach the Atomic era.
- [ ] Healthy expansion: about 6+ cities by turn 150. Unhappy on well under 30% of turns (baseline 55%).
- [ ] Wars happen and sometimes succeed, with cities changing hands. Baseline: 0.12 captures per player per game.
- [ ] No resource hoarding: gold is spent (baseline ~1,000 banked late), and missionaries are not spammed
  (baseline 11.6 per player).
- [ ] **Difficulty ladder is monotonic:** Deity > Emperor > Prince > Chieftain in win share and score share.
- [ ] Each change is justified by a head-to-head A/B result (new vs frozen old), not by a single game.

## How the lab works (`citar/lab.py`)

- `python -m citar.lab run --workers 10 --night-workers 2` is a long-running runner, started detached with
  PowerShell `Start-Process` (output in `saves/lab/runner.out`). It plays queued experiments in parallel. Each game
  is an independent subprocess (`python -m citar.lab play SPEC OUT`, files in `saves/lab/jobs/`) at below-normal
  priority. The first version used `ProcessPoolExecutor(max_tasks_per_child=1)`, which deadlocked on Windows with
  Python 3.14 after about an hour (worker processes spawned but never ran). A game that outlives its runner still
  writes its result, and the next runner collects it. During the quiet hours in `benchmarks/settings.json` (21:00–06:00, because of fan noise) it
  drops to the night worker count. Override this in `saves/lab/config.json`, e.g. `{"workers": 10, "night_workers": 2}`;
  the runner re-reads it continuously.
- `python -m citar.lab submit saves/lab/specs/NNN-*.json` queues experiments. Bot code is **frozen at submit time**
  into `citar/bots/frozen_<hash>.py`, so editing `basic.py` never contaminates a queued experiment.
- `python -m citar.lab status` shows progress. `python -m citar.lab report NAME...` shows per-label win share,
  score share with a 95% CI, techs and cities at turns 100/200/300, and head-to-head score-share differences
  (`*` = significant).
- Results are appended per game to `saves/lab/results/NAME.jsonl`, so nothing is lost if the runner or the session
  stops. **To resume after a restart,** check `status` (it says whether the runner PID is alive) and start the
  runner again if needed. Unfinished games are simply replayed.
- `python -m citar.lab stop` stops the runner.
- **Factorial experiments** (`"factors": {"knob": [level0, level1], ...}`, `"base_params"`, `"players": 4`): every
  seat plays the frozen bot with its own mix of knob levels. Each factor's levels are spread evenly over the seats
  of every game, so the report measures each factor's effect *within games*, on score share, final techs, final
  cities and T200 techs. That screens many knobs with one batch of games; interactions are ignored. For named
  choices, use strings the bot understands (e.g. `policy_order_peaceful`: `default`, `rationalism_early`,
  `liberty_first`).
- Throughput: a 330-turn, 4-bot Small game takes about 5 min on one core; with 10 in parallel, expect roughly
  10–15 min per game.
- The GPU is not useful here: the engine and bots are pure Python. LLM seats use the GPU, so an optional LLM-vs-bot
  check game can run in the daytime.

## Method

1. Measure the baseline (`base-prince4`) and the difficulty ladder (`ladder-v0`).
2. Diagnose the biggest weakness from reports plus single diagnostic games. Scratch scripts in the session
   scratchpad print per-civ state every 25 turns.
3. Change `basic.py`, or expose a knob in `DEFAULT_PARAMS` and A/B the values.
4. A/B test: two seats of the new bot against two seats of the previous frozen bot, rotated, for 20–24 games.
   Accept if the score-share difference is significantly positive, or neutral while fixing a measured problem.
5. Repeat. Rerun the ladder and baseline after big changes.

Record every experiment below: name, question, result, decision.

## Engine changes made for this work

- Bot speed: `bv_cache_turns` (default 5) reuses a city's building values until its size, buildings, happiness band,
  war state or threat change. The bot's building simulations were about 23% of run time in large empires.
- GPU: the engine is branchy Python, and nothing in it vectorizes, so the GPU can't speed up simulations. GPU work is
  LLM seats: `python -m citar.bench --opponents 3 --gpu max ...`. LM Studio models run on the user's desktop,
  which has quiet hours from 21:00; `saves/lab/desktop_guard.py` stops runs and unloads models at 20:50.

- **Happiness for conditionals** (`economy.happiness_for_conditionals`): while a civ's happiness is being
  computed, conditions (`while the empire is happy`, `when above [n] [Happiness]`) use the last fully computed
  value, as UnCiv's `getHappiness()` does. Before, every city recomputed the whole empire's happiness
  re-entrantly: 4.1M calls and about 30% of run time in a 250-turn game.
- War trigger fix (bot): see experiment 7.

- Per-seat difficulty (`Player.difficulty`) follows UnCiv's semantics. Humanlike seats (human, LLM, MCP) get their
  difficulty's player values. Bots get its `aiDifficultyLevel` base values plus its AI bonuses: cheaper
  units/buildings, growth, free techs, extra starting units. A separate game-wide `barbarian_difficulty` controls
  the bonus against barbarians, the camp spawn delay, and `turnBarbariansCanEnterPlayerTiles`, which was newly
  implemented.
- Lobby: a per-seat difficulty plus the barbarian difficulty. Benchmark scenarios: bot difficulty and barbarian
  difficulty.
- Speed-ups, about 25% overall:
  - A merged civ-wide unique index (`economy.civ_index`).
  - A per-turn worker-job cache (`Game._jobcache`).
  - A per-unit viewable-tiles cache (`Game._viewcache`, keyed by `_ygen`).
  - Citizens-only cache invalidation (`invalidate_city(citizens_only=True)`).
  - Memoised bot tech valuation.

## Experiment log

| # | Experiment | Question | Result | Decision |
|---|---|---|---|---|
| 0 | `saves/balance/citar-baseline-20260918-094851.json` (balance.py, 6 games) | First full Quick games after the UnCiv refactor | Time 6/6; 57 techs at T330; 55% unhappy turns; about 1,000 gold banked; 11.6 missionaries per player | Motivated this work |
| 1 | `base-prince4` (20 games, v0 = `frozen_4ce67344`) | Baseline with the current bot | 18/20 games: Time 18/18; median civ 19/35/50 techs and 3.3/4.7/7 cities at T100/200/300; unhappy about 53% of turns; science 24 → 88 → 208 per turn at T100/200/300. The whole tree costs about 150k science, and bots make about 21k by T300. | Pace is about 7x too slow for a science win: the problem is the economy, not the endgame |
| 2 | `ladder-v0` | Difficulty ladder with v0 | Dropped (superseded by `ladder-v1`) | |
| 3 | `ab1-settler-wonders` (20 games, full length) | Settler moves-0 bug fix, UnCiv wonder gating, science/food weights (classic production) | No significant differences. Wonder gating slightly lowers score share (score counts wonders). v0 0.272, fix 0.262, wonders 0.224, wonders+sci 0.243 | Keep the bug fixes; leave wonder gating off in classic mode |
| 4 | `ab2-prodmode` (24 games, 200 turns) | Classic vs UnCiv-style production (`prod_mode`), luxury-trade frequency | unciv+lux3 is best: share 0.296 and 8.4 cities at T200 vs classic 0.237 and 5.4. It beats classic+sci significantly | `prod_mode=unciv`, `lux_trade_every=3` |
| 5 | `fac1` (40 games, 200 turns, factorial) | Base `prod_mode=unciv`; screen 8 knobs | Significant: `war_long_turns` 45 hurts (score −0.034, cities −1); `u_settler` 60 hurts (score −0.041, techs −1); `u_science` 2.0 gives +1.3 techs; `settler_min_hap` 2 vs −1 gives +2.3 techs but −1.4 cities (score +0.03, not significant). Neutral: site_new_lux, tech_mode, lux_trade_every, rationalism_early (+0.011) | `u_science=2.0`, rationalism_early; keep the rest |
| 6 | `fac2` (40 games, 200 turns, factorial) | Base `prod_mode=unciv`; screen 8 knobs | Significant: `u_happiness_low` 6 gives +1.1 cities (score +0.032). Not significant but positive: siege_city_pref 3 (+0.029), siege_move_first (+0.026), u_food 3.6 over 2.0 (+0.031), no small-city food focus (+0.022). Neutral or negative: typed CS gifts, worker knobs | `u_happiness_low=6`, `siege_city_pref=3`, `siege_move_first=True` |
| 7 | **v1 defaults** (set in `DEFAULT_PARAMS`) | Above, plus `belief_mode=prefs` and `faith_buildings` (a functional test bought 8 Pagodas), and fixed war triggers: the field-army requirement is capped at max(4, min(8, cities/2+2)), or 2 units at 4x power. Wide empires had never gathered a field army of `cities` units. | The conquest test wins by domination at about T160 | → confirm |
| 8 | `v1-vs-v0` (24 games, full length, 2v2) | Confirm v1 against v0 | **v1 wins 15/24** (v1 7 + v1b 8) against v0's 9. Score share 0.264 and 0.281 against 0.225 and 0.230 (v1b over v0 significant; pooled about +0.045). Techs at T330 62–63 vs 57–58; cities 15–17 vs 8; religions founded 0.5–0.7 vs 0.8; captures 0.12–0.21 | **Accepted: v1 is the new baseline** |
| 8c | `base-v1` (20 games), `ladder-v1` (12/16 so far) | v1 pacing; difficulty ladder (Chieftain/Prince/Emperor/Deity, UnCiv values) | **base-v1:** Time 19/20, Diplomatic 1/20; median civ 20/39/59 techs and 3.8/7.8/12 cities at T100/200/300 (v0: 19/35/50 and 3.3/4.7/7); 65 techs at T330; unhappy on 47–57% of turns. **ladder-v1:** Deity wins 12/12 (Cultural 7, Domination 2, Time 3; share 0.72, 34 cities, 78 techs, 12 captures). Emperor 0.16, Chieftain 0.125, Prince 0.106. Chieftain has more cities than Prince (11.8 vs 6.5) and Prince is unhappy on 39% of turns, others under 8%. Games #5, #9, #11 and #15 took more than 150 minutes and were killed by `--max-minutes` (the logged "exit code 1" is the kill, not a crash) | Deity is far too strong for the v1 bot at every other level; the Chieftain/Prince inversion is confirmed. The runner now uses `--max-minutes 480` |
| 8d | `ladder-v1-mono` (16) | Same with `ai_base_values=monotonic` | **First run invalid:** the runner was started before `ai_base_values` was passed into game specs, so it duplicated `ladder-v1`, identically up to T200. Results moved to `results/ladder-v1-mono-INVALID-noflag.jsonl.bak`; resubmitted 2026-09-19 03:27 | *running* |
| 8b | `ladder-v0` (4 games completed before it was dropped) | Old-bot ladder | Deity 0.416 (1 Cultural win at T322), Emperor 0.327, **Chieftain 0.143 > Prince 0.114**. Chieftain was unhappy 6% of turns, Prince 44% | UnCiv quirk: every non-Prince AI uses Chieftain base values (12 base happiness, ×0.6 unhappiness, +1 per luxury). Only the Prince AI uses Prince's strict values. Added option `ai_base_values="monotonic"` (easier AIs use Prince base values plus their penalties) → `ladder-v1-mono` |
| 10 | `fac4` (40 games, full length, factorial on v1) | Wonders are worth 40 score each (UnCiv `scoreFromWonders`) against 4 per tech and about 5.6 per city on a Small map, so Time victories reward wonder building. Screen `u_wonder_bonus` [4, 12], `u_wonder_gate` [on, off], `u_faith` [1, 2], `bv_cache_turns` [5, 0], `war_prep_rate` [1, 2], `u_settler` [30, 15] | *queued* | |
| 11 | `fac5` (40 games, full length, factorial on v1) | War trace (war_v1.txt): v1 declares about 2 wars per game but captures 0.17 cities. Armies of 3–6 mostly Spearmen and Catapults trickle in *after* the declaration, and wars end in peace after 25–28 turns. Unhappiness (−75% growth) persists on about 50% of turns. Screen `prep_gather` (assemble at a rally point before declaring, then advance at once), `unhappy_avoid_growth`, `settler_min_hap` [2, 5], `war_prep_rate` [1, 2] | *queued* | |
| 9 | `fac3` (40 games, 200 turns, factorial on v1) | Screen `war_prep_rate` [1, 2.5], `settler_min_hap` [2, 0], `u_culture` [1, 2], `u_production` [2, 3], `u_gold` [0.67, 1], `site_new_lux` [0, 8], `tech_cost_exp` [0.8, 0.5], `workers_per_city` [1.8, 2.5] | Only `settler_min_hap` 2 vs 0 is significant: −1.5 cities (−0.026 share, +0.8 techs). All other knobs are within noise: `site_new_lux` 8 +0.033, `u_culture` 2 +0.023, `u_gold` 1.0 −0.031, `war_prep_rate` 2.5 −0.024 | Keep `settler_min_hap` 0; `site_new_lux` and `u_culture` are candidates for v2 (weak positive) |

### Findings from single-game traces (2026-09-18)

Scratch tools: trace.py (per-turn decisions of one civ), settlers.py, prodexplain.py, wartrace.py, deals.py.

- **Settler bug:** a standing goto moves the settler at the start of the turn, so the bot saw `moves == 0`, treated
  the failed move as an unreachable site, blacklisted it for 30 turns and cleared the order. Settlers idled for 5–20
  turns. Fixed: skip units with no moves, and only blacklist when `find_path` fails.
- **Pathfinding** planned turn-ends on tiles occupied by the civ's own units of the same kind, so the unit then
  waited for 5+ turns. Fixed in `movement.find_path`.
- **Settler danger:** it retreated whenever any hostile unit was within 3 tiles and oscillated for 15+ turns near
  barbarians. Now: danger means an enemy within reach; a free military unit escorts the settler on the same tile;
  a site is abandoned after 3 retreats; a waiting settler gets a defender built.
- **Wonders:** the capital spent turns 47–119 on six wonders. With a +0.5 bonus per rule text plus ×1.2 + 2, a
  wonder outscored the Library 52 to 14. Knobs added: `wonder_min_pop`, `wonder_avg_prod` (UnCiv's gating).
- **Military overbuilding (classic mode):** one civ built 12 Trebuchets, 12 Riflemen and 10 Catapults but only 2
  Libraries in 6 cities, with no war captures.
- **Scouting:** contact isn't enough; bots need to know a rival city's location to plan wars. The scouting rules
  now use `_knows_rival_city`.
- **Isolation:** on continents maps a civ can be alone. Nobody has Astronomy by T150, so no contact happens.
- **Happiness is the expansion brake:** base 9 + luxuries 4 each − 3 per city − 1 per citizen. A size-15 capital
  alone costs 15. Few luxury types are nearby, luxury trades happen (24 accepted in 150 turns), and 250-gold
  city-state gifts are frequent.
- The engine matches UnCiv's formulas: tech cost (`TechManager.costOfTech`), population science (`CityStats`,
  1 per citizen), growth (`getFoodToNextPopulation`) and Quick speed modifiers. City healing is 20 per turn, as
  in UnCiv.
- **Sieges** (siege.py): units attack every turn but shoot the defenders around the city rather than the city
  (killing a unit scores 3x). The Trebuchet stayed 3 tiles away with range 2, so the walled city (250 HP) healed
  to full. Knobs: `siege_city_pref`, `siege_move_first`. Peace came after 25 turns even while a siege was
  progressing (fixed: no peace offer while a siege is progressing; `war_long_turns` knob).
- **Economy at T150** (econ.py, unciv mode): capitals are size 16–24, while other cities are size 1–10 with +0–2
  food and 1–14 production. One civ had 3 workers for 3 cities and only 10 of 31 worked tiles improved. Knobs:
  `small_city_focus`, `workers_per_city`, `worker_unimproved`. City-state gifts are now typed (`cs_gift_mode`). UnCiv's own AI researches almost at random among the cheapest techs, so research
  order is a minor lever.

## LLM vs bot checks (GPU)

- `python -m citar.bench --model M --opponents 3 --map-size small --turns N [--gpu max] [--load-context C]` plays one
  LLM seat against 3 BasicBots (v1 defaults at run time). Output: saves/lab/llm_*.out; report:
  saves/benchmark-*.json. The game saves to saves/<id>/.
- 2026-09-18: gemma-4-e4b (desktop GPU, until 20:45) and gemma-4-e4b's little sibling gemma-4-e2b (laptop GPU,
  loaded by the user, may run 24/7) both play seed 4242 (small continents, 4 players). Do not use
  `--load-context`: it unloads all models, including the other machine's.
- Results (seed 4242, Quick, small continents, 3 v1 bots):
  - **gemma-4-e2b (laptop):** 330 turns, 47.6 s per turn, 0.85 errors per turn. The LLM civ was **wiped out**: 0 cities at the end, 32 techs, score 128. The bots scored 2028/902/645 with 69/62/66 techs and 22/11/6 cities; bot 1 won on Time.
  - **gemma-4-e4b (desktop, stopped at the 20:45 limit after 139 turns):** 139 s per turn. 6 cities, the most of any civ (the bots had 5/4/3), but 19 techs against 25–29 and score 166 against 475/326/309.
  - Both small models lose clearly to v1 bots. That suits the goal: bots are a baseline that a weak LLM does not beat.
- **Probe `industrial-trades`** (2026-09-19, scenario `industrial-duel`, 15 cases; the subject is Carthage, P1):

  | Subject | Pass rate (8 cases have an expected answer) | Avg per case | Notable |
  |---|---|---|---|
  | gemma-4-e4b (×1, then ×3) | 0.875 / 0.833 | 50 s | Accepts nearly everything, including 10 gold for 2 Iron (3/3) and a 500-gold bribe to attack a city-state (3/3). Rejects only tribute. Consistent across repeats. |
  | bonsai-27b | 0.75 | 275 s | Accepts the lowball, rejects the war bribe, and counters tribute ("give me 100, return 200"). |
  | qwen3.8-27b | **1.0** | 286 s | Rejects the lowball ("an insult"), tribute and the bribe. Counters the tech price (400 → 200) and the gold-per-turn swap. Answers the threat with open borders and a research agreement. |
  | Scripted bot (×3) | 0.875 | <1 s | Counters the lowball, rejects tribute, **accepts the war bribe (3/3)**. |

  - Both 27B models hit the 30-minute turn limit in the one-turn-at-war case.
  - **Bot weaknesses found and fixed in basic.py** (not yet in a frozen version; they ship with v2):
    - A war declaration in a deal used to be a flat 200. `_war_item_value` now prices it by the target's relative strength, +300 for a city-state, +400 for betraying a friend or pact partner, and a discount when the bot already fights or plans to fight the target, all scaled by aggression. Result on the war bribe: aggression 0.2 counters for 159 more gold, 0.4 for 93 more, 0.8 accepts.
    - A spare strategic resource was worth 12 gold at any era; it's now 12 × (era + 1). The Iron lowball goes from "add 24 gold" to "add 120 gold".
- Future LLM runs: use the Benchmarks page (server scheduler) so the user can watch them. CLI runs must write to saves/lab/*.out, which the Lab page shows as side runs.

## Next steps (keep current)

Progress of everything queued is on the web GUI **Lab page** (http://localhost:8765/#/lab): runner health, per-experiment progress and ETA, each game's current turn, side runs and the runner log. Click an experiment to see its report.

1. When `fac4`, `fac5` and `ladder-v1-mono` finish, read them and fold significant knobs into `DEFAULT_PARAMS` as v2. Then confirm v2 with a full-length 2v2 against v1 (`frozen_7149efb1`).
2. Ladder: Deity dominates (0.72 share), and the Chieftain AI beats the Prince AI. Present the unciv vs monotonic choice to the user once `ladder-v1-mono` is in.
3. Investigate why ladder games #5, #9, #11 and #15 run more than 150 minutes (a turn-time trace with periodic stack dumps is in scratchpad `l5.*`). Likely late-game unit counts under Deity; possibly an engine hot spot to optimise.
4. Next areas: happiness (Prince unhappy 39–57% of turns), war capture rate (`prep_gather` in fac5), endgame projects, gold spending.
5. Optional (needs the user's OK): build UnCiv offline and run its AI simulation to calibrate the pace.


---

*This page is generated from [`docs/research/BOT_TUNING.md`](https://github.com/jprodgers/CITAR/blob/main/docs/research/BOT_TUNING.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
