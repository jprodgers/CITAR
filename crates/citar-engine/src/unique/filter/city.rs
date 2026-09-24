//! City filters: `city_matches` (`uniques.py:658-717`), as a tree over [`CityLeaf`].
//!
//! Every word `_city_single` knows becomes its leaf, in both the long form (`in capital`) and the
//! short one (`Capital`); any other term is a test of the city's owner, seen by the viewer, as
//! Python's fall-through to `civ_matches` was. The viewer is the city's owner unless the caller
//! names another (`uniques.py:660-661`).

use super::super::table::CondDeps;
use super::super::world::FilterFacts;
use super::civ::{self, CivLeaf};
use super::expr::{Expr, Leaf};
use super::statics::Statics;
use crate::base::ids::{BuildingId, CityId, PlayerId};
use crate::base::sets::BuildingSet;
use crate::rules::defs::NationKind;

/// One test of a city, from the viewer's side where it says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CityLeaf {
    /// `in your cities`, `Your`: the viewer owns it.
    Yours,
    /// `in all coastal cities`, `Coastal`.
    Coastal,
    /// `in capital`, `Capital`.
    Capital,
    /// `in all non-occupied cities`, `Non-occupied`: no annexation unhappiness, or a puppet.
    NonOccupied,
    /// `in all cities connected to capital`.
    ConnectedToCapital,
    /// `in all cities with a garrison`, `Garrisoned`.
    Garrisoned,
    /// `in all cities in which the majority religion is a major religion`.
    MajorReligion,
    /// `in all cities in which the majority religion is an enhanced religion`.
    EnhancedReligion,
    /// `in non-enemy foreign cities`: another civilization's, not at war with the viewer.
    NonEnemyForeign,
    /// `in enemy cities`, `Enemy`: its owner is at war with the viewer.
    Enemy,
    /// `in foreign cities`, `Foreign`: another civilization's.
    Foreign,
    /// `in annexed cities`, `Annexed`: taken from its founder and not a puppet.
    Annexed,
    /// `in puppeted cities`, `Puppeted`.
    Puppeted,
    /// `in resisting cities`, `Resisting`.
    Resisting,
    /// `in cities being razed`, `Razing`.
    Razing,
    /// `in holy cities`, `Holy`.
    Holy,
    /// `in cities following our religion`: its majority religion is the one the viewer founded.
    FollowsViewersReligion,
    /// It has a building of the set: `in all cities with a world wonder`.
    Has(BuildingSet),
    /// Its owner passes the civilization test, seen by the viewer.
    Owner(CivLeaf),
}

impl Leaf for CityLeaf {
    fn constant(&self) -> Option<bool> {
        match self {
            Self::Has(s) if s.is_empty() => Some(false),
            Self::Owner(c) => c.constant(),
            _ => None,
        }
    }

    fn and(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (Self::Owner(a), Self::Owner(b)) => a.and(b).map(Self::Owner),
            _ => None,
        }
    }

    fn or(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (Self::Has(a), Self::Has(b)) => Some(Self::Has(*a | *b)),
            (Self::Owner(a), Self::Owner(b)) => a.or(b).map(Self::Owner),
            _ => None,
        }
    }

    /// Besides the city itself: the war state for enemy cities; whether it is the capital,
    /// which the city class names (`CITY`: a counted city's is in `CITY_COUNT`, which whatever
    /// counts cities adds); the units in the city for a garrison (`UNIT_SET`); religions' state;
    /// and its owner's trade network for the connection to the capital (`CONNECTED`:
    /// `cities.py:1967-2067`).
    fn deps(&self) -> CondDeps {
        match self {
            Self::Owner(c) => c.deps(),
            Self::NonEnemyForeign | Self::Enemy => CondDeps::WAR,
            Self::Capital => CondDeps::CITY,
            Self::Garrisoned => CondDeps::UNIT_SET,
            Self::ConnectedToCapital => CondDeps::CONNECTED,
            Self::MajorReligion | Self::EnhancedReligion | Self::FollowsViewersReligion => {
                CondDeps::RELIGION_STATE
            }
            _ => CondDeps::empty(),
        }
    }
}

