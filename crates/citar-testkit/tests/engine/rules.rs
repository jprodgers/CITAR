//! `rules` against the Python ruleset, as `scripts/refcheck/rules_dump.py` recorded it, and
//! against its own contract (package 1a-03):
//! - gate 1: the embedded ruleset loads in under 20 ms in release (reported; a failure only above
//!   60 ms);
//! - gate 2: `client_json` equals Python's `rules_client()`, key order included;
//! - gate 3: `resolve` matches Python for every name and id of every table;
//! - gate 4: the `RulesetId` ignores line endings and indentation and sees a changed number (the
//!   golden `ruleset.json` holds it the same on every target);
//! - gate 5: a dangling reference, an unknown field, a table over capacity and a missing file are
//!   each refused, with the file and object named.

use citar_engine::base::ids::{
    BarbarianLevelId, BaseUnitId, BeliefId, BuildingId, DifficultyId, EraId, FeatureId, Id,
    ImprovementId, MapSizeId, MapTypeId, NationId, PolicyId, PromotionId, ResourceId, SpecialistId,
    SpeedId, TechId, TerrainId, UnitTypeId, VictoryId,
};
use citar_engine::base::sets::{FeatureSet, TechSet};
use citar_engine::base::stats::Stat;
use citar_engine::rules::defs::{
    ImprovementKind, NationKind, PolicyKind, QuestKind, QuestTargetKind, Route, TerrainType,
};
use citar_engine::rules::source::all_file_names;
use citar_engine::rules::{
    NameKind, Named, Ruleset, RulesetErrorKind, RulesetErrors, RulesetFiles, embedded,
};
use serde_json::{Value, json};

const DUMP: &str = include_str!("../../data/rules_dump.json");

fn dump() -> Value {
    serde_json::from_str(DUMP).expect("rules_dump.json is JSON")
}

pub(super) fn shipped() -> &'static Ruleset {
    Ruleset::shared()
}

/// The embedded files as owned text, for tests that change them.
fn owned_files() -> Vec<(String, Vec<u8>)> {
    embedded().files.iter().map(|&(n, b)| (n.to_owned(), b.to_vec())).collect()
}

fn load_owned(files: &[(String, Vec<u8>)]) -> Result<Ruleset, RulesetErrors> {
    Ruleset::load(&RulesetFiles::new(
        files.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect(),
    ))
}

/// Loads the embedded ruleset with one file's JSON changed by `edit`.
pub(super) fn load_edited(
    file: &str,
    edit: impl FnOnce(&mut Value),
) -> Result<Ruleset, RulesetErrors> {
    let mut files = owned_files();
    let slot = files.iter_mut().find(|(n, _)| n == file).expect("a ruleset file");
    let mut v: Value = serde_json::from_slice(&slot.1).expect("JSON");
    edit(&mut v);
    slot.1 = serde_json::to_vec(&v).expect("JSON");
    load_owned(&files)
}

/// Every place where `a` and `b` differ, object key order included.
fn differences(path: &str, a: &Value, b: &Value, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let kx: Vec<&String> = x.keys().collect();
            let ky: Vec<&String> = y.keys().collect();
            if kx != ky {
                out.push(format!("{path}: keys {kx:?} against {ky:?}"));
            }
            for (k, v) in x {
                if let Some(w) = y.get(k) {
                    differences(&format!("{path}.{k}"), v, w, out);
                }
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                out.push(format!("{path}: {} items against {}", x.len(), y.len()));
            }
            for (i, (v, w)) in x.iter().zip(y).enumerate() {
                differences(&format!("{path}[{i}]"), v, w, out);
            }
        }
        _ => {
            if a != b {
                out.push(format!("{path}: {a} against {b}"));
            }
        }
    }
}

// ---- Gate 2: the client JSON --------------------------------------------------------------------

