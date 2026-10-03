//! The CITAR bots (DESIGN.md P2.3): compiled versions that play a seat as the engine's
//! [`SeatDriver`](citar_engine::game::SeatDriver).
//!
//! A version is a code module plus its parameter schema, compiled in ([`versions()`]): `basic-1`,
//! the port of `citar/bots/basic.py`, and `idle`, the port of `citar/bots/idle.py`. A seat plays
//! a [`BotSpec`] (version, tuning, aggression, who owns each kind of diplomacy) through a
//! [`Bot`], which holds no game state between calls: what lasts lives in the seat's
//! `DriverMemory` ([`memory`]), and what lasts a turn is local to the turn. So any host (the
//! runner, the bindings, later the helper) can build a `Bot` from its spec at any time.
//!
//! The bot reads the game through `&Game` and acts only through `Game::act`: `&mut Game` appears
//! in `driver.rs` alone (`cargo xtask check`), whose `Turn` is the one holder of it.
//!
//! Package 2-00a wrote these signatures, so that the runner and the bindings could be written
//! beside the port; 2-01a filled in the parameters (their schema, generated struct, cleaning and
//! resolution), the versions, owners, memory, streams and the driver, whose `basic-1` turn calls
//! its phases in Python's order. The phases come with 2-01b (the economy), 2-03 (units) and 2-05
//! (diplomacy, [`advice`] and [`evaluate`], which return neutral values until then); DESIGN.md
//! P2.3.10 maps every line of `basic.py` to its home.

#![forbid(unsafe_code)]

mod basic1;
mod driver;
mod idle;
pub mod memory;
pub mod owners;
pub mod params;
pub mod stream;
pub mod versions;

use std::collections::BTreeMap;
use std::sync::Arc;

use citar_engine::base::ids::{NegotiationId, PlayerId};
use citar_engine::game::Game;
use citar_engine::rules::{ENGINE_CODE, Ruleset};
use citar_engine::state::diplo::DealItem;
use serde::Serialize;
use serde_json::Value;

pub use self::memory::Memory;
pub use self::owners::{Owner, Owners, OwnersError};
pub use self::params::{NameList, Overrides, ParamError, Params, Resolved, Tuning};
pub use self::stream::Stream;
pub use self::versions::{LATEST, Version, VersionId};

/// The bot's content code: 16 hex digits of blake3 over its sources, its parameter schemas, its
/// version and the locked versions of what it links, computed by `build.rs` (DESIGN.md P2.2.1).
pub const BOT_CODE: &str = env!("CITAR_BOT_CODE");

