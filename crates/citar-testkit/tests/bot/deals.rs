//! One `basic-1` turn's diplomacy, and what a deal is worth to the bot (package 2-05): what the
//! rule scripts cannot show, since they can neither give a seat a memory nor read it.
//!
//! - A war being prepared names its target and rally point in memory, gathers the army there,
//!   then is declared with a war plan for the army (`memory.war_plan`).
//! - What peace, friendship and a war on someone are worth moves with the war the seat plans or
//!   prepares (`evaluate`, basic.py:2558-2643), and the advice shows the war it prepares.
//! - The luxury trades and the advice's wants visit the civilizations met in an order: player-id
//!   order in play, Python's order when the reference checks ask (`decisions::ask_in_order`).
//! - Typed city-state gifts go where they buy the most; idle spies watch the capital furthest
//!   ahead.
//! - Parameters of 0 that divided in Python (`diplo_every`, `lux_trade_every`,
//!   `war_need_city_div`) play a turn.

use std::sync::Arc;

use citar_bot::decisions::{Question, ask, ask_in_order};
use citar_bot::memory::{WarPlan, WarPrep};
use citar_bot::{Bot, BotSpec, Memory, Refusals, Tuning, VersionId, advice, clean, evaluate};
use citar_engine::api::testops;
use citar_engine::base::ids::{CityId, PlayerId};
use citar_engine::game::city_states::influence::influence;
use citar_engine::game::espionage::spies;
use citar_engine::game::setup::config_from_value;
use citar_engine::game::{
    DebugOptions, DriveOptions, DriverOutcome, Drivers, Game, SeatDriver, Stop,
};
use citar_engine::rules::Ruleset;
use citar_engine::state::diplo::{DealItem, NegStatus};
use citar_engine::state::players::DriverMemory;
use citar_testkit::script::map_doc;
use serde_json::{Value, json};

const ME: PlayerId = PlayerId(0);
const YOU: PlayerId = PlayerId(1);
const THEM: PlayerId = PlayerId(2);

/// The arena with `majors` civilizations and `city_states` city-states and nothing on it,
/// every check on.
fn arena(majors: usize, city_states: u32) -> Game {
    let (doc, _) = map_doc("arena").expect("the arena");
    let seat = json!({"controller": "bot"});
    let cfg = json!({"seed": 11, "map": doc, "players": vec![seat; majors],
                     "city_states": city_states, "barbarians": "off", "ruins": false});
    let r = Ruleset::shared();
    let (mut g, _) = Game::new(r, &config_from_value(r, cfg).expect("settings")).expect("a game");
    g.set_debug_options(DebugOptions::ALL);
    testops::apply(&mut g, &json!([{"op": "clear_units", "player": "all"}])).expect("bare");
    g
}

fn ops(g: &mut Game, ops: &Value) -> Vec<Value> {
    g.apply_ops(ops).unwrap_or_else(|e| panic!("{e:?}")).0
}

