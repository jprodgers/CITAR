//! The city-states' rules on the unit tests' small game: Rome and Greece, Geneva, and the
//! barbarians on a 10x8 grassland map. Every test ends with the checks clean.

#![allow(
    clippy::float_cmp,
    reason = "the numbers the rules give here are exact: whole, halves and tenths"
)]

use super::actions::{
    CityStatePlan, gift_gold, marry, plan_gift_gold, plan_marriage, plan_tribute, pledge,
    tribute_modifiers, tribute_willingness,
};
use super::influence::{
    Relationship, data, data_mut, pair, pair_mut, raw_influence, relationship, resting_point,
    set_influence,
};
use super::quests::{camp_cleared, complete_quests, quest_target, quests_end_turn};
use super::turn::{
    barbarian_killed_near, end_turn, great_person_gift_tick, on_attacked, on_military_unit_killed,
};
use crate::base::ids::{BaseUnitId, PlayerId, QuestKindId, TechId, TileIdx};
use crate::game::Game;
use crate::game::core::testing;
use crate::game::derive::rev::PlayerTouch;
use crate::game::espionage::{self, city_state_election_tick, coup_chance, hold_elections};
use crate::rules::defs::{CityStatePersonality, QuestKind, QuestScope};
use crate::state::TurnClock;
use crate::state::players::{Quest, QuestTarget, SpyAction};

const ROME: PlayerId = PlayerId(0);
const GREECE: PlayerId = PlayerId(1);
const GENEVA: PlayerId = PlayerId(2);
const BARBS: PlayerId = PlayerId(3);

fn set_turn(g: &mut Game, turn: i32) {
    let clock = TurnClock { turn, ..*g.state().clock() };
    g.set_clock(clock);
}

fn clean(g: &mut Game) {
    g.settle();
    let v = g.take_violations();
    assert!(v.is_empty(), "violations: {v:?}");
    let broken = g.check_invariants();
    assert!(broken.is_empty(), "{broken:?}");
    let stale = g.verify_caches();
    assert!(stale.is_empty(), "{stale:?}");
}

/// Everyone but the barbarians knows Agriculture, the first era's starting tech (the Warrior's).
fn agriculture(g: &mut Game) {
    let t = g.rules().lookup::<TechId>("Agriculture").expect("Agriculture");
    for p in [ROME, GREECE, GENEVA] {
        crate::game::research::add_tech_silently(g, p, &[t]);
    }
}

/// The small game with Geneva a city-state of `kind`, neutral, with a city at (4,3) and its ring;
/// Rome's city at (1,1), Greece's at (8,6), with theirs; everyone met, and knowing Agriculture.
fn game(kind: &str) -> Game {
    let mut g = testing::duel();
    agriculture(&mut g);
    let t = g
        .rules()
        .city_state_types()
        .iter()
        .find(|(_, d)| &*d.name == kind)
        .map(|(id, _)| id)
        .expect("a city-state type");
    if let Some(d) = data_mut(&mut g, GENEVA) {
        d.cs_type = Some(t);
        d.personality = Some(CityStatePersonality::Neutral);
    }
    for (p, t, name) in [(ROME, 11, "Roma"), (GREECE, 68, "Athenai"), (GENEVA, 34, "Geneva")] {
        let c = testing::city(&mut g, p, TileIdx(t), name);
        let ring: Vec<TileIdx> = g.grid().neighbors(TileIdx(t)).collect();
        for n in ring {
            crate::game::cities::borders::take_ownership(&mut g, c, n);
        }
    }
    for (a, b) in [(ROME, GREECE), (ROME, GENEVA), (GREECE, GENEVA)] {
        g.set_met(a, b).expect("two players");
    }
    g.settle();
    clean(&mut g);
    g
}

fn gold(g: &Game, p: PlayerId) -> f64 {
    g.player(p).map_or(0.0, |x| x.econ.gold)
}

fn row(g: &Game, kind: QuestKind) -> QuestKindId {
    g.rules().quests().iter().find(|(_, d)| d.kind == kind).map(|(k, _)| k).expect("a quest")
}

fn quest(g: &Game, kind: QuestKind, to: PlayerId, target: QuestTarget) -> Quest {
    let k = row(g, kind);
    let d = &g.rules().quests()[k];
    Quest {
        kind: k,
        assignee: to,
        turn: g.turn(),
        scope: d.scope,
        target,
        influence: d.influence.unwrap_or(40),
        duration: d.duration.unwrap_or(0),
    }
}

