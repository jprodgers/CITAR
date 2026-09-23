//! The compiled uniques of a ruleset (DESIGN.md 5.5): the hot records, their cold metadata, the
//! conditionals, the interned texts and filter handles, and each source object's uniques split
//! by what the engine does with them.
//!
//! Python kept every unique as a parsed object on its source (`Unique` and `UniqueMap`,
//! `uniques.py:105-209`) and looked its placeholder up in dicts at each use. Here a unique is a
//! 24-byte [`Unique`] in one table, found by [`UniqueId`]; everything the hot paths do not read
//! lives beside it in [`UniqueMeta`].

use core::fmt;
use core::ops::Range;

use bitflags::bitflags;

use super::generated::{CondData, TriggerCond, UniqueData, UniqueType};
use super::params::Param;
use crate::base::ids::{
    AbilityKey, BaseUnitId, BeliefId, BuildingId, CityFilterId, CityStateTypeId, CivFilterId,
    CombatantFilterId, CondId, EraId, IdVec, ImprovementId, NationId, ObjectFilterId, PolicyId,
    PromotionId, ResourceId, RuinId, SetRef, SpecialistId, StatsId, TagId, TechId, TerrainId,
    TextId, TileFilterId, UniqueId, UnitFilterId, UnitTypeId,
};
use crate::base::sets::{BitSet, TagSet};
use crate::base::stats::Stats;

/// What the engine does with a unique of some type (`unique_supported.toml`'s `role`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// A standing modifier, read through the unique indexes with its conditionals.
    Effect,
    /// A standing yes or no, read by presence.
    Flag,
    /// Gates the object that carries it: buildable, researchable, adoptable.
    Requirement,
    /// Happens once: when its source is gained, when its trigger fires, or as a unit action.
    OneTime,
    /// A unit action, which action modifiers limit.
    Action,
    /// A weight for the AI's choices.
    Ai,
    /// Read by map generation only.
    Mapgen,
    /// Compiled and checked, then read by nothing.
    Inert,
    /// A conditional modifier.
    Cond,
    /// A trigger modifier.
    Trigger,
    /// A unit action modifier.
    ActionMod,
    /// A modifier that changes the unique itself: game speed, a timer.
    Meta,
    /// A text UnCiv does not know that filters name ([`UniqueData::Tag`]).
    Tag,
}

/// The hot part of a compiled unique: what it does, where its conditionals are, and what they
/// read. 24 bytes (DESIGN.md 5.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Unique {
    pub data: UniqueData,
    pub conds: CondSpan,
    /// The classes of the conditionals ([`CondDeps`], the low 24 bits) and the unique's
    /// [`UFlags`] (the top 8). Read through [`Unique::deps`] and [`Unique::flags`].
    packed: u32,
}

const _: () = assert!(core::mem::size_of::<UniqueData>() == 16);
const _: () = assert!(core::mem::size_of::<Unique>() <= 32, "DESIGN.md 5.5");
const _: () = assert!(core::mem::size_of::<Unique>() == 24);

impl Unique {
    pub(crate) fn new(data: UniqueData, conds: CondSpan, deps: CondDeps, flags: UFlags) -> Self {
        Self { data, conds, packed: deps.bits() | (u32::from(flags.bits()) << 24) }
    }

    /// What its conditionals read: empty when it has none, so evaluation can be skipped.
    #[must_use]
    #[inline]
    pub const fn deps(&self) -> CondDeps {
        CondDeps::from_bits_truncate(self.packed & 0x00ff_ffff)
    }

    /// The unique's own flags.
    #[must_use]
    #[inline]
    pub const fn flags(&self) -> UFlags {
        UFlags::from_bits_truncate((self.packed >> 24) as u8)
    }
}

/// Where a unique's conditionals are in [`UniqueTable::conds`]: a run of `len` from `start`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CondSpan {
    pub start: u16,
    pub len: u16,
}

impl CondSpan {
    /// The conditionals' ids: the same run as [`UniqueTable::conds`] reads.
    ///
    /// # Panics
    /// If the span ends past `u16::MAX`, which no compiled span does: the compiler refuses more
    /// conditionals than that.
    pub fn ids(self) -> impl Iterator<Item = CondId> {
        (self.start..self.start + self.len).map(CondId)
    }

