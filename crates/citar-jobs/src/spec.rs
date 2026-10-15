//! Job kinds and specs (design/phase4-helper.md P4.4.1): the closed vocabulary of work a helper or
//! the server's own executor runs, each spec typed and bounded, its canonical bytes and its hash.
//!
//! - [`LabSpec`] is `lab.game_spec(exp, i)`'s dict typed (`citar/lab.py`): one game of a member's
//!   or an administrator's experiment ([`Kind::LabGame`]) or of the site's ladder
//!   ([`Kind::RankingGame`]). The kinds share the spec and differ only in who may queue them and
//!   which machines opt in.
//! - [`ReferenceSpec`] is one game of citar-sim's statistical baseline ([`Kind::ReferenceGame`]),
//!   with none of what makes the baseline a run on one machine (its directory, workers, name,
//!   time budget, smoke and checks): a job never runs under a wall-clock budget, so a slow,
//!   capped or paused machine computes what a fast one does.
//!
//! [`check_spec`] reads a spec as the wire carries it: unknown keys are refused at every level
//! (`deny_unknown_fields`), then the bounds are checked (strings of 200 characters at most, `i`
//! below 1,000,000, a seed JSON and the protocol carry exactly, the seats within the ruleset's
//! players, `turns` at most the speed's last turn, bot versions compiled in, overrides through
//! `citar_bot::clean`), and the game's settings are read by the engine itself
//! (`config_from_value`), which knows every size, map type, speed, difficulty, barbarian level
//! and nation. A spec is data: it names no path, URL, code or ruleset text.
//!
//! **The hash:** `spec_hash = blake3("CITAR-SPEC-V1\0" ‖ kind ‖ "\0" ‖ canonical bytes)`, where
//! the canonical bytes are the typed spec written back by serde: its fields in their fixed
//! order, its maps sorted (`BTreeMap`), a key left out and a `null` alike. The server computes it
//! when it makes a job (through the binding), and the helper and the job host compute it again
//! and refuse a spec whose hash differs (`bad_spec`).

use std::collections::BTreeMap;
use std::fmt;

use citar_bot::VersionId;
use citar_engine::game::setup::config_from_value;
use citar_engine::rules::Ruleset;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The most characters a spec's text may hold.
pub const MAX_TEXT: usize = 200;

/// The largest game index `i` of an experiment.
pub const MAX_I: u32 = 999_999;

/// The largest seed: every integer up to it is exact in a JSON number read as a double, as
/// Python and the protocol's `int` carry it.
pub const MAX_SEED: u64 = (1 << 53) - 1;

/// The most city-states a lab game may ask for: the largest map's own number twice over.
pub const MAX_CITY_STATES: u8 = 64;

/// The most entries a seat's overrides or levels may hold.
pub const MAX_ENTRIES: usize = 200;

/// The most per-resource rules a spec's resources may hold.
pub const MAX_RESOURCE_RULES: usize = 200;

/// What the spec hash starts with.
pub const SPEC_DOMAIN: &[u8] = b"CITAR-SPEC-V1\0";

/// A kind of job: the closed vocabulary (P4.4.1). A new kind is a breaking change of protocol 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// One game of a lab experiment.
    LabGame,
    /// One game of a ranking experiment: the site's own ladder.
    RankingGame,
    /// One game of a reference run: the baseline the bots are compared against.
    ReferenceGame,
}

impl Kind {
    /// Every kind, in the protocol's order.
    pub const ALL: [Self; 3] = [Self::LabGame, Self::RankingGame, Self::ReferenceGame];

    /// Its name on the wire.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::LabGame => "lab_game",
            Self::RankingGame => "ranking_game",
            Self::ReferenceGame => "reference_game",
        }
    }

    /// The kind named `name`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Why a spec is refused: the reason `bad_spec` carries.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SpecError(pub String);

