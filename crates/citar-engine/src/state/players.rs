//! Players: seats, stocks, research, policies, great people, religion, and the data only major
//! civilizations or only city-states have (DESIGN.md 4.5).
//!
//! Replaces `state.py:20-63` (the Phase 0 seat model: controller, handicap, automatic decisions
//! and explicit overrides) and `state.py:193-312` (`Player`), with the 28 keys of the untyped
//! `Player.flags` dict turned into typed fields:
//! - `start` becomes [`Player::start_tile`];
//! - `culture_last8` and `science_last8` become [`Economy::culture_hist`] and
//!   [`Economy::science_hist`]; `total_culture`, `total_faith` join [`Economy`];
//! - `ra_science` becomes [`TechState::ra_bonus`];
//! - `gp_threshold`, `maya_limited` and `long_count_pool` join [`GreatPeople`];
//! - `free_beliefs` and `choose_pantheon_belief` join [`ReligionState`];
//! - `revolt_in`, `last_ruins` and `skip_explore` join [`CivExtras`];
//! - the explorer keys `explore_targets` and `explore_hist` move to the unit
//!   (`units::ExploreMemory`);
//! - `pairs`, `quest_state`, `war_quests`, `election_in`, `barb_help_cd` and `recently_bullied`
//!   join [`CityStateData`];
//! - `eras_spy_earned`, `cs_attacks` and `cs_gp_gift` join [`MajorData`];
//! - `last_gold_rate` is kept, and now written ([`Economy::last_gold_rate`]); `gained_<unit>` is
//!   kept, and now written ([`CivExtras::units_gained`]);
//! - `last_stats` (never read) is dropped.
//!
//! Also dropped as dead: `cs_unit_timer`, `tribute_turn`, `ruins_rewards`, `spy_eras` and
//! `faith_buys`. `met` moved to the diplomacy matrix, `memory` into [`MajorData`], and
//! `free_buildings` onto the cities.

use core::fmt;
use std::collections::BTreeMap;

use serde_json::Value;

use crate::base::fmt::PyFloat;
use crate::base::ids::{
    BaseUnitId, BuildingId, CityId, CityStateTypeId, DifficultyId, NationId, PlayerId, QuestKindId,
    ReligionId, ResourceId, RuinId, TechId, TerrainId, TextId, TileIdx, Turn, UniqueId,
};
use crate::base::sets::{
    BaseUnitSet, BitSet, EraSet, PlayerSet, PlayerVec, PolicySet, TechSet, TerrainSet,
};
use crate::base::stats::Stat;
use crate::rules::defs::{BeliefKind, CityStatePersonality, QuestScope, QuestTargetKind};

use super::cities::Constructible;
use super::memory::TileMemoryLayer;

/// What kind of player: a major civilization, a city-state or the barbarians. The same
/// vocabulary as a nation's `kind`.
pub use crate::rules::defs::NationKind as PlayerKind;

/// How far a civilization has come with religion, and what a spy is doing: the vocabularies the
/// uniques name too, so they live in `rules::defs` and the rules compare them directly.
pub use crate::rules::defs::{ReligionProgress, SpyAction};

// ---- Seats ------------------------------------------------------------------------------------

/// Who drives a civilization's turns (`state.py:20-24`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Controller {
    /// A person at the web client.
    Human,
    /// A language model the server drives.
    Llm,
    /// A model connected over MCP.
    Mcp,
    /// The engine's bot.
    Bot,
    /// The bot, with a model making some diplomatic decisions.
    Hybrid,
    /// A city-state's own AI.
    Minor,
    /// The barbarians' AI.
    Barbarian,
}

impl Controller {
    /// Every controller.
    pub const ALL: [Self; 7] =
        [Self::Human, Self::Llm, Self::Mcp, Self::Bot, Self::Hybrid, Self::Minor, Self::Barbarian];

    /// The name the lobby and the saves use: `hybrid`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Llm => "llm",
            Self::Mcp => "mcp",
            Self::Bot => "bot",
            Self::Hybrid => "hybrid",
            Self::Minor => "minor",
            Self::Barbarian => "barbarian",
        }
    }

    /// The controller called `name`, exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }

    /// Whether people or models play it, deciding for themselves as a human would
    /// (`HUMANLIKE_CONTROLLERS`, `state.py:22`).
    #[must_use]
    pub const fn is_humanlike(self) -> bool {
        matches!(self, Self::Human | Self::Llm | Self::Mcp)
    }

    /// Whether the engine's bots run it and manage its cities (`BOT_MANAGED`, `state.py:23`).
    #[must_use]
    pub const fn is_bot_managed(self) -> bool {
        !self.is_humanlike()
    }

    /// The handicap it gets unless its seat says otherwise: a human's for people and models,
    /// since giving a model the AI's bonuses would make a benchmark against the bot meaningless,
    /// and the AI's for everything else, hybrid seats included (`state.py:28-34`).
    #[must_use]
    pub const fn default_handicap(self) -> Handicap {
        if self.is_humanlike() { Handicap::Human } else { Handicap::Ai }
    }

    /// The decisions the engine takes for it unless its seat says otherwise: none for people and
    /// models, all of them for the bots (`state.py:37-41`).
    #[must_use]
    pub const fn default_auto(self) -> AutoDecisions {
        let on = !self.is_humanlike();
        AutoDecisions { un_vote: on, conquest: on, free_picks: on }
    }
}

