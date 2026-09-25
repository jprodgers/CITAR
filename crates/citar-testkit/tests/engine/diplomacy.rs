//! Diplomacy and espionage (package 1c-05), on the arena, with every check on after each call:
//! - deals carried out: a luxury traded for some turns and cut short when its giver can no longer
//!   supply it, gold every turn, open borders, a city, a map, a war joined, and what each leaves
//!   when the round ends (`process_round`);
//! - research agreements, which bank both sides' science and pay each the smaller sum the round
//!   after they end;
//! - the kitchen sink's extras: `upon declaring friendship` and `upon declaring a defensive pact`
//!   fire for both parties of such a deal, `[n]% spy effectiveness [cities]` speeds its spies, and
//!   `Spies in [cities] cities act as though they have [n] levels for [action]` raises a spy's
//!   rank where it applies, and nowhere else;
//! - spies: stealing technology, reproducibly, spies fleeing a captured city, a dead spy
//!   replaced;
//! - the host's `close_negotiation` and `open_negotiation_as`, and the negotiation view;
//! - the invariants DIPLO-1 and NEG-1 under random negotiation sequences (gate 4).

use citar_engine::api::{inspect, testops};
use citar_engine::base::ids::{CityId, NegotiationId, PlayerId, ResourceId};
use citar_engine::game::diplomacy::actions::{
    DeclareWar, Denounce, EndTurn, OpenNegotiation, RespondNegotiation, SendMessage,
};
use citar_engine::game::diplomacy::category::{Category, proposal_categories};
use citar_engine::game::diplomacy::deals;
use citar_engine::game::espionage::{self, MoveSpy};
use citar_engine::game::invariants::Code;
use citar_engine::game::{Action, DebugOptions, ErrCode, Game, economy};
use citar_engine::rules::{Named, Ruleset};
use citar_engine::state::diplo::{DealItem, NegStatus};
use citar_engine::state::players::SpyAction;
use citar_testkit::rulesets::kitchen_sink;
use citar_testkit::script::{map_doc, new_game};
use proptest::prelude::*;
use serde_json::{Value, json};

const ME: PlayerId = PlayerId(0);
const YOU: PlayerId = PlayerId(1);
const THIRD: PlayerId = PlayerId(2);

fn id<I: Named>(r: &Ruleset, name: &str) -> I {
    r.lookup::<I>(name).unwrap_or_else(|| panic!("the ruleset has {name}"))
}

/// A bare game on the arena with these nations, espionage on, no unit, every check on.
fn arena(r: &'static Ruleset, nations: &[&str]) -> Game {
    let (doc, _) = map_doc("arena").expect("the arena");
    let players: Vec<Value> = nations.iter().map(|n| json!({"nation": n})).collect();
    let cfg = json!({
        "seed": 1,
        "players": players,
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

fn shipped(n: usize) -> Game {
    arena(Ruleset::shared(), &vec!["BenchmarkCiv"; n])
}

fn ops(g: &mut Game, v: &Value) -> Vec<Value> {
    g.apply_ops(v).unwrap_or_else(|e| panic!("{v}: {e}")).0
}

fn test_ops(g: &mut Game, v: &Value) -> Vec<Value> {
    testops::apply(g, v).unwrap_or_else(|e| panic!("{v}: {e}")).0
}

fn act(g: &mut Game, p: PlayerId, a: Action) -> Value {
    let what = format!("{a:?}");
    g.act(p, a).unwrap_or_else(|e| panic!("{what}: {}", e.message)).0
}

fn refused(g: &mut Game, p: PlayerId, a: Action) -> String {
    let what = format!("{a:?}");
    match g.act(p, a) {
        Ok((v, _)) => panic!("{what} should be refused, and did {v}"),
        Err(e) => e.message,
    }
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

fn open(to: PlayerId, give: Value, receive: Value) -> Action {
    Action::OpenNegotiation(OpenNegotiation {
        to: i64::from(to.0),
        message: json!("A deal?"),
        give: Some(give),
        receive: Some(receive),
    })
}

fn answer(nid: i64, action: &str) -> Action {
    Action::RespondNegotiation(RespondNegotiation {
        negotiation_id: nid,
        action: json!(action),
        message: Some(json!("So be it.")),
        give: None,
        receive: None,
    })
}

/// `me` offers `give` for `receive`, and the other side accepts: the deal's id.
fn deal(g: &mut Game, me: PlayerId, to: PlayerId, give: Value, receive: Value) -> u32 {
    let opened = act(g, me, open(to, give, receive));
    let nid = opened["negotiation_id"].as_i64().expect("an id");
    let done = act(g, to, answer(nid, "accept"));
    assert_eq!(done["status"], "accepted", "{done}");
    let n = g.negotiation(NegotiationId::new(u32::try_from(nid).expect("small")).expect("an id"));
    n.and_then(|n| n.deal).map(|d| d.get()).expect("a deal")
}

/// Asserts a whole amount exactly, which no rounding touches.
#[track_caller]
fn exactly(got: f64, want: f64) {
    assert!((got - want).abs() < 1e-9, "{got}, not {want}");
}

/// Embassies both ways, which the shipped ruleset's `Requires establishing embassies to conduct
/// advanced diplomacy` asks before open borders, research agreements and defensive pacts.
fn embassies(g: &mut Game, a: PlayerId, b: PlayerId) {
    deal(g, a, b, json!([{"type": "embassy"}]), json!([{"type": "embassy"}]));
}

fn gold(g: &Game, p: PlayerId) -> f64 {
    g.player(p).map_or(0.0, |x| x.econ.gold)
}

fn end_round(g: &mut Game) {
    test_ops(g, &json!([{"op": "end_round"}]));
}

fn events_of(g: &Game, kind: &str) -> Vec<Value> {
    inspect::inspect(g, &json!({"what": "events", "type": kind}))
        .expect("events")
        .as_array()
        .cloned()
        .unwrap_or_default()
}

// ---- Deals ---------------------------------------------------------------------------------------

#[test]
fn a_luxury_trade_runs_its_turns_and_is_cut_short_when_its_giver_runs_out() {
    let mut g = shipped(2);
    let r = g.rules();
    let cotton: ResourceId = id(r, "Cotton");
    ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 4, "name": "Roma"},
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "name": "Veii"},
            {"op": "set_tile", "x": 4, "y": 3, "improvement": "Plantation"},
            {"op": "meet", "a": 0, "b": 1},
        ]),
    );
    assert_eq!(economy::resource_amount(&g, ME, cotton), 1, "the cotton by Roma, improved");
    let refusal = refused(
        &mut g,
        ME,
        open(YOU, json!([{"type": "resource", "resource": "Cotton", "amount": 2}]), json!([])),
    );
    assert_eq!(refusal, "Civilization 1 does not have 2 spare Cotton (has 1).");
    deal(
        &mut g,
        ME,
        YOU,
        json!([{"type": "resource", "resource": "cotton", "turns": 5}]),
        json!([]),
    );
    assert_eq!(economy::resource_amount(&g, ME, cotton), 0);
    assert_eq!(economy::resource_amount(&g, YOU, cotton), 1);
    let d = g.state().diplo().deals[0].clone();
    assert!(d.active);
    assert_eq!(d.ongoing.len(), 1);
    assert_eq!(d.ongoing[0].until, 6, "the turn it was made, and five more");
    end_round(&mut g);
    assert!(g.state().diplo().deals[0].active, "a trade that still runs");
    // The plantation goes: the giver has none to give, and the trade is cut at the round's end.
    ops(&mut g, &json!([{"op": "set_tile", "x": 4, "y": 3, "improvement": null}]));
    assert_eq!(economy::resource_amount(&g, ME, cotton), -1);
    end_round(&mut g);
    let d = g.state().diplo().deals[0].clone();
    assert!(!d.active, "every recurring part has run out, so the deal expired");
    assert_eq!(d.ongoing[0].until, 1, "cut to the turn before the round that cut it");
    let cut = events_of(&g, "deal_cut");
    assert_eq!(cut.len(), 1);
    assert_eq!(
        cut[0]["text"],
        "A trade of Cotton between Civilization 1 and Civilization 2 was cut short."
    );
    assert_eq!(cut[0]["audience"], json!([0, 1]));
    let expired = events_of(&g, "deal_expired");
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0]["text"], "A deal between Civilization 1 and Civilization 2 has expired.");
    assert_eq!(economy::resource_amount(&g, YOU, cotton), 0);
    clean(&mut g);
}