impl SpecError {
    fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

/// One seat of a lab game (`lab.game_spec`'s seats, as `lab.normalize` and `factorial_seats`
/// write them).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabSeat {
    /// What the result calls the seat.
    #[serde(default)]
    pub label: Option<String>,
    /// The bot version: `basic` (the latest), `basic-N` or `idle`; `None` for the latest.
    #[serde(default)]
    pub bot: Option<String>,
    /// The seat's difficulty; `None` for the game's.
    #[serde(default)]
    pub difficulty: Option<String>,
    /// Overrides of the version's parameters, cleaned against its schema when played.
    #[serde(default)]
    pub params: Option<BTreeMap<String, Value>>,
    /// The profile's fixed aggression, 0 to 1; `None` lets the seat decide.
    #[serde(default)]
    pub aggression: Option<f64>,
    /// The profile the seat plays, its revision and its name, recorded as they were queued.
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub profile_rev: Option<u32>,
    #[serde(default)]
    pub profile_name: Option<String>,
    /// A factorial experiment's levels for this seat, recorded with its result.
    #[serde(default)]
    pub levels: Option<BTreeMap<String, Value>>,
    /// The seat's nation; `None` for the game's.
    #[serde(default)]
    pub nation: Option<String>,
}

/// Resource rules for one kind of resource (`mapgen.MapOptions`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceKind {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub each: Option<BTreeMap<String, ResourceRule>>,
}

/// What one resource does on the map: `off`, or `cap` and `share` with a value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRule {
    pub mode: ResourceMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
}

/// A resource rule's mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceMode {
    Off,
    Cap,
    Share,
}

/// A lab game's resource options (`mapgen.MapOptions`): a density for all, and per kind.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategic: Option<ResourceKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub luxury: Option<ResourceKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bonus: Option<ResourceKind>,
}

/// One lab or ranking game: `lab.game_spec(exp, i)`'s keys, typed (P4.4.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabSpec {
    /// The experiment's name.
    pub exp: String,
    /// The game's index in it.
    pub i: u32,
    pub seed: u64,
    pub map_type: String,
    pub size: String,
    pub speed: String,
    /// The last turn; 0 for the speed's own.
    pub turns: u32,
    pub difficulty: String,
    /// The barbarian level; `None` for the engine's default (`normal`).
    #[serde(default)]
    pub barbarians: Option<String>,
    #[serde(default)]
    pub barbarian_difficulty: Option<String>,
    /// The nation of every seat that names none; `None` draws them.
    #[serde(default)]
    pub nation: Option<String>,
    /// `None` for the map size's own number.
    #[serde(default)]
    pub city_states: Option<u8>,
    pub seats: Vec<LabSeat>,
    /// `unciv` (the default) or `monotonic`.
    #[serde(default)]
    pub ai_base_values: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map_edges: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub river_density: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Resources>,
}

/// One reference game: one game of citar-sim's baseline, typed, with no budget, path, workers,
/// name, smoke or checks (P4.4.1). Every major is a `basic-1` bot, its aggression spread by seat
/// and seed, as the baseline seats them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceSpec {
    pub i: u32,
    pub seed: u64,
    pub size: String,
    pub map_type: String,
    pub barbarians: String,
    pub speed: String,
    /// The last turn; `None` for the speed's own.
    #[serde(default)]
    pub turn_limit: Option<u32>,
    /// Overrides for every seat's bot, as a profile gives them.
    #[serde(default)]
    pub params: Option<BTreeMap<String, Value>>,
}

/// A typed spec.
#[derive(Clone, Debug, PartialEq)]
pub enum Spec {
    /// A lab or ranking game.
    Lab(Box<LabSpec>),
    /// A reference game.
    Reference(ReferenceSpec),
}

/// A spec's hash: 32 bytes of blake3, 64 hex digits on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SpecHash(pub [u8; 32]);

impl SpecHash {
    /// The hash of `canonical`, the spec's canonical bytes, for a job of `kind`.
    #[must_use]
    pub fn of(kind: Kind, canonical: &[u8]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(SPEC_DOMAIN);
        h.update(kind.name().as_bytes());
        h.update(b"\0");
        h.update(canonical);
        Self(*h.finalize().as_bytes())
    }

    /// 64 lower-case hex digits.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }

    /// The hash from 64 hex digits.
    #[must_use]
    pub fn from_hex(text: &str) -> Option<Self> {
        unhex32(text).map(Self)
    }
}

impl fmt::Display for SpecHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// A spec read, bounded and hashed: what a job is played from.
#[derive(Clone, Debug, PartialEq)]
pub struct Checked {
    pub kind: Kind,
    pub spec: Spec,
    /// The typed spec written back: what the hash covers.
    pub canonical: Vec<u8>,
    pub hash: SpecHash,
}

