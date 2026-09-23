//! Unit filters: `unit_matches` (`uniques.py:528-559`), as a tree over [`UnitLeaf`].
//!
//! A term becomes the branches of `_unit_single` that could ever match it. The words Python
//! answered first return alone (`other`, `Wounded`, `Embarked`, ...); any other term is the
//! disjunction of the unit's base unit (a static set), its owner (a civilization filter), its
//! promotions (a static set) and its `Set Up` status.

use super::super::table::{CondDeps, StaticDomain};
use super::super::world::FilterFacts;
use super::civ::{self, CivLeaf};
use super::expr::{Expr, Leaf};
use super::statics::{Statics, typed};
use crate::base::ids::{PlayerId, UnitId};
use crate::base::sets::{BaseUnitSet, PromotionSet};
use crate::rules::defs::NationKind;

/// One test of a unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnitLeaf {
    /// `other`: not the unit whose unique is being evaluated.
    Other,
    /// `Wounded`: it has lost health.
    Wounded,
    /// `Embarked`.
    Embarked,
    /// `Set Up`: the one status a unit carries (`combat.py:763-764`).
    SetUp,
    /// Its row in `units.json` is in the set: its name, type, era, role or tags.
    Base(BaseUnitSet),
    /// It has a promotion of the set: by name, or a tag the promotion carries.
    Promotion(PromotionSet),
    /// Its owner passes the civilization test, seen by the viewer.
    Owner(CivLeaf),
}

impl Leaf for UnitLeaf {
    fn constant(&self) -> Option<bool> {
        match self {
            Self::Base(s) if s.is_empty() => Some(false),
            Self::Promotion(s) if s.is_empty() => Some(false),
            Self::Owner(c) => c.constant(),
            _ => None,
        }
    }

    fn and(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            // A unit has one base unit, so both sets must hold it.
            (Self::Base(a), Self::Base(b)) => Some(Self::Base(*a & *b)),
            (Self::Owner(a), Self::Owner(b)) => a.and(b).map(Self::Owner),
            _ => None,
        }
    }

    fn or(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (Self::Base(a), Self::Base(b)) => Some(Self::Base(*a | *b)),
            (Self::Promotion(a), Self::Promotion(b)) => Some(Self::Promotion(*a | *b)),
            (Self::Owner(a), Self::Owner(b)) => a.or(b).map(Self::Owner),
            _ => None,
        }
    }

    fn deps(&self) -> CondDeps {
        match self {
            Self::Owner(c) => c.deps(),
            _ => CondDeps::empty(),
        }
    }
}

/// Who asks about a unit: the unit whose unique is being evaluated (for `other`), and the
/// civilization the owner tests are seen by. Python passed its `Ctx`, or nothing (`uniques.py:528,
/// 552`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct UnitScope {
    /// The unit in context, if any.
    pub this: Option<UnitId>,
    /// The civilization in context, if any.
    pub viewer: Option<PlayerId>,
}

impl UnitLeaf {
    /// Whether unit `u` passes.
    pub fn eval<W: FilterFacts>(&self, w: &W, u: UnitId, scope: UnitScope) -> bool {
        match self {
            Self::Other => scope.this != Some(u),
            Self::Wounded => w.unit_wounded(u),
            Self::Embarked => w.unit_embarked(u),
            Self::SetUp => w.unit_set_up(u),
            Self::Base(s) => s.contains(w.unit_base(u)),
            Self::Promotion(s) => !s.is_disjoint(&w.unit_promotions(u)),
            Self::Owner(c) => c.eval(w, w.unit_owner(u), scope.viewer),
        }
    }
}

/// One term of a unit filter.
pub(crate) fn term(st: &mut Statics<'_>, s: &str) -> Expr<UnitLeaf> {
    let owner = |k| Expr::Leaf(UnitLeaf::Owner(CivLeaf::Kind(k)));
    match s {
        "other" => Expr::Leaf(UnitLeaf::Other),
        "Wounded" | "wounded units" => Expr::Leaf(UnitLeaf::Wounded),
        "Barbarians" | "Barbarian" => owner(NationKind::Barbarian),
        "City-State" => owner(NationKind::CityState),
        "Embarked" => Expr::Leaf(UnitLeaf::Embarked),
        "Non-City" => Expr::Const(true),
        _ => {
            let base: BaseUnitSet = typed(&st.term(StaticDomain::BaseUnit, s));
            let everything = base.len() == st.rules().base_units().len();
            if everything && !base.is_empty() {
                return Expr::Const(true);
            }
            let promotions: PromotionSet = typed(&st.term(StaticDomain::Promotion, s));
            let mut any = vec![
                Expr::Leaf(UnitLeaf::Base(base)),
                civ::term(st, s).map(&mut |c| UnitLeaf::Owner(*c)),
                Expr::Leaf(UnitLeaf::Promotion(promotions)),
            ];
            if s == "Set Up" {
                any.push(Expr::Leaf(UnitLeaf::SetUp));
            }
            Expr::Any(any.into()).fold()
        }
    }
}
