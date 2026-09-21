# CITAR (Civ Inspired Tool for AI Research) — Design Document

A top-down hex *Civilization V*-style game in which the opponents, or all of the players, are AI models. They connect
to the game, take turns, negotiate, bluff and wage war as a human would. The rules, numbers and underlying logic
come from **UnCiv**'s "Civ V – Gods & Kings" ruleset (see §2). CITAR adds the AI interface, benchmarking, a text
harness and a browser client.

---

## 1. Goals

- Civ V (Gods & Kings) rules at UnCiv's numbers: all nine eras, 80 techs, 127 units, 70 buildings, 40 wonders, 14
  national wonders, 10 policy branches, religion, city-states, great people, golden ages, espionage, the UN and
  diplomatic victory, and nukes.
- Fog of war enforced by the **server**: no player, human or AI, receives information their units, cities or spies
  can't see.
- Any mix of seats: human, AI via MCP, AI via the built-in LLM adapter, or a scripted bot. Full AI-vs-AI spectating.
- A deterministic engine: saves, benchmarks and replays are reproducible from a seed.
- **Benchmarking:** models play full games against scripted bots on fixed seeds. `BenchmarkCiv`, a civilization with
  no unique abilities, units or buildings, keeps every seat equal.

**Not imported:** graphics, sounds, civilopedia and flavour text, leader dialogue and tutorials. CITAR's renderer
draws flat vector terrain and unit glyphs.

---

## 2. Rule data and the UnCiv import

- `scripts/import_unciv.py` reads the UnCiv G&K JSON (`android/assets/jsons/Civ V - Gods & Kings`), keeps the
  numbers and the rule text ("uniques"), drops flavour text, and writes `citar/data/ruleset/*.json`. Rerun it to
  pick up a newer UnCiv version. `scripts/gen_unique_types.py` generates `engine/unique_types.py` from UnCiv's
  `UniqueType.kt`. `scripts/check_uniques.py` lists ruleset uniques whose text matches no UnCiv unique type
  (usually a typo), and `scripts/check_refs.py` finds calls to missing functions across engine modules.
- `citar/data/custom/` holds CITAR additions (currently `BenchmarkCiv`). `citar/data/game.json` holds CITAR's own
  constants: map sizes, map types, lobby defaults, barbarian levels, and limits for the AI interface.
- **Licence:** UnCiv is MPL-2.0. The derived data and the engine code ported from UnCiv's Kotlin carry that licence.
  Attribution is in `citar/data/ruleset/NOTICE.md` and the README.
- **Uniques:** as in UnCiv, most behaviour is written as rule text on techs, buildings, policies, beliefs, promotions,
  nations and eras, for example `[+15]% Strength <when attacking>`. `engine/uniques.py` parses the placeholders,
  parameters and conditionals, collects uniques from every source that applies (`UniqueMap`), and evaluates
  conditionals against a context (`Ctx`). Triggered uniques (`engine/triggers.py`) run on events such as adopting a
  policy, founding a city or discovering a tech.
- Every ruleset object is keyed by its UnCiv display name ("Bronze Working", "Great Library"). `Rules.resolve`
  also accepts lower-case or snake_case ids, so AI players can write either form.

---

## 3. Architecture

```
            ┌───────────────────────────────────────────────────┐
            │                 Game Server (Python)              │
            │  ┌──────────────┐   ┌──────────────────────────┐  │
            │  │    Engine     │   │ Tool registry (tools.py) │  │
            │  │ (pure rules,  │◄──┤ one schema per action →  │  │
            │  │ seeded RNG,   │   │ REST, web UI, MCP, LLM   │  │
            │  │ no I/O)       │   └──────────────────────────┘  │
            │  └──────┬───────┘                                  │
            │   per-seat fog-filtered views     event log        │
            │  ┌──────┴──────────────────────────────────────┐   │
            │  │   HTTP JSON API  +  WebSocket push           │   │
            └──┴──────┬──────────────┬──────────────┬─────────┴───┘
                      │              │              │
          Browser client     MCP bridge (stdio)   LLM adapter (in-process)
          (human / spectator)
```

**Engine modules** (`citar/engine/`, pure standard library):

| Module | Covers |
|---|---|
| `rules`, `uniques`, `unique_types`, `triggers` | ruleset loading, the unique language, triggered effects |
| `state`, `game`, `hexmap` | serializable state, the `Game` facade (caches, events, RNG), hex math |
| `mapgen` | UnCiv-style map generation: landmass, elevation, climate, rivers along tile edges, natural wonders, resources, fair starts, city-state placement, ruins |
| `tiles`, `movement`, `visibility` | yields, movement (UnCiv costs, ZOC, embarkation, roads/railroads), line of sight with elevation |
| `cities`, `economy` | stats, growth, borders, citizens and specialists, production and purchases, happiness, maintenance, supply, resources |
| `research`, `policies`, `religion`, `great_people` | techs, social policies, pantheons and religions, great people and golden ages |
| `combat`, `units`, `conquest` | damage formula and modifiers, promotions, upgrades, air and nuclear combat, city capture |
| `workers`, `actions`, `automation` | improvements and pillaging, unit special actions, exploration and worker automation |
| `diplomacy`, `city_states`, `espionage`, `victory` | relations, deals, city-state influence and quests, spies, the UN, victories and score |
| `barbarians`, `ruins`, `turns` | camps and raiders, ancient ruins, turn order |
| `views`, `briefing`, `tools` | fog-filtered JSON views, text briefings and ASCII maps, the tool registry |

