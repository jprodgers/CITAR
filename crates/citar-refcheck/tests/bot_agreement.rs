//! The bot's choices against the Python bot's on the committed states (packages 2-01b, 2-03 and
//! 2-05, gates 2, 1 and 2; DESIGN.md P2.3.11): every kind of stages 1 to 3 at its floor of 95%
//! over the items where either engine says something, and every miss with a cause. The values are refcheck's
//! `bot_decisions` group, which `cargo refcheck run` enforces. Recordings edited by hand show
//! that a miss is found, that items are matched by what names them, and that a miss is put down
//! to an intended entry only when the entry explains a value that choice weighs.
//!
//! Set `CITAR_REFCHECK_CORPUS` to the corpus and `CITAR_BOT_DUMP` to its recording to hold the
//! corpus to the same floors.

use std::path::{Path, PathBuf};

use citar_engine::base::ids::PlayerId;
use citar_engine::game::Game;
use citar_engine::rules::Ruleset;
use citar_refcheck::agreement::{self, Cause, Choice, FLOOR, Miss, Tally};
use citar_refcheck::answer::bot_decisions::{MetOrders, met_orders, recorded};
use citar_refcheck::fixture::{self, Fixture, FixtureSet};
use citar_refcheck::intended::Intended;
use citar_refcheck::ratchet::DEFAULT_FIXTURES;
use citar_refcheck::run::INTENDED;
use serde_json::{Value, json};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn committed() -> Vec<FixtureSet> {
    let root = root();
    DEFAULT_FIXTURES.iter().map(|d| FixtureSet::new(&root.join(d), &root)).collect()
}

/// The repository's intended list.
fn intended() -> Intended {
    Intended::load(&root().join(INTENDED)).expect("the intended list")
}

fn holds(sets: &[FixtureSet], what: &str) {
    let found = agreement::run(&root(), sets, |_| true).expect("the fixtures");
    assert!(found.skipped.is_empty(), "{what}: {:?}", found.skipped);
    for c in Choice::ALL {
        let t = found.tallies.get(&c).copied().unwrap_or_default();
        assert!(t.rate() >= FLOOR, "{what}: {} at {:.3}", c.name(), t.rate());
        assert!(t.asked > 0, "{what}: {} asked of nobody", c.name());
    }
    let unattributed: Vec<String> = found
        .misses
        .iter()
        .filter(|m| m.cause == Cause::Unattributed)
        .map(|m| format!("{} {} {}", m.state, m.player, m.choice.name()))
        .collect();
    assert!(unattributed.is_empty(), "{what}: {unattributed:?}");
    assert!(found.holds(), "{what}");
}

#[test]
fn the_bot_chooses_as_pythons_did_on_the_committed_states() {
    let found = agreement::run(&root(), &committed(), |_| true).expect("the fixtures");
    assert_eq!((found.states, found.civilizations), (12, 38));
    // The choices that say something on the committed states, as 2-00b's as-built note counts
    // them: none of them may be lost by a port that answers nothing.
    let considered = |c: Choice| found.tallies.get(&c).map_or(0, |t| t.considered);
    assert_eq!(considered(Choice::NextResearch), 76);
    assert_eq!(considered(Choice::PreferredPolicy), 38);
    assert_eq!(considered(Choice::Garrison), 96);
    assert_eq!(considered(Choice::Danger), 14);
    assert_eq!(considered(Choice::Sites), 29);
    assert_eq!(considered(Choice::Spare), 21);
    assert_eq!(considered(Choice::Attacks), 19);
    assert_eq!(considered(Choice::WarTarget), 8);
    assert_eq!(considered(Choice::Reachable), 45);
    assert_eq!(considered(Choice::LuxTrade), 1);
    assert_eq!(considered(Choice::AdviceWarReadiness), 8);
    assert_eq!(considered(Choice::AdviceSpareLuxuries), 1);
    assert_eq!(considered(Choice::AdviceWants), 4);
    holds(&committed(), "the committed states");
}

#[test]
#[allow(clippy::disallowed_methods, reason = "a test's switch")]
fn the_bot_chooses_as_pythons_did_on_the_corpus() {
    let Some(dir) = std::env::var_os("CITAR_REFCHECK_CORPUS") else { return };
    if std::env::var_os("CITAR_BOT_DUMP").is_none() {
        return;
    }
    holds(&[FixtureSet::new(Path::new(&dir), &root())], "the corpus");
}

