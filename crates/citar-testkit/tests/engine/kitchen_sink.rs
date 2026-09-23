//! The kitchen-sink ruleset (package 1a-05b) against its contract:
//! - gate a: every unique text of the kitchen sink compiles, on the object that carries it;
//! - gate b: the shipped ruleset and the kitchen sink together use every type
//!   `unique_supported.toml` lists, and the types it marks `(extra)` are exactly those the shipped
//!   ruleset does not use.
//!
//! The kitchen sink is the shipped ruleset with `testdata/rulesets/kitchen_sink/` merged over it
//! (`citar_testkit::rulesets`). The packages of 1b and 1c test their systems' extra types with it.

use std::collections::BTreeSet;

use citar_engine::base::ids::{BaseUnitId, NationId, SpeedId, TerrainId};
use citar_engine::rules::Ruleset;
use citar_engine::rules::defs::BeliefType;
use citar_engine::unique::generated::p;
use citar_engine::unique::params::{BeliefKind, FoundingOrEnhancing, SpyAction};
use citar_engine::unique::text::{placeholder, split_modifiers};
use citar_engine::unique::{CondData, Role, TriggerCond, UFlags, UniqueData, UniqueType};
use citar_testkit::rulesets::{KITCHEN_SINK, kitchen_sink};
use serde_json::Value;

use super::rules::shipped;
use super::uniques::named_sources;

const SUPPORTED: &str = include_str!("../../../citar-engine/unique_supported.toml");

/// The unique lists of a kitchen-sink file's objects, each with the kind of source the loader
/// makes of it (as `named_sources` names them).
fn lists_of(file: &str) -> &'static [(&'static str, &'static str)] {
    match file {
        "ruleset/beliefs.json" => &[("uniques", "Belief")],
        "ruleset/buildings.json" => &[("uniques", "Building")],
        "ruleset/city_state_types.json" => &[
            ("friendBonusUniques", "CityStateFriend"),
            ("allyBonusUniques", "CityStateAlly"),
            ("uniques", "CityStateType"),
        ],
        "ruleset/improvements.json" => &[("uniques", "Improvement")],
        "ruleset/nations.json" => &[("uniques", "Nation")],
        "ruleset/promotions.json" => &[("uniques", "Promotion")],
        "ruleset/ruins.json" => &[("uniques", "Ruins")],
        "ruleset/terrains.json" => &[("uniques", "Terrain")],
        "ruleset/units.json" => &[("uniques", "Unit")],
        other => panic!("{other} is not a file the kitchen sink is expected to patch"),
    }
}

/// Every unique text the kitchen sink adds: (source kind, object, texts).
fn sink_texts() -> Vec<(&'static str, String, Vec<String>)> {
    let mut out = Vec::new();
    for &(file, patch) in KITCHEN_SINK {
        let v: Value = serde_json::from_str(patch).expect("the patch is JSON");
        for (name, obj) in v.as_object().expect("a patch of objects by name") {
            for &(field, kind) in lists_of(file) {
                let texts: Vec<String> = obj
                    .get(field)
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(|t| t.as_str().expect("a text").to_owned()).collect())
                    .unwrap_or_default();
                out.push((kind, name.clone(), texts));
            }
        }
    }
    out
}

/// The types a text names: its own and its modifiers'.
fn types_of(text: &str, out: &mut BTreeSet<UniqueType>) {
    let (main, mods) = split_modifiers(text);
    for part in std::iter::once(main.as_str()).chain(mods) {
        if let Some(ty) = UniqueType::from_placeholder(&placeholder(part).0) {
            out.insert(ty);
        }
    }
}

/// The types a loaded ruleset's texts use, as uniques and as modifiers.
fn types_used(r: &Ruleset) -> BTreeSet<UniqueType> {
    let t = r.uniques();
    let mut out = BTreeSet::new();
    for (id, _) in t.iter() {
        types_of(t.text_of(id), &mut out);
    }
    out
}