/// Which difficulty numbers a civilization gets, and which of UnCiv's "Human player" and
/// "AI player" filters match it (`HANDICAPS`, `state.py:24`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Handicap {
    Human,
    Ai,
}

impl Handicap {
    /// The name: `human` or `ai`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Ai => "ai",
        }
    }

    /// The handicap called `name`, exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "human" => Some(Self::Human),
            "ai" => Some(Self::Ai),
            _ => None,
        }
    }
}

/// A decision the engine can take for a civilization (`AUTO_DECISIONS`, `state.py:25`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AutoDecision {
    /// Voting in the United Nations.
    UnVote,
    /// Annexing, puppeting or razing a conquered city.
    Conquest,
    /// Free techs and free great people.
    FreePicks,
}

impl AutoDecision {
    /// Every decision, in Python's order.
    pub const ALL: [Self; 3] = [Self::UnVote, Self::Conquest, Self::FreePicks];

    /// The key: `un_vote`, `conquest`, `free_picks`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::UnVote => "un_vote",
            Self::Conquest => "conquest",
            Self::FreePicks => "free_picks",
        }
    }

    /// The decision called `name`, exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.name() == name)
    }
}

/// Which decisions the engine takes for a civilization.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AutoDecisions {
    pub un_vote: bool,
    pub conquest: bool,
    pub free_picks: bool,
}

impl AutoDecisions {
    /// Whether the engine takes decision `d`.
    #[must_use]
    pub const fn get(self, d: AutoDecision) -> bool {
        match d {
            AutoDecision::UnVote => self.un_vote,
            AutoDecision::Conquest => self.conquest,
            AutoDecision::FreePicks => self.free_picks,
        }
    }

    /// Sets whether the engine takes decision `d`.
    pub fn set(&mut self, d: AutoDecision, on: bool) {
        match d {
            AutoDecision::UnVote => self.un_vote = on,
            AutoDecision::Conquest => self.conquest = on,
            AutoDecision::FreePicks => self.free_picks = on,
        }
    }

    /// With `over` laid on top.
    #[must_use]
    pub fn overlaid(mut self, over: AutoOverrides) -> Self {
        for d in AutoDecision::ALL {
            if let Some(on) = over.get(d) {
                self.set(d, on);
            }
        }
        self
    }
}

/// Some of the automatic decisions set explicitly, the rest left to the controller.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AutoOverrides {
    pub un_vote: Option<bool>,
    pub conquest: Option<bool>,
    pub free_picks: Option<bool>,
}

impl AutoOverrides {
    /// The explicit setting of `d`, if any.
    #[must_use]
    pub const fn get(self, d: AutoDecision) -> Option<bool> {
        match d {
            AutoDecision::UnVote => self.un_vote,
            AutoDecision::Conquest => self.conquest,
            AutoDecision::FreePicks => self.free_picks,
        }
    }

    /// Sets `d` explicitly.
    pub fn set(&mut self, d: AutoDecision, on: bool) {
        match d {
            AutoDecision::UnVote => self.un_vote = Some(on),
            AutoDecision::Conquest => self.conquest = Some(on),
            AutoDecision::FreePicks => self.free_picks = Some(on),
        }
    }

    /// Whether nothing is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.un_vote.is_none() && self.conquest.is_none() && self.free_picks.is_none()
    }

    /// With `newer`'s settings laid over these.
    #[must_use]
    pub fn merged(mut self, newer: Self) -> Self {
        for d in AutoDecision::ALL {
            if let Some(on) = newer.get(d) {
                self.set(d, on);
            }
        }
        self
    }
}

/// A seat's explicit handicap and automatic decisions, which outlast a change of controller
/// (`Player.overrides`, `state.py:206`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SeatOverrides {
    pub handicap: Option<Handicap>,
    pub auto: AutoOverrides,
}

/// A seat setting the lobby or a scenario sent that is not allowed. The message names the rule.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SeatError(pub String);