/// The cities of the arena's first three seats: A (5,5), B (18,10) and C (18,4).
fn cities(g: &mut Game, majors: u8) -> Vec<CityId> {
    let at = [(5, 5), (18, 10), (18, 4)];
    (0..majors)
        .map(|p| {
            let (x, y) = at[usize::from(p)];
            let out = ops(
                g,
                &json!([{"op": "found_city", "player": p, "x": x, "y": y,
                                       "capital": true, "pop": 3}]),
            );
            out[0]["city_id"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .and_then(CityId::new)
                .unwrap_or_else(|| panic!("no city in {}", out[0]))
        })
        .collect()
}

/// A `basic-1` spec with `params` over its defaults, `tech_noise` 0 and the draws pinned as the
/// rule scripts pin them (no peace, friendship or war unless `params` says so).
fn spec(params: &Value, aggression: Option<f64>) -> Arc<BotSpec> {
    let mut p = json!({"tech_noise": 0, "ranged_chance": 0, "peace_offer_chance": 0,
                       "friend_chance": 0, "friend_chance_aggr": 0, "war_chance": 0,
                       "war_chance_aggr": 0});
    if let (Some(base), Some(more)) = (p.as_object_mut(), params.as_object()) {
        base.extend(more.clone());
    }
    let o = clean("basic-1", &p).expect("parameters");
    let tuning = Arc::new(Tuning::new(VersionId::Basic1, o));
    Arc::new(BotSpec::new(VersionId::Basic1, tuning, None, aggression))
}

/// Plays my turn with a bot of `spec`, as the rule scripts' bot step does (a negotiation it
/// opened that waits on a seat nobody drives is closed when the drive stops for it, as a host's
/// wait runs out), then the other seats' turns, so that it is my turn again. Gives the actions
/// the bot took and had refused.
fn round(g: &mut Game, spec: &Arc<BotSpec>) -> Refusals {
    let n = g.state().players().len();
    let mut b = Bot::new(Arc::clone(spec));
    for _ in 0..100 {
        let mut d = Drivers::none(n).with(ME, &mut b);
        let (stop, _) = g.drive(&mut d, DriveOptions::default().with_seat_limit(1)).expect("live");
        drop(d);
        let Stop::AwaitingReply { nids, .. } = stop else { break };
        for nid in nids {
            g.close_negotiation(nid, NegStatus::Expired, "No answer came.", None).expect("open");
        }
    }
    assert_ne!(g.current(), ME, "my turn ended");
    let v = g.take_violations();
    assert!(v.is_empty(), "{v:?}");
    while g.current() != ME {
        g.end_turn(g.current()).expect("the others end their turns");
    }
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
    b.refusals().clone()
}

/// What my seat remembers.
fn memory(g: &Game) -> Memory {
    g.player(ME).and_then(|p| p.seat().driver()).map(Memory::decode).unwrap_or_default()
}

/// A seat driver that gives the seat a memory, and plays nothing.
struct Recall(Memory);

impl SeatDriver for Recall {
    fn play_turn(&mut self, _: &mut Game, _: PlayerId, mem: &mut DriverMemory) -> DriverOutcome {
        *mem = self.0.encode().expect("a small memory");
        DriverOutcome::Done
    }

    fn respond(
        &mut self,
        _: &mut Game,
        _: PlayerId,
        _: citar_engine::base::ids::NegotiationId,
        _: &mut DriverMemory,
    ) -> DriverOutcome {
        DriverOutcome::Done
    }
}

/// Gives my seat `m` as its memory (through a turn of mine, as only a driver writes it), and
/// comes back to my turn.
fn remember(g: &mut Game, m: Memory) {
    let n = g.state().players().len();
    let mut r = Recall(m);
    let mut d = Drivers::none(n).with(ME, &mut r);
    g.drive(&mut d, DriveOptions::default().with_seat_limit(1)).expect("live");
    drop(d);
    while g.current() != ME {
        g.end_turn(g.current()).expect("the others end their turns");
    }
}

#[test]
fn a_war_prepared_gathers_its_army_at_the_rally_then_is_declared_with_a_plan() {
    let mut g = arena(2, 0);
    let homes = cities(&mut g, 2);
    // A garrison and three more warriors at home; nothing at B.
    ops(
        &mut g,
        &json!([
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 5, "y": 5},
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 6, "y": 5},
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 6, "y": 6},
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 4, "y": 5},
            {"op": "meet", "a": 0, "b": 1},
            {"op": "reveal", "player": 0},
        ]),
    );
    let eager = spec(
        &json!({"war_chance": 1, "war_min_turn": 0, "war_power_ratio": 0, "war_power_ratio_aggr": 0,
                "war_max_threat": 100, "declare_ratio": 0, "declare_ratio_aggr": 0,
                "war_need_min": 2, "war_need_max": 2, "war_need_base": 2, "prep_gather": true,
                "overwhelm_units": 99}),
        Some(1.0),
    );
    let target = g.city(homes[1]).map(|c| c.tile()).expect("B");
    round(&mut g, &eager);
    let prep = memory(&g).war_prep.expect("a war being prepared");
    assert_eq!((prep.player, prep.target), (YOU, Some(target)));
    let rally = prep.rally.expect("a rally point");
    assert!(!g.at_war(ME, YOU), "the army has not gathered yet");
    let mut declared = None;
    for _ in 0..20 {
        let turn = g.turn();
        round(&mut g, &eager);
        if g.at_war(ME, YOU) {
            declared = Some(turn);
            break;
        }
        let m = memory(&g);
        assert_eq!(
            m.war_prep.as_ref().map(|w| (w.target, w.rally)),
            Some((Some(target), Some(rally)))
        );
    }
    let turn = declared.expect("war was declared once the army gathered");
    assert_eq!(g.relation(ME, YOU).and_then(|r| r.war_declared_by), Some(ME));
    let m = memory(&g);
    assert_eq!(m.war_prep, None, "the preparation is over");
    let plan = m.war_plan.expect("a war plan");
    assert_eq!(
        (plan.city, plan.since, plan.advance, plan.rally),
        (target, turn, true, Some(rally))
    );
    let near = g.player_units(ME).filter(|u| g.grid().distance(u.tile(), rally) <= 2).count();
    assert!(near >= 2, "{near} units at the rally");
}

