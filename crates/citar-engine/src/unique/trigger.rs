//! Triggers and one-time effects (DESIGN.md 5.9): when a triggered unique fires, and what a
//! one-time unique does, decoded.
//!
//! Ports the matching half of `triggers.py:13-74`: `fire` gathered the uniques carrying a trigger
//! of one kind from the civilization's maps, the city's local maps and the unit's map, filtered by
//! the trigger's parameter and by their conditionals, and applied each (`trigger`,
//! `triggers.py:75-367`). Here [`fire`] only finds them; [`OneTimeEffect::decode`] says what each
//! does, fully resolved, and `game::triggers::apply_one_time` (package 1b-08) does it.
//!
//! Python's callers passed a `filt` lambda comparing the trigger's parameter with what happened,
//! one per site. Here what happened is a typed [`TriggerEvent`], and [`TriggerCond::matches`]
//! compares it with the compiled parameter, the same at every site: so `upon gaining a [unit]`
//! reads its filter wherever a unit is gained (`great_people.py:140` fired it for every great
//! person, whatever the filter said). That fix, and `upon being defeated` firing at all, are rule
//! differences no reference check can show: refcheck asks questions of a standing state, and
//! triggers fire only while a turn is played (DESIGN.md 5.9). A unit that is going away (lost,
//! defeated, expended) is in its event as [`UnitFacts`] taken before it was removed, so a site
//! may fire after the removal, as Python's did (`combat.py:602-608, 831`).

use smallvec::SmallVec;

use super::cond::applies;
use super::filter::UnitFacts;
use super::generated::{TriggerCond, UniqueData, UniqueType};
use super::params::{CountOrAll, PolicyOrBelief};
use super::table::UFlags;
use super::world::{Ctx, EvalWorld, IndexLayer, IndexRef};
use crate::base::ids::{
    BaseUnitId, BuildingId, CityFilterId, CityId, EraId, ImprovementId, PlayerId, PromotionId,
    SetRef, TechId, TileFilterId, TileIdx, UniqueId, UnitFilterId, UnitId,
};
use crate::base::stats::Stat;
use crate::rules::Ruleset;
use crate::rules::defs::BeliefKind;

/// What a trigger waits for: one kind per trigger the engine fires (25), each fired at one kind
/// of site.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TriggerKind {
    /// `upon discovering [techFilter] technology`: research (`research.py:320`).
    Research,
    /// `upon entering the [era]` (`research.py:375`).
    EnteringEra,
    /// `upon adopting [policy/belief]` (`policies.py:144`, `religion.py:534`).
    Adopting,
    /// `upon declaring war on [civFilter] Civilizations` (`diplomacy.py:234`).
    DeclaringWar,
    /// `upon being declared war on by [civFilter] Civilizations` (`diplomacy.py:235`).
    BeingDeclaredWarUpon,
    /// `upon entering a war with [civFilter] Civilizations`: either side (`diplomacy.py:236-237`).
    EnteringWar,
    /// `upon signing a peace treaty with [civFilter] Civilizations` (`diplomacy.py:263-264`).
    SigningPeace,
    /// `upon declaring friendship` (`diplomacy.py:584-585`).
    DeclaringFriendship,
    /// `upon declaring a defensive pact` (`diplomacy.py:595-596`).
    SigningDefensivePact,
    /// `upon entering a Golden Age` (`great_people.py:266`).
    EnteringGoldenAge,
    /// `upon conquering a city` (`conquest.py:193`).
    ConqueringCity,
    /// `upon losing a city` (`conquest.py:148`).
    LosingCity,
    /// `upon founding a city` (`cities.py:2180`).
    FoundingCity,
    /// `upon building a [improvementFilter] improvement` (`workers.py:416`).
    BuildingImprovement,
    /// `upon gaining a [baseUnitFilter] unit` (`units.py:135`, `great_people.py:140`).
    GainingUnit,
    /// `upon losing a [mapUnitFilter] unit` (`combat.py:608`).
    LosingUnit,
    /// `upon turn end` (`turns.py:86`).
    TurnEnd,
    /// `upon turn start` (`turns.py:49`).
    TurnStart,
    /// `upon founding a Pantheon` (`religion.py:519`).
    FoundingPantheon,
    /// `upon founding a Religion` (`religion.py:667`).
    FoundingReligion,
    /// `upon enhancing a Religion` (`religion.py:695`).
    EnhancingReligion,
    /// `upon defeating a [mapUnitFilter] unit` (`combat.py:831`).
    DefeatingUnit,
    /// `upon expending a [mapUnitFilter] unit` (`great_people.py:368`, `units.py:480`).
    ExpendingUnit,
    /// `upon being defeated`, which Python let through and never fired: the combat port fires it
    /// for the unit that loses a fight.
    Defeat,
    /// `upon being promoted` (`units.py:237`).
    Promotion,
}