impl SeatOverrides {
    /// Checks a seat's `handicap` and `auto` settings as the lobby sends them, and returns them
    /// in the form a seat keeps (`seat_overrides`, `state.py:44-63`).
    ///
    /// Absent, `null` and `""` leave the handicap to the controller; an absent or empty (falsy)
    /// `auto` sets nothing. Values must be true or false: coercing the string `"false"` would
    /// hand the seat the very decision it declined.
    pub fn parse(handicap: Option<&Value>, auto: Option<&Value>) -> Result<Self, SeatError> {
        let mut out = Self::default();
        match handicap {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) if s.is_empty() => {}
            Some(Value::String(s)) if Handicap::from_name(s).is_some() => {
                out.handicap = Handicap::from_name(s);
            }
            Some(other) => {
                return Err(SeatError(format!(
                    "handicap must be 'human' or 'ai', not {}.",
                    py_repr(other)
                )));
            }
        }
        let Some(auto) = auto.filter(|v| py_truthy(v)) else { return Ok(out) };
        let keys_ok = auto
            .as_object()
            .is_some_and(|m| m.keys().all(|k| AutoDecision::from_name(k).is_some()));
        let Some(map) = auto.as_object().filter(|_| keys_ok) else {
            let keys: Vec<&str> = AutoDecision::ALL.iter().map(|d| d.name()).collect();
            return Err(SeatError(format!(
                "auto must be an object whose keys are among {}, e.g. {{\"un_vote\": false}}.",
                keys.join(", ")
            )));
        };
        let bad: Vec<&str> =
            map.iter().filter(|(_, v)| !v.is_boolean()).map(|(k, _)| k.as_str()).collect();
        if !bad.is_empty() {
            return Err(SeatError(format!(
                "auto values must be true or false ({} is not).",
                bad.join(", ")
            )));
        }
        for (k, v) in map {
            if let (Some(d), Some(on)) = (AutoDecision::from_name(k), v.as_bool()) {
                out.auto.set(d, on);
            }
        }
        Ok(out)
    }
}

/// Python's truth value of a JSON value: null, false, zero and empty are false.
fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python's `repr` of a JSON value, for messages that quote a bad setting back.
fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => i.to_string(),
            (None, Some(f)) => PyFloat(f).to_string(),
            (None, None) => n.to_string(),
        },
        Value::String(s) => {
            let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
            let mut out = String::with_capacity(s.len() + 2);
            out.push(quote);
            for c in s.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if c == quote => {
                        out.push('\\');
                        out.push(c);
                    }
                    c => out.push(c),
                }
            }
            out.push(quote);
            out
        }
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", py_repr(&Value::String(k.clone())), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// A seat driver's own memory, opaque to the engine: saved as base64, digested as its bytes,
/// and handed back to the seat's driver each turn (DESIGN.md 4.5, 6.12). The Python bot kept its
/// plans in the object and lost them on every save (`bots/basic.py:678-697`).
///
/// Its fields are private so that every memory is built by [`new`](Self::new), which holds it to
/// [`MAX_LEN`](Self::MAX_LEN): the engine can never write a save it would refuse to load.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct DriverMemory {
    kind: u16,
    version: u16,
    bytes: Box<[u8]>,
}

/// A driver's memory over [`DriverMemory::MAX_LEN`] bytes, refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a driver's memory of {len} bytes is over the limit of {max}", max = DriverMemory::MAX_LEN)]
pub struct DriverTooLarge {
    /// The bytes offered.
    pub len: usize,
}

impl DriverMemory {
    /// The most bytes a driver may keep, which [`new`](Self::new) enforces on the way in and
    /// loading on the way back: enough for any plan, and small enough that a corrupt length
    /// cannot ask for gigabytes.
    pub const MAX_LEN: usize = 4 << 20;

    /// A driver's memory: `bytes` in the format `version` of driver `kind`. Refused over
    /// [`MAX_LEN`](Self::MAX_LEN) bytes.
    pub fn new(
        kind: u16,
        version: u16,
        bytes: impl Into<Box<[u8]>>,
    ) -> Result<Self, DriverTooLarge> {
        let bytes = bytes.into();
        if bytes.len() > Self::MAX_LEN {
            return Err(DriverTooLarge { len: bytes.len() });
        }
        Ok(Self { kind, version, bytes })
    }

    /// An empty memory for a driver.
    #[must_use]
    pub fn empty(kind: u16, version: u16) -> Self {
        Self { kind, version, bytes: Box::default() }
    }

    /// Which driver wrote it.
    #[must_use]
    pub const fn kind(&self) -> u16 {
        self.kind
    }

    /// The version of that driver's format.
    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    /// The bytes, as the driver wrote them.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The bytes, taken out.
    #[must_use]
    pub fn into_bytes(self) -> Box<[u8]> {
        self.bytes
    }
}

impl fmt::Debug for DriverMemory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "DriverMemory(kind {}, version {}, {} bytes)",
            self.kind,
            self.version,
            self.bytes.len()
        )
    }
}

/// Who plays a seat and on what terms (the Phase 0 split, `state.py:20-63, 201-207, 274-296`).
///
/// The handicap and automatic decisions follow the controller, except where the seat set them
/// explicitly: those overrides outlast a change of controller. The automatic decisions may also
/// be changed during play; a new controller re-derives them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seat {
    controller: Controller,
    handicap: Handicap,
    auto: AutoDecisions,
    overrides: SeatOverrides,
    difficulty: Option<DifficultyId>,
    driver: Option<DriverMemory>,
}

impl Seat {
    /// A new seat: the controller's defaults, then the explicit settings on top.
    #[must_use]
    pub fn new(
        controller: Controller,
        overrides: SeatOverrides,
        difficulty: Option<DifficultyId>,
    ) -> Self {
        Self {
            controller,
            handicap: overrides.handicap.unwrap_or(controller.default_handicap()),
            auto: controller.default_auto().overlaid(overrides.auto),
            overrides,
            difficulty,
            driver: None,
        }
    }