#[test]
fn gold_every_turn_flows_until_its_turns_are_up_and_a_one_off_deal_ends_with_its_round() {
    let mut g = shipped(2);
    ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Roma"},
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "name": "Veii"},
            {"op": "meet", "a": 0, "b": 1},
            {"op": "set_player", "player": 0, "gold": 100},
        ]),
    );
    let net =
        citar_engine::game::query::civ_stats(&g, ME).total[citar_engine::base::stats::Stat::Gold];
    let too_much = net.trunc() + 1.0;
    let refusal = refused(
        &mut g,
        ME,
        open(YOU, json!([{"type": "gold_per_turn", "amount": too_much}]), json!([])),
    );
    assert!(refusal.contains("gold per turn; cannot pay"), "{refusal}");
    // A gift of 1 gold a turn for 2 turns.
    deal(&mut g, ME, YOU, json!([{"type": "gold_per_turn", "amount": 1, "turns": 2}]), json!([]));
    exactly(economy::deal_gold_per_turn(&g, ME), -1.0);
    exactly(economy::deal_gold_per_turn(&g, YOU), 1.0);
    // A lump sum: the deal has nothing that recurs, and ends with the round.
    deal(&mut g, ME, YOU, json!([{"type": "gold", "amount": 10}]), json!([]));
    exactly(gold(&g, ME), 90.0);
    end_round(&mut g);
    let deals = &g.state().diplo().deals;
    assert!(deals[0].active && !deals[1].active);
    end_round(&mut g);
    end_round(&mut g);
    assert!(g.state().diplo().deals[0].active, "its last turn, the third");
    end_round(&mut g);
    assert!(!g.state().diplo().deals[0].active, "the round after its last turn");
    exactly(economy::deal_gold_per_turn(&g, YOU), 0.0);
    assert_eq!(events_of(&g, "deal_expired").len(), 1, "the one-off deal ended without a word");
    clean(&mut g);
}

