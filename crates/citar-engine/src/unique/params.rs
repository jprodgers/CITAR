//! Compiling a unique's parameters by kind (DESIGN.md 5.5, step 3).
//!
//! Python kept parameters as text and read them at each use: `num()` turned anything it could not
//! parse into 0 (`uniques.py:93-102`), `parse_stats` into no stats (`uniques.py:79-90`), and names
//! were compared with the tables' keys wherever a rule happened to look. Here each parameter is
//! compiled once, by the kind its type's signature gives it, and anything that does not compile
//! stops the ruleset from loading:
//! - amounts are whole numbers within ±1,000,000 (and at least 1 or 0 where the kind says so);
//! - fractions are read with `str::parse`, correctly rounded, and interned as a [`FracId`];
//! - stats are interned as a [`StatsId`], and a stat name UnCiv does not have is an error;
//! - names become ids, looked up exactly;
//! - filters become handles to their text, interned per kind (`unique::filter` compiles them);
//! - small vocabularies become enums, and countables a [`Countable`].

use core::fmt;
use core::num::NonZeroU32;

use super::countable::{self, Countable, CountableText};
use super::generated::{ParamKind, UniqueType};
use super::table::{ObjectFilter, StaticDomain, StaticFilter, UniqueTable};
use crate::base::collections::{DetMap, DetSet};
use crate::base::ids::{
    AbilityKey, BaseUnitId, BeliefId, BuildingId, CityFilterId, CivFilterId, CombatantFilterId,
    DifficultyId, EraId, FeatureId, FracId, Id, IdVec, ObjectFilterId, PolicyId, PromotionId,
    ResourceId, SetRef, SpecialistId, StatsId, TagId, TechId, TerrainId, TextId, TileFilterId,
    UnitFilterId, VictoryId,
};
use crate::base::sets::BitSet;
use crate::base::stats::{Stat, Stats};
use crate::rules::Ruleset;
use crate::rules::defs::TerrainType;

/// The largest amount a parameter may give, either way. Nothing in the ruleset comes near it; it
/// keeps sums of amounts far from `i32`'s edges.
pub const AMOUNT_LIMIT: i32 = 1_000_000;

// ---- Small vocabularies -----------------------------------------------------------------------

/// `[stat/resource]`: a stat, or a resource (`uniques.py:811-824` looked resources up first).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatOrResource {
    Stat(Stat),
    Resource(ResourceId),
}

/// `[policy/belief]`: a policy branch, a policy or a belief.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PolicyOrBelief {
    Policy(PolicyId),
    Belief(BeliefId),
}

/// `[populationFilter]`: which of a city's citizens count (`uniques.py:720-732`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PopulationFilter {
    /// `Population`: all of them.
    Population,
    /// `Specialists`.
    Specialists,
    /// `Followers of this Religion`: the followers of the city's majority religion.
    FollowersOfThisReligion,
    /// `Followers of the Majority Religion`: the same, as UnCiv also writes it.
    FollowersOfTheMajorityReligion,
    /// `Unemployed`: citizens working no tile and no specialist slot.
    Unemployed,
    /// A specialist's name: the specialists of that kind.
    Specialist(SpecialistId),
}

/// `[costOrStrength]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CostOrStrength {
    Cost,
    Strength,
}

/// `[foundingOrEnhancing]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FoundingOrEnhancing {
    Founding,
    Enhancing,
}

/// `[terrainQuality]`: how start placement values a terrain, in UnCiv's words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TerrainQuality {
    Undesirable,
    Food,
    Production,
    Desirable,
}

/// `[regionType]`: a kind of start region, named after a terrain, or `Hybrid`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegionType {
    Hybrid,
    Terrain(TerrainId),
}

/// `[unitTriggerTarget]`: the unit a one-time effect acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnitTriggerTarget {
    /// `This Unit`: the unit whose unique it is.
    ThisUnit,
}

/// `[positiveAmount/'all']`: a count, or all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CountOrAll(Option<NonZeroU32>);

impl CountOrAll {
    /// `All`.
    pub const ALL: Self = Self(None);

    /// A count of at least 1.
    #[must_use]
    pub fn count(n: u32) -> Option<Self> {
        NonZeroU32::new(n).map(|n| Self(Some(n)))
    }

    /// The count, or `None` for all.
    #[must_use]
    pub fn get(self) -> Option<u32> {
        self.0.map(NonZeroU32::get)
    }
}

