//! Religion, great people, triggers and ruins (package 1b-08), on the arena:
//! - every kind of one-time effect applied, each unit effect among them, with every check clean
//!   after (gate 4);
//! - the kitchen sink's triggers fired where their events happen: a pantheon, a religion founded
//!   and enhanced, a golden age, a great prophet gained, a turn's start and end, a policy adopted,
//!   a city founded, a war declared and peace made; and its `May choose [n] additional [kind]
//!   beliefs when [founding] a religion` and `Can speed up the construction of a wonder`;
//! - religious pressure from the spatial grid equal to Python's walk over every city, on the late
//!   fixtures and the corpus;
//! - great prophets faith brings, great person points and births, and the Maya long count.

use std::collections::{BTreeMap, BTreeSet};

use citar_engine::api::testops;
use citar_engine::base::ids::{BaseUnitId, CityId, PlayerId, UniqueId, UnitId};
use citar_engine::base::stats::Stat;
use citar_engine::game::religion::{self, found, prophets};
use citar_engine::game::{DebugOptions, Game, great_people, triggers};
use citar_engine::rules::defs::{BeliefKind, BeliefType, ReligionProgress};
use citar_engine::rules::{Named, Ruleset};
use citar_engine::state::cities::Constructible;
use citar_engine::unique::trigger::{OneTimeEffect, TriggerSite, UnitEffect};
use citar_testkit::fixtures;
use citar_testkit::rulesets::kitchen_sink;
use citar_testkit::script::{map_doc, new_game};
use serde_json::{Value, json};

const ME: PlayerId = PlayerId(0);
const YOU: PlayerId = PlayerId(1);

fn id<I: Named>(r: &Ruleset, name: &str) -> I {
    r.lookup::<I>(name).unwrap_or_else(|| panic!("the ruleset has {name}"))
}

/// A bare game on the arena: two majors of no nation's ability, espionage on, no unit.
fn arena(r: &'static Ruleset, nations: [&str; 2]) -> Game {
    let (doc, _) = map_doc("arena").expect("the arena");
    let cfg = json!({
        "seed": 1,
        "players": [{"nation": nations[0]}, {"nation": nations[1]}],
        "city_states": 0,
        "barbarians": "off",
        "ruins": false,
        "espionage": true,
        "map": doc,
    });
    let mut g = new_game(r, cfg.as_object().expect("an object")).unwrap_or_else(|e| panic!("{e}"));
    g.set_debug_options(DebugOptions::ALL);
    testops::apply(&mut g, &json!([{"op": "clear_units", "player": "all"}])).expect("cleared");
    g
}

fn ops(g: &mut Game, v: &Value) -> Vec<Value> {
    g.apply_ops(v).unwrap_or_else(|e| panic!("{v}: {e}")).0
}

/// The game settles, as every call ends.
fn settle(g: &mut Game) {
    testops::apply(g, &json!([])).expect("settled");
}

fn clean(g: &mut Game) {
    let v = g.take_violations();
    assert!(v.is_empty(), "violations: {v:?}");
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
    assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
}

fn city_of(out: &Value) -> CityId {
    out["city_id"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .and_then(CityId::new)
        .unwrap_or_else(|| panic!("no city in {out}"))
}

fn unit_of(out: &Value) -> UnitId {
    out["unit_ids"][0]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .and_then(UnitId::new)
        .unwrap_or_else(|| panic!("no unit in {out}"))
}

// ---- Every one-time effect (gate 4) ----------------------------------------------------------------

/// A kind's name, and a unit effect's own.
fn kind(e: &OneTimeEffect) -> String {
    match e {
        OneTimeEffect::Unit(u) => format!("Unit/{u:?}").split('(').next().unwrap_or("").to_owned(),
        _ => e.kind_name().to_owned(),
    }
}

/// What the effect test plays on: Roma at A and Veii on the coast, the other civilization's
/// Antium at B, a wounded Warrior at H and a barbarian camp eight tiles from it, out of sight.
struct Stage {
    g: Game,
    roma: CityId,
    warrior: UnitId,
}

fn stage(r: &'static Ruleset) -> Stage {
    let mut g = arena(r, ["BenchmarkCiv", "BenchmarkCiv"]);
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Roma", "pop": 3},
            {"op": "found_city", "player": 0, "x": 3, "y": 11, "name": "Veii"},
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "name": "Antium"},
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 7, "y": 5, "hp": 60},
            {"op": "set_tile", "x": 15, "y": 4, "improvement": "Barbarian encampment"},
        ]),
    );
    Stage { roma: city_of(&out[0]), warrior: unit_of(&out[3]), g }
}