**Turn order** follows UnCiv's `TurnManager`: start-of-turn (resources, city processing, golden ages, great
people, religion), player actions, end-of-turn (production, growth, research, culture, faith, gold, unit healing
and upkeep), then an end-of-round pass (barbarians, city-state elections, UN votes, victory checks).

---

## 4. Game setup (lobby)

| Setting | Options (default **bold**) |
|---|---|
| Map size | Duel 44×28 (2p, 4 CS) · **Small 60×38 (4p, 8 CS)** · Standard 76×48 (6p, 12 CS) · Large 92×58 (8p, 16 CS) |
| Map type | **Continents** · Pangaea · Archipelago · Inland Sea · Fractal |
| Speed | Quick (330 turns) · **Standard (500)** · Epic (750) · Marathon (1500) |
| Difficulty | Settler … **Prince** … Deity (UnCiv difficulty table: happiness, costs, AI bonuses, barbarian bonuses) |
| Civilizations | each seat picks one of 35 civilizations, BenchmarkCiv, or random. Civ and city names are still free to edit. |
| City-states | count (default by map size), from 40 city-states |
| Victories | Scientific · Cultural · Domination · Diplomatic · Time (all **on**) |
| Options | religion · espionage · ancient ruins · tech trading (all **on**); barbarians off / **normal** / raging; nukes always on |
| Turn limit | the speed's time-victory turn by default |

**Start:** as in UnCiv, a Settler, a Warrior and (from the era table) extra starting units at a fair start location
chosen from the nation's start bias.

**Benchmarks** default to **Quick speed, 330 turns, BenchmarkCiv in every seat**.

---

## 5. The AI interface

### Seats
| Seat type | How it plays |
|---|---|
| **Human** | Browser client with that seat's token. Several humans can join over the LAN. |
| **MCP** | `citar_mcp.py` bridges an MCP client (Claude Code, Claude Desktop, …) to the seat; `wait_for_turn` blocks until it's the seat's turn or a negotiation needs a reply. |
| **LLM adapter** | The server drives the model: `anthropic` or any OpenAI-compatible endpoint (LM Studio, Ollama, llama.cpp, vLLM). API keys come from environment variables and are never saved. |
| **Scripted bot** | `citar/bots/basic.py`: needs-based research, production and policies; religion, great people, spies, city-state gifts, trading, war preparation and sieges. |

### One tool set
Each action is declared once with `@tool(...)` in `engine/tools.py`. The REST API, browser UI, MCP bridge and LLM
adapter all derive from that registry. Invalid actions return a clear error ("Swordsman requires Iron") so models
can adapt.

- **Information:** `get_briefing`, `get_map`, `get_tile`, `get_unit(s)`, `get_city`/`get_cities`, `get_empire`,
  `get_players`, `get_diplomacy`, `get_city_states`, `get_tech_tree`, `get_policies`, `get_religion`,
  `get_great_people`, `get_espionage`, `get_victory_status`, `get_rules`, `get_events`, `preview_attack`,
  `read_notes`.
- **Units:** `move_unit`, `attack`, `air_sweep`, `unit_order` (fortify, sleep, explore, automate, disband, …),
  `unit_action` (found a city or religion, spread religion, great-person actions, great improvements, spaceship
  parts, …), `build_improvement`, `found_city`, `upgrade_unit`, `promote_unit`.
- **Cities:** `set_production`, `change_queue`, `set_auto_production`, `buy` (gold or faith), `set_city_focus`,
  `work_tile`, `set_specialists`, `buy_tile`, `city_attack`, `rename_city`, `city_status` (annex, puppet, raze).
- **Empire:** `set_research`, `choose_free_tech`, `adopt_policy`, `found_pantheon`, `choose_great_person`,
  `set_civ_name`.
- **Diplomacy:** `send_message`, `open_negotiation`, `respond_negotiation`, `declare_war`, `denounce`,
  `city_state_action` (gift gold or units, pledge protection, demand tribute), `un_vote`, `move_spy`,
  `stage_coup`.
- **Turn:** `write_notes`, `log_thought`, `end_turn`.