#[test]
fn open_borders_a_city_a_map_and_a_war_change_hands() {
    let mut g = shipped(3);
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Roma"},
            {"op": "found_city", "player": 0, "x": 5, "y": 11, "name": "Ostia"},
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "name": "Veii"},
            {"op": "found_city", "player": 2, "x": 18, "y": 4, "name": "Capua"},
            {"op": "meet", "a": 0, "b": "all"},
            {"op": "meet", "a": 1, "b": 2},
            {"op": "grant_tech", "player": [0, 1], "tech": "Civil Service"},
        ]),
    );
    let (roma, ostia) = (city_of(&out[0]), city_of(&out[1]));
    let refusal =
        refused(&mut g, ME, open(YOU, json!([{"type": "city", "city_id": roma.get()}]), json!([])));
    assert_eq!(refusal, "Capitals cannot be traded.");
    let refusal =
        refused(&mut g, ME, open(YOU, json!([{"type": "city", "city_id": 999}]), json!([])));
    assert_eq!(refusal, "Civilization 1 does not own city 999.");
    let refusal = refused(&mut g, ME, open(YOU, json!([{"type": "open_borders"}]), json!([])));
    assert_eq!(refusal, "Open borders require embassies in each other's capitals first.");
    embassies(&mut g, ME, YOU);
    let explored = |g: &Game, p: PlayerId| g.player(p).map_or(0, |x| x.explored.len());
    let before = explored(&g, YOU);
    deal(
        &mut g,
        ME,
        YOU,
        json!([{"type": "city", "city_id": ostia.get()}, {"type": "share_map"}, {"type": "open_borders", "turns": 3}]),
        json!([{"type": "declare_war", "target": 2}]),
    );
    assert_eq!(g.city(ostia).map(|c| c.owner()), Some(YOU));
    assert!(explored(&g, YOU) > before, "the map shows what Rome explored");
    assert!(g.has_open_borders(ME, YOU) && !g.has_open_borders(YOU, ME));
    assert!(g.at_war(YOU, THIRD), "the war agreed to");
    let wars = events_of(&g, "war_declared");
    assert_eq!(
        wars.last().map(|e| e["text"].clone()),
        Some(json!(
            "Civilization 2 declared war on Civilization 3 (as agreed with Civilization 1)!"
        ))
    );
    // The borders close at the end of the round after their last turn.
    for _ in 0..5 {
        end_round(&mut g);
    }
    assert!(!g.has_open_borders(ME, YOU));
    let rel =
        inspect::inspect(&g, &json!({"what": "relation", "a": 0, "b": 1})).expect("a relation");
    assert_eq!(rel["open_borders_until"], json!([0, 0]), "a lapsed term is none");
    clean(&mut g);
}

/// A war one side of a deal would declare on a civilization with a defensive pact with the other
/// side is refused, proposed or accepted: the pact would put the deal's parties at war with the
/// deal in force and close the chat being accepted (`deal-war-on-a-partners-pact-refused`).
#[test]
fn a_deal_may_not_declare_war_on_the_other_sides_pact_partner() {
    let mut g = shipped(3);
    ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Roma"},
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "name": "Veii"},
            {"op": "found_city", "player": 2, "x": 18, "y": 4, "name": "Capua"},
            {"op": "meet", "a": 0, "b": "all"},
            {"op": "meet", "a": 1, "b": 2},
            {"op": "set_player", "player": 1, "gold": 50},
        ]),
    );
    let war_on_capua = || json!([{"type": "declare_war", "target": 2}]);
    // Proposed while Veii and Capua have a pact.
    ops(&mut g, &json!([{"op": "set_relation", "a": 1, "b": 2, "defensive_pact": true}]));
    let refusal = refused(&mut g, ME, open(YOU, war_on_capua(), json!([])));
    assert_eq!(refusal, "Civilization 2 has a defensive pact with Civilization 3.");
    // Proposed before the pact, accepted after it.
    ops(&mut g, &json!([{"op": "set_relation", "a": 1, "b": 2, "defensive_pact": false}]));
    let opened =
        act(&mut g, ME, open(YOU, war_on_capua(), json!([{"type": "gold", "amount": 20}])));
    let nid = opened["negotiation_id"].as_i64().expect("an id");
    ops(&mut g, &json!([{"op": "set_relation", "a": 1, "b": 2, "defensive_pact": true}]));
    let refusal = refused(&mut g, YOU, answer(nid, "accept"));
    assert_eq!(refusal, "Civilization 2 has a defensive pact with Civilization 3.");
    assert!(!g.at_war(ME, THIRD) && !g.at_war(ME, YOU));
    assert!(g.state().diplo().deals.is_empty());
    // Without the pact, the war is declared and the deal stands between two at peace.
    ops(&mut g, &json!([{"op": "set_relation", "a": 1, "b": 2, "defensive_pact": false}]));
    let done = act(&mut g, YOU, answer(nid, "accept"));
    assert_eq!(done["status"], "accepted", "{done}");
    assert!(g.at_war(ME, THIRD) && !g.at_war(ME, YOU));
    let n = g.negotiation(NegotiationId::new(1).expect("an id")).expect("the chat");
    assert_eq!(n.status, NegStatus::Accepted);
    assert!(g.state().diplo().deals.iter().all(|d| d.active));
    clean(&mut g);
}