#[test]
fn client_json_equals_python_rules_client() {
    let rust: Value = serde_json::from_str(shipped().client_json()).expect("client JSON");
    let python = dump()["client"].clone();
    assert_eq!(rust, python, "compared as JSON values");
    let mut diffs = Vec::new();
    differences("client", &rust, &python, &mut diffs);
    assert!(diffs.is_empty(), "the client JSON differs from Python's:\n{}", diffs.join("\n"));
}

// ---- Gate 3: resolve --------------------------------------------------------------------------

#[test]
fn resolve_matches_python_for_every_name_and_id() {
    let rules = shipped();
    let dump = dump();
    let tables = dump["resolve"].as_object().expect("resolve rows by kind");
    assert_eq!(tables.len(), NameKind::ALL.len());
    let mut rows = 0;
    let mut wrong = Vec::new();
    for (kind, list) in tables {
        let k = NameKind::from_name(kind).expect("a kind Python has");
        for row in list.as_array().expect("rows") {
            let text = row[0].as_str().expect("the text");
            let want = row[1].as_str();
            let got = rules.resolve_name(k, text);
            if got != want {
                wrong
                    .push(format!("resolve({kind}, {text:?}) is {want:?} in Python, {got:?} here"));
            }
            rows += 1;
        }
    }
    assert!(rows > 6_000, "only {rows} rows recorded");
    assert!(wrong.is_empty(), "{} of {rows} differ:\n{}", wrong.len(), wrong.join("\n"));
}

/// Every name of `I`'s table resolves, exactly and loosely, to its own id, and back.
fn resolves_to_itself<I: Named>(rules: &Ruleset) -> usize {
    let mut n = 0;
    for (pos, name) in rules.names(I::KIND).enumerate() {
        let id = I::from_index(pos).expect("an id");
        assert_eq!(rules.resolve::<I>(name), Some(id), "{:?} {name}", I::KIND);
        assert_eq!(rules.lookup::<I>(name), Some(id));
        assert_eq!(rules.name(id), Some(name));
        assert_eq!(rules.resolve_name(I::KIND, name), Some(name));
        n += 1;
    }
    n
}

#[test]
fn every_table_resolves_its_own_names_and_ids_to_themselves() {
    let rules = shipped();
    let counts = [
        resolves_to_itself::<TechId>(rules),
        resolves_to_itself::<BaseUnitId>(rules),
        resolves_to_itself::<BuildingId>(rules),
        resolves_to_itself::<PromotionId>(rules),
        resolves_to_itself::<TerrainId>(rules),
        resolves_to_itself::<ResourceId>(rules),
        resolves_to_itself::<ImprovementId>(rules),
        resolves_to_itself::<BeliefId>(rules),
        resolves_to_itself::<PolicyId>(rules),
        resolves_to_itself::<NationId>(rules),
        resolves_to_itself::<EraId>(rules),
        resolves_to_itself::<SpecialistId>(rules),
        resolves_to_itself::<SpeedId>(rules),
        resolves_to_itself::<DifficultyId>(rules),
        resolves_to_itself::<UnitTypeId>(rules),
        resolves_to_itself::<VictoryId>(rules),
    ];
    assert!(counts.iter().all(|&n| n > 0));
    assert_eq!(rules.resolve_name(NameKind::Speed, "quick"), Some("Quick"));
    assert_eq!(rules.resolve_name(NameKind::Tech, "bronze_working"), Some("Bronze Working"));
    let bronze = rules.resolve::<TechId>("bronze_working").expect("a tech");
    assert_eq!(&*rules.techs()[bronze].name, "Bronze Working");
    assert_eq!(rules.lookup::<TechId>("bronze_working"), None, "lookup is exact");
    assert_eq!(rules.resolve::<BaseUnitId>("bronze_working"), None, "a tech is no unit");
    // Policies share one id space, branches first.
    let tradition = rules.resolve::<PolicyId>("tradition").expect("a branch");
    assert!(rules.policies()[tradition].is_branch());
    let aristocracy = rules.resolve::<PolicyId>("Aristocracy").expect("a policy");
    assert!(aristocracy.index() >= usize::from(rules.policy_branch_count()));
}

