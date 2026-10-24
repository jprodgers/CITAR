//! The `deal_checks` group (DESIGN.md 9.2, package 1c-05): seeded proposals between major
//! civilizations who have met, each as `diplomacy._make_proposal` reads it, whether each side's
//! items pass `diplomacy.validate_items` (else the first refusal), how `describe_items` reads
//! them, and the research agreement's cost (`scripts/refcheck/queries.py::deal_checks`).
//!
//! Python sampled the pairs and the proposals and wrote them next to each answer, so the Rust side
//! reads the same `give` and `receive` through `game::diplomacy::deals`. Each side's valuation by
//! a fresh bot (`bot_value`: `BasicBot(seed=0).evaluate`, default parameters, aggression 0.4, no
//! memory of wars planned) is `citar_bot::evaluate` of a `basic-1` seat in the same conditions
//! (package 2-05, DESIGN.md P2.3.11 point 2). The states hold no bot memory, so the seat's is
//! fresh. It is compared only with `--with-bot`.

use std::sync::{Arc, OnceLock};

use citar_bot::{BotSpec, Overrides, Tuning, VersionId};
use citar_engine::base::ids::PlayerId;
use citar_engine::game::Game;
use citar_engine::game::diplomacy::deals::{
    describe_items, make_proposal, ra_cost, validate_items,
};
use serde_json::{Map, Value, json};

use super::{AnswerError, AnswerModule, Ctx};
use crate::Group;

/// The `deal_checks` answer module.
#[derive(Debug, Clone, Copy, Default)]
pub struct DealChecks;

impl AnswerModule for DealChecks {
    fn group(&self) -> Group {
        Group::DealChecks
    }

    fn answer(&self, cx: &Ctx<'_>, expected: &Value) -> Result<Value, AnswerError> {
        let g = cx.game.ok_or_else(|| AnswerError::new("no loaded game"))?;
        let deals = expected
            .get("deals")
            .and_then(Value::as_array)
            .map(|v| v.iter().map(|e| deal(g, e)).collect::<Result<Vec<_>, _>>())
            .transpose()?
            .unwrap_or_default();
        Ok(json!({ "deals": deals }))
    }
}

/// A player id the recording wrote.
fn player(e: &Value, key: &str) -> Result<PlayerId, AnswerError> {
    e.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u8::try_from(n).ok())
        .map(PlayerId)
        .ok_or_else(|| AnswerError::new(format!("a deal check without its {key}: {e}")))
}

/// One proposal checked, keys in Python's order.
fn deal(g: &Game, e: &Value) -> Result<Value, AnswerError> {
    let (a, b) = (player(e, "a")?, player(e, "b")?);
    let give = e.get("give").cloned().unwrap_or(Value::Null);
    let receive = e.get("receive").cloned().unwrap_or(Value::Null);
    let mut out = Map::new();
    out.insert("a".into(), json!(a.0));
    out.insert("b".into(), json!(b.0));
    out.insert("give".into(), give.clone());
    out.insert("receive".into(), receive.clone());
    out.insert("ra_cost".into(), json!(ra_cost(g, a, b)));
    let proposal = match make_proposal(g, a, b, Some(&give), Some(&receive)) {
        Ok(p) => p,
        Err(err) => {
            out.insert("valid".into(), json!(false));
            out.insert("error".into(), json!(err.message));
            return Ok(Value::Object(out));
        }
    };
    let rules = g.rules();
    out.insert(
        "proposal".into(),
        proposal.as_ref().and_then(|t| t.to_json(rules)).unwrap_or(Value::Null),
    );
    let checked = proposal.as_ref().map_or(Ok(()), |t| {
        validate_items(g, a, b, t.gives(a), t)?;
        validate_items(g, b, a, t.gives(b), t)
    });
    match checked {
        Ok(()) => {
            out.insert("valid".into(), json!(true));
        }
        Err(err) => {
            out.insert("valid".into(), json!(false));
            out.insert("error".into(), json!(err.message));
        }
    }
    if let Some(t) = &proposal {
        let mut describe = Map::new();
        describe.insert(a.0.to_string(), json!(describe_items(g, t.gives(a))));
        describe.insert(b.0.to_string(), json!(describe_items(g, t.gives(b))));
        out.insert("describe".into(), Value::Object(describe));
        let spec = fresh_bot();
        let mut value = Map::new();
        value.insert(
            a.0.to_string(),
            json!(citar_bot::evaluate(g, spec, a, b, t.gives(a), t.gives(b))),
        );
        value.insert(
            b.0.to_string(),
            json!(citar_bot::evaluate(g, spec, b, a, t.gives(b), t.gives(a))),
        );
        out.insert("bot_value".into(), Value::Object(value));
    }
    Ok(Value::Object(out))
}

/// The bot Python asked: `basic-1` at its defaults and the default aggression.
fn fresh_bot() -> &'static BotSpec {
    static SPEC: OnceLock<BotSpec> = OnceLock::new();
    SPEC.get_or_init(|| {
        let tuning = Arc::new(Tuning::new(VersionId::Basic1, Overrides::default()));
        BotSpec::new(VersionId::Basic1, tuning, None, None)
    })
}