**Briefing:** the text briefing opens with ALERTS (falling gold, unhappiness, threatened cities with the exact
`city_attack` call, civilians in danger, starving cities, free techs, policies or great people to pick, and so on).
It then shows empire stats, cities, units needing orders with suggested actions, available techs, a local ASCII map
using UnCiv terrain names, events, and diplomacy. After each batch of actions the model gets a TURN PROGRESS note.

### Negotiation
Messages are free text and never binding. Deals are binding and enforced by the engine. They can include gold,
gold per turn, resources, open borders, embassies, peace, declarations of friendship, research agreements,
defensive pacts, declaring war on a third party, cities, maps and technologies. The other side is interrupted
out of turn to accept, reject, counter or reply. The exchange repeats until one side accepts or walks away, or
the round cap is reached.

---

## 6. Saves, replays and recap

- Games autosave every turn to `saves/<game id>/autosave.citar` (gzipped JSON with the full state, event log,
  messages, AI thoughts and notebooks). Named saves come from the game screen.
- Per-turn frames record territory, cities, units and stats for the **recap viewer**: timeline scrubbing, "as player
  X saw it" fog, graphs, events, diplomacy transcripts and AI reasoning.
- Saves from CIGAR (the pre-UnCiv ruleset) are incompatible. They were moved to `saves/_cigar_archive/` along with
  the old rule files.

---

## 7. Decisions log

- **CIGAR v0.1 (2026-09-16/17):** the original simplified ruleset (56 techs, 6 eras, CITAR's own numbers), the
  benchmark suite and scheduler, model scoring, the AI harness (alerts, turn progress, guard rails for weak
  models), and the human-play improvements (queue editing, route previews, tile locking, and more). The balance
  passes for that ruleset no longer apply.
- **CITAR refactor (2026-09-18):** renamed from CIGAR to CITAR, "Civ Inspired Tool for AI Research".
  - All rule data and logic come from UnCiv's G&K ruleset at pure UnCiv numbers: no flavour text and no graphics.
    Every missing subsystem was added: religion, social policies, city-states with the diplomatic victory and the
    UN, great people and golden ages, espionage, difficulty levels, ancient ruins, natural wonders and nukes.
  - Every speed is available. Standard is the default for normal games, and benchmarks use Quick (330 turns), even
    though that roughly doubles test time compared with the old 200-turn games.
  - Seats pick a civilization, or get a random one, and keep free naming. BenchmarkCiv, with no abilities, fills
    every benchmark seat by default.
  - Turn numbering: CITAR's turn 1 is UnCiv's turn 0, both for the year and for the time victory, which fires
    after the speed's last turn.
  - The legacy v0.1 bot was removed, since it targeted the old rules. BasicBot was rewritten for the new systems.
    Its building choices simulate the city's stats with the building added, following UnCiv's
    `getStatDifferenceFromBuilding`.
- **Bot balance (2026-09-18), first pass from bot-vs-bot Quick games:** the bots were limited by happiness, not
  production. BasicBot now values techs that unlock improvements for the luxuries it owns and improves new luxury
  types first, including those beyond the 3-tile work radius and those at sea. It weighs happiness buildings
  against the UnCiv expansion rule (happiness above the city count) and spends surplus gold on buildings and
  city-states.
- **Servers and Reports (2026-09-19):**
  - A server registry (`citar/servers.py`, `config/servers.json`) replaces every inline provider/URL/key setting.
    Seats, suites, probe runs and report narratives store a reference (`server_id`, `model_id`, `profile_id` plus
    overrides) that `servers.resolve_llm` turns into an agent config when the AI starts, so key or endpoint changes
    apply without editing games. Pre-registry saves, benchmark runs, suites and probe runs were moved to
    `saves/_archive_2026-09-19_pre-servers/`; lab results were kept for the bot-tuning campaign.
  - API keys: OS credential store via `keyring`, an environment variable, or a passphrase-encrypted file outside
    the project (`citar/keystore.py`). The browser can store a key but never read it back.
  - Hardware: `citar/hwinfo.py` is a standalone, stdlib-only collector for Windows, Linux and macOS (also served as
    `collect_hardware.py`); `scripts/collect_hardware.ps1` covers Windows machines without Python. Other machines are
    imported from the collector's JSON rather than contacted over the network.
  - Restricted hours are per server (replacing the global quiet hours): benchmark games finish the model turn in
    progress (up to the grace minutes) before pausing; probes and the lab wait; lobby games only warn.
  - Usage and cost are separate: `citar/usage.py` records what each activity used (server time held, model
    generation time, tokens, host CPU time, one-minute power samples) and `citar/costing.py` prices it at report time
    with effective-dated settings. Fixed costs are charged on a calendar basis (unused time stays unallocated),
    idle power is shared by time held and power above idle by work done, and tokens at the model's prices.
  - Reports (`citar/reports/`) are built on demand into self-contained HTML with inline SVG charts, deterministic
    findings (confidence intervals, Welch tests, Pareto frontier, what-if pricing, lifespan sensitivity), and an
    optional narrative written by a model picked from the registry.