// ---- The tables -------------------------------------------------------------------------------

#[test]
fn the_shipped_tables_have_the_census_sizes() {
    let r = shipped();
    // DESIGN.md 5.1.
    assert_eq!(r.techs().len(), 80);
    assert_eq!(r.base_units().len(), 127);
    assert_eq!(r.unit_types().len(), 28);
    assert_eq!(r.buildings().len(), 124);
    assert_eq!(r.promotions().len(), 106);
    assert_eq!(r.terrains().len(), 33);
    assert_eq!(r.resources().len(), 35);
    assert_eq!(r.improvements().len(), 35);
    assert_eq!(r.beliefs().len(), 56);
    assert_eq!(usize::from(r.policy_branch_count()), 10);
    assert_eq!(r.policies().len(), 70);
    assert_eq!(r.nations().len(), 83, "82 and BenchmarkCiv");
    assert_eq!(r.eras().len(), 9);
    let counts = serde_json::to_value(r.counts()).expect("counts");
    assert_eq!(
        counts,
        json!({"techs": 80, "units": 127, "buildings": 124, "nations": 83, "policies": 60})
    );
    let kinds = |k: TerrainType| r.terrains().as_slice().iter().filter(|t| t.kind == k).count();
    assert_eq!(kinds(TerrainType::Land) + kinds(TerrainType::Water), 9);
    assert_eq!(kinds(TerrainType::TerrainFeature), 10);
    assert_eq!(kinds(TerrainType::NaturalWonder), 14);
    assert_eq!(r.max_players(), 24);
    assert_eq!(r.speed_names().collect::<Vec<_>>(), ["Quick", "Standard", "Epic", "Marathon"]);
    assert_eq!(r.difficulty_names().next(), Some("Settler"));
    assert_eq!(r.map_sizes().len(), 6);
    // The lobby's lists are held by id, found by key.
    let k = r.constants();
    let small = k.map_size_id("small").expect("small");
    assert_eq!((small, r.map_sizes().get(small).map(|m| m.width)), (MapSizeId(1), Some(60)));
    assert_eq!(k.map_size("small").map(|m| &*m.name), Some("Small"));
    assert_eq!(k.map_type_id("fractal"), Some(MapTypeId(4)));
    let raging = k.barbarian_level_id("raging").expect("raging");
    assert_eq!(raging, BarbarianLevelId(2));
    let level = |b: BarbarianLevelId| k.barbarian_levels.get(b).and_then(|l| l.level.clone());
    assert!(level(raging).is_some_and(|l| l.raging));
    assert_eq!(k.barbarian_level_id("off").and_then(level), None);
    assert_eq!(k.map_size_id("Small"), None, "keys are exact");
    assert!(r.global_uniques().all.len() >= 8);
    assert_eq!(r.fracs().len(), 19, "the distinct fractions of the terrains' generation rules");
}