#[test]
fn a_first_contact_brings_a_gift_the_first_major_most() {
    let mut g = testing::duel();
    let (a, b) = (gold(&g, ROME), gold(&g, GREECE));
    g.make_contact(ROME, GENEVA);
    g.make_contact(GENEVA, GREECE);
    assert_eq!(gold(&g, ROME) - a, 30.0);
    assert_eq!(gold(&g, GREECE) - b, 15.0);
    // An aggressor gets nothing.
    let mut g = testing::duel();
    super::turn::on_attacked(&mut g, GENEVA, ROME);
    let a = gold(&g, ROME);
    g.make_contact(ROME, GENEVA);
    assert_eq!(gold(&g, ROME), a);
    clean(&mut g);
}

#[test]
fn influence_drifts_to_its_resting_point() {
    let mut g = game("Cultured");
    set_influence(&mut g, GENEVA, ROME, 50.0).expect("a city-state");
    set_influence(&mut g, GENEVA, GREECE, -20.0).expect("a city-state");
    assert_eq!(relationship(&g, GENEVA, ROME), Relationship::Friend);
    end_turn(&mut g, GENEVA);
    assert_eq!(raw_influence(&g, GENEVA, ROME), 49.0);
    // Greece's influence recovers at twice the normal rate.
    assert_eq!(raw_influence(&g, GENEVA, GREECE), -18.0);
    // A hostile city-state forgets faster, and an aggressor faster still unless it is hostile.
    if let Some(d) = data_mut(&mut g, GENEVA) {
        d.personality = Some(CityStatePersonality::Hostile);
    }
    end_turn(&mut g, GENEVA);
    assert_eq!(raw_influence(&g, GENEVA, ROME), 47.5);
    // A protector rests at 10, a civilization it is wary of 20 lower.
    pledge(&mut g, GREECE, GENEVA);
    assert_eq!(resting_point(&g, GENEVA, GREECE), 10.0);
    if let Some(p) = pair_mut(&mut g, GENEVA, GREECE) {
        p.wary = true;
    }
    assert_eq!(resting_point(&g, GENEVA, GREECE), -10.0);
    clean(&mut g);
}

#[test]
fn a_gift_of_gold_buys_influence_that_falls_as_the_game_goes_on() {
    let mut g = game("Cultured");
    if let Some(p) = g.player_mut(ROME, PlayerTouch::STOCKS) {
        p.econ.gold = 2000.0;
    }
    let (gift, inf) = plan_gift_gold(&g, ROME, GENEVA, 250).expect("a gift");
    assert_eq!((gift, inf), (250, 25));
    set_turn(&mut g, 200);
    assert_eq!(plan_gift_gold(&g, ROME, GENEVA, 250).map(|x| x.1), Ok(15));
    assert_eq!(plan_gift_gold(&g, ROME, GENEVA, 1000).map(|x| x.1), Ok(70));
    let refused = plan_gift_gold(&g, ROME, GENEVA, 5000).expect_err("too much");
    assert_eq!(refused.message, "You only have 2000 gold.");
    let out = gift_gold(&mut g, ROME, GENEVA, 250, 15);
    assert_eq!(out["influence_gained"], 15);
    assert_eq!(gold(&g, ROME), 1750.0);
    assert_eq!(gold(&g, GENEVA), 250.0);
    clean(&mut g);
}

#[test]
fn tribute_is_paid_to_the_feared_and_not_again_soon() {
    let mut g = game("Cultured");
    assert!(tribute_willingness(&g, GENEVA, ROME, false) <= 0);
    let names: Vec<&str> = tribute_modifiers(&g, GENEVA, ROME, false).iter().map(|m| m.0).collect();
    assert_eq!(names, ["Base value", "Military Rank", "Military near City-State"]);
    for t in [33, 35, 44, 25] {
        testing::unit(&mut g, ROME, "Swordsman", TileIdx(t));
    }
    assert!(tribute_willingness(&g, GENEVA, ROME, false) > 0);
    assert_eq!(relationship(&g, GENEVA, ROME), Relationship::Afraid);
    plan_tribute(&g, ROME, GENEVA, false).expect("afraid");
    let before = gold(&g, ROME);
    let out = super::actions::tribute(&mut g, ROME, GENEVA, false);
    assert_eq!(out["tribute"], "50 gold");
    assert_eq!(gold(&g, ROME) - before, 50.0);
    assert_eq!(raw_influence(&g, GENEVA, ROME), -15.0);
    assert_eq!(pair(&g, GENEVA, ROME).bullied, 20);
    let e = plan_tribute(&g, ROME, GENEVA, false).expect_err("recently");
    assert!(e.message.contains("Very recently paid tribute -300"), "{}", e.message);
    // A small city-state gives no worker.
    let e = plan_tribute(&g, GREECE, GENEVA, true).expect_err("small");
    assert!(e.message.contains("Demanding a Worker from small City-State -300"));
    clean(&mut g);
}

