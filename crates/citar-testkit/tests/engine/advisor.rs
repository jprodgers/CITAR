//! The production advisor (package 1c-07):
//! - gate 1: the what-if of one more building (`cities::what_if::what_if_building`) gives, bit
//!   for bit, the city's stats and happiness that adding the building and reading the city's
//!   memos gives, on every city of the reference states and every building it could build (on
//!   the committed states, every building it does not have), and neither it nor the add and
//!   take-away leaves the digest moved;
//! - gate 2: over the same states the advisor, in both of its modes and as automatic production
//!   asks it, gives only what the city can build, and the same answer twice, and again on the
//!   state loaded afresh;
//! - gate 3: a puppet picks a building that is no wonder, or Gold;
//! - the kitchen sink's extra of this system: `Triggers victory` is worth the victory building's
//!   value in a strong city.

use citar_engine::base::ids::{BaseUnitId, BuildingId, CityId};
use citar_engine::base::stats::Stat;
use citar_engine::game::Game;
use citar_engine::game::advisor::{self, AdvisorParams, ProductionMode};
use citar_engine::game::cities::construction::{buildable_items, is_buildable};
use citar_engine::game::cities::what_if::{toggle_building_for_test, what_if_building};
use citar_engine::game::derive::rev::BitEq as _;
use citar_engine::game::query;
use citar_engine::rules::Ruleset;
use citar_engine::state::cities::{Constructible, Perpetual};
use citar_testkit::fixtures::{self, Fixture};

fn load(f: &Fixture) -> Game {
    let bytes = fixtures::read_state(f).unwrap_or_else(|e| panic!("{e}"));
    let (g, _report) = Game::from_python(Ruleset::shared(), &bytes)
        .unwrap_or_else(|e| panic!("{} does not load: {e}", f.name));
    g
}

fn committed() -> Vec<Fixture> {
    fixtures::committed().unwrap_or_else(|e| panic!("{e}"))
}

fn corpus() -> Vec<Fixture> {
    fixtures::corpus().unwrap_or_else(|e| panic!("{e}")).unwrap_or_default()
}

fn cities(g: &Game) -> Vec<CityId> {
    g.state().cities().iter().map(citar_engine::state::cities::City::id).collect()
}

/// The buildings the what-if is asked about in a city: every one it does not have, or those it
/// could build.
fn candidates(g: &Game, c: CityId, all: bool) -> Vec<BuildingId> {
    let r = g.rules();
    let Some(city) = g.city(c) else { return Vec::new() };
    if all {
        return r.buildings().ids().filter(|&b| !city.buildings.contains(b)).collect();
    }
    let items = buildable_items(g, c);
    items.buildings.iter().chain(items.wonders.iter()).collect()
}

/// Gate 1 on one state: the mismatches, as lines.
fn what_if_agrees(f: &Fixture, all: bool) -> (Vec<String>, usize) {
    let mut g = load(f);
    let before = g.digest().expect("a digest");
    let mut out = Vec::new();
    let mut asked = 0;
    for c in cities(&g) {
        for b in candidates(&g, c, all) {
            let w = what_if_building(&g, c, b).expect("a city without the building");
            asked += 1;
            let now = query::city_stats(&g, c).total;
            toggle_building_for_test(&mut g, c, b, true);
            let with = query::city_stats(&g, c).total;
            toggle_building_for_test(&mut g, c, b, false);
            let back = query::city_stats(&g, c).total;
            let name = &g.rules().buildings()[b].name;
            if !w.before.bit_eq(&now) || !back.bit_eq(&now) {
                out.push(format!(
                    "{} city {} {name}: before {:?} now {now:?}",
                    f.name,
                    c.get(),
                    w.before
                ));
            }
            if !w.after.bit_eq(&with) {
                out.push(format!(
                    "{} city {} {name}: what-if {:?}, built {with:?}",
                    f.name,
                    c.get(),
                    w.after
                ));
            }
        }
    }
    assert_eq!(g.digest().expect("a digest"), before, "{}: the digest moved", f.name);
    (out, asked)
}