#[test]
fn references_resolve_to_ids() {
    let r = shipped();
    let unit = |name: &str| r.lookup::<BaseUnitId>(name).expect("a unit");
    let warrior = &r.base_units()[unit("Warrior")];
    assert_eq!(warrior.strength, 8);
    assert_eq!(warrior.range, 2, "Python's default");
    assert!(warrior.melee && warrior.military && !warrior.ranged);
    let archer = &r.base_units()[unit("Archer")];
    assert!(archer.ranged && !archer.melee);
    assert_eq!(archer.required_tech.map(|t| &*r.techs()[t].name), Some("Archery"));
    assert_eq!(
        r.base_units()[unit("Swordsman")].upgrades_to.map(|u| &*r.base_units()[u].name),
        Some("Longswordsman")
    );
    assert!(r.base_units()[unit("Great Scientist")].great_person);
    let worker = &r.base_units()[unit("Worker")];
    let class = worker.builder.expect("workers build");
    let filters: Vec<&str> = r.derived().builder_classes[usize::from(class.0)]
        .iter()
        .map(|&f| r.uniques().text(r.uniques().object(f).text))
        .collect();
    assert_eq!(filters, ["Land"]);
    assert!(r.base_units()[unit("Warrior")].builder.is_none());
    let babylon = r.lookup::<NationId>("Babylon").expect("a nation");
    assert_eq!(r.nations()[babylon].kind, NationKind::Major);
    assert!(r.derived().major_nations.contains(&babylon));
    let bench = r.nations().as_slice().last().expect("the custom nation");
    assert_eq!(&*bench.name, "BenchmarkCiv");
    assert_eq!(bench.key.as_deref(), Some("benchmarkciv"), "Python's id for a custom nation");
    assert!(bench.benchmark);
    let aristocracy = &r.policies()[r.lookup::<PolicyId>("Aristocracy").expect("a policy")];
    let PolicyKind::Member { branch, requires, finisher } = &aristocracy.kind else {
        panic!("Aristocracy is a policy")
    };
    assert_eq!(&*r.policies()[*branch].name, "Tradition");
    assert_eq!(requires.len(), 1);
    assert!(!finisher);
    // The Monument raises culture; the Colosseum happiness.
    let building = |name: &str| &r.buildings()[r.lookup::<BuildingId>(name).expect("a building")];
    assert!(building("Monument").stat_related.contains(Stat::Culture));
    assert!(building("Colosseum").stat_related.contains(Stat::Happiness));
    assert!(building("The Great Library").any_wonder);
}

#[test]
fn derived_tables_follow_python() {
    let r = shipped();
    let order: Vec<&str> = r.derived().tech_order.iter().map(|&t| &*r.techs()[t].name).collect();
    assert_eq!(&order[..4], ["Agriculture", "Animal Husbandry", "Archery", "Mining"]);
    assert_eq!(order.len(), 80);
    let agriculture = TechId(0);
    assert_eq!(&*r.techs()[agriculture].name, "Agriculture");
    // Features: Hill lowest, Fallout highest.
    let features: Vec<&str> =
        r.derived().features.iter().map(|(_, &t)| &*r.terrains()[t].name).collect();
    assert_eq!(features.first(), Some(&"Hill"));
    assert_eq!(features.last(), Some(&"Fallout"));
    assert_eq!(features.len(), 10);
    assert!(features.len() <= FeatureSet::CAPACITY);
    let hill = r.derived().known.hill;
    let fallout = r.derived().known.fallout;
    assert_eq!(hill, FeatureId(0));
    let mut tile = FeatureSet::EMPTY;
    tile.insert(hill);
    let forest =
        r.terrains().as_slice().iter().find(|t| &*t.name == "Forest").and_then(|t| t.feature);
    tile.insert(forest.expect("Forest is a feature"));
    assert_eq!(tile.top(), forest, "a forested hill shows the forest");
    tile.insert(fallout);
    assert_eq!(tile.top(), Some(fallout), "fallout lies on top");
    // Improvements Python told by name.
    let imp =
        |name: &str| &r.improvements()[r.lookup::<ImprovementId>(name).expect("an improvement")];
    assert_eq!(imp("Road").kind, ImprovementKind::Route(Route::Road));
    assert_eq!(imp("Remove Railroad").kind, ImprovementKind::RemoveRoute(Route::Railroad));
    assert_eq!(imp("Remove Fallout").kind, ImprovementKind::RemoveFeature(fallout));
    assert!(imp("Academy").great);
    assert_eq!(r.derived().feature_removals.len(), 4, "forest, jungle, fallout, marsh");
    assert_eq!(r.derived().removal_of[fallout], r.lookup::<ImprovementId>("Remove Fallout"));
    assert!(r.derived().known.barbarian_camp.is_some());
    // Great people and spaceship parts, and the speeds' last turns.
    assert!(r.derived().great_person_units.len() >= 5);
    assert_eq!(r.derived().spaceship_parts.len(), 4);
    let max: Vec<i32> = r.speeds().as_slice().iter().map(|s| s.max_turns()).collect();
    assert_eq!(max, [330, 500, 750, 1500]);
    assert!(r.terrains().as_slice().iter().any(|t| t.rough));
    let rough_hill = r.terrains()[r.derived().features[hill]].rough;
    assert!(rough_hill, "hills are rough terrain");
    let _: TerrainId = r.derived().features[hill];
}