impl Checked {
    /// The canonical spec as a JSON value.
    ///
    /// # Panics
    /// Never: the canonical bytes are JSON this crate wrote.
    #[must_use]
    pub fn canonical_value(&self) -> Value {
        serde_json::from_slice(&self.canonical).expect("the canonical bytes are JSON")
    }

    /// The bot versions the spec names, sorted, each once: what a claim lists (P4.4.4). A lab
    /// seat that names none plays the latest, and a reference game is all `basic-1`.
    #[must_use]
    pub fn bots(&self) -> Vec<String> {
        let mut out: Vec<String> = match &self.spec {
            Spec::Lab(s) => s
                .seats
                .iter()
                .filter_map(|seat| version_of(seat.bot.as_deref()))
                .map(|v| v.id().to_owned())
                .collect(),
            Spec::Reference(_) => vec![VersionId::Basic1.id().to_owned()],
        };
        out.sort();
        out.dedup();
        out
    }
}

/// The version a seat's `bot` names: `None` (no bot named) is the latest.
pub(crate) fn version_of(bot: Option<&str>) -> Option<VersionId> {
    VersionId::resolve(bot.unwrap_or("basic"))
}

/// Reads `spec` as a job of `kind` under `rules`: typed (unknown keys refused), bounded, its
/// settings read by the engine, and hashed. With `expected`, a spec whose hash is not it is
/// refused too.
///
/// # Errors
/// The reason the spec is refused (`bad_spec`).
pub fn check_spec(
    rules: &'static Ruleset,
    kind: Kind,
    spec: &Value,
    expected: Option<&str>,
) -> Result<Checked, SpecError> {
    if !spec.is_object() {
        return Err(SpecError::new("a spec is a JSON object"));
    }
    let typed = match kind {
        Kind::LabGame | Kind::RankingGame => Spec::Lab(Box::new(
            LabSpec::deserialize(spec).map_err(|e| SpecError::new(format!("{kind} spec: {e}")))?,
        )),
        Kind::ReferenceGame => Spec::Reference(
            ReferenceSpec::deserialize(spec)
                .map_err(|e| SpecError::new(format!("{kind} spec: {e}")))?,
        ),
    };
    match &typed {
        Spec::Lab(s) => s.check(rules)?,
        Spec::Reference(s) => s.check(rules)?,
    }
    let canonical = match &typed {
        Spec::Lab(s) => serde_json::to_vec(s),
        Spec::Reference(s) => serde_json::to_vec(s),
    }
    .map_err(|e| SpecError::new(format!("the spec cannot be written back: {e}")))?;
    let hash = SpecHash::of(kind, &canonical);
    if let Some(want) = expected
        && want != hash.to_hex()
    {
        return Err(SpecError::new(format!(
            "the spec hash {} is not the hash of the spec, {hash}",
            clip(want, 80)
        )));
    }
    Ok(Checked { kind, spec: typed, canonical, hash })
}

impl LabSpec {
    /// The game's settings, as `lab.play` gave them to `engine_api.run_game`: a key whose value
    /// is `None` is left out, as the facade's `_run_config` dropped it.
    #[must_use]
    pub fn config(&self) -> Value {
        let mut m = Map::new();
        let mut put = |k: &str, v: Value| {
            if !v.is_null() {
                m.insert(k.to_owned(), v);
            }
        };
        put("map_type", json!(self.map_type));
        put("map_size", json!(self.size));
        put("seed", json!(self.seed));
        put("barbarians", json!(self.barbarians));
        put("barbarian_difficulty", json!(self.barbarian_difficulty));
        put("difficulty", json!(self.difficulty));
        put("speed", json!(self.speed));
        put("city_states", json!(self.city_states));
        // `spec.get("ai_base_values") or "unciv"`: an empty name is the default too.
        let ai = self.ai_base_values.as_deref().filter(|a| !a.is_empty()).unwrap_or("unciv");
        put("ai_base_values", json!(ai));
        put("turn_limit", if self.turns == 0 { Value::Null } else { json!(self.turns) });
        put("map_edges", json!(self.map_edges));
        put("river_density", json!(self.river_density));
        put("resources", serde_json::to_value(&self.resources).unwrap_or(Value::Null));
        let seats: Vec<Value> = self
            .seats
            .iter()
            .map(|s| {
                // `seat.get("nation") or spec.get("nation")`
                let nation =
                    s.nation.as_deref().filter(|n| !n.is_empty()).or(self.nation.as_deref());
                json!({"controller": "bot", "nation": nation, "difficulty": s.difficulty})
            })
            .collect();
        put("players", Value::Array(seats));
        Value::Object(m)
    }

