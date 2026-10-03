//! `basic-1`'s parameters (DESIGN.md P2.3.2, package 2-01a gates 1 and 4): the schema, the
//! generated struct and the engine's advisor agree; `clean()` answers 2-00b's clean-params
//! table; names resolve against the ruleset; and the fingerprint hashes what plays.

use std::sync::Arc;

use citar_bot::params::{self, BY_KEY, COUNT, Kind, SPECS};
use citar_bot::{BotSpec, NameList, Overrides, Params, Tuning, VersionId, clean, fingerprint};
use citar_engine::base::ids::{BaseUnitId, PromotionId};
use citar_engine::game::advisor::{AdvisorParams, GarrisonMode, ProductionMode};
use citar_engine::rules::Ruleset;
use serde::Deserialize;
use serde_json::{Map, Value, json};

fn schema() -> Value {
    serde_json::from_str(citar_bot::schema("basic-1").expect("basic-1's schema")).expect("JSON")
}

/// The schema's parameters, in its order.
fn schema_params() -> Vec<Value> {
    let doc = schema();
    let groups = doc["groups"].as_array().expect("groups").clone();
    groups.iter().flat_map(|g| g["params"].as_array().cloned().unwrap_or_default()).collect()
}

// ---- The schema, the struct and the table ----------------------------------------------------

#[test]
fn the_schema_is_the_file_verbatim_and_the_table_follows_it() {
    let file = include_str!("../../../citar-bot/params/basic-1.json");
    assert_eq!(citar_bot::schema("basic-1"), Ok(file));
    assert_eq!(citar_bot::schema("basic"), Ok(file));
    let doc = schema();
    assert_eq!(doc["engine"], "basic-1");
    assert_eq!(doc["groups"].as_array().map(Vec::len), Some(17));
    let specs = schema_params();
    assert_eq!(specs.len(), 373);
    assert_eq!(COUNT, 373);
    for (s, j) in SPECS.iter().zip(&specs) {
        assert_eq!(j["key"], s.key);
        assert_eq!(j["type"], s.kind.name(), "{}", s.key);
        assert_eq!(j["label"], s.label, "{}", s.key);
        assert_eq!(j["default"], s.default.to_json(), "{}", s.key);
        assert_eq!(params::spec(s.key).map(|x| x.key), Some(s.key));
    }
    let mut keys: Vec<&str> = SPECS.iter().map(|s| s.key).collect();
    keys.sort();
    assert_eq!(keys, BY_KEY.iter().map(|(k, _)| *k).collect::<Vec<_>>());
    assert!(params::spec("site_cache_turns").is_none(), "a retired key");
    assert!(params::spec("bv_cache_turns").is_none(), "a retired key");
    // The kinds the design counts (P2.3.2, 2-00b's as-built note).
    let count = |k: Kind| SPECS.iter().filter(|s| s.kind == k).count();
    let kinds =
        [Kind::Int, Kind::Float, Kind::Choice, Kind::Bool, Kind::Order, Kind::List].map(count);
    assert_eq!(kinds, [179, 168, 9, 8, 7, 2]);
}

/// Gate 1: the effective defaults deserialize into `Params`, and equal the generated
/// `Params::schema_defaults()`.
#[test]
fn the_defaults_deserialize_into_params() {
    let map = Value::Object(params::defaults());
    let p = Params::deserialize(&map).expect("the defaults read");
    assert_eq!(p, Params::schema_defaults());
    assert_eq!(Tuning::new(VersionId::Basic1, Overrides::default()).params(), &p);
    // Some fields by their schema's values.
    assert_eq!(p.counter_rounds, 4);
    assert!((p.tech_noise - 0.1).abs() < 1e-12);
    assert!(!p.lux_buy && p.faith_buildings);
    assert_eq!(p.small_city_focus, None);
    assert_eq!(p.tech_mode, params::TechMode::Classic);
    assert_eq!(p.free_gp_early, params::FreeGpEarly::GreatScientist);
    assert_eq!(p.policy_order_peaceful, NameList::Preset("rationalism_early".into()));
    assert_eq!(p.policy_order_aggressive, NameList::Default);
    assert_eq!(p.promo_in_city, NameList::Names(vec!["Cover".into()]));
}

