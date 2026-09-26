//! History: events, messages, thoughts, stats rows, action records and replay frames, and the
//! heads `State` keeps of them (DESIGN.md 4.7).
//!
//! Replaces `GameState.events`, `messages`, `thoughts` and `stats` (`state.py:352-356`), the
//! event records of `Game.emit` (`game.py:806-878`), the stats rows of `victory.record_stats`
//! (`victory.py:426-455`), the message records of `diplomacy.add_message` (`diplomacy.py:306-311`)
//! and `Game.action_log` (`tools.py:130`).
//!
//! History is not state: it is append-only and lives in a [`Chronicle`] beside the game. `State`
//! keeps two heads of it:
//! - [`ChronicleHeads`], digested: counts of what the engine produced (events, messages, stats
//!   rows) and a running hash that pins their wording;
//! - [`HostHeads`], saved but never digested: counts of host activity (host events, thoughts,
//!   action records, frames, journal chunks) and the one event id sequence engine and host events
//!   share. Host activity must not move the digest.

use smallvec::SmallVec;

use super::cities::Constructible;
use super::diplo::NegStatus;
use super::world::UnResult;
use crate::base::ids::{
    BaseUnitId, BeliefId, BuildingId, CityId, DealId, EraId, EventId, ImprovementId, MessageId,
    NegotiationId, PlayerId, PolicyId, ReligionId, RuinId, TechId, TileIdx, Turn, UnitId,
    VictoryId,
};
use crate::base::sets::PlayerSet;

/// Defines [`EngineEvent`] from its table: the variant, the type name `emit` was called with,
/// and whether the event is private (`PRIVATE_EVENTS`, `game.py:808-812`).
macro_rules! engine_events {
    ($($variant:ident $name:literal $private:literal,)*) => {
        /// An event type the engine emits: every type `citar/engine` passes to `emit`
        /// (a test lists them from the Python sources).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum EngineEvent {
            $(
                #[doc = concat!("`", $name, "`")]
                $variant,
            )*
        }

        impl EngineEvent {
            /// Every type, in name order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];

            /// The type's name: `city_founded`.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }

            /// Whether only its audience hears of it, never the civilizations that can see its
            /// tile: a rival is not told what a city built or who was born there
            /// (`PRIVATE_EVENTS`, `game.py:808-812, 872`).
            #[must_use]
            pub const fn is_private(self) -> bool {
                match self {
                    $(Self::$variant => $private,)*
                }
            }
        }
    };
}

engine_events! {
    Bankrupt "bankrupt" false,
    Borders "borders" true,
    BuildFailed "build_failed" false,
    BuildingBuilt "building_built" true,
    CampCleared "camp_cleared" false,
    CampRecruit "camp_recruit" false,
    CampSpawned "camp_spawned" false,
    CapitalMoved "capital_moved" false,
    Chop "chop" false,
    CityAutoProduction "city_auto_production" true,
    CityCaptured "city_captured" false,
    CityDemand "city_demand" true,
    CityDestroyed "city_destroyed" false,
    CityFounded "city_founded" false,
    CityGrowth "city_growth" true,
    CityIdle "city_idle" true,
    CityRazed "city_razed" false,
    CityRenamed "city_renamed" false,
    CitySacked "city_sacked" false,
    CityStarving "city_starving" true,
    CivRenamed "civ_renamed" false,
    CivilianRecaptured "civilian_recaptured" false,
    CivilianReturned "civilian_returned" false,
    Combat "combat" false,
    CsAlly "cs_ally" false,
    CsAllyLost "cs_ally_lost" false,
    CsBorder "cs_border" false,
    CsGift "cs_gift" false,
    CsGrateful "cs_grateful" false,
    CsMarried "cs_married" false,
    CsMeet "cs_meet" false,
    CsQuest "cs_quest" false,
    CsRelationship "cs_relationship" false,
    CsWary "cs_wary" false,
    Deal "deal" false,
    DealCut "deal_cut" false,
    DealExpired "deal_expired" false,
    Denounce "denounce" false,
    Eliminated "eliminated" false,
    Era "era" false,
    ExploreDone "explore_done" true,
    Faith "faith" true,
    FirstContact "first_contact" false,
    FreeTech "free_tech" false,
    FreeUnit "free_unit" false,
    Friendship "friendship" false,
    Gain "gain" false,
    GameOver "game_over" false,
    GameStart "game_start" false,
    GoldenAge "golden_age" false,
    GoldenAgeEnd "golden_age_end" false,
    GreatPersonAvailable "great_person_available" false,
    GreatPersonBorn "great_person_born" true,
    ImprovementBuilt "improvement_built" false,
    Liberated "liberated" false,
    Loot "loot" false,
    Message "message" false,
    NaturalWonder "natural_wonder" false,
    Negotiation "negotiation" false,
    Nuke "nuke" false,
    OrdersInterrupted "orders_interrupted" true,
    Pact "pact" false,
    Pantheon "pantheon" false,
    Peace "peace" false,
    Pillaged "pillaged" false,
    Plunder "plunder" false,
    Policy "policy" false,
    PolicyAvailable "policy_available" true,
    ProductionBlocked "production_blocked" true,
    ProductionInvalid "production_invalid" true,
    PromotionReady "promotion_ready" true,
    Religion "religion" false,
    ReligionEnhanced "religion_enhanced" false,
    ReligionFounded "religion_founded" false,
    ResearchAgreement "research_agreement" false,
    ResearchNeeded "research_needed" false,
    ResistanceEnd "resistance_end" true,
    Revolt "revolt" false,
    Ruins "ruins" false,
    Spaceship "spaceship" false,
    Spy "spy" true,
    Tech "tech" false,
    TradeMission "trade_mission" false,
    TurnEnd "turn_end" false,
    TurnStart "turn_start" false,
    UnVote "un_vote" false,
    UnitBuilt "unit_built" true,
    UnitCaptured "unit_captured" false,
    UnitKilled "unit_killed" false,
    UnitWoke "unit_woke" true,
    Victory "victory" false,
    WarDeclared "war_declared" false,
    Wltkd "wltkd" true,
    WltkdEnd "wltkd_end" true,
    WonderBuilt "wonder_built" false,
    WonderRefund "wonder_refund" true,
    WonderStarted "wonder_started" false,
}