    fn check(&self, rules: &'static Ruleset) -> Result<(), SpecError> {
        text("exp", &self.exp)?;
        index(self.i)?;
        seed(self.seed)?;
        for (k, v) in [("map_type", &self.map_type), ("size", &self.size), ("speed", &self.speed)] {
            text(k, v)?;
        }
        text("difficulty", &self.difficulty)?;
        for (k, v) in [
            ("barbarians", &self.barbarians),
            ("barbarian_difficulty", &self.barbarian_difficulty),
            ("nation", &self.nation),
            ("ai_base_values", &self.ai_base_values),
            ("map_edges", &self.map_edges),
        ] {
            if let Some(v) = v {
                text(k, v)?;
            }
        }
        if let Some(n) = self.city_states
            && n > MAX_CITY_STATES
        {
            return Err(SpecError::new(format!("city_states {n} is more than {MAX_CITY_STATES}")));
        }
        let max = usize::from(rules.max_players());
        if self.seats.is_empty() || self.seats.len() > max {
            return Err(SpecError::new(format!(
                "a lab game has 1 to {max} seats, not {}",
                self.seats.len()
            )));
        }
        for (n, seat) in self.seats.iter().enumerate() {
            seat.check().map_err(|e| SpecError::new(format!("seat {n}: {e}")))?;
        }
        if let Some(d) = self.river_density {
            number_in("river_density", d, 0.0, 5.0)?;
        }
        if let Some(r) = &self.resources {
            r.check()?;
        }
        let new = config_from_value(rules, self.config())
            .map_err(|e| SpecError::new(format!("the game's settings: {e}")))?;
        let last = rules.speeds()[new.config().speed].max_turns();
        if i64::from(self.turns) > i64::from(last) {
            return Err(SpecError::new(format!(
                "turns {} is past the speed's last turn, {last}",
                self.turns
            )));
        }
        Ok(())
    }
}

impl LabSeat {
    fn check(&self) -> Result<(), SpecError> {
        for (k, v) in [
            ("label", &self.label),
            ("difficulty", &self.difficulty),
            ("profile", &self.profile),
            ("profile_name", &self.profile_name),
            ("nation", &self.nation),
            ("bot", &self.bot),
        ] {
            if let Some(v) = v {
                text(k, v)?;
            }
        }
        let Some(version) = version_of(self.bot.as_deref()) else {
            return Err(SpecError::new(format!(
                "unknown bot version {:?}: the versions compiled in are basic (the latest), {}",
                clip(self.bot.as_deref().unwrap_or_default(), 80),
                citar_bot::versions().iter().map(|v| v.id.id()).collect::<Vec<_>>().join(", ")
            )));
        };
        if let Some(a) = self.aggression {
            number_in("aggression", a, 0.0, 1.0)?;
        }
        if let Some(levels) = &self.levels {
            scalars("levels", levels)?;
        }
        let params = self.params.as_ref().map(|p| scalars("params", p)).transpose()?;
        let overrides =
            params.map_or(Value::Null, |p| Value::Object(p.clone().into_iter().collect()));
        citar_bot::clean(version.id(), &overrides)
            .map_err(|e| SpecError::new(format!("params: {}", e.message)))?;
        Ok(())
    }
}

impl Resources {
    fn check(&self) -> Result<(), SpecError> {
        if let Some(d) = self.density {
            number_in("resources.density", d, 0.0, 5.0)?;
        }
        let mut rules = 0;
        for (name, kind) in
            [("strategic", &self.strategic), ("luxury", &self.luxury), ("bonus", &self.bonus)]
        {
            let Some(kind) = kind else { continue };
            if let Some(d) = kind.density {
                number_in(&format!("resources.{name}.density"), d, 0.0, 5.0)?;
            }
            for (resource, rule) in kind.each.iter().flatten() {
                rules += 1;
                text("a resource's name", resource)?;
                if let Some(v) = rule.value {
                    number_in(&format!("resources.{name}.each.{resource}"), v, 0.0, 10_000.0)?;
                }
            }
        }
        if rules > MAX_RESOURCE_RULES {
            return Err(SpecError::new(format!(
                "{rules} resource rules are more than {MAX_RESOURCE_RULES}"
            )));
        }
        Ok(())
    }
}

