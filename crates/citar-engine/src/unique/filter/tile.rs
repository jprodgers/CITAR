//! Tile filters: `tile_matches` and `tile_terrain_matches` (`uniques.py:391-474`), as trees over
//! [`TileLeaf`].
//!
//! A term has two parts, as `_tile_single` had: what the tile's terrain says
//! (`_tile_terrain_single`), and what stands on it (the improvement lines). A full tile filter asks
//! both; a tile-terrain filter only the first. The words Python answered first become their leaf
//! alone; any other term is the disjunction of the tile's terrains (the static terrain set), its
//! resource (as the viewer sees it) and its owner (a civilization filter).
//!
//! Several words are sets of terrains, so that they need nothing but the tile's terrains:
//! `Water` and `Land` are the water base terrains or not, `Featureless` no feature, `Open terrain`
//! no rough terrain, and `Elevated` a terrain that map generation raises (`Occurs in chains at
//! high elevations` or `Occurs in groups around high elevations`: Mountain and Hill), where
//! Python compared the names Mountain and Hill (`uniques.py:458-459`) and map generation the
//! uniques (`mapgen.py:431-437, 478-479`).
//!
//! Map generation reads tile filters too, through [`TileFacts`] alone; a filter it reads may use
//! only the terrain-level leaves ([`TileLeaf::is_terrain_level`]).

use super::super::UniqueType;
use super::super::table::{CondDeps, StaticDomain};
use super::super::world::{FilterFacts, TileFacts};
use super::civ::{self, CivLeaf};
use super::expr::{Expr, Leaf};
use super::statics::{Statics, typed};
use crate::base::ids::{PlayerId, TerrainId, TileIdx};
use crate::base::sets::{ImprovementSet, ResourceSet, TerrainSet};
use crate::rules::Ruleset;
use crate::rules::defs::TerrainType;

/// One test of a tile, from the viewer's side where it says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TileLeaf {
    /// One of the tile's terrains (base, features, natural wonder) is in the set.
    Terrains(TerrainSet),
    /// `River`.
    River,
    /// `Fresh water`, `Fresh Water`.
    FreshWater,
    /// A neighbour is coast: with land, `Coastal`.
    NextToCoast,
    /// `Unowned`.
    Unowned,
    /// `your`: the viewer owns it.
    Yours,
    /// `Foreign Land`, `Foreign`: not friendly territory to the viewer.
    ForeignLand,
    /// `Friendly Land`, `Friendly`: friendly territory to the viewer.
    FriendlyLand,
    /// `Enemy Land`, `Enemy`: owned by a civilization at war with the viewer.
    EnemyLand,
    /// Its owner passes the civilization test, seen by the viewer.
    Owner(CivLeaf),
    /// `resource`: it has a resource the viewer can see.
    AnyResource,
    /// Its resource is in the set, and the viewer (if any) can see it.
    Resource(ResourceSet),
    /// `unimproved`: no improvement, or a pillaged one.
    Unimproved,
    /// `improved`: an improvement that is not pillaged.
    Improved,
    /// `pillaged`: its improvement or its route is pillaged.
    Pillaged,
    /// `worked`: a city works it.
    Worked,
    /// Its improvement or its route, where not pillaged, is in the set.
    Improvement(ImprovementSet),
}

impl Leaf for TileLeaf {
    fn constant(&self) -> Option<bool> {
        match self {
            Self::Terrains(s) if s.is_empty() => Some(false),
            Self::Resource(s) if s.is_empty() => Some(false),
            Self::Improvement(s) if s.is_empty() => Some(false),
            Self::Owner(c) => c.constant(),
            _ => None,
        }
    }

    fn and(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            // A tile has one resource, so both sets must hold it; the viewer test is the same.
            (Self::Resource(a), Self::Resource(b)) => Some(Self::Resource(*a & *b)),
            (Self::Owner(a), Self::Owner(b)) => a.and(b).map(Self::Owner),
            _ => None,
        }
    }

    fn or(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (Self::Terrains(a), Self::Terrains(b)) => Some(Self::Terrains(*a | *b)),
            (Self::Resource(a), Self::Resource(b)) => Some(Self::Resource(*a | *b)),
            (Self::Improvement(a), Self::Improvement(b)) => Some(Self::Improvement(*a | *b)),
            (Self::Owner(a), Self::Owner(b)) => a.or(b).map(Self::Owner),
            _ => None,
        }
    }

    /// The viewer's techs for its resources, the diplomatic state for friend and enemy land.
    fn deps(&self) -> CondDeps {
        match self {
            Self::Owner(c) => c.deps(),
            Self::AnyResource | Self::Resource(_) => CondDeps::TECHS,
            Self::ForeignLand | Self::FriendlyLand | Self::EnemyLand => CondDeps::WAR,
            _ => CondDeps::empty(),
        }
    }
}