/// A save or journal writes an event type by its name, the digest by its index.
impl serde::Serialize for EngineEvent {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_unit_variant("EngineEvent", *self as u32, self.name())
    }
}

impl<'de> serde::Deserialize<'de> for EngineEvent {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        crate::base::codec::deserialize_by_name(d, "event type", Self::ALL, Self::name)
    }
}

impl EngineEvent {
    /// The type called `name`, exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .binary_search_by(|e| e.name().cmp(name))
            .ok()
            .and_then(|i| Self::ALL.get(i).copied())
    }
}

/// An event's type: one the engine emits, or one a host adds (`agent_error`, `game_paused`,
/// `game_resumed`), which counts only in the host heads.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    Engine(EngineEvent),
    Host(Box<str>),
}

impl EventType {
    /// The type's name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Engine(e) => e.name(),
            Self::Host(s) => s,
        }
    }
}

/// What an event says in fields, beyond its text: every key `emit` is passed today, typed
/// (`game.py:860`). Scrubbing walks the typed player fields instead of `_EVENT_PID_KEYS`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventData {
    pub player: Option<PlayerId>,
    pub a: Option<PlayerId>,
    pub b: Option<PlayerId>,
    pub attacker: Option<PlayerId>,
    pub defender: Option<PlayerId>,
    pub awaiting: Option<PlayerId>,
    pub killer: Option<PlayerId>,
    pub owner: Option<PlayerId>,
    pub old_owner: Option<PlayerId>,
    pub new_owner: Option<PlayerId>,
    pub sender: Option<PlayerId>,
    pub winner: Option<PlayerId>,
    pub unit: Option<UnitId>,
    pub city: Option<CityId>,
    pub deal: Option<DealId>,
    pub negotiation: Option<NegotiationId>,
    pub message: Option<MessageId>,
    /// What was built or started: a building, a wonder or a unit.
    pub item: Option<Constructible>,
    pub unit_type: Option<BaseUnitId>,
    /// A building destroyed.
    pub building: Option<BuildingId>,
    pub improvement: Option<ImprovementId>,
    pub tech: Option<TechId>,
    pub policy: Option<PolicyId>,
    pub belief: Option<BeliefId>,
    pub era: Option<EraId>,
    pub religion: Option<ReligionId>,
    /// A ruin's reward.
    pub reward: Option<RuinId>,
    pub victory: Option<VictoryId>,
    pub status: Option<NegStatus>,
    pub gold: Option<i32>,
    pub citizen_killed: Option<bool>,
    /// A world leader vote's result.
    pub results: Option<Box<UnResult>>,
}