impl TriggerKind {
    /// How many kinds there are.
    pub const COUNT: usize = 25;

    /// Every kind, in declaration order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Research,
        Self::EnteringEra,
        Self::Adopting,
        Self::DeclaringWar,
        Self::BeingDeclaredWarUpon,
        Self::EnteringWar,
        Self::SigningPeace,
        Self::DeclaringFriendship,
        Self::SigningDefensivePact,
        Self::EnteringGoldenAge,
        Self::ConqueringCity,
        Self::LosingCity,
        Self::FoundingCity,
        Self::BuildingImprovement,
        Self::GainingUnit,
        Self::LosingUnit,
        Self::TurnEnd,
        Self::TurnStart,
        Self::FoundingPantheon,
        Self::FoundingReligion,
        Self::EnhancingReligion,
        Self::DefeatingUnit,
        Self::ExpendingUnit,
        Self::Defeat,
        Self::Promotion,
    ];

    /// The trigger's unique type: where an index holds the uniques it fires.
    #[must_use]
    pub const fn ty(self) -> UniqueType {
        match self {
            Self::Research => UniqueType::TriggerUponResearch,
            Self::EnteringEra => UniqueType::TriggerUponEnteringEra,
            Self::Adopting => UniqueType::TriggerUponAdoptingPolicyOrBelief,
            Self::DeclaringWar => UniqueType::TriggerUponDeclaringWarFiltered,
            Self::BeingDeclaredWarUpon => UniqueType::TriggerUponBeingDeclaredWarUpon,
            Self::EnteringWar => UniqueType::TriggerUponEnteringWar,
            Self::SigningPeace => UniqueType::TriggerUponSigningPeace,
            Self::DeclaringFriendship => UniqueType::TriggerUponDeclaringFriendship,
            Self::SigningDefensivePact => UniqueType::TriggerUponSigningDefensivePact,
            Self::EnteringGoldenAge => UniqueType::TriggerUponEnteringGoldenAge,
            Self::ConqueringCity => UniqueType::TriggerUponConqueringCity,
            Self::LosingCity => UniqueType::TriggerUponLosingCity,
            Self::FoundingCity => UniqueType::TriggerUponFoundingCity,
            Self::BuildingImprovement => UniqueType::TriggerUponBuildingImprovement,
            Self::GainingUnit => UniqueType::TriggerUponGainingUnit,
            Self::LosingUnit => UniqueType::TriggerUponLosingUnit,
            Self::TurnEnd => UniqueType::TriggerUponTurnEnd,
            Self::TurnStart => UniqueType::TriggerUponTurnStart,
            Self::FoundingPantheon => UniqueType::TriggerUponFoundingPantheon,
            Self::FoundingReligion => UniqueType::TriggerUponFoundingReligion,
            Self::EnhancingReligion => UniqueType::TriggerUponEnhancingReligion,
            Self::DefeatingUnit => UniqueType::TriggerUponDefeatingUnit,
            Self::ExpendingUnit => UniqueType::TriggerUponExpendingUnit,
            Self::Defeat => UniqueType::TriggerUponDefeat,
            Self::Promotion => UniqueType::TriggerUponPromotion,
        }
    }
}

