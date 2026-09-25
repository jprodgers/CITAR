# Rule scripts

A rule script pins down one behaviour of the game engine: it sets a game up, acts, and checks
what the engine then says. The same script runs on both engines:

- the Rust engine, through `crates/citar-testkit/src/script` (`cargo nextest run -p citar-testkit
  --test rules`, one test per script);
- the Python engine, through `tests/rulescript.py` (`python -m unittest tests.test_rule_scripts`),
  which drives `citar.engine_api.EngineGame` only.

Scripts replace the Python tests that poked the engine's internals (DESIGN.md 9.3 in
`crates/citar-engine/`). They play on hand-made maps with named places instead of generated ones,
so a script means the same thing on both engines and before and after a system is ported. A
script lands with the package that ports what it tests, and passes on Python first.

`_selftest.toml` tests the runners themselves, including checks that must fail. Keep the two
runners in step: a change to the language changes both, this file, and the self-test.

## A script

```toml
about = "Seats get their handicap and automatic decisions from their controller."
from = "tests/test_controllers.py::DefaultTests::test_defaults_follow_the_controller"
map = "arena"          # optional: tests/rules/maps/<map>.json; the arena by default
start = "bare"         # optional: "bare" (the default) or "full"

[config]               # optional: settings over the runner's defaults
players = [{ controller = "human" }, { controller = "bot" }]

[[step]]
check = { what = "player", player = 0 }
path = "handicap"
eq = "human"
```

| Key | |
|---|---|
| `about` | required: what the script pins down, in a sentence or two |
| `from` | the Python test it ports, if any |
| `map` | the map, `arena` by default |
| `start` | `bare` or `full` |
| `config` | settings, as `EngineGame.new` takes them |
| `step` | the steps, in order |

Any other key is refused. The file name is the test's name.

## The game

The runner builds the settings from, in order:

1. its defaults: `seed = 1`, two players (`players = [{}, {}]`);
2. with `start = "bare"`: `city_states = 0`, `barbarians = "off"`, `ruins = false`;
3. the script's `config`, key by key (a key replaces the default whole);
4. a `nation = "BenchmarkCiv"` for every player that names none: the benchmark civilization has
   no unique ability, so no nation's bonus leaks into a script. Such players are called
   "Civilization 1", "Civilization 2", ... by seat;
5. the map document, inline.

Then, with `start = "bare"`, every unit is removed (`clear_units` for `all`): a bare game has the
map, the players, their starting techs, gold and culture, and nothing else.

Player ids are the seats in order, then the city-states, then the barbarians. The engines draw
city-states' nations differently, so scripts refer to a city-state by id and never by name.

`start = "full"` keeps the starting units and camps. The Rust runner refuses it until package
1c-09 ports the rest of setup. Both runners set games up through the engine's own setup
(`EngineGame.new`, and `Game::config_from_json` with `Game::new` in Rust), which begins the first
seat's turn and announces it (`turn_start`, then `game_start`) before the bare prelude runs: count
events by type, and never pin an event id at the start of a game.

A civilization, or a city-state, with no unit and no city is eliminated at the end of a round
(once package 1c-08 ports eliminations in Rust; Python always did). A script that plays across a
round gives each player a unit first, with the `add_unit` operation.

## Steps

Each step is a `[[step]]` table with exactly one of `op`, `ops`, `tool`, `check`, `new_game`, `set`
or `repeat`, its kind's own keys, and any of these that its kind takes (the runners refuse any
other key, so a misplaced one never passes unread):