fn supported() -> BTreeSet<UniqueType> {
    UniqueType::ALL.into_iter().filter(|t| t.role().is_some()).collect()
}

fn names(types: &BTreeSet<UniqueType>) -> Vec<&'static str> {
    types.iter().map(|t| t.name()).collect()
}

// ---- Gate a: every text compiles ----------------------------------------------------------------

#[test]
fn every_kitchen_sink_text_compiles_on_its_object() {
    let r = kitchen_sink();
    let t = r.uniques();
    let sources = named_sources(r);
    let mut count = 0;
    for (kind, name, want) in sink_texts() {
        let (_, _, u) = sources
            .iter()
            .find(|(k, n, _)| *k == kind && *n == name)
            .unwrap_or_else(|| panic!("the kitchen sink's {kind} {name} is not loaded"));
        let got: Vec<&str> = u.ids().map(|id| t.text_of(id)).collect();
        assert_eq!(got, want, "{kind} {name}");
        for id in u.ids() {
            assert!(t.meta(id).ty.is_some(), "{:?} is a typed unique, not a tag", t.text_of(id));
        }
        count += want.len();
    }
    // It is the shipped ruleset and more: every shipped text is still there, and the timed
    // unique of the kitchen-sink nation has its variant.
    assert_eq!(t.len(), shipped().uniques().len() + count + 1);
}

#[test]
fn the_extra_types_decode_to_their_payloads() {
    let r = kitchen_sink();
    let t = r.uniques();
    let nation = &r.nations()[r.lookup::<NationId>("Kitchen Sink").expect("a nation")].uniques;
    let find = |u: &citar_engine::unique::SourceUniques, ty: UniqueType| {
        u.ids().find(|&id| t.meta(id).ty == Some(ty)).unwrap_or_else(|| panic!("no {}", ty.name()))
    };
    // The three new kinds of parameter.
    let spies = t.get(find(nation, UniqueType::CounterIntelligenceSpyRankBonus)).data;
    let UniqueData::CounterIntelligenceSpyRankBonus(p) = spies else { panic!("{spies:?}") };
    assert_eq!((p.levels, p.action), (1, SpyAction::CounterIntelligence));
    let beliefs = t.get(find(nation, UniqueType::FreeExtraBeliefs)).data;
    let UniqueData::FreeExtraBeliefs(p) = beliefs else { panic!("{beliefs:?}") };
    assert_eq!(
        (p.count, p.belief, p.when),
        (1, BeliefKind::Type(BeliefType::Follower), FoundingOrEnhancing::Founding)
    );
    let quick = r.lookup::<SpeedId>("Quick").expect("a speed");
    let on_quick = CondData::ConditionalSpeed(p::ConditionalSpeed { speed: quick });
    assert!(t.all_conds().iter().any(|(_, c)| c.data == on_quick), "<on [Quick] game speed>");
    // One-time effects: triggered ones fire, the others happen when their source is gained.
    let policies = find(nation, UniqueType::OneTimeAmountFreePolicies);
    assert!(nation.triggered.contains(&policies));
    assert_eq!(
        t.meta(policies).trigger.map(|c| c.ty()),
        Some(UniqueType::TriggerUponEnteringGoldenAge)
    );
    let ruin = r
        .ruins()
        .as_slice()
        .iter()
        .find(|x| &*x.name == "your unit is lost in the ruins")
        .expect("a ruin");
    let destroyed = find(&ruin.uniques, UniqueType::OneTimeUnitDestroyed);
    assert!(ruin.uniques.on_gain.contains(&destroyed));
    let raider =
        &r.base_units()[r.lookup::<BaseUnitId>("Kitchen Sink Raider").expect("a unit")].uniques;
    let defeat = find(raider, UniqueType::OneTimeGainStat);
    assert_eq!(t.meta(defeat).trigger, Some(TriggerCond::TriggerUponDefeat));
    let sage = &r.base_units()[r.lookup::<BaseUnitId>("Kitchen Sink Sage").expect("a unit")];
    let culture = find(&sage.uniques, UniqueType::CanHurryPolicy);
    assert!(sage.uniques.actions.contains(&culture));
    assert_eq!(t.meta(culture).role, Role::Action);
    // A timed effect a trigger grants is no standing effect.
    let timed = find(nation, UniqueType::Strength);
    assert!(t.get(timed).flags().contains(UFlags::TIMED | UFlags::TRIGGERED));
    assert!(!nation.civ.contains(&timed));
    // Map generation reads the larger-landmass rule.
    let spire = r.lookup::<TerrainId>("Kitchen Sink Spire").expect("a wonder");
    assert_eq!(r.gen_tables().wonders[spire].on_largest, [1]);
    let placed = find(&r.terrains()[spire].uniques, UniqueType::NaturalWonderLargerLandmass);
    assert!(r.gen_tables().placed.contains(&placed));
}

