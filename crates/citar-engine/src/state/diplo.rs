//! Diplomacy: the relation of every pair of players, opinions, deals and negotiations
//! (DESIGN.md 4.6).
//!
//! Replaces the relation and deal shapes of `diplomacy.py:60-96` and `530-900`, and
//! `GameState.relations`, `open_borders`, `deals` and `negotiations` (`state.py:348-351`):
//! - Python kept a relation per `"a,b"` key, created on first read (`diplomacy.py:65-74`), and
//!   open borders per `"a>b"` key. Here a [`PairMatrix`] holds a [`Relation`] for every pair
//!   from the start, open borders folded in, so a read never writes;
//! - who has met whom and who is at war are also kept as one [`PlayerSet`] per player, rebuilt
//!   on load, so the hot checks are one bit test; the barbarians are at war with everyone;
//! - opinions moved out of the relation into an [`OpinionBook`] keyed by holder, which fixes
//!   scenario opinions: they were written under `"a>b"` (`scenario.py:429`) and read by holder
//!   (`diplomacy.py:86-96`), so they never counted;
//! - deal items are a typed [`DealItem`] whose JSON is Python's item dict.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::change::{Change, Changes};
use crate::base::ids::{CityId, DealId, NegotiationId, PlayerId, ResourceId, TechId, Turn};
use crate::base::sets::{PlayerSet, PlayerVec};
use crate::rules::Ruleset;

// ---- PairMatrix -------------------------------------------------------------------------------

/// One value per unordered pair of distinct players.
///
/// The pair `(lo, hi)`, `lo < hi`, lives at `hi * (hi - 1) / 2 + lo`, so the cells of the first
/// `n` players come first and the matrix holds exactly `n * (n - 1) / 2` cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairMatrix<T> {
    n: u8,
    cells: Vec<T>,
}

impl<T> PairMatrix<T> {
    /// The cell index of the pair `a`, `b` in a matrix of `n` players; `None` if `a == b` or
    /// either is not a player.
    #[must_use]
    pub const fn index(n: u8, a: PlayerId, b: PlayerId) -> Option<usize> {
        let (lo, hi) = if a.0 < b.0 { (a.0, b.0) } else { (b.0, a.0) };
        if lo == hi || hi >= n {
            return None;
        }
        let hi = hi as usize;
        Some(hi * (hi - 1) / 2 + lo as usize)
    }

    /// The number of cells for `n` players.
    #[must_use]
    pub const fn cells_for(n: u8) -> usize {
        n as usize * (n as usize).saturating_sub(1) / 2
    }

    /// A matrix for `n` players with every cell `T::default()`.
    #[must_use]
    pub fn new(n: u8) -> Self
    where
        T: Default,
    {
        let mut cells = Vec::with_capacity(Self::cells_for(n));
        cells.resize_with(Self::cells_for(n), T::default);
        Self { n, cells }
    }

    /// A matrix from its cells in index order; `None` unless there are exactly
    /// `n * (n - 1) / 2` of them.
    #[must_use]
    pub fn from_cells(n: u8, cells: Vec<T>) -> Option<Self> {
        (cells.len() == Self::cells_for(n)).then_some(Self { n, cells })
    }

    /// The number of players.
    #[must_use]
    pub const fn n(&self) -> u8 {
        self.n
    }

    /// The cell of the pair `a`, `b`, in either order.
    #[must_use]
    pub fn get(&self, a: PlayerId, b: PlayerId) -> Option<&T> {
        self.cells.get(Self::index(self.n, a, b)?)
    }

    /// The cell of the pair `a`, `b`, in either order.
    pub fn get_mut(&mut self, a: PlayerId, b: PlayerId) -> Option<&mut T> {
        let i = Self::index(self.n, a, b)?;
        self.cells.get_mut(i)
    }

    /// Every cell in index order.
    #[must_use]
    pub fn cells(&self) -> &[T] {
        &self.cells
    }

    /// Every pair `(lo, hi)` with its cell, in index order: by `hi`, then `lo`.
    pub fn pairs(&self) -> impl Iterator<Item = (PlayerId, PlayerId, &T)> {
        (1..self.n)
            .flat_map(|hi| (0..hi).map(move |lo| (PlayerId(lo), PlayerId(hi))))
            .zip(&self.cells)
            .map(|((lo, hi), c)| (lo, hi, c))
    }
}

// ---- Relations --------------------------------------------------------------------------------