// ---- Compiled parameters ----------------------------------------------------------------------

/// One compiled parameter, whatever its kind: what a payload's fields hold, in one type, for
/// code that lists them ([`super::UniqueData::params`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Param {
    Int(i32),
    Frac(FracId),
    Stats(StatsId),
    Stat(Stat),
    StatOrResource(StatOrResource),
    CityFilter(CityFilterId),
    UnitFilter(UnitFilterId),
    CivFilter(CivFilterId),
    CombatantFilter(CombatantFilterId),
    TileFilter(TileFilterId),
    Set(SetRef),
    Object(ObjectFilterId),
    Building(BuildingId),
    BaseUnit(BaseUnitId),
    Promotion(PromotionId),
    Resource(ResourceId),
    Tech(TechId),
    Era(EraId),
    Difficulty(DifficultyId),
    Victory(VictoryId),
    Terrain(TerrainId),
    Feature(FeatureId),
    PolicyOrBelief(PolicyOrBelief),
    Population(PopulationFilter),
    CostOrStrength(CostOrStrength),
    FoundingOrEnhancing(FoundingOrEnhancing),
    TerrainQuality(TerrainQuality),
    Region(RegionType),
    Target(UnitTriggerTarget),
    Countable(Countable),
    CountOrAll(CountOrAll),
    Text(TextId),
}

/// A field type of a payload, taken back out of a [`Param`].
pub(crate) trait FromParam: Sized {
    fn from_param(p: Param) -> Option<Self>;
}

macro_rules! param_field {
    ($($ty:ty => $variant:ident,)*) => {$(
        impl From<$ty> for Param {
            fn from(x: $ty) -> Self {
                Self::$variant(x)
            }
        }

        impl FromParam for $ty {
            fn from_param(p: Param) -> Option<Self> {
                match p {
                    Param::$variant(x) => Some(x),
                    _ => None,
                }
            }
        }
    )*};
}

param_field! {
    i32 => Int,
    FracId => Frac,
    StatsId => Stats,
    Stat => Stat,
    StatOrResource => StatOrResource,
    CityFilterId => CityFilter,
    UnitFilterId => UnitFilter,
    CivFilterId => CivFilter,
    CombatantFilterId => CombatantFilter,
    TileFilterId => TileFilter,
    SetRef => Set,
    ObjectFilterId => Object,
    BuildingId => Building,
    BaseUnitId => BaseUnit,
    PromotionId => Promotion,
    ResourceId => Resource,
    TechId => Tech,
    EraId => Era,
    DifficultyId => Difficulty,
    VictoryId => Victory,
    TerrainId => Terrain,
    FeatureId => Feature,
    PolicyOrBelief => PolicyOrBelief,
    PopulationFilter => Population,
    CostOrStrength => CostOrStrength,
    FoundingOrEnhancing => FoundingOrEnhancing,
    TerrainQuality => TerrainQuality,
    RegionType => Region,
    UnitTriggerTarget => Target,
    Countable => Countable,
    CountOrAll => CountOrAll,
    TextId => Text,
}

impl From<i16> for Param {
    fn from(x: i16) -> Self {
        Self::Int(i32::from(x))
    }
}

impl FromParam for i16 {
    fn from_param(p: Param) -> Option<Self> {
        match p {
            Param::Int(x) => i16::try_from(x).ok(),
            _ => None,
        }
    }
}

/// A parameter's value as the ruleset meant it, for reports and the reference checks: numbers as
/// numbers, stats as stats, and everything else as the text it was compiled from.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamValue {
    Int(i64),
    Real(f64),
    Stats(Stats),
    Text(String),
}

