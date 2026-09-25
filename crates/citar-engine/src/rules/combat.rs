//! The ruleset's texts combat reads (`combat.py:219, 227-228`), resolved once at load, and the
//! objects that may carry the unique types a fight looks for on units other than its own.
//!
//! Python compared two parameters with names in every fight: a great general's radius bonus
//! applies to any military unit when its filter is `Military` (`combat.py:219`), whatever the
//! filter would say of the unit, and `Great General provides double combat bonus` doubles the
//! bonus of a general that is a great person of the `War` pool (`combat.py:227-228`). The
//! comparisons happen here, once, and the game reads ids (`game::combat::strength`).

use smallvec::SmallVec;

use super::Ruleset;
use crate::base::ids::{BaseUnitId, UniqueId, UnitFilterId};
use crate::base::sets::{BaseUnitSet, PromotionSet};
use crate::unique::{UniqueData, UniqueType};

/// What combat reads of the ruleset's texts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CombatRules {
    /// The filters of `[n]% Strength bonus for [Military] units within [n] tiles` whose text is
    /// `Military`: the bonus applies without asking the filter (`combat.py:219`).
    military: SmallVec<[UnitFilterId; 1]>,
    /// The base units that are great people of the `War` pool, with their unit type's uniques
    /// (`Great Person - [War]`, whatever its conditionals, `combat.py:228`).
    pub war_generals: BaseUnitSet,
    /// What may carry `[n]% Strength bonus for [units] units within [n] tiles`: a great general's
    /// aura is looked for only on units that could have one, where Python asked every unit of
    /// the side.
    pub aura: Carriers,
    /// What may carry `[n]% Strength for enemy [units] units in adjacent [tiles] tiles`, looked
    /// for only on the enemies beside a fighter that could have it.
    pub adjacent: Carriers,
}

/// The base units (with their unit type's uniques) and the promotions that carry a unique type,
/// whatever its conditionals.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Carriers {
    pub units: BaseUnitSet,
    pub promotions: PromotionSet,
}

impl Carriers {
    /// The carriers of `ty` in `r`.
    fn of(r: &Ruleset, ty: UniqueType) -> Self {
        let t = r.uniques();
        let has = |u: UniqueId| t.meta(u).ty == Some(ty);
        let mut out = Self::default();
        for (id, b) in r.base_units().iter() {
            if b.uniques.ids().chain(r.unit_types()[b.unit_type].uniques.ids()).any(has) {
                out.units.insert(id);
            }
        }
        for (id, p) in r.promotions().iter() {
            if p.uniques.ids().any(has) {
                out.promotions.insert(id);
            }
        }
        out
    }

    /// Whether a unit of `base` with `promotions` may carry the type.
    #[must_use]
    pub fn may(&self, base: BaseUnitId, promotions: &PromotionSet) -> bool {
        self.units.contains(base) || !self.promotions.is_disjoint(promotions)
    }
}

impl CombatRules {
    /// The combat texts of `r`, whose uniques are compiled.
    pub(crate) fn new(r: &Ruleset) -> Self {
        let t = r.uniques();
        let mut military = SmallVec::new();
        for (_, u) in t.iter() {
            if let UniqueData::StrengthBonusInRadius(x) = u.data
                && t.unit_filter(x.units) == "Military"
                && !military.contains(&x.units)
            {
                military.push(x.units);
            }
        }
        let mut war_generals = BaseUnitSet::new();
        for (id, b) in r.base_units().iter() {
            let mut ids = b.uniques.ids().chain(r.unit_types()[b.unit_type].uniques.ids());
            let war = ids.any(|u| {
                t.meta(u).ty == Some(UniqueType::GreatPerson)
                    && matches!(t.get(u).data, UniqueData::GreatPerson(x) if t.text(x.pool) == "War")
            });
            if war {
                war_generals.insert(id);
            }
        }
        Self {
            military,
            war_generals,
            aura: Carriers::of(r, UniqueType::StrengthBonusInRadius),
            adjacent: Carriers::of(r, UniqueType::StrengthForAdjacentEnemies),
        }
    }

    /// Whether a radius bonus's unit filter is the text `Military`, which Python let every unit
    /// through.
    #[must_use]
    pub fn is_military_filter(&self, f: UnitFilterId) -> bool {
        self.military.contains(&f)
    }
}