    /// Whether the unique has no conditionals.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    fn range(self) -> Range<usize> {
        usize::from(self.start)..usize::from(self.start) + usize::from(self.len)
    }
}

bitflags! {
    /// What a conditional reads, by class (DESIGN.md 5.8): a memo that evaluates it validates
    /// against the revisions these map to, and a unique whose conditionals read nothing skips
    /// evaluation. 24 bits, so a [`Unique`] keeps its [`UFlags`] in the top byte of the same
    /// word.
    ///
    /// Package 1a-07 assigns each conditional its classes. Until then every conditional is
    /// compiled as reading [`CondDeps::all`]: always correct, never fast.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct CondDeps: u32 {
        const TURN = 1 << 0;
        const HAPPINESS_SEEN = 1 << 1;
        const STOCKS = 1 << 2;
        const RESOURCES = 1 << 3;
        const GOLDEN_AGE = 1 << 4;
        const WAR = 1 << 5;
        const ERA = 1 << 6;
        const TECHS = 1 << 7;
        const POLICIES = 1 << 8;
        const RESEARCH_QUEUE = 1 << 9;
        const RELIGION_STATE = 1 << 10;
        const CIV_BUILDINGS = 1 << 11;
        const GLOBAL_BUILDINGS = 1 << 12;
        const GLOBAL_POLICIES = 1 << 13;
        const CITY_COUNT = 1 << 14;
        const UNIT_SET = 1 << 15;
        /// The seat's controller and handicap: the Human and AI player filters.
        const SEAT = 1 << 16;
        const CONFIG = 1 << 17;
        const CHANCE = 1 << 18;
        /// The city in context.
        const CITY = 1 << 19;
        /// The unit in context.
        const UNIT = 1 << 20;
        /// The tile in context.
        const TILE = 1 << 21;
        /// The fight in context.
        const COMBAT = 1 << 22;
    }
}

bitflags! {
    /// A compiled unique's own flags, kept in the top byte of its [`CondDeps`] word.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct UFlags: u8 {
        /// Applies only in its own city: a city filter or conditional `in this city`
        /// (`uniques.py:130`).
        const LOCAL = 1 << 0;
        /// `<(modified by game speed)>`.
        const SPEED = 1 << 1;
        /// `<for [n] turns>`: granted for a while when it happens, never standing
        /// (`uniques.py:131, 193-199`).
        const TIMED = 1 << 2;
        /// Carries a trigger: `<upon ...>`.
        const TRIGGERED = 1 << 3;
        /// Carries unit action modifiers.
        const ACTION = 1 << 4;
        /// The variant of a timed unique without its timer, which a civilization holds while
        /// the timer runs.
        const TEMPORARY = 1 << 5;
    }
}

/// A compiled conditional.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cond {
    pub data: CondData,
    pub deps: CondDeps,
    /// The conditional's text, as written between `<` and `>`.
    pub text: TextId,
}

/// A unit action's limits, folded from its action modifiers (`units.py:401-473`). Each field is
/// one modifier, so the unique's text can be told from them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ActionMods {
    /// `<by consuming this unit>`.
    pub consume: bool,
    /// `<after which this unit is consumed>`: when the last use is spent.
    pub consumed_after: bool,
    /// `<once>`.
    pub once: bool,
    /// `<[n] times>`.
    pub times: Option<u16>,
    /// `<[n] additional time(s)>`: more uses of the action of the same type.
    pub extra_times: Option<u16>,
    /// `<for [n] movement>`: what a use costs, instead of 1.
    pub movement: Option<i32>,
}

impl ActionMods {
    /// Whether the unique carries no action modifier.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// How many uses the modifiers give, if they limit them: `<once>` is one.
    #[must_use]
    pub fn uses(&self) -> Option<u16> {
        if self.once { Some(1) } else { self.times }
    }
}