/// Why a bot call was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BotError {
    /// No version by that name.
    #[error("'{0}' is not a bot version (the versions are {v}).", v = versions::names())]
    UnknownVersion(String),
    /// Parameters that do not clean.
    #[error(transparent)]
    Params(#[from] ParamError),
    /// Diplomacy owners that do not parse.
    #[error(transparent)]
    Owners(#[from] OwnersError),
}

/// Everything about how a seat's bot plays (DESIGN.md P2.3.1).
#[derive(Clone, Debug)]
pub struct BotSpec {
    /// The version that plays.
    pub version: VersionId,
    /// Its parameters, shared by every spec with the same overrides, with their resolution
    /// against each ruleset met.
    pub tuning: Arc<Tuning>,
    /// What the seat plays with: the profile's fixed aggression, else the seat's, else 0.4,
    /// held to 0..1 as `BasicBot.__init__` held it.
    pub aggression: f64,
    /// The profile's own aggression, held to 0..1 as it plays, `None` when the seat decides: what
    /// the fingerprint hashes, so that one profile is one entry however the lab seats it
    /// (DESIGN.md P2.8.6).
    pub fixed_aggression: Option<f64>,
    /// Who decides each kind of diplomacy.
    pub owners: Owners,
}

impl BotSpec {
    /// The aggression a bot plays with when neither the profile nor the seat gives one
    /// (`BasicBot(aggression=0.4)`).
    pub const DEFAULT_AGGRESSION: f64 = 0.4;

    /// A spec: version `version` with `tuning`, the profile's `fixed` aggression if it has one,
    /// else the seat's, else [`DEFAULT_AGGRESSION`](Self::DEFAULT_AGGRESSION), held to 0..1; the
    /// bot owns every kind of diplomacy.
    ///
    /// A NaN is no value: a NaN fixed aggression leaves the seat to decide, and a NaN seat value
    /// leaves the default. (Python's `max(0.0, min(1.0, nan))` gave 1.0, the most aggressive bot,
    /// by the order of its arguments.) The fixed aggression is kept as it plays, held to 0..1,
    /// so two profiles that play alike share a fingerprint, as Python's profiles clamped it on
    /// saving.
    #[must_use]
    pub fn new(
        version: VersionId,
        tuning: Arc<Tuning>,
        fixed: Option<f64>,
        seat: Option<f64>,
    ) -> Self {
        let held = |a: f64| (!a.is_nan()).then(|| a.clamp(0.0, 1.0));
        let fixed = fixed.and_then(held);
        let aggression = fixed.or_else(|| seat.and_then(held)).unwrap_or(Self::DEFAULT_AGGRESSION);
        Self { version, tuning, aggression, fixed_aggression: fixed, owners: Owners::default() }
    }

    /// This spec with these owners: what `set_diplomacy` swaps in (DESIGN.md P2.6.5).
    #[must_use]
    pub fn with_owners(mut self, owners: Owners) -> Self {
        self.owners = owners;
        self
    }
}

/// Actions a bot took and had refused, by tool name (DESIGN.md P2.3.6): the bot proposes freely
/// and the rules refuse, so a refusal is normal, and the counts show a bot that loops on one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Refusals {
    by_tool: BTreeMap<&'static str, (u32, u32)>,
}

impl Refusals {
    /// Counts one action of `tool`, taken or refused.
    pub fn record(&mut self, tool: &'static str, ok: bool) {
        let (taken, refused) = self.by_tool.entry(tool).or_default();
        let n = if ok { taken } else { refused };
        *n = n.saturating_add(1);
    }

    /// (taken, refused) for `tool`.
    #[must_use]
    pub fn of(&self, tool: &str) -> (u32, u32) {
        self.by_tool.get(tool).copied().unwrap_or_default()
    }

    /// Every tool used, in name order, with (taken, refused).
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, u32, u32)> + '_ {
        self.by_tool.iter().map(|(&t, &(ok, no))| (t, ok, no))
    }

    /// The actions refused, every tool together.
    #[must_use]
    pub fn refused(&self) -> u32 {
        self.by_tool.values().fold(0u32, |n, &(_, no)| n.saturating_add(no))
    }

    /// `{tool: [taken, refused]}`, the shape a drive's result carries (DESIGN.md P2.6.5).
    #[must_use]
    pub fn to_json(&self) -> Value {
        self.by_tool
            .iter()
            .map(|(t, (ok, no))| ((*t).to_owned(), serde_json::json!([ok, no])))
            .collect()
    }
}

/// A seat's bot: its spec and the actions it has taken and had refused (DESIGN.md P2.3.1). It
/// implements [`SeatDriver`](citar_engine::game::SeatDriver) (in `driver.rs`) and holds no game
/// state between calls, so it is cheap to build afresh for each drive.
#[derive(Clone, Debug)]
pub struct Bot {
    spec: Arc<BotSpec>,
    refusals: Refusals,
}

impl Bot {
    /// A bot that plays `spec`.
    #[must_use]
    pub fn new(spec: Arc<BotSpec>) -> Self {
        Self { spec, refusals: Refusals::default() }
    }

    /// What it plays.
    #[must_use]
    pub const fn spec(&self) -> &Arc<BotSpec> {
        &self.spec
    }

    /// The actions it has taken and had refused since it was built.
    #[must_use]
    pub const fn refusals(&self) -> &Refusals {
        &self.refusals
    }
}