/// What happened, at a site that fires triggers: the kind, and what the trigger's parameter is
/// compared with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TriggerEvent {
    /// The civilization discovered the tech.
    Research(TechId),
    /// The civilization entered the era.
    EnteringEra(EraId),
    /// The civilization adopted the policy or took the belief.
    Adopting(PolicyOrBelief),
    /// The civilization declared war on `on`.
    DeclaringWar {
        on: PlayerId,
    },
    /// `by` declared war on the civilization.
    BeingDeclaredWarUpon {
        by: PlayerId,
    },
    /// The civilization entered a war with `with`, either side declaring.
    EnteringWar {
        with: PlayerId,
    },
    /// The civilization made peace with `with`.
    SigningPeace {
        with: PlayerId,
    },
    DeclaringFriendship,
    SigningDefensivePact,
    EnteringGoldenAge,
    ConqueringCity,
    LosingCity,
    FoundingCity,
    /// The civilization built the improvement.
    BuildingImprovement(ImprovementId),
    /// The civilization gained a unit of this row.
    GainingUnit(BaseUnitId),
    /// The civilization lost a unit: its facts, taken before it was removed.
    LosingUnit(UnitFacts),
    TurnEnd,
    TurnStart,
    FoundingPantheon,
    FoundingReligion,
    EnhancingReligion,
    /// The civilization's unit defeated a unit: the victim's facts, taken before it was removed.
    DefeatingUnit(UnitFacts),
    /// The civilization expended a unit: its facts, taken before it was removed.
    ExpendingUnit(UnitFacts),
    /// The unit in context was defeated.
    Defeat,
    /// The unit in context was promoted.
    Promotion,
}

impl TriggerEvent {
    /// Its kind.
    #[must_use]
    pub const fn kind(&self) -> TriggerKind {
        match self {
            Self::Research(_) => TriggerKind::Research,
            Self::EnteringEra(_) => TriggerKind::EnteringEra,
            Self::Adopting(_) => TriggerKind::Adopting,
            Self::DeclaringWar { .. } => TriggerKind::DeclaringWar,
            Self::BeingDeclaredWarUpon { .. } => TriggerKind::BeingDeclaredWarUpon,
            Self::EnteringWar { .. } => TriggerKind::EnteringWar,
            Self::SigningPeace { .. } => TriggerKind::SigningPeace,
            Self::DeclaringFriendship => TriggerKind::DeclaringFriendship,
            Self::SigningDefensivePact => TriggerKind::SigningDefensivePact,
            Self::EnteringGoldenAge => TriggerKind::EnteringGoldenAge,
            Self::ConqueringCity => TriggerKind::ConqueringCity,
            Self::LosingCity => TriggerKind::LosingCity,
            Self::FoundingCity => TriggerKind::FoundingCity,
            Self::BuildingImprovement(_) => TriggerKind::BuildingImprovement,
            Self::GainingUnit(_) => TriggerKind::GainingUnit,
            Self::LosingUnit(_) => TriggerKind::LosingUnit,
            Self::TurnEnd => TriggerKind::TurnEnd,
            Self::TurnStart => TriggerKind::TurnStart,
            Self::FoundingPantheon => TriggerKind::FoundingPantheon,
            Self::FoundingReligion => TriggerKind::FoundingReligion,
            Self::EnhancingReligion => TriggerKind::EnhancingReligion,
            Self::DefeatingUnit(_) => TriggerKind::DefeatingUnit,
            Self::ExpendingUnit(_) => TriggerKind::ExpendingUnit,
            Self::Defeat => TriggerKind::Defeat,
            Self::Promotion => TriggerKind::Promotion,
        }
    }
}