impl Param {
    /// The parameter's value, with ids named and handles back to their text.
    #[must_use]
    pub fn value(self, rules: &Ruleset) -> ParamValue {
        let u = rules.uniques();
        let name = |n: Option<&str>| ParamValue::Text(n.unwrap_or("?").to_owned());
        let text = |s: &str| ParamValue::Text(s.to_owned());
        match self {
            Self::Int(x) => ParamValue::Int(i64::from(x)),
            Self::Frac(f) => ParamValue::Real(rules.fracs().get(f).copied().unwrap_or(f64::NAN)),
            Self::Stats(s) => ParamValue::Stats(*u.stats(s)),
            Self::Stat(s) => text(s.name()),
            Self::StatOrResource(StatOrResource::Stat(s)) => text(s.name()),
            Self::StatOrResource(StatOrResource::Resource(r)) => name(rules.name(r)),
            Self::CityFilter(f) => text(u.city_filter(f)),
            Self::UnitFilter(f) => text(u.unit_filter(f)),
            Self::CivFilter(f) => text(u.civ_filter(f)),
            Self::CombatantFilter(f) => text(u.combatant_filter(f)),
            Self::TileFilter(f) => text(u.tile_filter(f)),
            Self::Set(s) => text(u.text(u.set(s).text)),
            Self::Object(o) => text(u.text(u.object(o).text)),
            Self::Building(x) => name(rules.name(x)),
            Self::BaseUnit(x) => name(rules.name(x)),
            Self::Promotion(x) => name(rules.name(x)),
            Self::Resource(x) => name(rules.name(x)),
            Self::Tech(x) => name(rules.name(x)),
            Self::Era(x) => name(rules.name(x)),
            Self::Difficulty(x) => name(rules.name(x)),
            Self::Victory(x) => name(rules.name(x)),
            Self::Terrain(x) => name(rules.name(x)),
            Self::Feature(f) => name(rules.derived().features.get(f).and_then(|&t| rules.name(t))),
            Self::PolicyOrBelief(PolicyOrBelief::Policy(x)) => name(rules.name(x)),
            Self::PolicyOrBelief(PolicyOrBelief::Belief(x)) => name(rules.name(x)),
            Self::Population(p) => match p {
                PopulationFilter::Specialist(s) => name(rules.name(s)),
                _ => text(population_word(p)),
            },
            Self::CostOrStrength(CostOrStrength::Cost) => text("Cost"),
            Self::CostOrStrength(CostOrStrength::Strength) => text("Strength"),
            Self::FoundingOrEnhancing(FoundingOrEnhancing::Founding) => text("founding"),
            Self::FoundingOrEnhancing(FoundingOrEnhancing::Enhancing) => text("enhancing"),
            Self::TerrainQuality(q) => text(match q {
                TerrainQuality::Undesirable => "Undesirable",
                TerrainQuality::Food => "Food",
                TerrainQuality::Production => "Production",
                TerrainQuality::Desirable => "Desirable",
            }),
            Self::Region(RegionType::Hybrid) => text("Hybrid"),
            Self::Region(RegionType::Terrain(t)) => name(rules.name(t)),
            Self::Target(UnitTriggerTarget::ThisUnit) => text("This Unit"),
            Self::Countable(c) => ParamValue::Text(countable_text(u, c)),
            Self::CountOrAll(c) => match c.get() {
                None => text("All"),
                Some(n) => ParamValue::Int(i64::from(n)),
            },
            Self::Text(t) => text(u.text(t)),
        }
    }
}

fn population_word(p: PopulationFilter) -> &'static str {
    match p {
        PopulationFilter::Population => "Population",
        PopulationFilter::Specialists => "Specialists",
        PopulationFilter::FollowersOfThisReligion => "Followers of this Religion",
        PopulationFilter::FollowersOfTheMajorityReligion => "Followers of the Majority Religion",
        PopulationFilter::Unemployed => "Unemployed",
        PopulationFilter::Specialist(_) => "",
    }
}

/// A countable written back as the ruleset writes it.
fn countable_text(u: &UniqueTable, c: Countable) -> String {
    match c {
        Countable::Int(n) => n.to_string(),
        Countable::Turns => "turns".into(),
        Countable::Cities => "Cities".into(),
        Countable::Units => "Units".into(),
        Countable::CompletedBranches => "Completed Policy branches".into(),
        Countable::Stat(s) => s.name().into(),
        Countable::UnitsMatching(f) => format!("[{}] Units", u.unit_filter(f)),
        Countable::CitiesMatching(f) => format!("[{}] Cities", u.city_filter(f)),
        Countable::RemainingCivs(f) => format!("Remaining [{}] Civilizations", u.civ_filter(f)),
        Countable::BuildingsMatching(s) => format!("[{}] Buildings", u.text(u.set(s).text)),
    }
}

// ---- Errors -----------------------------------------------------------------------------------

/// A parameter that does not compile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamError {
    /// Its position among the unique's parameters, from 0.
    pub index: usize,
    /// The kind it was compiled as.
    pub kind: Option<ParamKind>,
    /// Its text.
    pub text: Box<str>,
    /// What is wrong, as a phrase.
    pub message: String,
}