| Key | |
|---|---|
| `note` | a comment the runners ignore |
| `as = "name"` | `op`, `ops`, `tool` and `check` only: binds the step's result (an op's or tool's return value, a check's subject at its path) |
| `error = "text"` | `op`, `ops`, `tool` and `new_game` only: the step must be refused with `text` in its message; `error = true`: refused with any message |
| `must_fail = true` or `"text"` | the step itself must fail (for the self-test): a check that does not hold, an unexpected error, a refused script |
| `intended = "id"` | the expected value is the Rust engine's, which differs from Python's on purpose: the Python runner skips the step. The id is listed in `refcheck/intended.toml` or `tests/rules/intended.toml` |
| `coerce = true` | the step types numbers as strings on purpose (see [Numbers](#numbers)) |

### `op`

```toml
[[step]]
op = "grant_tech"
args = { player = 0, tech = "Pottery" }
as = "granted"
```

A scenario operation (`EngineGame.apply_ops`, `Game::apply_ops`) or a test operation
(`EngineGame.test_ops`, `api::testops`), by name; `args` are its parameters. The result is the
operation's return value. An error names the operation: `Operation 1 (grant_tech): Unknown tech
'Foo'.`, `Test operation 1 (unmeet): ...`.

`inspect` with `{ what = "ops" }` lists both kinds with their parameters, and
`{ what = "pending" }` those not ported to the Rust engine yet.

### `ops`

```toml
[[step]]
ops = [
  { op = "set_player", player = 0, gold = 40 },
  { op = "grant_tech", player = 1, tech = "Pottery" },
]
```

Scenario operations applied as one list, each table an operation with its parameters (and `at`)
inline. The Rust engine applies a list all or nothing; Python left the operations before a failed
one applied. The result is the list of what each returned.

### `tool`

```toml
[[step]]
tool = "set_research"
player = 0             # optional: whose turn it is, by default
args = { tech = "Writing" }
```

A player's tool call, as a host makes it (`EngineGame.execute`; in Rust the arguments go through
`api::tools::normalize` and the typed action through `Game::act`). The result is the tool's.

### `check`

```toml
[[step]]
check = { what = "relation", a = 0, b = 1 }
path = "embassy[0]"
eq = true
```

The subject is an `inspect` query ([below](#inspect)), or a bound variable by name
(`check = "granted"`). `path` picks a place in it ([Paths](#paths)); the matchers say what must
hold there. Every matcher given must hold:

| Matcher | Holds when |
|---|---|
| `eq = v`, `ne = v` | the value equals `v`, or does not |
| `gt`, `ge`, `lt`, `le` | the number compares so |
| `approx = n` | the number is within `tol` (default `1e-6`) times the larger of 1 and the two sizes |
| `contains = v`, `not_contains = v` | a list has an item equal to `v`, a string has `v` in it, an object has the key `v`; or not |
| `len = n` | the list, string (in characters) or object has `n` items |
| `absent = true` | the path leads nowhere (no other matcher then); `absent = false`: it leads somewhere |
| `is_null = true` | the value is null (`false`: it is not) |
| `matches = "re"` | the string matches the regular expression somewhere (keep to the syntax Rust's `regex` and Python's `re` share) |
| `any = { ... }` | some item of the list passes the matchers in the table, which may have its own `path` into the item |
| `none = { ... }` | no item does |
| `subset = x` | `x` is part of the value: each key of an object with an equal value, each item of a list |

Values compare as JSON: numbers by value (`3` equals `3.0`, and a boolean is never a number),
objects whatever their key order, lists in order.

### `new_game`

```toml
[[step]]
new_game = { players = [{ controller = "bot", handicap = "deity" }] }
error = "handicap must be 'human' or 'ai'"
```

A new game from the script's settings with these keys replaced. Without `error` it replaces the
current game; with it, the settings must be refused (`ValueError` in Python).

### `set`

```toml
[[step]]
set = { start_gold = "=gold + 10", name = "Rome" }
```

Binds variables.

### `repeat`

```toml
[[step]]
repeat = 3
steps = [{ op = "set_turn", args = { turn = 5 } }]
```

Runs the steps, in order, that many times.

## Values

A string that starts with `=` is an expression, evaluated when the step runs; `==` at the start
is a literal `=`. Expressions have:

- numbers, `'text'` in single quotes, `null`, `true` and `false` (TOML has no null: write
  `"=null"`);
- variables by name, with a path after them: `granted.techs_added.0`, `ids[0]`;
- `+ - * / // %` and parentheses: whole numbers stay whole except under `/`, and `//` and `%`
  round toward minus infinity, as Python's do; `+` also joins strings;
- `x(ref)` and `y(ref)`, a tile reference's coordinates, and `len(v)`.

### Tiles

In `args` and in `check` queries, `at` is replaced by `x` and `y`. It is a tile reference:

- an anchor of the map: `"A"`;
- coordinates: `"(3,4)"`;
- a walk: `"A>e>ne"`, a step per direction (`e`, `ne`, `nw`, `w`, `sw`, `se`) in odd-r offset
  coordinates, where odd rows are shifted right;
- a selector: a table, which is a `find_tiles` query: `at = { at = "A", radius = 2,
  terrain = "Plains" }` is the first tile it finds, and `pick = 1` the second.

### Paths

| Segment | Selects |
|---|---|
| `key` first, or `.key` | the object key `key` (letters, digits, `_` and `-`): `techs_added.0` |
| `["any key"]` | any object key, as a JSON string |
| `[3]` | item 3 of a list |
| `[id=4]` | the first item of a list that is an object whose `id` is 4 |
| `[#0=12]` | the first item of a list that is a list whose item 0 is 12 |

A selector value is a JSON literal, or a bare word for a string: `[type=Warrior]`. This is the
path grammar of `refcheck/intended.toml` without its wildcards.

### Numbers

Tools and operations coerce `"3"` to 3, as Python's did. So that this is tested on purpose and
never by accident, the runners refuse a step whose `args`, `ops`, `check` query or `new_game`
types a number as a string, unless the step says `coerce = true`.

`normalize.json` is the table of how a tool's arguments are coerced (`tools.py:113-127`): both
engines run every case, Python through `tools.execute` in `tests/test_rule_scripts.py`, Rust
through `api::tools::normalize_with` in `crates/citar-testkit/tests/engine/tools.rs`. A case with
an `"intended"` id is a deliberate difference, which only Rust runs, as with an `intended` step.

## Inspect

`{ what = ..., ... }` asks the engine (`EngineGame.inspect`, `api::inspect`). Every shape is the
same from both engines, and every set in it is sorted.

| `what` | Takes | Gives |
|---|---|---|
| `game` | | `turn`, `current`, `phase` (`playing`, `over`), `winner`, `victory`, `width`, `height`, `players` (how many), `majors`, `city_states` (ids), `barbarians` (id or null) |
| `player` | `player` | `id`, `kind` (`major`, `city_state`, `barbarian`), `name`, `leader`, `nation`, `alive`, `controller`, `handicap`, `auto` (`un_vote`, `conquest`, `free_picks`), `overrides` (the `handicap` and `auto` keys the seat set explicitly), `difficulty` (the seat's, or a major's the game's), `gold`, `culture`, `faith`, `golden_age_turns`, `free_policies`, `free_techs`, `future_techs`, `techs` (names), `research` (`queue`, `goal`, `progress` (science stored by tech), `overflow`), `policies`, `met` (ids), `capital`, `cities`, `units` (ids), `explored` (how many tiles), `natural_wonders` (the names it has discovered), `notes`, `city_state` (null, or `type`, `ally` and `influence` by major id), `happiness` (now), `happiness_seen` (what its cities and conditionals go by: committed at setup and at the start and end of its turns; Python's is the live figure), `gold_rate` (written at the end of each of its turns; Python never wrote it) |
| `tile` | `x`, `y` (or `at`) | `x`, `y`, `terrain`, `features`, `wonder`, `resource`, `resource_amount`, `improvement`, `pillaged`, `route` (`Road`, `Railroad` or null), `route_pillaged`, `river` (the edge mask), `owner`, `city`, `units` (ids), `visible` (the ids of the players who see it now) |
| `relation` | `a`, `b` | `a`, `b`, `met`, `war`, `war_declared_by`, `since`, `treaty_until`, `friendship_until`, `pact_until`, `ra_until`, `embassy` (`[a's with b, b's with a]`), `open_borders_until` (`[a lets b in, b lets a in]`), `opinion` (`[a's of b, b's of a]`), `friends`, `pact` |
| `unit` | `unit` | `id`, `owner`, `type`, `x`, `y`, `hp`, `xp`, `promotions` |
| `units` | optionally `player`, `x`, `y` | the units, by id, as `unit` gives them |
| `city` | `city` | `id`, `name`, `owner`, `x`, `y`, `pop`, `buildings`, `worked`, `locked` and `workable` (tiles as `[x, y]`), `specialists` (count by name, those it has), `focus`, `avoid_growth`, `food` (stored), `yields` (the city's total of each stat: `food` is the surplus), `queue` (item names, the first being built), `progress` (production stored by item), `overflow`, `culture` (stored toward the next tile), `health`, `tiles` (how many tiles it owns) |
| `buildable` | `city` | what the city can build now: `units`, `buildings`, `wonders` and `other` (`Gold`, `Science`) as sorted names, and `production`, the production cost of each unit, building and wonder by name |
| `costs` | `player` | `tech` (what each tech it could research now costs it, by name), `policy` (its next policy's culture), `adoptable` (the policies and branches a major could adopt now, sorted) |
| `religion` | `player` | `state` (`none`, `pantheon`, `religion`, `enhanced`), `religion` (its pantheon's belief or its religion's name, or null), `display` (the name it is shown under), `beliefs` (sorted), `free_beliefs` (count by kind, those it has), `pantheon_cost` and `prophet_cost` (the faith its next pantheon and great prophet cost), `prophets_earned`, `holy_city` (id or null) |
| `religion` | `city` | `majority` (a religion's name, or null), `followers` (count by religion), `pressures` (by religion, `None` for no religion), `holy_city_of` (a religion's name, or null) |
| `great_people` | `player` | `points` (great person points by great person), `free` (free great people to choose), `earned`, `golden_age_points`, `golden_ages`, `golden_age_turns`, `golden_age_needed` (the happiness the next golden age needs), `temp_uniques` (each unique held for some turns: `text`, the timed unique's, and `turns` left) |
| `events` | optionally `since` (an event id), `type`, `player` (only what that player hears of) | `id`, `turn`, `type`, `text`, `audience` (ids, or null for everyone) |
| `find_tiles` | filters | `x`, `y`, `distance`, nearest first, then by row and column |
| `ops` | | `scenario` and `test`: each operation with its `params` |
| `pending` | | what the Rust engine has not ported yet: `kind` (`inspect`, `scenario_op`, `test_op`, `turn_stage`, `setup_stage`), `name`, `package` (Python: nothing). A turn stage is named by its table and stage, `player_start S2: research progress` |
| `negotiation`, `view`, `briefing` | | not yet: they come with packages 1c-05, 1d-02 and 1d-03, and the Rust engine refuses them as not ported until then |

`find_tiles` filters: `x` and `y` (or `at`), the place distances are counted from; `radius`, the
farthest a tile may be; `terrain`, `feature`, `resource`, `improvement`, names the tile must have;
`owner`, a player id or `"none"`; `land`, `river`, `city`, `units`, `bare` (no feature, resource,
improvement or natural wonder), each true or false; `limit`, the most tiles to give.

## Test operations

What a script does that no player or editor may. `{ what = "ops" }` lists them.

| Op | Takes | Does |
|---|---|---|
| `clear_units` | `player`: an id, a list, or `all` (every player, the barbarians included) | removes every unit of those players |
| `set_turn` | `turn` | sets the turn number |
| `unmeet` | `a`, `b` | the two no longer know each other |
| `set_controller` | `player`, `controller`; optionally `handicap`, `auto` | hands the seat to another driver, as a host does |
| `set_auto` | `player`, `decision`, `on` | the engine takes one decision for the civilization, or not, until its controller changes |
| `refresh_visibility` | | brings what everyone sees up to date |
| `reload` | | saves the game and loads the save |
| `end_turn` | optionally `player` (the current one by default) | ends that player's turn, as the host's `end_turn` does: city-states and the barbarians play inside the call, a round ends after the last player, and play stops at the next major civilization, whose turn begins. Gives `turn` and `current` |
| `end_round` | | ends every turn left in the round, and the round. Gives `turn` and `current` |
| `force_turn` | `player` | makes it that player's turn now and begins it (nothing if it already is). Gives `turn` and `current` |
| `complete_construction` | `city` | finishes what the city builds now, as its turn would, whatever is stored: a building joins the city, a unit is placed in it or beside it. Refused when the city builds nothing that completes, and when no tile has room for the unit. Gives `completed` (the name) |
| `found_religion` | `unit`, `name`, `beliefs` (names) | the great prophet founds a religion in the city it stands in and is spent, as its unit action does (`religion.found_religion`). Gives `founded` (the name shown), `religion` (the ruleset's name), `beliefs`, `holy_city` |
| `enhance_religion` | `unit`, `beliefs` (names) | the great prophet enhances its owner's religion from the city it stands in and is spent (`religion.enhance_religion`). Gives `enhanced`, `beliefs` |
| `enter_ruins` | `unit` | the unit explores the ancient ruins it stands on, as moving onto them does (`ruins.enter`). Refused where there are none. Gives `found` (whether a reward was) |

The rest land with their systems: `set_unit` and `ready_unit` (1c-02), `capture_civilian` and
`attack_as` (1c-03), `automate` and `progress_builds` (1c-04), `add_spy`, `close_negotiation` and
`open_negotiation_as` (1c-05), `barbarian_act` and `sack_city` (1c-06).

## Maps

`maps/arena.json` is 24 by 16 tiles, odd-r (odd rows shifted right), with no wrapping: an editor
map document (`citar/engine/maps.py`) with an `anchors` key the runners read and give the
engines without. Grassland, but for an ocean along the west edge (`x` 0 and 1), coast beside it
(`x` 2), and plains around B (`x` 15 to 21, `y` 8 to 12).

| Anchor | Tile | What |
|---|---|---|
| `A` | (5,5) | the first seat's start |
| `B` | (18,10) | the second seat's start, on plains |
| `C` | (18,4) | the third seat's start |
| `D` | (5,11) | the fourth seat's start |
| `E` | (11,13) | the fifth seat's start |
| `CS` | (11,7) | the city-state start, 6 to 8 tiles from every seat |
| `H` | (7,5) | a grassland hill, 2 east of A |
| `F` | (3,5) | a grassland forest, 2 west of A |
| `R` | (6,4) | north-east of A, with a river along its east and north-east edges |
| `W` | (2,5) | coast, 3 west of A |
| `L` | (4,3) | cotton, a luxury, on grassland 2 north of A |
| `M` | (5,7) | a mountain, 2 south of A |

The generator is not kept: edit the file, keep every anchor where it is, and check with
`maps.validate` that the document is its own clean form.