impl TriggerCond {
    /// Its kind.
    #[must_use]
    pub const fn kind(&self) -> TriggerKind {
        match self {
            Self::TriggerUponResearch(_) => TriggerKind::Research,
            Self::TriggerUponEnteringEra(_) => TriggerKind::EnteringEra,
            Self::TriggerUponAdoptingPolicyOrBelief(_) => TriggerKind::Adopting,
            Self::TriggerUponDeclaringWarFiltered(_) => TriggerKind::DeclaringWar,
            Self::TriggerUponBeingDeclaredWarUpon(_) => TriggerKind::BeingDeclaredWarUpon,
            Self::TriggerUponEnteringWar(_) => TriggerKind::EnteringWar,
            Self::TriggerUponSigningPeace(_) => TriggerKind::SigningPeace,
            Self::TriggerUponDeclaringFriendship => TriggerKind::DeclaringFriendship,
            Self::TriggerUponSigningDefensivePact => TriggerKind::SigningDefensivePact,
            Self::TriggerUponEnteringGoldenAge => TriggerKind::EnteringGoldenAge,
            Self::TriggerUponConqueringCity => TriggerKind::ConqueringCity,
            Self::TriggerUponLosingCity => TriggerKind::LosingCity,
            Self::TriggerUponFoundingCity => TriggerKind::FoundingCity,
            Self::TriggerUponBuildingImprovement(_) => TriggerKind::BuildingImprovement,
            Self::TriggerUponGainingUnit(_) => TriggerKind::GainingUnit,
            Self::TriggerUponLosingUnit(_) => TriggerKind::LosingUnit,
            Self::TriggerUponTurnEnd => TriggerKind::TurnEnd,
            Self::TriggerUponTurnStart => TriggerKind::TurnStart,
            Self::TriggerUponFoundingPantheon => TriggerKind::FoundingPantheon,
            Self::TriggerUponFoundingReligion => TriggerKind::FoundingReligion,
            Self::TriggerUponEnhancingReligion => TriggerKind::EnhancingReligion,
            Self::TriggerUponDefeatingUnit(_) => TriggerKind::DefeatingUnit,
            Self::TriggerUponExpendingUnit(_) => TriggerKind::ExpendingUnit,
            Self::TriggerUponDefeat => TriggerKind::Defeat,
            Self::TriggerUponPromotion => TriggerKind::Promotion,
        }
    }

    /// Whether the trigger fires for `event`, which happened to civilization `civ`: the kinds
    /// agree, and the event passes the trigger's parameter. The other civilization of a war or a
    /// peace is seen by `civ` (`diplomacy.py:234-237`), a unit is matched by its facts with
    /// nothing in context (`combat.py:608`), and a tech, an improvement or a unit's row is in the
    /// trigger's set.
    pub fn matches<W: EvalWorld>(&self, event: &TriggerEvent, civ: PlayerId, w: &W) -> bool {
        use TriggerEvent as E;
        let t = w.rules().uniques();
        let f = t.filters();
        let civ_seen = |x, other| f.civ_matches(x, w, other, Some(civ));
        let unit_seen = |x, u: UnitFacts| f.unit_facts_match(x, w, &u, None);
        match (*self, *event) {
            (Self::TriggerUponResearch(x), E::Research(tech)) => t.in_set(x.techs, tech),
            (Self::TriggerUponEnteringEra(x), E::EnteringEra(era)) => x.era == era,
            (Self::TriggerUponAdoptingPolicyOrBelief(x), E::Adopting(what)) => x.adopted == what,
            (Self::TriggerUponDeclaringWarFiltered(x), E::DeclaringWar { on }) => {
                civ_seen(x.civs, on)
            }
            (Self::TriggerUponBeingDeclaredWarUpon(x), E::BeingDeclaredWarUpon { by }) => {
                civ_seen(x.civs, by)
            }
            (Self::TriggerUponEnteringWar(x), E::EnteringWar { with }) => civ_seen(x.civs, with),
            (Self::TriggerUponSigningPeace(x), E::SigningPeace { with }) => civ_seen(x.civs, with),
            (Self::TriggerUponBuildingImprovement(x), E::BuildingImprovement(i)) => {
                t.in_set(x.improvements, i)
            }
            (Self::TriggerUponGainingUnit(x), E::GainingUnit(b)) => t.in_set(x.units, b),
            (Self::TriggerUponLosingUnit(x), E::LosingUnit(u)) => unit_seen(x.units, u),
            (Self::TriggerUponDefeatingUnit(x), E::DefeatingUnit(u)) => unit_seen(x.units, u),
            (Self::TriggerUponExpendingUnit(x), E::ExpendingUnit(u)) => unit_seen(x.units, u),
            (
                Self::TriggerUponDeclaringFriendship
                | Self::TriggerUponSigningDefensivePact
                | Self::TriggerUponEnteringGoldenAge
                | Self::TriggerUponConqueringCity
                | Self::TriggerUponLosingCity
                | Self::TriggerUponFoundingCity
                | Self::TriggerUponTurnEnd
                | Self::TriggerUponTurnStart
                | Self::TriggerUponFoundingPantheon
                | Self::TriggerUponFoundingReligion
                | Self::TriggerUponEnhancingReligion
                | Self::TriggerUponDefeat
                | Self::TriggerUponPromotion,
                _,
            ) => self.kind() == event.kind(),
            _ => false,
        }
    }
}