#[test]
fn a_what_if_equals_adding_the_building_on_the_committed_states() {
    let mut bad = Vec::new();
    let mut asked = 0;
    for f in committed() {
        let (b, n) = what_if_agrees(&f, true);
        bad.extend(b);
        asked += n;
    }
    assert!(asked > 1000, "only {asked} asked");
    assert!(bad.is_empty(), "{} of {asked} differ:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn a_what_if_equals_adding_the_building_on_the_corpus() {
    let mut bad = Vec::new();
    let mut asked = 0;
    for f in corpus() {
        let (b, n) = what_if_agrees(&f, false);
        bad.extend(b);
        asked += n;
    }
    assert!(bad.is_empty(), "{} of {asked} differ:\n{}", bad.len(), bad.join("\n"));
}

/// Gate 2 on one state: every answer buildable and the same twice, and the same on a fresh load.
fn advice_holds(f: &Fixture) -> Vec<String> {
    let g = load(f);
    let before = g.digest().expect("a digest");
    let mut out = Vec::new();
    let unciv = AdvisorParams::default();
    let classic = AdvisorParams { prod_mode: ProductionMode::Classic, ..AdvisorParams::default() };
    let ask = |g: &Game, c: CityId| -> [Option<Constructible>; 3] {
        let p = g.city(c).map(citar_engine::state::cities::City::owner).expect("a city");
        [
            advisor::advise_production(g, p, c, &unciv),
            advisor::advise_production(g, p, c, &classic),
            advisor::auto_pick(g, c),
        ]
    };
    let majors: Vec<CityId> = cities(&g)
        .into_iter()
        .filter(|&c| {
            g.city(c).is_some_and(|x| {
                g.player(x.owner()).is_some_and(citar_engine::state::players::Player::is_major)
            })
        })
        .collect();
    let mut first = Vec::new();
    for &c in &majors {
        // Gate 3: whatever the city is, the puppet's pick is a building that is no wonder, or
        // Gold; and a puppet's automatic production is it.
        let pup = advisor::puppet_pick(&g, c);
        let fits = match pup {
            None | Some(Constructible::Perpetual(Perpetual::Gold)) => true,
            Some(Constructible::Building(b)) => !g.rules().buildings()[b].any_wonder,
            Some(_) => false,
        };
        if !fits || pup.is_some_and(|x| !is_buildable(&g, c, x)) {
            out.push(format!("{} city {}: a puppet would pick {pup:?}", f.name, c.get()));
        }
        if g.city(c).is_some_and(|x| x.puppet) && advisor::auto_pick(&g, c) != pup {
            out.push(format!("{} city {}: the puppet picks otherwise", f.name, c.get()));
        }
        let a = ask(&g, c);
        for item in a.iter().flatten() {
            if !is_buildable(&g, c, *item) {
                out.push(format!("{} city {}: {item:?} cannot be built", f.name, c.get()));
            }
        }
        if ask(&g, c) != a {
            out.push(format!("{} city {}: a second answer differs", f.name, c.get()));
        }
        first.push(a);
    }
    assert_eq!(g.digest().expect("a digest"), before, "{}: the digest moved", f.name);
    let fresh = load(f);
    for (&c, a) in majors.iter().zip(&first) {
        if ask(&fresh, c) != *a {
            out.push(format!("{} city {}: a fresh load answers otherwise", f.name, c.get()));
        }
    }
    out
}

#[test]
fn the_advisor_gives_what_can_be_built_and_the_same_twice() {
    let mut bad = Vec::new();
    for f in committed().into_iter().chain(corpus()) {
        bad.extend(advice_holds(&f));
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn a_what_if_moves_the_stats_a_building_gives() {
    // The answers are not all nothing: on the committed states most buildings a city could build
    // change something.
    let (mut asked, mut moved) = (0, 0);
    for f in committed() {
        let g = load(&f);
        for c in cities(&g) {
            for b in candidates(&g, c, false) {
                let w = what_if_building(&g, c, b).expect("a what-if");
                asked += 1;
                if Stat::ALL.iter().any(|&k| w.delta()[k] != 0.0) {
                    moved += 1;
                }
            }
        }
    }
    assert!(moved * 2 > asked, "{moved} of {asked} moved a stat");
}

#[test]
fn the_reference_states_reach_every_part_of_the_what_if() {
    // Of what the overlay can hold beside the city's own and its owner's index: a building that
    // changes its owner's resources (needing one), and the resource layer with them; a harbour's
    // trade network; a city in We Love The King Day asking its owner's happiness (the corpus).
    use citar_engine::game::cities::what_if::reach_for_test;
    let mut n = [0usize; 7];
    let mut asked = 0;
    let states: Vec<(Fixture, bool)> = committed()
        .into_iter()
        .map(|f| (f, true))
        .chain(corpus().into_iter().map(|f| (f, false)))
        .collect();
    let with_corpus = states.len() > 12;
    for (f, all) in states {
        let g = load(&f);
        for c in cities(&g) {
            for b in candidates(&g, c, all) {
                let x = reach_for_test(&g, c, b).expect("a what-if");
                asked += 1;
                let hits =
                    [x.local, x.civ, x.supply, x.resource_layer, x.network, x.deficit, x.happiness];
                for (i, hit) in hits.into_iter().enumerate() {
                    n[i] += usize::from(hit);
                }
            }
        }
    }
    let [local, civ, supply, layer, network, _deficit, happiness] = n;
    assert!(local > 0 && civ > 0 && supply > 0 && layer > 0 && network > 0, "{n:?} of {asked}");
    if with_corpus {
        assert!(happiness > 0, "{n:?} of {asked}");
    }
}

// ---- Gate 4: how often the advisor agrees with Python's (a report) ------------------------------

/// Python's advisor on the reference states (`scripts/refcheck/advisor_dump.py`).
#[derive(serde::Deserialize)]
struct Dump {
    states: Vec<DumpState>,
}

#[derive(serde::Deserialize)]
struct DumpState {
    case: String,
    turn: u32,
    cities: Vec<DumpCity>,
}

#[derive(serde::Deserialize)]
struct DumpCity {
    city: u32,
    auto: Option<String>,
    puppet_pick: Option<String>,
    unciv: Option<String>,
    classic: Option<String>,
}

/// How many of the dump's answers the Rust advisor gives alike, for one question.
#[derive(Clone, Copy, Default)]
struct Agreement {
    agree: usize,
    asked: usize,
    /// Of those that differ, where Python founds a city: the sites a settler would go to wait
    /// for package 1c-04's scoring.
    settler: usize,
}

/// How many of the dump's answers the Rust advisor gives alike, for automatic production, the
/// puppet's pick, and the two modes.
fn agreement(dump: &Dump, folder: &[Fixture]) -> [Agreement; 4] {
    let mut out = [Agreement::default(); 4];
    let name = |g: &Game, x: Option<Constructible>| {
        x.map(|i| citar_engine::game::cities::construction::item_name(g.rules(), i).to_string())
    };
    let founds = |g: &Game, x: &Option<String>| {
        x.as_deref()
            .and_then(|n| g.rules().lookup::<BaseUnitId>(n))
            .is_some_and(|u| g.rules().derived().advisor.founders.contains(u))
    };
    let classic = AdvisorParams { prod_mode: ProductionMode::Classic, ..AdvisorParams::default() };
    for s in &dump.states {
        let Some(f) = folder.iter().find(|f| f.case == s.case && f.turn == s.turn) else {
            continue;
        };
        let g = load(f);
        for row in &s.cities {
            let c =
                CityId::new(row.city).unwrap_or_else(|| panic!("{}: city {}", f.name, row.city));
            let p = g.city(c).map(citar_engine::state::cities::City::owner).expect("the city");
            let rust = [
                name(&g, advisor::auto_pick(&g, c)),
                name(&g, advisor::puppet_pick(&g, c)),
                name(&g, advisor::advise_production(&g, p, c, &AdvisorParams::default())),
                name(&g, advisor::advise_production(&g, p, c, &classic)),
            ];
            let python = [&row.auto, &row.puppet_pick, &row.unciv, &row.classic];
            for (n, (a, b)) in out.iter_mut().zip(rust.iter().zip(python)) {
                n.asked += 1;
                if a == b {
                    n.agree += 1;
                } else if founds(&g, b) {
                    n.settler += 1;
                }
            }
        }
    }
    out
}

#[allow(clippy::disallowed_macros, reason = "a report prints what it measured")]
fn report(what: &str, n: [Agreement; 4]) {
    let names = ["automatic production", "a puppet's pick", "unciv mode", "classic mode"];
    for (name, x) in names.iter().zip(n) {
        #[allow(clippy::cast_precision_loss, reason = "a share to print")]
        let pct = if x.asked == 0 { 0.0 } else { 100.0 * x.agree as f64 / x.asked as f64 };
        eprintln!(
            "advisor agreement, {what}, {name}: {} of {} ({pct:.1}%); of the {} that differ, {} \
             where Python founds a city",
            x.agree,
            x.asked,
            x.asked - x.agree,
            x.settler
        );
    }
}

#[test]
#[allow(clippy::disallowed_methods, reason = "the dumps are files, the corpus a test's switch")]
fn the_advisors_agreement_with_python_is_reported() {
    // Informational (gate 4): the share of cities where the Rust advisor picks what Python's did.
    // It differs on purpose where the draw, the order of units and ties differ, and picks no
    // settler until city sites are scored (package 1c-04).
    let path = fixtures::repo_root().join("crates/citar-testkit/data/advisor.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let dump: Dump = serde_json::from_str(&text).expect("the dump");
    let n = agreement(&dump, &committed());
    assert_eq!(n[0].asked, dump.states.iter().map(|s| s.cities.len()).sum::<usize>());
    report("committed states", n);
    // The corpus's, recorded beside it (or where `CITAR_ADVISOR_DUMP` says) when there is one.
    if let Some(dir) = std::env::var_os(fixtures::CORPUS_ENV) {
        let path = std::env::var_os("CITAR_ADVISOR_DUMP")
            .map_or_else(|| std::path::Path::new(&dir).join("advisor.json"), Into::into);
        if let Ok(text) = std::fs::read_to_string(&path) {
            let dump: Dump = serde_json::from_str(&text).expect("the corpus's dump");
            report("corpus", agreement(&dump, &corpus()));
        }
    }
}

// ---- The kitchen sink and overlays ------------------------------------------------------------

mod sink {
    use citar_engine::api::testops;
    use citar_engine::base::ids::{BuildingId, CityId, PlayerId};
    use citar_engine::base::stats::{Stat, Stats};
    use citar_engine::game::advisor::{self, AdvisorParams};
    use citar_engine::game::cities::what_if::{
        reach_for_test, toggle_building_for_test, what_if_building,
    };
    use citar_engine::game::derive::rev::BitEq as _;
    use citar_engine::game::{DebugOptions, Game, query};
    use citar_engine::rules::Ruleset;
    use citar_engine::state::cities::Constructible;
    use citar_testkit::rulesets::{KITCHEN_SINK, files_of, kitchen_sink, overlay};
    use citar_testkit::script::{map_doc, new_game};
    use serde_json::{Value, json};

    const ME: PlayerId = PlayerId(0);

    /// A bare game on the arena: the Kitchen Sink against a benchmark civilization.
    fn arena(r: &'static Ruleset) -> Game {
        let (doc, _) = map_doc("arena").expect("the arena");
        let cfg = json!({
            "seed": 1,
            "players": [{"nation": "Kitchen Sink"}, {"nation": "BenchmarkCiv"}],
            "city_states": 0,
            "barbarians": "off",
            "ruins": false,
            "map": doc,
        });
        let mut g =
            new_game(r, cfg.as_object().expect("an object")).unwrap_or_else(|e| panic!("{e}"));
        g.set_debug_options(DebugOptions::ALL);
        testops::apply(&mut g, &json!([{"op": "clear_units", "player": "all"}])).expect("cleared");
        g
    }

    fn ops(g: &mut Game, ops: &Value) -> Vec<Value> {
        g.apply_ops(ops).unwrap_or_else(|e| panic!("{e:?}")).0
    }

    fn founded(out: &Value) -> CityId {
        out["city_id"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .and_then(CityId::new)
            .unwrap_or_else(|| panic!("no city in {out}"))
    }

    /// The what-if of `b` in `c`, held to adding it; what the building adds.
    fn agrees(g: &mut Game, c: CityId, b: BuildingId) -> Stats {
        let w = what_if_building(g, c, b).expect("a what-if");
        toggle_building_for_test(g, c, b, true);
        let with = query::city_stats(g, c).total;
        toggle_building_for_test(g, c, b, false);
        let name = &g.rules().buildings()[b].name;
        assert!(w.after.bit_eq(&with), "{name}: {:?} against {with:?}", w.after);
        w.delta()
    }

    /// The ruleset of the kitchen sink with `patch` over its buildings.
    fn sink_with_buildings(patch: &Value) -> &'static Ruleset {
        let patch = patch.to_string();
        let mut patches = KITCHEN_SINK.to_vec();
        patches.push(("ruleset/buildings.json", &patch));
        let files = overlay(&patches).expect("the patches apply");
        Ruleset::leak(&files_of(&files)).expect("the ruleset loads")
    }

    #[test]
    fn a_strong_city_builds_what_wins_the_game() {
        // `Triggers victory` is worth the victory building's value in a city that produces at
        // least the average: the Kitchen Sink Wonder is built before anything else. Without it
        // the wonder is worth what its culture is, and a monument more.
        let pick = |r: &'static Ruleset| {
            let mut g = arena(r);
            let out = ops(
                &mut g,
                &json!([
                    {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Sinkhold", "pop": 4},
                    {"op": "grant_tech", "player": 0, "tech": "Philosophy"},
                ]),
            );
            let c = founded(&out[0]);
            let wonder = r.lookup::<BuildingId>("Kitchen Sink Wonder").expect("the wonder");
            assert!(agrees(&mut g, c, wonder)[Stat::Culture] > 0.0);
            (advisor::advise_production(&g, ME, c, &AdvisorParams::auto_production()), wonder)
        };
        let (picked, wonder) = pick(kitchen_sink());
        assert_eq!(picked, Some(Constructible::Building(wonder)));
        let r = sink_with_buildings(&json!({"Kitchen Sink Wonder": {"uniques": []}}));
        let (picked, wonder) = pick(r);
        assert!(picked.is_some() && picked != Some(Constructible::Building(wonder)), "{picked:?}");
    }

    /// Sinkhold, of four, with `building` built: a city on the arena for the what-if of the
    /// Kitchen Sink Wonder, which `[in all cities with a world wonder]` then names.
    fn sinkhold_with(r: &'static Ruleset, building: &str, warriors: u32) -> (Game, CityId) {
        let mut g = arena(r);
        let out = ops(
            &mut g,
            &json!([
                {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Sinkhold", "pop": 4},
                {"op": "set_city", "x": 5, "y": 5, "add_buildings": [building]},
            ]),
        );
        if warriors > 0 {
            ops(
                &mut g,
                &json!([{"op": "add_unit", "player": 0, "unit": "Warrior", "x": 6, "y": 6,
                         "count": warriors}]),
            );
        }
        (g, founded(&out[0]))
    }

    #[test]
    fn a_tile_modifier_of_cities_with_a_wonder_comes_with_the_wonder() {
        // `[stats] from [tiles] tiles [in all cities with a world wonder]` reads the buildings of
        // the city through its city filter, which names no class: the what-if of a wonder
        // gathers the city's tile modifiers again, as building it does.
        let r = sink_with_buildings(&json!({"Wonder Gardens": {
            "name": "Wonder Gardens", "cost": 60, "maintenance": 1,
            "uniques": [
                "[+2 Culture] from [Land] tiles [in all cities with a world wonder]",
                "[+1 Gold] from [Land] tiles without [Forest] [in all cities with a world wonder]",
            ],
            "id": "wonder_gardens",
        }}));
        let (mut g, c) = sinkhold_with(r, "Wonder Gardens", 0);
        let wonder = r.lookup::<BuildingId>("Kitchen Sink Wonder").expect("the wonder");
        let d = agrees(&mut g, c, wonder);
        // The wonder's own 3 culture, and 2 more from each land tile the city works.
        assert!(d[Stat::Culture] >= 5.0 && d[Stat::Gold] > 0.0, "{d:?}");
        assert!(g.take_violations().is_empty());
    }

    #[test]
    fn unit_supply_of_cities_with_a_wonder_comes_with_the_wonder() {
        // `[n] Unit Supply per [k] population [in all cities with a world wonder]` reads the
        // buildings of each city of its owner: the what-if of a wonder asks the unit supply again,
        // and the penalty of a civilization over it lifts, as building it does.
        let r = sink_with_buildings(&json!({"Wonder Barracks": {
            "name": "Wonder Barracks", "cost": 60, "maintenance": 1,
            "uniques": ["[+2] Unit Supply per [1] population [in all cities with a world wonder]"],
            "id": "wonder_barracks",
        }}));
        let (mut g, c) = sinkhold_with(r, "Wonder Barracks", 20);
        let wonder = r.lookup::<BuildingId>("Kitchen Sink Wonder").expect("the wonder");
        let reach = reach_for_test(&g, c, wonder).expect("a what-if");
        assert!(reach.deficit, "{reach:?}");
        let d = agrees(&mut g, c, wonder);
        assert!(d[Stat::Production] > 0.0, "{d:?}");
        assert!(g.take_violations().is_empty());
        assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
    }

    #[test]
    fn a_building_that_adds_unit_supply_lifts_the_penalty_in_the_what_if() {
        // A civilization over its unit supply loses production in every city; a building that
        // adds supply lifts it, which the what-if reads from its owner's unit supply asked again.
        let r = sink_with_buildings(&json!({"Supply Depot": {
            "name": "Supply Depot", "cost": 60, "maintenance": 1,
            "uniques": ["[+4] Unit Supply"], "id": "supply_depot",
        }}));
        let mut g = arena(r);
        let out = ops(
            &mut g,
            &json!([
                {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Sinkhold", "pop": 3},
                {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 6, "y": 6, "count": 20},
            ]),
        );
        let c = founded(&out[0]);
        let depot = r.lookup::<BuildingId>("Supply Depot").expect("the depot");
        let reach = reach_for_test(&g, c, depot).expect("a what-if");
        assert!(reach.civ && reach.deficit, "{reach:?}");
        let d = agrees(&mut g, c, depot);
        assert!(d[Stat::Production] > 0.0, "{d:?}");
        // Every other building of the kitchen sink agrees in the city too.
        let all: Vec<BuildingId> = r.buildings().ids().collect();
        for b in all {
            if g.city(c).is_some_and(|x| !x.buildings.contains(b)) {
                agrees(&mut g, c, b);
            }
        }
        assert!(g.take_violations().is_empty());
        assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
    }
}