/// Which side of a pair a player is: 0 for the lower id, 1 for the higher.
#[must_use]
pub const fn side(a: PlayerId, b: PlayerId) -> usize {
    if a.0 < b.0 { 0 } else { 1 }
}

/// The relation between two players (`diplomacy.py:60-63`, plus `open_borders`).
///
/// Two-sided fields are indexed by [`side`]: `embassy[side(p, q)]` says whether `p` has an
/// embassy with `q`. A turn of 0 means never, since turns start at 1.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct Relation {
    pub met: bool,
    pub war: bool,
    pub war_declared_by: Option<PlayerId>,
    /// The turn the current war or peace began.
    pub since: Turn,
    /// The peace treaty holds until this turn.
    pub treaty_until: Turn,
    pub friendship_until: Turn,
    pub pact_until: Turn,
    /// The research agreement runs until this turn.
    pub ra_until: Turn,
    /// Science each side has put into the research agreement.
    pub ra_science: [i32; 2],
    /// Whether each side has an embassy with the other.
    pub embassy: [bool; 2],
    /// Until when each side denounces the other.
    pub denounced_until: [Turn; 2],
    /// Until when each side lets the other's units in (Python's `open_borders["a>b"]`).
    pub open_borders_until: [Turn; 2],
}

/// Why a pair was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PairError {
    /// Not two distinct players of the game.
    #[error("{0} and {1} are not two players of this game")]
    NotAPair(PlayerId, PlayerId),
}

// ---- Opinions ---------------------------------------------------------------------------------

/// Why one civilization thinks better or worse of another (the keys `add_opinion` is called
/// with, and the scenario's).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OpinionKey {
    Friendship,
    Denounced,
    DenouncedFriend,
    Betrayal,
    Warmonger,
    SharedEnemy,
    CapturedOurCities,
    LiberatedCity,
    ReturnedCivilian,
    UsedNukes,
    SpiedOnUs,
    AttackedProtectedMinor,
    AttackedAlliedMinor,
    BulliedProtectedMinor,
    DestroyedProtectedMinor,
    /// Set by a scenario.
    Scenario,
}

impl OpinionKey {
    /// How many keys there are.
    pub const COUNT: usize = 16;

    /// Every key, in storage order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Friendship,
        Self::Denounced,
        Self::DenouncedFriend,
        Self::Betrayal,
        Self::Warmonger,
        Self::SharedEnemy,
        Self::CapturedOurCities,
        Self::LiberatedCity,
        Self::ReturnedCivilian,
        Self::UsedNukes,
        Self::SpiedOnUs,
        Self::AttackedProtectedMinor,
        Self::AttackedAlliedMinor,
        Self::BulliedProtectedMinor,
        Self::DestroyedProtectedMinor,
        Self::Scenario,
    ];

    /// The key Python used: `captured_our_cities`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Friendship => "friendship",
            Self::Denounced => "denounced",
            Self::DenouncedFriend => "denounced_friend",
            Self::Betrayal => "betrayal",
            Self::Warmonger => "warmonger",
            Self::SharedEnemy => "shared_enemy",
            Self::CapturedOurCities => "captured_our_cities",
            Self::LiberatedCity => "liberated_city",
            Self::ReturnedCivilian => "returned_civilian",
            Self::UsedNukes => "used_nukes",
            Self::SpiedOnUs => "spied_on_us",
            Self::AttackedProtectedMinor => "attacked_protected_minor",
            Self::AttackedAlliedMinor => "attacked_allied_minor",
            Self::BulliedProtectedMinor => "bullied_protected_minor",
            Self::DestroyedProtectedMinor => "destroyed_protected_minor",
            Self::Scenario => "scenario",
        }
    }

    /// The key called `name`, exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }
}

/// The range an opinion is clamped to (`diplomacy.py:82`).
pub const OPINION_LIMIT: f64 = 100.0;

/// What each civilization thinks of each other, by reason.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpinionBook {
    entries: BTreeMap<(PlayerId, PlayerId), [f64; OpinionKey::COUNT]>,
}

impl OpinionBook {
    /// An empty book.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `amount` to what `holder` thinks of `about` for one reason, clamped to
    /// ±[`OPINION_LIMIT`] (`diplomacy.py:77-82`). A civilization has no opinion of itself.
    pub fn add(&mut self, holder: PlayerId, about: PlayerId, key: OpinionKey, amount: f64) {
        if holder == about {
            return;
        }
        let slot = &mut self.entries.entry((holder, about)).or_insert([0.0; OpinionKey::COUNT])
            [key as usize];
        *slot = (*slot + amount).clamp(-OPINION_LIMIT, OPINION_LIMIT);
    }

