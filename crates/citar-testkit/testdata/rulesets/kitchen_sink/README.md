# The kitchen-sink ruleset

A test mod that uses every unique type and conditional the engine supports but the shipped ruleset
does not use. These are the types marked `(extra)` in
[`unique_supported.toml`](../../../../citar-engine/unique_supported.toml). Each one appears at least
once, on an object where a ruleset would put it.

Each file is a JSON merge patch (RFC 7396) over the shipped ruleset file of the same name in
`citar/data/`. An object in a patch is added to its table, or merged into the object of the same
name. `citar_testkit::rulesets::kitchen_sink()` loads the result, and
`tests/engine/kitchen_sink.rs` checks it.

| File | Adds | What it carries |
|---|---|---|
| `nations.json` | Kitchen Sink | civilization-wide effects, and almost every trigger and conditional |
| `buildings.json` | Kitchen Sink Works, Kitchen Sink Wonder | building costs and purchases, obsolescence, victory |
| `units.json` | seven units | embarking, water travel, pillaging, invisibility, interception, great-person actions, unit triggers |
| `promotions.json` | Kitchen Sink Veteran | free promotion, flat strength, the unit conditionals |
| `improvements.json` | Kitchen Sink Outpost | pillaging, maintenance, obsolescence |
| `terrains.json` | Kitchen Sink Spire | a natural wonder on the largest landmass |
| `beliefs.json` | Kitchen Sink Faith | buying units and buildings with faith |
| `city_state_types.json` | Kitchen Sink | city-state gifts |
| `ruins.json` | four ruins | one-time effects on the unit that enters them |

When a type joins `unique_supported.toml`, add a use of it here. The coverage test fails until some
ruleset uses it.