    /// A seat as a save or an older game had it: a missing handicap is derived from the
    /// controller and the overrides, and the saved automatic decisions win over both
    /// (`Player.__post_init__`, `state.py:274-280`).
    #[must_use]
    pub fn restore(
        controller: Controller,
        handicap: Option<Handicap>,
        saved_auto: AutoOverrides,
        overrides: SeatOverrides,
        difficulty: Option<DifficultyId>,
        driver: Option<DriverMemory>,
    ) -> Self {
        let derived = Self::new(controller, overrides, difficulty);
        Self {
            handicap: handicap.unwrap_or(derived.handicap),
            auto: derived.auto.overlaid(saved_auto),
            driver,
            ..derived
        }
    }

    /// Who drives the turns.
    #[must_use]
    pub const fn controller(&self) -> Controller {
        self.controller
    }

    /// The difficulty numbers it gets.
    #[must_use]
    pub const fn handicap(&self) -> Handicap {
        self.handicap
    }

    /// Whether it gets a human's difficulty numbers and matches UnCiv's "Human player" filter.
    #[must_use]
    pub const fn is_humanlike(&self) -> bool {
        matches!(self.handicap, Handicap::Human)
    }

    /// The decisions the engine takes for it.
    #[must_use]
    pub const fn auto(&self) -> AutoDecisions {
        self.auto
    }

    /// Its explicit settings.
    #[must_use]
    pub const fn overrides(&self) -> SeatOverrides {
        self.overrides
    }

    /// Its own difficulty, or `None` for the game's.
    #[must_use]
    pub const fn difficulty(&self) -> Option<DifficultyId> {
        self.difficulty
    }

    /// Its driver's memory, if the driver has kept any.
    #[must_use]
    pub const fn driver(&self) -> Option<&DriverMemory> {
        self.driver.as_ref()
    }

    /// Hands the seat to another controller (`Player.set_controller`, `state.py:282-296`).
    ///
    /// `handicap` and `auto` become explicit settings (`auto` may name only some decisions);
    /// everything not set explicitly, now or before, follows the new controller.
    pub(super) fn set_controller(
        &mut self,
        controller: Controller,
        handicap: Option<Handicap>,
        auto: AutoOverrides,
    ) {
        if let Some(h) = handicap {
            self.overrides.handicap = Some(h);
        }
        self.overrides.auto = self.overrides.auto.merged(auto);
        self.controller = controller;
        self.handicap = self.overrides.handicap.unwrap_or(controller.default_handicap());
        self.auto = controller.default_auto().overlaid(self.overrides.auto);
    }

    /// Changes one automatic decision for now, not as an override: a new controller re-derives
    /// it.
    pub(super) fn set_auto(&mut self, d: AutoDecision, on: bool) {
        self.auto.set(d, on);
    }

    /// Sets the seat's own difficulty, or `None` for the game's.
    pub(super) fn set_difficulty(&mut self, d: Option<DifficultyId>) {
        self.difficulty = d;
    }

    fn take_driver(&mut self) -> Option<DriverMemory> {
        self.driver.take()
    }

    fn put_driver(&mut self, mem: Option<DriverMemory>) {
        self.driver = mem;
    }
}

// ---- Colours ----------------------------------------------------------------------------------

/// A civilization's colour.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgb(pub [u8; 3]);

impl Rgb {
    /// The colour of `#rrggbb` text, spaces around it and letter case ignored; `None` for
    /// anything else (`game.py:62-67`).
    #[must_use]
    pub fn from_hex(text: &str) -> Option<Self> {
        let t = text.trim();
        let hex = t
            .strip_prefix('#')
            .filter(|h| h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit()))?;
        let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
        Some(Self([byte(0)?, byte(2)?, byte(4)?]))
    }

    /// `#rrggbb`, lower case.
    #[must_use]
    pub fn to_hex(self) -> String {
        let [r, g, b] = self.0;
        format!("#{r:02x}{g:02x}{b:02x}")
    }
}

impl fmt::Debug for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

// ---- Stocks and progress ----------------------------------------------------------------------

/// Stocks, golden ages and the recent history of yields.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Economy {
    pub gold: f64,
    pub culture: f64,
    pub faith: f64,
    pub golden_age_points: f64,
    /// Turns of golden age left.
    pub golden_age_turns: i32,
    /// Golden ages had.
    pub golden_ages: i32,
    /// Culture earned over the game (`turns.py:92`).
    pub total_culture: i64,
    /// Faith earned over the game (`turns.py:100`).
    pub total_faith: i64,
    /// Culture per turn over the last 8 turns, slot `turn % 8` (`policies.py:160-161`).
    pub culture_hist: [i32; 8],
    /// Science per turn over the last 8 turns, slot `turn % 8` (`research.py:187-188`).
    pub science_hist: [i32; 8],
    /// Gold per turn at the end of the last turn, which citizen ranking reads (`cities.py:765,
    /// 777-784`). Python read it and never wrote it; it is written at stage E2 (DESIGN.md 6.6).
    pub last_gold_rate: f64,
    /// Happiness as the conditionals and citizen ranking see it, committed at fixed stages of a
    /// turn (DESIGN.md 6.6).
    pub happiness_seen: i32,
}