#[test]
fn a_research_agreement_pays_both_the_smaller_science_the_round_after_it_ends() {
    let mut g = shipped(2);
    ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Roma", "pop": 4},
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "name": "Veii"},
            {"op": "meet", "a": 0, "b": 1},
            {"op": "grant_tech", "player": [0, 1], "tech": "Education"},
            {"op": "set_player", "player": [0, 1], "gold": 1000},
        ]),
    );
    let refusal =
        refused(&mut g, ME, open(YOU, json!([{"type": "research_agreement"}]), json!([])));
    assert_eq!(refusal, "Research agreements require a declaration of friendship.");
    embassies(&mut g, ME, YOU);
    let cost = deals::ra_cost(&g, ME, YOU);
    assert_eq!(cost, 250, "the Medieval era's, at the standard speed");
    deal(
        &mut g,
        ME,
        YOU,
        json!([{"type": "research_agreement"}, {"type": "declaration_of_friendship"}]),
        json!([]),
    );
    exactly(gold(&g, ME), 750.0);
    exactly(gold(&g, YOU), 750.0);
    let until = g.relation(ME, YOU).map(|r| r.ra_until).expect("a relation");
    assert_eq!(until, 1 + g.speed().deal_duration);
    // Two negotiations a turn with the same side is the most: the rule is asked directly.
    let again =
        deals::make_proposal(&g, ME, YOU, Some(&json!([{"type": "research_agreement"}])), None)
            .expect("it reads")
            .expect("a proposal");
    let refusal = g.validate_items(ME, YOU, again.gives(ME), &again).expect_err("one at a time");
    assert_eq!(refusal.message, "You already have a research agreement.");
    end_round(&mut g);
    let banked = g.relation(ME, YOU).map(|r| r.ra_science).expect("a relation");
    assert!(banked[0] > 0 && banked[1] > 0, "each side's science, banked: {banked:?}");
    test_ops(&mut g, &json!([{"op": "set_turn", "turn": until}]));
    end_round(&mut g);
    let banked = g.relation(ME, YOU).map(|r| r.ra_science).expect("a relation");
    // The round after its last turn: both are paid the smaller sum.
    end_round(&mut g);
    let bonus = banked[0].min(banked[1]);
    for p in [ME, YOU] {
        assert_eq!(g.player(p).map(|x| x.tech.ra_bonus), Some(bonus), "{p}");
    }
    assert_eq!(g.relation(ME, YOU).map(|r| r.ra_science), Some([0, 0]));
    let told = events_of(&g, "research_agreement");
    assert_eq!(told.len(), 1);
    assert_eq!(
        told[0]["text"],
        "The research agreement between Civilization 1 and Civilization 2 has concluded."
    );
    clean(&mut g);
}

#[test]
fn deal_items_are_read_as_callers_write_them_and_categorised() {
    let g = shipped(2);
    let t = deals::make_proposal(
        &g,
        ME,
        YOU,
        Some(&json!([{"type": "gold", "amount": "25"}, {"type": "defensive_pact"}])),
        Some(&json!([{"type": "open_borders"}, {"type": "resource", "resource": "iron", "amount": 0}])),
    )
    .expect("it reads")
    .expect("a proposal");
    let iron = id(g.rules(), "Iron");
    assert_eq!(t.gives(ME), [DealItem::Gold { amount: 25 }, DealItem::DefensivePact]);
    assert_eq!(
        t.gives(YOU),
        [
            DealItem::OpenBorders { turns: g.speed().deal_duration },
            DealItem::Resource { resource: iron, amount: 1, turns: g.speed().deal_duration },
            DealItem::DefensivePact,
        ],
        "the pact on both sides, the default turns, at least one of a resource"
    );
    assert_eq!(
        proposal_categories(Some(&t)),
        [Category::Trades, Category::Agreements],
        "trades and agreements"
    );
    assert_eq!(
        deals::describe_items(&g, t.gives(YOU)),
        format!(
            "open borders for {0} turns, 1 Iron for {0} turns, a defensive pact",
            g.speed().deal_duration
        )
    );
    assert_eq!(deals::describe_items(&g, &[]), "nothing");
    let none =
        deals::make_proposal(&g, ME, YOU, Some(&json!([])), Some(&json!([]))).expect("it reads");
    assert!(none.is_none(), "empty lists are no proposal");
    let bad = deals::make_proposal(&g, ME, YOU, Some(&json!("gold")), None).expect_err("a string");
    assert_eq!(
        bad.message,
        "Deal items must be a list of objects like {\"type\": \"gold\", \"amount\": 50}."
    );
}

// ---- The kitchen sink ------------------------------------------------------------------------------

#[test]
fn friendship_and_a_defensive_pact_fire_the_kitchen_sinks_triggers() {
    let r = kitchen_sink();
    let mut g = arena(r, &["Kitchen Sink", "BenchmarkCiv"]);
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Sinkhold"},
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "name": "Veii"},
            {"op": "meet", "a": 0, "b": 1},
            {"op": "grant_tech", "player": [0, 1], "techs": ["Writing", "Chivalry"]},
        ]),
    );
    let sinkhold = city_of(&out[0]);
    embassies(&mut g, ME, YOU);
    let tiles = |g: &Game| {
        inspect::inspect(g, &json!({"what": "city", "city": sinkhold.get()})).expect("a city")["tiles"]
            .as_u64()
            .unwrap_or(0)
    };
    let science = |g: &Game| {
        g.player(ME).map_or(0.0, |x| x.tech.overflow + x.tech.progress.values().sum::<f64>())
    };
    let (before_tiles, before_science) = (tiles(&g), science(&g));
    // `Gain control over [3] tiles [in capital] <upon declaring friendship>` and `Gain [40]
    // [Science] <upon declaring a defensive pact>`, once each for one deal of both.
    deal(
        &mut g,
        ME,
        YOU,
        json!([{"type": "defensive_pact"}, {"type": "declaration_of_friendship"}]),
        json!([]),
    );
    assert!(g.relation(ME, YOU).is_some_and(|r| r.pact_until > 1 && r.friendship_until == 31));
    assert_eq!(tiles(&g), before_tiles + 3, "three tiles more for the capital");
    assert!((science(&g) - before_science - 40.0).abs() < 1e-9, "forty science");
    assert_eq!(events_of(&g, "pact").len(), 1);
    assert_eq!(events_of(&g, "friendship").len(), 1);
    clean(&mut g);
}