/// Gate 1: a key missing from the struct or from the schema fails: the struct refuses a key it
/// lacks and asks for every one it has.
#[test]
fn a_key_missing_from_either_side_fails() {
    let defaults = params::defaults();
    for s in &SPECS {
        let mut map = defaults.clone();
        map.shift_remove(s.key);
        let e = Params::deserialize(&Value::Object(map)).expect_err("a field the map lacks");
        assert!(e.to_string().contains(s.key), "{}: {e}", s.key);
    }
    let mut map = defaults;
    map.insert("site_cache_turns".to_owned(), json!(0));
    let e = Params::deserialize(&Value::Object(map)).expect_err("a key the struct lacks");
    assert!(e.to_string().contains("site_cache_turns"), "{e}");
}

/// Gate 1: the advisor's parameters from basic-1's defaults are `AdvisorParams::default()`,
/// which 1c-07 checked against Python field by field.
#[test]
fn the_advisors_parameters_from_the_defaults_are_its_own() {
    let map = Value::Object(params::defaults());
    let a = AdvisorParams::from_map(&map, 0.4).expect("the advisor reads the defaults");
    assert_eq!(a, AdvisorParams::default());
    let t = Tuning::new(VersionId::Basic1, Overrides::default());
    assert_eq!(t.advisor(0.4), AdvisorParams::default());
    assert_eq!(t.advisor(0.25), AdvisorParams::auto_production());
    // An override reaches it.
    let o = clean(
        "basic-1",
        &json!({"prod_mode": "classic", "garrison_mode": "exposed",
                                     "c_danger": 900, "site_radius": 7}),
    )
    .expect("clean");
    let a = Tuning::new(VersionId::Basic1, o).advisor(0.4);
    assert_eq!(a.prod_mode, ProductionMode::Classic);
    assert_eq!(a.garrison_mode, GarrisonMode::Exposed);
    assert!((a.c_danger - 900.0).abs() < 1e-12);
    assert_eq!(a.site_radius, 7);
}

/// Every field of the advisor's parameters is held to its schema type (2-00b's
/// `tests/test_bot_params.py` holds the same on the Python side): the keys it reads are found by
/// taking each away; a float key reads a fraction, an int key refuses one, a choice key refuses
/// a name its enum lacks.
#[test]
fn every_advisor_field_has_its_schema_type() {
    let defaults = params::defaults();
    let read = |m: &Map<String, Value>| AdvisorParams::from_map(&Value::Object(m.clone()), 0.4);
    let mut fields = 0;
    for s in &SPECS {
        let mut without = defaults.clone();
        without.shift_remove(s.key);
        if read(&without).is_ok() {
            continue;
        }
        fields += 1;
        let with = |v: Value| {
            let mut m = defaults.clone();
            m.insert(s.key.to_owned(), v);
            read(&m)
        };
        match s.kind {
            Kind::Float => assert!(with(json!(0.5)).is_ok(), "{} is an f64", s.key),
            Kind::Int => assert!(with(json!(0.5)).is_err(), "{} is an integer", s.key),
            Kind::Choice => assert!(with(json!("no such mode")).is_err(), "{} is an enum", s.key),
            Kind::Bool => assert!(with(json!(1)).is_err(), "{} is a bool", s.key),
            Kind::Order | Kind::List => panic!("the advisor reads no names ({})", s.key),
        }
    }
    // Every field (149, `c_escort` since package 2-01b) but the aggression, which is the
    // seat's.
    assert_eq!(fields, 148, "the advisor's parameters read from the map");
    // A negative count or radius, which no editor offers, reads as 0.
    let mut m = defaults;
    m.insert("site_radius".to_owned(), json!(-3));
    assert_eq!(read(&m).map(|a| a.site_radius).ok(), Some(0));
}

// ---- clean() against 2-00b's table ---------------------------------------------------------