#[test]
fn what_a_deal_is_worth_follows_the_war_the_bot_plans_or_prepares() {
    let mut g = arena(3, 0);
    let homes = cities(&mut g, 3);
    ops(
        &mut g,
        &json!([
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 5, "y": 5},
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 6, "y": 5},
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 4, "y": 5},
            {"op": "meet", "a": 0, "b": "all"},
            {"op": "set_player", "player": 0, "gold": 300},
        ]),
    );
    let s = spec(&json!({}), None);
    let p = s.tuning.params();
    let peace = [DealItem::PeaceTreaty];
    let friends = [DealItem::DeclarationOfFriendship];
    let war_on_them = [DealItem::DeclareWar { target: THEM }];
    let worth =
        |g: &Game, give: &[DealItem], receive: &[DealItem]| evaluate(g, &s, ME, YOU, give, receive);
    // Fresh: far stronger than they are, peace costs its strength; a peace treaty, listed on
    // both sides, counts on each (the port keeps Python's weighting).
    let strong = -2.0 * f64::from(p.deal_peace_when_strong);
    assert!((worth(&g, &peace, &peace) - strong).abs() < 1e-9);
    assert!((worth(&g, &[], &friends) - f64::from(p.deal_friendship)).abs() < 1e-9);
    let fresh_war = worth(&g, &war_on_them, &[]);
    assert!(fresh_war < 0.0, "a war it gives costs it: {fresh_war}");
    // A war planned on their city, begun this turn: peace while winning costs that, each side.
    let b = g.city(homes[1]).map(|c| c.tile()).expect("B");
    let plan = WarPlan {
        city: b,
        since: g.turn(),
        advance: true,
        checked: None,
        rally: None,
        siege_ready: false,
    };
    remember(&mut g, Memory { war_plan: Some(plan), ..Memory::default() });
    let winning = -2.0 * f64::from(p.deal_peace_while_winning);
    assert!((worth(&g, &peace, &peace) - winning).abs() < 1e-9);
    // A war prepared on the third civilization: friendship is refused, and a war on them is
    // cheaper, since it meant to fight them anyway.
    let c = g.city(homes[2]).map(|c| c.tile()).expect("C");
    let prep = WarPrep { player: THEM, since: g.turn(), target: Some(c), rally: None };
    remember(&mut g, Memory { war_prep: Some(prep), ..Memory::default() });
    assert!((worth(&g, &[], &friends) + f64::from(p.deal_friendship_refuse)).abs() < 1e-9);
    let planned = worth(&g, &war_on_them, &[]);
    assert!((planned - fresh_war * p.deal_war_planned_share).abs() < 1e-6, "{planned} {fresh_war}");
    // The advice shows the war it prepares, and writes nothing.
    let before = g.digest().expect("a digest");
    let a = advice(&g, ME, &s, None);
    assert_eq!(a.war_readiness.len(), 1, "{a:?}");
    let w = &a.war_readiness[0];
    assert_eq!((w.player, w.at_war, w.preparing, w.army_gathered), (THEM, false, true, false));
    assert_eq!(g.digest().expect("a digest"), before);
}

/// Two improved cottons at each of `tiles` for `owners`, with Calendar.
fn cotton(g: &mut Game, owners: &[(u8, [(i32, i32); 2])]) {
    for &(p, at) in owners {
        ops(g, &json!([{"op": "grant_tech", "player": p, "tech": "Calendar"}]));
        for (x, y) in at {
            ops(
                g,
                &json!([{"op": "set_tile", "x": x, "y": y, "resource": "Cotton",
                            "improvement": "Plantation"}]),
            );
        }
    }
}

#[test]
fn luxuries_and_wants_follow_the_order_the_civilizations_are_visited_in() {
    // I have cotton to spare; they have none, and gold.
    let mut g = arena(3, 0);
    cities(&mut g, 3);
    ops(
        &mut g,
        &json!([{"op": "meet", "a": 0, "b": "all"},
                        {"op": "set_player", "player": "all", "gold": 300}]),
    );
    cotton(&mut g, &[(0, [(6, 5), (4, 5)])]);
    let to =
        |v: &Value| v.as_array().map(|a| a.iter().map(|o| o["to"].clone()).collect::<Vec<_>>());
    let offers = ask(&g, ME, Question::LuxTrade);
    assert_eq!(to(&offers), Some(vec![json!(1)]), "{offers}");
    assert_eq!(offers[0]["give"], json!([{"type": "resource", "resource": "Cotton", "amount": 1}]));
    let reversed = ask_in_order(&g, ME, Question::LuxTrade, &[THEM, YOU]);
    assert_eq!(to(&reversed), Some(vec![json!(2)]), "{reversed}");
    // They have cotton to spare and I have none: I want it from whoever comes first.
    let mut g = arena(3, 0);
    cities(&mut g, 3);
    ops(&mut g, &json!([{"op": "meet", "a": 0, "b": "all"}]));
    cotton(&mut g, &[(1, [(19, 10), (17, 10)]), (2, [(19, 4), (17, 4)])]);
    let wants = |v: Value| v["none"]["wants"].clone();
    assert_eq!(
        wants(ask(&g, ME, Question::Advice)),
        json!([{"type": "resource", "resource": "Cotton", "from": 1}])
    );
    assert_eq!(
        wants(ask_in_order(&g, ME, Question::Advice, &[THEM, YOU])),
        json!([{"type": "resource", "resource": "Cotton", "from": 2}])
    );
}