    /// Sets one reason's value outright, as a scenario does.
    pub fn set(&mut self, holder: PlayerId, about: PlayerId, key: OpinionKey, value: f64) {
        if holder == about {
            return;
        }
        self.entries.entry((holder, about)).or_insert([0.0; OpinionKey::COUNT])[key as usize] =
            value;
    }

    /// What `holder` thinks of `about` for one reason.
    #[must_use]
    pub fn get(&self, holder: PlayerId, about: PlayerId, key: OpinionKey) -> f64 {
        self.entries.get(&(holder, about)).map_or(0.0, |v| v[key as usize])
    }

    /// What `holder` thinks of `about`, all reasons summed in key order (`diplomacy.py:85-96`).
    #[must_use]
    pub fn total(&self, holder: PlayerId, about: PlayerId) -> f64 {
        self.entries.get(&(holder, about)).map_or(0.0, |v| v.iter().sum())
    }

    /// Every (holder, about) pair with an opinion, and its values by key.
    pub fn iter(&self) -> impl Iterator<Item = ((PlayerId, PlayerId), &[f64; OpinionKey::COUNT])> {
        self.entries.iter().map(|(&k, v)| (k, v))
    }

    /// Puts a pair's values back, as a save has them.
    pub fn insert(&mut self, holder: PlayerId, about: PlayerId, values: [f64; OpinionKey::COUNT]) {
        self.entries.insert((holder, about), values);
    }
}

// ---- Deals ------------------------------------------------------------------------------------

/// The kinds of deal item (`ITEM_TYPES`, `diplomacy.py:17-31`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DealItemKind {
    Gold,
    GoldPerTurn,
    Resource,
    OpenBorders,
    Embassy,
    PeaceTreaty,
    DeclarationOfFriendship,
    ResearchAgreement,
    DefensivePact,
    DeclareWar,
    City,
    ShareMap,
    Tech,
}

impl DealItemKind {
    /// Every kind, in Python's order.
    pub const ALL: [Self; 13] = [
        Self::Gold,
        Self::GoldPerTurn,
        Self::Resource,
        Self::OpenBorders,
        Self::Embassy,
        Self::PeaceTreaty,
        Self::DeclarationOfFriendship,
        Self::ResearchAgreement,
        Self::DefensivePact,
        Self::DeclareWar,
        Self::City,
        Self::ShareMap,
        Self::Tech,
    ];

    /// The item's `type`: `gold_per_turn`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Gold => "gold",
            Self::GoldPerTurn => "gold_per_turn",
            Self::Resource => "resource",
            Self::OpenBorders => "open_borders",
            Self::Embassy => "embassy",
            Self::PeaceTreaty => "peace_treaty",
            Self::DeclarationOfFriendship => "declaration_of_friendship",
            Self::ResearchAgreement => "research_agreement",
            Self::DefensivePact => "defensive_pact",
            Self::DeclareWar => "declare_war",
            Self::City => "city",
            Self::ShareMap => "share_map",
            Self::Tech => "tech",
        }
    }

    /// The kind whose `type` is `name`, exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }

    /// Whether both sides give it together (`MUTUAL`, `diplomacy.py:32`).
    #[must_use]
    pub const fn is_mutual(self) -> bool {
        matches!(
            self,
            Self::PeaceTreaty
                | Self::DeclarationOfFriendship
                | Self::ResearchAgreement
                | Self::DefensivePact
        )
    }

    /// The keys of its JSON besides `type`, in Python's order.
    #[must_use]
    pub const fn fields(self) -> &'static [&'static str] {
        match self {
            Self::Gold => &["amount"],
            Self::GoldPerTurn => &["amount", "turns"],
            Self::Resource => &["resource", "amount", "turns"],
            Self::OpenBorders => &["turns"],
            Self::DeclareWar => &["target"],
            Self::City => &["city_id"],
            Self::Tech => &["tech"],
            Self::Embassy
            | Self::PeaceTreaty
            | Self::DeclarationOfFriendship
            | Self::ResearchAgreement
            | Self::DefensivePact
            | Self::ShareMap => &[],
        }
    }
}