#[test]
fn every_kind_of_one_time_effect_applies() {
    let r = kitchen_sink();
    let t = r.uniques();
    let mut first: BTreeMap<String, (UniqueId, OneTimeEffect)> = BTreeMap::new();
    for (u, _) in t.iter() {
        if let Some(e) = OneTimeEffect::decode(r, u) {
            first.entry(kind(&e)).or_insert((u, e));
        }
    }
    let mut kinds = BTreeSet::new();
    for (name, &(u, effect)) in &first {
        let Stage { mut g, roma, warrior } = stage(r);
        if matches!(effect, OneTimeEffect::SpiesLevelUp { .. }) {
            let (spy, _) = first["GainSpy"];
            assert!(triggers::apply(&mut g, spy, &TriggerSite::civ(ME), None));
        }
        let at = g.unit(warrior).map(|x| x.tile());
        let site = TriggerSite { civ: ME, city: Some(roma), unit: Some(warrior), tile: at };
        let before = g.clone();
        let text = t.text_of(u).to_owned();
        let happened = triggers::apply(&mut g, u, &site, None);
        settle(&mut g);
        let (old, new) = (before.player(ME).expect("me"), g.player(ME).expect("me"));
        let pop = |g: &Game| g.player_cities(ME).map(|c| u32::from(c.pop)).sum::<u32>();
        let buildings = |g: &Game| g.player_cities(ME).map(|c| c.buildings.len()).sum::<usize>();
        let units = |g: &Game| g.player_units(ME).count();
        let ok = match effect {
            OneTimeEffect::Timed { turns, .. } => {
                new.civ.temp_uniques.len() == old.civ.temp_uniques.len() + 1
                    && new
                        .civ
                        .temp_uniques
                        .last()
                        .is_some_and(|x| x.turns == i16::try_from(turns).unwrap_or(0))
            }
            OneTimeEffect::FreeUnits { .. } => units(&g) > units(&before),
            OneTimeEffect::FreePolicies { count } => {
                new.policy.free_policies == old.policy.free_policies + count
            }
            OneTimeEffect::Adopt(_) => new.policy.adopted.len() > old.policy.adopted.len(),
            OneTimeEffect::GoldenAge { .. } => new.econ.golden_age_turns > 0,
            OneTimeEffect::FreeGreatPerson => new.gp.free == old.gp.free + 1,
            OneTimeEffect::GainPopulation { .. }
            | OneTimeEffect::GainPopulationRandomCity { .. } => pop(&g) > pop(&before),
            OneTimeEffect::FreeTechs { count } => {
                new.tech.free_techs == old.tech.free_techs + count
            }
            OneTimeEffect::DiscoverTech(tech) => new.tech.known.contains(tech),
            OneTimeEffect::FreeTechsFromEras { .. } => new.tech.known.len() > old.tech.known.len(),
            OneTimeEffect::RevealEntireMap => {
                new.explored.len() == usize::try_from(g.grid().size()).unwrap_or(0)
            }
            OneTimeEffect::FreeBelief(k) => new.religion.free(k) == old.religion.free(k) + 1,
            OneTimeEffect::TriggerVoting => g.state().world().un.next_vote.is_some(),
            OneTimeEffect::GainStat { stat, .. } => match stat {
                Stat::Gold => new.econ.gold > old.econ.gold,
                Stat::Culture => new.econ.culture > old.econ.culture,
                Stat::Faith => new.econ.faith > old.econ.faith,
                Stat::Happiness => new.econ.golden_age_points > old.econ.golden_age_points,
                Stat::Science => {
                    new.tech.overflow > old.tech.overflow
                        || new.tech.known.len() > old.tech.known.len()
                }
                Stat::Food | Stat::Production => !happened,
            },
            OneTimeEffect::GainPantheon | OneTimeEffect::GainProphet { .. } => {
                new.econ.faith > old.econ.faith
            }
            OneTimeEffect::GainTechPercent { tech, .. } => {
                new.tech.progress.get(&tech).copied().unwrap_or(0.0) > 0.0
            }
            OneTimeEffect::TakeOverTilesInRadius { .. }
            | OneTimeEffect::TakeOverTilesInCity { .. } => {
                let owned = |g: &Game| {
                    g.state().tiles().iter().filter(|(_, x)| x.owner() == Some(ME)).count()
                };
                owned(&g) > owned(&before)
            }
            OneTimeEffect::RevealTiles { .. } | OneTimeEffect::RevealCrudeMap { .. } => {
                new.explored.len() > old.explored.len()
            }
            OneTimeEffect::GlobalSpiesWhenEnteringEra => [ME, YOU].iter().all(|&p| {
                g.player(p).and_then(|x| x.major.as_deref()).is_some_and(|m| m.spies.len() == 1)
            }),
            OneTimeEffect::SpiesLevelUp { .. } => {
                new.major.as_deref().and_then(|m| m.spies.first()).is_some_and(|s| s.rank > 1)
            }
            OneTimeEffect::GainSpy => new.major.as_deref().is_some_and(|m| m.spies.len() == 1),
            OneTimeEffect::FreeBuilding { .. }
            | OneTimeEffect::FreeStatBuildings { .. }
            | OneTimeEffect::FreeSpecificBuildings { .. } => buildings(&g) > buildings(&before),
            OneTimeEffect::PromoteUnits { promotion, .. } => {
                // No unit of ours may be of the kind it promotes: then nothing happens.
                g.unit(warrior).is_some_and(|x| x.promotions.contains(promotion)) || !happened
            }
            OneTimeEffect::CityStateGreatPersonGift => {
                new.major.as_deref().is_some_and(|m| m.cs_gp_gift.is_some())
            }
            OneTimeEffect::Unit(e) => {
                let (was, now) = (before.unit(warrior).expect("the warrior"), g.unit(warrior));
                match e {
                    UnitEffect::Heal(_) => now.is_some_and(|x| x.hp > was.hp),
                    UnitEffect::Damage(n) => {
                        now.is_none_or(|x| i32::from(x.hp) == i32::from(was.hp) - n)
                    }
                    UnitEffect::GainXp(n) => now.is_some_and(|x| x.xp == was.xp + n),
                    // Upgrades are package 1c-02's: nothing happens yet.
                    UnitEffect::Upgrade | UnitEffect::SpecialUpgrade => !happened,
                    UnitEffect::GainPromotion(p) => now.is_some_and(|x| x.promotions.contains(p)),
                    UnitEffect::Movement(n) => {
                        now.is_some_and(|x| x.moves == (was.moves + n * 60).max(0))
                    }
                    UnitEffect::Destroyed => now.is_none(),
                }
            }
        };
        assert!(ok, "{name}: {text:?} (happened: {happened})");
        if !matches!(effect, OneTimeEffect::Unit(UnitEffect::Upgrade | UnitEffect::SpecialUpgrade))
            && !matches!(effect, OneTimeEffect::PromoteUnits { .. })
        {
            assert!(happened, "{name}: {text:?} says nothing happened");
        }
        clean(&mut g);
        kinds.insert(effect.kind_name());
    }
    assert_eq!(kinds.len(), OneTimeEffect::KINDS, "{kinds:?}");
}

