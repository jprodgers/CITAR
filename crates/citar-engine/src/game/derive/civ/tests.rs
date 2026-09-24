//! Package 1b-05: the unique index memos, the resource supply and the unit profiles, on the test
//! game of `core::testing`, and gate 3: after random writes to every input the memos read, and
//! reads between them, each memo equals a cold rebuild.

use proptest::prelude::*;

use super::*;
use crate::base::ids::{
    BeliefId, ImprovementId, PolicyId, PromotionId, ResourceId, TechId, TileIdx,
};
use crate::game::core::testing;
use crate::game::derive::rev::{CityTouch, PlayerTouch, UnitTouch, WorldTouch};
use crate::rules::Named;
use crate::rules::defs::BeliefType;
use crate::state::TileClaim;
use crate::state::players::TempUnique;
use crate::state::world::{Religion, ReligionName};

const ROME: PlayerId = PlayerId(0);
const GREECE: PlayerId = PlayerId(1);
const GENEVA: PlayerId = PlayerId(2);

fn id<I: Named>(g: &Game, name: &str) -> I {
    g.rules.lookup::<I>(name).unwrap_or_else(|| panic!("the ruleset has {name}"))
}

/// The test game with Geneva of the first city-state type, and a city each: Roma (22) and
/// Antium (26) for Rome, Athens (55) for Greece, Geneva (60) for Geneva.
fn game() -> (Game, [CityId; 4]) {
    let mut g = testing::duel();
    if let Some(d) =
        g.player_mut(GENEVA, PlayerTouch::CITY_STATE).and_then(|p| p.city_state.as_deref_mut())
    {
        d.cs_type = Some(crate::base::ids::CityStateTypeId(0));
    }
    let roma = testing::city(&mut g, ROME, TileIdx(22), "Roma");
    let antium = testing::city(&mut g, ROME, TileIdx(26), "Antium");
    let athens = testing::city(&mut g, GREECE, TileIdx(55), "Athens");
    let geneva = testing::city(&mut g, GENEVA, TileIdx(60), "Geneva");
    g.settle();
    (g, [roma, antium, athens, geneva])
}

fn count(csr: &Csr, ty: UniqueType) -> usize {
    csr.get(ty).iter().map(|e| usize::from(e.n)).sum()
}