/// Research.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TechState {
    pub known: TechSet,
    /// The current tech, then the rest of the path to the goal.
    pub queue: Vec<TechId>,
    pub goal: Option<TechId>,
    /// Science invested in each tech.
    pub progress: BTreeMap<TechId, f64>,
    pub overflow: f64,
    pub free_techs: i32,
    pub future_techs: i32,
    /// Science waiting from concluded research agreements (Python's `flags["ra_science"]`).
    pub ra_bonus: i32,
}

/// Social policies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PolicyState {
    /// Adopted policies and branches, finishers included.
    pub adopted: PolicySet,
    /// Policies counted for cost (UnCiv's `numberOfAdoptedPolicies`).
    pub adopted_count: i32,
    pub free_policies: i32,
}

/// Great people: points toward the next one, and the thresholds that rise with each.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GreatPeople {
    /// Points toward each great person, from buildings and specialists.
    pub points: BTreeMap<BaseUnitId, f64>,
    /// Points earned in combat, toward great generals and admirals.
    pub combat_points: BTreeMap<BaseUnitId, f64>,
    /// The next threshold of each great-person pool, `None` being the shared pool of the units
    /// with no `Is part of Great Person group []` unique (`great_people.py:75-81`).
    pub pool_threshold: BTreeMap<Option<TextId>, i64>,
    /// The next threshold of each combat great person.
    pub combat_threshold: BTreeMap<BaseUnitId, i64>,
    /// Free great people to choose.
    pub free: i32,
    pub earned: i32,
    pub prophets_earned: i32,
    /// Free great people still owed by the Maya calendar (`great_people.py:182-235`).
    pub maya_limited: i32,
    /// The great people the Maya may still choose from.
    pub long_count_pool: Vec<BaseUnitId>,
}

/// A civilization's religion.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReligionState {
    pub progress: ReligionProgress,
    /// The religion (or pantheon) it founded.
    pub founded: Option<ReligionId>,
    /// Free beliefs to choose, by kind, slot [`BeliefKind::index`] (Python's
    /// `flags["free_beliefs"]`): `Gain a free [Any] belief` counts in the fifth slot.
    pub free_beliefs: [u8; BeliefKind::COUNT],
    /// Whether founding a religion straight away still owes it a pantheon belief.
    pub choose_pantheon_belief: bool,
}

impl ReligionState {
    /// The free beliefs of one kind.
    #[must_use]
    pub const fn free(&self, k: BeliefKind) -> u8 {
        self.free_beliefs[k.index()]
    }

    /// Sets the free beliefs of one kind.
    pub fn set_free(&mut self, k: BeliefKind, n: u8) {
        self.free_beliefs[k.index()] = n;
    }
}

/// A unique a civilization holds for some turns (`{"text", "turns"}`, `triggers.py:91`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TempUnique {
    pub unique: UniqueId,
    /// Turns left.
    pub turns: i16,
}

/// The rest of a civilization's standing data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CivExtras {
    pub temp_uniques: Vec<TempUnique>,
    /// Times each item was built, for "Cost increases when built".
    pub built_increasing: BTreeMap<Constructible, u16>,
    /// Times each item was bought with increasing cost, great prophets included.
    pub bought_increasing: BTreeMap<Constructible, u16>,
    /// Free buildings of a stat already granted, by (stat, city), sorted.
    pub free_stat_buildings: Vec<(Stat, CityId)>,
    /// Specific free buildings already granted, by (building, city), sorted.
    pub free_specific_buildings: Vec<(BuildingId, CityId)>,
    /// Natural wonders it has discovered.
    pub natural_wonders: TerrainSet,
    /// Units it has gained, which "after gaining [unit]" reads (`movement.py:134`). Python read
    /// `flags["gained_<unit>"]` and never wrote it, so Carthage never crossed mountains.
    pub units_gained: BaseUnitSet,
    /// Turns until the next revolt while unhappy.
    pub revolt_in: Option<i16>,
    /// The last two ruin rewards, newest last (`ruins.py:45`).
    pub last_ruins: [Option<RuinId>; 2],
    /// Exploration targets its explorers gave up on, sorted, at most 300 (`automation.py:282`).
    pub explore_skip: Vec<TileIdx>,
}

// ---- Major civilizations ----------------------------------------------------------------------

/// A spy (`espionage.py:4`). Spies are not map units.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Spy {
    pub name: Box<str>,
    pub rank: u8,
    /// Its city, or `None` for the hideout.
    pub city: Option<CityId>,
    pub action: SpyAction,
    /// Turns left in the current action.
    pub turns: i16,
    /// Progress toward stealing a tech.
    pub progress: i32,
}