/// Which ruleset object a unique came from. The order of the variants is Python's `civ_umaps`
/// order (`economy.py:91-129`), then the objects outside it, and a [`UniqueId`] follows it: so
/// sorting uniques by id is canonical (DESIGN.md 5.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    Nation(NationId),
    Building(BuildingId),
    /// A policy branch or a policy.
    Policy(PolicyId),
    Tech(TechId),
    /// The variant of the timed unique `.0` without its timer.
    Temporary(UniqueId),
    Era(EraId),
    /// What a city-state of this type gives its friends.
    CityStateFriend(CityStateTypeId),
    /// What a city-state of this type gives its ally.
    CityStateAlly(CityStateTypeId),
    /// The type's own uniques.
    CityStateType(CityStateTypeId),
    Belief(BeliefId),
    Resource(ResourceId),
    /// `global_uniques.json`.
    Global,
    Terrain(TerrainId),
    Improvement(ImprovementId),
    UnitType(UnitTypeId),
    /// A unit of `units.json`, its own uniques only: its unit type's stay on the unit type, whose
    /// tags the unit's [`SourceUniques`] also carry.
    Unit(BaseUnitId),
    Promotion(PromotionId),
    Ruins(RuinId),
}

impl Source {
    /// The kind of source, as [`UniqueMeta::key`] hashes it.
    #[must_use]
    pub const fn kind_name(self) -> &'static str {
        match self {
            Self::Nation(_) => "Nation",
            Self::Building(_) => "Building",
            Self::Policy(_) => "Policy",
            Self::Tech(_) => "Tech",
            Self::Temporary(_) => "Temporary",
            Self::Era(_) => "Era",
            Self::CityStateFriend(_) => "CityStateFriend",
            Self::CityStateAlly(_) => "CityStateAlly",
            Self::CityStateType(_) => "CityStateType",
            Self::Belief(_) => "Belief",
            Self::Resource(_) => "Resource",
            Self::Global => "Global",
            Self::Terrain(_) => "Terrain",
            Self::Improvement(_) => "Improvement",
            Self::UnitType(_) => "UnitType",
            Self::Unit(_) => "Unit",
            Self::Promotion(_) => "Promotion",
            Self::Ruins(_) => "Ruins",
        }
    }
}

/// The cold part of a compiled unique: what only compiling, reporting, triggers and actions read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UniqueMeta {
    /// Its type; `None` for a tag.
    pub ty: Option<UniqueType>,
    pub role: Role,
    pub source: Source,
    /// Its text, as the ruleset wrote it.
    pub text: TextId,
    /// FNV-1a-64 of its source's kind and name, its occurrence and its text: the key of its
    /// random draws (DESIGN.md 7.2) and its identity in saves. The same text on two sources has
    /// two keys, and editing one unique moves no other unique's key.
    pub key: u64,
    /// How many identical texts come before it on the same source.
    pub occurrence: u16,
    /// `<for [n] turns>`: how long it lasts once granted.
    pub timed: Option<u16>,
    /// For a timed unique, the variant without the timer that is granted.
    pub temp_variant: Option<UniqueId>,
    /// When it fires, for a unique with a trigger.
    pub trigger: Option<TriggerCond>,
    /// Its action modifiers.
    pub actions: ActionMods,
    /// The ability a limited action counts its uses under: Python's `ph|params` key
    /// (`units.py:417`).
    pub ability: Option<AbilityKey>,
    /// The tag it gives its source, when a filter names its text (DESIGN.md 5.6).
    pub tag: Option<TagId>,
}