/// One thing a side gives in a deal (DESIGN.md 4.6). A save writes it as Python's dict:
/// `{"type": "resource", "resource": "Iron", "amount": 1, "turns": 30}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DealItem {
    /// A lump sum of gold.
    Gold {
        amount: i32,
    },
    /// Gold every turn.
    GoldPerTurn {
        amount: i32,
        turns: i32,
    },
    /// A strategic or luxury resource every turn.
    Resource {
        resource: ResourceId,
        amount: i32,
        turns: i32,
    },
    /// Let the other side's units in.
    OpenBorders {
        turns: i32,
    },
    /// Let the other side establish an embassy.
    Embassy,
    PeaceTreaty,
    DeclarationOfFriendship,
    ResearchAgreement,
    DefensivePact,
    /// The giver declares war on a third civilization.
    DeclareWar {
        target: PlayerId,
    },
    /// One of the giver's cities, not the capital.
    City {
        city_id: CityId,
    },
    /// The giver's explored map.
    ShareMap,
    /// A technology.
    Tech {
        tech: TechId,
    },
}

/// A deal item JSON that is not one, with what is wrong.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct DealItemError(pub String);

impl DealItem {
    /// Its kind.
    #[must_use]
    pub const fn kind(&self) -> DealItemKind {
        match self {
            Self::Gold { .. } => DealItemKind::Gold,
            Self::GoldPerTurn { .. } => DealItemKind::GoldPerTurn,
            Self::Resource { .. } => DealItemKind::Resource,
            Self::OpenBorders { .. } => DealItemKind::OpenBorders,
            Self::Embassy => DealItemKind::Embassy,
            Self::PeaceTreaty => DealItemKind::PeaceTreaty,
            Self::DeclarationOfFriendship => DealItemKind::DeclarationOfFriendship,
            Self::ResearchAgreement => DealItemKind::ResearchAgreement,
            Self::DefensivePact => DealItemKind::DefensivePact,
            Self::DeclareWar { .. } => DealItemKind::DeclareWar,
            Self::City { .. } => DealItemKind::City,
            Self::ShareMap => DealItemKind::ShareMap,
            Self::Tech { .. } => DealItemKind::Tech,
        }
    }

    /// The item as Python's dict: `{"type": "resource", "resource": "Iron", "amount": 1,
    /// "turns": 30}`, keys in Python's order, rule objects by name. `None` if an id is not in
    /// the ruleset.
    #[must_use]
    pub fn to_json(&self, rules: &Ruleset) -> Option<Value> {
        let mut m = Map::new();
        m.insert("type".to_owned(), Value::from(self.kind().name()));
        match *self {
            Self::Gold { amount } => {
                m.insert("amount".to_owned(), amount.into());
            }
            Self::GoldPerTurn { amount, turns } => {
                m.insert("amount".to_owned(), amount.into());
                m.insert("turns".to_owned(), turns.into());
            }
            Self::Resource { resource, amount, turns } => {
                m.insert("resource".to_owned(), rules.name(resource)?.into());
                m.insert("amount".to_owned(), amount.into());
                m.insert("turns".to_owned(), turns.into());
            }
            Self::OpenBorders { turns } => {
                m.insert("turns".to_owned(), turns.into());
            }
            Self::DeclareWar { target } => {
                m.insert("target".to_owned(), target.0.into());
            }
            Self::City { city_id } => {
                m.insert("city_id".to_owned(), city_id.get().into());
            }
            Self::Tech { tech } => {
                m.insert("tech".to_owned(), rules.name(tech)?.into());
            }
            Self::Embassy
            | Self::PeaceTreaty
            | Self::DeclarationOfFriendship
            | Self::ResearchAgreement
            | Self::DefensivePact
            | Self::ShareMap => {}
        }
        Some(Value::Object(m))
    }