/// Gate 1: `clean()` answers every case of `tests/data/clean_params_cases.json` as Python's
/// `clean_params` did, or as the case's `basic1` says where basic-1 differs on purpose (the
/// table's fixes: int-integral, names-known, dropped, retyped).
#[test]
fn clean_matches_the_clean_params_table() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/data/clean_params_cases.json");
    #[allow(clippy::disallowed_methods, reason = "the table is a file")]
    let text = std::fs::read_to_string(path).expect("the table");
    let doc: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(doc["version"], "basic-1");
    let engine = doc["engine"].as_str().expect("the engine's name");
    let fixes = doc["fixes"].as_object().expect("fixes");
    assert_eq!(
        fixes.keys().map(String::as_str).collect::<Vec<_>>(),
        ["int-integral", "names-known", "dropped", "retyped"]
    );
    let cases = doc["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), 80);
    let mut fixed = 0;
    for c in cases {
        let name = c["name"].as_str().expect("a name");
        let want = match c.get("fix") {
            Some(fix) => {
                assert!(fixes.contains_key(fix.as_str().unwrap_or_default()), "{name}");
                fixed += 1;
                &c["basic1"]
            }
            None => c,
        };
        let got = clean(engine, &c["params"]);
        match (want.get("out"), want.get("error"), got) {
            (Some(out), None, Ok(o)) => {
                assert_eq!(&o.to_json(), out, "{name}");
                // What clean() gives always reads (Tuning::new relies on it).
                let map = Value::Object(o.effective());
                assert!(Params::deserialize(&map).is_ok(), "{name}");
                assert!(AdvisorParams::from_map(&map, 0.4).is_ok(), "{name}");
            }
            (None, Some(err), Err(e)) => assert_eq!(Value::from(e.message), *err, "{name}"),
            (out, err, got) => panic!("{name}: want out {out:?} or error {err:?}, got {got:?}"),
        }
    }
    assert_eq!(fixed, 8, "the table's fix cases, two of each");
    // Under its own name the version is named as asked.
    let e = clean("basic-1", &json!({"bogus": 1})).expect_err("unknown");
    assert_eq!(e.message, "bogus is not a parameter of basic-1.");
    assert_eq!(e.key.as_deref(), Some("bogus"));
}