impl TileLeaf {
    /// Whether the leaf reads only what the tile's terrain says ([`TileFacts`]), so that map
    /// generation can evaluate it.
    #[must_use]
    pub const fn is_terrain_level(&self) -> bool {
        matches!(self, Self::Terrains(_) | Self::River | Self::FreshWater | Self::NextToCoast)
    }

    /// The leaf's answer from the terrain alone, or `None` for a leaf that reads more.
    pub fn eval_terrain<W: TileFacts + ?Sized>(&self, w: &W, t: TileIdx) -> Option<bool> {
        Some(match self {
            Self::Terrains(s) => !s.is_disjoint(&w.tile_terrains(t)),
            Self::River => w.tile_river(t),
            Self::FreshWater => w.tile_fresh_water(t),
            Self::NextToCoast => w.tile_next_to_coast(t),
            _ => return None,
        })
    }

    /// Whether tile `t` passes, seen by `viewer`.
    pub fn eval<W: FilterFacts>(&self, w: &W, t: TileIdx, viewer: Option<PlayerId>) -> bool {
        if let Some(yes) = self.eval_terrain(w, t) {
            return yes;
        }
        match self {
            Self::Unowned => w.tile_owner(t).is_none(),
            Self::Yours => viewer.is_some() && w.tile_owner(t) == viewer,
            Self::ForeignLand => viewer.is_some_and(|v| !w.tile_friendly_to(t, v)),
            Self::FriendlyLand => viewer.is_some_and(|v| w.tile_friendly_to(t, v)),
            Self::EnemyLand => match (viewer, w.tile_owner(t)) {
                (Some(v), Some(o)) => w.at_war(v, o),
                _ => false,
            },
            Self::Owner(c) => w.tile_owner(t).is_some_and(|o| c.eval(w, o, viewer)),
            Self::AnyResource => match (viewer, w.tile_resource(t)) {
                (Some(v), Some(r)) => w.resource_visible(v, r),
                _ => false,
            },
            Self::Resource(s) => w
                .tile_resource(t)
                .is_some_and(|r| s.contains(r) && viewer.is_none_or(|v| w.resource_visible(v, r))),
            Self::Unimproved => w.tile_improvement(t).is_none(),
            Self::Improved => w.tile_improvement(t).is_some(),
            Self::Pillaged => w.tile_pillaged(t),
            Self::Worked => w.tile_worked(t),
            Self::Improvement(s) => {
                w.tile_improvement(t).is_some_and(|i| s.contains(i))
                    || w.tile_route(t).is_some_and(|i| s.contains(i))
            }
            Self::Terrains(_) | Self::River | Self::FreshWater | Self::NextToCoast => false,
        }
    }
}

/// The terrain sets several words stand for, computed once per ruleset.
pub(crate) struct TerrainWords {
    /// The water base terrains.
    water: TerrainSet,
    /// The terrain features.
    features: TerrainSet,
    /// The rough terrains.
    rough: TerrainSet,
    /// The terrains map generation raises: Mountain and Hill.
    elevated: TerrainSet,
    /// Every terrain.
    all: TerrainSet,
}

impl TerrainWords {
    pub(crate) fn new(r: &Ruleset) -> Self {
        let set = |f: &dyn Fn(TerrainId) -> bool| -> TerrainSet {
            r.terrains().ids().filter(|&t| f(t)).collect()
        };
        let has = |t: TerrainId, ty: UniqueType| {
            let table = r.uniques();
            r.terrains()[t].uniques.ids().any(|x| table.meta(x).ty == Some(ty))
        };
        Self {
            water: set(&|t| r.terrains()[t].kind == TerrainType::Water),
            features: set(&|t| r.terrains()[t].kind == TerrainType::TerrainFeature),
            rough: set(&|t| r.terrains()[t].rough),
            elevated: set(&|t| {
                has(t, UniqueType::OccursInChains) || has(t, UniqueType::OccursInGroups)
            }),
            all: set(&|_| true),
        }
    }

