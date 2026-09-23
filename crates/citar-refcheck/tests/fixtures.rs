//! The refcheck pipeline on the committed fixtures, with stand-in answer modules: the recorded
//! answer itself, and copies of it with known changes. No engine group exists yet, so these are
//! what show that loading, comparing, explaining, enforcing and reporting hold together.
//!
//! Set `CITAR_REFCHECK_CORPUS` to a corpus folder to run the self-comparison over it too.

use std::path::{Path, PathBuf};

use serde_json::Value;

use citar_refcheck::answer::{AnswerError, AnswerModule, Answers, Ctx};
use citar_refcheck::compare::{DiffKind, Grid};
use citar_refcheck::enforced::Enforced;
use citar_refcheck::fixture::{self, Fixture, FixtureSet};
use citar_refcheck::intended::Intended;
use citar_refcheck::ratchet::{DEFAULT_FIXTURES, Ratchet};
use citar_refcheck::run::{self, Config, Outcome, Run, RunOptions, Verdict};
use citar_refcheck::{Group, report, suggest};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn committed_sets() -> Vec<FixtureSet> {
    let root = root();
    DEFAULT_FIXTURES.iter().map(|d| FixtureSet::new(&root.join(d), &root)).collect()
}

type Edit = fn(&Ctx<'_>, &mut Value);

/// An answer module that answers with the recorded answer, edited.
struct Stand {
    group: Group,
    edit: Option<Edit>,
}

impl AnswerModule for Stand {
    fn group(&self) -> Group {
        self.group
    }

    fn answer(&self, cx: &Ctx<'_>, expected: &Value) -> Result<Value, AnswerError> {
        let mut v = expected.clone();
        if let Some(edit) = self.edit {
            edit(cx, &mut v);
        }
        Ok(v)
    }
}

struct Stands(Vec<Stand>);

impl Answers for Stands {
    fn module(&self, group: Group) -> Option<&dyn AnswerModule> {
        self.0.iter().find(|m| m.group == group).map(|m| m as &dyn AnswerModule)
    }
}

/// Every recorded group answers with its own recording, except where an edit is given.
fn stands(edits: &[(Group, Edit)]) -> Stands {
    Stands(
        Group::recorded()
            .map(|group| Stand {
                group,
                edit: edits.iter().find(|(g, _)| *g == group).map(|(_, e)| *e),
            })
            .collect(),
    )
}

fn run_with(opts: RunOptions, config: &Config, answers: &Stands) -> Run {
    run::run(opts, config, answers).unwrap_or_else(|e| panic!("run: {e}"))
}

fn diffs_of(run: &Run) -> Vec<(String, String, String, DiffKind)> {
    run.findings()
        .map(|(s, c)| {
            (s.group.name().to_string(), s.case.clone(), c.diff.path.to_string(), c.diff.kind)
        })
        .collect()
}

#[test]
fn every_committed_fixture_loads_and_equals_itself() {
    let root = root();
    let mut sets = committed_sets();
    if let Some(corpus) = std::env::var_os("CITAR_REFCHECK_CORPUS") {
        sets.push(FixtureSet::new(Path::new(&corpus), &root));
    }
    let run = run_with(RunOptions::new(&root, sets), &Config::default(), &stands(&[]));
    assert!(run.load_failures.is_empty(), "{:?}", run.load_failures);
    assert!(run.states.len() >= 12, "9 mini and 3 late states, found {}", run.states.len());
    // Every recorded group was compared on every fixture, and the specs' keys hold on real data.
    for g in Group::recorded() {
        assert_eq!(run.summary(g).compared, run.states.len() as u64, "{g}");
    }
    assert_eq!(diffs_of(&run), Vec::new());
    assert_eq!(run.exit_code(), 0);
}

#[test]
fn committed_fixtures_are_the_expected_twelve() {
    let refs = fixture::discover(&committed_sets()).unwrap();
    let names: Vec<&str> = refs.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "duel-continents-normal/t1",
            "duel-continents-normal/t20",
            "duel-continents-normal/t50",
            "scenario-duel-fractal/t10",
            "scenario-duel-fractal/t11",
            "scenario-duel-fractal/t12",
            "scenario-small-continents-s3001/t61",
            "small-continents-normal-s1025/t280",
            "small-pangaea-raging/t1",
            "small-pangaea-raging/t12",
            "small-pangaea-raging/t50",
            "standard-pangaea-normal-s1031/t120",
        ],
        "sorted by case, then turn as a number"
    );
    for r in &refs {
        let meta = Fixture::load_meta(r).unwrap();
        assert_eq!(meta.turn, r.turn);
    }
}