// ---- Gate 4: the RulesetId --------------------------------------------------------------------

#[test]
fn the_ruleset_id_ignores_formatting_and_sees_values() {
    let base = shipped().id();
    // Re-indented, with CRLF line endings.
    let reformatted: Vec<(String, Vec<u8>)> = owned_files()
        .into_iter()
        .map(|(n, b)| {
            let v: Value = serde_json::from_slice(&b).expect("JSON");
            let text = serde_json::to_string_pretty(&v).expect("JSON").replace('\n', "\r\n");
            (n, text.into_bytes())
        })
        .collect();
    assert_ne!(reformatted, owned_files(), "the files really changed");
    let again = load_owned(&reformatted).expect("the reformatted ruleset loads");
    assert_eq!(again.id(), base);
    // One number changed.
    let changed = load_edited("ruleset/techs.json", |v| {
        v["techs"]["Agriculture"]["cost"] = json!(21);
    })
    .expect("the edited ruleset loads");
    assert_ne!(changed.id(), base);
    let version = shipped().version();
    assert!(version.starts_with("2-") && version.len() == 14, "{version}");
    assert_eq!(&version[2..], &base.to_hex()[..12]);
}

#[test]
fn leaking_the_same_ruleset_twice_leaks_one_copy() {
    let a = Ruleset::leak(&embedded()).expect("loads");
    let b = Ruleset::leak(&embedded()).expect("loads");
    assert!(std::ptr::eq(a, b));
    assert!(std::ptr::eq(a, Ruleset::shared()));
}

// ---- Gate 5: what is refused ------------------------------------------------------------------

/// The one problem of `kind` the load reported, which must name `file` and `object`.
pub(super) fn refused(
    result: Result<Ruleset, RulesetErrors>,
    kind: RulesetErrorKind,
    file: &str,
    object: &str,
) -> String {
    let errs = result.expect_err("the ruleset is refused");
    let found: Vec<_> = errs.0.iter().filter(|e| e.kind == kind).collect();
    assert!(!found.is_empty(), "no {kind:?} problem in: {errs}");
    let e = found
        .iter()
        .find(|e| &*e.file == file && &*e.object == object)
        .unwrap_or_else(|| panic!("no {kind:?} problem for {file}: {object} in: {errs}"));
    e.to_string()
}

#[test]
fn a_dangling_reference_is_refused() {
    let r = load_edited("ruleset/units.json", |v| {
        v["Warrior"]["requiredTech"] = json!("Bronze Workin");
    });
    let text = refused(r, RulesetErrorKind::UnknownReference, "ruleset/units.json", "Warrior");
    assert!(text.contains("requiredTech \"Bronze Workin\" is not in ruleset/techs.json"), "{text}");
    // Python checked only a few references (rules.py:238-260); these it never checked.
    let r = load_edited("ruleset/promotions.json", |v| {
        v["Accuracy II"]["prerequisites"] = json!(["Accuracy 1"]);
    });
    refused(r, RulesetErrorKind::UnknownReference, "ruleset/promotions.json", "Accuracy II");
    let r = load_edited("game.json", |v| v["default_speed"] = json!("Fast"));
    refused(r, RulesetErrorKind::UnknownReference, "game.json", "");
}

#[test]
fn an_unknown_field_is_refused() {
    let r = load_edited("ruleset/units.json", |v| {
        v["Warrior"]["requiredTeck"] = json!("Bronze Working");
    });
    let text = refused(r, RulesetErrorKind::Schema, "ruleset/units.json", "Warrior");
    assert!(text.contains("unknown field `requiredTeck`"), "{text}");
    let r = load_edited("game.json", |v| v["constants"]["city_strenght_base"] = json!(8.0));
    let text = refused(r, RulesetErrorKind::Schema, "game.json", "");
    assert!(text.contains("city_strenght_base"), "{text}");
    // A comment key is not a field.
    let r = load_edited("ruleset/units.json", |v| v["_doc"] = json!("notes"));
    assert!(r.is_ok());
}