impl EventData {
    /// The keys, in field order, as `emit` was passed them.
    pub const KEYS: [&'static str; 32] = [
        "player",
        "a",
        "b",
        "attacker",
        "defender",
        "awaiting",
        "killer",
        "owner",
        "old_owner",
        "new_owner",
        "sender",
        "winner",
        "unit",
        "city",
        "deal",
        "negotiation",
        "message",
        "item",
        "unit_type",
        "building",
        "improvement",
        "tech",
        "policy",
        "belief",
        "era",
        "religion",
        "reward",
        "victory",
        "status",
        "gold",
        "citizen_killed",
        "results",
    ];

    /// Every player the data names, with the key naming it, for scrubbing unmet civilizations.
    pub fn players(&self) -> impl Iterator<Item = (&'static str, PlayerId)> + '_ {
        [
            ("player", self.player),
            ("a", self.a),
            ("b", self.b),
            ("attacker", self.attacker),
            ("defender", self.defender),
            ("awaiting", self.awaiting),
            ("killer", self.killer),
            ("owner", self.owner),
            ("old_owner", self.old_owner),
            ("new_owner", self.new_owner),
            ("sender", self.sender),
            ("winner", self.winner),
        ]
        .into_iter()
        .filter_map(|(k, p)| p.map(|p| (k, p)))
    }
}

/// What a name in an event's text refers to (Python's `"c"`, `"l"` and `"t"`).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    Civ,
    Leader,
    City,
}

impl RefKind {
    /// Python's one-letter code.
    #[must_use]
    pub const fn code(self) -> char {
        match self {
            Self::Civ => 'c',
            Self::Leader => 'l',
            Self::City => 't',
        }
    }

    /// The kind of Python's one-letter code.
    #[must_use]
    pub const fn from_code(c: char) -> Option<Self> {
        match c {
            'c' => Some(Self::Civ),
            'l' => Some(Self::Leader),
            't' => Some(Self::City),
            _ => None,
        }
    }
}

/// Where an event's text names a civilization, leader or city (`game.py:842-858`), so each viewer
/// can be shown "Unknown Civilization" for civilizations it has not met. Offsets are UTF-8 bytes;
/// Python's were code points.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct NameRef {
    pub start: u32,
    pub end: u32,
    pub player: PlayerId,
    pub kind: RefKind,
}

/// One event (`game.py:873-878`). Its `x` and `y` are derived from its tile.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub id: EventId,
    pub turn: Turn,
    pub kind: EventType,
    pub text: Box<str>,
    /// Who hears of it; `None` is everyone.
    pub audience: Option<PlayerSet>,
    pub tile: Option<TileIdx>,
    pub data: Option<Box<EventData>>,
    pub refs: SmallVec<[NameRef; 2]>,
}

/// A message between civilizations (`diplomacy.py:306-311`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub id: MessageId,
    pub turn: Turn,
    pub from: PlayerId,
    pub to: PlayerSet,
    pub text: Box<str>,
}

/// A seat's recorded reasoning, or an action or system note, for spectators and the replay
/// (`tools.py:1084`, `engine_api.py:642-645`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Thought {
    pub turn: Turn,
    pub player: PlayerId,
    pub text: Box<str>,
    /// What kind of note it is, if the host said.
    pub kind: Option<Box<str>>,
}

/// One major civilization's statistics at the end of a round (`victory.py:431-453`). It carries
/// every key of `scripts/refcheck/baseline.py`'s `STAT_KEYS`, which the Phase 2 runner writes
/// from these rows.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CivStats {
    pub player: PlayerId,
    pub alive: bool,
    pub score: i32,
    pub cities: u16,
    pub population: u32,
    pub land: u32,
    pub techs: u16,
    pub policies: u16,
    pub military: i64,
    pub gold: i64,
    pub gold_per_turn: f64,
    pub science: f64,
    pub culture: f64,
    pub faith: f64,
    pub production: f64,
    pub happiness: i32,
    pub era: EraId,
    pub units: u32,
    pub golden_age: bool,
}

impl CivStats {
    /// The keys Python's rows had, in field order.
    pub const KEYS: [&'static str; 18] = [
        "alive",
        "score",
        "cities",
        "population",
        "land",
        "techs",
        "policies",
        "military",
        "gold",
        "gold_per_turn",
        "science",
        "culture",
        "faith",
        "production",
        "happiness",
        "era",
        "units",
        "golden_age",
    ];

    /// The row of an eliminated civilization: not alive, score 0.
    #[must_use]
    pub fn eliminated(player: PlayerId) -> Self {
        Self {
            player,
            alive: false,
            score: 0,
            cities: 0,
            population: 0,
            land: 0,
            techs: 0,
            policies: 0,
            military: 0,
            gold: 0,
            gold_per_turn: 0.0,
            science: 0.0,
            culture: 0.0,
            faith: 0.0,
            production: 0.0,
            happiness: 0,
            era: EraId(0),
            units: 0,
            golden_age: false,
        }
    }
}