#[test]
fn the_kitchen_sinks_spies_work_faster_and_guard_its_capital_better() {
    let r = kitchen_sink();
    let mut g = arena(r, &["Kitchen Sink", "BenchmarkCiv"]);
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Sinkhold"},
            {"op": "found_city", "player": 0, "x": 5, "y": 11, "name": "Drainport"},
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "name": "Veii"},
        ]),
    );
    let (sinkhold, drainport) = (city_of(&out[0]), city_of(&out[1]));
    test_ops(&mut g, &json!([{"op": "add_spy", "player": 0}, {"op": "add_spy", "player": 1}]));
    let spy = |g: &Game, p: PlayerId| espionage::spies(g, p)[0].clone();
    // `[+15]% spy effectiveness [in all cities]`: at the hideout, the civilization's own.
    assert!((espionage::efficiency(&g, ME, &spy(&g, ME)) - 1.15).abs() < 1e-12);
    assert!((espionage::efficiency(&g, YOU, &spy(&g, YOU)) - 1.0).abs() < 1e-12);
    // `Spies in [Capital] cities act as though they have [+1] levels for
    // [Counter-intelligence]`: in its capital, guarding it.
    let to =
        |c: CityId| Action::MoveSpy(MoveSpy { spy: json!("Agent 1"), city_id: json!(c.get()) });
    act(&mut g, ME, to(sinkhold));
    assert_eq!(espionage::effective_rank(&g, ME, &spy(&g, ME)), 1, "moving, not guarding");
    test_ops(&mut g, &json!([{"op": "end_turn"}]));
    assert_eq!(spy(&g, ME).action, SpyAction::CounterIntelligence);
    assert_eq!(espionage::effective_rank(&g, ME, &spy(&g, ME)), 2, "guarding the capital");
    assert!((espionage::efficiency(&g, ME, &spy(&g, ME)) - 1.15).abs() < 1e-12, "in its own city");
    test_ops(&mut g, &json!([{"op": "end_turn"}]));
    act(&mut g, ME, to(drainport));
    test_ops(&mut g, &json!([{"op": "end_turn"}]));
    assert_eq!(spy(&g, ME).action, SpyAction::CounterIntelligence);
    assert_eq!(espionage::effective_rank(&g, ME, &spy(&g, ME)), 1, "not in the capital");
    clean(&mut g);
}

// ---- Spies ----------------------------------------------------------------------------------------------

/// A spy of the first player's sent into the second's city, which makes science and knows techs
/// the first could learn: the game, and the city.
fn spy_in_veii() -> (Game, CityId) {
    let mut g = shipped(2);
    let out = ops(
        &mut g,
        &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5, "name": "Roma"},
            {"op": "found_city", "player": 1, "x": 18, "y": 10, "name": "Veii", "pop": 8, "buildings": ["Library"]},
            {"op": "grant_tech", "player": 1, "techs": ["Pottery", "Mining", "Archery", "Writing"]},
            {"op": "meet", "a": 0, "b": 1},
            {"op": "reveal", "player": 0},
        ]),
    );
    let veii = city_of(&out[1]);
    test_ops(&mut g, &json!([{"op": "add_spy", "player": 0}]));
    act(&mut g, ME, Action::MoveSpy(MoveSpy { spy: json!("Agent 1"), city_id: json!(veii.get()) }));
    (g, veii)
}

/// Plays rounds until the spy has stolen a tech or died, at most `rounds`: the outcome.
fn until_theft(g: &mut Game, rounds: u32) -> Option<String> {
    for _ in 0..rounds {
        end_round(g);
        let stolen = events_of(g, "spy")
            .into_iter()
            .filter_map(|e| e["text"].as_str().map(str::to_owned))
            .find(|t| t.starts_with("Your spy Agent 1 stole") || t.contains("was killed"));
        if stolen.is_some() {
            return stolen;
        }
    }
    None
}

#[test]
fn a_spy_steals_technology_and_the_same_game_steals_the_same() {
    let (mut g, veii) = spy_in_veii();
    end_round(&mut g);
    assert_eq!(espionage::spies(&g, ME)[0].action, SpyAction::EstablishingNetwork);
    assert_eq!(espionage::spies(&g, ME)[0].turns, 3);
    for _ in 0..3 {
        end_round(&mut g);
    }
    assert_eq!(espionage::spies(&g, ME)[0].action, SpyAction::StealingTech);
    end_round(&mut g);
    assert!(espionage::spies(&g, ME)[0].progress > 0, "science taken toward the theft");
    let before = g.player(ME).map(|x| x.tech.known.len()).unwrap_or(0);
    let outcome = until_theft(&mut g, 60).expect("a theft within sixty turns");
    let after = g.player(ME).map(|x| x.tech.known.len()).unwrap_or(0);
    if outcome.contains("killed") {
        assert_eq!(espionage::spies(&g, ME)[0].action, SpyAction::Dead);
        assert_eq!(after, before);
    } else {
        assert_eq!(after, before + 1, "{outcome}");
        assert_eq!(espionage::spies(&g, ME)[0].rank, 2, "a theft promotes the spy");
        assert_eq!(espionage::spies(&g, ME)[0].city, Some(veii), "it stays to steal again");
    }
    // Keyed draws: the same game plays out alike.
    let (mut again, _) = spy_in_veii();
    for _ in 0..5 {
        end_round(&mut again);
    }
    assert_eq!(until_theft(&mut again, 60), Some(outcome));
    assert_eq!(again.digest().ok(), g.digest().ok());
    clean(&mut g);
}