/// Every version compiled in, the latest first (DESIGN.md P2.8.5).
#[must_use]
pub fn versions() -> &'static [Version] {
    &versions::VERSIONS
}

/// The parameter schema of version `v`, the JSON verbatim (`{"engine", "groups"}`, the Bots
/// page's shape, DESIGN.md P2.3.2). `basic` names the latest version.
///
/// # Errors
/// An unknown version.
pub fn schema(v: &str) -> Result<&'static str, BotError> {
    let id = VersionId::resolve(v).ok_or_else(|| BotError::UnknownVersion(v.to_owned()))?;
    Ok(params::schema::of(id))
}

/// Overrides for version `v` cleaned against its schema: unknown keys refused, values coerced to
/// their parameter's type, values equal to the default dropped, keys sorted (DESIGN.md P2.3.2,
/// `profiles.clean_params`, with its two laxities fixed: `params/clean.rs`). `null` is no
/// overrides; the idle bot takes none and ignores any.
///
/// # Errors
/// An unknown version, overrides that are not an object, or a value its parameter refuses, with
/// Python's message naming the version as `v` does.
pub fn clean(v: &str, overrides: &Value) -> Result<Overrides, ParamError> {
    params::clean::clean(v, overrides)
}

/// What actually plays, hashed (DESIGN.md P2.8.6): 12 hex digits of blake3 over the build id,
/// the version, the canonical overrides and the profile's fixed aggression (or `seat`). Two lab
/// seats of one profile share it whatever aggression their positions give them; any change to
/// the build, the version, an override or the fixed aggression gives another.
#[must_use]
pub fn fingerprint(spec: &BotSpec, build_id: &str) -> String {
    let mut h = blake3::Hasher::new();
    let mut put = |bytes: &[u8]| {
        h.update(&(bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    put(b"CITAR-BOT");
    put(build_id.as_bytes());
    put(spec.version.id().as_bytes());
    put(spec.tuning.overrides().canonical().as_bytes());
    // Three decimals, as Python's fingerprint rounded it: the editor's slider has no more.
    let aggression = spec.fixed_aggression.map_or_else(|| "seat".to_owned(), |a| format!("{a:.3}"));
    put(aggression.as_bytes());
    hex12(&h.finalize())
}

/// The build id (DESIGN.md P2.2.1): 12 hex digits of blake3 over the engine's and the bot's
/// content codes and the ruleset's id. It changes exactly when what a game does can: a commit
/// outside the engine and the bot leaves it, and a modded ruleset has its own. A function, not a
/// constant, because a runtime ruleset (`CITAR_RULESET_DIR`, P2.8.4) has its own id.
#[must_use]
pub fn build_id(rules: &Ruleset) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"CITAR-BUILD");
    h.update(ENGINE_CODE.as_bytes());
    h.update(BOT_CODE.as_bytes());
    h.update(&rules.id().0);
    hex12(&h.finalize())
}

fn hex12(hash: &blake3::Hash) -> String {
    hash.as_bytes()[..6].iter().map(|b| format!("{b:02x}")).collect()
}

/// What a build is, for `build_info()` (DESIGN.md P2.2.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BuildInfo {
    /// The package version, CITAR's version.
    pub version: &'static str,
    /// [`build_id`] of the ruleset given.
    pub build_id: String,
    /// The label for people: `git describe` when the build set one, else `unknown`.
    pub label: &'static str,
    /// The ruleset's id, 64 hex digits.
    pub rules: String,
    /// The engine's content code.
    pub engine_code: &'static str,
    /// The bot's content code.
    pub bot_code: &'static str,
}

/// What this build is, with `rules`.
#[must_use]
pub fn build_info(rules: &Ruleset) -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION"),
        build_id: build_id(rules),
        label: citar_engine::rules::BUILD_LABEL,
        rules: rules.id().to_hex(),
        engine_code: ENGINE_CODE,
        bot_code: BOT_CODE,
    }
}