impl ParamError {
    /// A type compiled by the builder of another role, which only a generator bug can cause.
    pub(crate) fn role(ty: UniqueType, holder: &str) -> Self {
        Self {
            index: 0,
            kind: None,
            text: "".into(),
            message: format!("{} is not compiled as {holder}", ty.name()),
        }
    }
}

impl fmt::Display for ParamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            Some(kind) => write!(
                f,
                "parameter {} [{}] {:?}: {}",
                self.index + 1,
                kind.name(),
                self.text,
                self.message
            ),
            None => f.write_str(&self.message),
        }
    }
}

// ---- Compiling --------------------------------------------------------------------------------

/// A unique's parameters, being compiled against a ruleset.
pub(crate) struct ParamCx<'a, 'r> {
    pub(crate) params: &'a [&'a str],
    pub(crate) lx: &'a mut Lexicon<'r>,
    /// A parameter the compiler decided itself, by position: the relevant-promotion fixup.
    pub(crate) fixed: Option<(usize, Param)>,
}

impl ParamCx<'_, '_> {
    /// Parameter `i`, compiled as `kind` into the field type `T`.
    pub(crate) fn get<T: FromParam>(&mut self, i: usize, kind: ParamKind) -> Result<T, ParamError> {
        let text = self.params.get(i).copied().unwrap_or("");
        let fail =
            |message: String| ParamError { index: i, kind: Some(kind), text: text.into(), message };
        if i >= self.params.len() {
            return Err(fail("missing".into()));
        }
        let p = match self.fixed {
            Some((at, p)) if at == i => p,
            _ => self.lx.compile(kind, text).map_err(fail)?,
        };
        T::from_param(p).ok_or_else(|| fail("compiled to the wrong type (a generator bug)".into()))
    }
}

/// Interns into an [`IdVec`], deduplicating by key: the id of `key`, or a new one.
struct Interner<K, I, T> {
    index: DetMap<K, I>,
    items: IdVec<I, T>,
}

impl<K: core::hash::Hash + Eq, I: Id, T> Interner<K, I, T> {
    fn new() -> Self {
        Self { index: DetMap::default(), items: IdVec::new() }
    }

    fn get_or_insert(&mut self, key: K, make: impl FnOnce() -> T) -> Result<I, String> {
        if let Some(&id) = self.index.get(&key) {
            return Ok(id);
        }
        let id = self
            .items
            .push(make())
            .map_err(|_| format!("more than {} entries; widen {}", self.items.len(), I::NAME))?;
        self.index.insert(key, id);
        Ok(id)
    }
}

/// Everything the compiler interns, with the ruleset it resolves names in. Turned into a
/// [`UniqueTable`] (and the ruleset's fracs) when the compiler is done.
pub(crate) struct Lexicon<'r> {
    pub(crate) rules: &'r Ruleset,
    texts: DetSet<Box<str>>,
    stats: Interner<[u64; Stat::COUNT], StatsId, Stats>,
    fracs: Interner<u64, FracId, f64>,
    unit_filters: Interner<TextId, UnitFilterId, TextId>,
    tile_filters: Interner<TextId, TileFilterId, TextId>,
    city_filters: Interner<TextId, CityFilterId, TextId>,
    civ_filters: Interner<TextId, CivFilterId, TextId>,
    combatant_filters: Interner<TextId, CombatantFilterId, TextId>,
    sets: Interner<(StaticDomain, TextId, Option<Vec<u64>>), SetRef, StaticFilter>,
    objects: Interner<(ParamKind, TextId), ObjectFilterId, ObjectFilter>,
    tags: Interner<TextId, TagId, TextId>,
    abilities: Interner<TextId, AbilityKey, TextId>,
}

impl<'r> Lexicon<'r> {
    pub(crate) fn new(rules: &'r Ruleset) -> Self {
        Self {
            rules,
            texts: DetSet::default(),
            stats: Interner::new(),
            fracs: Interner::new(),
            unit_filters: Interner::new(),
            tile_filters: Interner::new(),
            city_filters: Interner::new(),
            civ_filters: Interner::new(),
            combatant_filters: Interner::new(),
            sets: Interner::new(),
            objects: Interner::new(),
            tags: Interner::new(),
            abilities: Interner::new(),
        }
    }

    /// Interns a text.
    pub(crate) fn text(&mut self, s: &str) -> Result<TextId, String> {
        if let Some(i) = self.texts.get_index_of(s) {
            return TextId::from_index(i).ok_or_else(|| "too many texts".into());
        }
        let (i, _) = self.texts.insert_full(s.into());
        TextId::from_index(i).ok_or_else(|| "more texts than a TextId holds".into())
    }