/// The roll a theft by the first spy of `ME` in the city on `at` would make this turn: its
/// keyed stream draws the tech first, then the roll below 300 (`espionage::steal_tech`).
fn theft_roll(g: &Game, at: citar_engine::base::ids::TileIdx) -> i32 {
    use citar_engine::base::rng::{KeyPart, Purpose, Rng};
    let keys = [0, ME.key(), at.key(), g.turn().key()];
    let mut rng = Rng::keyed(g.state().seed(), Purpose::Spy, &keys);
    let options = espionage::techs_to_steal(g, ME, YOU).len();
    let _tech = rng.below(u64::try_from(options).expect("a few"));
    i32::try_from(rng.below(300)).expect("below 300")
}

/// A spy of the highest rank steals from Veii, each round it steals moved to a turn whose roll
/// falls within `band` once its skill is taken off; what each side hears of the theft.
fn theft_in_band(band: std::ops::Range<i32>) -> (Vec<String>, Vec<String>) {
    let (mut g, veii) = spy_in_veii();
    espionage::level_up(&mut g, ME, 0, 2);
    let at = g.city(veii).map(|c| c.tile()).expect("Veii");
    let heard = |g: &Game, p: PlayerId| -> Vec<String> {
        events_of(g, "spy")
            .into_iter()
            .filter(|e| e["audience"].as_array().is_some_and(|a| a.contains(&json!(p.0))))
            .filter_map(|e| e["text"].as_str().map(str::to_owned))
            .filter(|t| t.contains("stole") || t.contains("killed"))
            .collect()
    };
    for _ in 0..60 {
        let s = espionage::spies(&g, ME)[0].clone();
        if s.action == SpyAction::StealingTech {
            let skill = espionage::skill_percent(&g, ME, &s);
            let mut turn = g.turn();
            while !band.contains(&(theft_roll(&g, at) - skill)) {
                turn += 1;
                test_ops(&mut g, &json!([{"op": "set_turn", "turn": turn}]));
            }
        }
        let known = g.player(ME).map(|x| x.tech.known.len()).unwrap_or(0);
        end_round(&mut g);
        if g.player(ME).map(|x| x.tech.known.len()).unwrap_or(0) > known {
            clean(&mut g);
            return (heard(&g, ME), heard(&g, YOU));
        }
    }
    panic!("no theft within sixty rounds");
}

/// A theft whose roll falls below the spy's skill goes unnoticed: the thief gets the tech and
/// the victim hears nothing (`espionage.py:238`, `0 <= result < 100`); within a hundred of it,
/// the victim learns of the theft but not the thief.
#[test]
fn a_theft_below_the_spys_skill_goes_unnoticed() {
    let (mine, theirs) = theft_in_band(-300..0);
    assert_eq!(mine.len(), 1, "{mine:?}");
    assert!(mine[0].starts_with("Your spy Agent 1 stole the technology"), "{mine:?}");
    assert!(theirs.is_empty(), "the victim heard {theirs:?}");
    let (mine, theirs) = theft_in_band(0..100);
    assert_eq!(mine.len(), 1, "{mine:?}");
    assert_eq!(theirs.len(), 1, "{theirs:?}");
    assert!(theirs[0].starts_with("An unidentified spy stole the technology"), "{theirs:?}");
}

#[test]
fn a_constabulary_slows_the_spies_of_others_in_its_city() {
    let (mut g, veii) = spy_in_veii();
    end_round(&mut g);
    let spy = |g: &Game| espionage::spies(g, ME)[0].clone();
    assert_eq!(spy(&g).city, Some(veii));
    assert!((espionage::efficiency(&g, ME, &spy(&g)) - 1.0).abs() < 1e-12);
    // `[-25]% enemy spy effectiveness [in this city]`, shipped on the Constabulary.
    ops(
        &mut g,
        &json!([{"op": "set_city", "city": veii.get(), "add_buildings": ["Constabulary"]}]),
    );
    assert!((espionage::efficiency(&g, ME, &spy(&g)) - 0.75).abs() < 1e-12);
    // It slows only the others': a spy of Veii's own in Veii works at its own pace.
    test_ops(&mut g, &json!([{"op": "add_spy", "player": 1}]));
    let theirs = espionage::spies(&g, YOU)[0].clone();
    assert!((espionage::efficiency(&g, YOU, &theirs) - 1.0).abs() < 1e-12, "at its hideout");
    clean(&mut g);
}

#[test]
fn spies_flee_a_captured_city_and_a_dead_spy_is_replaced() {
    let (mut g, veii) = spy_in_veii();
    end_round(&mut g);
    assert_eq!(espionage::spies(&g, ME)[0].city, Some(veii));
    // Rome takes Veii: the spy in it flees home.
    ops(
        &mut g,
        &json!([
            {"op": "set_relation", "a": 0, "b": 1, "state": "war"},
            {"op": "add_unit", "player": 0, "unit": "Warrior", "x": 17, "y": 10},
            {"op": "set_city", "city": veii.get(), "health": 1},
        ]),
    );
    let w = g.player_units(ME).next().map(|u| u.id()).expect("a warrior");
    test_ops(&mut g, &json!([{"op": "attack_as", "unit": w.get(), "x": 18, "y": 10}]));
    assert_eq!(g.city(veii).map(|c| c.owner()), Some(ME));
    let s = espionage::spies(&g, ME)[0].clone();
    assert_eq!((s.city, s.action), (None, SpyAction::None));
    let fled = events_of(&g, "spy");
    assert!(
        fled.iter().any(|e| e["text"] == "After the city of Veii was captured, your spy Agent 1 has fled back to our hideout."),
        "{fled:?}"
    );
    clean(&mut g);
}

