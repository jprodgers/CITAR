//! The unique compiler (package 1a-05, DESIGN.md 5.4-5.6) against its contract:
//! - gate 1: every one of the 1,615 shipped unique texts compiles (the snapshot of what they
//!   compile to is the golden `uniques.json`, checked in `tests/determinism.rs`);
//! - gate 2: an unknown text, an unknown tag nothing names, a bad stat, an out-of-range amount,
//!   two triggers and a `for every` multiplier are each refused, naming the file and object;
//! - gate 3: a `Unique` is 24 bytes, within the 32 the design allows;
//! - gate 4: the same text on two sources has two keys, and adding a unique moves no other key.
//!
//! The refcheck `uniques` group (gate 6) compares the same compile with Python's reading of each
//! text; gate 5 (gen.rs fresh) is `cargo xtask check`'s.

use std::collections::BTreeSet;

use citar_engine::base::ids::{
    BaseUnitId, BeliefId, BuildingId, NationId, PolicyId, PromotionId, ResourceId, TechId,
    UniqueId, UnitTypeId,
};
use citar_engine::base::stats::Stat;
use citar_engine::rules::{Ruleset, RulesetErrorKind};
use citar_engine::unique::params::{Param, ParamValue};
use citar_engine::unique::{
    ActionMods, Role, Source, SourceUniques, UFlags, Unique, UniqueData, UniqueType,
};
use serde_json::{Value, json};

use super::rules::{load_edited, refused, shipped};

const BUILDINGS: &str = "ruleset/buildings.json";
const UNITS: &str = "ruleset/units.json";