#[test]
fn display_modifiers_change_nothing() {
    // <hidden from users>, <Civilopedia link []> and <Suppress warning []> say how UnCiv shows a
    // unique; Python passed them over (uniques.py:1013-1019), and so does the compiler.
    let r = kitchen_sink();
    let t = r.uniques();
    let nation = &r.nations()[r.lookup::<NationId>("Kitchen Sink").expect("a nation")].uniques;
    let id = nation
        .ids()
        .find(|&id| t.text_of(id).contains("<hidden from users>"))
        .expect("the displayed unique");
    let u = t.get(id);
    assert!(matches!(u.data, UniqueData::StatsPerCity(_)));
    assert!(u.conds.is_empty() && u.flags().is_empty() && t.modifiers(id).is_empty());
    assert!(nation.civ.contains(&id), "it stands like any other effect");
}

// ---- Gate b: coverage ---------------------------------------------------------------------------

#[test]
fn the_shipped_ruleset_and_the_kitchen_sink_use_every_supported_type() {
    let supported = supported();
    let shipped = types_used(shipped());
    let mut own = BTreeSet::new();
    for (_, _, texts) in sink_texts() {
        for text in &texts {
            types_of(text, &mut own);
        }
    }
    assert!(shipped.is_subset(&supported), "every shipped type is supported");
    let unsupported: BTreeSet<UniqueType> = own.difference(&supported).copied().collect();
    assert!(unsupported.is_empty(), "the kitchen sink uses {:?}", names(&unsupported));
    let covered: BTreeSet<UniqueType> = shipped.union(&own).copied().collect();
    let missing: BTreeSet<UniqueType> = supported.difference(&covered).copied().collect();
    assert!(missing.is_empty(), "no ruleset uses {:?}", names(&missing));
    // The census: DESIGN.md 5.1's 402 types in the shipped ruleset, and 125 more.
    assert_eq!(shipped.len(), 402);
    assert_eq!(supported.len(), 402 + 125);
    // The loaded kitchen sink reads its texts the same way.
    assert_eq!(types_used(kitchen_sink()), supported);
}

#[test]
fn the_supported_list_marks_exactly_the_extra_types() {
    let shipped = types_used(shipped());
    let extra: BTreeSet<UniqueType> = supported().difference(&shipped).copied().collect();
    let marked: BTreeSet<UniqueType> = SUPPORTED
        .lines()
        .filter(|l| !l.starts_with('#') && l.contains("# (extra)"))
        .map(|l| {
            let name = l.split(" = ").next().unwrap_or("");
            UniqueType::ALL
                .into_iter()
                .find(|t| t.name() == name)
                .unwrap_or_else(|| panic!("{name} is no type"))
        })
        .collect();
    let unmarked: BTreeSet<UniqueType> = extra.difference(&marked).copied().collect();
    let wrongly: BTreeSet<UniqueType> = marked.difference(&extra).copied().collect();
    assert!(unmarked.is_empty(), "mark these (extra): {:?}", names(&unmarked));
    assert!(wrongly.is_empty(), "the shipped ruleset uses these: {:?}", names(&wrongly));
}