/// One source object's uniques, split by what the engine does with them (DESIGN.md 5.5).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceUniques {
    /// Every unique of the source, in the order the ruleset lists them.
    pub all: Range<u16>,
    /// Standing effects, flags and tags that hold wherever the source counts: what a
    /// civilization's (or a unit's, or a religion's followers') unique index holds. A `LOCAL`
    /// unique of any source but a building or a resource is here too, with its
    /// [`UFlags::LOCAL`] bit: there `in this city` means the city in context, as it did in
    /// Python (`uniques.py:668`), not the source's own city.
    pub civ: Box<[UniqueId]>,
    /// A building's or a resource's standing effects, flags and tags that hold only in the
    /// source's own city: the ones marked [`UFlags::LOCAL`]. Python split buildings' uniques so
    /// (`economy.py:108`, `cities.py:59`); a resource's is the Marble decision (DESIGN.md 5.12).
    /// Empty for every other source.
    pub local: Box<[UniqueId]>,
    /// What happens once when the source is gained: one-time effects without a trigger, timed
    /// uniques, and standing effects that also happen on gain.
    pub on_gain: Box<[UniqueId]>,
    /// What happens when its trigger fires.
    pub triggered: Box<[UniqueId]>,
    /// Unit actions.
    pub actions: Box<[UniqueId]>,
    /// Weights for the AI.
    pub ai: Box<[UniqueId]>,
    /// The tags the source carries unconditionally. A base unit's include its unit type's, as
    /// Python's unit map held its type's uniques (`rules.py:116-118`); the uniques themselves stay
    /// on the unit type.
    pub tags: TagSet,
    /// The tags the source carries under conditionals; a base unit's include its unit type's.
    pub cond_tags: TagSet,
}

impl SourceUniques {
    /// Every unique of the source, in order.
    pub fn ids(&self) -> impl Iterator<Item = UniqueId> {
        self.all.clone().map(UniqueId)
    }

    /// Whether the source has no uniques.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
    }
}

/// The kind of object a static filter selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StaticDomain {
    BaseUnit,
    Building,
    Improvement,
    Resource,
    Tech,
    Era,
}

/// A static filter ([`SetRef`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticFilter {
    pub domain: StaticDomain,
    /// The filter as written.
    pub text: TextId,
    /// The objects it selects, by index, when the compiler decided them: the relevant-promotion
    /// fixup (`units.py:153`). Package 1a-06 evaluates the rest.
    pub members: Option<BitSet>,
}

/// A parameter that may name objects of several kinds ([`ObjectFilterId`]), compiled once for
/// each kind its parameter kind allows. Package 1a-06 drops the kinds a text cannot match.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObjectFilter {
    /// The filter as written.
    pub text: TextId,
    pub tiles: Option<TileFilterId>,
    pub buildings: Option<SetRef>,
    pub improvements: Option<SetRef>,
    /// The specialist it names, if its text is a specialist's name.
    pub specialist: Option<SpecialistId>,
}

/// Every compiled unique of a ruleset, with what they refer to.
///
/// Filters are handles to their text for now; package 1a-06 compiles them (DESIGN.md 5.7).
#[derive(Clone, Default, PartialEq)]
pub struct UniqueTable {
    pub(crate) uniques: IdVec<UniqueId, Unique>,
    pub(crate) meta: IdVec<UniqueId, UniqueMeta>,
    pub(crate) conds: IdVec<CondId, Cond>,
    pub(crate) texts: IdVec<TextId, Box<str>>,
    pub(crate) stats: IdVec<StatsId, Stats>,
    pub(crate) sets: IdVec<SetRef, StaticFilter>,
    pub(crate) objects: IdVec<ObjectFilterId, ObjectFilter>,
    pub(crate) unit_filters: IdVec<UnitFilterId, TextId>,
    pub(crate) tile_filters: IdVec<TileFilterId, TextId>,
    pub(crate) city_filters: IdVec<CityFilterId, TextId>,
    pub(crate) civ_filters: IdVec<CivFilterId, TextId>,
    pub(crate) combatant_filters: IdVec<CombatantFilterId, TextId>,
    pub(crate) tags: IdVec<TagId, TextId>,
    pub(crate) abilities: IdVec<AbilityKey, TextId>,
}

