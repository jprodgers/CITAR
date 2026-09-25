//! The ruleset's names and uniques the production advisor reads (`bots/basic.py:642-655,
//! 846-864, 1113-1590`), resolved once at load.
//!
//! Python compared unit type names (`Scout`, `Siege`, `Mounted`, `Armored`) and a victory's
//! (`Scientific`) at every choice, and looked each unit's and building's uniques up by type;
//! here the comparisons happen once, and the game reads ids and sets (`game::advisor`).

use super::Ruleset;
use crate::base::ids::{BuildingId, IdVec, UnitTypeId, VictoryId};
use crate::base::sets::{BaseUnitSet, BuildingSet, ResourceSet};
use crate::unique::index::{Extra, building_extra};
use crate::unique::{SourceUniques, UniqueData, UniqueTable, UniqueType};

const SCOUT: &str = "Scout";
const SIEGE: &str = "Siege";
const MOUNTED: &str = "Mounted";
const ARMORED: &str = "Armored";
const SCIENTIFIC: &str = "Scientific";

/// What the production advisor reads of the ruleset.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdvisorRules {
    /// The unit type of scouts (`_is_recon`, `basic.py:642-644`).
    pub scout: Option<UnitTypeId>,
    /// The unit type of siege units (`basic.py:1328, 1563-1566`).
    pub siege: Option<UnitTypeId>,
    /// The unit types that defend less well than they attack (`basic.py:1567`).
    pub mounted: Option<UnitTypeId>,
    pub armored: Option<UnitTypeId>,
    /// The scientific victory, which the spaceship's resources are kept for
    /// (`_breaks_space_reserve`, `basic.py:846-863`); a ruleset without it counts it enabled, as
    /// Python's `victory_enabled` did.
    pub scientific: Option<VictoryId>,
    /// The units that found cities (`FoundCity`, their unit type's uniques included, as
    /// Python's unit map held them).
    pub founders: BaseUnitSet,
    /// The units that build improvements (`BuildImprovements`).
    pub workers: BaseUnitSet,
    /// The units that build improvements on water (`CreateWaterImprovements`).
    pub boats: BaseUnitSet,
    /// The units no army is built of (`NuclearWeapon`, `SelfDestructs`).
    pub not_army: BaseUnitSet,
    /// The spaceship's units: parts and what is added in the capital (`SpaceshipPart`,
    /// `AddInCapital`).
    pub space: BaseUnitSet,
    /// The spaceship's parts alone (`SpaceshipPart`).
    pub parts: BaseUnitSet,
    /// The resources the spaceship's parts need.
    pub space_resources: ResourceSet,
    /// The buildings that win the game (`Triggers a Cultural Victory upon completion`,
    /// `Triggers victory`).
    pub victory: BuildingSet,
    /// The buildings that open the spaceship's construction (`Enables construction of Spaceship
    /// parts`).
    pub space_program: BuildingSet,
    /// Each building's `[n]% Food is carried over after population increases`, in its order.
    pub carry_over: IdVec<BuildingId, Box<[i32]>>,
    /// What one more copy of each building adds to its city's own index, and to its owner's
    /// (`unique::index::building_extra`): what the what-if of a building adds
    /// (`cities::what_if`), gathered once.
    pub adds_local: IdVec<BuildingId, Extra>,
    pub adds_civ: IdVec<BuildingId, Extra>,
}

/// Whether a source has a unique of type `ty`, whatever its conditionals (`_umap.get`).
fn has(t: &UniqueTable, s: &SourceUniques, ty: UniqueType) -> bool {
    s.ids().any(|id| t.meta(id).ty == Some(ty))
}

impl AdvisorRules {
    /// The advisor's names and uniques of `r`, whose uniques are compiled.
    pub(crate) fn new(r: &Ruleset) -> Self {
        let t = r.uniques();
        let unit_type =
            |name: &str| r.unit_types().iter().find(|(_, u)| &*u.name == name).map(|(id, _)| id);
        let mut out = Self {
            scout: unit_type(SCOUT),
            siege: unit_type(SIEGE),
            mounted: unit_type(MOUNTED),
            armored: unit_type(ARMORED),
            scientific: r
                .victories()
                .iter()
                .find(|(_, v)| &*v.name == SCIENTIFIC)
                .map(|(id, _)| id),
            ..Self::default()
        };
        for (id, u) in r.base_units().iter() {
            let kind = &r.unit_types()[u.unit_type].uniques;
            let unit_has = |ty| has(t, &u.uniques, ty) || has(t, kind, ty);
            if unit_has(UniqueType::FoundCity) {
                out.founders.insert(id);
            }
            if unit_has(UniqueType::BuildImprovements) {
                out.workers.insert(id);
            }
            if unit_has(UniqueType::CreateWaterImprovements) {
                out.boats.insert(id);
            }
            if unit_has(UniqueType::NuclearWeapon) || unit_has(UniqueType::SelfDestructs) {
                out.not_army.insert(id);
            }
            let part = unit_has(UniqueType::SpaceshipPart);
            if part {
                out.parts.insert(id);
                if let Some(res) = u.required_resource {
                    out.space_resources.insert(res);
                }
            }
            if part || unit_has(UniqueType::AddInCapital) {
                out.space.insert(id);
            }
        }
        let mut carry: Vec<Box<[i32]>> = Vec::with_capacity(r.buildings().len());
        for (id, b) in r.buildings().iter() {
            if has(t, &b.uniques, UniqueType::TriggersCulturalVictory)
                || has(t, &b.uniques, UniqueType::TriggersVictory)
            {
                out.victory.insert(id);
            }
            if has(t, &b.uniques, UniqueType::EnablesConstructionOfSpaceshipParts) {
                out.space_program.insert(id);
            }
            carry.push(
                b.uniques
                    .ids()
                    .filter_map(|u| match t.get(u).data {
                        UniqueData::CarryOverFood(x) => Some(x.percent),
                        _ => None,
                    })
                    .collect(),
            );
        }
        out.carry_over = IdVec::from_vec(carry);
        out.adds_local = r.buildings().ids().map(|b| building_extra(r, b, true)).collect();
        out.adds_civ = r.buildings().ids().map(|b| building_extra(r, b, false)).collect();
        out
    }
}