// ---- The kitchen sink's triggers ---------------------------------------------------------------

/// A kitchen-sink civilization against a benchmark one, each with a city.
fn sink() -> (Game, CityId) {
    let r = kitchen_sink();
    let mut g = arena(r, ["Kitchen Sink", "BenchmarkCiv"]);
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Sinkhold"},
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "name": "Antium"},
        ]),
    );
    (g, city_of(&out[0]))
}

fn faith(g: &Game, p: PlayerId) -> f64 {
    g.player(p).map_or(0.0, |x| x.econ.faith)
}

#[test]
fn the_kitchen_sinks_religious_triggers_fire() {
    let r = kitchen_sink();
    let (mut g, sinkhold) = sink();
    // Founding a city discovered Writing (`Discover [Writing] <upon founding a city>`).
    assert!(g.has_tech(ME, Some(id(r, "Writing"))));
    // `Gain [30] [Faith] <upon founding a Pantheon>`.
    ops(&mut g, &json!([{"op": "set_player", "player": 0, "faith": 10}]));
    let (b, pay) = found::plan_pantheon(&g, ME, "Goddess of Love").expect("a pantheon");
    found::apply_pantheon(&mut g, ME, b, pay);
    settle(&mut g);
    assert!((faith(&g, ME) - 30.0).abs() < 1e-9, "{}", faith(&g, ME));
    // A religion: `May choose [1] additional [Follower] beliefs when [founding] a religion` owes
    // two follower beliefs; `Gain a free [Follower] belief <upon founding a Religion>` owes the
    // next one.
    let needed = found::beliefs_to_choose(&g, ME, false);
    assert_eq!(
        needed.to_vec(),
        [(BeliefKind::Type(BeliefType::Founder), 1), (BeliefKind::Type(BeliefType::Follower), 2)]
    );
    let at = g.city(sinkhold).map(|c| c.tile()).expect("the city");
    let names: Vec<String> =
        ["Ceremonial Burial", "Pagodas", "Mosques"].into_iter().map(str::to_owned).collect();
    let plan = found::plan_religion(&g, ME, at, "Kitchen Faith", &names, None).expect("founded");
    found::apply_religion(&mut g, ME, &plan, |_| {});
    settle(&mut g);
    let pl = g.player(ME).expect("me");
    assert_eq!(pl.religion.progress, ReligionProgress::Religion);
    assert_eq!(pl.religion.free(BeliefKind::Type(BeliefType::Follower)), 1);
    assert_eq!(religion::holy_city(&g, pl.religion.founded.expect("a religion")), Some(sinkhold));
    // `Research [50]% of [Philosophy] <upon enhancing a Religion>`.
    // The free follower belief is chosen now.
    let enhance: Vec<String> = ["Kitchen Sink Faith", "Cathedrals", "Choral Music"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let chosen = found::plan_enhance(&g, ME, at, &enhance).expect("enhanced");
    found::apply_enhance(&mut g, ME, &chosen, |_| {});
    settle(&mut g);
    let philosophy = id(r, "Philosophy");
    let progress = g.player(ME).and_then(|x| x.tech.progress.get(&philosophy).copied());
    assert!(progress.is_some_and(|p| p > 0.0), "{progress:?}");
    // `Gain [10] [Faith] <upon gaining a [Great Prophet] unit>`, when a city makes one.
    let before = faith(&g, ME);
    let prophet: BaseUnitId = id(r, "Great Prophet");
    assert!(prophets::prophet_unit(&g, ME) == Some(prophet));
    ops(&mut g, &json!([{"op": "set_player", "player": 0, "faith": 5000}]));
    let before_units = g.player_units(ME).count();
    prophets::start_turn(&mut g, ME);
    settle(&mut g);
    assert_eq!(g.player_units(ME).count(), before_units + 1, "a great prophet appeared");
    assert!(faith(&g, ME) > 5000.0 - f64::from(prophets::faith_for_next_prophet(&g, ME)) - before);
    let prophet_now = g.player_units(ME).find(|u| u.base == prophet).expect("the prophet");
    assert_eq!(prophet_now.religion, g.player(ME).and_then(|x| x.religion.founded));
    clean(&mut g);
}

#[test]
fn the_kitchen_sinks_turn_policy_and_golden_age_triggers_fire() {
    let r = kitchen_sink();
    let (mut g, _) = sink();
    // `Gain [5] [Gold] <upon turn start>` and `Gain [5] [Culture] <upon turn end>`.
    let (gold, culture) = g.player(ME).map(|x| (x.econ.gold, x.econ.culture)).expect("me");
    testops::apply(&mut g, &json!([{"op": "end_turn"}])).expect("ended");
    let culture_after = g.player(ME).map(|x| x.econ.culture).expect("me");
    assert!(culture_after >= culture + 5.0, "{culture} -> {culture_after}");
    testops::apply(&mut g, &json!([{"op": "end_turn"}])).expect("the other's turn ended");
    let gold_after = g.player(ME).map(|x| x.econ.gold).expect("me");
    assert!(gold_after >= gold + 5.0, "{gold} -> {gold_after}");
    // `Free Great Person <upon adopting [Aristocracy]>`.
    ops(&mut g, &json!([{"op": "adopt_policy", "player": 0, "policy": "Aristocracy"}]));
    assert_eq!(g.player(ME).map(|x| x.gp.free), Some(1));
    // `[1] Free Social Policies <upon entering a Golden Age>`.
    let free = g.player(ME).map(|x| x.policy.free_policies).expect("me");
    great_people::enter_golden_age(&mut g, ME, None);
    settle(&mut g);
    let pl = g.player(ME).expect("me");
    assert_eq!(pl.policy.free_policies, free + 1);
    assert_eq!(pl.econ.golden_age_turns, 10);
    let _ = r;
    clean(&mut g);
}

#[test]
fn the_kitchen_sinks_war_and_peace_triggers_fire() {
    let r = kitchen_sink();
    let (mut g, _) = sink();
    ops(&mut g, &json!([{"op": "meet", "a": 0, "b": 1}]));
    let units = g.player_units(ME).count();
    // `Free [Warrior] appears <upon entering a war with [Major] Civilizations>`, and the other side
    // is declared war upon: `[+10]% Strength <for [10] turns> <upon being declared war on by
    // [Major] Civilizations>` is not the benchmark's, so it holds nothing new.
    ops(&mut g, &json!([{"op": "set_relation", "a": 1, "b": 0, "state": "war"}]));
    let warrior: BaseUnitId = id(r, "Warrior");
    assert!(g.player_units(ME).count() > units, "a free warrior");
    assert!(g.player_units(ME).any(|u| u.base == warrior));
    let temp = g.player(ME).map(|x| x.civ.temp_uniques.len()).expect("me");
    assert_eq!(temp, 1, "declared war upon: the timed strength");
    // `Gain [50] [Gold] <upon signing a peace treaty with [Major] Civilizations>`.
    let gold = g.player(ME).map(|x| x.econ.gold).expect("me");
    ops(&mut g, &json!([{"op": "set_relation", "a": 0, "b": 1, "state": "peace"}]));
    assert_eq!(g.player(ME).map(|x| x.econ.gold), Some(gold + 50.0));
    clean(&mut g);
}

#[test]
fn a_unit_that_speeds_up_wonders_hurries_a_wonder_alone() {
    let r = kitchen_sink();
    let (mut g, sinkhold) = sink();
    let out = ops(
        &mut g,
        &json!([
            {"op": "grant_tech", "player": 0, "techs": ["Bronze Working", "Writing"]},
            {"op": "add_unit", "player": 0, "unit": "Kitchen Sink Sage", "x": 5, "y": 5},
            {"op": "set_city", "city": sinkhold.get(), "production": "Monument"},
        ]),
    );
    let sage = unit_of(&out[1]);
    // New units have no movement until package 1c-02: the raider's `[This Unit] gains [1]
    // movement` gives the sage some.
    let t = r.uniques();
    let gain = t
        .iter()
        .map(|(u, _)| u)
        .find(|&u| {
            matches!(
                OneTimeEffect::decode(r, u),
                Some(OneTimeEffect::Unit(UnitEffect::Movement(n))) if n > 0
            )
        })
        .expect("a movement gain");
    let site = TriggerSite { civ: ME, city: None, unit: Some(sage), tile: None };
    assert!(triggers::apply(&mut g, gain, &site, None));
    // A building that is no wonder: the sage may not hurry it.
    // refcheck: hurry-wonder-construction
    let e = great_people::plan_hurry_construction(&g, sage).expect_err("not a wonder");
    assert_eq!(e.message, "This unit cannot hurry construction.");
    let library = "The Great Library";
    ops(&mut g, &json!([{"op": "set_city", "city": sinkhold.get(), "production": library}]));
    let wonder = Constructible::Building(id(r, library));
    let plan = great_people::plan_hurry_construction(&g, sage).expect("a wonder");
    assert_eq!((plan.0, plan.1), (sinkhold, wonder));
    let out = great_people::apply_hurry_construction(&mut g, sage, plan);
    settle(&mut g);
    assert_eq!(out["item"], library);
    assert!(g.unit(sage).is_none(), "the sage is spent");
    assert!(
        g.city(sinkhold).and_then(|c| c.progress.get(&wonder).copied()).is_some_and(|p| p > 0.0)
    );
    clean(&mut g);
}

// ---- Great people --------------------------------------------------------------------------------

#[test]
fn great_person_points_pile_up_and_a_great_person_is_born_at_the_threshold() {
    let r = Ruleset::shared();
    let mut g = arena(r, ["BenchmarkCiv", "BenchmarkCiv"]);
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Roma", "pop": 4,
             "buildings": ["The Great Library"]},
            {"op": "add_unit", "player": 1, "unit": "Warrior", "x": 18, "y": 10},
        ]),
    );
    let roma = city_of(&out[0]);
    let gpp = great_people::city_gpp(&g, roma);
    assert_eq!(gpp.to_vec(), [(id::<BaseUnitId>(r, "Great Scientist"), 1)], "the library's point");
    testops::apply(&mut g, &json!([{"op": "end_round"}])).expect("a round");
    let pl = g.player(ME).expect("me");
    for &(u, n) in &gpp {
        #[allow(clippy::cast_precision_loss, reason = "points are small")]
        let n = n as f64;
        assert_eq!(pl.gp.points.get(&u).copied(), Some(n), "{:?}", r.name(u));
    }
    // A hundred points of one kind at standard speed: born in the capital at the next turn's
    // start, and the pool's next threshold doubles.
    let (kind, _) = gpp[0];
    let need = great_people::points_required(&g, ME, kind);
    assert_eq!(need, 100);
    let turns = u32::try_from(need / gpp[0].1 + 1).unwrap_or(1);
    for _ in 0..turns {
        testops::apply(&mut g, &json!([{"op": "end_round"}])).expect("a round");
    }
    let pl = g.player(ME).expect("me");
    assert_eq!(pl.gp.earned, 1);
    assert!(g.player_units(ME).any(|u| u.base == kind));
    assert_eq!(great_people::points_required(&g, ME, kind), 200);
    clean(&mut g);
}