    /// The item a Python dict describes, strictly: its `type` and exactly that kind's keys, whole
    /// numbers where Python stored ints, and rule objects by their exact names. This reads what
    /// the engine and saves wrote; the tools' lenient reading of what models send is
    /// `api::tools::normalize`'s.
    pub fn from_json(v: &Value, rules: &Ruleset) -> Result<Self, DealItemError> {
        let err = |m: String| Err(DealItemError(m));
        let Some(m) = v.as_object() else {
            return err(format!("a deal item must be an object, not {v}"));
        };
        let Some(kind) = m.get("type").and_then(Value::as_str).and_then(DealItemKind::from_name)
        else {
            return err(format!("a deal item needs a known \"type\": {v}"));
        };
        let fields = kind.fields();
        if let Some(k) = m.keys().find(|k| *k != "type" && !fields.contains(&k.as_str())) {
            return err(format!("a {} item has no key {k:?}", kind.name()));
        }
        let int = |key: &str| -> Result<i64, DealItemError> {
            m.get(key).and_then(Value::as_i64).ok_or_else(|| {
                DealItemError(format!("a {} item needs a whole number {key:?}", kind.name()))
            })
        };
        let i32_of = |key: &str| -> Result<i32, DealItemError> {
            i32::try_from(int(key)?).map_err(|_| {
                DealItemError(format!("{key:?} of a {} item is out of range", kind.name()))
            })
        };
        let name = |key: &str| -> Result<&str, DealItemError> {
            m.get(key).and_then(Value::as_str).ok_or_else(|| {
                DealItemError(format!("a {} item needs a name {key:?}", kind.name()))
            })
        };
        Ok(match kind {
            DealItemKind::Gold => Self::Gold { amount: i32_of("amount")? },
            DealItemKind::GoldPerTurn => {
                Self::GoldPerTurn { amount: i32_of("amount")?, turns: i32_of("turns")? }
            }
            DealItemKind::Resource => {
                let n = name("resource")?;
                let Some(resource) = rules.lookup::<ResourceId>(n) else {
                    return err(format!("unknown resource {n:?}"));
                };
                Self::Resource { resource, amount: i32_of("amount")?, turns: i32_of("turns")? }
            }
            DealItemKind::OpenBorders => Self::OpenBorders { turns: i32_of("turns")? },
            DealItemKind::Embassy => Self::Embassy,
            DealItemKind::PeaceTreaty => Self::PeaceTreaty,
            DealItemKind::DeclarationOfFriendship => Self::DeclarationOfFriendship,
            DealItemKind::ResearchAgreement => Self::ResearchAgreement,
            DealItemKind::DefensivePact => Self::DefensivePact,
            DealItemKind::DeclareWar => {
                let t = int("target")?;
                let Ok(target) = u8::try_from(t) else {
                    return err(format!("{t} is not a player id"));
                };
                Self::DeclareWar { target: PlayerId(target) }
            }
            DealItemKind::City => {
                let c = int("city_id")?;
                let Some(city_id) = u32::try_from(c).ok().and_then(CityId::new) else {
                    return err(format!("{c} is not a city id"));
                };
                Self::City { city_id }
            }
            DealItemKind::ShareMap => Self::ShareMap,
            DealItemKind::Tech => {
                let n = name("tech")?;
                let Some(tech) = rules.lookup::<TechId>(n) else {
                    return err(format!("unknown tech {n:?}"));
                };
                Self::Tech { tech }
            }
        })
    }
}

/// What one side gives.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Side {
    pub giver: PlayerId,
    pub items: Vec<DealItem>,
}

/// A proposal, or a concluded deal's terms: what each of the two sides gives. Python kept a dict
/// from `str(player)` to items, the proposer's side first.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Terms {
    pub sides: [Side; 2],
}

impl Terms {
    /// What `p` gives; empty if `p` is not a side.
    #[must_use]
    pub fn gives(&self, p: PlayerId) -> &[DealItem] {
        self.sides.iter().find(|s| s.giver == p).map_or(&[], |s| s.items.as_slice())
    }

    /// Whether either side gives an item of this kind.
    #[must_use]
    pub fn has(&self, kind: DealItemKind) -> bool {
        self.sides.iter().any(|s| s.items.iter().any(|i| i.kind() == kind))
    }

    /// The terms as Python's dict, sides in order. `None` if an id is not in the ruleset.
    #[must_use]
    pub fn to_json(&self, rules: &Ruleset) -> Option<Value> {
        let mut m = Map::new();
        for s in &self.sides {
            let items = s.items.iter().map(|i| i.to_json(rules)).collect::<Option<Vec<_>>>()?;
            m.insert(s.giver.0.to_string(), Value::Array(items));
        }
        Some(Value::Object(m))
    }