/// What only a major civilization has.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MajorData {
    /// Its memory of tiles out of sight.
    pub memory: TileMemoryLayer,
    pub spies: Vec<Spy>,
    /// Eras whose "spy on entering" reward it has had (`espionage.py:73`).
    pub spy_eras_earned: EraSet,
    /// Spaceship parts added, by part.
    pub spaceship: BTreeMap<BaseUnitId, u16>,
    /// Its own notes (an LLM's scratchpad).
    pub notes: Box<str>,
    /// City-states it has attacked, which makes others wary (`city_states.py:716`).
    pub cs_attacks: u16,
    /// Turns until allied city-states gift it a great person (`city_states.py:363-380`).
    pub cs_gp_gift: Option<i16>,
}

impl MajorData {
    /// The data of a new civilization on a map of `tiles` tiles.
    #[must_use]
    pub fn new(tiles: u32) -> Self {
        Self { memory: TileMemoryLayer::new(tiles), ..Self::default() }
    }
}

// ---- City-states ------------------------------------------------------------------------------

/// What a city-state quest is about, typed by the quest's kind (DESIGN.md 4.5). Python kept it
/// untyped in `data1`; `data2` was always empty and is dropped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum QuestTarget {
    /// Nothing: the route quest.
    #[default]
    None,
    /// The barbarian camp to clear.
    Tile(TileIdx),
    Resource(ResourceId),
    /// The wonder to build.
    Building(BuildingId),
    /// The great person to acquire.
    UnitType(BaseUnitId),
    /// The civilization or city-state to find, conquer, bully or denounce.
    Player(PlayerId),
    NaturalWonder(TerrainId),
    Religion(ReligionId),
    /// A contest's starting score: culture, faith or techs when the quest was given.
    Baseline(i64),
    /// The investment bonus, in percent.
    Percent(i16),
}

impl QuestTarget {
    /// The kind of target this is.
    #[must_use]
    pub const fn kind(self) -> QuestTargetKind {
        match self {
            Self::None => QuestTargetKind::None,
            Self::Tile(_) => QuestTargetKind::Tile,
            Self::Resource(_) => QuestTargetKind::Resource,
            Self::Building(_) => QuestTargetKind::Building,
            Self::UnitType(_) => QuestTargetKind::UnitType,
            Self::Player(_) => QuestTargetKind::Player,
            Self::NaturalWonder(_) => QuestTargetKind::NaturalWonder,
            Self::Religion(_) => QuestTargetKind::Religion,
            Self::Baseline(_) => QuestTargetKind::Baseline,
            Self::Percent(_) => QuestTargetKind::Percent,
        }
    }
}

/// A quest a city-state gave (`city_states.py:1055-1062`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Quest {
    /// Its row of `quests.json`.
    pub kind: QuestKindId,
    pub assignee: PlayerId,
    /// The turn it was given.
    pub turn: Turn,
    /// Given to one civilization, or to all as a contest.
    pub scope: QuestScope,
    pub target: QuestTarget,
    /// Influence it rewards.
    pub influence: i32,
    /// Turns it lasts, 0 for no limit.
    pub duration: i32,
}

/// When a city-state next gives quests: -1 means not scheduled, 0 due, more a countdown
/// (Python's `flags["quest_state"]`, `city_states.py:983-997`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestTimers {
    /// The countdown to the next global quest.
    pub global: i16,
    /// The countdown to each major's next individual quest.
    pub individual: BTreeMap<PlayerId, i16>,
}

impl Default for QuestTimers {
    fn default() -> Self {
        Self { global: -1, individual: BTreeMap::new() }
    }
}

impl QuestTimers {
    /// A major's countdown; -1 if none is scheduled.
    #[must_use]
    pub fn individual(&self, p: PlayerId) -> i16 {
        self.individual.get(&p).copied().unwrap_or(-1)
    }
}

/// A city-state's standing with one major: countdowns, and what it remembers of them
/// (Python's `flags["pairs"][major]`, `city_states.py:25-31`). A countdown at 0 is off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CsPair {
    /// Turns since the major last demanded tribute, counting down.
    pub bullied: i16,
    /// Turns before a pledge to protect may be withdrawn.
    pub pledged: i16,
    /// Turns before protection may be pledged again.
    pub withdrew: i16,
    pub border_conflict: i16,
    /// Turns the city-state forgives military units in its land.
    pub anger_free: i16,
    pub recently_attacked: i16,
    pub marriage_cooldown: i16,
    /// Turns to its next gift of a military unit, while a friend or ally.
    pub unit_timer: Option<i16>,
    /// Whether it has grown wary of the major's aggression.
    pub wary: bool,
}

/// The "kill the attacker's units" pseudo-quest of a city-state under attack
/// (`city_states.py:731-760`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WarQuest {
    /// Kills wanted.
    pub needed: u16,
    /// Kills made, by killer.
    pub kills: BTreeMap<PlayerId, u16>,
}