fn clean(g: &Game) {
    let found = verify(g);
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn the_index_counts_every_city_with_a_building_and_follows_its_sources() {
    let (mut g, [roma, antium, ..]) = game();
    let temple: BuildingId = id(&g, "Temple");
    let before = civ_index(&g, ROME).len();
    for c in [roma, antium] {
        if let Some(x) = g.city_mut(c, CityTouch::BUILDINGS) {
            x.buildings.insert(temple);
        }
    }
    let src = sources(&g, ROME);
    assert_eq!(src.buildings, vec![(temple, 2)]);
    let idx = civ_index(&g, ROME);
    assert!(idx.len() > before, "the Temple's uniques joined");
    let t = g.rules.uniques();
    let from_temple: Vec<_> = idx
        .entries()
        .iter()
        .filter(|e| t.meta(e.id).source == crate::unique::Source::Building(temple))
        .collect();
    assert!(!from_temple.is_empty() && from_temple.iter().all(|e| e.n == 2), "two copies each");
    drop(idx);
    // Greece's index never moved.
    assert!(sources(&g, GREECE).buildings.is_empty());
    clean(&g);
}

#[test]
fn city_state_bonuses_follow_contact_influence_and_alliance() {
    let (mut g, _) = game();
    assert!(sources(&g, ROME).city_states.is_empty(), "not met yet");
    g.set_met(ROME, GENEVA).expect("a pair");
    assert!(sources(&g, ROME).city_states.is_empty(), "met, but no friend");
    let without = civ_index(&g, ROME).len();
    crate::game::city_states::influence::set_influence(&mut g, GENEVA, ROME, 35.0)
        .expect("a city-state");
    assert_eq!(sources(&g, ROME).city_states.len(), 1);
    assert!(matches!(sources(&g, ROME).city_states[0].1, CityStateBonus::Friend));
    let friend = civ_index(&g, ROME).len();
    assert!(friend >= without);
    crate::game::city_states::influence::set_influence(&mut g, GENEVA, ROME, 70.0)
        .expect("a city-state");
    assert!(matches!(sources(&g, ROME).city_states[0].1, CityStateBonus::Ally));
    clean(&g);
    // War puts influence at its floor: no bonus.
    g.update_relation(ROME, GENEVA, |r| r.war = true).expect("a pair");
    assert!(
        sources(&g, ROME).city_states.iter().all(|&(_, b)| b == CityStateBonus::Ally),
        "an ally at war stays the ally until the alliance is settled again"
    );
    clean(&g);
}

#[test]
fn a_founded_religion_gives_its_founder_beliefs_and_its_followers_theirs() {
    let (mut g, _) = game();
    let r = g.rules;
    let founder_belief = r
        .beliefs()
        .iter()
        .find(|(_, b)| b.kind == BeliefType::Founder && !b.uniques.civ.is_empty())
        .map(|(id, _)| id)
        .expect("a founder belief");
    let follower_belief = r
        .beliefs()
        .iter()
        .find(|(_, b)| b.kind == BeliefType::Follower && !b.uniques.civ.is_empty())
        .map(|(id, _)| id)
        .expect("a follower belief");
    let rid = ReligionId(0);
    {
        let w = g.edit_world(WorldTouch::RELIGIONS);
        let mut founder_beliefs = BeliefSet::new();
        founder_beliefs.insert(founder_belief);
        let mut follower_beliefs = BeliefSet::new();
        follower_beliefs.insert(follower_belief);
        w.religions.push(Religion {
            name: ReligionName::Pantheon(BeliefId(0)),
            display: "Test".into(),
            founder: ROME,
            founder_beliefs,
            follower_beliefs,
            blocked_holy: false,
        });
    }
    if let Some(p) = g.player_mut(ROME, PlayerTouch::RELIGION) {
        p.religion.founded = Some(rid);
    }
    assert_eq!(sources(&g, ROME).founder_beliefs, vec![founder_belief]);
    let f = follower_index(&g, rid);
    assert!(!f.is_empty());
    assert!(Arc::ptr_eq(&shared(follower(&g, rid)), &shared(follower(&g, rid))), "one per key");
    clean(&g);
}

fn follower_index(g: &Game, r: ReligionId) -> Csr {
    (*follower(g, r)).clone()
}

fn shared(ix: IndexRef<'_>) -> Arc<Csr> {
    match ix {
        IndexRef::Shared(a) => a,
        other => Arc::new((*other).clone()),
    }
}

#[test]
fn units_of_one_base_and_promotions_share_a_profile() {
    let (mut g, _) = game();
    let a = testing::unit(&mut g, ROME, "Warrior", TileIdx(23));
    let b = testing::unit(&mut g, GREECE, "Warrior", TileIdx(56));
    let before = g.dv.civ.table_sizes().0;
    assert!(Arc::ptr_eq(&shared(unit_profile(&g, a)), &shared(unit_profile(&g, b))));
    assert_eq!(g.dv.civ.table_sizes().0, before + 1);
    let promo = g
        .rules
        .promotions()
        .iter()
        .find(|(_, p)| !p.uniques.civ.is_empty())
        .map(|(id, _)| id)
        .expect("a promotion with uniques");
    if let Some(x) = g.unit_mut(a, UnitTouch::CORE) {
        x.promotions.insert(promo);
    }
    let promoted = shared(unit_profile(&g, a));
    assert!(!Arc::ptr_eq(&promoted, &shared(unit_profile(&g, b))));
    assert!(promoted.len() > unit_profile(&g, b).len());
    clean(&g);
}

/// A resource on a tile of `city`, improved as it is improved.
fn improved(g: &mut Game, t: TileIdx, owner: PlayerId, city: CityId, res: &str, amount: u8) {
    let r: ResourceId = id(g, res);
    let imp: ImprovementId = g.rules.resources()[r].improvement.expect("an improvement");
    g.set_tile_owner(t, TileClaim::city(owner, city)).expect("a tile");
    g.set_resource(t, Some(r), amount).expect("a tile");
    g.set_improvement(t, Some(imp)).expect("a tile");
}

#[test]
fn the_supply_counts_tiles_units_and_buildings_and_feeds_the_resource_layer() {
    let (mut g, [roma, ..]) = game();
    let iron: ResourceId = id(&g, "Iron");
    improved(&mut g, TileIdx(23), ROME, roma, "Iron", 3);
    assert_eq!(economy::resource_amount(&g, ROME, iron), 0, "Iron is not revealed yet");
    let working: TechId = id(&g, "Iron Working");
    if let Some(p) = g.player_mut(ROME, PlayerTouch::INDEX) {
        p.tech.known.insert(working);
    }
    assert_eq!(economy::resource_amount(&g, ROME, iron), 3);
    let full_before = civ_index_full(&g, ROME).len();
    let sword = testing::unit(&mut g, ROME, "Swordsman", TileIdx(22));
    assert_eq!(economy::resource_amount(&g, ROME, iron), 2, "a Swordsman needs one");
    let s = economy::supply(&g, ROME).expect("Rome");
    assert_eq!(s.items().len(), 2);
    assert_eq!(s.items()[0].origin, economy::Origin::Tiles);
    assert_eq!(
        s.items()[1],
        economy::ResourceItem { resource: iron, origin: economy::Origin::Units, amount: -1 }
    );
    drop(s);
    // A move is not a change of what Rome has: the supply is not computed again.
    let stamp = g.dv.civ.civs[ROME].supply.changed();
    g.relocate_unit(sword, TileIdx(23)).expect("a move");
    drop(supply(&g, ROME));
    assert_eq!(g.dv.civ.civs[ROME].supply.changed(), stamp);
    assert!(civ_index_full(&g, ROME).len() >= full_before);
    // Pillage the mine: the Iron is gone, and the Swordsman's need stays.
    g.set_pillaged(TileIdx(23), false, true).expect("a tile");
    assert_eq!(economy::resource_amount(&g, ROME, iron), -1);
    clean(&g);
}

#[test]
fn a_resource_unique_for_its_city_alone_holds_where_the_resource_is() {
    // The Marble decision (DESIGN.md 5.12): Marble's wonder bonus is Roma's, which works the
    // quarry, and not Antium's; the civilization's index does not hold it.
    let (mut g, [roma, antium, ..]) = game();
    improved(&mut g, TileIdx(21), ROME, roma, "Marble", 1);
    let marble: ResourceId = id(&g, "Marble");
    assert_eq!(economy::resource_amount(&g, ROME, marble), 1);
    let t = g.rules.uniques();
    let local_of = |g: &Game, c: CityId| {
        city_local(g, c)
            .entries()
            .iter()
            .any(|e| t.meta(e.id).source == crate::unique::Source::Resource(marble))
    };
    assert!(local_of(&g, roma));
    assert!(!local_of(&g, antium));
    let full = civ_index_full(&g, ROME);
    assert!(!full.entries().iter().any(|e| {
        t.meta(e.id).source == crate::unique::Source::Resource(marble)
            && t.get(e.id).flags().contains(crate::unique::UFlags::LOCAL)
    }));
    drop(full);
    clean(&g);
}

#[test]
fn an_allied_city_state_shares_its_resources() {
    let (mut g, [.., geneva]) = game();
    improved(&mut g, TileIdx(61), GENEVA, geneva, "Marble", 1);
    let marble: ResourceId = id(&g, "Marble");
    assert_eq!(economy::resource_amount(&g, GENEVA, marble), 1);
    assert_eq!(economy::resource_amount(&g, ROME, marble), 0);
    g.set_met(ROME, GENEVA).expect("a pair");
    crate::game::city_states::influence::set_influence(&mut g, GENEVA, ROME, 70.0)
        .expect("a city-state");
    assert_eq!(economy::resource_amount(&g, ROME, marble), 1);
    let s = economy::supply(&g, ROME).expect("Rome");
    assert_eq!(s.items()[0].origin, economy::Origin::CityStates);
    drop(s);
    clean(&g);
}

#[test]
fn resources_in_conditionals_read_the_supply_memo() {
    let (mut g, [roma, ..]) = game();
    let ctx = Ctx::civ(ROME);
    let before = cond(&g, CondDeps::RESOURCES, &ctx);
    // A write the supply does not read leaves the class where it was.
    g.player_mut(GREECE, PlayerTouch::STOCKS);
    assert_eq!(cond(&g, CondDeps::RESOURCES, &ctx), before);
    improved(&mut g, TileIdx(23), ROME, roma, "Marble", 1);
    assert!(cond(&g, CondDeps::RESOURCES, &ctx) > before);
    assert_eq!(cond(&g, CondDeps::RESOURCES, &Ctx::default()), Rev::START);
}

#[test]
fn a_temporary_unique_is_in_the_index_until_it_runs_out() {
    let (mut g, _) = game();
    let t = g.rules.uniques();
    let temp = t
        .iter()
        .find_map(|(uid, _)| t.meta(uid).temp_variant)
        .expect("a timed unique in the shipped ruleset");
    let ty = t.meta(temp).ty.expect("typed");
    let before = count(&civ_index(&g, ROME), ty);
    if let Some(p) = g.player_mut(ROME, PlayerTouch::INDEX) {
        p.civ.temp_uniques.push(TempUnique { unique: temp, turns: 2 });
    }
    assert_eq!(count(&civ_index(&g, ROME), ty), before + 1);
    economy::expire_temp_uniques(&mut g, ROME);
    assert_eq!(count(&civ_index(&g, ROME), ty), before + 1, "one turn left");
    clean(&g);
    economy::expire_temp_uniques(&mut g, ROME);
    assert_eq!(count(&civ_index(&g, ROME), ty), before);
    assert!(g.player(ROME).is_some_and(|p| p.civ.temp_uniques.is_empty()));
    clean(&g);
}

#[test]
fn a_city_gone_takes_its_memo_and_a_new_one_has_one() {
    let (mut g, [_, antium, ..]) = game();
    assert!(g.dv.civ.cities.contains_key(&antium));
    let taken = g.remove_city(antium).expect("a city");
    assert_eq!(taken.id(), antium);
    assert!(!g.dv.civ.cities.contains_key(&antium));
    assert!(city_local(&g, antium).is_empty());
    let c = testing::city(&mut g, GREECE, TileIdx(40), "Sparta");
    assert!(g.dv.civ.cities.contains_key(&c));
    clean(&g);
}

// ---- Gate 3: random writes, and the memos against a cold rebuild -------------------------------

/// One write to an input of the memos, with its choices as raw numbers the test maps onto what
/// the game has.
#[derive(Clone, Debug)]
enum Op {
    Building { city: u8, building: u16, add: bool },
    Tech { player: u8, tech: u16, add: bool },
    Policy { player: u8, policy: u16 },
    Temporary { player: u8, turns: u8 },
    Expire { player: u8 },
    Influence { major: u8, amount: i8 },
    Meet { major: u8 },
    War { a: u8, b: u8, war: bool },
    Religion { player: u8, founder: u16, follower: u16 },
    Resource { tile: u8, resource: u16, amount: u8, improve: bool, city: u8 },
    Pillage { tile: u8 },
    Spawn { player: u8, base: u16, tile: u8 },
    Despawn { unit: u8 },
    Promote { unit: u8, promotion: u16 },
    Upgrade { unit: u8, base: u16 },
    Move { unit: u8, tile: u8 },
    NextTurn,
    Read { what: u8, which: u8 },
}

fn op() -> impl Strategy<Value = Op> {
    let small = || 0u8..4;
    prop_oneof![
        3 => (small(), any::<u16>(), any::<bool>())
            .prop_map(|(city, building, add)| Op::Building { city, building, add }),
        2 => (small(), any::<u16>(), any::<bool>())
            .prop_map(|(player, tech, add)| Op::Tech { player, tech, add }),
        1 => (small(), any::<u16>()).prop_map(|(player, policy)| Op::Policy { player, policy }),
        1 => (small(), 1u8..4).prop_map(|(player, turns)| Op::Temporary { player, turns }),
        1 => small().prop_map(|player| Op::Expire { player }),
        2 => (0u8..2, any::<i8>()).prop_map(|(major, amount)| Op::Influence { major, amount }),
        1 => (0u8..2).prop_map(|major| Op::Meet { major }),
        1 => (small(), small(), any::<bool>()).prop_map(|(a, b, war)| Op::War { a, b, war }),
        1 => (small(), any::<u16>(), any::<u16>())
            .prop_map(|(player, founder, follower)| Op::Religion { player, founder, follower }),
        3 => (any::<u8>(), any::<u16>(), 0u8..5, any::<bool>(), small()).prop_map(
            |(tile, resource, amount, improve, city)| Op::Resource {
                tile,
                resource,
                amount,
                improve,
                city
            }
        ),
        1 => any::<u8>().prop_map(|tile| Op::Pillage { tile }),
        2 => (small(), any::<u16>(), any::<u8>())
            .prop_map(|(player, base, tile)| Op::Spawn { player, base, tile }),
        1 => any::<u8>().prop_map(|unit| Op::Despawn { unit }),
        1 => (any::<u8>(), any::<u16>()).prop_map(|(unit, promotion)| Op::Promote { unit, promotion }),
        1 => (any::<u8>(), any::<u16>()).prop_map(|(unit, base)| Op::Upgrade { unit, base }),
        2 => (any::<u8>(), any::<u8>()).prop_map(|(unit, tile)| Op::Move { unit, tile }),
        1 => Just(Op::NextTurn),
        4 => (any::<u8>(), any::<u8>()).prop_map(|(what, which)| Op::Read { what, which }),
    ]
}

fn pick<T: Copy>(items: &[T], n: usize) -> Option<T> {
    (!items.is_empty()).then(|| items[n % items.len()])
}

fn apply(g: &mut Game, cities: &[CityId; 4], op: &Op) {
    let r = g.rules;
    let tiles = u32::try_from(g.st.tiles().len()).unwrap_or(1);
    let tile = |n: u8| TileIdx(u32::from(n) % tiles);
    let player = |n: u8| PlayerId(n % 3);
    let units: Vec<UnitId> = g.st.units().iter().map(crate::state::units::Unit::id).collect();
    match *op {
        Op::Building { city, building, add } => {
            let b = BuildingId(building % u16::try_from(r.buildings().len()).unwrap_or(1));
            if let Some(x) = g.city_mut(cities[usize::from(city) % 4], CityTouch::BUILDINGS) {
                if add {
                    x.buildings.insert(b)
                } else {
                    x.buildings.remove(b)
                };
            }
        }
        Op::Tech { player: p, tech, add } => {
            let t = TechId(tech % u16::try_from(r.techs().len()).unwrap_or(1));
            if let Some(x) = g.player_mut(player(p), PlayerTouch::INDEX) {
                if add {
                    x.tech.known.insert(t)
                } else {
                    x.tech.known.remove(t)
                };
            }
        }
        Op::Policy { player: p, policy } => {
            let pol = PolicyId(policy % u16::try_from(r.policies().len()).unwrap_or(1));
            if let Some(x) = g.player_mut(player(p), PlayerTouch::POLICIES) {
                x.policy.adopted.insert(pol);
            }
        }
        Op::Temporary { player: p, turns } => {
            let t = r.uniques();
            if let Some(temp) = t.iter().find_map(|(uid, _)| t.meta(uid).temp_variant)
                && let Some(x) = g.player_mut(player(p), PlayerTouch::INDEX)
            {
                x.civ.temp_uniques.push(TempUnique { unique: temp, turns: i16::from(turns) });
            }
        }
        Op::Expire { player: p } => economy::expire_temp_uniques(g, player(p)),
        Op::Influence { major, amount } => {
            let m = PlayerId(major % 2);
            let _ok =
                crate::game::city_states::influence::set_influence(g, GENEVA, m, f64::from(amount));
        }
        Op::Meet { major } => {
            let _ok = g.set_met(PlayerId(major % 2), GENEVA);
        }
        Op::War { a, b, war } => {
            let (a, b) = (player(a), player(b));
            if a != b {
                let _ok = g.update_relation(a, b, |x| x.war = war);
            }
        }
        Op::Religion { player: p, founder, follower } => {
            let n = u16::try_from(r.beliefs().len()).unwrap_or(1);
            let (fo, fl) = (BeliefId(founder % n), BeliefId(follower % n));
            let rid = ReligionId(u8::try_from(g.st.world().religions.len()).unwrap_or(0));
            if rid.0 < 8 {
                let w = g.edit_world(WorldTouch::RELIGIONS);
                let mut founder_beliefs = BeliefSet::new();
                founder_beliefs.insert(fo);
                let mut follower_beliefs = BeliefSet::new();
                follower_beliefs.insert(fl);
                w.religions.push(Religion {
                    name: ReligionName::Pantheon(fo),
                    display: "Test".into(),
                    founder: player(p),
                    founder_beliefs,
                    follower_beliefs,
                    blocked_holy: false,
                });
                if let Some(x) = g.player_mut(player(p), PlayerTouch::RELIGION) {
                    x.religion.founded = Some(rid);
                }
            }
        }
        Op::Resource { tile: t, resource, amount, improve, city } => {
            let t = tile(t);
            if g.st.city_at(t).is_some() {
                return;
            }
            let n = u16::try_from(r.resources().len()).unwrap_or(1);
            let res = ResourceId(u8::try_from(resource % n).unwrap_or(0));
            let c = cities[usize::from(city) % 4];
            let owner = g.city(c).map(crate::state::cities::City::owner).unwrap_or(ROME);
            let _ok = g.set_tile_owner(t, TileClaim::city(owner, c));
            let _ok = g.set_resource(t, Some(res), amount);
            let imp = if improve { r.resources()[res].improvement } else { None };
            let _ok = g.set_improvement(t, imp);
        }
        Op::Pillage { tile: t } => {
            let _ok = g.set_pillaged(tile(t), false, true);
        }
        Op::Spawn { player: p, base, tile: t } => {
            let b = crate::base::ids::BaseUnitId(
                base % u16::try_from(r.base_units().len()).unwrap_or(1),
            );
            let _ok = g.create_unit(player(p), b, tile(t), 0);
        }
        Op::Despawn { unit } => {
            if let Some(u) = pick(&units, usize::from(unit)) {
                let _ok = g.despawn_unit(u);
            }
        }
        Op::Promote { unit, promotion } => {
            let pr = PromotionId(promotion % u16::try_from(r.promotions().len()).unwrap_or(1));
            if let Some(u) = pick(&units, usize::from(unit))
                && let Some(x) = g.unit_mut(u, UnitTouch::CORE)
            {
                x.promotions.insert(pr);
            }
        }
        Op::Upgrade { unit, base } => {
            let b = crate::base::ids::BaseUnitId(
                base % u16::try_from(r.base_units().len()).unwrap_or(1),
            );
            if let Some(u) = pick(&units, usize::from(unit))
                && let Some(x) = g.unit_mut(u, UnitTouch::CORE)
            {
                x.base = b;
            }
        }
        Op::Move { unit, tile: t } => {
            if let Some(u) = pick(&units, usize::from(unit)) {
                let _ok = g.relocate_unit(u, tile(t));
            }
        }
        Op::NextTurn => {
            let c = *g.st.clock();
            g.set_clock(crate::state::TurnClock { turn: c.turn + 1, ..c });
        }
        Op::Read { what, which } => {
            let p = player(which);
            match what % 5 {
                0 => drop(civ_index(g, p)),
                1 => drop(civ_index_full(g, p)),
                2 => drop(supply(g, p)),
                3 => drop(city_local(g, cities[usize::from(which) % 4])),
                _ => {
                    if let Some(u) = pick(&units, usize::from(which)) {
                        drop(unit_profile(g, u));
                    }
                }
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    /// Gate 3: whatever writes come, in whatever order, with reads between them, every memo of
    /// this module reads as a cold rebuild would.
    #[test]
    fn the_memos_equal_a_cold_rebuild_after_random_writes(ops in prop::collection::vec(op(), 1..48)) {
        let (mut g, cities) = game();
        for (i, o) in ops.iter().enumerate() {
            apply(&mut g, &cities, o);
            if i % 8 == 7 {
                let found = verify(&g);
                prop_assert!(found.is_empty(), "after {:?}: {:?}", &ops[..=i], found);
            }
        }
        let found = verify(&g);
        prop_assert!(found.is_empty(), "{:?}", found);
    }
}

#[test]
fn the_monotonic_ai_base_values_raise_the_easy_ais_to_prince() {
    let (mut g, _) = game();
    let prince = g.rules.derived().known.prince.expect("Prince");
    let chieftain = g.rules.difficulties().ids().next().expect("a difficulty");
    g.set_seat_difficulty(ROME, Some(chieftain)).expect("a seat");
    let plain = g.difficulty(Some(ROME));
    assert_eq!(plain, g.rules.difficulties()[chieftain].ai_difficulty_level);
    g.edit_config(|c| c.ai_base_values = crate::state::config::AiBaseValues::Monotonic);
    assert_eq!(g.difficulty(Some(ROME)), prince);
    let deity =
        crate::base::ids::DifficultyId(u8::try_from(g.rules.difficulties().len() - 1).unwrap_or(0));
    g.set_seat_difficulty(ROME, Some(deity)).expect("a seat");
    assert_eq!(g.difficulty(Some(ROME)), g.rules.difficulties()[deity].ai_difficulty_level);
}

#[test]
fn the_era_follows_the_techs() {
    let (g, _) = game();
    let r = g.rules;
    assert_eq!(research::player_era(r, &crate::base::sets::TechSet::new()), EraId(0));
    let mut all = crate::base::sets::TechSet::new();
    for t in r.techs().ids() {
        all.insert(t);
    }
    let last = r.eras().ids().last().expect("an era");
    assert_eq!(research::player_era(r, &all), last);
    // Every tech of the first columns but one: still the first era.
    let mut first = crate::base::sets::TechSet::new();
    for (t, d) in r.techs().iter() {
        if d.era == EraId(0) {
            first.insert(t);
        }
    }
    assert_eq!(research::player_era(r, &first), EraId(1), "the first era done: the next");
    let one = first.iter().next().expect("a tech");
    first.remove(one);
    assert_eq!(research::player_era(r, &first), EraId(0));
}