    /// The text interned as `id`.
    pub(crate) fn text_of(&self, id: TextId) -> &str {
        self.texts.get_index(id.index()).map_or("", |s| s)
    }

    /// The tag for `text`, interning it.
    pub(crate) fn tag(&mut self, text: &str) -> Result<TagId, String> {
        let t = self.text(text)?;
        self.tags.get_or_insert(t, || t)
    }

    /// The ability counted under `key`, interning it.
    pub(crate) fn ability(&mut self, key: &str) -> Result<AbilityKey, String> {
        let t = self.text(key)?;
        self.abilities.get_or_insert(t, || t)
    }

    /// A static filter whose members the compiler decided (the relevant-promotion fixup).
    pub(crate) fn fixed_set(
        &mut self,
        domain: StaticDomain,
        text: &str,
        members: BitSet,
    ) -> Result<SetRef, String> {
        let t = self.text(text)?;
        let key = (domain, t, Some(members.words().to_vec()));
        self.sets.get_or_insert(key, || StaticFilter { domain, text: t, members, fixed: true })
    }

    /// The texts of every filter interned so far, for finding the terms that name tags.
    pub(crate) fn filter_texts(&self) -> Vec<TextId> {
        let mut out: Vec<TextId> = Vec::new();
        out.extend(self.unit_filters.items.as_slice());
        out.extend(self.tile_filters.items.as_slice());
        out.extend(self.city_filters.items.as_slice());
        out.extend(self.civ_filters.items.as_slice());
        out.extend(self.combatant_filters.items.as_slice());
        out.extend(self.sets.items.as_slice().iter().map(|s| s.text));
        out.extend(self.objects.items.as_slice().iter().map(|o| o.text));
        out
    }

    /// Everything interned, as the ruleset keeps it: the unique table's shared parts, and the
    /// fracs.
    pub(crate) fn finish(self, table: &mut UniqueTable) -> IdVec<FracId, f64> {
        table.texts = self.texts.into_iter().collect();
        table.stats = self.stats.items;
        table.sets = self.sets.items;
        table.objects = self.objects.items;
        table.unit_filters = self.unit_filters.items;
        table.tile_filters = self.tile_filters.items;
        table.city_filters = self.city_filters.items;
        table.civ_filters = self.civ_filters.items;
        table.combatant_filters = self.combatant_filters.items;
        table.tags = self.tags.items;
        table.abilities = self.abilities.items;
        self.fracs.items
    }