/// Where a trigger fires: the civilization it happened to, and the city, unit and tile it
/// happened at, if any (`fire`'s arguments, `triggers.py:38-39`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TriggerSite {
    pub civ: PlayerId,
    pub city: Option<CityId>,
    pub unit: Option<UnitId>,
    pub tile: Option<TileIdx>,
}

impl TriggerSite {
    /// A site with the civilization alone.
    #[must_use]
    pub const fn civ(civ: PlayerId) -> Self {
        Self { civ, city: None, unit: None, tile: None }
    }

    /// The context its uniques' conditionals are evaluated in: the site's, the tile derived from
    /// the unit or the city (`triggers.py:41`).
    #[must_use]
    pub fn ctx<W: EvalWorld>(&self, w: &W) -> Ctx {
        Ctx {
            civ: Some(self.civ),
            city: self.city,
            unit: self.unit,
            tile: self.tile,
            ..Ctx::default()
        }
        .resolve(w)
    }
}

/// The uniques that fire for `event` at `site` (`fire`, `triggers.py:38-65`), each as many times as
/// the index holds it, in Python's order: the civilization's, then the city's local ones and its
/// religion's, then, with `include_unit`, the unit's profile's. A unique fires when its trigger
/// matches the event and its conditionals hold at the site; a timed unique's conditionals are
/// its effect's, asked while it lasts, so it fires on its trigger alone, as UnCiv lets it
/// (`Unique.conditionalsApply`), where Python asked them at the grant.
pub fn fire<W: EvalWorld>(
    w: &W,
    site: &TriggerSite,
    event: &TriggerEvent,
    include_unit: bool,
) -> SmallVec<[UniqueId; 4]> {
    let ctx = site.ctx(w);
    let ty = event.kind().ty();
    let mut out = SmallVec::new();
    let mut take = |ix: IndexRef<'_>| {
        let t = w.rules().uniques();
        for e in ix.get(ty) {
            let meta = t.meta(e.id);
            // refcheck: timed-uniques-granted-whatever-their-conditionals
            let fires = meta.trigger.is_some_and(|tr| {
                tr.matches(event, site.civ, w) && (meta.timed.is_some() || applies(e.id, &ctx, w))
            });
            if fires {
                out.extend(core::iter::repeat_n(e.id, usize::from(e.n)));
            }
        }
    };
    take(w.civ_index(site.civ, IndexLayer::Full));
    if let Some(c) = site.city {
        take(w.city_local(c));
        if let Some(r) = w.city_majority_religion(c) {
            take(w.follower(r));
        }
    }
    if let (Some(u), true) = (site.unit, include_unit) {
        take(w.unit_index(u));
    }
    out
}

// ---- One-time effects ------------------------------------------------------------------------

/// Which cities an effect reaches: the city in context for `[in this city]`, else the
/// civilization's cities the filter selects (`_cities_for`, `triggers.py:58-61`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CityScope {
    ThisCity,
    Matching(CityFilterId),
}

/// What a one-time effect does to the unit whose unique it is (`[This Unit] ...`,
/// `triggers.py:339-367`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnitEffect {
    /// Heals it by this much, to full health at most.
    Heal(i32),
    Damage(i32),
    GainXp(i32),
    /// Upgrades it for free.
    Upgrade,
    /// Upgrades it for free along its special path (a great person's).
    SpecialUpgrade,
    GainPromotion(PromotionId),
    /// Changes its movement left by this many movement points: positive to gain, negative to
    /// lose.
    Movement(i32),
    Destroyed,
}