#[test]
fn quests_are_given_from_turn_30_rewarded_and_dropped() {
    let mut g = game("Cultured");
    for turn in 29..=60 {
        set_turn(&mut g, turn);
        quests_end_turn(&mut g, GENEVA);
        if turn == 29 {
            assert!(data(&g, GENEVA).is_some_and(|d| d.timers.global == -1), "not before 30");
        }
    }
    let d = data(&g, GENEVA).expect("a city-state");
    for m in [ROME, GREECE] {
        let theirs: Vec<&Quest> = d
            .quests
            .iter()
            .filter(|q| q.assignee == m && q.scope == QuestScope::Individual)
            .collect();
        assert!(!theirs.is_empty() && theirs.len() <= 2, "{m:?}: {theirs:?}");
    }
    for q in &d.quests {
        let def = &g.rules().quests()[q.kind];
        assert_eq!(q.target.kind(), def.target, "{}", def.name);
        assert_eq!(q.scope, def.scope);
    }
    // The same question, asked twice, has the same answer.
    let k = row(&g, QuestKind::AcquireGreatPerson);
    assert_eq!(quest_target(&g, GENEVA, k, ROME), quest_target(&g, GENEVA, k, ROME));
    clean(&mut g);
}

#[test]
fn a_quest_is_completed_as_it_happens() {
    let mut g = game("Cultured");
    let give = quest(&g, QuestKind::GiveGold, ROME, QuestTarget::Player(GREECE));
    let pledged = quest(&g, QuestKind::PledgeToProtect, GREECE, QuestTarget::Player(ROME));
    if let Some(d) = data_mut(&mut g, GENEVA) {
        d.quests = vec![give, pledged];
    }
    complete_quests(&mut g, GENEVA, ROME, QuestKind::GiveGold);
    assert_eq!(raw_influence(&g, GENEVA, ROME), 20.0);
    assert_eq!(data(&g, GENEVA).map(|d| d.quests.len()), Some(1));
    pledge(&mut g, GREECE, GENEVA);
    assert_eq!(raw_influence(&g, GENEVA, GREECE), 20.0);
    assert!(data(&g, GENEVA).is_some_and(|d| d.quests.is_empty()));
    // A camp cleared rewards the one that cleared it and ends the quest for everyone.
    let t = TileIdx(7);
    let camp = |to| quest(&g, QuestKind::ClearBarbarianCamp, to, QuestTarget::Tile(t));
    let (a, b) = (camp(ROME), camp(GREECE));
    if let Some(d) = data_mut(&mut g, GENEVA) {
        d.quests = vec![a, b];
    }
    camp_cleared(&mut g, t, GREECE);
    assert_eq!(raw_influence(&g, GENEVA, GREECE), 70.0);
    assert_eq!(raw_influence(&g, GENEVA, ROME), 20.0);
    assert!(data(&g, GENEVA).is_some_and(|d| d.quests.is_empty()));
    clean(&mut g);
}

#[test]
fn a_contest_goes_to_the_best_score() {
    let mut g = game("Cultured");
    let mut a = quest(&g, QuestKind::ContestCulture, ROME, QuestTarget::Baseline(0));
    let mut b = quest(&g, QuestKind::ContestCulture, GREECE, QuestTarget::Baseline(0));
    a.duration = 1;
    b.duration = 1;
    let influence = a.influence;
    if let Some(d) = data_mut(&mut g, GENEVA) {
        d.quests = vec![a, b];
    }
    for (p, total) in [(ROME, 10), (GREECE, 25)] {
        if let Some(x) = g.player_mut(p, PlayerTouch::OTHER) {
            x.econ.total_culture = total;
        }
    }
    set_turn(&mut g, 5);
    quests_end_turn(&mut g, GENEVA);
    assert_eq!(raw_influence(&g, GENEVA, GREECE), f64::from(influence));
    assert_eq!(raw_influence(&g, GENEVA, ROME), 0.0);
    assert!(data(&g, GENEVA).is_some_and(|d| d.quests.is_empty()));
    clean(&mut g);
}