impl UniqueTable {
    /// How many uniques there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.uniques.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.uniques.is_empty()
    }

    /// Every unique with its id, in id order.
    pub fn iter(&self) -> impl Iterator<Item = (UniqueId, &Unique)> {
        self.uniques.iter()
    }

    /// The unique `id`.
    ///
    /// # Panics
    /// If `id` is not this ruleset's, which only a bug can cause.
    #[must_use]
    #[inline]
    pub fn get(&self, id: UniqueId) -> &Unique {
        &self.uniques[id]
    }

    /// The cold part of the unique `id`.
    #[must_use]
    pub fn meta(&self, id: UniqueId) -> &UniqueMeta {
        &self.meta[id]
    }

    /// The unique's conditionals, in the order the text writes them.
    #[must_use]
    pub fn conds(&self, u: &Unique) -> &[Cond] {
        &self.conds.as_slice()[u.conds.range()]
    }

    /// The conditional `id`.
    #[must_use]
    pub fn cond(&self, id: CondId) -> &Cond {
        &self.conds[id]
    }

    /// Every conditional, in id order.
    #[must_use]
    pub fn all_conds(&self) -> &IdVec<CondId, Cond> {
        &self.conds
    }

    /// An interned text.
    #[must_use]
    pub fn text(&self, id: TextId) -> &str {
        &self.texts[id]
    }

    /// The text of the unique `id`.
    #[must_use]
    pub fn text_of(&self, id: UniqueId) -> &str {
        self.text(self.meta[id].text)
    }

    /// An interned `Stats`.
    #[must_use]
    pub fn stats(&self, id: StatsId) -> &Stats {
        &self.stats[id]
    }

    /// Every interned `Stats`.
    #[must_use]
    pub fn all_stats(&self) -> &IdVec<StatsId, Stats> {
        &self.stats
    }

    /// A static filter.
    #[must_use]
    pub fn set(&self, id: SetRef) -> &StaticFilter {
        &self.sets[id]
    }

    /// Every static filter.
    #[must_use]
    pub fn sets(&self) -> &IdVec<SetRef, StaticFilter> {
        &self.sets
    }

    /// A parameter compiled for several kinds of object.
    #[must_use]
    pub fn object(&self, id: ObjectFilterId) -> &ObjectFilter {
        &self.objects[id]
    }

    /// Every parameter compiled for several kinds of object.
    #[must_use]
    pub fn objects(&self) -> &IdVec<ObjectFilterId, ObjectFilter> {
        &self.objects
    }

    /// The text of a unit filter.
    #[must_use]
    pub fn unit_filter(&self, id: UnitFilterId) -> &str {
        self.text(self.unit_filters[id])
    }

    /// The text of a tile filter.
    #[must_use]
    pub fn tile_filter(&self, id: TileFilterId) -> &str {
        self.text(self.tile_filters[id])
    }

    /// The text of a city filter.
    #[must_use]
    pub fn city_filter(&self, id: CityFilterId) -> &str {
        self.text(self.city_filters[id])
    }

    /// The text of a civilization filter.
    #[must_use]
    pub fn civ_filter(&self, id: CivFilterId) -> &str {
        self.text(self.civ_filters[id])
    }

    /// The text of a combatant filter.
    #[must_use]
    pub fn combatant_filter(&self, id: CombatantFilterId) -> &str {
        self.text(self.combatant_filters[id])
    }

    /// The text a tag stands for.
    #[must_use]
    pub fn tag(&self, id: TagId) -> &str {
        self.text(self.tags[id])
    }

    /// How many tags there are.
    #[must_use]
    pub fn tag_count(&self) -> usize {
        self.tags.len()
    }

    /// The tag a filter term names, if it is one.
    #[must_use]
    pub fn tag_named(&self, text: &str) -> Option<TagId> {
        self.tags.iter().find(|&(_, &t)| self.text(t) == text).map(|(id, _)| id)
    }

    /// The key a limited action's uses are counted under, as Python wrote it: `ph|params`.
    #[must_use]
    pub fn ability(&self, id: AbilityKey) -> &str {
        self.text(self.abilities[id])
    }

    /// How many abilities there are.
    #[must_use]
    pub fn ability_count(&self) -> usize {
        self.abilities.len()
    }

    /// The modifiers of the unique `id` as its text wrote them, each as its type and compiled
    /// parameters: the conditionals in order, then the trigger, the action modifiers and the meta
    /// modifiers that the compiler folded (a text's order among those is not kept). For reports
    /// and the reference checks.
    #[must_use]
    pub fn modifiers(&self, id: UniqueId) -> Vec<(UniqueType, Vec<Param>)> {
        let u = self.get(id);
        let m = self.meta(id);
        let mut out: Vec<(UniqueType, Vec<Param>)> =
            self.conds(u).iter().map(|c| (c.data.ty(), c.data.params())).collect();
        if let Some(t) = m.trigger {
            out.push((t.ty(), t.params()));
        }
        let a = m.actions;
        let count = |n: u16| vec![Param::Int(i32::from(n))];
        if a.consume {
            out.push((UniqueType::UnitActionConsumeUnit, Vec::new()));
        }
        if a.consumed_after {
            out.push((UniqueType::UnitActionAfterWhichConsumed, Vec::new()));
        }
        if a.once {
            out.push((UniqueType::UnitActionOnce, Vec::new()));
        }
        if let Some(n) = a.times {
            out.push((UniqueType::UnitActionLimitedTimes, count(n)));
        }
        if let Some(n) = a.extra_times {
            out.push((UniqueType::UnitActionExtraLimitedTimes, count(n)));
        }
        if let Some(n) = a.movement {
            out.push((UniqueType::UnitActionMovementCost, vec![Param::Int(n)]));
        }
        if u.flags().contains(UFlags::SPEED) {
            out.push((UniqueType::ModifiedByGameSpeed, Vec::new()));
        }
        if let Some(n) = m.timed {
            out.push((UniqueType::ConditionalTimedUnique, count(n)));
        }
        out
    }
}