    /// Compiles one parameter's text as `kind`.
    #[allow(clippy::too_many_lines, reason = "one arm per kind")]
    fn compile(&mut self, kind: ParamKind, text: &str) -> Result<Param, String> {
        use ParamKind as K;
        let r = self.rules;
        Ok(match kind {
            K::Amount | K::RelativeAmount => Param::Int(amount(text, -AMOUNT_LIMIT)?),
            K::PositiveAmount => Param::Int(amount(text, 1)?),
            K::NonNegativeAmount => Param::Int(amount(text, 0)?),
            K::Amount16 => {
                let n = amount(text, -AMOUNT_LIMIT)?;
                if i16::try_from(n).is_err() {
                    return Err(format!("{n} does not fit the 16 bits this field has"));
                }
                Param::Int(n)
            }
            K::Fraction => Param::Frac(self.fraction(text)?),
            K::Stats => Param::Stats(self.stats(text)?),
            K::Stat => Param::Stat(stat(text)?),
            K::CivWideStat => {
                let s = stat(text)?;
                if !s.is_civ_wide() {
                    return Err(format!("{s} is not a stat a civilization pools"));
                }
                Param::Stat(s)
            }
            K::Stockpile => {
                let s = stat(text)?;
                if !matches!(s, Stat::Gold | Stat::Science | Stat::Culture | Stat::Faith) {
                    return Err(format!("{s} is not a stat a civilization stockpiles"));
                }
                Param::Stat(s)
            }
            K::StatOrResource => match r.lookup::<ResourceId>(text) {
                Some(res) => Param::StatOrResource(StatOrResource::Resource(res)),
                None => Param::StatOrResource(StatOrResource::Stat(
                    stat(text).map_err(|_| format!("{text:?} is neither a resource nor a stat"))?,
                )),
            },
            K::CityFilter => Param::CityFilter(self.city_filter(text)?),
            K::MapUnitFilter => Param::UnitFilter(self.unit_filter(text)?),
            K::CivFilter => Param::CivFilter(self.civ_filter(text)?),
            K::CombatantFilter => {
                let t = self.filter_text(text)?;
                Param::CombatantFilter(self.combatant_filters.get_or_insert(t, || t)?)
            }
            K::TileFilter | K::TerrainFilter | K::SimpleTerrain => {
                Param::TileFilter(self.tile_filter(text)?)
            }
            K::BaseUnitFilter => Param::Set(self.set(StaticDomain::BaseUnit, text)?),
            K::BuildingFilter => Param::Set(self.set(StaticDomain::Building, text)?),
            K::ImprovementFilter => Param::Set(self.set(StaticDomain::Improvement, text)?),
            K::ResourceFilter => Param::Set(self.set(StaticDomain::Resource, text)?),
            K::TechFilter => Param::Set(self.set(StaticDomain::Tech, text)?),
            K::EraFilter => Param::Set(self.set(StaticDomain::Era, text)?),
            K::TileOrBuildingFilter
            | K::TileSpecialistOrBuildingFilter
            | K::ImprovementOrTerrainFilter => Param::Object(self.object(kind, text)?),
            K::BuildingName => Param::Building(named(r, text, "building")?),
            // Whether the unit is a great person is known only once its uniques are compiled,
            // which is this pass, so a great person is checked as a unit, as Python did.
            K::Unit | K::GreatPerson => Param::BaseUnit(named(r, text, "unit")?),
            K::Promotion => Param::Promotion(named(r, text, "promotion")?),
            K::Resource => Param::Resource(named(r, text, "resource")?),
            K::Tech => Param::Tech(named(r, text, "technology")?),
            K::Era => Param::Era(named(r, text, "era")?),
            K::Difficulty => Param::Difficulty(named(r, text, "difficulty")?),
            K::VictoryType => Param::Victory(named(r, text, "victory")?),
            K::TerrainName => Param::Terrain(named(r, text, "terrain")?),
            K::TerrainFeature => {
                let t: TerrainId = named(r, text, "terrain")?;
                match r.terrains()[t].feature {
                    Some(f) => Param::Feature(f),
                    None => return Err(format!("{text:?} is not a terrain feature")),
                }
            }
            K::BaseTerrainOrFeature => {
                let t: TerrainId = named(r, text, "terrain")?;
                if r.terrains()[t].kind == TerrainType::NaturalWonder {
                    return Err(format!("{text:?} is a natural wonder"));
                }
                Param::Terrain(t)
            }
            K::PolicyOrBelief => match (r.lookup::<PolicyId>(text), r.lookup::<BeliefId>(text)) {
                (Some(p), _) => Param::PolicyOrBelief(PolicyOrBelief::Policy(p)),
                (None, Some(b)) => Param::PolicyOrBelief(PolicyOrBelief::Belief(b)),
                (None, None) => return Err(format!("{text:?} is neither a policy nor a belief")),
            },
            K::PopulationFilter => Param::Population(match text {
                "Population" => PopulationFilter::Population,
                "Specialists" => PopulationFilter::Specialists,
                "Followers of this Religion" => PopulationFilter::FollowersOfThisReligion,
                "Followers of the Majority Religion" => {
                    PopulationFilter::FollowersOfTheMajorityReligion
                }
                "Unemployed" => PopulationFilter::Unemployed,
                _ => PopulationFilter::Specialist(r.lookup::<SpecialistId>(text).ok_or_else(
                    || format!("{text:?} is no population filter and no specialist"),
                )?),
            }),
            K::CostOrStrength => Param::CostOrStrength(match text {
                "Cost" => CostOrStrength::Cost,
                "Strength" => CostOrStrength::Strength,
                _ => return Err("expected Cost or Strength".into()),
            }),
            K::FoundingOrEnhancing => Param::FoundingOrEnhancing(match text {
                "founding" => FoundingOrEnhancing::Founding,
                "enhancing" => FoundingOrEnhancing::Enhancing,
                _ => return Err("expected founding or enhancing".into()),
            }),
            K::TerrainQuality => Param::TerrainQuality(match text {
                "Undesirable" => TerrainQuality::Undesirable,
                "Food" => TerrainQuality::Food,
                "Production" => TerrainQuality::Production,
                "Desirable" => TerrainQuality::Desirable,
                _ => return Err("expected Undesirable, Food, Production or Desirable".into()),
            }),
            K::RegionType => {
                Param::Region(match text {
                    "Hybrid" => RegionType::Hybrid,
                    _ => RegionType::Terrain(r.lookup::<TerrainId>(text).ok_or_else(|| {
                        format!("{text:?} is no region type: Hybrid, or a terrain")
                    })?),
                })
            }
            K::UnitTriggerTarget => Param::Target(match text {
                "This Unit" => UnitTriggerTarget::ThisUnit,
                _ => return Err("expected This Unit".into()),
            }),
            K::Countable => Param::Countable(self.countable(text)?),
            K::CountOrAll => Param::CountOrAll(if matches!(text, "All" | "all") {
                CountOrAll::ALL
            } else {
                let n = amount(text, 1)?;
                u32::try_from(n).ok().and_then(CountOrAll::count).ok_or("not a count")?
            }),
            K::Comment => Param::Text(self.text(text)?),
        })
    }