#[test]
fn an_attack_is_remembered_and_its_killers_rewarded() {
    let mut g = game("Cultured");
    pledge(&mut g, GREECE, GENEVA);
    on_attacked(&mut g, GENEVA, ROME);
    let d = data(&g, GENEVA).expect("a city-state");
    assert_eq!(d.war_quests.get(&ROME).map(|w| w.needed), Some(3));
    assert_eq!(pair(&g, GENEVA, ROME).recently_attacked, 2);
    assert_eq!(crate::game::diplomacy::relations::opinion(&g, GREECE, ROME), -20.0);
    for _ in 0..2 {
        on_military_unit_killed(&mut g, GREECE, ROME);
    }
    assert_eq!(raw_influence(&g, GENEVA, GREECE), 0.0);
    on_military_unit_killed(&mut g, GREECE, ROME);
    assert_eq!(raw_influence(&g, GENEVA, GREECE), 100.0);
    assert!(data(&g, GENEVA).is_some_and(|d| d.war_quests.is_empty()));
    // A barbarian killed near it earns thanks, and forgiveness for units in its land.
    barbarian_killed_near(&mut g, ROME, TileIdx(35));
    assert_eq!(raw_influence(&g, GENEVA, ROME), 12.0);
    assert_eq!(pair(&g, GENEVA, ROME).anger_free, 5);
    clean(&mut g);
}

#[test]
fn a_militaristic_friend_is_gifted_units() {
    let mut g = game("Militaristic");
    set_influence(&mut g, GENEVA, ROME, 100.0).expect("a city-state");
    let before = g.player_units(ROME).count();
    let mut gifted_at = None;
    for turn in 1..=40 {
        set_turn(&mut g, turn);
        end_turn(&mut g, GENEVA);
        if g.player_units(ROME).count() > before {
            gifted_at = Some(turn);
            break;
        }
    }
    let turn = gifted_at.expect("a unit within 40 turns");
    assert!((15..=18).contains(&turn), "every 17 turns, give or take one: {turn}");
    let gift = g.player_units(ROME).last().map(|u| u.base).expect("a unit");
    assert!(g.rules().base_units()[gift].military);
    assert!(pair(&g, GENEVA, ROME).unit_timer.is_none());
    clean(&mut g);
}

#[test]
fn allied_city_states_give_great_people() {
    let mut g = game("Cultured");
    set_influence(&mut g, GENEVA, ROME, 100.0).expect("a city-state");
    if let Some(m) = g.player_mut(ROME, PlayerTouch::OTHER).and_then(|p| p.major.as_deref_mut()) {
        m.cs_gp_gift = Some(1);
    }
    let before = g.player_units(ROME).count();
    great_person_gift_tick(&mut g, ROME);
    let after: Vec<BaseUnitId> = g.player_units(ROME).map(|u| u.base).collect();
    assert_eq!(after.len(), before + 1);
    assert!(after.last().is_some_and(|&u| g.rules().base_units()[u].great_person));
    let next = g.player(ROME).and_then(|p| p.major.as_deref()).and_then(|m| m.cs_gp_gift);
    assert!(next.is_some_and(|n| (37..=43).contains(&n)), "{next:?}");
    // Without allies the countdown waits.
    set_influence(&mut g, GENEVA, ROME, 0.0).expect("a city-state");
    great_person_gift_tick(&mut g, ROME);
    assert_eq!(g.player(ROME).and_then(|p| p.major.as_deref()).and_then(|m| m.cs_gp_gift), next);
    clean(&mut g);
}

#[test]
fn a_city_state_learns_what_most_majors_know() {
    let mut g = game("Cultured");
    let pottery = g.rules().lookup::<TechId>("Pottery").expect("Pottery");
    crate::game::research::add_tech_silently(&mut g, ROME, &[pottery]);
    end_turn(&mut g, GENEVA);
    assert!(!g.has_tech(GENEVA, Some(pottery)), "one of two is no majority");
    crate::game::research::add_tech_silently(&mut g, GREECE, &[pottery]);
    end_turn(&mut g, GENEVA);
    assert!(g.has_tech(GENEVA, Some(pottery)));
    clean(&mut g);
}