/// Short: the table runs to thousands of entries.
impl fmt::Debug for UniqueTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UniqueTable")
            .field("uniques", &self.uniques.len())
            .field("conds", &self.conds.len())
            .field("texts", &self.texts.len())
            .finish_non_exhaustive()
    }
}

/// FNV-1a-64 over parts, each prefixed with its length so no two part lists share an input.
#[must_use]
pub fn fnv1a64(parts: &[&[u8]]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(PRIME);
        }
    };
    for part in parts {
        eat(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        eat(part);
    }
    h
}

/// A unique's key (DESIGN.md 7.2): its source's kind and name, which of the source's identical
/// texts it is, and its text.
#[must_use]
pub fn unique_key(kind: &str, source_name: &str, occurrence: u16, text: &str) -> u64 {
    fnv1a64(&[kind.as_bytes(), source_name.as_bytes(), &occurrence.to_le_bytes(), text.as_bytes()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_and_deps_share_a_word_without_touching() {
        let u = Unique::new(
            UniqueData::Tag(TagId(3)),
            CondSpan { start: 7, len: 2 },
            CondDeps::all(),
            UFlags::LOCAL | UFlags::TEMPORARY,
        );
        assert_eq!(u.deps(), CondDeps::all());
        assert_eq!(u.flags(), UFlags::LOCAL | UFlags::TEMPORARY);
        let v = Unique::new(u.data, u.conds, CondDeps::empty(), UFlags::all());
        assert_eq!(v.deps(), CondDeps::empty());
        assert_eq!(v.flags(), UFlags::all());
        assert_eq!(u.conds.ids().map(|c| c.0).collect::<Vec<_>>(), [7, 8]);
        assert!(CondDeps::all().bits() < 1 << 24, "the classes fit 24 bits");
    }

    #[test]
    fn fnv_matches_its_reference_values() {
        // FNV-1a-64 of the empty input is its offset basis; of "a", the published vector.
        let raw = |bytes: &[u8]| {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for &b in bytes {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
            h
        };
        assert_eq!(raw(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(raw(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(fnv1a64(&[b"ab", b"c"]), fnv1a64(&[b"a", b"bc"]), "length-prefixed parts");
        assert_ne!(
            unique_key("Building", "Temple", 0, "x"),
            unique_key("Promotion", "Temple", 0, "x")
        );
        assert_ne!(
            unique_key("Building", "Temple", 0, "x"),
            unique_key("Building", "Temple", 1, "x")
        );
    }

    #[test]
    fn sources_sort_in_civ_umaps_order() {
        let mut s = [
            Source::Global,
            Source::Tech(TechId(0)),
            Source::Nation(NationId(5)),
            Source::Temporary(UniqueId(0)),
            Source::Building(BuildingId(1)),
        ];
        s.sort();
        assert_eq!(
            s,
            [
                Source::Nation(NationId(5)),
                Source::Building(BuildingId(1)),
                Source::Tech(TechId(0)),
                Source::Temporary(UniqueId(0)),
                Source::Global,
            ]
        );
    }
}
