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

`citar/engine/uniques.py` parses those into `Unique` objects, and the systems that care ask a
`UniqueMap` what applies in a given context — this city, this unit, this attack. So a building with
a unique the engine already knows needs no code at all.

Conditionals work the same way:

```
"[+15]% Strength <when attacking> <vs [Land] units>"
"[+2 Food] from every [Lake]"
"[+1 Happiness] from every [Colosseum]"
```

`citar/engine/unique_types.py` lists every unique type CITAR knows, generated from UnCiv's own enum.

### In the Rust engine (0.1.6)

The Rust engine in `crates/citar-engine` supports every unique type and conditional the Python
engine handled. That is the 402 types the shipped ruleset uses and 125 more that other UnCiv
rulesets use: 527 of UnCiv's 637 types. `crates/citar-engine/unique_supported.toml` lists them.

The engine compiles every unique when the ruleset loads. A mod does not load at all if one of its
uniques:

- is misspelt;
- has a parameter that does not read, such as a stat that does not exist, a name nothing has, or
  a number out of range (`Must be on [-1] largest landmasses`);
- uses one of UnCiv's other 110 types.

The error names the file, the object and what is wrong. Nothing is silently ignored.

The 125 types the shipped ruleset does not use are marked `(extra)` in that file. They all
compile now, and every conditional is evaluated. The other rules arrive with the parts of the engine
that read them, as each game system is ported. A few conditionals read differently from the
Python engine:

- the building conditionals take a building filter, so `<if [Wonder] is constructed>` works;
- `<when between [a] and [b] [stat]>` scales both bounds by game speed on a unique
  `<(modified by game speed)>`, as `<when above>` and `<when below>` do;
- `<if no Civilization has adopted []>` counts beliefs as well as policies;
- `<vs [] units>` asks about units only; `<vs [City]>` is the one for cities.

`refcheck/intended.toml` lists each of these with its reason.

`crates/citar-testkit/testdata/rulesets/kitchen_sink/` is a small mod that uses every one of the
125, so it has a worked example of each. Its files are JSON
merge patches over the shipped ruleset files: an object in a patch is added, or merged into the one
of the same name.

To support another UnCiv type, add its line to `unique_supported.toml`: its role, a name for each
parameter, and the systems that read it. Then run `cargo xtask gen-uniques`, handle it where those
systems read it, and test it.

---

## Where content lives

| | |
|---|---|
| `citar/data/ruleset/` | The UnCiv-derived ruleset. **Generated** — do not edit by hand |
| `citar/data/custom/` | CITAR's own additions, same format, merged over the ruleset |
| `citar/data/game.json` | Map sizes, lobby defaults, the AI interface's limits |

`citar/data/custom/nations.json` is the worked example: it adds `BenchmarkCiv`, a civilization with
no unique ability, unit, building or start bias, so that benchmark seats differ only in how they
are played.

---

## Adding something

1. Put it in a file under `citar/data/custom/` with the same shape as the ruleset file it extends.
2. Check that every unique you used is known:

   ```bash
   python scripts/check_uniques.py
   ```

   It flags unique text matching no known type. A unique the engine does not know is silently
   inert, which is the failure mode this catches.

3. Check your references:

   ```bash
   python scripts/check_refs.py
   ```

   Catches a `requiredTech` that does not exist, a promotion naming a missing unit type, and
   similar.

4. Try it:

   ```bash
   citar sim --players 4 --turns 60
   ```

   A headless bot game in the console is the fastest way to see whether something is wildly
   mispriced.

5. Balance it, if it matters:

   ```bash
   citar balance --games 22 --label my-change
   ```

   See [BOTS.md](BOTS.md#the-balance-simulator).

---

## Adding a new kind of rule

When a unique type does not exist, code is needed. These steps are for the Python engine; for the
Rust engine, see [In the Rust engine (0.1.6)](#in-the-rust-engine-016).

1. Add the unique type to `citar/engine/unique_types.py`.
2. Handle it in the engine module that owns the system — `cities.py` for a city yield,
   `combat.py` for a combat modifier, and so on. Find a similar unique and follow it.
3. Test it. `tests/test_mechanics.py` is where rule behaviour is pinned.

Keep the parsing in `uniques.py` and the meaning in the system module. The interpreter should not
know what a Library is.

---

## Adding a player action

An action a player can take — human, bot or model — goes in `citar/engine/tools.py`:

```python
@tool("do_the_thing", "What it does, written for whoever reads it",
      x="Tile x", y="Tile y")
def do_the_thing(game, player, x, y):
    ...
```

Registering it makes it available to the browser, to MCP clients and to the LLM adapter at once,
with its JSON schema generated from the signature. There is nowhere else to add it.

Two things to get right, because a model reads them:

- **The description is documentation for an agent.** Say what it does and when it applies.
- **Errors explain the rule.** `raise ActionError("That tile is not adjacent")`, not
  `ActionError("invalid")`. A model that is told why usually fixes it; a model that is told
  "invalid" tries again unchanged.

---

## Updating to a newer UnCiv

```bash
git clone --depth 1 https://github.com/yairm210/Unciv /tmp/unciv
python scripts/import_unciv.py /tmp/unciv
python scripts/gen_unique_types.py /tmp/unciv
python -m unittest discover -s tests
python scripts/check_uniques.py
```

`import_unciv.py` reads the "Civ V – Gods & Kings" ruleset and writes `citar/data/ruleset/`,
dropping everything presentational — quotes, civilopedia text, sounds, colours, leader dialogue.
Only numbers and rules come across.

Expect new unique types to appear unreferenced. `check_uniques.py` lists them; each is either
harmless or a mechanic to implement.

Bumping `RULES_VERSION` in `citar/engine/rules.py` marks old saves as incompatible, which is
correct when the rules a game was played under have changed.

---

## Licence

Files in `citar/data/ruleset/` are derived from UnCiv and are MPL-2.0, as is the rest of CITAR. If
you publish a mod, it is your own work under whatever licence you choose — the ruleset it extends
is not yours to relicense. See [NOTICE.md](https://github.com/jprodgers/CITAR/blob/main/NOTICE.md).