impl ReferenceSpec {
    /// The baseline's game this spec stands for (citar-sim), with the baseline's checkpoints. Its
    /// budget is never used: a job runs without one.
    #[must_use]
    pub fn game(&self) -> citar_sim::baseline::GameSpec {
        citar_sim::baseline::GameSpec {
            i: self.i,
            seed: self.seed,
            size: self.size.clone(),
            map_type: self.map_type.clone(),
            barbarians: self.barbarians.clone(),
            speed: self.speed.clone(),
            turn_limit: self.turn_limit,
            checkpoints: citar_sim::baseline::CHECKPOINTS,
            budget: std::time::Duration::ZERO,
        }
    }

    /// The overrides every seat's bot plays with, as a JSON value (`null` for none).
    #[must_use]
    pub fn params_value(&self) -> Value {
        self.params.as_ref().map_or(Value::Null, |p| Value::Object(p.clone().into_iter().collect()))
    }

    fn check(&self, rules: &'static Ruleset) -> Result<(), SpecError> {
        index(self.i)?;
        seed(self.seed)?;
        for (k, v) in [
            ("size", &self.size),
            ("map_type", &self.map_type),
            ("barbarians", &self.barbarians),
            ("speed", &self.speed),
        ] {
            text(k, v)?;
        }
        if let Some(p) = &self.params {
            scalars("params", p)?;
        }
        citar_bot::clean(VersionId::Basic1.id(), &self.params_value())
            .map_err(|e| SpecError::new(format!("params: {}", e.message)))?;
        let new = config_from_value(rules, self.game().config(rules))
            .map_err(|e| SpecError::new(format!("the game's settings: {e}")))?;
        let last = rules.speeds()[new.config().speed].max_turns();
        if let Some(t) = self.turn_limit
            && (t == 0 || i64::from(t) > i64::from(last))
        {
            return Err(SpecError::new(format!(
                "turn_limit {t} is not a turn from 1 to the speed's last, {last}"
            )));
        }
        Ok(())
    }
}

fn text(key: &str, v: &str) -> Result<(), SpecError> {
    let n = v.chars().count();
    if n > MAX_TEXT {
        return Err(SpecError::new(format!("{key} is {n} characters, more than {MAX_TEXT}")));
    }
    Ok(())
}

fn index(i: u32) -> Result<(), SpecError> {
    if i > MAX_I {
        return Err(SpecError::new(format!("i {i} is not below 1,000,000")));
    }
    Ok(())
}

fn seed(s: u64) -> Result<(), SpecError> {
    if s > MAX_SEED {
        return Err(SpecError::new(format!("seed {s} is more than 2^53 - 1")));
    }
    Ok(())
}

fn number_in(key: &str, x: f64, lo: f64, hi: f64) -> Result<(), SpecError> {
    if !x.is_finite() || x < lo || x > hi {
        return Err(SpecError::new(format!("{key} {x} is not a number from {lo} to {hi}")));
    }
    Ok(())
}

/// A map of names to plain values (numbers, text, booleans, `null`), bounded.
fn scalars<'a>(
    key: &str,
    m: &'a BTreeMap<String, Value>,
) -> Result<&'a BTreeMap<String, Value>, SpecError> {
    if m.len() > MAX_ENTRIES {
        return Err(SpecError::new(format!(
            "{key} holds {} entries, more than {MAX_ENTRIES}",
            m.len()
        )));
    }
    for (name, v) in m {
        text(&format!("a name in {key}"), name)?;
        match v {
            Value::Array(_) | Value::Object(_) => {
                return Err(SpecError::new(format!(
                    "{key}.{} must be a number, text or a boolean",
                    clip(name, 80)
                )));
            }
            Value::String(s) => text(&format!("{key}.{}", clip(name, 80)), s)?,
            Value::Number(n) if n.as_f64().is_none_or(|x| !x.is_finite()) => {
                return Err(SpecError::new(format!(
                    "{key}.{} is no finite number",
                    clip(name, 80)
                )));
            }
            _ => {}
        }
    }
    Ok(m)
}

/// `text` cut to `n` characters, for a reason that quotes what it refuses.
pub(crate) fn clip(text: &str, n: usize) -> String {
    match text.char_indices().nth(n) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

/// Lower-case hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 15)]));
    }
    out
}