impl CityLeaf {
    /// Whether city `c` passes, seen by `viewer`: the city's owner when `None`.
    pub fn eval<W: FilterFacts>(&self, w: &W, c: CityId, viewer: Option<PlayerId>) -> bool {
        let owner = w.city_owner(c);
        let v = viewer.unwrap_or(owner);
        match *self {
            Self::Yours => v == owner,
            Self::Coastal => w.city_coastal(c),
            Self::Capital => w.city_is_capital(c),
            Self::NonOccupied => !w.city_annex_unhappiness(c) || w.city_puppet(c),
            Self::ConnectedToCapital => w.city_connected_to_capital(c),
            Self::Garrisoned => w.city_garrisoned(c),
            Self::MajorReligion => {
                w.city_majority_religion(c).is_some_and(|r| w.religion_is_major(r))
            }
            Self::EnhancedReligion => {
                w.city_majority_religion(c).is_some_and(|r| w.religion_is_enhanced(r))
            }
            Self::NonEnemyForeign => v != owner && !w.at_war(owner, v),
            Self::Enemy => w.at_war(owner, v),
            Self::Foreign => v != owner,
            Self::Annexed => w.city_founder(c) != owner && !w.city_puppet(c),
            Self::Puppeted => w.city_puppet(c),
            Self::Resisting => w.city_resisting(c),
            Self::Razing => w.city_razing(c),
            Self::Holy => w.city_holy(c),
            Self::FollowsViewersReligion => {
                w.city_majority_religion(c).is_some_and(|r| w.civ_religion(v) == Some(r))
            }
            Self::Has(s) => !s.is_disjoint(&w.city_buildings(c)),
            Self::Owner(leaf) => leaf.eval(w, owner, Some(v)),
        }
    }
}

/// One term of a city filter.
pub(crate) fn term(st: &mut Statics<'_>, s: &str) -> Expr<CityLeaf> {
    Expr::Leaf(match s {
        "in this city" | "in all cities" | "All" | "all" => return Expr::Const(true),
        // Python answered yes: a religion's uniques reach only the cities that follow it, and
        // that scoping is the follower index's (DESIGN.md 5.12), not the filter's.
        "in cities following this religion" => return Expr::Const(true),
        "in your cities" | "Your" => CityLeaf::Yours,
        "in all coastal cities" | "Coastal" => CityLeaf::Coastal,
        "in capital" | "Capital" => CityLeaf::Capital,
        "in all non-occupied cities" | "Non-occupied" => CityLeaf::NonOccupied,
        "in all cities with a world wonder" => {
            let r = st.rules();
            let wonders: BuildingSet = r
                .buildings()
                .iter()
                .filter(|(_, b)| b.is_wonder)
                .map(|(id, _): (BuildingId, _)| id)
                .collect();
            CityLeaf::Has(wonders)
        }
        "in all cities connected to capital" => CityLeaf::ConnectedToCapital,
        "in all cities with a garrison" | "Garrisoned" => CityLeaf::Garrisoned,
        "in all cities in which the majority religion is a major religion" => {
            CityLeaf::MajorReligion
        }
        "in all cities in which the majority religion is an enhanced religion" => {
            CityLeaf::EnhancedReligion
        }
        "in non-enemy foreign cities" => CityLeaf::NonEnemyForeign,
        "in enemy cities" | "Enemy" => CityLeaf::Enemy,
        "in foreign cities" | "Foreign" => CityLeaf::Foreign,
        "in annexed cities" | "Annexed" => CityLeaf::Annexed,
        "in puppeted cities" | "Puppeted" => CityLeaf::Puppeted,
        "in resisting cities" | "Resisting" => CityLeaf::Resisting,
        "in cities being razed" | "Razing" => CityLeaf::Razing,
        "in holy cities" | "Holy" => CityLeaf::Holy,
        "in City-State cities" => CityLeaf::Owner(CivLeaf::Kind(NationKind::CityState)),
        "in cities following our religion" => CityLeaf::FollowsViewersReligion,
        _ => return civ::term(st, s).map(&mut |c| CityLeaf::Owner(*c)).fold(),
    })
    .fold()
}
