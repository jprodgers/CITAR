//! Who decides each kind of diplomacy (DESIGN.md P2.3.8, package 2-01a gate 3): `Owners::set`
//! refuses with `set_diplomacy`'s messages, and `owns` agrees with Phase 0's
//! `owns_negotiation` cases (`test_bot_diplomacy.CategoryTests`, `test_engine_api`'s
//! `test_the_diplomacy_switch`), ported here as unit tests.

use citar_bot::{Owner, Owners};
use citar_engine::game::diplomacy::category::{CATEGORIES, Category};
use citar_engine::rules::Ruleset;
use citar_engine::state::diplo::Terms;
use serde_json::{Value, json};

fn owners(v: &Value) -> Owners {
    Owners::from_json(v).expect("owners")
}

/// Whether the bot with `o` answers a negotiation whose proposal on the table is `proposal`.
fn owns(o: &Owners, proposal: &Value) -> bool {
    let terms = match proposal {
        Value::Null => None,
        p => Some(Terms::from_json(p, Ruleset::shared()).expect("terms")),
    };
    o.owns_terms(terms.as_ref())
}

#[test]
fn set_refuses_with_set_diplomacys_messages_and_changes_nothing() {
    let mut o = owners(&json!({"war": "llm"}));
    let before = o;
    // `BasicBot(diplomacy={"trade": "llm"})`: an unknown category.
    let e = o.set(&json!({"trade": "llm"})).expect_err("unknown category");
    assert_eq!(
        e.to_string(),
        "Unknown diplomacy category trade (the categories are trades, agreements, peace, war, \
         denounce, un, city_states, espionage, captured_cities, chat)."
    );
    let e = o.set(&json!({"trade": "llm", "wars": "bot"})).expect_err("unknown categories");
    assert!(e.0.starts_with("Unknown diplomacy categories trade, wars ("), "{e}");
    // `BasicBot(diplomacy={"trades": "model"})`: an owner that is neither.
    let e = o.set(&json!({"trades": "model"})).expect_err("bad owner");
    assert_eq!(e.0, "Diplomacy categories are owned by 'bot' or 'llm' (trades is neither).");
    let e = o.set(&json!({"trades": "model", "peace": "llm", "chat": null})).expect_err("bad");
    assert_eq!(e.0, "Diplomacy categories are owned by 'bot' or 'llm' (trades, chat is neither).");
    // Unknown categories are named before bad owners, as Python checked them.
    let e = o.set(&json!({"chat": "model", "trade": "llm"})).expect_err("both");
    assert!(e.0.starts_with("Unknown diplomacy category trade"), "{e}");
    let e = o.set(&json!(["war"])).expect_err("not an object");
    assert!(e.0.contains("an object"), "{e}");
    assert_eq!(o, before, "a refusal changes nothing");
}

#[test]
fn set_hands_what_it_names_and_gives_the_rest_to_the_bot() {
    // `bot = BasicBot(diplomacy={"trades": "llm"})`: trades the model's, war the bot's.
    let mut o = owners(&json!({"trades": "llm"}));
    assert_eq!(o.of(Category::Trades), Owner::Llm);
    assert_eq!(o.of(Category::War), Owner::Bot);
    // A second set replaces the first: what it does not name goes back to the bot.
    o.set(&json!({"war": "llm", "chat": "bot"})).expect("valid");
    assert!(o.llm(Category::War) && !o.llm(Category::Trades) && !o.llm(Category::Chat));
    o.set(&Value::Null).expect("none named");
    assert_eq!(o, Owners::ALL_BOT);
    assert_eq!(o, Owners::default());
    let all: Value = CATEGORIES.iter().map(|c| (c.name().to_owned(), json!("llm"))).collect();
    o.set(&all).expect("every category");
    assert!(CATEGORIES.iter().all(|&c| o.llm(c)));
    assert_eq!(o.to_json(), all);
    // The typed form.
    let t = Owners::ALL_BOT.with(Category::Peace, Owner::Llm);
    assert_eq!(t, owners(&json!({"peace": "llm"})));
}

/// `CategoryTests.test_who_owns_a_negotiation`.
#[test]
fn who_owns_a_negotiation() {
    let bot = owners(&json!({"trades": "llm"}));
    let gold = json!({"0": [{"type": "gold", "amount": 5}], "1": [{"type": "embassy"}]});
    let embassy = json!({"0": [{"type": "embassy"}], "1": []});
    assert!(!owns(&bot, &gold), "one model-owned item makes the deal the model's");
    assert!(owns(&bot, &embassy));
    assert!(owns(&bot, &Value::Null));
    let chat = owners(&json!({"chat": "llm"}));
    assert!(!owns(&chat, &Value::Null));
    // A proposal of nothing on either side is talk, which the model owns with chat.
    assert!(!owns(&chat, &json!({"0": [], "1": []})));
}

/// `test_engine_api.test_the_diplomacy_switch` (its frozen bot is archived, P2.8).
#[test]
fn the_diplomacy_switch() {
    let mut bot = Owners::default();
    bot.set(&json!({"trades": "llm"})).expect("valid");
    assert!(!owns(&bot, &json!({"0": [{"type": "gold", "amount": 5}], "1": []})));
    assert!(owns(&bot, &Value::Null));
    assert!(bot.set(&json!({"trade": "llm"})).is_err());
}

/// The cases of `bot_model_owned_deferred.toml` and more: each item touches its category, and
/// the bot answers only what touches none the model owns.
#[test]
fn each_category_of_a_proposal_hands_it_to_the_model() {
    let cases = [
        (
            json!({"0": [{"type": "gold", "amount": 10}], "1": [{"type": "share_map"}]}),
            &["trades"][..],
        ),
        (
            json!({"0": [{"type": "gold", "amount": 10}], "1": [{"type": "embassy"}]}),
            &["trades", "agreements"],
        ),
        (json!({"0": [{"type": "peace_treaty"}], "1": [{"type": "peace_treaty"}]}), &["peace"]),
        (json!({"0": [{"type": "open_borders", "turns": 30}], "1": []}), &["agreements"]),
    ];
    for (proposal, touched) in cases {
        for c in CATEGORIES {
            let o = Owners::ALL_BOT.with(c, Owner::Llm);
            let model = touched.contains(&c.name());
            assert_eq!(owns(&o, &proposal), !model, "{} owning {proposal}", c.name());
        }
    }
    // Owning only chat, the model does not own a deal on the table.
    let chat = owners(&json!({"chat": "llm"}));
    assert!(owns(&chat, &json!({"0": [{"type": "gold", "amount": 1}], "1": []})));
    // With nothing on the table, owning everything but chat leaves the talk to the bot.
    let all_but_chat =
        owners(&json!({"trades": "llm", "agreements": "llm", "peace": "llm", "war": "llm"}));
    assert!(owns(&all_but_chat, &Value::Null));
}