// The three edits of the mutated copy (DESIGN.md Appendix B, 1a-04 gate 3).

/// One yield +0.5: the first owned tile's food.
fn yield_plus_half(_: &Ctx<'_>, v: &mut Value) {
    if let Some(food) = v.pointer_mut("/owned/0/yields/food") {
        *food = Value::from(food.as_f64().unwrap_or(0.0) + 0.5);
    }
}

/// A set-typed list reordered: every city's workable tiles, backwards.
fn workable_reversed(_: &Ctx<'_>, v: &mut Value) {
    for city in v["cities"].as_array_mut().into_iter().flatten() {
        if let Some(list) = city.get_mut("workable").and_then(Value::as_array_mut) {
            list.reverse();
        }
    }
}

/// The first route with a step that has another way round, through a tile adjacent to both its
/// neighbours on the route: (entry, step, the other tile).
fn find_reroute(grid: &Grid, paths: &Value) -> Option<(usize, usize, u64)> {
    for (e, entry) in paths.as_array()?.iter().enumerate() {
        let Some(path) = entry.get("path").and_then(Value::as_array) else { continue };
        let tiles: Vec<u64> = path.iter().filter_map(Value::as_u64).collect();
        for k in 1..tiles.len().saturating_sub(1) {
            let (a, c) = (tiles[k - 1], tiles[k + 1]);
            if let Some(b) =
                grid.neighbors(a).into_iter().find(|&b| !tiles.contains(&b) && grid.adjacent(b, c))
            {
                return Some((e, k, b));
            }
        }
    }
    None
}

/// A path rerouted at equal cost. The step costs are kept, so the summed cost is the same.
fn reroute_first_path(cx: &Ctx<'_>, v: &mut Value) {
    let grid = cx.fixture.expect("a fixture group").grid;
    if let Some((e, k, b)) = find_reroute(&grid, &v["paths"]) {
        v["paths"][e]["path"][k] = Value::from(b);
    }
}

const MUTATED: &str = "scenario-duel-fractal/t11";

fn mutated() -> Stands {
    stands(&[
        (Group::TileYields, yield_plus_half),
        (Group::CityStats, workable_reversed),
        (Group::Movement, reroute_first_path),
    ])
}

fn one_case(case: &str) -> RunOptions {
    let root = root();
    let mut opts = RunOptions::new(&root, committed_sets());
    opts.cases = vec![case.to_string()];
    opts
}

#[test]
fn a_mutated_copy_gives_exactly_the_expected_differences() {
    let run = run_with(one_case(MUTATED), &Config::default(), &mutated());
    let got: Vec<(String, DiffKind)> =
        diffs_of(&run).into_iter().map(|(g, _, p, k)| (format!("{g}: {p}"), k)).collect();
    // The fixture's first owned tile and first reroutable path, as recorded.
    let fixture_ref = fixture::discover(&committed_sets())
        .unwrap()
        .into_iter()
        .find(|r| r.name == MUTATED)
        .unwrap();
    let fixture = Fixture::load(&fixture_ref, &committed_sets()).unwrap();
    let idx = fixture.queries["tile_yields"]["owned"][0]["idx"].as_u64().unwrap();
    let (k, _, _) = find_reroute(&fixture.grid, &fixture.queries["movement"]["paths"])
        .expect("a route to reroute");
    assert_eq!(
        got,
        [
            (format!("tile_yields: owned[idx={idx}].yields.food"), DiffKind::Number),
            (format!("movement: paths[{k}].path"), DiffKind::PathEquivalent),
        ],
        "the reordered workable lists are multisets, so they give nothing"
    );
    let s = run.summary(Group::TileYields);
    assert_eq!((s.unexplained, s.explained), (1, 0));
    assert_eq!(run.summary(Group::Movement).accepted, 1);
    assert_eq!(run.summary(Group::Movement).unexplained, 0);
    assert_eq!(run.exit_code(), 0, "nothing is enforced");
}