/// What a one-time unique does, fully resolved (DESIGN.md 5.9): one kind per one-time type (39),
/// merged where two types differ only in a count, plus the standing effects that also happen
/// once when their source is gained (`TRIGGERABLE`, `triggers.py:13-26`), and a timed unique's
/// grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OneTimeEffect {
    /// `Free [unit] appears`, `[n] free [unit] units appear`, `Free [unit] found in the ruins`:
    /// the ruins' unit is placed near the tile in context.
    FreeUnits { unit: BaseUnitId, count: i32, near_tile: bool },
    /// `Free Social Policy`, `[n] Free Social Policies`.
    FreePolicies { count: i32 },
    /// `Adopt [policy/belief]`. Python adopted a policy and did nothing for a belief
    /// (`triggers.py:141-147`); which a belief does is for the religion port to say.
    Adopt(PolicyOrBelief),
    /// `Empire enters golden age`, or one of `[n]` turns.
    GoldenAge { turns: Option<i32> },
    /// `Free Great Person`.
    FreeGreatPerson,
    /// `[n] population [cityFilter]`.
    GainPopulation { count: i32, cities: CityScope },
    /// `[n] population in a random city`.
    GainPopulationRandomCity { count: i32 },
    /// `Free Technology`, `[n] Free Technologies`.
    FreeTechs { count: i32 },
    /// `Discover [tech]`.
    DiscoverTech(TechId),
    /// `[n] free random researchable Tech(s) from the [eraFilter]`.
    FreeTechsFromEras { count: i32, eras: SetRef },
    /// `Reveals the entire map`.
    RevealEntireMap,
    /// `Gain a free [beliefType] belief`.
    FreeBelief(BeliefKind),
    /// `Triggers voting for the Diplomatic Victory`.
    TriggerVoting,
    /// `Gain [n] [stat]`, `Gain [min]-[max] [stat]`: an amount drawn from `min..=max` (equal for
    /// a fixed gain), scaled by game speed when `speed`.
    GainStat { stat: Stat, min: i32, max: i32, speed: bool },
    /// `Gain enough Faith for a Pantheon`.
    GainPantheon,
    /// `Gain enough Faith for [n]% of a Great Prophet`.
    GainProphet { percent: i32 },
    /// `Research [n]% of [tech]`.
    GainTechPercent { percent: i32, tech: TechId },
    /// `Gain control over [tileFilter] tiles in a [n]-tile radius`.
    TakeOverTilesInRadius { tiles: TileFilterId, radius: i32 },
    /// `Gain control over [n] tiles [cityFilter]`.
    TakeOverTilesInCity { count: i32, cities: CityScope },
    /// `Reveal up to [n/All] [tileFilter] within a [n] tile radius`.
    RevealTiles { count: CountOrAll, tiles: TileFilterId, radius: i32 },
    /// `From a randomly chosen tile [n] tiles away from the ruins, reveal tiles up to [n] tiles
    /// away with [n]% chance`.
    RevealCrudeMap { distance: i32, radius: i32, percent: i32 },
    /// `Gain an extra spy` when entering an era, for every civilization: espionage's.
    GlobalSpiesWhenEnteringEra,
    /// `Promotes all spies [n] time(s)`.
    SpiesLevelUp { times: i32 },
    /// `Gain an extra spy`.
    GainSpy,
    /// `[This Unit] ...`.
    Unit(UnitEffect),
    /// `Gain a free [building] [cityFilter]` (`GainFreeBuildings`), standing and on gain.
    FreeBuilding { building: BuildingId, cities: CityScope },
    /// `Provides the cheapest [stat] building in your first [n] cities for free`.
    FreeStatBuildings { stat: Stat, cities: i32 },
    /// `Provides a [building] in your first [n] cities for free`.
    FreeSpecificBuildings { building: BuildingId, cities: i32 },
    /// `All [mapUnitFilter] units gain the [promotion] promotion`, standing and on gain.
    PromoteUnits { units: UnitFilterId, promotion: PromotionId },
    /// `Allied City-States will occasionally gift Great People`: the first gift comes sooner
    /// (`triggers.py:327-330`).
    CityStateGreatPersonGift,
    /// A timed unique: the civilization holds `variant`, its timer's temporary variant, for
    /// `turns` turns (`triggers.py:88-92`).
    Timed { variant: UniqueId, turns: u16 },
}