// ---- The host's commands --------------------------------------------------------------------------

#[test]
fn a_host_opens_for_a_seat_out_of_turn_and_closes_a_negotiation_with_a_note() {
    let mut g = shipped(2);
    ops(
        &mut g,
        &json!([{"op": "meet", "a": 0, "b": 1}, {"op": "set_player", "player": 1, "gold": 50}]),
    );
    let (opened, batch) = g
        .open_negotiation_as(
            YOU,
            ME,
            "Peace and gold?",
            Some(&json!([{"type": "gold", "amount": 20}])),
            None,
        )
        .expect("opened out of turn");
    assert_eq!(
        opened,
        json!({"negotiation_id": 1, "status": "open", "awaiting": "Civilization 1"})
    );
    assert_eq!(batch.len(), 1, "the negotiation event");
    assert_eq!(
        g.end_turn_refusal(YOU).as_deref().map(|s| s.starts_with("You are waiting for")),
        Some(true)
    );
    assert_eq!(
        g.end_turn_refusal(ME).as_deref().map(|s| s.starts_with("Answer Civilization 2")),
        Some(true)
    );
    let nid = NegotiationId::new(1).expect("an id");
    let view = g.negotiation_view(nid, ME).expect("a view");
    assert_eq!(view["current_proposal"]["summary"], "You give nothing; you receive 20 gold.");
    assert_eq!(view["your_move"], true);
    assert_eq!(g.max_chat_messages(), 30);
    let e = g.close_negotiation(nid, NegStatus::Accepted, "no", None).expect_err("not a close");
    assert_eq!(e.code, ErrCode::Negotiation);
    let (n, _) = g
        .close_negotiation(nid, NegStatus::Expired, "(no reply in time)", Some(ME))
        .expect("closed");
    assert_eq!((n.status, n.awaiting, n.history.len()), (NegStatus::Expired, None, 2));
    assert_eq!(n.history[1].by, Some(ME));
    assert!(g.end_turn_refusal(ME).is_none());
    assert!(g.negotiation_view(NegotiationId::new(9).expect("an id"), ME).is_err());
    act(&mut g, ME, Action::EndTurn(EndTurn {}));
    assert_eq!(g.current(), YOU);
    clean(&mut g);
}

// ---- Gate 4: the invariants under random negotiations ----------------------------------------------

/// One thing a player may do in a negotiation, or around one.
#[derive(Clone, Debug)]
enum Step {
    Open { by: u8, to: u8, items: u8 },
    Respond { by: u8, nid: u8, action: u8, items: u8 },
    /// The side a recent open negotiation waits on accepts it: `back` counts from the latest.
    Accept { back: u8 },
    Close { nid: u8, status: u8 },
    EndTurn,
    HostEndTurn,
    War { by: u8, on: u8 },
    Denounce { by: u8, on: u8 },
    Message { by: u8, to: u8 },
    NextTurn,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        4 => (0u8..3, 0u8..3, 0u8..POOL).prop_map(|(by, to, items)| Step::Open { by, to, items }),
        6 => (0u8..3, 1u8..6, 0u8..6, 0u8..POOL)
            .prop_map(|(by, nid, action, items)| Step::Respond { by, nid, action, items }),
        3 => (0u8..3).prop_map(|back| Step::Accept { back }),
        1 => (1u8..6, 0u8..4).prop_map(|(nid, status)| Step::Close { nid, status }),
        1 => Just(Step::EndTurn),
        1 => Just(Step::HostEndTurn),
        1 => (0u8..3, 0u8..3).prop_map(|(by, on)| Step::War { by, on }),
        1 => (0u8..3, 0u8..3).prop_map(|(by, on)| Step::Denounce { by, on }),
        1 => (0u8..3, 0u8..3).prop_map(|(by, to)| Step::Message { by, to }),
        1 => Just(Step::NextTurn),
    ]
}

/// How many deal items [`items`] draws from.
const POOL: u8 = 9;

/// A small pool of deal items, some of which no side can give: wars on the third civilization
/// and on the second, whose defensive pact the setup signs, so that a war on one of them would
/// bring the other in against whoever declared it.
fn items(n: u8) -> Value {
    match n {
        0 => json!([]),
        1 => json!([{"type": "gold", "amount": 10}]),
        2 => json!([{"type": "share_map"}]),
        3 => json!([{"type": "peace_treaty"}]),
        4 => json!([{"type": "declaration_of_friendship"}]),
        5 => json!([{"type": "gold", "amount": 5000}]),
        6 => json!([{"type": "open_borders", "turns": 3}, {"type": "gold", "amount": 5}]),
        7 => json!([{"type": "declare_war", "target": 2}]),
        _ => json!([{"type": "declare_war", "target": 1}]),
    }
}

const ACTIONS: [&str; 6] = ["accept", "counter", "reply", "reject", "withdraw", "haggle"];
const STATUSES: [&str; 4] = ["rejected", "expired", "cancelled", "accepted"];

