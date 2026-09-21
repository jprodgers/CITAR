# Maps, scenarios and probes

A full game answers "which model plays better". These three answer "what does this model do when
*this* happens", which is usually the more useful question and takes minutes rather than days.

---

## Map sizes

| Size | Tiles | Civilizations | City-states |
|---|---|---|---|
| Duel | 40×24 | 2 | 4 |
| Tiny | 56×36 | 4 | 8 |
| Small | 66×42 | 6 | 12 |
| Standard | 80×52 | 8 | 16 |
| Large | 104×64 | 10 | 20 |
| Huge | 112×70 | 12 | 24 |
| Gargantuan | 160×100 | 24 | 32 |

A game holds up to 24 civilizations. Gargantuan exists for watching many models interact at once;
it is slow, and one turn of 24 AI players is a long time.

---

## The map editor

**Map editor** in the top navigation. Generate a map or start blank, then paint terrain, features,
resources, natural wonders, rivers (click tile edges), improvements, roads and start positions.

Left-drag paints, right-drag pans, the wheel zooms, `Ctrl+Z` undoes. **Check** lists problems —
a start position on ice, a civilization with no fresh water — and saving fixes what it can.

Maps are plain JSON in `saves/maps/<id>.json`; the format is documented at the top of
`citar/engine/maps.py`. Choose a saved map under *Map* in the lobby. Seats without a start position
get one assigned.

---

## Scenarios

A scenario is a prepared game you can replay from the same state as often as you like. Start from a
saved map, a generated map, a running game, or a save.

The editor has four tabs:

- **Place** — found cities, add units, remove things, claim tiles, edit a city's population and
  buildings.
- **Players** — advance a civilization to an era, grant techs and policies, set gold, faith and
  culture, reveal the map.
- **Diplomacy** — set met, war, embassies, friendship, defensive pacts, open borders, city-state
  influence.
- **Operations** — apply any change as JSON, for the things the UI does not cover.

Each seat has a default type: human, LLM, bot, MCP, or **probe script** — the scripted counterparty
used in probes below. Scenarios are saved to `saves/scenarios/<id>.citarscn`.

---

## Probes

A probe is a queue of test cases run against one AI seat of a scenario. Every case starts from the
scenario's saved state, applies its own setup, lets the model respond once, records what it did,
and resets.

This is the closest thing CITAR has to a unit test for a model.

### The three kinds of case

| Kind | What happens |
|---|---|
| `offer` | The counterparty proposes a deal — gold, gold per turn, resources, techs, cities, agreements, a declaration of war |
| `message` | Talk, with no deal attached |
| `turn` | The model plays one whole turn |

A case can declare what it expects: `accept`, `reject`, `counter`, `reply`, or tools the model must
or must not use. Runs report pass rates per case and per model.

Each case's end state is saved, and **Open game** loads it — so when a model does something
strange, you can go and look at the position it did it in.

Probe files are JSON in `saves/probes/<id>.json`; the format is at the top of `citar/probes.py`.

### Checking a probe without a GPU

Use the **Dry run** model preset. It exercises the whole path — scenario load, case setup, seat
wiring, result recording — without a model, which is how you find a broken case in seconds rather
than after an hour of GPU time.

### An example

The repository ships the scenario `industrial-duel` with the probe `industrial-trades`: fifteen
trade and war cases in an industrial-era standoff. It is a reasonable template for your own.

---

## Which to use

| Question | Use |
|---|---|
| Which model plays better? | A [benchmark](Benchmarks) |
| Does this model understand that it is losing a war? | A probe with `turn` cases from a losing position |
| Will it accept a bad deal? | A probe with `offer` cases |
| Does a prompt change help? | Probes — same position, same cases, before and after |
| How does it open? | A benchmark with a low turn limit |

Probes are repeatable and cheap; benchmarks are expensive and closer to the real question. Use
probes to iterate and benchmarks to conclude.


---

*This page is generated from [`docs/SCENARIOS.md`](https://github.com/jprodgers/CITAR/blob/main/docs/SCENARIOS.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