impl OneTimeEffect {
    /// How many kinds there are.
    pub const KINDS: usize = 31;

    /// The kind's name, for reports and for tests that every kind decodes.
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::FreeUnits { .. } => "FreeUnits",
            Self::FreePolicies { .. } => "FreePolicies",
            Self::Adopt(_) => "Adopt",
            Self::GoldenAge { .. } => "GoldenAge",
            Self::FreeGreatPerson => "FreeGreatPerson",
            Self::GainPopulation { .. } => "GainPopulation",
            Self::GainPopulationRandomCity { .. } => "GainPopulationRandomCity",
            Self::FreeTechs { .. } => "FreeTechs",
            Self::DiscoverTech(_) => "DiscoverTech",
            Self::FreeTechsFromEras { .. } => "FreeTechsFromEras",
            Self::RevealEntireMap => "RevealEntireMap",
            Self::FreeBelief(_) => "FreeBelief",
            Self::TriggerVoting => "TriggerVoting",
            Self::GainStat { .. } => "GainStat",
            Self::GainPantheon => "GainPantheon",
            Self::GainProphet { .. } => "GainProphet",
            Self::GainTechPercent { .. } => "GainTechPercent",
            Self::TakeOverTilesInRadius { .. } => "TakeOverTilesInRadius",
            Self::TakeOverTilesInCity { .. } => "TakeOverTilesInCity",
            Self::RevealTiles { .. } => "RevealTiles",
            Self::RevealCrudeMap { .. } => "RevealCrudeMap",
            Self::GlobalSpiesWhenEnteringEra => "GlobalSpiesWhenEnteringEra",
            Self::SpiesLevelUp { .. } => "SpiesLevelUp",
            Self::GainSpy => "GainSpy",
            Self::Unit(_) => "Unit",
            Self::FreeBuilding { .. } => "FreeBuilding",
            Self::FreeStatBuildings { .. } => "FreeStatBuildings",
            Self::FreeSpecificBuildings { .. } => "FreeSpecificBuildings",
            Self::PromoteUnits { .. } => "PromoteUnits",
            Self::CityStateGreatPersonGift => "CityStateGreatPersonGift",
            Self::Timed { .. } => "Timed",
        }
    }

    /// What the unique `id` does once, if it does anything once: a one-time unique, a standing
    /// effect that also happens when gained, or a timed unique, which grants its temporary
    /// variant. `None` for any other unique.
    #[must_use]
    #[allow(clippy::too_many_lines, reason = "one arm per one-time type")]
    pub fn decode(rules: &Ruleset, id: UniqueId) -> Option<Self> {
        use UniqueData as D;
        let t = rules.uniques();
        let u = t.get(id);
        let m = t.meta(id);
        if let (Some(turns), Some(variant)) = (m.timed, m.temp_variant) {
            return Some(Self::Timed { variant, turns });
        }
        let scope = |c: CityFilterId| {
            if t.is_this_city(c) { CityScope::ThisCity } else { CityScope::Matching(c) }
        };
        let unit = |e| Some(Self::Unit(e));
        Some(match u.data {
            D::OneTimeFreeUnit(x) => Self::FreeUnits { unit: x.unit, count: 1, near_tile: false },
            D::OneTimeAmountFreeUnits(x) => {
                Self::FreeUnits { unit: x.unit, count: x.count, near_tile: false }
            }
            D::OneTimeFreeUnitRuins(x) => {
                Self::FreeUnits { unit: x.unit, count: 1, near_tile: true }
            }
            D::OneTimeFreePolicy => Self::FreePolicies { count: 1 },
            D::OneTimeAmountFreePolicies(x) => Self::FreePolicies { count: x.count },
            D::OneTimeAdoptPolicyOrBelief(x) => Self::Adopt(x.adopted),
            D::OneTimeEnterGoldenAge => Self::GoldenAge { turns: None },
            D::OneTimeEnterGoldenAgeTurns(x) => Self::GoldenAge { turns: Some(x.turns) },
            D::OneTimeFreeGreatPerson => Self::FreeGreatPerson,
            D::OneTimeGainPopulation(x) => {
                Self::GainPopulation { count: x.count, cities: scope(x.cities) }
            }
            D::OneTimeGainPopulationRandomCity(x) => {
                Self::GainPopulationRandomCity { count: x.count }
            }
            D::OneTimeFreeTech => Self::FreeTechs { count: 1 },
            D::OneTimeAmountFreeTechs(x) => Self::FreeTechs { count: x.count },
            D::OneTimeDiscoverTech(x) => Self::DiscoverTech(x.tech),
            D::OneTimeFreeTechRuins(x) => Self::FreeTechsFromEras { count: x.count, eras: x.eras },
            D::OneTimeRevealEntireMap => Self::RevealEntireMap,
            D::OneTimeFreeBelief(x) => Self::FreeBelief(x.belief),
            D::OneTimeTriggerVoting => Self::TriggerVoting,
            D::OneTimeGainStat(x) => Self::GainStat {
                stat: x.stat,
                min: x.amount,
                max: x.amount,
                speed: u.flags().contains(UFlags::SPEED),
            },
            // Python drew between the two in either order (`triggers.py:204`).
            D::OneTimeGainStatRange(x) => Self::GainStat {
                stat: x.stat,
                min: x.min.min(x.max),
                max: x.min.max(x.max),
                speed: u.flags().contains(UFlags::SPEED),
            },
            D::OneTimeGainPantheon => Self::GainPantheon,
            D::OneTimeGainProphet(x) => Self::GainProphet { percent: x.percent },
            D::OneTimeGainTechPercent(x) => {
                Self::GainTechPercent { percent: x.percent, tech: x.tech }
            }
            D::OneTimeTakeOverTilesInRadius(x) => {
                Self::TakeOverTilesInRadius { tiles: x.tiles, radius: x.radius }
            }
            D::OneTimeTakeOverTilesInCity(x) => {
                Self::TakeOverTilesInCity { count: x.count, cities: scope(x.cities) }
            }
            D::OneTimeRevealSpecificMapTiles(x) => {
                Self::RevealTiles { count: x.count, tiles: x.tiles, radius: x.radius }
            }
            D::OneTimeRevealCrudeMap(x) => {
                Self::RevealCrudeMap { distance: x.distance, radius: x.radius, percent: x.percent }
            }
            D::OneTimeGlobalSpiesWhenEnteringEra => Self::GlobalSpiesWhenEnteringEra,
            D::OneTimeSpiesLevelUp(x) => Self::SpiesLevelUp { times: x.times },
            D::OneTimeGainSpy => Self::GainSpy,
            D::OneTimeUnitHeal(x) => return unit(UnitEffect::Heal(x.hp)),
            D::OneTimeUnitDamage(x) => return unit(UnitEffect::Damage(x.damage)),
            D::OneTimeUnitGainXP(x) => return unit(UnitEffect::GainXp(x.xp)),
            D::OneTimeUnitUpgrade(_) => return unit(UnitEffect::Upgrade),
            D::OneTimeUnitSpecialUpgrade(_) => return unit(UnitEffect::SpecialUpgrade),
            D::OneTimeUnitGainPromotion(x) => return unit(UnitEffect::GainPromotion(x.promotion)),
            D::OneTimeUnitGainMovement(x) => return unit(UnitEffect::Movement(x.movement)),
            D::OneTimeUnitLoseMovement(x) => {
                return unit(UnitEffect::Movement(x.movement.saturating_neg()));
            }
            D::OneTimeUnitDestroyed(_) => return unit(UnitEffect::Destroyed),
            D::GainFreeBuildings(x) => {
                Self::FreeBuilding { building: x.building, cities: scope(x.cities) }
            }
            D::FreeStatBuildings(x) => Self::FreeStatBuildings { stat: x.stat, cities: x.cities },
            D::FreeSpecificBuildings(x) => {
                Self::FreeSpecificBuildings { building: x.building, cities: x.cities }
            }
            D::UnitsGainPromotion(x) => {
                Self::PromoteUnits { units: x.units, promotion: x.promotion }
            }
            D::CityStateCanGiftGreatPeople => Self::CityStateGreatPersonGift,
            _ => return None,
        })
    }
}