#[test]
fn the_maya_get_a_great_person_when_a_baktun_ends() {
    let r = Ruleset::shared();
    let mut g = arena(r, ["The Maya", "BenchmarkCiv"]);
    ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Mutal"},
            {"op": "add_unit", "player": 1, "unit": "Warrior", "x": 18, "y": 10},
            {"op": "grant_tech", "player": 0, "tech": "Theology"},
        ]),
    );
    let baktun = |g: &Game, t: i32| {
        citar_engine::base::num::trunc_i64(((g.year(Some(t)) + 3114.0) / 394.0).floor())
    };
    let turn = (2..500).find(|&t| baktun(&g, t) != baktun(&g, t - 1)).expect("a b'ak'tun ends");
    testops::apply(&mut g, &json!([{"op": "set_turn", "turn": turn}])).expect("the turn");
    great_people::maya_long_count(&mut g, ME);
    settle(&mut g);
    let pl = g.player(ME).expect("me");
    assert_eq!((pl.gp.free, pl.gp.maya_limited), (1, 1));
    assert!(!pl.gp.long_count_pool.is_empty());
    // Chosen once, a kind leaves the pool.
    let (name, cap) =
        great_people::plan_free_great_person(&g, ME, "Great Scientist").expect("a free one");
    great_people::apply_free_great_person(&mut g, ME, name, cap);
    settle(&mut g);
    let pl = g.player(ME).expect("me");
    assert_eq!((pl.gp.free, pl.gp.maya_limited), (0, 0));
    assert!(!pl.gp.long_count_pool.contains(&name));
    clean(&mut g);
}