    /// The terms a Python dict describes: two distinct player ids, each with a list of items.
    pub fn from_json(v: &Value, rules: &Ruleset) -> Result<Self, DealItemError> {
        let m = v
            .as_object()
            .filter(|m| m.len() == 2)
            .ok_or_else(|| DealItemError(format!("terms must name two sides: {v}")))?;
        let mut sides = Vec::with_capacity(2);
        for (k, items) in m {
            let giver = k
                .parse::<u8>()
                .map(PlayerId)
                .map_err(|_| DealItemError(format!("{k:?} is not a player id")))?;
            let list = items
                .as_array()
                .ok_or_else(|| DealItemError(format!("side {k} must list its items")))?;
            let items = list
                .iter()
                .map(|i| DealItem::from_json(i, rules))
                .collect::<Result<Vec<_>, _>>()?;
            sides.push(Side { giver, items });
        }
        let [a, b]: [Side; 2] =
            sides.try_into().map_err(|_| DealItemError("terms must name two sides".to_owned()))?;
        if a.giver == b.giver {
            return Err(DealItemError(format!("terms name player {} twice", a.giver)));
        }
        Ok(Self { sides: [a, b] })
    }
}

/// A recurring part of a deal: gold or a resource every turn until a turn (`diplomacy.py:554-557`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ongoing {
    /// A `GoldPerTurn` or `Resource` item.
    pub item: DealItem,
    pub from: PlayerId,
    pub to: PlayerId,
    /// The last turn it pays.
    pub until: Turn,
}

/// A concluded deal (`diplomacy.py:530-609`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deal {
    pub id: DealId,
    pub turn: Turn,
    pub parties: [PlayerId; 2],
    pub terms: Terms,
    pub ongoing: Vec<Ongoing>,
    pub active: bool,
    /// The sentence describing it.
    pub summary: Box<str>,
}

// ---- Negotiations -----------------------------------------------------------------------------

/// Where a negotiation stands (`diplomacy.py:700`).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum NegStatus {
    Open,
    Accepted,
    Rejected,
    Expired,
    Cancelled,
}

impl NegStatus {
    /// Every status.
    pub const ALL: [Self; 5] =
        [Self::Open, Self::Accepted, Self::Rejected, Self::Expired, Self::Cancelled];

    /// The name: `cancelled`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }

    /// The status called `name`, exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// What an entry of a negotiation did.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum NegAction {
    Open,
    Reply,
    Counter,
    Accept,
    Reject,
    /// Closed from outside the conversation: timed out, or made moot.
    Close,
}

impl NegAction {
    /// Every action.
    pub const ALL: [Self; 6] =
        [Self::Open, Self::Reply, Self::Counter, Self::Accept, Self::Reject, Self::Close];

    /// The name: `counter`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Reply => "reply",
            Self::Counter => "counter",
            Self::Accept => "accept",
            Self::Reject => "reject",
            Self::Close => "close",
        }
    }

    /// The action called `name`, exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.name() == name)
    }
}

/// One entry of a negotiation's history (`diplomacy.py:712-722`). `exchanges`, which only the
/// archived bots read, is dropped.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NegEntry {
    /// Its number, from 1.
    pub seq: u16,
    /// Who made it; `None` when the game closed the negotiation.
    pub by: Option<PlayerId>,
    pub action: NegAction,
    pub message: Box<str>,
    pub proposal: Option<Terms>,
    pub turn: Turn,
    /// The game's note on it, such as why it closed.
    pub note: Option<Box<str>>,
}

/// A chat between two major civilizations with a deal on the table (`diplomacy.py:695-760`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Negotiation {
    pub id: NegotiationId,
    pub initiator: PlayerId,
    pub responder: PlayerId,
    pub turn: Turn,
    pub status: NegStatus,
    /// Whose move it is while it is open.
    pub awaiting: Option<PlayerId>,
    pub proposal: Option<Terms>,
    pub proposal_by: Option<PlayerId>,
    pub history: Vec<NegEntry>,
    /// The deal it concluded.
    pub deal: Option<DealId>,
}

// ---- Diplomacy --------------------------------------------------------------------------------

/// Every relation, with the war and contact masks, opinions, deals and negotiations.
#[derive(Clone, Debug, PartialEq)]
pub struct Diplomacy {
    relations: PairMatrix<Relation>,
    barbarians: PlayerSet,
    war_mask: PlayerVec<PlayerSet>,
    met_mask: PlayerVec<PlayerSet>,
    pub opinions: OpinionBook,
    pub deals: Vec<Deal>,
    pub negotiations: Vec<Negotiation>,
}

impl Diplomacy {
    /// No relations yet between `n` players, of whom `barbarians` are at war with everyone.
    #[must_use]
    pub fn new(n: u8, barbarians: PlayerSet) -> Self {
        Self::from_relations(PairMatrix::new(n), barbarians)
    }