/// 32 bytes from 64 lower-case hex digits.
pub(crate) fn unhex32(text: &str) -> Option<[u8; 32]> {
    let b = text.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    let (pairs, _) = b.as_chunks::<2>();
    for (o, [hi, lo]) in out.iter_mut().zip(pairs) {
        *o = digit(*hi)? << 4 | digit(*lo)?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> &'static Ruleset {
        Ruleset::shared()
    }

    pub(crate) fn duel() -> Value {
        json!({
            "exp": "t", "i": 0, "seed": 11, "map_type": "continents", "size": "duel",
            "speed": "Quick", "turns": 20, "difficulty": "Prince", "barbarians": "normal",
            "barbarian_difficulty": null, "nation": null, "city_states": null,
            "seats": [{"label": "a", "bot": "basic-1"}, {"label": "b", "bot": "basic"}],
            "ai_base_values": null
        })
    }

    #[test]
    fn kinds_have_their_wire_names() {
        for k in Kind::ALL {
            assert_eq!(Kind::from_name(k.name()), Some(k));
            assert_eq!(serde_json::to_value(k).expect("a name"), json!(k.name()));
        }
        assert_eq!(Kind::from_name("burn"), None);
    }

    #[test]
    fn a_spec_hashes_its_canonical_bytes_with_its_kind() {
        let c = check_spec(rules(), Kind::LabGame, &duel(), None).expect("a good spec");
        assert_eq!(c.hash, SpecHash::of(Kind::LabGame, &c.canonical));
        let mut h = blake3::Hasher::new();
        h.update(b"CITAR-SPEC-V1\0lab_game\0");
        h.update(&c.canonical);
        assert_eq!(c.hash.0, *h.finalize().as_bytes());
        let ranked = check_spec(rules(), Kind::RankingGame, &duel(), None).expect("good");
        assert_eq!(ranked.canonical, c.canonical);
        assert_ne!(ranked.hash, c.hash, "the kind is hashed");
        // Key order, a null and a missing key give the same bytes.
        let mut v = duel();
        let obj = v.as_object_mut().expect("an object");
        obj.remove("nation");
        let reordered: Map<String, Value> = obj.clone().into_iter().rev().collect();
        let again = check_spec(rules(), Kind::LabGame, &Value::Object(reordered), None)
            .expect("still good");
        assert_eq!(again.hash, c.hash);
        assert_eq!(c.bots(), ["basic-1"]);
        assert_eq!(SpecHash::from_hex(&c.hash.to_hex()), Some(c.hash));
        // The expected hash is checked.
        assert!(check_spec(rules(), Kind::LabGame, &duel(), Some(&c.hash.to_hex())).is_ok());
        let wrong = "0".repeat(64);
        let e = check_spec(rules(), Kind::LabGame, &duel(), Some(&wrong)).expect_err("refused");
        assert!(e.0.contains("is not the hash of the spec"), "{e}");
    }

    fn refused(kind: Kind, spec: &Value) -> String {
        check_spec(rules(), kind, spec, None).expect_err("refused").0
    }

    #[test]
    fn unknown_keys_bounds_and_names_are_refused_with_their_reasons() {
        let with = |path: &[&str], v: Value| {
            let mut s = duel();
            let mut at = &mut s;
            for k in &path[..path.len() - 1] {
                at = if let Ok(i) = k.parse::<usize>() { &mut at[i] } else { &mut at[*k] };
            }
            at[path[path.len() - 1]] = v;
            s
        };
        let r = refused(Kind::LabGame, &with(&["path"], json!("/etc")));
        assert!(r.contains("unknown field `path`"), "{r}");
        let r = refused(Kind::LabGame, &with(&["seats", "0", "code"], json!("x")));
        assert!(r.contains("unknown field `code`"), "{r}");
        let r = refused(Kind::LabGame, &with(&["i"], json!(1_000_000)));
        assert!(r.contains("not below 1,000,000"), "{r}");
        let r = refused(Kind::LabGame, &with(&["seed"], json!(1u64 << 53)));
        assert!(r.contains("2^53"), "{r}");
        let r = refused(Kind::LabGame, &with(&["seed"], json!(-1)));
        assert!(r.contains("expected u64"), "{r}");
        let r = refused(Kind::LabGame, &with(&["turns"], json!(100_000)));
        assert!(r.contains("past the speed's last turn"), "{r}");
        let r = refused(Kind::LabGame, &with(&["size"], json!("planet")));
        assert!(r.contains("Unknown map_size 'planet'"), "{r}");
        let r = refused(Kind::LabGame, &with(&["speed"], json!("Ludicrous")));
        assert!(r.contains("speed"), "{r}");
        let r = refused(Kind::LabGame, &with(&["difficulty"], json!("Impossible")));
        assert!(r.contains("difficulty"), "{r}");
        let r = refused(Kind::LabGame, &with(&["seats", "0", "bot"], json!("basic-9")));
        assert!(r.contains("unknown bot version"), "{r}");
        let r = refused(Kind::LabGame, &with(&["seats", "0", "params"], json!({"nope": 1})));
        assert!(r.contains("params"), "{r}");
        let r = refused(Kind::LabGame, &with(&["seats", "1", "aggression"], json!(1.5)));
        assert!(r.contains("aggression"), "{r}");
        let r = refused(Kind::LabGame, &with(&["seats", "1", "label"], json!("x".repeat(201))));
        assert!(r.contains("more than 200"), "{r}");
        let r = refused(Kind::LabGame, &with(&["seats"], json!([])));
        assert!(r.contains("seats"), "{r}");
        let r = refused(Kind::LabGame, &with(&["river_density"], json!(9)));
        assert!(r.contains("river_density"), "{r}");
        let r = refused(Kind::LabGame, &with(&["seats", "0", "levels"], json!({"a": [1]})));
        assert!(r.contains("levels.a"), "{r}");
        let r = refused(Kind::LabGame, &json!([1, 2]));
        assert!(r.contains("JSON object"), "{r}");
    }

    #[test]
    fn a_reference_spec_has_no_budget_path_or_workers() {
        let good = json!({"i": 0, "seed": 5000, "size": "duel", "map_type": "continents",
                          "barbarians": "normal", "speed": "Quick", "turn_limit": 40});
        let c = check_spec(rules(), Kind::ReferenceGame, &good, None).expect("good");
        assert_eq!(c.bots(), ["basic-1"]);
        for (k, v) in [
            ("dir", json!("/tmp")),
            ("budget", json!(60)),
            ("max_minutes", json!(1)),
            ("workers", json!(4)),
            ("name", json!("x")),
            ("smoke", json!(true)),
            ("checks", json!(true)),
        ] {
            let mut s = good.clone();
            s[k] = v;
            let r = refused(Kind::ReferenceGame, &s);
            assert!(r.contains(&format!("unknown field `{k}`")), "{k}: {r}");
        }
        let mut s = good.clone();
        s["turn_limit"] = json!(0);
        assert!(refused(Kind::ReferenceGame, &s).contains("turn_limit"));
        // A lab spec is no reference spec, nor the other way round.
        assert!(refused(Kind::ReferenceGame, &duel()).contains("unknown field"));
        assert!(refused(Kind::LabGame, &good).contains("unknown field `turn_limit`"));
    }

    #[test]
    fn the_config_is_what_lab_play_gave_run_game() {
        let mut s = duel();
        s["seats"][0]["nation"] = json!("Rome");
        s["nation"] = json!("Greece");
        s["map_edges"] = json!("wrap_x");
        let c = check_spec(rules(), Kind::LabGame, &s, None).expect("good");
        let Spec::Lab(spec) = c.spec else { panic!("a lab spec") };
        let cfg = spec.config();
        assert_eq!(cfg["turn_limit"], json!(20));
        assert_eq!(cfg["ai_base_values"], json!("unciv"));
        assert_eq!(cfg["map_edges"], json!("wrap_x"));
        assert!(cfg.get("city_states").is_none(), "None is left out");
        assert_eq!(cfg["players"][0]["nation"], json!("Rome"));
        assert_eq!(cfg["players"][1]["nation"], json!("Greece"));
        assert_eq!(cfg["players"][1]["difficulty"], Value::Null);
    }

    #[test]
    fn hex_reads_back() {
        let h = SpecHash([7; 32]);
        assert_eq!(SpecHash::from_hex(&h.to_hex()), Some(h));
        assert_eq!(SpecHash::from_hex(&"G".repeat(64)), None);
        assert_eq!(SpecHash::from_hex(&"A".repeat(64)), None, "lower case only");
        assert_eq!(SpecHash::from_hex("ab"), None);
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(clip("ab", 3), "ab");
    }
}
