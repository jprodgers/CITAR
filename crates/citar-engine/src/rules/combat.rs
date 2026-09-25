//! The ruleset's texts combat reads (`combat.py:219, 227-228`), resolved once at load.
//!
//! Python compared two parameters with names in every fight: a great general's radius bonus
//! applies to any military unit when its filter is `Military` (`combat.py:219`), whatever the
//! filter would say of the unit, and `Great General provides double combat bonus` doubles the
//! bonus of a general that is a great person of the `War` pool (`combat.py:227-228`). The
//! comparisons happen here, once, and the game reads ids (`game::combat::strength`).

use smallvec::SmallVec;

use super::Ruleset;
use crate::base::ids::UnitFilterId;
use crate::base::sets::BaseUnitSet;
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
        Self { military, war_generals }
    }

    /// Whether a radius bonus's unit filter is the text `Military`, which Python let every unit
    /// through.
    #[must_use]
    pub fn is_military_filter(&self, f: UnitFilterId) -> bool {
        self.military.contains(&f)
    }
}