/// A committed state's recording, and its game loaded as refcheck loads it.
fn state(name: &str) -> (Game, Vec<Value>) {
    let (g, rows, _) = state_and_order(name);
    (g, rows)
}

/// [`state`], with the order Python's state lists each player's civilizations met in.
fn state_and_order(name: &str) -> (Game, Vec<Value>, MetOrders) {
    let sets = committed();
    let r = fixture::discover(&sets)
        .expect("the fixtures")
        .into_iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("no fixture {name}"));
    let f = Fixture::load(&r, &sets).expect("it loads");
    let rows = recorded(&root(), &f.meta.case, f.meta.turn).expect("recorded").to_vec();
    let (g, _) = Game::from_python(Ruleset::shared(), f.state.get().as_bytes()).expect("a game");
    (g, rows, met_orders(f.state.get()))
}

const DUEL: &str = "duel-continents-normal/t50";
/// A state of a war: both civilizations name a war target, and units of each would attack.
const WAR: &str = "scenario-duel-fractal/t10";
/// A state where a civilization would trade a luxury (player 3, Salt to player 4), and one wants
/// it (player 4).
const TRADE: &str = "standard-pangaea-normal-s1031/t120";
/// A state of the intended entry `marble-bonus-in-its-own-city`: player 2's potential values of
/// the four technologies it names differ, as the group's run shows; player 3's do not.
const MARBLE: &str = "scenario-small-continents-s3001/t61";

/// The misses of one choice.
fn of(misses: &[Miss], c: Choice) -> Vec<&Miss> {
    misses.iter().filter(|m| m.choice == c).collect()
}

fn tally(tallies: &[(Choice, Tally)], c: Choice) -> Tally {
    tallies.iter().find(|(x, _)| *x == c).map(|(_, t)| *t).unwrap_or_default()
}

/// The row of player `pid` in `rows`.
fn row(rows: &mut [Value], pid: u64) -> &mut Value {
    rows.iter_mut().find(|r| r["player"] == json!(pid)).expect("the player's row")
}

#[test]
fn a_choice_that_differs_is_a_miss_and_a_value_it_does_not_weigh_is_no_cause() {
    let (g, rows) = state(DUEL);
    let list = intended();
    let (_, misses) = agreement::compare_state(&g, DUEL, &rows, &list);
    assert!(misses.is_empty(), "{misses:?}");
    // Another preferred policy: a miss, and nothing explains it.
    let mut edited = rows.clone();
    edited[0]["empire"]["preferred_policy"] = json!("Honor");
    let (tallies, misses) = agreement::compare_state(&g, DUEL, &edited, &list);
    assert_eq!(misses.len(), 1, "{misses:?}");
    assert_eq!(misses[0].choice, Choice::PreferredPolicy);
    assert_eq!(misses[0].python, Some(json!("Honor")));
    assert_eq!(misses[0].cause, Cause::Unattributed);
    let t = tally(&tallies, Choice::PreferredPolicy);
    assert_eq!((t.considered, t.agree), (2, 1));
    // A value of the same civilization differing too explains nothing: the policy does not
    // weigh the supply, and no intended entry covers it.
    edited[0]["context"]["supply"] = json!(-7);
    let (_, misses) = agreement::compare_state(&g, DUEL, &edited, &list);
    assert_eq!(misses.len(), 1, "{misses:?}");
    assert_eq!(misses[0].cause, Cause::Unattributed);
}