#[test]
fn a_table_over_capacity_is_refused() {
    let r = load_edited("ruleset/techs.json", |v| {
        let techs = v["techs"].as_object_mut().expect("techs");
        let template = techs["Agriculture"].clone();
        for i in 0..(TechSet::CAPACITY - 80 + 1) {
            let name = format!("Future Tech {i}");
            let mut t = template.clone();
            t["name"] = json!(name);
            t["id"] = json!(format!("future_tech_{i}"));
            techs.insert(name, t);
        }
    });
    let text = refused(r, RulesetErrorKind::Capacity, "ruleset/techs.json", "");
    assert!(text.contains("129 techs") && text.contains("TECH_WORDS"), "{text}");
    let r = load_edited("ruleset/terrains.json", |v| {
        let terrains = v.as_object_mut().expect("terrains");
        let template = terrains["Forest"].clone();
        for i in 0..7 {
            let name = format!("Thicket {i}");
            let mut t = template.clone();
            t["name"] = json!(name);
            t["id"] = json!(format!("thicket_{i}"));
            terrains.insert(name, t);
        }
    });
    let text = refused(r, RulesetErrorKind::Capacity, "ruleset/terrains.json", "");
    assert!(text.contains("17 terrain features"), "{text}");
    // The lobby's lists in game.json are held by u8 ids too.
    let r = load_edited("game.json", |v| {
        let types = v["map_types"].as_object_mut().expect("map types");
        for i in 0..(256 - 5 + 1) {
            types.insert(format!("type_{i}"), json!({"name": format!("Type {i}")}));
        }
    });
    let text = refused(r, RulesetErrorKind::Capacity, "game.json", "map_types");
    assert!(text.contains("257 map types") && text.contains("MapTypeId"), "{text}");
}

#[test]
fn a_missing_file_is_refused() {
    let mut files = owned_files();
    files.retain(|(n, _)| n != "ruleset/beliefs.json");
    refused(load_owned(&files), RulesetErrorKind::MissingFile, "ruleset/beliefs.json", "");
    // The custom nations are optional, as in Python.
    let mut files = owned_files();
    files.retain(|(n, _)| n != "custom/nations.json");
    let r = load_owned(&files).expect("loads without custom nations");
    assert_eq!(r.nations().len(), 82);
    assert_ne!(r.id(), shipped().id());
}

#[test]
fn other_problems_are_refused_too() {
    // A key given twice, which Python's json.load would have resolved silently.
    let mut files = owned_files();
    let slot = files.iter_mut().find(|(n, _)| n == "game.json").expect("game");
    let text = String::from_utf8(slot.1.clone()).expect("UTF-8");
    slot.1 = text
        .replacen("\"move_scale\": 60,", "\"move_scale\": 60, \"move_scale\": 30,", 1)
        .into_bytes();
    refused(load_owned(&files), RulesetErrorKind::Json, "game.json", "");
    // An object whose name is not its key.
    let r =
        load_edited("ruleset/techs.json", |v| v["techs"]["Archery"]["name"] = json!("Bowmanship"));
    refused(r, RulesetErrorKind::Name, "ruleset/techs.json", "Archery");
    // Eras out of number order.
    let r = load_edited("ruleset/eras.json", |v| v["Classical era"]["number"] = json!(7));
    refused(r, RulesetErrorKind::Invalid, "ruleset/eras.json", "Classical era");
    // A speed calendar that runs backwards.
    let r = load_edited("ruleset/speeds.json", |v| v["Quick"]["turns"][1]["untilTurn"] = json!(10));
    refused(r, RulesetErrorKind::Invalid, "ruleset/speeds.json", "Quick");
    // An unknown file and a repeated one.
    let mut files = owned_files();
    files.push(("ruleset/tech.json".to_owned(), b"{}".to_vec()));
    refused(load_owned(&files), RulesetErrorKind::UnknownFile, "ruleset/tech.json", "");
    // Every problem at once, not just the first.
    let r = load_edited("ruleset/units.json", |v| {
        v["Warrior"]["requiredTech"] = json!("Nope");
        v["Archer"]["requiredTech"] = json!("Nope");
    });
    assert_eq!(r.err().map(|e| e.0.len()), Some(2));
    // The Hill feature, which the engine needs.
    let r = load_edited("ruleset/terrains.json", |v| {
        v["Hill"]["type"] = json!("Land");
    });
    refused(r, RulesetErrorKind::Missing, "ruleset/terrains.json", "Hill");
}

