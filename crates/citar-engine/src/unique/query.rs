//! The queries (DESIGN.md 5.11): which uniques of a type hold for a civilization, a city, a unit,
//! a tile's terrains or one object, in a context. Rule code reads uniques through these, as
//! `uq::civ(w, p, UniqueType::Strength, &ctx)`.
//!
//! Ports `civ_uniques` and `civ_has` (`economy.py:132-160`), `cities.city_uniques` and
//! `local_uniques` (`cities.py:69-89`), `units.unit_uniques` (`units.py:41-48`), and
//! `UniqueMap.matching` and `has` (`uniques.py:190-209`). Each query walks one or more unique
//! indexes ([`super::index`]) at the type's run, in Python's order, and yields each unique whose
//! conditionals hold ([`super::cond::applies`]) with the number of copies the index holds, so a
//! sum over five Monuments evaluates the Monument's conditionals once. A standing query never
//! meets a triggered unique, which an index holds at its trigger's type, nor a timed one, which is
//! granted as its temporary variant, as `matching` left timed uniques out.
//!
//! A query borrows the indexes it walks for as long as it runs: a production world's memos are
//! validated before they are lent (DESIGN.md 6.3), and nothing can write the game meanwhile.

use core::ops::Range;

use smallvec::SmallVec;

use super::cond::{Problem, ProblemKind, applies, holds};
use super::generated::{UniqueData, UniqueType};
use super::table::{SourceUniques, UFlags, Unique};
use super::world::{Ctx, EvalWorld, IndexLayer, IndexRef};
use crate::base::ids::{CityId, PlayerId, TileIdx, UniqueId, UnitId};

/// One unique a query found, with the number of copies its index holds.
#[derive(Clone, Copy, Debug)]
pub struct Hit<'w> {
    pub id: UniqueId,
    pub unique: &'w Unique,
    /// How many copies: the cities with the building that carries it, the grants of a timed
    /// unique. 1 for an object's own uniques.
    pub n: u16,
}

impl Hit<'_> {
    /// What the unique does.
    #[must_use]
    pub fn data(&self) -> &UniqueData {
        &self.unique.data
    }
}

/// Where a query looks.
enum Layer<'w> {
    /// An index's run of the type.
    Index(IndexRef<'w>),
    /// Every unique of one object, in the order it lists them.
    Object(Range<u16>),
}

/// The uniques of one type that hold in a context, walked lazily: an [`Iterator`] of [`Hit`]s.
pub struct Hits<'w, W: EvalWorld> {
    w: &'w W,
    ty: UniqueType,
    ctx: Ctx,
    layers: SmallVec<[Layer<'w>; 4]>,
    layer: usize,
    pos: usize,
}

impl<'w, W: EvalWorld> Hits<'w, W> {
    fn new(w: &'w W, ty: UniqueType, ctx: &Ctx) -> Self {
        Self { w, ty, ctx: *ctx, layers: SmallVec::new(), layer: 0, pos: 0 }
    }

    fn index(mut self, ix: IndexRef<'w>) -> Self {
        self.layers.push(Layer::Index(ix));
        self
    }

    fn object(mut self, s: &SourceUniques) -> Self {
        self.layers.push(Layer::Object(s.all.clone()));
        self
    }

    /// The next candidate of the current layer, or `None` at its end.
    fn candidate(&mut self) -> Option<(UniqueId, u16)> {
        let t = self.w.rules().uniques();
        match &self.layers[self.layer] {
            Layer::Index(ix) => {
                let e = ix.get(self.ty).get(self.pos).copied()?;
                self.pos += 1;
                Some((e.id, e.n))
            }
            Layer::Object(range) => {
                let (start, end) = (usize::from(range.start), usize::from(range.end));
                while start + self.pos < end {
                    let id = UniqueId(u16::try_from(start + self.pos).unwrap_or(u16::MAX));
                    self.pos += 1;
                    let standing = !t.get(id).flags().intersects(UFlags::TIMED | UFlags::TRIGGERED);
                    if standing && t.meta(id).ty == Some(self.ty) {
                        return Some((id, 1));
                    }
                }
                None
            }
        }
    }
}

impl<'w, W: EvalWorld> Iterator for Hits<'w, W> {
    type Item = Hit<'w>;

    fn next(&mut self) -> Option<Hit<'w>> {
        let t = self.w.rules().uniques();
        while self.layer < self.layers.len() {
            match self.candidate() {
                Some((id, n)) => {
                    if applies(id, &self.ctx, self.w) {
                        return Some(Hit { id, unique: t.get(id), n });
                    }
                }
                None => {
                    self.layer += 1;
                    self.pos = 0;
                }
            }
        }
        None
    }
}

/// The civilization's uniques of type `ty` that hold in `ctx`, the resource layer included
/// (`civ_uniques`).
pub fn civ<'w, W: EvalWorld>(w: &'w W, p: PlayerId, ty: UniqueType, ctx: &Ctx) -> Hits<'w, W> {
    Hits::new(w, ty, ctx).index(w.civ_index(p, IndexLayer::Full))
}

/// The civilization's uniques of type `ty` that hold in `ctx`, without the resource layer: what
/// the resource supply reads (`_civ_uniques_nores`, DESIGN.md 6.6).
pub fn civ_no_resources<'w, W: EvalWorld>(
    w: &'w W,
    p: PlayerId,
    ty: UniqueType,
    ctx: &Ctx,
) -> Hits<'w, W> {
    Hits::new(w, ty, ctx).index(w.civ_index(p, IndexLayer::NoResources))
}