    /// These relations, with the masks built.
    #[must_use]
    pub fn from_relations(relations: PairMatrix<Relation>, barbarians: PlayerSet) -> Self {
        let mut d = Self {
            relations,
            barbarians,
            war_mask: PlayerVec::new(),
            met_mask: PlayerVec::new(),
            opinions: OpinionBook::new(),
            deals: Vec::new(),
            negotiations: Vec::new(),
        };
        d.rebuild_masks(barbarians);
        d
    }

    /// The number of players.
    #[must_use]
    pub fn players(&self) -> u8 {
        self.relations.n()
    }

    /// The relations.
    #[must_use]
    pub fn relations(&self) -> &PairMatrix<Relation> {
        &self.relations
    }

    /// The barbarian players.
    #[must_use]
    pub fn barbarians(&self) -> PlayerSet {
        self.barbarians
    }

    /// Rebuilds the war and contact masks from the relations, with `barbarians` at war with
    /// everyone (`game.py:664-672`).
    pub fn rebuild_masks(&mut self, barbarians: PlayerSet) {
        let n = self.relations.n();
        self.barbarians = barbarians;
        self.war_mask = PlayerVec::from_elem(PlayerSet::EMPTY, usize::from(n));
        self.met_mask = PlayerVec::from_elem(PlayerSet::EMPTY, usize::from(n));
        for a in 0..n {
            for b in (a + 1)..n {
                self.sync(PlayerId(a), PlayerId(b));
            }
        }
    }

    /// Brings the masks of one pair in line with its relation.
    fn sync(&mut self, a: PlayerId, b: PlayerId) {
        let Some(r) = self.relations.get(a, b).copied() else { return };
        let war = r.war || self.barbarians.contains(a) || self.barbarians.contains(b);
        for (x, y) in [(a, b), (b, a)] {
            if let Some(m) = self.war_mask.get_mut(x) {
                if war {
                    m.insert(y);
                } else {
                    m.remove(y);
                }
            }
            if let Some(m) = self.met_mask.get_mut(x) {
                if r.met {
                    m.insert(y);
                } else {
                    m.remove(y);
                }
            }
        }
    }

    /// The relation between two players.
    #[must_use]
    pub fn relation(&self, a: PlayerId, b: PlayerId) -> Option<&Relation> {
        self.relations.get(a, b)
    }

    /// Whether two players are at war: one bit test. A player is never at war with itself; the
    /// barbarians always are with everyone else.
    #[must_use]
    #[inline]
    pub fn at_war(&self, a: PlayerId, b: PlayerId) -> bool {
        self.war_mask.get(a).is_some_and(|m| m.contains(b))
    }

    /// Whether two players have met: one bit test. Everyone has met themselves (`game.py:686-690`).
    #[must_use]
    #[inline]
    pub fn has_met(&self, a: PlayerId, b: PlayerId) -> bool {
        a == b || self.met_mask.get(a).is_some_and(|m| m.contains(b))
    }

    /// Everyone `p` is at war with.
    #[must_use]
    pub fn war_mask(&self, p: PlayerId) -> PlayerSet {
        self.war_mask.get(p).copied().unwrap_or_default()
    }

    /// Everyone `p` has met.
    #[must_use]
    pub fn met_mask(&self, p: PlayerId) -> PlayerSet {
        self.met_mask.get(p).copied().unwrap_or_default()
    }

    /// Edits the relation of `a` and `b`, then brings the masks in line. The changes say what
    /// moved: [`Change::Met`] if contact changed, [`Change::Diplo`] if anything else did, nothing
    /// if nothing did.
    pub fn update(
        &mut self,
        a: PlayerId,
        b: PlayerId,
        f: impl FnOnce(&mut Relation),
    ) -> Result<Changes, PairError> {
        let r = self.relations.get_mut(a, b).ok_or(PairError::NotAPair(a, b))?;
        let before = *r;
        f(r);
        let after = *r;
        self.sync(a, b);
        let mut out = Changes::new();
        if before.met != after.met {
            out.push(Change::Met { a, b });
        }
        let rest_before = Relation { met: after.met, ..before };
        if rest_before != after {
            out.push(Change::Diplo { a, b });
        }
        Ok(out)
    }

    /// Records that `a` and `b` have met; no change if they had.
    pub fn meet(&mut self, a: PlayerId, b: PlayerId) -> Result<Changes, PairError> {
        self.update(a, b, |r| r.met = true)
    }