    /// The terrain sets as the leaf `Terrains`, a constant where the set says so: a tile always
    /// has a base terrain, so a set holding every terrain holds everywhere.
    fn leaf(&self, s: TerrainSet) -> Expr<TileLeaf> {
        if !s.is_empty() && self.all.is_subset(&s) {
            Expr::Const(true)
        } else {
            Expr::Leaf(TileLeaf::Terrains(s))
        }
    }
}

fn not(e: Expr<TileLeaf>) -> Expr<TileLeaf> {
    Expr::Not(Box::new(e))
}

/// The terrain part of one term: `_tile_terrain_single` (`uniques.py:423-474`).
pub(crate) fn terrain_term(st: &mut Statics<'_>, words: &TerrainWords, s: &str) -> Expr<TileLeaf> {
    let leaf = Expr::Leaf;
    let water = || words.leaf(words.water);
    let e = match s {
        "All" | "all" | "Terrain" => Expr::Const(true),
        "Water" => water(),
        "Land" => not(water()),
        "Coastal" => Expr::All([not(water()), leaf(TileLeaf::NextToCoast)].into()),
        "River" => leaf(TileLeaf::River),
        "Unowned" => leaf(TileLeaf::Unowned),
        "your" => leaf(TileLeaf::Yours),
        "Foreign Land" | "Foreign" => leaf(TileLeaf::ForeignLand),
        "Friendly Land" | "Friendly" => leaf(TileLeaf::FriendlyLand),
        "Enemy Land" | "Enemy" => leaf(TileLeaf::EnemyLand),
        "resource" => leaf(TileLeaf::AnyResource),
        "Water resource" => Expr::All([water(), leaf(TileLeaf::AnyResource)].into()),
        "Featureless" => not(words.leaf(words.features)),
        "Open terrain" => not(words.leaf(words.rough)),
        // Map generation took `Rough` for `Rough terrain` (`mapgen.py:482-483`).
        "Rough" => words.leaf(words.rough),
        "Fresh water" | "Fresh Water" => leaf(TileLeaf::FreshWater),
        // `[non-fresh water]` is no `non-[...]`: the improvement yields read it as the opposite of
        // fresh water (`tiles.py:259`), and it is that here for every reader.
        "non-fresh water" => not(leaf(TileLeaf::FreshWater)),
        "Elevated" => words.leaf(words.elevated),
        _ => {
            let terrains: TerrainSet = typed(&st.term(StaticDomain::Terrain, s));
            Expr::Any(
                [
                    words.leaf(terrains),
                    leaf(TileLeaf::Resource(resource_set(st, s))),
                    civ::term(st, s).map(&mut |c| TileLeaf::Owner(*c)),
                ]
                .into(),
            )
        }
    };
    e.fold()
}

/// The improvement part of one term: the rest of `_tile_single` (`uniques.py:401-420`).
pub(crate) fn improvement_term(st: &mut Statics<'_>, s: &str) -> Expr<TileLeaf> {
    Expr::Leaf(match s {
        "unimproved" => TileLeaf::Unimproved,
        "improved" => TileLeaf::Improved,
        "pillaged" => TileLeaf::Pillaged,
        "worked" => TileLeaf::Worked,
        _ => TileLeaf::Improvement(typed(&st.term(StaticDomain::Improvement, s))),
    })
    .fold()
}

/// One term of a full tile filter: its terrain part or its improvement part.
pub(crate) fn term(st: &mut Statics<'_>, words: &TerrainWords, s: &str) -> Expr<TileLeaf> {
    Expr::Any([terrain_term(st, words, s), improvement_term(st, s)].into()).fold()
}

/// The resources a tile term names: by name, by a tag they carry, or `<type> resource`
/// (`uniques.py:467-472`). Not `resource_matches`, which also takes `any` and the stat names.
fn resource_set(st: &Statics<'_>, s: &str) -> ResourceSet {
    let r = st.rules();
    let tag = st.tag(s);
    r.resources()
        .iter()
        .filter(|(_, x)| {
            *x.name == *s
                || Statics::tagged(tag, &x.uniques)
                || s.strip_suffix(" resource") == Some(super::statics::resource_type_name(x.kind))
        })
        .map(|(id, _)| id)
        .collect()
}