#[test]
fn military_units_in_its_land_anger_a_city_state() {
    let mut g = game("Cultured");
    testing::unit(&mut g, ROME, "Warrior", TileIdx(35));
    end_turn(&mut g, GENEVA);
    assert_eq!(raw_influence(&g, GENEVA, ROME), -10.0);
    assert_eq!(pair(&g, GENEVA, ROME).border_conflict, 10, "counted down from next turn");
    // A barbarian near it takes its mind off them.
    testing::unit(&mut g, BARBS, "Warrior", TileIdx(36));
    end_turn(&mut g, GENEVA);
    assert_eq!(raw_influence(&g, GENEVA, ROME), -9.0);
    clean(&mut g);
}

#[test]
fn a_long_time_ally_may_be_married() {
    let mut g = game("Cultured");
    let austria = g.rules().lookup::<crate::base::ids::NationId>("Austria").expect("Austria");
    let err = plan_marriage(&g, ROME, GENEVA).expect_err("no ally");
    assert_eq!(err.message, "They must be your ally.");
    if let Some(p) = g.player_mut(ROME, PlayerTouch::INDEX) {
        p.nation = austria;
    }
    set_influence(&mut g, GENEVA, ROME, 100.0).expect("a city-state");
    assert_eq!(pair(&g, GENEVA, ROME).marriage_cooldown, 5);
    let err = plan_marriage(&g, ROME, GENEVA).expect_err("too soon");
    assert_eq!(err.message, "You must be allied for 5 more turns.");
    if let Some(p) = pair_mut(&mut g, GENEVA, ROME) {
        p.marriage_cooldown = 0;
    }
    let cost = super::actions::marriage_cost(&g, GENEVA);
    assert_eq!(cost, 500);
    let err = plan_marriage(&g, ROME, GENEVA).expect_err("no gold");
    assert_eq!(err.message, "This costs 500 gold.");
    if let Some(p) = g.player_mut(ROME, PlayerTouch::STOCKS) {
        p.econ.gold = 600.0;
    }
    assert_eq!(plan_marriage(&g, ROME, GENEVA), Ok(500));
    let c = g.player(GENEVA).and_then(|p| p.capital).expect("a capital");
    marry(&mut g, ROME, GENEVA, 500);
    let city = g.city(c).expect("the city");
    assert_eq!((city.owner(), city.founder, city.puppet), (ROME, ROME, true));
    assert_eq!(gold(&g, ROME), 100.0);
    g.settle();
    clean(&mut g);
}

#[test]
fn elections_are_won_by_riggers_or_nobody() {
    let mut g = game("Cultured");
    let cap = g.player(GENEVA).and_then(|p| p.capital).expect("a capital");
    let i = espionage::add_spy(&mut g, ROME).expect("a spy");
    if let Some(s) = g
        .player_mut(ROME, PlayerTouch::SPIES)
        .and_then(|p| p.major.as_deref_mut())
        .and_then(|m| m.spies.get_mut(i))
    {
        s.city = Some(cap);
        s.action = SpyAction::RiggingElections;
    }
    set_influence(&mut g, GENEVA, ROME, 40.0).expect("a city-state");
    set_influence(&mut g, GENEVA, GREECE, 10.0).expect("a city-state");
    // The first tick schedules the first election.
    city_state_election_tick(&mut g, GENEVA);
    assert!(data(&g, GENEVA).and_then(|d| d.election_in).is_some());
    let (r0, g0) = (raw_influence(&g, GENEVA, ROME), raw_influence(&g, GENEVA, GREECE));
    hold_elections(&mut g, GENEVA);
    let (r1, g1) = (raw_influence(&g, GENEVA, ROME), raw_influence(&g, GENEVA, GREECE));
    assert!(
        (r1 - r0, g1 - g0) == (20.0, -5.0) || (r1 - r0, g1 - g0) == (-5.0, 0.0),
        "the rigger wins, or nobody: {:?}",
        (r1 - r0, g1 - g0)
    );
    let n = g.rules().constants().formulas.city_state_election_turns;
    assert_eq!(data(&g, GENEVA).and_then(|d| d.election_in), i16::try_from(n).ok());
    clean(&mut g);
}