/// One civilization the advice weighs a war with (`advice()["war_readiness"]`, basic.py:2747).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WarReadiness {
    pub player: PlayerId,
    pub name: String,
    pub at_war: bool,
    pub preparing: bool,
    /// Our military power over theirs, two decimals.
    pub power_ratio: f64,
    pub army_gathered: bool,
}

/// Something the bot would ask for (`advice()["wants"]`, basic.py:2756-2770).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Want {
    /// A luxury another civilization has to spare and we lack.
    Resource { resource: String, from: PlayerId },
    /// Peace with a civilization we are losing a war against.
    PeaceTreaty { with: PlayerId },
}

/// What the bot makes of a seat's diplomatic situation, as plain data for a language model to
/// weigh, with Python's keys (`BasicBot.advice`, basic.py:2713-2771).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Advice {
    /// [`evaluate`] of the proposal on the table, in gold from the seat's side, one decimal;
    /// `None` without one.
    pub deal_value: Option<f64>,
    pub war_readiness: Vec<WarReadiness>,
    /// Luxuries the seat could trade away without losing their happiness, by name, sorted.
    pub spare_luxuries: Vec<String>,
    /// At most five.
    pub wants: Vec<Want>,
}

/// The advice of `spec`'s bot for seat `pid`, about negotiation `nid` if one is given (DESIGN.md
/// P2.3.8). It reads the game and the seat's memory and writes nothing.
///
/// The stub of package 2-00a advises nothing; 2-05 ports it.
#[must_use]
pub fn advice(g: &Game, pid: PlayerId, spec: &BotSpec, nid: Option<NegotiationId>) -> Advice {
    let _ = (g, pid, spec, nid);
    Advice::default()
}

/// What `spec`'s bot, playing `pid`, makes of a deal with `other` in which it gives `give` and
/// receives `receive`: its value in gold, from `pid`'s side (`BasicBot.evaluate`,
/// basic.py:2558-2643).
///
/// The stub of package 2-00a values every deal at 0, neither good nor bad; 2-05 ports it.
#[must_use]
pub fn evaluate(
    g: &Game,
    spec: &BotSpec,
    pid: PlayerId,
    other: PlayerId,
    give: &[DealItem],
    receive: &[DealItem],
) -> f64 {
    let _ = (g, spec, pid, other, give, receive);
    0.0
}