#[test]
fn a_miss_is_put_down_to_an_intended_difference_in_a_value_its_choice_weighs() {
    let (g, rows) = state(DUEL);
    // A list that explains, on this state alone, player 0's classic value of Acoustics and its
    // supply.
    let list = Intended::parse(&format!(
        r#"
[[differences]]
id = "a-classic-value"
reason = "a test's"
where = [{{ group = "bot_decisions", path = "majors[*].tech_values.classic.Acoustics" }}]
cases = ["{DUEL}"]

[[differences]]
id = "a-supply"
reason = "a test's"
where = [{{ group = "bot_decisions", path = "majors[*].context.supply" }}]
cases = ["{DUEL}"]
"#
    ))
    .expect("a list");
    let mut edited = rows.clone();
    let r = row(&mut edited, 0);
    let acoustics = r["tech_values"]["classic"]["Acoustics"].as_f64().expect("a value");
    r["tech_values"]["classic"]["Acoustics"] = json!(acoustics + 5.0);
    r["context"]["supply"] = json!(-7);
    r["next_research"]["classic"]["tech"] = json!("Pottery");
    r["next_research"]["potential"]["tech"] = json!("Pottery");
    r["empire"]["preferred_policy"] = json!("Honor");
    let (_, misses) = agreement::compare_state(&g, DUEL, &edited, &list);
    let research = of(&misses, Choice::NextResearch);
    assert_eq!(research.len(), 2, "{misses:?}");
    // The classic path weighs the classic values: the entry explaining one is its cause.
    let classic = research.iter().find(|m| m.item == "classic").expect("the classic miss");
    assert_eq!(
        classic.cause,
        Cause::Intended {
            ids: vec!["a-classic-value".to_owned()],
            places: vec!["majors[player=0].tech_values.classic.Acoustics".to_owned()],
        }
    );
    // The potential path does not weigh it, and the policy weighs neither it nor the supply.
    let potential = research.iter().find(|m| m.item == "potential").expect("the potential miss");
    assert_eq!(potential.cause, Cause::Unattributed);
    assert_eq!(of(&misses, Choice::PreferredPolicy)[0].cause, Cause::Unattributed);
    // With the repository's list the classic value is a difference nothing explains, so it is
    // no cause either: the group's own run reports it.
    let (_, misses) = agreement::compare_state(&g, DUEL, &edited, &intended());
    assert!(
        of(&misses, Choice::NextResearch).iter().all(|m| m.cause == Cause::Unattributed),
        "{misses:?}"
    );
}

#[test]
fn the_marble_entry_explains_its_four_technologies_and_no_other() {
    let (g, rows) = state(MARBLE);
    let list = intended();
    let (_, misses) = agreement::compare_state(&g, MARBLE, &rows, &list);
    assert!(misses.is_empty(), "{misses:?}");
    // Player 2's potential path, had it differed: the four values the entry moves explain it.
    let mut edited = rows.clone();
    row(&mut edited, 2)["next_research"]["potential"]["tech"] = json!("Pottery");
    let (_, misses) = agreement::compare_state(&g, MARBLE, &edited, &list);
    assert_eq!(misses.len(), 1, "{misses:?}");
    let Cause::Intended { ids, places } = &misses[0].cause else { panic!("{misses:?}") };
    assert_eq!(ids, &["marble-bonus-in-its-own-city".to_owned()]);
    let mut places = places.clone();
    places.sort();
    assert_eq!(
        places,
        [
            "majors[player=2].tech_values.potential.Ecology",
            "majors[player=2].tech_values.potential.Industrialization",
            "majors[player=2].tech_values.potential[\"Metal Casting\"]",
            "majors[player=2].tech_values.potential[\"Nuclear Fission\"]",
        ]
    );
    // Player 3's values agree. Another potential value of its differing would be a pure bot
    // difference, which the entry does not cover; one of the four would be the entry's.
    let mut edited = rows.clone();
    let r = row(&mut edited, 3);
    r["next_research"]["potential"]["tech"] = json!("Pottery");
    let math = r["tech_values"]["potential"]["Mathematics"].as_f64().expect("a value");
    r["tech_values"]["potential"]["Mathematics"] = json!(math + 5.0);
    let (_, misses) = agreement::compare_state(&g, MARBLE, &edited, &list);
    assert_eq!(misses.len(), 1, "{misses:?}");
    assert_eq!(misses[0].cause, Cause::Unattributed);
    let r = row(&mut edited, 3);
    let ecology = r["tech_values"]["potential"]["Ecology"].as_f64().expect("a value");
    r["tech_values"]["potential"]["Ecology"] = json!(ecology + 5.0);
    let (_, misses) = agreement::compare_state(&g, MARBLE, &edited, &list);
    let Cause::Intended { ids, places } = &misses[0].cause else { panic!("{misses:?}") };
    assert_eq!(ids, &["marble-bonus-in-its-own-city".to_owned()]);
    assert_eq!(places, &["majors[player=3].tech_values.potential.Ecology".to_owned()]);
}