#[test]
fn reordering_a_list_that_is_not_a_set_is_a_difference() {
    fn swap_first_calls(_: &Ctx<'_>, v: &mut Value) {
        if let Some(calls) = v["calls"].as_array_mut() {
            calls.swap(0, 1);
        }
    }
    let run = run_with(
        one_case(MUTATED),
        &Config::default(),
        &stands(&[(Group::ToolErrors, swap_first_calls)]),
    );
    assert!(!diffs_of(&run).is_empty());
    assert!(diffs_of(&run).iter().all(|(g, _, p, _)| g == "tool_errors"
        && (p.starts_with("calls[0]") || p.starts_with("calls[1]"))));
}

fn config(intended: &str, enforced: &str) -> Config {
    Config {
        intended: Intended::parse(intended).unwrap_or_else(|e| panic!("{e}")),
        enforced: Enforced::parse(enforced).unwrap_or_else(|e| panic!("{e}")),
    }
}

#[test]
fn intended_entries_explain_enforced_differences_and_go_stale() {
    let enforced = "[[enforce]]\ngroup = \"tile_yields\"\n";
    // Unexplained and enforced: exit 1.
    let run = run_with(one_case(MUTATED), &config("", enforced), &mutated());
    assert_eq!(run.summary(Group::TileYields).unexplained_enforced, 1);
    assert_eq!(run.exit_code(), 1);

    let intended = r#"
[[differences]]
id = "half-food"
reason = "Rust adds half a food"
where = [{ group = "tile_yields", path = "owned[*].yields.food" }]
rust = { min = 0.5, max = 100 }

[[differences]]
id = "wrong-value"
reason = "at the same place, but the value is not what this entry says"
where = [{ group = "tile_yields", path = "owned[*].yields.food" }]
rust = 1000

[[differences]]
id = "no-such-difference"
reason = "a covered entry that explains nothing is stale"
where = [{ group = "civs", path = "civs[*].score" }]

[[differences]]
id = "other-fixtures"
reason = "an entry whose cases are not in the run is not stale"
where = [{ group = "civs", path = "civs[*].era" }]
cases = ["standard-*"]

[[differences]]
id = "not-compared"
reason = "an entry whose group is not compared is not stale"
where = [{ group = "uniques", path = "texts[*]" }]

[[differences]]
id = "bot-values"
reason = "bot values are compared only with --with-bot"
where = [{ group = "deal_checks", path = "deals[*].bot_value.**" }]
broad = true
"#;
    let run = run_with(one_case(MUTATED), &config(intended, enforced), &mutated());
    let yields: Vec<_> = run.findings().filter(|(s, _)| s.group == Group::TileYields).collect();
    assert_eq!(yields.len(), 1);
    let (_, c) = yields[0];
    assert_eq!(c.verdict, Verdict::Explained("half-food".into()));
    assert_eq!(c.near.len(), 1, "wrong-value points there but its constraint fails");
    assert_eq!(c.near[0].id, "wrong-value");
    assert_eq!(run.exit_code(), 0);
    let stale: Vec<&str> = run.stale().map(|u| u.id.as_str()).collect();
    assert_eq!(stale, ["wrong-value", "no-such-difference"]);

    let mut strict = one_case(MUTATED);
    strict.strict = true;
    strict.groups = Group::recorded().collect();
    let run = run_with(strict, &config(intended, enforced), &mutated());
    assert_eq!(run.exit_code(), 3, "stale entries fail --strict");
    let mut strict_all = one_case(MUTATED);
    strict_all.strict = true;
    let run = run_with(strict_all, &config(intended, enforced), &mutated());
    assert_eq!(
        run.exit_code(),
        1,
        "--strict wants every selected group compared, uniques included"
    );
}

#[test]
fn an_enforced_group_without_an_answer_module_is_a_configuration_error() {
    let only_civs = Stands(vec![Stand { group: Group::Civs, edit: None }]);
    let e = run::run(
        one_case(MUTATED),
        &config("", "[[enforce]]\ngroup = \"tile_yields\"\n"),
        &only_civs,
    )
    .unwrap_err();
    assert!(e.message().contains("tile_yields"), "{e}");
}