/// What the table does not ask: numbers clean() refuses beyond Python's refusals, and the keys'
/// order.
#[test]
fn clean_refuses_what_params_cannot_hold() {
    let refused = |v: Value| clean("basic-1", &json!({"counter_rounds": v})).is_err();
    assert!(refused(json!(3_000_000_000_i64)), "outside i32");
    assert!(refused(json!("1e10")), "outside i32");
    assert!(refused(json!("inf")) && refused(json!("nan")), "not finite");
    assert!(!refused(json!(-2_147_483_648_i64)), "i32::MIN fits");
    assert!(!refused(json!("1_000")), "Python's underscores between digits");
    let float = |v: Value| clean("basic-1", &json!({"tech_noise": v}));
    assert!(float(json!("inf")).is_err() && float(json!("-nan")).is_err(), "not finite");
    assert!(float(json!(1e300)).is_ok());
    // Not an object.
    for bad in [json!([1]), json!("x"), json!(3), json!(true)] {
        let e = clean("basic-1", &bad).expect_err("not an object");
        assert_eq!(e.message, "Bot parameters are an object of {key: value}.");
    }
    // Keys come back sorted, whatever order they were given in.
    let o = clean("basic-1", &json!({"tech_noise": 0, "counter_rounds": 2, "lux_buy": true}))
        .expect("clean");
    assert_eq!(
        o.iter().map(|(k, _)| k).collect::<Vec<_>>(),
        ["counter_rounds", "lux_buy", "tech_noise"]
    );
    assert_eq!(o.canonical(), r#"{"counter_rounds":2,"lux_buy":true,"tech_noise":0.0}"#);
    // The idle bot ignores everything, as Python's did.
    assert_eq!(clean("idle", &json!({"tech_noise": "x", "bogus": [1]})), Ok(Overrides::default()));
}

/// Overrides overlay the defaults and come through as typed values.
#[test]
fn overrides_reach_the_struct() {
    let o = clean(
        "basic",
        &json!({"counter_rounds": "6", "tech_mode": "potential", "small_city_focus": "food",
                "policy_order_aggressive": "liberty_first", "promo_lines": ["Drill"],
                "beliefs_pantheon": "default", "policy_order_peaceful": null,
                "mil_war_mult": 4}),
    )
    .expect("clean");
    let t = Tuning::new(VersionId::Basic1, o);
    let p = t.params();
    assert_eq!(p.counter_rounds, 6);
    assert_eq!(p.tech_mode, params::TechMode::Potential);
    assert_eq!(p.small_city_focus, Some(params::SmallCityFocus::Food));
    assert_eq!(p.policy_order_aggressive, NameList::Preset("liberty_first".into()));
    assert_eq!(p.promo_lines, NameList::Names(vec!["Drill".into()]));
    assert_eq!(p.beliefs_pantheon, NameList::Default);
    assert_eq!(p.policy_order_peaceful, NameList::Default);
    assert!((p.mil_war_mult - 4.0).abs() < 1e-12);
    assert!((t.advisor(0.4).mil_war_mult - 4.0).abs() < 1e-12);
    // The idle bot's tuning keeps no overrides.
    let idle = Tuning::new(VersionId::Idle, Overrides::default());
    assert!(idle.overrides().is_empty());
    assert_eq!(idle.version(), VersionId::Idle);
}

// ---- Resolution ----------------------------------------------------------------------------

fn names<I: citar_engine::rules::Named + Copy>(ids: &[I]) -> Vec<String> {
    let r = Ruleset::shared();
    ids.iter().map(|&id| r.name(id).unwrap_or("?").to_owned()).collect()
}

#[test]
fn the_defaults_resolve_against_the_shipped_ruleset() {
    let t = Tuning::new(VersionId::Basic1, Overrides::default());
    let r = t.resolved(Ruleset::shared());
    assert!(Arc::ptr_eq(&r, &t.resolved(Ruleset::shared())), "resolved once per ruleset");
    // `policy_order_peaceful` defaults to the preset rationalism_early; the aggressive order to
    // the built-in one (its `default` preset).
    assert_eq!(
        names(&r.policy_order_peaceful),
        [
            "Tradition",
            "Liberty",
            "Rationalism",
            "Patronage",
            "Commerce",
            "Piety",
            "Freedom",
            "Order",
            "Honor",
            "Autocracy"
        ]
    );
    assert_eq!(
        names(&r.policy_order_aggressive),
        [
            "Honor",
            "Tradition",
            "Liberty",
            "Commerce",
            "Rationalism",
            "Autocracy",
            "Patronage",
            "Piety",
            "Order",
            "Freedom"
        ]
    );
    assert_eq!(names(&r.beliefs_founder)[..2], ["Ceremonial Burial", "Tithe"]);
    assert_eq!(r.beliefs_pantheon.len(), 10);
    assert_eq!(r.beliefs_follower.len(), 16);
    assert_eq!(r.beliefs_enhancer.len(), 9);
    let gp = |id: Option<BaseUnitId>| id.and_then(|i| Ruleset::shared().name(i)).map(str::to_owned);
    assert_eq!(gp(r.free_gp_early).as_deref(), Some("Great Scientist"));
    assert_eq!(gp(r.free_gp_late).as_deref(), Some("Great Engineer"));
    // Promotion lines take every promotion whose name starts with one: Cover I and II.
    let promos = |set: &citar_engine::base::sets::PromotionSet| -> Vec<String> {
        names(&set.iter().collect::<Vec<PromotionId>>())
    };
    assert_eq!(promos(&r.promo_in_city), ["Cover I", "Cover II"]);
    assert!(promos(&r.promo_lines).iter().all(|n| !n.starts_with("Cover")));
    assert!(promos(&r.promo_lines).iter().any(|n| n == "Shock I"));
    // The shipped ruleset has no pantheon called Tradition, which `pantheon_unciv` lists (as
    // Python's PANTHEON_PREFS did): skipped and counted.
    assert_eq!(r.pantheon_unciv.len(), 8);
    assert!(!names(&r.pantheon_unciv).iter().any(|n| n == "Tradition"));
    assert_eq!(r.unknown_names, 1);
}

#[test]
fn name_lists_resolve_as_written_and_unknown_names_are_counted() {
    let o = clean(
        "basic-1",
        &json!({"policy_order_peaceful": "default", "policy_order_aggressive": [],
                "beliefs_founder": ["Tithe", "Pilgrimage"], "free_gp_early": "Great Writer",
                "promo_in_city": null, "beliefs_pantheon": []}),
    )
    .expect("clean");
    let t = Tuning::new(VersionId::Basic1, o);
    let r = t.resolved(Ruleset::shared());
    // "default": the built-in peaceful order (Python's POLICY_ORDER["peaceful"]).
    assert_eq!(names(&r.policy_order_peaceful)[..3], ["Tradition", "Liberty", "Piety"]);
    // An empty order is the default one, as Python's `order or POLICY_ORDER[...]` made it.
    assert_eq!(names(&r.policy_order_aggressive)[..2], ["Honor", "Tradition"]);
    assert_eq!(names(&r.beliefs_founder), ["Tithe", "Pilgrimage"]);
    // An empty belief order stays empty; 2-01b gives it Python's meaning where it is used (all
    // four orders in turn in `choose_beliefs`, the first available pantheon in `empire_choices`).
    assert!(r.beliefs_pantheon.is_empty());
    // null for a list without presets: its default names.
    assert_eq!(r.promo_in_city.iter().count(), 2);
    // The shipped ruleset has no Great Writer unit: skipped and counted, with Tradition.
    assert_eq!(r.free_gp_early, None);
    assert_eq!(r.unknown_names, 2);
}

/// `choose_beliefs`' ranks (basic.py:1707), worked out once per ruleset: a place counts in the
/// order as written, and a kind written empty, or `Any`, ranks by the four orders together.
#[test]
fn belief_places_count_each_order_as_written() {
    use citar_engine::base::ids::BeliefId;
    use citar_engine::rules::defs::{BeliefKind, BeliefType};
    let o = clean(
        "basic-1",
        &json!({"beliefs_pantheon": ["Tradition", "Fertility Rites"], "beliefs_founder": [],
                "beliefs_follower": ["Pagodas"], "beliefs_enhancer": ["Messiah"]}),
    )
    .expect("clean");
    let t = Tuning::new(VersionId::Basic1, o);
    let r = t.resolved(Ruleset::shared());
    let place = |kind: BeliefKind, name: &str| {
        r.belief_place(kind, Ruleset::shared().lookup::<BeliefId>(name).expect(name))
    };
    let [pantheon, founder, follower, _] = BeliefType::ALL.map(BeliefKind::Type);
    // Tradition, which the ruleset lacks, keeps its place, as Python's `index` counted it.
    assert_eq!(place(pantheon, "Fertility Rites"), Some(1));
    assert_eq!(place(pantheon, "Goddess of Love"), None);
    // A kind's own order is its alone.
    assert_eq!(place(follower, "Pagodas"), Some(0));
    assert_eq!(place(follower, "Messiah"), None);
    // The founder order was written empty: the four orders one after another.
    for kind in [founder, BeliefKind::Any] {
        assert_eq!(place(kind, "Fertility Rites"), Some(1));
        assert_eq!(place(kind, "Pagodas"), Some(2));
        assert_eq!(place(kind, "Messiah"), Some(3));
        assert_eq!(place(kind, "Tithe"), None);
    }
}

// ---- Fingerprints (gate 4) -----------------------------------------------------------------

fn spec(version: VersionId, overrides: &Value, fixed: Option<f64>, seat: Option<f64>) -> BotSpec {
    let o = clean(version.id(), overrides).expect("clean");
    BotSpec::new(version, Arc::new(Tuning::new(version, o)), fixed, seat)
}

/// Gate 4: two lab seats of one profile at different positions (the lab spreads the seat's
/// aggression by position) share a fingerprint; a different fixed aggression, override, version
/// or build id changes it.
#[test]
fn a_fingerprint_hashes_the_profile_not_the_seat() {
    let profile = json!({"counter_rounds": 2, "tech_mode": "potential"});
    let build = "0123456789ab";
    let seat_a = spec(VersionId::Basic1, &profile, None, Some(0.2));
    let seat_b = spec(VersionId::Basic1, &profile, None, Some(0.9));
    let f = fingerprint(&seat_a, build);
    assert_eq!(f.len(), 12);
    assert!((seat_a.aggression - seat_b.aggression).abs() > 0.5, "the seats play differently");
    assert_eq!(fingerprint(&seat_b, build), f, "one profile, one fingerprint");
    // The same overrides written another way clean to the same profile.
    let same = spec(
        VersionId::Basic1,
        &json!({"tech_mode": "potential", "counter_rounds": "2",
                                               "tech_noise": 0.1}),
        None,
        Some(0.5),
    );
    assert_eq!(fingerprint(&same, build), f, "defaults dropped, keys sorted, values typed");
    let differs = [
        (
            "a fixed aggression",
            fingerprint(&spec(VersionId::Basic1, &profile, Some(0.5), None), build),
        ),
        (
            "another fixed aggression",
            fingerprint(&spec(VersionId::Basic1, &profile, Some(0.6), None), build),
        ),
        (
            "another override",
            fingerprint(
                &spec(
                    VersionId::Basic1,
                    &json!({"counter_rounds": 3, "tech_mode": "potential"}),
                    None,
                    None,
                ),
                build,
            ),
        ),
        (
            "one override fewer",
            fingerprint(&spec(VersionId::Basic1, &json!({"counter_rounds": 2}), None, None), build),
        ),
        ("another version", fingerprint(&spec(VersionId::Idle, &profile, None, None), build)),
        ("another build", fingerprint(&seat_a, "ba9876543210")),
    ];
    let mut seen = vec![f.clone()];
    for (what, other) in differs {
        assert!(!seen.contains(&other), "{what} must change the fingerprint");
        seen.push(other);
    }
    // A fixed aggression is hashed as it plays, to three decimals.
    assert_eq!(
        fingerprint(&spec(VersionId::Basic1, &profile, Some(0.5), Some(0.1)), build),
        fingerprint(&spec(VersionId::Basic1, &profile, Some(0.5001), Some(0.8)), build),
    );
}