/// Every object's uniques, with the object's kind and name.
fn named_sources(r: &Ruleset) -> Vec<(&'static str, &str, &SourceUniques)> {
    let mut out: Vec<(&'static str, &str, &SourceUniques)> = Vec::new();
    out.extend(r.nations().as_slice().iter().map(|x| ("Nation", &*x.name, &x.uniques)));
    out.extend(r.buildings().as_slice().iter().map(|x| ("Building", &*x.name, &x.uniques)));
    out.extend(r.policies().as_slice().iter().map(|x| ("Policy", &*x.name, &x.uniques)));
    out.extend(r.techs().as_slice().iter().map(|x| ("Tech", &*x.name, &x.uniques)));
    out.extend(r.eras().as_slice().iter().map(|x| ("Era", &*x.name, &x.uniques)));
    for c in r.city_state_types().as_slice() {
        out.extend([
            ("CityStateFriend", &*c.name, &c.friend),
            ("CityStateAlly", &*c.name, &c.ally),
            ("CityStateType", &*c.name, &c.uniques),
        ]);
    }
    out.extend(r.beliefs().as_slice().iter().map(|x| ("Belief", &*x.name, &x.uniques)));
    out.extend(r.resources().as_slice().iter().map(|x| ("Resource", &*x.name, &x.uniques)));
    out.push(("Global", "", r.global_uniques()));
    out.extend(r.terrains().as_slice().iter().map(|x| ("Terrain", &*x.name, &x.uniques)));
    out.extend(r.improvements().as_slice().iter().map(|x| ("Improvement", &*x.name, &x.uniques)));
    out.extend(r.unit_types().as_slice().iter().map(|x| ("UnitType", &*x.name, &x.uniques)));
    out.extend(r.base_units().as_slice().iter().map(|x| ("Unit", &*x.name, &x.uniques)));
    out.extend(r.promotions().as_slice().iter().map(|x| ("Promotion", &*x.name, &x.uniques)));
    out.extend(r.ruins().as_slice().iter().map(|x| ("Ruins", &*x.name, &x.uniques)));
    out
}

/// Every object's uniques.
fn all_sources(r: &Ruleset) -> Vec<&SourceUniques> {
    named_sources(r).into_iter().map(|(_, _, u)| u).collect()
}

/// The first unique of `u` whose text is `text`.
fn by_text(r: &Ruleset, u: &SourceUniques, text: &str) -> UniqueId {
    u.ids()
        .find(|&id| r.uniques().text_of(id) == text)
        .unwrap_or_else(|| panic!("no unique {text:?}"))
}

/// Adds `text` to the uniques of `object` in a file whose objects are keyed at the top level.
fn with_unique(
    file: &str,
    object: &str,
    text: &str,
) -> Result<Ruleset, citar_engine::rules::RulesetErrors> {
    load_edited(file, |v| {
        let list = v[object]["uniques"].as_array_mut().expect("a uniques list");
        list.push(json!(text));
    })
}

// ---- Gate 1: every shipped text compiles ---------------------------------------------------------

#[test]
fn every_shipped_text_compiles() {
    let r = shipped();
    let t = r.uniques();
    let texts: usize = all_sources(r).iter().map(|u| u.all.len()).sum();
    assert_eq!(texts, 1615, "DESIGN.md 5.1");
    // One more: the variant of the one timed unique, without its timer.
    assert_eq!(t.len(), 1616);
    let temporary: Vec<UniqueId> =
        t.iter().filter(|(_, u)| u.flags().contains(UFlags::TEMPORARY)).map(|(id, _)| id).collect();
    assert_eq!(temporary.len(), 1);
    let distinct: BTreeSet<&str> = t.iter().map(|(id, _)| t.text_of(id)).collect();
    assert_eq!(distinct.len(), 954, "DESIGN.md 5.1");
    let main: BTreeSet<UniqueType> = t.iter().filter_map(|(id, _)| t.meta(id).ty).collect();
    assert_eq!(main.len(), 342, "types used as uniques (DESIGN.md 5.1)");
    let mods: BTreeSet<UniqueType> =
        t.iter().flat_map(|(id, _)| t.modifiers(id)).map(|(ty, _)| ty).collect();
    assert_eq!(mods.len(), 60, "types used as modifiers (DESIGN.md 5.1)");
    // The one text UnCiv does not know is the Aircraft tag.
    let unknown: BTreeSet<&str> =
        t.iter().filter(|(id, _)| t.meta(*id).ty.is_none()).map(|(id, _)| t.text_of(id)).collect();
    assert_eq!(unknown.into_iter().collect::<Vec<_>>(), ["Aircraft"]);
    // Every object's uniques are in order and in no other object's.
    let mut seen = BTreeSet::new();
    for u in all_sources(r) {
        for id in u.ids() {
            assert!(seen.insert(id), "{id:?} is two objects'");
        }
    }
    assert_eq!(seen.len(), 1615);
}

#[test]
fn ids_follow_civ_umaps_order() {
    let r = shipped();
    let t = r.uniques();
    let sources: Vec<Source> = t.iter().map(|(id, _)| t.meta(id).source).collect();
    assert!(sources.windows(2).all(|w| w[0] <= w[1]), "sorting by id is canonical");
    // Nations first, then buildings, policies and techs, the timed unique's variant, then eras.
    assert!(matches!(sources[0], Source::Nation(_)));
    let temp = sources.iter().position(|s| matches!(s, Source::Temporary(_))).expect("a variant");
    assert!(matches!(sources[temp - 1], Source::Tech(_)));
    assert!(matches!(sources[temp + 1], Source::Era(_)));
    assert!(matches!(sources.last(), Some(Source::Ruins(_))));
}

#[test]
fn a_timed_unique_has_a_variant_without_its_timer() {
    let r = shipped();
    let t = r.uniques();
    let branch = r.lookup::<PolicyId>("Autocracy Complete").expect("a policy");
    let id = by_text(
        r,
        &r.policies()[branch].uniques,
        "[+25]% Strength <when attacking> <for [Military] units> <for [50] turns>",
    );
    let (u, m) = (t.get(id), t.meta(id));
    assert_eq!(m.timed, Some(50));
    assert!(u.flags().contains(UFlags::TIMED));
    assert_eq!(t.conds(u).len(), 2, "the timer is no conditional");
    let v = m.temp_variant.expect("a variant");
    let (vu, vm) = (t.get(v), t.meta(v));
    assert_eq!(vm.source, Source::Temporary(id));
    assert_eq!(vm.timed, None);
    assert_eq!(vu.data, u.data);
    assert_eq!(vu.conds, u.conds, "the same conditionals");
    assert_eq!(vu.flags(), UFlags::TEMPORARY);
    assert_ne!(vm.key, m.key);
    // Granted when its source is gained, never standing.
    assert!(r.policies()[branch].uniques.on_gain.contains(&id));
    assert!(!r.policies()[branch].uniques.civ.contains(&id));
}

#[test]
fn aircraft_is_a_tag_that_filters_name() {
    let r = shipped();
    let t = r.uniques();
    let tag = t.tag_named("Aircraft").expect("a tag");
    for name in ["Fighter", "Bomber", "Atomic Bomber"] {
        let ty = &r.unit_types()[r.lookup::<UnitTypeId>(name).expect("a unit type")];
        assert!(ty.uniques.tags.contains(tag), "{name}");
        let id = by_text(r, &ty.uniques, "Aircraft");
        assert_eq!(t.get(id).data, UniqueData::Tag(tag));
        assert_eq!(t.meta(id).role, Role::Tag);
    }
    // A unit carries its type's tags, as Python's unit map held its type's uniques
    // (rules.py:116-118), and the uniques stay on the type.
    let unit = |name: &str| &r.base_units()[r.lookup::<BaseUnitId>(name).expect("a unit")];
    for name in ["Fighter", "Jet Fighter", "Bomber", "Atomic Bomb", "Guided Missile", "Warrior"] {
        let u = unit(name);
        let of_type = &r.unit_types()[u.unit_type].uniques;
        assert_eq!(u.uniques.tags.contains(tag), of_type.tags.contains(tag), "{name}");
        assert!(u.uniques.ids().all(|id| t.text_of(id) != "Aircraft"), "{name}");
    }
    assert!(unit("Fighter").uniques.tags.contains(tag), "the Fighter unit is Aircraft");
    assert!(unit("Atomic Bomb").uniques.tags.contains(tag));
    assert!(!unit("Guided Missile").uniques.tags.contains(tag), "a missile is Air, not Aircraft");
    assert!(!unit("Warrior").uniques.tags.contains(tag));
    // Known texts without parameters that filters name are tags of their sources too.
    let hill = &r.terrains()[r.derived().features[r.derived().known.hill]];
    let rough = t.tag_named("Rough terrain").expect("filters name rough terrain");
    assert!(hill.uniques.tags.contains(rough));
    assert!(t.tag_count() < 256);
}

#[test]
fn relevant_units_are_those_that_can_take_the_promotion() {
    let r = shipped();
    let t = r.uniques();
    let alhambra = r.lookup::<BuildingId>("Alhambra").expect("a building");
    let id = by_text(
        r,
        &r.buildings()[alhambra].uniques,
        "All newly-trained [relevant] units [in this city] receive the [Drill I] promotion",
    );
    let UniqueData::UnitStartingPromotions(p) = t.get(id).data else { panic!("the type") };
    let drill = r.lookup::<PromotionId>("Drill I").expect("a promotion");
    assert_eq!(p.promotion, drill);
    let members = t.set(p.units).members.clone().expect("decided by the compiler");
    let types = &r.promotions()[drill].unit_types;
    for (u, def) in r.base_units().iter() {
        let index = u32::from(u.0);
        assert_eq!(members.contains(index), types.contains(&def.unit_type), "{}", def.name);
    }
    assert!(t.get(id).flags().contains(UFlags::LOCAL));
}

#[test]
fn uniques_are_split_by_what_they_do() {
    let r = shipped();
    let t = r.uniques();
    // A one-time effect with a trigger fires; a standing effect stands.
    let babylon = &r.nations()[r.lookup::<NationId>("Babylon").expect("a nation")].uniques;
    let free = by_text(
        r,
        babylon,
        "Free [Great Scientist] appears <upon discovering [Writing] technology>",
    );
    assert_eq!(&*babylon.triggered, [free]);
    assert!(matches!(
        t.meta(free).trigger,
        Some(citar_engine::unique::TriggerCond::TriggerUponResearch(_))
    ));
    assert_eq!(babylon.civ.len(), 1);
    // A building's own city: local, and also granted when built.
    let library = r.lookup::<BuildingId>("The Great Library").expect("a building");
    let gl = &r.buildings()[library].uniques;
    let free_library = by_text(r, gl, "Gain a free [Library] [in this city]");
    assert!(gl.local.contains(&free_library) && gl.on_gain.contains(&free_library));
    assert!(!gl.civ.contains(&free_library));
    // The Marble fix: a resource's unique in this city is local (DESIGN.md 5.12).
    let marble = &r.resources()[r.lookup::<ResourceId>("Marble").expect("a resource")].uniques;
    assert_eq!(marble.local.len(), 1);
    // Only buildings and resources split: a belief's `in this city` is the city in context, so
    // its unique stands in the follower's or founder's index, keeping its LOCAL bit
    // (religion.py:59-81 read beliefs whole; uniques.py:668).
    for (belief, text) in [
        ("Fertility Rites", "[+10]% growth [in this city]"),
        ("Dance of the Aurora", "[+1 Faith] from [Tundra] tiles without [Forest] [in this city]"),
        ("Swords into Ploughshares", "[+15]% growth [in this city] <when not at war>"),
    ] {
        let b = &r.beliefs()[r.lookup::<BeliefId>(belief).expect("a belief")].uniques;
        let id = by_text(r, b, text);
        assert!(t.get(id).flags().contains(UFlags::LOCAL), "{belief}");
        assert!(b.civ.contains(&id) && b.local.is_empty(), "{belief}");
    }
    for (kind, name, u) in named_sources(r) {
        if !matches!(kind, "Building" | "Resource") {
            assert!(u.local.is_empty(), "{kind} {name} has local uniques");
        }
    }
    // A unit action and its limits.
    let prophet = &r.base_units()[r.lookup::<BaseUnitId>("Great Prophet").expect("a unit")].uniques;
    let spread = by_text(
        r,
        prophet,
        "Can Spread Religion <for [2] movement> <[4] times> <after which this unit is consumed>",
    );
    assert!(prophet.actions.contains(&spread));
    assert_eq!(
        t.meta(spread).actions,
        ActionMods {
            consume: false,
            consumed_after: true,
            once: false,
            times: Some(4),
            extra_times: None,
            movement: Some(2)
        }
    );
    let ability = t.meta(spread).ability.expect("a limited action has an ability");
    assert_eq!(t.ability(ability), "Can Spread Religion|", "Python's key (units.py:417)");
    // AI weights.
    let writing = &r.techs()[r.lookup::<TechId>("Writing").expect("a tech")].uniques;
    assert!(writing.ai.iter().all(|&id| t.meta(id).ty == Some(UniqueType::AiChoiceWeight)));
    // Every unique is in at most one of the standing partitions.
    for u in all_sources(r) {
        for id in u.ids() {
            let standing = [&u.civ, &u.local].iter().filter(|p| p.contains(&id)).count();
            assert!(standing <= 1);
        }
    }
}

#[test]
fn parameters_compile_by_kind() {
    let r = shipped();
    let t = r.uniques();
    let value = |p: Param| p.value(r);
    let colosseum = r.lookup::<BuildingId>("Colosseum").expect("a building");
    for id in r.buildings()[colosseum].uniques.ids() {
        for p in t.get(id).data.params() {
            assert!(
                !matches!(value(p), ParamValue::Text(ref s) if s == "?"),
                "every id names something"
            );
        }
    }
    // Fractions are the correctly rounded doubles, interned once.
    let fracs = r.fracs().as_slice();
    let distinct: BTreeSet<u64> = fracs.iter().map(|x| x.to_bits()).collect();
    assert_eq!(distinct.len(), fracs.len());
    assert!(fracs.iter().any(|&x| x.to_bits() == 0.6_f64.to_bits()));
    // Stats are interned: 44 distinct (DESIGN.md 5.5), each once.
    let stats = t.all_stats().as_slice();
    assert_eq!(stats.len(), 44);
    for (i, a) in stats.iter().enumerate() {
        assert!(stats[..i].iter().all(|b| a.0.map(f64::to_bits) != b.0.map(f64::to_bits)));
    }
    assert!(stats.iter().any(|s| s[Stat::Culture] > 0.0));
    // A countable and its filter.
    let mut countables = 0;
    for (_, c) in t.all_conds().iter() {
        for p in c.data.params() {
            if let Param::Countable(_) = p {
                countables += 1;
                assert!(matches!(value(p), ParamValue::Text(_)));
            }
        }
    }
    assert_eq!(countables, 12);
}

#[test]
fn a_building_named_by_a_parameter_resolves() {
    let r = shipped();
    let t = r.uniques();
    let carthage = &r.nations()[r.lookup::<NationId>("Carthage").expect("a nation")].uniques;
    let id = by_text(r, carthage, "Gain a free [Harbor] [in all coastal cities]");
    let UniqueData::GainFreeBuildings(p) = t.get(id).data else { panic!("the type") };
    assert_eq!(r.name(p.building), Some("Harbor"));
    assert_eq!(t.city_filter(p.cities), "in all coastal cities");
    assert!(!t.get(id).flags().contains(UFlags::LOCAL));
    assert_eq!(value_of(r, id), json!(["Harbor", "in all coastal cities"]));
}

fn value_of(r: &Ruleset, id: UniqueId) -> Value {
    let params: Vec<Value> = r
        .uniques()
        .get(id)
        .data
        .params()
        .into_iter()
        .map(|p| match p.value(r) {
            ParamValue::Int(n) => json!(n),
            ParamValue::Real(x) => json!(x),
            ParamValue::Stats(s) => json!(s.0),
            ParamValue::Text(s) => json!(s),
        })
        .collect();
    Value::Array(params)
}

// ---- Gate 2: what is refused --------------------------------------------------------------------

#[test]
fn an_unknown_text_is_refused() {
    let file = BUILDINGS;
    let text = refused(
        with_unique(file, "Monument", "[+1 Culture] from every Frobnicator"),
        RulesetErrorKind::UnknownUnique,
        file,
        "Monument",
    );
    assert!(
        text.contains("from every Frobnicator") && text.contains("no unique type UnCiv has"),
        "{text}"
    );
    // An unknown modifier too.
    let text = refused(
        with_unique(file, "Monument", "[+1 Culture] <when the moon is full>"),
        RulesetErrorKind::UnknownUnique,
        file,
        "Monument",
    );
    assert!(text.contains("the modifier <when the moon is full>"), "{text}");
}

#[test]
fn an_unknown_tag_nothing_names_is_refused() {
    let text = refused(
        with_unique(UNITS, "Warrior", "Stealthy"),
        RulesetErrorKind::UnknownUnique,
        UNITS,
        "Warrior",
    );
    assert!(text.contains("no filter names it as a tag"), "{text}");
    // The same text is a tag once a filter names it.
    let r = load_edited(UNITS, |v| {
        let list = v["Warrior"]["uniques"].as_array_mut().expect("uniques");
        list.push(json!("Stealthy"));
        list.push(json!("[+10]% Strength <vs [Stealthy] units>"));
    })
    .expect("a named tag loads");
    let tag = r.uniques().tag_named("Stealthy").expect("a tag");
    let warrior = r.lookup::<BaseUnitId>("Warrior").expect("a unit");
    assert!(r.base_units()[warrior].uniques.tags.contains(tag));
}

#[test]
fn a_tag_is_named_by_its_trimmed_text() {
    // Python matched a tag by the unique's placeholder, trimmed (has_tag, uniques.py:182-188).
    let r = with_unique(UNITS, "Warrior", "Aircraft ").expect("a stray space is still Aircraft");
    let t = r.uniques();
    let tag = t.tag_named("Aircraft").expect("a tag");
    assert_eq!(t.tag_count(), shipped().uniques().tag_count(), "no new tag");
    let warrior = &r.base_units()[r.lookup::<BaseUnitId>("Warrior").expect("a unit")].uniques;
    assert!(warrior.tags.contains(tag));
    let id = by_text(&r, warrior, "Aircraft ");
    assert_eq!(t.get(id).data, UniqueData::Tag(tag));
}

#[test]
fn a_failed_text_still_names_its_tags() {
    // The text that names the tag fails; the tag is not also reported as a typo.
    let errs = load_edited(UNITS, |v| {
        let list = v["Warrior"]["uniques"].as_array_mut().expect("uniques");
        list.push(json!("Stealthy"));
        list.push(json!("[+10]% Strength <vs [Stealthy] units> <for every [Cities]>"));
    })
    .expect_err("refused");
    assert_eq!(errs.0.len(), 1, "{errs}");
    assert!(errs.has(RulesetErrorKind::UniqueModifier), "{errs}");
}

#[test]
fn a_bad_stat_is_refused() {
    let file = BUILDINGS;
    let text = refused(
        with_unique(file, "Monument", "[+1 Culturre] [in this city]"),
        RulesetErrorKind::UniqueParameter,
        file,
        "Monument",
    );
    assert!(text.contains("parameter 1 [stats]") && text.contains("not stats"), "{text}");
    let text = refused(
        with_unique(file, "Monument", "[+15]% [Gould] [in this city]"),
        RulesetErrorKind::UniqueParameter,
        file,
        "Monument",
    );
    assert!(text.contains("not a stat"), "{text}");
}

#[test]
fn an_out_of_range_amount_is_refused() {
    let text = refused(
        with_unique(UNITS, "Warrior", "[+2000000]% Strength"),
        RulesetErrorKind::UniqueParameter,
        UNITS,
        "Warrior",
    );
    assert!(text.contains("out of range"), "{text}");
    let text = refused(
        with_unique(UNITS, "Warrior", "[1.5]% Strength"),
        RulesetErrorKind::UniqueParameter,
        UNITS,
        "Warrior",
    );
    assert!(text.contains("not a whole number"), "{text}");
    // A positive amount of 0.
    let text = refused(
        with_unique(UNITS, "Warrior", "Can carry [2] [Aircraft] units <[0] times>"),
        RulesetErrorKind::UniqueParameter,
        UNITS,
        "Warrior",
    );
    assert!(text.contains("out of range (1 to"), "{text}");
}

#[test]
fn two_triggers_are_refused() {
    let text = refused(
        load_edited("ruleset/nations.json", |v| {
            v["Babylon"]["uniques"].as_array_mut().expect("uniques").push(json!(
                "Free [Great Scientist] appears <upon discovering [Writing] technology> <upon \
                 entering the [Medieval era]>"
            ));
        }),
        RulesetErrorKind::UniqueModifier,
        "ruleset/nations.json",
        "Babylon",
    );
    assert!(text.contains("second trigger"), "{text}");
}

#[test]
fn a_for_every_multiplier_is_refused() {
    let file = BUILDINGS;
    let text = refused(
        with_unique(file, "Monument", "[+1 Gold] [in this city] <for every [Cities]>"),
        RulesetErrorKind::UniqueModifier,
        file,
        "Monument",
    );
    assert!(text.contains("multiplies the unique") && text.contains("uniques.py:1081"), "{text}");
}

#[test]
fn other_misplaced_uniques_are_refused() {
    let file = BUILDINGS;
    // A type UnCiv has that the engine does not support says what to add.
    let text = refused(
        with_unique(file, "Monument", "Nullifies [Gold] [in all cities]"),
        RulesetErrorKind::UnsupportedUnique,
        file,
        "Monument",
    );
    assert!(text.contains("NullifiesStat") && text.contains("unique_supported.toml"), "{text}");
    // A conditional written as a unique, and a unique written as a modifier.
    refused(
        with_unique(file, "Monument", "when attacking"),
        RulesetErrorKind::UniqueModifier,
        file,
        "Monument",
    );
    let text = refused(
        with_unique(file, "Monument", "[+1 Gold] [in this city] <Rough terrain>"),
        RulesetErrorKind::UniqueModifier,
        file,
        "Monument",
    );
    assert!(text.contains("not a modifier"), "{text}");
    // The same action modifier twice.
    refused(
        with_unique(UNITS, "Missionary", "Can Spread Religion <once> <once>"),
        RulesetErrorKind::UniqueModifier,
        UNITS,
        "Missionary",
    );
    // A name that names nothing.
    let text = refused(
        with_unique(file, "Monument", "Gain a free [Temple of Doom] [in this city]"),
        RulesetErrorKind::UniqueParameter,
        file,
        "Monument",
    );
    assert!(text.contains("no building is called \"Temple of Doom\""), "{text}");
}

#[test]
fn a_modifier_the_role_has_no_use_for_is_refused() {
    // A trigger on a standing effect: Python stood it from the start, since a trigger does not
    // filter (uniques.py:790-791), and nothing could apply it once.
    let text = refused(
        with_unique(UNITS, "Warrior", "[+10]% Strength <upon discovering [Writing] technology>"),
        RulesetErrorKind::UniqueModifier,
        UNITS,
        "Warrior",
    );
    assert!(text.contains("fires once") && text.contains("Strength"), "{text}");
    // Action modifiers on what is no unit action, which nothing read.
    let text = refused(
        with_unique(UNITS, "Warrior", "[+10]% Strength <[2] times>"),
        RulesetErrorKind::UniqueModifier,
        UNITS,
        "Warrior",
    );
    assert!(text.contains("<[2] times> limits a unit action"), "{text}");
    // A timer on a requirement, and on a one-time effect, which Python stored for its turns
    // instead of applying (triggers.py:88-92) and then never read.
    let text = refused(
        with_unique(BUILDINGS, "Monument", "Only available <for [10] turns>"),
        RulesetErrorKind::UniqueModifier,
        BUILDINGS,
        "Monument",
    );
    assert!(text.contains("a timer goes on an effect or a flag"), "{text}");
    let nations = "ruleset/nations.json";
    let text = refused(
        load_edited(nations, |v| {
            let list = v["Babylon"]["uniques"].as_array_mut().expect("uniques");
            list.push(json!("Free [Great Scientist] appears <for [10] turns>"));
        }),
        RulesetErrorKind::UniqueModifier,
        nations,
        "Babylon",
    );
    assert!(text.contains("OneTime"), "{text}");
}

#[test]
fn a_timed_effect_may_fire_on_a_trigger() {
    // Granted for its turns when the trigger fires (triggers.py:88-92), and never standing.
    let text = "[+10]% Strength <upon discovering [Writing] technology> <for [10] turns>";
    let r = load_edited("ruleset/nations.json", |v| {
        v["Babylon"]["uniques"].as_array_mut().expect("uniques").push(json!(text));
    })
    .expect("loads");
    let babylon = &r.nations()[r.lookup::<NationId>("Babylon").expect("a nation")].uniques;
    let id = by_text(&r, babylon, text);
    let m = r.uniques().meta(id);
    assert_eq!(m.timed, Some(10));
    assert!(m.trigger.is_some());
    assert!(babylon.triggered.contains(&id) && !babylon.civ.contains(&id));
    let v = m.temp_variant.expect("a variant");
    assert!(r.uniques().meta(v).trigger.is_none(), "the variant holds; it does not fire");
}

#[test]
fn every_problem_is_reported_at_once() {
    let errs = load_edited(BUILDINGS, |v| {
        let monument = v["Monument"]["uniques"].as_array_mut().expect("uniques");
        monument.push(json!("[+1 Culturre]"));
        // A typo'd tag is reported with the rest, not only once they are fixed.
        monument.push(json!("Stealthy"));
        let temple = v["Temple"]["uniques"].as_array_mut().expect("uniques");
        temple.push(json!("[+1 Gold] [in this city] <for every [Cities]>"));
    })
    .expect_err("refused");
    assert_eq!(errs.0.len(), 3, "{errs}");
    assert!(errs.has(RulesetErrorKind::UniqueParameter), "{errs}");
    assert!(errs.has(RulesetErrorKind::UniqueModifier), "{errs}");
    assert!(errs.has(RulesetErrorKind::UnknownUnique), "{errs}");
}

// ---- Gate 3: size -------------------------------------------------------------------------------

#[test]
fn a_unique_is_24_bytes() {
    assert_eq!(core::mem::size_of::<Unique>(), 24);
    assert!(core::mem::size_of::<Unique>() <= 32, "DESIGN.md 5.5");
    assert_eq!(core::mem::size_of::<UniqueData>(), 16);
}

// ---- Gate 4: keys -------------------------------------------------------------------------------

#[test]
fn the_same_text_on_two_sources_has_two_keys() {
    let r = shipped();
    let t = r.uniques();
    let keys: BTreeSet<u64> = t.iter().map(|(id, _)| t.meta(id).key).collect();
    assert_eq!(keys.len(), t.len(), "every key is distinct");
    let text = "[+50]% Golden Age length";
    let with: Vec<UniqueId> =
        t.iter().filter(|(id, _)| t.text_of(*id) == text).map(|(id, _)| id).collect();
    assert!(with.len() >= 2, "{text} is on several sources");
    let their: BTreeSet<u64> = with.iter().map(|&id| t.meta(id).key).collect();
    assert_eq!(their.len(), with.len());
    // And twice on one source: the occurrence tells them apart.
    let r = load_edited(UNITS, |v| {
        let list = v["Warrior"]["uniques"].as_array_mut().expect("uniques");
        list.push(json!("[+5]% Strength"));
        list.push(json!("[+5]% Strength"));
    })
    .expect("loads");
    let warrior = r.lookup::<BaseUnitId>("Warrior").expect("a unit");
    let twins: Vec<UniqueId> = r.base_units()[warrior]
        .uniques
        .ids()
        .filter(|&id| r.uniques().text_of(id) == "[+5]% Strength")
        .collect();
    assert_eq!(twins.len(), 2);
    let (a, b) = (r.uniques().meta(twins[0]), r.uniques().meta(twins[1]));
    assert_eq!((a.occurrence, b.occurrence), (0, 1));
    assert_ne!(a.key, b.key);
}

#[test]
fn adding_a_unique_moves_no_other_key() {
    let base = shipped();
    let keys = |r: &Ruleset| -> BTreeSet<u64> {
        let t = r.uniques();
        t.iter().map(|(id, _)| t.meta(id).key).collect()
    };
    let before = keys(base);
    // Added first on the first source: every unique id after it moves, and no key does.
    let edited = load_edited("ruleset/nations.json", |v| {
        v["Babylon"]["uniques"].as_array_mut().expect("uniques").insert(0, json!("[+1 Gold]"));
    })
    .expect("loads");
    let after = keys(&edited);
    assert_eq!(after.len(), before.len() + 1);
    assert!(before.is_subset(&after), "every old key is still there");
    let first = edited.uniques().text_of(UniqueId(0));
    assert_eq!(first, "[+1 Gold]");
    assert_ne!(edited.uniques().meta(UniqueId(1)).key, base.uniques().meta(UniqueId(1)).key);
    assert_eq!(edited.uniques().meta(UniqueId(1)).key, base.uniques().meta(UniqueId(0)).key);
}