/// The uniques of type `ty` that hold in a city (`cities.city_uniques`): its local ones, its
/// majority religion's follower beliefs', then its owner's (DESIGN.md 5.12).
pub fn city<'w, W: EvalWorld>(w: &'w W, c: CityId, ty: UniqueType, ctx: &Ctx) -> Hits<'w, W> {
    let mut hits = Hits::new(w, ty, ctx).index(w.city_local(c));
    if let Some(r) = w.city_majority_religion(c) {
        hits = hits.index(w.follower(r));
    }
    hits.index(w.civ_index(w.city_owner(c), IndexLayer::Full))
}

/// The uniques of type `ty` that hold in a city alone (`cities.local_uniques`,
/// `cities.py:69-78`): its local ones, then its majority religion's follower beliefs', without
/// its owner's.
pub fn local<'w, W: EvalWorld>(w: &'w W, c: CityId, ty: UniqueType, ctx: &Ctx) -> Hits<'w, W> {
    let hits = Hits::new(w, ty, ctx).index(w.city_local(c));
    match w.city_majority_religion(c) {
        Some(r) => hits.index(w.follower(r)),
        None => hits,
    }
}

/// The uniques of type `ty` of a unit's profile that hold (`units.unit_uniques`).
pub fn unit<'w, W: EvalWorld>(w: &'w W, u: UnitId, ty: UniqueType, ctx: &Ctx) -> Hits<'w, W> {
    Hits::new(w, ty, ctx).index(w.unit_index(u))
}

/// The unit's uniques of type `ty` that hold, then its owner's (`unit_uniques(with_civ=True)`).
pub fn unit_and_civ<'w, W: EvalWorld>(
    w: &'w W,
    u: UnitId,
    ty: UniqueType,
    ctx: &Ctx,
) -> Hits<'w, W> {
    Hits::new(w, ty, ctx)
        .index(w.unit_index(u))
        .index(w.civ_index(w.unit_owner(u), IndexLayer::Full))
}

/// The uniques of type `ty` of the tile's terrains (base, features, natural wonder) that hold.
pub fn terrains<'w, W: EvalWorld>(w: &'w W, t: TileIdx, ty: UniqueType, ctx: &Ctx) -> Hits<'w, W> {
    let r = w.rules();
    let mut hits = Hits::new(w, ty, ctx);
    for terrain in w.tile_terrains(t).iter() {
        hits = hits.object(&r.terrains()[terrain].uniques);
    }
    hits
}

/// The uniques of type `ty` of one object that hold (`UniqueMap.matching` on one object's map):
/// a building's, an improvement's, a unit type's.
pub fn object<'w, W: EvalWorld>(
    w: &'w W,
    uniques: &SourceUniques,
    ty: UniqueType,
    ctx: &Ctx,
) -> Hits<'w, W> {
    Hits::new(w, ty, ctx).object(uniques)
}

/// The civilization's uniques of type `ty`, their conditionals not evaluated (`civ_index(...)
/// .get(ph)`).
pub fn raw<W: EvalWorld>(w: &W, p: PlayerId, ty: UniqueType) -> Hits<'_, W> {
    civ(w, p, ty, &Ctx::IGNORE)
}

/// Whether a query found anything, stopping at the first (`civ_has`, `UniqueMap.has`).
pub fn any<'w>(mut hits: impl Iterator<Item = Hit<'w>>) -> bool {
    hits.next().is_some()
}

/// The sum of `f` over what a query found, each counted as many times as its copies; a unique for
/// which `f` gives `None` adds nothing. Saturating.
pub fn sum_i32<'w>(
    hits: impl Iterator<Item = Hit<'w>>,
    mut f: impl FnMut(&UniqueData) -> Option<i32>,
) -> i32 {
    hits.fold(0i32, |acc, h| match f(&h.unique.data) {
        Some(v) => acc.saturating_add(v.saturating_mul(i32::from(h.n))),
        None => acc,
    })
}

/// Why the requirements among `uniques` are not met for civilization `p` in `ctx`
/// (`cities.rejection_reasons`' `Only available` and `Can only be built` branches,
/// `cities.py:1222-1226` and `1297-1300`): one [`Problem`] per conditional that fails, in order.
/// `Can only be built` gives its whole text; the rest give [`super::table::Cond::describe`]'s.
pub fn requirement_problems<W: EvalWorld>(
    w: &W,
    uniques: impl IntoIterator<Item = UniqueId>,
    ctx: &Ctx,
    p: PlayerId,
) -> Vec<Problem> {
    let r = w.rules();
    let t = r.uniques();
    let nation = w.civ_nation(p);
    let mut out = Vec::new();
    for id in uniques {
        let built_variant = match t.meta(id).ty {
            Some(UniqueType::OnlyAvailable) => false,
            Some(UniqueType::CanOnlyBeBuiltWhen) => true,
            _ => continue,
        };
        // Its conditionals are read here rather than through `applies`: a memo recording what it
        // read must see them all the same.
        super::record::note(t, id);
        for c in t.conds(t.get(id)) {
            if holds(c, id, ctx, w) {
                continue;
            }
            let problem = c.describe(r, nation);
            out.push(if built_variant && problem.kind == ProblemKind::ShouldNotBeDisplayed {
                Problem {
                    kind: ProblemKind::CanOnlyBeBuiltInSpecificCities,
                    text: t.text_of(id).to_owned(),
                }
            } else {
                problem
            });
        }
    }
    out
}