/// What only a city-state has.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CityStateData {
    pub cs_type: Option<CityStateTypeId>,
    pub personality: Option<CityStatePersonality>,
    /// Its unique luxury (Mercantile).
    pub resource: Option<ResourceId>,
    /// The unit it gifts (Militaristic).
    pub unique_unit: Option<BaseUnitId>,
    /// Influence of each major.
    pub influence: PlayerVec<f64>,
    ally: Option<PlayerId>,
    pub protectors: PlayerSet,
    pub quests: Vec<Quest>,
    pub timers: QuestTimers,
    pub pairs: PlayerVec<CsPair>,
    /// War pseudo-quests, by attacker.
    pub war_quests: BTreeMap<PlayerId, WarQuest>,
    /// Turns to its next election (espionage), once scheduled.
    pub election_in: Option<i16>,
    /// Cooldown on calling for help against barbarians.
    pub barb_help_cd: i16,
    pub recently_bullied: i16,
}

impl CityStateData {
    /// A new city-state of this type.
    #[must_use]
    pub fn new(cs_type: Option<CityStateTypeId>) -> Self {
        Self { cs_type, ..Self::default() }
    }

    /// Its ally, if any. It changes only through `State::set_ally`, which reports the change.
    #[must_use]
    pub const fn ally(&self) -> Option<PlayerId> {
        self.ally
    }

    pub(super) fn set_ally(&mut self, ally: Option<PlayerId>) -> Option<PlayerId> {
        core::mem::replace(&mut self.ally, ally)
    }

    /// A major's influence.
    #[must_use]
    pub fn influence_of(&self, p: PlayerId) -> f64 {
        self.influence.get(p).copied().unwrap_or(0.0)
    }

    /// The standing with a major.
    #[must_use]
    pub fn pair(&self, p: PlayerId) -> CsPair {
        self.pairs.get(p).copied().unwrap_or_default()
    }
}

// ---- Player -----------------------------------------------------------------------------------

/// One civilization, city-state or the barbarians (`state.py:193-272`).
///
/// Its seat, and whether it is alive, are private: they change through `State`, which reports
/// the change.
#[derive(Clone, Debug, PartialEq)]
pub struct Player {
    id: PlayerId,
    seat: Seat,
    alive: bool,
    eliminated_turn: Option<Turn>,
    pub kind: PlayerKind,
    /// The civilization's display name, which players may change.
    pub name: Box<str>,
    pub leader: Box<str>,
    pub color: Rgb,
    pub nation: NationId,
    pub capital: Option<CityId>,
    pub original_capital: Option<CityId>,
    pub founded_city: bool,
    /// Cities founded, for naming the next.
    pub city_counter: u16,
    /// Where it started.
    pub start_tile: Option<TileIdx>,
    /// Tiles it has seen; the barbarians keep none.
    pub explored: BitSet,
    pub econ: Economy,
    pub tech: TechState,
    pub policy: PolicyState,
    pub gp: GreatPeople,
    pub religion: ReligionState,
    pub civ: CivExtras,
    /// Present for a major civilization.
    pub major: Option<Box<MajorData>>,
    /// Present for a city-state.
    pub city_state: Option<Box<CityStateData>>,
}

impl Player {
    /// A new player on a map of `tiles` tiles, alive, with nothing yet, and the data of its
    /// kind.
    #[must_use]
    pub fn new(
        id: PlayerId,
        kind: PlayerKind,
        name: Box<str>,
        nation: NationId,
        color: Rgb,
        seat: Seat,
        tiles: u32,
    ) -> Self {
        Self {
            id,
            seat,
            alive: true,
            eliminated_turn: None,
            kind,
            name,
            leader: Box::default(),
            color,
            nation,
            capital: None,
            original_capital: None,
            founded_city: false,
            city_counter: 0,
            start_tile: None,
            explored: BitSet::new(),
            econ: Economy::default(),
            tech: TechState::default(),
            policy: PolicyState::default(),
            gp: GreatPeople::default(),
            religion: ReligionState::default(),
            civ: CivExtras::default(),
            major: (kind == PlayerKind::Major).then(|| Box::new(MajorData::new(tiles))),
            city_state: (kind == PlayerKind::CityState).then(|| Box::new(CityStateData::default())),
        }
    }

    /// A player as a save or the converter has it, alive or eliminated.
    #[must_use]
    pub fn restore(mut self, alive: bool, eliminated_turn: Option<Turn>) -> Self {
        self.alive = alive;
        self.eliminated_turn = eliminated_turn;
        self
    }

    /// Its id: its position in the player list.
    #[must_use]
    #[inline]
    pub const fn id(&self) -> PlayerId {
        self.id
    }

    /// Its seat.
    #[must_use]
    #[inline]
    pub const fn seat(&self) -> &Seat {
        &self.seat
    }

    /// Takes its driver's memory out for a turn; `drive` hands it back with
    /// [`put_driver`](Self::put_driver) (DESIGN.md 6.12). The memory feeds no cache, so this is
    /// no seat change.
    pub fn take_driver(&mut self) -> Option<DriverMemory> {
        self.seat.take_driver()
    }