// The content code's helper, here so that its tests run with the bot's.
#[cfg(test)]
#[path = "../content_code.rs"]
mod build_code;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_code_helper_is_the_engines() {
        let norm = |s: &str| s.replace("\r\n", "\n");
        assert_eq!(
            norm(include_str!("../content_code.rs")),
            norm(include_str!("../../citar-engine/content_code.rs")),
            "crates/citar-bot/content_code.rs must stay a copy of the engine's"
        );
    }

    #[test]
    fn the_build_id_covers_both_codes_and_the_ruleset() {
        let r = Ruleset::shared();
        let id = build_id(r);
        assert_eq!(id.len(), 12);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_eq!(build_id(r), id, "a function of the build and the ruleset alone");
        assert_eq!(ENGINE_CODE.len(), 16);
        assert_eq!(BOT_CODE.len(), 16);
        let info = build_info(r);
        assert_eq!(info.build_id, id);
        assert_eq!(info.rules, r.id().to_hex());
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        // The save names the engine's code as its build.
        assert_eq!(citar_engine::rules::BUILD_ID, ENGINE_CODE);
    }

    fn spec(fixed: Option<f64>, seat: Option<f64>) -> BotSpec {
        let tuning = Arc::new(Tuning::new(VersionId::Basic1, Overrides::default()));
        BotSpec::new(VersionId::Basic1, tuning, fixed, seat)
    }

    #[test]
    fn aggression_is_the_profiles_else_the_seats_else_the_default_held_to_0_1() {
        assert!((spec(Some(0.7), Some(0.2)).aggression - 0.7).abs() < 1e-12);
        assert!((spec(None, Some(0.2)).aggression - 0.2).abs() < 1e-12);
        assert!((spec(None, None).aggression - 0.4).abs() < 1e-12);
        assert!((spec(Some(3.0), None).aggression - 1.0).abs() < 1e-12);
        assert!((spec(None, Some(-1.0)).aggression).abs() < 1e-12);
        assert!((spec(None, Some(f64::NAN)).aggression - 0.4).abs() < 1e-12);
        assert_eq!(spec(None, Some(0.2)).fixed_aggression, None);
        // A NaN fixed value is none: the seat decides, and the fingerprint says so.
        let nan = spec(Some(f64::NAN), Some(0.2));
        assert!((nan.aggression - 0.2).abs() < 1e-12);
        assert_eq!(nan.fixed_aggression, None);
        assert!((spec(Some(f64::NAN), None).aggression - 0.4).abs() < 1e-12);
        // An out-of-range fixed value is kept as it plays.
        let high = spec(Some(3.0), Some(0.2));
        assert_eq!(high.fixed_aggression, Some(1.0));
        assert_eq!(spec(Some(-0.5), None).fixed_aggression, Some(0.0));
        assert_eq!(spec(Some(f64::INFINITY), None).fixed_aggression, Some(1.0));
    }

    #[test]
    fn a_fingerprint_hashes_the_profile_and_not_the_seat() {
        let a = fingerprint(&spec(None, Some(0.25)), "b1");
        assert_eq!(a.len(), 12);
        assert_eq!(fingerprint(&spec(None, Some(0.75)), "b1"), a, "seats differ, one profile");
        assert_ne!(fingerprint(&spec(Some(0.25), Some(0.25)), "b1"), a, "a fixed aggression");
        assert_ne!(fingerprint(&spec(None, None), "b2"), a, "another build");
        let idle = BotSpec::new(VersionId::Idle, spec(None, None).tuning, None, None);
        assert_ne!(fingerprint(&idle, "b1"), a, "another version");
        // What plays is what is hashed: a NaN fixed value is the seat's, and 3 plays as 1.
        assert_eq!(fingerprint(&spec(Some(f64::NAN), Some(0.9)), "b1"), a, "NaN: the seat's");
        assert_eq!(
            fingerprint(&spec(Some(3.0), None), "b1"),
            fingerprint(&spec(Some(1.0), Some(0.1)), "b1"),
            "two profiles that play alike"
        );
    }

    #[test]
    fn refusals_count_by_tool() {
        let mut r = Refusals::default();
        r.record("move_unit", true);
        r.record("move_unit", false);
        r.record("found_city", false);
        assert_eq!(r.of("move_unit"), (1, 1));
        assert_eq!(r.of("attack"), (0, 0));
        assert_eq!(r.refused(), 2);
        assert_eq!(r.to_json(), serde_json::json!({"found_city": [0, 1], "move_unit": [1, 1]}));
    }

    #[test]
    fn versions_schemas_and_cleaning() {
        let ids: Vec<&str> = versions().iter().map(|v| v.id.id()).collect();
        assert_eq!(ids, ["basic-1", "idle"]);
        assert!(versions()[0].latest);
        assert_eq!(schema("idle"), Ok(r#"{"engine":"idle","groups":[]}"#));
        let basic: Value = serde_json::from_str(schema("basic").expect("basic-1's")).expect("JSON");
        assert_eq!(basic["engine"], "basic-1");
        assert_eq!(schema("basic"), schema("basic-1"), "basic names the latest");
        let e = schema("frozen_abc").expect_err("no such version");
        assert_eq!(
            e.to_string(),
            "'frozen_abc' is not a bot version (the versions are basic-1, idle)."
        );
        assert_eq!(clean("idle", &serde_json::json!({"anything": 1})), Ok(Overrides::default()));
        assert_eq!(clean("basic-1", &Value::Null), Ok(Overrides::default()));
        let o = clean("basic-1", &serde_json::json!({"tech_noise": 0})).expect("cleans");
        assert_eq!(o.to_json(), serde_json::json!({"tech_noise": 0.0}));
        assert!(clean("basic-1", &serde_json::json!([1])).is_err());
        assert!(clean("nope", &Value::Null).is_err());
    }
}