#[test]
fn cities_are_matched_by_id_and_one_only_one_engine_names_is_a_miss() {
    let (g, rows) = state(DUEL);
    let list = intended();
    let (before, _) = agreement::compare_state(&g, DUEL, &rows, &list);
    // In another order, the same cities agree.
    let mut edited = rows.clone();
    let cities = edited[0]["cities"].as_array_mut().expect("the cities");
    assert!(cities.len() >= 2);
    cities.reverse();
    let (tallies, misses) = agreement::compare_state(&g, DUEL, &edited, &list);
    assert!(misses.is_empty(), "{misses:?}");
    assert_eq!(tally(&tallies, Choice::Danger), tally(&before, Choice::Danger));
    // A city Python's answer lacks: considered, and a miss, for each per-city kind.
    let gone = edited[0]["cities"].as_array_mut().expect("the cities").remove(0);
    let (tallies, misses) = agreement::compare_state(&g, DUEL, &edited, &list);
    let key = format!("city {}", gone["city"]);
    for c in [Choice::Danger, Choice::Garrison] {
        let m = of(&misses, c);
        assert_eq!(m.len(), 1, "{}: {misses:?}", c.name());
        assert_eq!((m[0].item.as_str(), m[0].python.as_ref()), (key.as_str(), None));
        assert!(m[0].rust.is_some());
        assert_eq!(m[0].cause, Cause::Unattributed);
        let (t, b) = (tally(&tallies, c), tally(&before, c));
        assert_eq!(t.asked, b.asked, "{}", c.name());
        assert_eq!(t.agree + 1, t.considered, "{}", c.name());
    }
}

#[test]
fn an_answer_that_says_nothing_on_both_sides_is_not_counted() {
    let (g, rows) = state(DUEL);
    let (tallies, _) = agreement::compare_state(&g, DUEL, &rows, &intended());
    let t = tally(&tallies, Choice::FreeNow);
    // Nobody holds a free technology at turn 50: asked of both, considered for neither.
    assert_eq!((t.asked, t.considered), (4, 0));
    assert_eq!(t.rate().to_bits(), 1f64.to_bits());
}

#[test]
fn attacks_are_matched_by_unit_and_a_war_target_weighs_the_wars_and_the_army() {
    let (g, rows) = state(WAR);
    let (tallies, misses) = agreement::compare_state(&g, WAR, &rows, &intended());
    assert!(misses.is_empty(), "{misses:?}");
    assert_eq!(tally(&tallies, Choice::WarTarget).considered, 2);
    let attacking = tally(&tallies, Choice::Attacks);
    assert_eq!(attacking.considered, 4, "{attacking:?}");
    // The units in another order agree; a target Python's answer gives elsewhere is a miss of
    // that unit, which nothing recorded explains.
    let mut edited = rows.clone();
    let r = row(&mut edited, 1);
    let attacks = r["attacks"].as_array_mut().expect("the attacks");
    attacks.reverse();
    let (_, misses) = agreement::compare_state(&g, WAR, &edited, &intended());
    assert!(misses.is_empty(), "{misses:?}");
    let r = row(&mut edited, 1);
    let first = r["attacks"]
        .as_array_mut()
        .expect("the attacks")
        .iter_mut()
        .find(|a| !a["target"].is_null())
        .expect("an attack");
    first["target"] = json!([0, 0]);
    let key = format!("unit {}", first["unit"]);
    let (_, misses) = agreement::compare_state(&g, WAR, &edited, &intended());
    assert_eq!(misses.len(), 1, "{misses:?}");
    assert_eq!((misses[0].choice, misses[0].item.as_str()), (Choice::Attacks, key.as_str()));
    assert_eq!(misses[0].cause, Cause::Unattributed);
    // A war target that differs is put down to an entry explaining a difference in the army it
    // counts; one in a value it does not weigh (the supply) explains nothing.
    let list = Intended::parse(&format!(
        r#"
[[differences]]
id = "an-army"
reason = "a test's"
broad = true
where = [{{ group = "bot_decisions", path = "majors[*].context.military.**" }}]
cases = ["{WAR}"]

[[differences]]
id = "a-supply"
reason = "a test's"
where = [{{ group = "bot_decisions", path = "majors[*].context.supply" }}]
cases = ["{WAR}"]
"#
    ))
    .expect("a list");
    let mut edited = rows.clone();
    let r = row(&mut edited, 0);
    let advance = r["war_target"]["advance"].as_bool().expect("a plan");
    r["war_target"]["advance"] = json!(!advance);
    r["context"]["supply"] = json!(-7);
    let (_, misses) = agreement::compare_state(&g, WAR, &edited, &list);
    let war = of(&misses, Choice::WarTarget);
    assert_eq!(war.len(), 1, "{misses:?}");
    assert_eq!(war[0].cause, Cause::Unattributed);
    let r = row(&mut edited, 0);
    r["context"]["military"].as_array_mut().expect("the army").push(json!(99_999));
    let (_, misses) = agreement::compare_state(&g, WAR, &edited, &list);
    let war = of(&misses, Choice::WarTarget);
    let Cause::Intended { ids, .. } = &war[0].cause else { panic!("{misses:?}") };
    assert_eq!(ids, &["an-army".to_owned()]);
}