/// One round's statistics, a row per major civilization in id order.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatsRow {
    pub turn: Turn,
    pub civs: Vec<CivStats>,
}

/// A tool call as the host logged it, with no wall-clock time: hosts add their own.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRecord {
    pub turn: Turn,
    pub player: PlayerId,
    pub tool: Box<str>,
    /// The arguments as compact JSON.
    pub args: Box<str>,
}

/// One replay frame, as `save::journal`'s frame writer encoded it: a keyframe or a delta.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameRecord {
    pub turn: Turn,
    pub keyframe: bool,
    #[serde(with = "crate::base::codec::bytes_b64")]
    pub bytes: Box<[u8]>,
}

/// The replay frames, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameLog {
    pub frames: Vec<FrameRecord>,
}

/// What kind of entry was appended to a [`Chronicle`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Appended {
    Event,
    Message,
    Thought,
    Stats,
    Action,
    Frame,
}

/// A game's history, append-only, in memory beside its state.
///
/// Each kind of entry has its own list, and [`order`](Self::order) records the order they were
/// appended in across the lists: the journal writes entries in that order, so the running hash,
/// which folds events, messages and stats rows in as they happen, can be recomputed on load.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chronicle {
    events: Vec<Event>,
    messages: Vec<Message>,
    thoughts: Vec<Thought>,
    stats: Vec<StatsRow>,
    actions: Vec<ActionRecord>,
    frames: FrameLog,
    order: Vec<Appended>,
}

impl Chronicle {
    /// An empty history.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every event, oldest first.
    #[must_use]
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// The events after `since`, oldest first. Ids ascend with position, so this is a search.
    #[must_use]
    pub fn events_since(&self, since: u32) -> &[Event] {
        let start = self.events.partition_point(|e| e.id.get() <= since);
        self.events.get(start..).unwrap_or(&[])
    }

    /// Every message, oldest first.
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Every thought, oldest first.
    #[must_use]
    pub fn thoughts(&self) -> &[Thought] {
        &self.thoughts
    }

    /// Every stats row, oldest first.
    #[must_use]
    pub fn stats(&self) -> &[StatsRow] {
        &self.stats
    }

    /// Every action record, oldest first.
    #[must_use]
    pub fn actions(&self) -> &[ActionRecord] {
        &self.actions
    }

    /// The replay frames.
    #[must_use]
    pub fn frames(&self) -> &FrameLog {
        &self.frames
    }

    /// The kind of every entry, in the order they were appended.
    #[must_use]
    pub fn order(&self) -> &[Appended] {
        &self.order
    }

    /// Appends an event.
    pub fn push_event(&mut self, e: Event) {
        self.events.push(e);
        self.order.push(Appended::Event);
    }

    /// Appends a message.
    pub fn push_message(&mut self, m: Message) {
        self.messages.push(m);
        self.order.push(Appended::Message);
    }

    /// Appends a thought.
    pub fn push_thought(&mut self, t: Thought) {
        self.thoughts.push(t);
        self.order.push(Appended::Thought);
    }

    /// Appends a stats row.
    pub fn push_stats(&mut self, s: StatsRow) {
        self.stats.push(s);
        self.order.push(Appended::Stats);
    }

    /// Appends an action record.
    pub fn push_action(&mut self, a: ActionRecord) {
        self.actions.push(a);
        self.order.push(Appended::Action);
    }

    /// Appends a replay frame.
    pub fn push_frame(&mut self, f: FrameRecord) {
        self.frames.frames.push(f);
        self.order.push(Appended::Frame);
    }
}

/// What a running-hash entry is, so an event and a message with the same bytes differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EntryKind {
    Event = 1,
    Message = 2,
    Stats = 3,
}

/// The heads of what the engine produced, kept in `State` and digested.
///
/// The running hash is `blake3(hash ‖ kind ‖ canon(entry))`; for an event, `canon(entry)`
/// covers everything but its id, which host events shift. It pins event wording across platforms,
/// which the state digest alone would miss.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChronicleHeads {
    pub engine_events: u32,
    pub messages: u32,
    pub stats: u32,
    #[serde(with = "crate::base::codec::hash_hex")]
    pub hash: [u8; 32],
    /// The newest stats row, which `save::summary` reads for the scores.
    pub last_stats: Option<StatsRow>,
}