// ---- Religious pressure --------------------------------------------------------------------------

/// Python's walk (`religion.pressures_from_surroundings`, `religion.py:266-284`): every other city
/// of the world, in id order.
fn naive_pressures(g: &Game, c: CityId) -> Vec<(u8, i32)> {
    let mut out: Vec<(u8, i32)> = Vec::new();
    let mut add = |r: u8, n: i32| match out.iter_mut().find(|(x, _)| *x == r) {
        Some((_, m)) => *m += n,
        None => out.push((r, n)),
    };
    let city = g.city(c).expect("the city");
    if let Some(r) = city.holy_city_of
        && !religion::blocked(g, c)
    {
        add(r.0, 5 * g.speed().religious_pressure_adjacent_city);
    }
    for other in g.state().cities().iter() {
        if other.id() == c {
            continue;
        }
        let Some(m) = religion::majority_religion(g, other.id()) else { continue };
        if !religion::is_major(g, m) {
            continue;
        }
        let range = u32::try_from(religion::spread_range(g, other.id())).unwrap_or(0);
        if g.grid().distance(other.tile(), city.tile()) > range {
            continue;
        }
        add(m.0, religion::pressure_to(g, other.id(), c));
    }
    out
}

fn same_pressures(g: &Game, label: &str) -> usize {
    let mut n = 0;
    for c in g.state().cities().iter() {
        let fast: Vec<(u8, i32)> = religion::pressures_from_surroundings(g, c.id())
            .iter()
            .map(|&(r, n)| (r.0, n))
            .collect();
        assert_eq!(fast, naive_pressures(g, c.id()), "{label}: city {}", c.id().get());
        n += usize::from(!fast.is_empty());
    }
    n
}