    fn fraction(&mut self, text: &str) -> Result<FracId, String> {
        let x: f64 = text.parse().map_err(|_| "not a number".to_owned())?;
        if !x.is_finite() || x.abs() > f64::from(AMOUNT_LIMIT) {
            return Err(format!("{text} is out of range (±{AMOUNT_LIMIT})"));
        }
        self.fracs.get_or_insert(x.to_bits(), || x)
    }

    fn stats(&mut self, text: &str) -> Result<StatsId, String> {
        let stats = super::text::parse_stats(text).ok_or_else(|| {
            let names: Vec<&str> = Stat::ALL.iter().map(|s| s.name()).collect();
            format!(
                "not stats: each part must be a number, a space and one of {}",
                names.join(", ")
            )
        })?;
        for (s, v) in stats.iter() {
            if !v.is_finite() || v.abs() > f64::from(AMOUNT_LIMIT) {
                return Err(format!("{s} {v} is out of range (±{AMOUNT_LIMIT})"));
            }
        }
        self.stats.get_or_insert(stats.0.map(f64::to_bits), || stats)
    }

    /// A filter's text, interned; empty is refused, since it matches nothing.
    fn filter_text(&mut self, text: &str) -> Result<TextId, String> {
        if text.is_empty() {
            return Err("an empty filter".into());
        }
        self.text(text)
    }

    fn unit_filter(&mut self, text: &str) -> Result<UnitFilterId, String> {
        let t = self.filter_text(text)?;
        self.unit_filters.get_or_insert(t, || t)
    }

    /// The tile filter `text`, interning it.
    pub(crate) fn tile_filter(&mut self, text: &str) -> Result<TileFilterId, String> {
        let t = self.filter_text(text)?;
        self.tile_filters.get_or_insert(t, || t)
    }

    fn city_filter(&mut self, text: &str) -> Result<CityFilterId, String> {
        let t = self.filter_text(text)?;
        self.city_filters.get_or_insert(t, || t)
    }

    fn civ_filter(&mut self, text: &str) -> Result<CivFilterId, String> {
        let t = self.filter_text(text)?;
        self.civ_filters.get_or_insert(t, || t)
    }

    fn set(&mut self, domain: StaticDomain, text: &str) -> Result<SetRef, String> {
        let t = self.filter_text(text)?;
        // `unique::filter` evaluates the members once every object is known.
        self.sets.get_or_insert((domain, t, None), || StaticFilter {
            domain,
            text: t,
            members: BitSet::new(),
            fixed: false,
        })
    }

    fn object(&mut self, kind: ParamKind, text: &str) -> Result<ObjectFilterId, String> {
        let t = self.filter_text(text)?;
        if let Some(&id) = self.objects.index.get(&(kind, t)) {
            return Ok(id);
        }
        let mut o = ObjectFilter {
            kind,
            text: t,
            tiles: None,
            buildings: None,
            improvements: None,
            specialist: None,
        };
        match kind {
            ParamKind::TileOrBuildingFilter => {
                o.tiles = Some(self.tile_filter(text)?);
                o.buildings = Some(self.set(StaticDomain::Building, text)?);
            }
            ParamKind::TileSpecialistOrBuildingFilter => {
                o.tiles = Some(self.tile_filter(text)?);
                o.buildings = Some(self.set(StaticDomain::Building, text)?);
                o.specialist = self.rules.lookup::<SpecialistId>(text);
            }
            _ => {
                o.improvements = Some(self.set(StaticDomain::Improvement, text)?);
                o.tiles = Some(self.tile_filter(text)?);
            }
        }
        self.objects.get_or_insert((kind, t), || o)
    }