impl ChronicleHeads {
    /// Folds one entry's canonical bytes into the running hash and counts it.
    pub fn absorb(&mut self, kind: EntryKind, canon: &[u8]) {
        let mut h = blake3::Hasher::new();
        h.update(&self.hash);
        h.update(&[kind as u8]);
        h.update(canon);
        self.hash = *h.finalize().as_bytes();
        let n = match kind {
            EntryKind::Event => &mut self.engine_events,
            EntryKind::Message => &mut self.messages,
            EntryKind::Stats => &mut self.stats,
        };
        *n = n.saturating_add(1);
    }
}

/// The heads of host activity, kept in `State`, saved, never digested.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostHeads {
    /// The next event id, shared by engine and host events so the feed stays one sequence.
    pub next_event_id: u32,
    pub host_events: u32,
    pub thoughts: u32,
    pub actions: u32,
    pub frames: u32,
    /// Journal chunks taken.
    pub journal_seq: u32,
    /// Where `Game::drive` stopped inside a turn, if it did. Absent from saves made before it.
    #[serde(default)]
    pub drive: Option<DriveMark>,
}

impl Default for HostHeads {
    fn default() -> Self {
        Self {
            next_event_id: 1,
            host_events: 0,
            thoughts: 0,
            actions: 0,
            frames: 0,
            journal_seq: 0,
            drive: None,
        }
    }
}

/// Where `Game::drive` stopped inside a turn (DESIGN.md 6.12): the seat whose driver has played
/// the turn, which drive has not ended yet, so that the next drive does not play it again, and
/// whether the seat's diplomat has had its stop since. It is the host's progress through a turn,
/// not the game's: saved, so that a game saved at such a stop resumes there, and never digested.
/// Beginning or ending a turn clears it, so a round's digest is taken without it whoever drove.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriveMark {
    /// The turn it was played in.
    pub turn: Turn,
    /// The seat whose driver played it.
    pub player: PlayerId,
    /// Whether drive has stopped for the seat's diplomat (`Stop::HybridDiplomat`).
    pub diplomat: bool,
}

impl HostHeads {
    /// Hands out the next event id; `None` once `u32` is spent.
    pub fn take_event_id(&mut self) -> Option<EventId> {
        let id = EventId::new(self.next_event_id)?;
        self.next_event_id = self.next_event_id.checked_add(1)?;
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_types_are_sorted_and_named_uniquely() {
        assert_eq!(EngineEvent::ALL.len(), 97);
        assert!(EngineEvent::ALL.windows(2).all(|w| w[0].name() < w[1].name()));
        for &e in EngineEvent::ALL {
            assert_eq!(EngineEvent::from_name(e.name()), Some(e));
        }
        assert_eq!(EngineEvent::from_name("agent_error"), None);
        assert_eq!(EngineEvent::ALL.iter().filter(|e| e.is_private()).count(), 22);
    }

    #[test]
    fn the_running_hash_depends_on_kind_order_and_bytes() {
        let mut a = ChronicleHeads::default();
        let mut b = ChronicleHeads::default();
        a.absorb(EntryKind::Event, b"x");
        b.absorb(EntryKind::Message, b"x");
        assert_ne!(a.hash, b.hash);
        let mut c = ChronicleHeads::default();
        c.absorb(EntryKind::Event, b"x");
        assert_eq!(a, c);
        c.absorb(EntryKind::Event, b"y");
        assert_eq!((c.engine_events, c.messages), (2, 0));
    }

    #[test]
    fn event_ids_start_at_one_and_events_since_skips_the_seen() {
        let mut host = HostHeads::default();
        let mut chron = Chronicle::new();
        for turn in 1..=4 {
            let Some(id) = host.take_event_id() else { return };
            chron.push_event(Event {
                id,
                turn,
                kind: EventType::Engine(EngineEvent::TurnStart),
                text: "t".into(),
                audience: None,
                tile: None,
                data: None,
                refs: SmallVec::new(),
            });
        }
        assert_eq!(chron.events()[0].id.get(), 1);
        assert_eq!(chron.events_since(2).iter().map(|e| e.turn).collect::<Vec<_>>(), [3, 4]);
        assert!(chron.events_since(9).is_empty());
        assert_eq!(chron.events_since(0).len(), 4);
    }

    #[test]
    fn event_data_names_its_players() {
        let d =
            EventData { a: Some(PlayerId(1)), winner: Some(PlayerId(4)), ..EventData::default() };
        assert_eq!(d.players().collect::<Vec<_>>(), [("a", PlayerId(1)), ("winner", PlayerId(4))]);
        let mut keys = EventData::KEYS.to_vec();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), EventData::KEYS.len());
    }
}
