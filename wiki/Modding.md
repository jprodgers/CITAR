# Adding and changing content

Most of CITAR is data. A new building, unit, technology, policy, belief or civilization is a JSON
entry, not code — because the rules are expressed as text that an interpreter reads, rather than as
branches in the engine.

---

## How rules are expressed

UnCiv writes rules as **uniques**: short sentences with typed parameters and optional conditions.

```json
"Library": {
  "name": "Library",
  "cost": 75,
  "maintenance": 1,
  "hurryCostModifier": 25,
  "requiredTech": "Writing",
  "uniques": ["[+1 Science] per [2] population [in this city]"]
}
```

The engine compiles each unique into a typed rule when the ruleset loads, and the systems that care
read the rules that apply in a given context — this city, this unit, this attack. So a building with
a unique the engine already knows needs no code at all.

Conditionals work the same way:

```
"[+15]% Strength <when attacking> <vs [Land] units>"
"[+2 Food] from every [Lake]"
"[+1 Happiness] from every [Colosseum]"
```

`crates/citar-engine/unique_types.tsv` lists every unique type UnCiv has, generated from UnCiv's
own enum, and `crates/citar-engine/unique_supported.toml` the ones CITAR supports.

### What the engine supports

The engine in `crates/citar-engine` supports every unique type and conditional the Python engine of
0.1.5 handled. That is the 402 types the shipped ruleset uses and 125 more that other UnCiv
rulesets use: 527 of UnCiv's 637 types. `unique_supported.toml` lists them, the 125 marked
`(extra)`, each with its role (a standing effect, a flag, a requirement, a one-time effect, a
conditional, a trigger, ...) and the game systems that read it. Every one is implemented: each
system reads the types that name it. Fourteen types the shipped ruleset uses are inert, as they
were in the Python engine: they compile, and nothing reads them (the file gives each one's reason).

**The ruleset is compiled when it loads.** Each unique is matched to its type once, and its
parameters become typed values: a stat, an amount, an id, a compiled filter, a conditional. A
game never reads unique text again. So a mod does not load at all if one of its uniques:

- is misspelt;
- has a parameter that does not read, such as a stat that does not exist, a name nothing has, or
  a number out of range (`Must be on [-1] largest landmasses`);
- uses one of UnCiv's other 110 types.

The JSON is read as strictly: an unknown or repeated field, an object whose `name` is not its
key, or a reference to something that does not exist (a `requiredTech`, a unit's promotion) is an
error too. Every error is reported, each naming the file, the object and what is wrong. Nothing is
silently ignored. [`citar ruleset check`](#playing-a-modded-ruleset) prints them.

**What reads differently from the Python engine.** A few rules a mod can meet:

- the building conditionals take a building filter, so `<if [Wonder] is constructed>` works;
- `<when between [a] and [b] [stat]>` scales both bounds by game speed on a unique
  `<(modified by game speed)>`, as `<when above>` and `<when below>` do;
- `<if no Civilization has adopted []>` counts beliefs as well as policies;
- `<vs [] units>` asks about units only; `<vs [City]>` is the one for cities;
- a resource's `[in this city]` uniques hold in the city whose improved tile provides the
  resource, not in every city of its owner;
- a timed unique (`<for [10] turns>`) is granted whatever its other conditionals say, which then
  decide, turn by turn, whether it applies.

The CHANGELOG lists every rule the engine reads differently from the Python engine, each with its
reason, from `refcheck/intended.toml` and `tests/rules/intended.toml`.

**When rules are evaluated.** Two things follow from how the engine keeps its numbers, and a mod
can see both:

- *Conditionals that read happiness* (`<while the empire is happy>`, `<when above [5]
  [Happiness]>`) read the value committed at the start and the end of the civilization's turn,
  not the live one. Within a turn it does not move, so citizens cannot flip back and forth as
  happiness crosses zero.
- *Citizens are placed after each action and at fixed points of the turn*, in one step the engine
  calls the settle, until no city wants to change. A unique whose condition reads a city's own
  citizens (`<in cities with [2] [Specialists]>`) can make a city's best placement depend on the
  placement; the engine then keeps the first placement the city comes back to. A settle takes at
  most `SETTLE_PASSES` (8) passes, though: two neighbouring cities whose placements keep changing
  each other's yields (with `<in tiles adjacent to [worked] tiles>`, say) stop there, each where
  the last pass left it, and test builds report it as invariant SETTLE-1. So avoid uniques that
  make neighbouring cities' best placements depend on each other.

Everything else is computed when it is read and kept until something it depends on changes, so
how often a value is asked for, or when, never changes the answer.

**The worked example.** `crates/citar-testkit/testdata/rulesets/kitchen_sink/` is a small mod
that uses every one of the 125 extra types, so it has an example of each, and the tests play it
(whole games, in the soak). Its files are JSON merge patches over the shipped ruleset files: an
object in a patch is added, or merged into the one of the same name.

---

## Where content lives

The ruleset is compiled into the engine from `crates/citar-engine/data/`:

| | |
|---|---|
| `ruleset/` | The UnCiv-derived ruleset. **Generated** — do not edit by hand |
| `custom/nations.json` | CITAR's own civilizations, merged over the ruleset's |
| `game.json` | Map sizes, lobby defaults, the AI interface's limits |

`custom/nations.json` is the worked example: it adds `BenchmarkCiv`, a civilization with no unique
ability, unit, building or start bias, so that benchmark seats differ only in how they are played.

---

## Playing a modded ruleset

An installed CITAR plays a modded ruleset without a Rust toolchain: the engine reads a ruleset
directory when it loads, instead of the one compiled into it.

1. **Copy the data.** Copy `crates/citar-engine/data/` from the source of your CITAR version (the
   GitHub release, or a checkout of its tag) to a folder of your own. Keep the layout:
   `ruleset/*.json`, `custom/nations.json` and `game.json`.
2. **Change it.** Add a civilization to `custom/nations.json`, or edit or add entries in the
   tables under `ruleset/`, in the same shape as the entries beside them. The engine reads exactly
   these files: a `.json` file of another name in `ruleset/` or `custom/` is reported, not
   ignored.
3. **Check it.**

   ```bash
   citar ruleset check path/to/my-ruleset        # or: python -m citar ruleset check path/to/my-ruleset
   ```

   It loads the directory as the engine would and prints every problem, each with its file, its
   object and its kind (`UnknownUnique`, `UnknownReference`, `Schema`, ...), and exits 1; or it
   prints the ruleset's id and what it holds, and exits 0. The loader checks in stages — the
   files, their fields, the references, the uniques, the filters — and stops after the first
   stage that finds a problem, so fix what it reports and run it again until it loads.
4. **Play it.** Set `CITAR_RULESET_DIR` to the folder and start CITAR again (the server, `citar
   sim`, the lab: every process that plays reads it once, when the engine loads):

   ```bash
   CITAR_RULESET_DIR=path/to/my-ruleset citar serve
   ```

   `citar doctor` says which ruleset the process plays, with its id. A directory that does not
   load stops the engine from loading at all, with the problems, rather than playing the shipped
   rules in its place.
5. **Try it.**

   ```bash
   CITAR_RULESET_DIR=path/to/my-ruleset citar sim --players 4 --turns 60
   ```

   A headless bot game in the console is the fastest way to see whether something is wildly
   mispriced. Balance it, if it matters, with `citar balance` (see
   [BOTS.md](Scripted-bots#the-balance-simulator)).

**A modded ruleset is its own ruleset.** Its id (`RulesetId`, a hash of the parsed files) differs
from the shipped ruleset's, so everything that records a ruleset tells the two apart:

- the build id (`citar doctor`, `engine_api.build_info()`), and with it every bot fingerprint, lab
  result and rating, so a modded process's results are never mixed with the shipped ruleset's;
- saves, which record the ruleset they were played with. A save loads under another ruleset only
  if every name it holds still resolves there, and then with a warning in the log; a save that
  names something the other ruleset lacks (the civilization you added, say) is refused, naming it.

If you build CITAR from source, you can also edit `crates/citar-engine/data/` itself and rebuild
(`cargo xtask develop`, or `pip install -e .`): the ruleset is then compiled in.

---

## Adding a new kind of rule

When a unique type the engine does not support is needed, code is needed, in Rust:

1. Add its line to `crates/citar-engine/unique_supported.toml`: its role, a name for each
   parameter, and the systems that read it.
2. Run `cargo xtask gen-uniques`, which writes `src/unique/gen.rs` (`cargo xtask check` fails
   while that file is out of date).
3. Handle it where those systems read it: an effect in the module of `src/game/` that owns the
   system; a conditional in `src/unique/cond.rs`, both its answer and what it reads, its
   dependencies. The caches that evaluate a conditional recompute when one of its dependencies
   changes, so a dependency left out leaves a stale answer, which the cache oracle in the tests
   reports.
4. Use it in the kitchen-sink ruleset, and test it in `crates/citar-testkit` (a rule script in
   `tests/rules/` where a behaviour can be set up and checked from outside).

Keep the parsing in `src/unique/` and the meaning in the system's module. The interpreter should
not know what a Library is.

---

## Adding a player action

An action a player can take — human, bot or model — is a tool in the engine's one registry,
`crates/citar-engine/src/api/tools/registry.rs`: its name, the description a model reads, its
parameters (from which its JSON schema is made), whether it is a query or an action, and when it
may be used. Registering it makes it available to the browser, to MCP clients and to the LLM
adapter at once. There is nowhere else to add it.

Two things to get right, because a model reads them:

- **The description is documentation for an agent.** Say what it does and when it applies.
- **Errors explain the rule.** "That tile is not adjacent", not "invalid". A model that is told
  why usually fixes it; a model that is told "invalid" tries again unchanged.

---

## Updating to a newer UnCiv

```bash
git clone --depth 1 https://github.com/yairm210/Unciv /tmp/unciv
python scripts/import_unciv.py /tmp/unciv
python scripts/gen_unique_types.py /tmp/unciv
cargo xtask gen-uniques
citar ruleset check crates/citar-engine/data
```

`import_unciv.py` reads the "Civ V – Gods & Kings" ruleset and writes
`crates/citar-engine/data/ruleset/`, dropping everything presentational — quotes, civilopedia text,
sounds, colours, leader dialogue. Only numbers and rules come across.

Expect new unique types to appear: `citar ruleset check` reports a unique the engine does not
support, and each is either harmless to leave out or a mechanic to implement (see [Adding a new
kind of rule](#adding-a-new-kind-of-rule)).

A changed ruleset has a new id, which saves record: an old save still loads if every name it holds
resolves in the new ruleset, with a warning. Bump `RULES_VERSION` in
`crates/citar-engine/src/rules/constants.rs` when the ruleset's format changes in a way old saves
cannot follow.

---

## Licence

Files in `crates/citar-engine/data/ruleset/` are derived from UnCiv and are MPL-2.0, as is the rest
of CITAR. If you publish a mod, it is your own work under whatever licence you choose — the ruleset
it extends is not yours to relicense. See
[NOTICE.md](https://github.com/jprodgers/CITAR/blob/main/NOTICE.md).


---

*This page is generated from [`docs/MODDING.md`](https://github.com/jprodgers/CITAR/blob/main/docs/MODDING.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