#[test]
fn the_json_report_is_byte_identical_across_runs() {
    let root = root();
    let a = run_with(RunOptions::new(&root, committed_sets()), &Config::default(), &mutated());
    let b = run_with(RunOptions::new(&root, committed_sets()), &Config::default(), &mutated());
    let (ja, jb) = (report::json(&a), report::json(&b));
    assert!(ja.contains("\"kind\": \"path_equivalent\""), "the report has differences in it");
    assert_eq!(ja, jb);
    assert_eq!(report::human(&a, 20), report::human(&b, 20));
    // Groups come in dependency order.
    let order: Vec<usize> = ["\"tile_yields\"", "\"city_stats\"", "\"movement\"", "\"briefing\""]
        .iter()
        .map(|g| ja.find(&format!("\"group\": {g}")).unwrap())
        .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn a_failing_answer_module_is_an_error_difference_and_the_run_goes_on() {
    struct Failing;
    impl AnswerModule for Failing {
        fn group(&self) -> Group {
            Group::Civs
        }
        fn answer(&self, _: &Ctx<'_>, _: &Value) -> Result<Value, AnswerError> {
            Err(AnswerError::new("not ported: economy::happiness"))
        }
    }
    struct Panicking;
    impl AnswerModule for Panicking {
        fn group(&self) -> Group {
            Group::Visible
        }
        fn answer(&self, _: &Ctx<'_>, _: &Value) -> Result<Value, AnswerError> {
            panic!("a bug in the answer module")
        }
    }
    struct Two;
    impl Answers for Two {
        fn module(&self, group: Group) -> Option<&dyn AnswerModule> {
            match group {
                Group::Civs => Some(&Failing),
                Group::Visible => Some(&Panicking),
                _ => None,
            }
        }
    }
    let run = run::run(one_case(MUTATED), &Config::default(), &Two).unwrap();
    let got = diffs_of(&run);
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got.iter().all(|(_, _, p, k)| p == "(root)" && *k == DiffKind::Error));
    assert_eq!(run.summary(Group::Civs).unexplained, 1);
    assert_eq!(run.summary(Group::Visible).unexplained, 1);
    assert!(matches!(
        run.subjects.iter().find(|s| s.group == Group::TileYields).unwrap().outcome,
        Outcome::NotPorted
    ));
}

#[test]
fn suggest_writes_stubs_that_load_and_explain_what_they_were_written_for() {
    let run = run_with(one_case(MUTATED), &Config::default(), &mutated());
    let stubs = suggest::suggest(&run);
    assert!(stubs.contains("path = \"owned[*].yields.food\""), "{stubs}");
    let list = Intended::parse(&stubs).unwrap_or_else(|e| panic!("{e}\n{stubs}"));
    assert_eq!(list.entries().len(), 1, "path_equivalent needs no entry");
    let rerun = run_with(
        one_case(MUTATED),
        &Config { intended: list, enforced: Enforced::default() },
        &mutated(),
    );
    assert_eq!(rerun.unexplained(), 0);
}

#[test]
fn the_ratchet_refuses_a_rise() {
    let run = run_with(one_case(MUTATED), &Config::default(), &mutated());
    let counts = run.counts();
    assert!(counts.contains(&(Group::TileYields, 1)));
    let zero: Vec<(Group, u64)> = counts.iter().map(|(g, _)| (*g, 0)).collect();
    let ratchet = Ratchet::default().updated(&zero).unwrap();
    let verdict = ratchet.check(&counts);
    assert!(verdict.fails());
    assert_eq!(verdict.rises, [(Group::TileYields, 0, 1)]);
    assert!(ratchet.updated(&counts).is_err(), "--update refuses it too");
    // From the other side, the same counts are a fall, which --update records.
    let high = Ratchet::default().updated(&counts).unwrap();
    assert!(!high.check(&zero).fails());
    assert_eq!(high.updated(&zero).unwrap(), ratchet);
}

#[test]
fn explain_shows_an_entry_or_a_place() {
    let intended = r#"
[[differences]]
id = "half-food"
reason = "Rust adds half a food"
where = [{ group = "tile_yields", path = "owned[*].yields.food" }]
"#;
    let cfg = config(intended, "");
    let run = run_with(one_case(MUTATED), &cfg, &mutated());
    let by_id = report::explain(&run, &cfg.intended, "half-food").unwrap();
    assert!(by_id.contains("explained 1 difference"), "{by_id}");
    let place = report::explain(&run, &cfg.intended, "tile_yields:owned[*].yields.*").unwrap();
    assert!(place.contains("tiles.tile_stats"), "the Python function is named: {place}");
    assert!(place.contains("1 difference"), "{place}");
    assert!(report::explain(&run, &cfg.intended, "nonsense").is_err());
}