#[test]
fn stage_three_asks_in_pythons_order_of_meeting_and_a_trade_weighs_the_luxuries_owned() {
    let (g, rows, met) = state_and_order(TRADE);
    let list = intended();
    let (tallies, misses) = agreement::compare_state_in_order(&g, TRADE, &rows, &list, &met);
    assert!(misses.is_empty(), "{misses:?}");
    assert_eq!(tally(&tallies, Choice::LuxTrade).considered, 1);
    assert_eq!(tally(&tallies, Choice::AdviceWants).considered, 1);
    // Python's state lists the civilizations met in the order they were met.
    let three: Vec<u8> = met[&PlayerId(3)].iter().map(|p| p.0).collect();
    assert_eq!(three, [6, 7, 12, 4, 14, 17, 8, 1]);
    // A rival's city that differs is a miss of that rival, which nothing recorded explains.
    let mut edited = rows.clone();
    row(&mut edited, 3)["reachable"]["1"] = json!(999);
    let (_, misses) = agreement::compare_state_in_order(&g, TRADE, &edited, &list, &met);
    assert_eq!(misses.len(), 1, "{misses:?}");
    assert_eq!((misses[0].choice, misses[0].item.as_str()), (Choice::Reachable, "rival 1"));
    assert_eq!(misses[0].cause, Cause::Unattributed);
    // A trade offered elsewhere is put down to an entry explaining a difference in the
    // luxuries the civilization owns, and to none explaining its supply.
    let explains = Intended::parse(&format!(
        r#"
[[differences]]
id = "a-luxury"
reason = "a test's"
broad = true
where = [{{ group = "bot_decisions", path = "majors[*].context.lux_owned.**" }}]
cases = ["{TRADE}"]

[[differences]]
id = "a-supply"
reason = "a test's"
where = [{{ group = "bot_decisions", path = "majors[*].context.supply" }}]
cases = ["{TRADE}"]
"#
    ))
    .expect("a list");
    let mut edited = rows.clone();
    let r = row(&mut edited, 3);
    r["lux_trade"][0]["to"] = json!(1);
    r["context"]["supply"] = json!(-7);
    let (_, misses) = agreement::compare_state_in_order(&g, TRADE, &edited, &explains, &met);
    assert_eq!(of(&misses, Choice::LuxTrade)[0].cause, Cause::Unattributed, "{misses:?}");
    let r = row(&mut edited, 3);
    r["context"]["lux_owned"].as_array_mut().expect("the luxuries").push(json!("Silk"));
    let (_, misses) = agreement::compare_state_in_order(&g, TRADE, &edited, &explains, &met);
    let trade = of(&misses, Choice::LuxTrade);
    let Cause::Intended { ids, .. } = &trade[0].cause else { panic!("{misses:?}") };
    assert_eq!(ids, &["a-luxury".to_owned()]);
}