#[test]
fn custom_nations_merge_as_python_merged_them() {
    // A custom nation with a known key replaces it in place (`rules.py:78-81`).
    let r = load_edited("custom/nations.json", |v| {
        let mut babylon = v["BenchmarkCiv"].clone();
        babylon["name"] = json!("Babylon");
        babylon["leaderName"] = json!("Hammurabi");
        v["Babylon"] = babylon;
    })
    .expect("loads");
    assert_eq!(r.nations().len(), 83);
    let babylon = r.lookup::<NationId>("Babylon").expect("Babylon");
    assert_eq!(Some(babylon), shipped().lookup::<NationId>("Babylon"));
    assert_eq!(r.nations()[babylon].leader_name.as_deref(), Some("Hammurabi"));
    assert_eq!(r.nations()[babylon].key.as_deref(), Some("babylon"), "made from its key");
    // A problem in a custom nation names the custom file.
    let r =
        load_edited("custom/nations.json", |v| v["BenchmarkCiv"]["personality"] = json!("Nobody"));
    refused(r, RulesetErrorKind::UnknownReference, "custom/nations.json", "BenchmarkCiv");
    let r = load_edited("custom/nations.json", |v| v["BenchmarkCiv"]["leader"] = json!("Nobody"));
    refused(r, RulesetErrorKind::Schema, "custom/nations.json", "BenchmarkCiv");
    let r = load_edited("custom/nations.json", |v| v["BenchmarkCiv"]["name"] = json!("Other"));
    refused(r, RulesetErrorKind::Name, "custom/nations.json", "BenchmarkCiv");
    // A custom nation that replaces a shipped one is the custom file's too.
    let r = load_edited("custom/nations.json", |v| {
        let mut babylon = v["BenchmarkCiv"].clone();
        babylon["name"] = json!("Hammurabi's");
        v["Babylon"] = babylon;
    });
    refused(r, RulesetErrorKind::Name, "custom/nations.json", "Babylon");
}

#[test]
fn every_quest_has_its_kind() {
    let r = shipped();
    let kinds: Vec<QuestKind> = r.quests().as_slice().iter().map(|q| q.kind).collect();
    assert_eq!(kinds.len(), QuestKind::ALL.len(), "the 17 quests of quests.json");
    for k in QuestKind::ALL {
        assert_eq!(kinds.iter().filter(|&&q| q == k).count(), 1, "{k:?} once");
    }
    for q in r.quests().as_slice() {
        assert_eq!(q.kind.name(), &*q.name);
        assert_eq!(q.target, q.kind.target());
    }
    let invest = r.quests().as_slice().iter().find(|q| q.kind == QuestKind::Invest);
    assert_eq!(invest.map(|q| q.target), Some(QuestTargetKind::Percent));
    // Python never gave a quest it had no code for; here it is an error.
    let r = load_edited("ruleset/quests.json", |v| {
        let mut q = v["Route"].clone();
        q["name"] = json!("Build Canal");
        q["id"] = json!("build_canal");
        v["Build Canal"] = q;
    });
    let text = refused(r, RulesetErrorKind::Invalid, "ruleset/quests.json", "Build Canal");
    assert!(text.contains("Clear Barbarian Camp"), "{text}");
}