#[test]
fn a_coup_makes_its_civilization_the_ally_or_kills_the_spy() {
    let mut g = game("Cultured");
    let cap = g.player(GENEVA).and_then(|p| p.capital).expect("a capital");
    set_influence(&mut g, GENEVA, GREECE, 60.0).expect("a city-state");
    set_influence(&mut g, GENEVA, ROME, 50.0).expect("a city-state");
    assert_eq!(data(&g, GENEVA).and_then(crate::state::players::CityStateData::ally), Some(GREECE));
    let i = espionage::add_spy(&mut g, ROME).expect("a spy");
    let place = |g: &mut Game, action| {
        if let Some(s) = g
            .player_mut(ROME, PlayerTouch::SPIES)
            .and_then(|p| p.major.as_deref_mut())
            .and_then(|m| m.spies.get_mut(i))
        {
            s.city = Some(cap);
            s.action = action;
        }
    };
    place(&mut g, SpyAction::RiggingElections);
    let s = espionage::spies(&g, ROME)[i].clone();
    assert!(espionage::can_coup(&g, ROME, &s));
    // 50% less half of the 10 points between them, and half the spy's skill.
    assert_eq!(coup_chance(&g, ROME, &s, false), 0.6);
    let mut won = None;
    for turn in 1..=20 {
        set_turn(&mut g, turn);
        set_influence(&mut g, GENEVA, GREECE, 60.0).expect("a city-state");
        set_influence(&mut g, GENEVA, ROME, 50.0).expect("a city-state");
        place(&mut g, SpyAction::Coup);
        espionage::end_turn(&mut g, ROME);
        let action = espionage::spies(&g, ROME)[i].action;
        if action == SpyAction::RiggingElections {
            won = Some(turn);
            break;
        }
        assert_eq!(action, SpyAction::Dead, "a failed coup kills the spy");
    }
    won.expect("one of twenty coups at 60% succeeds");
    assert_eq!(data(&g, GENEVA).and_then(crate::state::players::CityStateData::ally), Some(ROME));
    assert_eq!(raw_influence(&g, GENEVA, ROME), 60.0);
    assert_eq!(raw_influence(&g, GENEVA, GREECE), 40.0);
    clean(&mut g);
}

#[test]
fn a_city_state_founds_its_city_and_builds_a_defender() {
    let mut g = testing::duel();
    agriculture(&mut g);
    let settler = testing::unit(&mut g, GENEVA, "Settler", TileIdx(34));
    g.settle();
    super::ai::take_turn(&mut g, GENEVA);
    assert!(g.unit(settler).is_none(), "the settler founded the city");
    let c = g.player_cities(GENEVA).next().map(crate::state::cities::City::id).expect("a city");
    let first = g.city(c).and_then(|x| x.queue.first().copied());
    assert!(
        matches!(first, Some(crate::state::cities::Constructible::Unit(u)) if g.rules().base_units()[u].military),
        "{first:?}"
    );
    g.settle();
    clean(&mut g);
}

#[test]
fn a_plan_names_what_the_tool_will_do() {
    let g = game("Cultured");
    let a = super::actions::CityStateAction {
        player_id: 2,
        action: serde_json::json!("PLEDGE"),
        amount: None,
        unit_id: None,
    };
    assert_eq!(
        crate::game::action::Rule::check(&a, &g, ROME).ok(),
        Some(CityStatePlan::Pledge(GENEVA))
    );
}

#[test]
fn the_barbarians_are_no_common_enemy() {
    // refcheck: city-state-gifts-need-a-common-nation
    let mut g = game("Militaristic");
    assert!(g.at_war(ROME, BARBS) && g.at_war(GENEVA, BARBS));
    assert!(!super::turn::common_enemy(&g, ROME, GENEVA));
    use crate::game::diplomacy::relations::{WarReason, set_war};
    set_war(&mut g, ROME, GREECE, WarReason::Scenario).expect("two players");
    assert!(!super::turn::common_enemy(&g, ROME, GENEVA));
    set_war(&mut g, GENEVA, GREECE, WarReason::Scenario).expect("two players");
    assert!(super::turn::common_enemy(&g, ROME, GENEVA));
    clean(&mut g);
}