    /// Stores its driver's memory.
    pub fn put_driver(&mut self, mem: Option<DriverMemory>) {
        self.seat.put_driver(mem);
    }

    pub(super) fn seat_mut(&mut self) -> &mut Seat {
        &mut self.seat
    }

    /// Whether it is still in the game.
    #[must_use]
    #[inline]
    pub const fn alive(&self) -> bool {
        self.alive
    }

    /// The turn it was eliminated, if it was.
    #[must_use]
    pub const fn eliminated_turn(&self) -> Option<Turn> {
        self.eliminated_turn
    }

    pub(super) fn set_alive(&mut self, alive: bool, eliminated_turn: Option<Turn>) {
        self.alive = alive;
        self.eliminated_turn = eliminated_turn;
    }

    /// Whether it is a major civilization.
    #[must_use]
    pub fn is_major(&self) -> bool {
        self.kind == PlayerKind::Major
    }

    /// Whether it is a city-state.
    #[must_use]
    pub fn is_city_state(&self) -> bool {
        self.kind == PlayerKind::CityState
    }

    /// Whether it is the barbarians.
    #[must_use]
    pub fn is_barbarian(&self) -> bool {
        self.kind == PlayerKind::Barbarian
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repr_quotes_like_python() {
        assert_eq!(py_repr(&json!("deity")), "'deity'");
        assert_eq!(py_repr(&json!("it's")), "\"it's\"");
        assert_eq!(py_repr(&json!(5)), "5");
        assert_eq!(py_repr(&json!(2.5)), "2.5");
        assert_eq!(py_repr(&json!(false)), "False");
        assert_eq!(py_repr(&json!(["a", 1])), "['a', 1]");
    }

    #[test]
    fn colours_parse_as_python_did() {
        assert_eq!(Rgb::from_hex(" #AA0000 "), Some(Rgb([0xaa, 0, 0])));
        assert_eq!(Rgb::from_hex("#aa000"), None);
        assert_eq!(Rgb::from_hex("aa0000"), None);
        assert_eq!(Rgb::from_hex("#gg0000"), None);
        assert_eq!(Rgb([0x29, 0x9b, 0xcc]).to_hex(), "#299bcc");
    }

    #[test]
    fn a_new_player_gets_the_data_of_its_kind() {
        let seat = Seat::new(Controller::Minor, SeatOverrides::default(), None);
        let cs = Player::new(
            PlayerId(3),
            PlayerKind::CityState,
            "Geneva".into(),
            NationId(40),
            Rgb::default(),
            seat,
            100,
        );
        assert!(cs.city_state.is_some() && cs.major.is_none() && cs.alive());
        let seat = Seat::new(Controller::Bot, SeatOverrides::default(), None);
        let p = Player::new(
            PlayerId(0),
            PlayerKind::Major,
            "Rome".into(),
            NationId(1),
            Rgb::default(),
            seat,
            100,
        );
        assert_eq!(p.major.as_ref().map(|m| m.memory.len()), Some(100));
        assert_eq!(p.seat().handicap(), Handicap::Ai);
    }

    #[test]
    fn free_beliefs_have_a_slot_for_any() {
        let mut r = ReligionState::default();
        r.set_free(BeliefKind::Any, 2);
        r.set_free(BeliefKind::Type(crate::rules::defs::BeliefType::Follower), 1);
        assert_eq!(r.free_beliefs, [0, 0, 1, 0, 2]);
        assert_eq!(r.free(BeliefKind::Any), 2);
    }

    #[test]
    fn a_driver_memory_comes_out_for_a_turn_and_goes_back() {
        let seat = Seat::new(Controller::Bot, SeatOverrides::default(), None);
        let mut p = Player::new(
            PlayerId(0),
            PlayerKind::Major,
            "Rome".into(),
            NationId(1),
            Rgb::default(),
            seat,
            4,
        );
        assert!(p.take_driver().is_none());
        p.put_driver(DriverMemory::new(1, 2, vec![7, 8]).ok());
        let held = p.seat().driver().map(|d| (d.kind(), d.version(), d.bytes().len()));
        assert_eq!(held, Some((1, 2, 2)));
        assert_eq!(p.take_driver().map(|d| d.into_bytes().to_vec()), Some(vec![7, 8]));
        assert!(p.seat().driver().is_none());
        // A memory over the limit cannot be built, so no seat can hold one.
        let most = DriverMemory::new(1, 2, vec![0; DriverMemory::MAX_LEN]);
        assert!(most.is_ok_and(|d| d.bytes().len() == DriverMemory::MAX_LEN));
        let over = DriverMemory::new(1, 2, vec![0; DriverMemory::MAX_LEN + 1]);
        assert_eq!(over, Err(DriverTooLarge { len: DriverMemory::MAX_LEN + 1 }));
        assert_eq!(
            format!("{:?}", DriverMemory::empty(3, 1)),
            "DriverMemory(kind 3, version 1, 0 bytes)"
        );
        assert!(SpyAction::Coup.is_set_up() && !SpyAction::Moving.is_set_up());
    }
}