#[test]
fn a_branch_and_its_policies_agree() {
    // A branch as a member of another branch.
    let r = load_edited("ruleset/policies.json", |v| {
        v["branches"]["Tradition"]["members"]
            .as_array_mut()
            .expect("members")
            .push(json!("Liberty"));
    });
    let text = refused(r, RulesetErrorKind::UnknownReference, "ruleset/policies.json", "Tradition");
    assert!(text.contains("members \"Liberty\""), "{text}");
    // Another branch's policy listed as a member: both sides disagree.
    let r = load_edited("ruleset/policies.json", |v| {
        let members = v["branches"]["Tradition"]["members"].as_array_mut().expect("members");
        members.retain(|m| m != "Aristocracy");
        v["branches"]["Liberty"]["members"]
            .as_array_mut()
            .expect("members")
            .push(json!("Aristocracy"));
    });
    let errs = r.expect_err("refused");
    let text = errs.to_string();
    assert!(
        text.contains("Liberty: its member \"Aristocracy\" belongs to another branch"),
        "{text}"
    );
    assert!(text.contains("Aristocracy: its branch \"Tradition\" does not list it"), "{text}");
    // The finisher is adopted once the members are, so it is never one of them.
    let r = load_edited("ruleset/policies.json", |v| {
        v["branches"]["Tradition"]["members"]
            .as_array_mut()
            .expect("members")
            .push(json!("Tradition Complete"));
    });
    refused(r, RulesetErrorKind::Invalid, "ruleset/policies.json", "Tradition");
}

#[test]
fn game_constants_later_code_relies_on_are_checked() {
    let r = load_edited("game.json", |v| v["move_scale"] = json!(0));
    let text = refused(r, RulesetErrorKind::Invalid, "game.json", "move_scale");
    assert!(text.contains("at least 1"), "{text}");
    let r =
        load_edited("game.json", |v| v["constants"]["unit_upgrade_cost"]["round_to"] = json!(0));
    refused(r, RulesetErrorKind::Invalid, "game.json", "constants");
    let r = load_edited("game.json", |v| v["constants"]["city_state_election_turns"] = json!(-1));
    let text = refused(r, RulesetErrorKind::Invalid, "game.json", "constants");
    assert!(text.contains("city_state_election_turns is -1"), "{text}");
    let r = load_edited("game.json", |v| {
        v["map_size_predefined"].as_array_mut().expect("sizes").reverse();
    });
    refused(r, RulesetErrorKind::Invalid, "game.json", "map_size_predefined");
    // 63 majors and the barbarians fill a PlayerSet; one more does not fit.
    assert!(load_edited("game.json", |v| v["max_players"] = json!(63)).is_ok());
    let r = load_edited("game.json", |v| v["max_players"] = json!(64));
    refused(r, RulesetErrorKind::Capacity, "game.json", "max_players");
}

#[test]
fn the_embedded_files_are_the_ruleset_files() {
    let names: Vec<&str> = embedded().files.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, all_file_names().collect::<Vec<_>>());
}

// ---- Gate 1: load time ------------------------------------------------------------------------

#[test]
fn the_embedded_ruleset_loads_quickly() {
    #[allow(clippy::disallowed_types, reason = "timing the load is the point of the test")]
    fn best_of(n: u32) -> std::time::Duration {
        let files = embedded();
        (0..n)
            .map(|_| {
                let t = std::time::Instant::now();
                let r = Ruleset::load(&files).expect("loads");
                let took = t.elapsed();
                drop(r);
                took
            })
            .min()
            .unwrap_or_default()
    }
    let best = best_of(5);
    #[allow(clippy::disallowed_macros, reason = "the time is reported, not only asserted")]
    {
        println!("embedded ruleset load: best of 5 is {:.2} ms", best.as_secs_f64() * 1e3);
    }
    // The budget is 20 ms in release (DESIGN.md 10); until 1e-03 only 3x it fails.
    assert!(best.as_millis() < 60, "the ruleset took {best:?} to load");
}
