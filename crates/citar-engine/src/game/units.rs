//! Units: their uniques (`units.py`).
//!
//! Package 1b-05 ports `units.py:21-59`: the uniques that apply to a unit, which are its
//! profile's (its base unit's, its unit type's and its promotions', one shared index per
//! combination of base unit and promotions, DESIGN.md 5.12), with or without its owner's; the
//! context they are asked in; and which units garrison a city. Python cached each profile's map
//! by its type and sorted promotions (`g._static`); here the profile table in
//! `game::derive::civ` does, and never drops one. Package 1c-02 ports the rest of the file.

use super::Game;
use crate::base::ids::{UniqueId, UnitId};
use crate::rules::defs::Domain;
use crate::unique::{Ctx, UniqueType, uq};

/// The context a unique is asked about a unit in (`units.unit_ctx`, `units.py:36-38`): its
/// owner's, on its tile.
#[must_use]
pub fn unit_ctx(g: &Game, u: UnitId) -> Ctx {
    Ctx::unit(&g.view(), u)
}

/// The unit's uniques of type `ty` that hold in its context, and with `with_civ` its owner's
/// after them (`units.unit_uniques`, `units.py:41-48`), with their copies.
#[must_use]
pub fn unit_uniques(g: &Game, u: UnitId, ty: UniqueType, with_civ: bool) -> Vec<(UniqueId, u16)> {
    let v = g.view();
    let ctx = Ctx::unit(&v, u);
    if with_civ {
        uq::unit_and_civ(&v, u, ty, &ctx).map(|h| (h.id, h.n)).collect()
    } else {
        uq::unit(&v, u, ty, &ctx).map(|h| (h.id, h.n)).collect()
    }
}

/// Whether any unique of type `ty` holds for the unit, or with `with_civ` for it or its owner
/// (`units.unit_has`, `units.py:51-53`).
#[must_use]
pub fn unit_has(g: &Game, u: UnitId, ty: UniqueType, with_civ: bool) -> bool {
    let v = g.view();
    let ctx = Ctx::unit(&v, u);
    if with_civ {
        uq::any(uq::unit_and_civ(&v, u, ty, &ctx))
    } else {
        uq::any(uq::unit(&v, u, ty, &ctx))
    }
}

/// Whether a unit counts as a city's garrison, for its strength and its happiness
/// (`units.can_garrison`, `units.py:56-59`): a military land unit.
#[must_use]
pub fn can_garrison(g: &Game, u: UnitId) -> bool {
    g.unit(u).is_some_and(|x| {
        let d = &g.rules.base_units()[x.base];
        d.military && d.domain == Domain::Land
    })
}
