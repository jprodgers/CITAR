//! Civilization filters: `civ_matches` (`uniques.py:562-591`), as a tree over [`CivLeaf`].
//!
//! Units, cities and tiles ask the same question of their owner, so their trees embed these leaves
//! ([`super::unit::UnitLeaf::Owner`] and the like).

use super::super::table::{CondDeps, StaticDomain};
use super::super::world::FilterFacts;
use super::expr::{Expr, Leaf};
use super::statics::{Statics, typed};
use crate::base::ids::PlayerId;
use crate::base::sets::NationSet;
use crate::rules::defs::NationKind;

/// One test of a civilization, from a viewer's side where it says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CivLeaf {
    /// `Human player`: a human holds the seat.
    Human,
    /// `AI player`: a bot holds it.
    Ai,
    /// `Major`, `City-States`, `Barbarians`.
    Kind(NationKind),
    /// `Open Borders`: it has open borders with the viewer.
    OpenBorders,
    /// `Friendly`: the viewer itself, or one it counts a friend.
    Friendly,
    /// `Hostile`: at war with the viewer.
    Hostile,
    /// `Known`: the viewer itself, or one it has met.
    Known,
    /// A nation's name, or a tag nations carry.
    Nation(NationSet),
}

impl Leaf for CivLeaf {
    fn constant(&self) -> Option<bool> {
        match self {
            Self::Nation(s) if s.is_empty() => Some(false),
            _ => None,
        }
    }

    fn and(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (Self::Nation(a), Self::Nation(b)) => Some(Self::Nation(*a & *b)),
            // A civilization is of one kind: two different kinds hold nowhere.
            (Self::Kind(a), Self::Kind(b)) if a != b => Some(Self::Nation(NationSet::new())),
            _ => None,
        }
    }

    fn or(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (Self::Nation(a), Self::Nation(b)) => Some(Self::Nation(*a | *b)),
            _ => None,
        }
    }

    /// The seat for the human and AI tests (DESIGN.md 5.7), and the diplomatic state for the
    /// tests against the viewer, which `CondDeps::WAR` stands for.
    fn deps(&self) -> CondDeps {
        match self {
            Self::Human | Self::Ai => CondDeps::SEAT,
            Self::OpenBorders | Self::Friendly | Self::Hostile | Self::Known => CondDeps::WAR,
            Self::Kind(_) | Self::Nation(_) => CondDeps::empty(),
        }
    }
}

impl CivLeaf {
    /// Whether civilization `p` passes, seen by `viewer`. Python's tests against the viewer fail
    /// without one.
    pub fn eval<W: FilterFacts>(&self, w: &W, p: PlayerId, viewer: Option<PlayerId>) -> bool {
        match *self {
            Self::Human => w.civ_is_human(p),
            Self::Ai => !w.civ_is_human(p),
            Self::Kind(k) => w.civ_kind(p) == k,
            Self::OpenBorders => viewer.is_some_and(|v| w.has_open_borders(p, v)),
            Self::Friendly => viewer.is_some_and(|v| v == p || w.is_friend(v, p)),
            Self::Hostile => viewer.is_some_and(|v| w.at_war(v, p)),
            Self::Known => viewer.is_some_and(|v| v == p || w.has_met(v, p)),
            Self::Nation(s) => s.contains(w.civ_nation(p)),
        }
    }
}

/// One term of a civilization filter: the branches of `civ_matches.single` that could match it.
pub(crate) fn term(st: &mut Statics<'_>, s: &str) -> Expr<CivLeaf> {
    Expr::Leaf(match s {
        "All" | "all" => return Expr::Const(true),
        "Human player" => CivLeaf::Human,
        "AI player" => CivLeaf::Ai,
        "Major" => CivLeaf::Kind(NationKind::Major),
        "City-States" | "City-State" => CivLeaf::Kind(NationKind::CityState),
        "Barbarian" | "Barbarians" => CivLeaf::Kind(NationKind::Barbarian),
        "Open Borders" => CivLeaf::OpenBorders,
        "Friendly" => CivLeaf::Friendly,
        "Hostile" => CivLeaf::Hostile,
        "Known" => CivLeaf::Known,
        _ => {
            let nations: NationSet = typed(&st.term(StaticDomain::Nation, s));
            if nations.is_empty() {
                return Expr::Const(false);
            }
            CivLeaf::Nation(nations)
        }
    })
}
