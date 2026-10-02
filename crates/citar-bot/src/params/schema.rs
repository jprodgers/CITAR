//! Each version's parameter schema, verbatim: what `/api/bots/schema` serves and the bot's
//! parameters are generated from (DESIGN.md P2.3.2).
//!
//! `idle` has no parameters. `basic-1`'s schema is `params/basic-1.json`, which package 2-00b
//! exports from `PARAM_GROUPS` and 2-01a includes here; until then it has none to give.

use crate::versions::VersionId;

/// `idle`'s schema: no groups, as `profiles.schema("idle")` gave.
pub(crate) const IDLE: &str = r#"{"engine":"idle","groups":[]}"#;

/// Version `v`'s schema, if it is built in yet.
pub(crate) const fn of(v: VersionId) -> Option<&'static str> {
    match v {
        VersionId::Idle => Some(IDLE),
        VersionId::Basic1 => None,
    }
}