    fn countable(&mut self, text: &str) -> Result<Countable, String> {
        let c = countable::parse(text).ok_or_else(|| {
            format!(
                "{text:?} is not a countable: a number, turns, Cities, Units, Completed Policy \
                 branches, a stat, [filter] Units, [filter] Cities, Remaining [filter] \
                 Civilizations or [filter] Buildings"
            )
        })?;
        Ok(match c {
            CountableText::Int(n) => {
                if n.abs() > AMOUNT_LIMIT {
                    return Err(format!("{n} is out of range (±{AMOUNT_LIMIT})"));
                }
                Countable::Int(n)
            }
            CountableText::Turns => Countable::Turns,
            CountableText::Cities => Countable::Cities,
            CountableText::Units => Countable::Units,
            CountableText::CompletedBranches => Countable::CompletedBranches,
            CountableText::Stat(s) => Countable::Stat(s),
            CountableText::UnitsMatching(f) => Countable::UnitsMatching(self.unit_filter(f)?),
            CountableText::CitiesMatching(f) => Countable::CitiesMatching(self.city_filter(f)?),
            CountableText::RemainingCivs(f) => Countable::RemainingCivs(self.civ_filter(f)?),
            CountableText::BuildingsMatching(f) => {
                Countable::BuildingsMatching(self.set(StaticDomain::Building, f)?)
            }
        })
    }
}

/// A whole number of at least `min` and at most [`AMOUNT_LIMIT`], with an optional sign:
/// `+15`, `-33`, `2`. Python's `num()` read anything unparseable as 0 (`uniques.py:93-102`).
fn amount(text: &str, min: i32) -> Result<i32, String> {
    let n: i32 = text.parse().map_err(|_| "not a whole number".to_owned())?;
    if n < min || n > AMOUNT_LIMIT {
        return Err(format!("{n} is out of range ({min} to {AMOUNT_LIMIT})"));
    }
    Ok(n)
}

fn stat(text: &str) -> Result<Stat, String> {
    Stat::from_name(text).ok_or_else(|| {
        let names: Vec<&str> = Stat::ALL.iter().map(|s| s.name()).collect();
        format!("not a stat: expected one of {}", names.join(", "))
    })
}

/// The object called exactly `text` in the table of `I`.
fn named<I: crate::rules::Named>(r: &Ruleset, text: &str, what: &str) -> Result<I, String> {
    r.lookup::<I>(text).ok_or_else(|| format!("no {what} is called {text:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_are_whole_and_in_range() {
        assert_eq!(amount("+15", -AMOUNT_LIMIT), Ok(15));
        assert_eq!(amount("-33", -AMOUNT_LIMIT), Ok(-33));
        assert_eq!(amount("0", 0), Ok(0));
        assert!(amount("0", 1).is_err(), "a positive amount");
        assert!(amount("-1", 0).is_err(), "a non-negative amount");
        assert!(amount("1.5", 0).unwrap_err().contains("whole number"));
        assert!(amount(" 2", 0).is_err(), "no spaces");
        assert!(amount("2000000", 0).unwrap_err().contains("out of range"));
        assert!(amount("", 0).is_err());
    }

    #[test]
    fn stats_are_named_as_unciv_names_them() {
        assert_eq!(stat("Gold"), Ok(Stat::Gold));
        assert!(stat("gold").is_err());
        assert!(stat("Gold ").is_err());
    }

    #[test]
    fn count_or_all() {
        assert_eq!(CountOrAll::ALL.get(), None);
        assert_eq!(CountOrAll::count(3).and_then(CountOrAll::get), Some(3));
        assert_eq!(CountOrAll::count(0), None);
        assert_eq!(core::mem::size_of::<CountOrAll>(), 4);
    }

    #[test]
    fn params_convert_to_their_field_types_and_back() {
        assert_eq!(i32::from_param(Param::from(7_i32)), Some(7));
        assert_eq!(i16::from_param(Param::from(-7_i16)), Some(-7));
        assert_eq!(i16::from_param(Param::Int(40_000)), None);
        assert_eq!(StatsId::from_param(Param::Int(1)), None);
        assert_eq!(Stat::from_param(Param::from(Stat::Faith)), Some(Stat::Faith));
    }
}