/// The arena with two civilizations and two city-states, 2 (11,7) and 3 (13,12), which I have
/// met, standing with them as `influence` says, and rich.
fn courting(influence: [i32; 2]) -> Game {
    let mut g = arena(2, 2);
    cities(&mut g, 2);
    ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 2, "x": 11, "y": 7, "capital": true},
            {"op": "found_city", "player": 3, "x": 13, "y": 12, "capital": true},
            {"op": "meet", "a": 0, "b": 2},
            {"op": "meet", "a": 0, "b": 3},
            {"op": "set_influence", "city_state": 2, "player": 0, "amount": influence[0]},
            {"op": "set_influence", "city_state": 3, "player": 0, "amount": influence[1]},
            {"op": "set_player", "player": 0, "gold": 2000},
        ]),
    );
    g
}

#[test]
fn a_typed_gift_goes_where_it_buys_the_most_and_never_to_a_safe_ally() {
    let (near, far) = (PlayerId(2), PlayerId(3));
    let typed = spec(&json!({"cs_gift_mode": "typed"}), None);
    let classic = spec(&json!({}), None);
    // A gift takes 2 past the friend threshold and leaves 3 far below it.
    let mut g = courting([25, -40]);
    let done = round(&mut g, &typed);
    assert_eq!(done.of("city_state_action"), (1, 0));
    assert!(influence(&g, near, ME) > 40.0, "{}", influence(&g, near, ME));
    assert!(influence(&g, far, ME) < -30.0, "{}", influence(&g, far, ME));
    // Safely allied with 2, nothing to gain with 3: the typed gift keeps its gold, where the
    // classic one gives to the city-state it stands best with.
    let mut g = courting([100, -40]);
    assert_eq!(
        citar_engine::game::city_states::influence::data(&g, near).and_then(|d| d.ally()),
        Some(ME)
    );
    assert_eq!(round(&mut g, &typed).of("city_state_action"), (0, 0));
    let mut g = courting([100, -40]);
    assert_eq!(round(&mut g, &classic).of("city_state_action"), (1, 0));
    assert!(influence(&g, near, ME) > 100.0, "{}", influence(&g, near, ME));
}

#[test]
fn idle_spies_watch_the_capital_furthest_ahead_then_the_next() {
    let mut g = arena(3, 0);
    let homes = cities(&mut g, 3);
    ops(
        &mut g,
        &json!([
            {"op": "meet", "a": 0, "b": "all"},
            {"op": "reveal", "player": 0},
            {"op": "grant_tech", "player": 2,
             "techs": ["Pottery", "Mining", "Archery", "Animal Husbandry"]},
        ]),
    );
    testops::apply(
        &mut g,
        &json!([{"op": "add_spy", "player": 0}, {"op": "add_spy", "player": 0}]),
    )
    .expect("two spies");
    round(&mut g, &spec(&json!({}), None));
    let at: Vec<Option<CityId>> = spies(&g, ME).iter().map(|s| s.city).collect();
    assert_eq!(at, vec![Some(homes[2]), Some(homes[1])]);
}

#[test]
fn parameters_of_zero_that_python_divided_by_play_a_turn() {
    let mut g = arena(2, 0);
    cities(&mut g, 2);
    ops(
        &mut g,
        &json!([
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 6, "y": 5},
            {"op": "meet", "a": 0, "b": 1},
            {"op": "reveal", "player": 0},
            {"op": "grant_tech", "player": "all", "tech": "Writing"},
        ]),
    );
    let zero = spec(
        &json!({"diplo_every": 0, "lux_trade_every": 0, "war_need_city_div": 0, "war_chance": 1,
                "war_min_turn": 0, "war_power_ratio": 0, "war_power_ratio_aggr": 0,
                "war_max_threat": 100, "declare_ratio": 99, "declare_ratio_aggr": 0}),
        None,
    );
    round(&mut g, &zero);
    // Every turn is an agreements turn: the embassies were proposed.
    let opened: Vec<_> = g.negotiations().iter().filter(|n| n.initiator == ME).collect();
    assert_eq!(opened.len(), 1, "{opened:?}");
    assert!(
        opened[0].history[0].proposal.as_ref().is_some_and(|t| t.gives(ME) == [DealItem::Embassy])
    );
    // A war is prepared, and not declared (the declare ratio is out of reach).
    assert!(memory(&g).war_prep.is_some());
    assert!(!g.at_war(ME, YOU));
}