    /// Checks that the masks agree with the relations.
    pub fn verify(&self) -> Result<(), String> {
        let n = self.relations.n();
        if self.war_mask.len() != usize::from(n) || self.met_mask.len() != usize::from(n) {
            return Err(format!(
                "the masks cover {} and {} players, the relations {n}",
                self.war_mask.len(),
                self.met_mask.len()
            ));
        }
        for (a, b, r) in self.relations.pairs() {
            let war = r.war || self.barbarians.contains(a) || self.barbarians.contains(b);
            if self.at_war(a, b) != war || self.at_war(b, a) != war {
                return Err(format!("war mask of {a} and {b} disagrees with their relation"));
            }
            if self.has_met(a, b) != r.met || self.has_met(b, a) != r.met {
                return Err(format!("contact mask of {a} and {b} disagrees with their relation"));
            }
        }
        for p in 0..n {
            let p = PlayerId(p);
            if self.at_war(p, p) {
                return Err(format!("{p} is at war with itself"));
            }
        }
        Ok(())
    }

    /// A deal by id.
    #[must_use]
    pub fn deal(&self, id: DealId) -> Option<&Deal> {
        self.deals.iter().find(|d| d.id == id)
    }

    /// A negotiation by id.
    #[must_use]
    pub fn negotiation(&self, id: NegotiationId) -> Option<&Negotiation> {
        self.negotiations.iter().find(|n| n.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opinions_clamp_and_sum_by_holder() {
        let mut book = OpinionBook::new();
        let (a, b) = (PlayerId(0), PlayerId(1));
        book.add(a, b, OpinionKey::Warmonger, -70.0);
        book.add(a, b, OpinionKey::Warmonger, -70.0);
        book.add(a, b, OpinionKey::Friendship, 35.0);
        book.add(a, a, OpinionKey::Friendship, 35.0);
        assert_eq!(book.get(a, b, OpinionKey::Warmonger).to_bits(), (-100.0f64).to_bits());
        assert_eq!(book.total(a, b).to_bits(), (-65.0f64).to_bits());
        assert_eq!(book.total(b, a).to_bits(), 0.0f64.to_bits());
        assert_eq!(book.iter().count(), 1);
        for k in OpinionKey::ALL {
            assert_eq!(OpinionKey::from_name(k.name()), Some(k));
        }
    }

    #[test]
    fn updates_report_contact_and_the_rest_apart() -> Result<(), PairError> {
        let mut d = Diplomacy::new(4, PlayerSet::single(PlayerId(3)));
        let (a, b) = (PlayerId(0), PlayerId(2));
        assert!(d.at_war(PlayerId(3), a) && d.at_war(a, PlayerId(3)));
        assert!(!d.at_war(a, a) && d.has_met(a, a));
        assert_eq!(d.meet(a, b)?.as_slice(), &[Change::Met { a, b }]);
        assert!(d.meet(a, b)?.is_empty());
        assert_eq!(d.update(b, a, |r| r.war = true)?.as_slice(), &[Change::Diplo { a: b, b: a }]);
        assert!(d.at_war(a, b) && d.at_war(b, a) && d.has_met(b, a));
        assert_eq!(d.update(a, a, |_| {}), Err(PairError::NotAPair(a, a)));
        assert_eq!(d.update(a, PlayerId(4), |_| {}), Err(PairError::NotAPair(a, PlayerId(4))));
        assert_eq!(d.verify(), Ok(()));
        Ok(())
    }

    #[test]
    fn sides_follow_player_order() {
        assert_eq!(side(PlayerId(1), PlayerId(5)), 0);
        assert_eq!(side(PlayerId(5), PlayerId(1)), 1);
    }

    #[test]
    fn statuses_actions_and_kinds_by_name() {
        for s in NegStatus::ALL {
            assert_eq!(NegStatus::from_name(s.name()), Some(s));
        }
        for a in NegAction::ALL {
            assert_eq!(NegAction::from_name(a.name()), Some(a));
        }
        for k in DealItemKind::ALL {
            assert_eq!(DealItemKind::from_name(k.name()), Some(k));
        }
        let mutual: Vec<_> = DealItemKind::ALL
            .into_iter()
            .filter(|k| k.is_mutual())
            .map(DealItemKind::name)
            .collect();
        assert_eq!(
            mutual,
            ["peace_treaty", "declaration_of_friendship", "research_agreement", "defensive_pact"]
        );
    }
}