fn play(g: &mut Game, s: &Step) {
    let p = |n: u8| PlayerId(n);
    // A refusal is an answer like any other: the invariants must hold either way.
    let _answer = match *s {
        Step::Open { by, to, items: i } => {
            let a = Action::OpenNegotiation(OpenNegotiation {
                to: i64::from(to),
                message: json!("Talk?"),
                give: Some(items(i)),
                receive: Some(items(POOL - 1 - i)),
            });
            if p(by) == g.current() {
                g.act(p(by), a).map(|_| ())
            } else {
                g.open_negotiation_as(p(by), p(to), "Talk?", Some(&items(i)), None).map(|_| ())
            }
        }
        Step::Respond { by, nid, action, items: i } => g
            .act(
                p(by),
                Action::RespondNegotiation(RespondNegotiation {
                    negotiation_id: i64::from(nid),
                    action: json!(ACTIONS[usize::from(action)]),
                    message: Some(json!("Well.")),
                    give: Some(items(i)),
                    receive: Some(items(i / 2)),
                }),
            )
            .map(|_| ()),
        Step::Accept { back } => {
            let open: Vec<_> =
                g.negotiations().iter().filter(|n| n.status == NegStatus::Open).collect();
            let chosen = open.len().checked_sub(1 + usize::from(back)).and_then(|i| open.get(i));
            match chosen.and_then(|n| n.awaiting.map(|w| (w, n.id))) {
                Some((who, nid)) => g
                    .act(who, answer(i64::from(nid.get()), "accept"))
                    .map(|_| ()),
                None => Ok(()),
            }
        }
        Step::Close { nid, status } => testops::apply(
            g,
            &json!([{"op": "close_negotiation", "negotiation": nid, "status": STATUSES[usize::from(status)], "note": "Out of time."}]),
        )
        .map(|_| ()),
        Step::EndTurn => {
            let now = g.current();
            g.act(now, Action::EndTurn(EndTurn {})).map(|_| ())
        }
        Step::HostEndTurn => {
            let now = g.current();
            g.end_turn(now).map(|_| ())
        }
        Step::War { by, on } => {
            if p(by) == g.current() {
                g.act(p(by), Action::DeclareWar(DeclareWar { player_id: i64::from(on), message: None }))
                    .map(|_| ())
            } else {
                Ok(())
            }
        }
        Step::Denounce { by, on } => {
            if p(by) == g.current() {
                g.act(p(by), Action::Denounce(Denounce { player_id: i64::from(on) })).map(|_| ())
            } else {
                Ok(())
            }
        }
        Step::Message { by, to } => g
            .act(p(by), Action::SendMessage(SendMessage { to: json!(to), text: json!("Hello.") }))
            .map(|_| ()),
        Step::NextTurn => {
            let t = g.turn() + 1;
            testops::apply(g, &json!([{"op": "set_turn", "turn": t}])).map(|_| ())
        }
    };
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, .. ProptestConfig::default() })]

    /// Gate 4: DIPLO-1 (war and contact symmetric, no treaty at war, the masks right) and NEG-1
    /// (an open negotiation awaits one of its parties, its entries numbered without gaps and
    /// within the cap) hold after every step of random negotiations, with a small cap so chats
    /// reach it, and every other check with them; and no deal stays in force between two at
    /// war.
    #[test]
    fn negotiations_keep_diplomacy_consistent(steps in prop::collection::vec(step(), 1..40)) {
        let (doc, _) = map_doc("arena").expect("the arena");
        let cfg = json!({
            "seed": 3, "players": [{"nation": "BenchmarkCiv"}, {"nation": "BenchmarkCiv"}, {"nation": "BenchmarkCiv"}],
            "city_states": 0, "barbarians": "off", "ruins": false,
            "diplomacy": {"max_chat_messages": 3}, "map": doc,
        });
        let mut g = new_game(Ruleset::shared(), cfg.as_object().expect("an object"))
            .expect("a game");
        g.set_debug_options(DebugOptions::ALL);
        ops(&mut g, &json!([
            {"op": "found_city", "player": 0, "x": 5, "y": 5},
            {"op": "found_city", "player": 1, "x": 18, "y": 10},
            {"op": "found_city", "player": 2, "x": 18, "y": 4},
            {"op": "meet", "a": 0, "b": "all"},
            {"op": "meet", "a": 1, "b": 2},
            {"op": "set_player", "player": [0, 1, 2], "gold": 100},
            {"op": "grant_tech", "player": [0, 1, 2], "tech": "Civil Service"},
            {"op": "set_relation", "a": 1, "b": 2, "friends": true, "defensive_pact": true},
        ]));
        for s in &steps {
            play(&mut g, s);
            let broken: Vec<_> = g.take_violations();
            prop_assert!(broken.is_empty(), "after {s:?}: {broken:?}");
            let found = g.check_invariants();
            prop_assert!(
                !found.iter().any(|v| matches!(v.code, Code::Diplo1 | Code::Neg1)),
                "after {s:?}: {found:?}"
            );
            prop_assert!(found.is_empty(), "after {s:?}: {found:?}");
            let at_war = g
                .state()
                .diplo()
                .deals
                .iter()
                .find(|d| d.active && g.at_war(d.parties[0], d.parties[1]));
            prop_assert!(at_war.is_none(), "after {s:?}: a deal in force at war: {at_war:?}");
        }
        prop_assert!(g.verify_caches().is_empty(), "{:?}", g.verify_caches());
    }
}