#[test]
fn the_advice_is_held_whole_its_wars_rival_by_rival_within_the_tolerance() {
    let (g, rows, met) = state_and_order(WAR);
    let list = intended();
    let (tallies, misses) = agreement::compare_state_in_order(&g, WAR, &rows, &list, &met);
    assert!(misses.is_empty(), "{misses:?}");
    // Each side's war on the other.
    assert_eq!(tally(&tallies, Choice::AdviceWarReadiness).considered, 2);
    let compared = |edited: &[Value], list: &Intended| {
        agreement::compare_state_in_order(&g, WAR, edited, list, &met).1
    };
    // A power ratio within refcheck's tolerance agrees.
    let mut edited = rows.clone();
    let war = |rows: &mut [Value]| -> Value {
        row(rows, 0)["advice"]["none"]["war_readiness"][0].clone()
    };
    let set = |rows: &mut [Value], field: &str, v: Value| {
        row(rows, 0)["advice"]["none"]["war_readiness"][0][field] = v;
    };
    let before = war(&mut edited);
    assert_eq!(before["player"], json!(1));
    let ratio = before["power_ratio"].as_f64().expect("a ratio");
    set(&mut edited, "power_ratio", json!(ratio + 1e-9));
    assert!(compared(&edited, &list).is_empty());
    // One rounded the other way is a miss of that rival, which nothing recorded explains; and
    // so is a gathered army the other engine does not see.
    set(&mut edited, "power_ratio", json!(ratio + 0.01));
    let misses = compared(&edited, &list);
    assert_eq!(misses.len(), 1, "{misses:?}");
    assert_eq!(
        (misses[0].choice, misses[0].item.as_str()),
        (Choice::AdviceWarReadiness, "rival 1")
    );
    assert_eq!(misses[0].cause, Cause::Unattributed);
    let mut gathered = rows.clone();
    set(&mut gathered, "army_gathered", json!(true));
    assert_eq!(of(&compared(&gathered, &list), Choice::AdviceWarReadiness).len(), 1);
    // An entry explaining a difference in the army it counts is the miss's cause; one in a
    // value the advice does not weigh (the supply) explains nothing.
    let explains = Intended::parse(&format!(
        r#"
[[differences]]
id = "an-army"
reason = "a test's"
broad = true
where = [{{ group = "bot_decisions", path = "majors[*].context.military.**" }}]
cases = ["{WAR}"]

[[differences]]
id = "a-supply"
reason = "a test's"
where = [{{ group = "bot_decisions", path = "majors[*].context.supply" }}]
cases = ["{WAR}"]
"#
    ))
    .expect("a list");
    row(&mut edited, 0)["context"]["supply"] = json!(-7);
    let misses = compared(&edited, &explains);
    assert_eq!(of(&misses, Choice::AdviceWarReadiness)[0].cause, Cause::Unattributed);
    row(&mut edited, 0)["context"]["military"]
        .as_array_mut()
        .expect("the army")
        .push(json!(99_999));
    let misses = compared(&edited, &explains);
    let Cause::Intended { ids, .. } = &of(&misses, Choice::AdviceWarReadiness)[0].cause else {
        panic!("{misses:?}")
    };
    assert_eq!(ids, &["an-army".to_owned()]);
    // A war only the Rust bot's advice names is a miss with no Python answer.
    let mut edited = rows.clone();
    row(&mut edited, 0)["advice"]["none"]["war_readiness"] = json!([]);
    let misses = compared(&edited, &list);
    assert_eq!(misses.len(), 1, "{misses:?}");
    assert_eq!((misses[0].python.as_ref(), misses[0].rust.as_ref()), (None, Some(&before)));
}

#[test]
fn the_advice_s_luxuries_to_spare_weigh_the_luxuries_owned() {
    let (g, rows, met) = state_and_order(TRADE);
    let list = intended();
    let (tallies, misses) = agreement::compare_state_in_order(&g, TRADE, &rows, &list, &met);
    assert!(misses.is_empty(), "{misses:?}");
    assert_eq!(tally(&tallies, Choice::AdviceSpareLuxuries).considered, 1);
    assert_eq!(row(&mut rows.clone(), 3)["advice"]["none"]["spare_luxuries"], json!(["Salt"]));
    let explains = Intended::parse(&format!(
        r#"
[[differences]]
id = "a-luxury"
reason = "a test's"
broad = true
where = [{{ group = "bot_decisions", path = "majors[*].context.lux_owned.**" }}]
cases = ["{TRADE}"]
"#
    ))
    .expect("a list");
    // Another luxury to spare is a miss, which nothing explains until the luxuries the
    // civilization owns differ under an entry.
    let mut edited = rows.clone();
    row(&mut edited, 3)["advice"]["none"]["spare_luxuries"] = json!(["Salt", "Silk"]);
    let (_, misses) = agreement::compare_state_in_order(&g, TRADE, &edited, &explains, &met);
    assert_eq!(misses.len(), 1, "{misses:?}");
    assert_eq!(misses[0].choice, Choice::AdviceSpareLuxuries);
    assert_eq!(misses[0].cause, Cause::Unattributed);
    row(&mut edited, 3)["context"]["lux_owned"]
        .as_array_mut()
        .expect("the luxuries")
        .push(json!("Silk"));
    let (_, misses) = agreement::compare_state_in_order(&g, TRADE, &edited, &explains, &met);
    let Cause::Intended { ids, .. } = &misses[0].cause else { panic!("{misses:?}") };
    assert_eq!(ids, &["a-luxury".to_owned()]);
}