#[test]
fn the_grid_finds_the_pressure_pythons_walk_found() {
    let mut all = fixtures::committed().unwrap_or_else(|e| panic!("{e}"));
    all.extend(fixtures::corpus().unwrap_or_else(|e| panic!("{e}")).into_iter().flatten());
    let mut felt = 0;
    for f in &all {
        let bytes = fixtures::read_state(f).unwrap_or_else(|e| panic!("{e}"));
        let (g, _) = Game::from_python(Ruleset::shared(), &bytes).unwrap_or_else(|e| panic!("{e}"));
        if g.religion_enabled() {
            felt += same_pressures(&g, &format!("{f:?}"));
        }
    }
    assert!(felt > 0, "some city feels pressure");
}

#[test]
fn a_round_of_pressure_converts_a_neighbour_and_pays_the_founder() {
    let r = Ruleset::shared();
    let mut g = arena(r, ["BenchmarkCiv", "BenchmarkCiv"]);
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Roma", "pop": 6},
            {"op": "found_city", "player": 0, "x": 5, "y": 11, "name": "Veii"},
            {"op": "found_city", "player": 1, "x": 11, "y": 7, "name": "Antium"},
            {"op": "set_player", "player": 0, "faith": 100},
            {"op": "add_unit", "player": 0, "unit": "Great Prophet", "x": 5, "y": 5},
        ]),
    );
    let (roma, veii, antium) = (city_of(&out[0]), city_of(&out[1]), city_of(&out[2]));
    let prophet = unit_of(&out[4]);
    let (b, pay) = found::plan_pantheon(&g, ME, "Ancestor Worship").expect("a pantheon");
    found::apply_pantheon(&mut g, ME, b, pay);
    let names: Vec<String> =
        ["Ceremonial Burial", "Pagodas"].into_iter().map(str::to_owned).collect();
    let v = found::prophet_acts(&mut g, prophet, false, "Test Faith", &names).expect("founded");
    settle(&mut g);
    let rel = g.player(ME).and_then(|x| x.religion.founded).expect("a religion");
    assert_eq!(v["holy_city"], "Roma");
    assert_eq!(religion::majority_religion(&g, roma), Some(rel));
    // The holy city presses its neighbours within ten tiles, those of another civilization too,
    // every turn until they follow it.
    let pressure = religion::pressures_from_surroundings(&g, antium);
    assert!(pressure.iter().any(|&(r, n)| r == rel && n > 0), "{pressure:?}");
    same_pressures(&g, "the arena");
    // Veii follows its owner's pantheon first, which holds it longer.
    let follows = |g: &Game, c| religion::majority_religion(g, c) == Some(rel);
    let mut rounds = 0;
    while !(follows(&g, antium) && follows(&g, veii)) && rounds < 100 {
        testops::apply(&mut g, &json!([{"op": "end_round"}])).expect("a round");
        same_pressures(&g, "the arena");
        rounds += 1;
    }
    assert!(follows(&g, antium) && follows(&g, veii), "after {rounds} rounds");
    // Veii's conversion is announced and recorded. (A city whose growth alone tips its majority,
    // as Antium's may, is not: Python compared the majority after the new citizen came.)
    assert!(g.city(veii).is_some_and(|c| c.religions_adopted.contains(&rel)));
    clean(&mut g);
}
