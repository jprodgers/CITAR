//! Filters (DESIGN.md 5.7): the texts in brackets that select objects, compiled once at load.
//!
//! Ports `uniques.py:294-776`: the multi-filter grammar ([`parse`]) and every single-term
//! predicate. Python evaluated a filter's text at each use, 9.7 million times in a game; here:
//! - **static filters** (base units, buildings, terrains, improvements, resources, techs, eras,
//!   policies, promotions, and the nations civilization filters name) are sets, evaluated once
//!   over every object ([`statics`]); the compiler's `SetRef`s get their members here;
//! - **dynamic filters** (units, tiles, cities, civilizations, combatants) are pruned trees
//!   ([`Expr`]) over [`UnitLeaf`], [`TileLeaf`], [`CityLeaf`] and [`CivLeaf`]: each term becomes
//!   the disjunction of the branches of Python's if-chain that could ever match it, with its static
//!   parts as sets, and is then constant-folded;
//! - a term that matches nothing does not load, where Python quietly answered no; a filter nested
//!   deeper than [`parse::MAX_DEPTH`] does not load either;
//! - a parameter that may name objects of several kinds ([`ObjectFilter`]) keeps only the kinds its
//!   text can select, and does not load if a kind it still selects has a term that matches nothing
//!   of that kind (`non-[Temple]` as tiles is every tile).
//!
//! The world a dynamic filter reads is [`FilterFacts`]; map generation, which has no game, reads
//! tile filters through [`TileFacts`] alone ([`Filters::gen_matches`]).

pub mod city;
pub mod civ;
pub mod expr;
pub mod parse;
pub mod statics;
pub mod tile;
pub mod unit;

pub use self::city::CityLeaf;
pub use self::civ::CivLeaf;
pub use self::expr::{Expr, Leaf};
use self::statics::Statics;
use self::tile::TerrainWords;
pub use self::tile::TileLeaf;
pub use self::unit::{UnitFacts, UnitLeaf, UnitScope};
use super::countable::Countable;
use super::generated::{ParamKind, UniqueType};
use super::params::Param;
use super::table::{ObjectFilter, Source, StaticDomain, StaticId, UniqueTable};
use super::world::{FilterFacts, TileFacts};
use crate::base::collections::DetSet;
use crate::base::ids::{
    BaseUnitId, CityFilterId, CityId, CivFilterId, CombatantFilterId, IdVec, ObjectFilterId,
    PlayerId, SetRef, TileFilterId, TileIdx, UniqueId, UnitFilterId, UnitId,
};
use crate::base::sets::{BitSet, TerrainSet};
use crate::rules::Ruleset;

/// A tile filter, compiled for each way the rules read one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileFilter {
    /// `tile_matches`: what the tile's terrain says and what stands on it.
    pub full: Expr<TileLeaf>,
    /// `tile_terrain_matches`: what the tile's terrain says alone.
    pub terrain: Expr<TileLeaf>,
    /// `terrain_matches` over each terrain: the terrains the filter names, for the rules that ask
    /// it of a terrain rather than a tile (`workers.py:44-52`).
    pub terrains: TerrainSet,
    /// Whether map generation can read it: every term known from the terrain alone, through
    /// [`TileFacts`] (DESIGN.md 5.7).
    pub terrain_level: bool,
}

/// A combatant filter: a unit is asked as a unit filter, a city as a city filter in which `City`
/// holds (`combat.py:91-96`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CombatantFilter {
    pub unit: Expr<UnitLeaf>,
    pub city: Expr<CityLeaf>,
}

/// One side of a fight.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Combatant {
    Unit(UnitId),
    City(CityId),
}

/// A tile filter map generation reads (DESIGN.md 5.10): the map-generation tables' and the start
/// biases'. Only the loader makes one, and a ruleset loads only if every one is
/// [`TileFilter::terrain_level`] (`rules::gen_tables` checks them all), so that
/// [`Filters::gen_matches`] answers it from the tile's terrain alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GenFilter(TileFilterId);

impl GenFilter {
    /// The loader's: a handle whose ruleset the table builder refuses unless it is terrain-level.
    pub(crate) const fn new(id: TileFilterId) -> Self {
        Self(id)
    }

    /// The filter's handle, for its text and its trees.
    #[must_use]
    pub const fn id(self) -> TileFilterId {
        self.0
    }
}

/// Every dynamic filter of a ruleset, compiled, indexed by the handles the unique compiler gave
/// their texts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filters {
    pub(crate) units: IdVec<UnitFilterId, Expr<UnitLeaf>>,
    pub(crate) tiles: IdVec<TileFilterId, TileFilter>,
    pub(crate) cities: IdVec<CityFilterId, Expr<CityLeaf>>,
    pub(crate) civs: IdVec<CivFilterId, Expr<CivLeaf>>,
    pub(crate) combatants: IdVec<CombatantFilterId, CombatantFilter>,
}

impl Filters {
    /// Every unit filter, by handle.
    #[must_use]
    pub fn units(&self) -> &IdVec<UnitFilterId, Expr<UnitLeaf>> {
        &self.units
    }

    /// Every tile filter, by handle.
    #[must_use]
    pub fn tiles(&self) -> &IdVec<TileFilterId, TileFilter> {
        &self.tiles
    }

    /// Every city filter, by handle.
    #[must_use]
    pub fn cities(&self) -> &IdVec<CityFilterId, Expr<CityLeaf>> {
        &self.cities
    }

    /// Every civilization filter, by handle.
    #[must_use]
    pub fn civs(&self) -> &IdVec<CivFilterId, Expr<CivLeaf>> {
        &self.civs
    }

    /// Every combatant filter, by handle.
    #[must_use]
    pub fn combatants(&self) -> &IdVec<CombatantFilterId, CombatantFilter> {
        &self.combatants
    }

    /// A unit filter's tree.
    #[must_use]
    pub fn unit(&self, id: UnitFilterId) -> &Expr<UnitLeaf> {
        &self.units[id]
    }

    /// A tile filter's trees.
    #[must_use]
    pub fn tile(&self, id: TileFilterId) -> &TileFilter {
        &self.tiles[id]
    }

    /// A city filter's tree.
    #[must_use]
    pub fn city(&self, id: CityFilterId) -> &Expr<CityLeaf> {
        &self.cities[id]
    }

    /// A civilization filter's tree.
    #[must_use]
    pub fn civ(&self, id: CivFilterId) -> &Expr<CivLeaf> {
        &self.civs[id]
    }

    /// A combatant filter's trees.
    #[must_use]
    pub fn combatant(&self, id: CombatantFilterId) -> &CombatantFilter {
        &self.combatants[id]
    }

    /// Whether unit `u` passes the filter (`unit_matches`).
    pub fn unit_matches<W: FilterFacts>(
        &self,
        id: UnitFilterId,
        w: &W,
        u: UnitId,
        scope: UnitScope,
    ) -> bool {
        self.units[id].eval(&mut |l| l.eval(w, u, scope))
    }

    /// Whether a unit type passes the filter with no unit behind it (`base_unit_matches`,
    /// `uniques.py:477-525`): only what its row says, its name, type, era, role and tags; a test
    /// of a unit's owner, promotions or state fails.
    pub fn base_unit_matches(&self, id: UnitFilterId, base: BaseUnitId) -> bool {
        self.units[id].eval(&mut |l| match l {
            UnitLeaf::Base(s) => s.contains(base),
            _ => false,
        })
    }

    /// Whether a unit with these facts passes the filter, matched with nothing in context and
    /// its owner seen by `viewer`: a unit that is gone by the time its trigger is matched.
    pub fn unit_facts_match<W: FilterFacts>(
        &self,
        id: UnitFilterId,
        w: &W,
        u: &UnitFacts,
        viewer: Option<PlayerId>,
    ) -> bool {
        self.units[id].eval(&mut |l| l.eval_facts(w, u, viewer))
    }

    /// Whether tile `t` passes the filter, terrain and all that stands on it (`tile_matches`).
    pub fn tile_matches<W: FilterFacts>(
        &self,
        id: TileFilterId,
        w: &W,
        t: TileIdx,
        viewer: Option<PlayerId>,
    ) -> bool {
        self.tiles[id].full.eval(&mut |l| l.eval(w, t, viewer))
    }

    /// Whether tile `t`'s terrain passes the filter (`tile_terrain_matches`).
    pub fn tile_terrain_matches<W: FilterFacts>(
        &self,
        id: TileFilterId,
        w: &W,
        t: TileIdx,
        viewer: Option<PlayerId>,
    ) -> bool {
        self.tiles[id].terrain.eval(&mut |l| l.eval(w, t, viewer))
    }

    /// Whether tile `t` passes the filter as map generation reads it: from the terrain alone,
    /// through [`TileFacts`]. A [`GenFilter`] is terrain-level, which debug builds check.
    pub fn gen_matches<W: TileFacts + ?Sized>(&self, f: GenFilter, w: &W, t: TileIdx) -> bool {
        let tf = &self.tiles[f.0];
        debug_assert!(tf.terrain_level, "map generation reads only terrain-level filters");
        tf.terrain.eval(&mut |l| l.eval_terrain(w, t).unwrap_or(false))
    }

    /// Whether city `c` passes the filter, seen by `viewer`, or by its owner when `None`
    /// (`city_matches`).
    pub fn city_matches<W: FilterFacts>(
        &self,
        id: CityFilterId,
        w: &W,
        c: CityId,
        viewer: Option<PlayerId>,
    ) -> bool {
        self.cities[id].eval(&mut |l| l.eval(w, c, viewer))
    }

    /// Whether civilization `p` passes the filter, seen by `viewer` (`civ_matches`).
    pub fn civ_matches<W: FilterFacts>(
        &self,
        id: CivFilterId,
        w: &W,
        p: PlayerId,
        viewer: Option<PlayerId>,
    ) -> bool {
        self.civs[id].eval(&mut |l| l.eval(w, p, viewer))
    }

    /// Whether a combatant passes the filter (`Combatant.matches`, `combat.py:91-96`): a unit with
    /// no unit or viewer in context, a city seen by its owner.
    pub fn combatant_matches<W: FilterFacts>(
        &self,
        id: CombatantFilterId,
        w: &W,
        c: Combatant,
    ) -> bool {
        let f = &self.combatants[id];
        match c {
            Combatant::Unit(u) => f.unit.eval(&mut |l| l.eval(w, u, UnitScope::default())),
            Combatant::City(city) => f.city.eval(&mut |l| l.eval(w, city, None)),
        }
    }
}

// ---- Compiling ----------------------------------------------------------------------------------

/// A filter that does not compile, and where it is first used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Problem {
    /// The first unique that uses the filter, if a unique does.
    pub(crate) site: Option<UniqueId>,
    pub(crate) message: String,
}

/// What the filter stage decided, for the loader to store in the ruleset.
pub(crate) struct Compiled {
    pub(crate) filters: Filters,
    /// The members of every static filter the compiler did not decide itself.
    pub(crate) members: Vec<(SetRef, BitSet)>,
    /// Every object filter, with the kinds its text cannot match dropped.
    pub(crate) objects: Vec<ObjectFilter>,
    pub(crate) problems: Vec<Problem>,
}

/// The terms of one filter that match nothing, or the whole filter too deep to read.
#[derive(Clone, Debug, Default)]
struct Dead {
    too_deep: bool,
    terms: Vec<Box<str>>,
}

impl Dead {
    fn is_empty(&self) -> bool {
        !self.too_deep && self.terms.is_empty()
    }

    fn describe(&self, text: &str, what: &str) -> String {
        if self.too_deep {
            return format!(
                "the filter [{text}] nests deeper than {} levels, which the engine does not read",
                parse::MAX_DEPTH
            );
        }
        let terms: Vec<String> = self.terms.iter().map(|t| format!("{t:?}")).collect();
        format!(
            "in the filter [{text}], {} {} no {what}",
            terms.join(" and "),
            if terms.len() == 1 { "matches" } else { "match" }
        )
    }
}

/// Compiles one filter's text: each term by `term`, then folded. A term that compiles to `false`
/// is dead.
fn compile_one<L: Leaf>(
    text: &str,
    dead: &mut Dead,
    mut term: impl FnMut(&str) -> Expr<L>,
) -> Expr<L> {
    let Ok(tree) = parse::parse(text) else {
        dead.too_deep = true;
        return Expr::Const(false);
    };
    tree.bind(&mut |t| {
        let e = term(t);
        if e.constant() == Some(false) && !dead.terms.iter().any(|d| **d == **t) {
            dead.terms.push((*t).into());
        }
        e
    })
    .fold()
}

/// Compiles filter texts against one ruleset, sharing the term sets between them.
struct Compiler<'r> {
    st: Statics<'r>,
    words: TerrainWords,
}

impl<'r> Compiler<'r> {
    fn new(rules: &'r Ruleset) -> Self {
        Self { st: Statics::new(rules), words: TerrainWords::new(rules) }
    }

    fn unit(&mut self, text: &str) -> (Expr<UnitLeaf>, Dead) {
        let mut dead = Dead::default();
        let e = compile_one(text, &mut dead, |t| unit::term(&mut self.st, t));
        (e, dead)
    }

    /// The tile filter, the dead terms of its full form and those of its terrain form.
    fn tile(&mut self, text: &str) -> (TileFilter, Dead, Dead) {
        let (mut dead, mut dead_terrain) = (Dead::default(), Dead::default());
        let (st, words) = (&mut self.st, &self.words);
        let full = compile_one(text, &mut dead, |t| tile::term(st, words, t));
        let terrain = compile_one(text, &mut dead_terrain, |t| tile::terrain_term(st, words, t));
        let terrains = statics::typed(
            &st.filter(StaticDomain::Terrain, text).unwrap_or_else(|_| BitSet::new()),
        );
        let terrain_level =
            dead_terrain.is_empty() && terrain.leaves().iter().all(|l| l.is_terrain_level());
        (TileFilter { full, terrain, terrains, terrain_level }, dead, dead_terrain)
    }

    fn city(&mut self, text: &str) -> (Expr<CityLeaf>, Dead) {
        let mut dead = Dead::default();
        let e = compile_one(text, &mut dead, |t| city::term(&mut self.st, t));
        (e, dead)
    }

    fn civ(&mut self, text: &str) -> (Expr<CivLeaf>, Dead) {
        let mut dead = Dead::default();
        let e = compile_one(text, &mut dead, |t| civ::term(&mut self.st, t));
        (e, dead)
    }

    /// A combatant filter; a term is dead only if neither a unit nor a city can match it.
    fn combatant(&mut self, text: &str) -> (CombatantFilter, Dead) {
        let (unit, dead_unit) = self.unit(text);
        let mut dead_city = Dead::default();
        let city = compile_one(text, &mut dead_city, |t| match t {
            "City" => Expr::Const(true),
            _ => city::term(&mut self.st, t),
        });
        let dead = Dead {
            too_deep: dead_unit.too_deep,
            terms: dead_unit.terms.into_iter().filter(|t| dead_city.terms.contains(t)).collect(),
        };
        (CombatantFilter { unit, city }, dead)
    }

    /// A static filter's members, and its terms that select nothing.
    fn set(&mut self, domain: StaticDomain, text: &str) -> (BitSet, Dead) {
        let mut dead = Dead::default();
        match parse::parse(text) {
            Err(_) => dead.too_deep = true,
            Ok(tree) => {
                for t in tree.leaves() {
                    if self.st.term(domain, t).is_empty() && !dead.terms.iter().any(|d| **d == **t)
                    {
                        dead.terms.push((*t).into());
                    }
                }
            }
        }
        (self.st.filter(domain, text).unwrap_or_else(|_| BitSet::new()), dead)
    }
}

/// Compiles a unit filter's text against `rules`, as the loader compiles the ruleset's own: for
/// tools and tests.
///
/// # Errors
/// A sentence naming the terms that match no unit, or a filter nested too deep.
pub fn unit_filter(rules: &Ruleset, text: &str) -> Result<Expr<UnitLeaf>, String> {
    let (e, dead) = Compiler::new(rules).unit(text);
    dead.is_empty().then_some(e).ok_or_else(|| dead.describe(text, "unit"))
}

/// Compiles a tile filter's text against `rules`, in every form the rules read one.
///
/// # Errors
/// A sentence naming the terms that match no tile, or a filter nested too deep.
pub fn tile_filter(rules: &Ruleset, text: &str) -> Result<TileFilter, String> {
    let (f, dead, _) = Compiler::new(rules).tile(text);
    dead.is_empty().then_some(f).ok_or_else(|| dead.describe(text, "tile"))
}

/// Compiles a city filter's text against `rules`.
///
/// # Errors
/// A sentence naming the terms that match no city, or a filter nested too deep.
pub fn city_filter(rules: &Ruleset, text: &str) -> Result<Expr<CityLeaf>, String> {
    let (e, dead) = Compiler::new(rules).city(text);
    dead.is_empty().then_some(e).ok_or_else(|| dead.describe(text, "city"))
}

/// Compiles a civilization filter's text against `rules`.
///
/// # Errors
/// A sentence naming the terms that match no civilization, or a filter nested too deep.
pub fn civ_filter(rules: &Ruleset, text: &str) -> Result<Expr<CivLeaf>, String> {
    let (e, dead) = Compiler::new(rules).civ(text);
    dead.is_empty().then_some(e).ok_or_else(|| dead.describe(text, "civilization"))
}

/// Compiles a combatant filter's text against `rules`.
///
/// # Errors
/// A sentence naming the terms that match no unit and no city, or a filter nested too deep.
pub fn combatant_filter(rules: &Ruleset, text: &str) -> Result<CombatantFilter, String> {
    let (f, dead) = Compiler::new(rules).combatant(text);
    dead.is_empty().then_some(f).ok_or_else(|| dead.describe(text, "unit or city"))
}

/// Compiles every filter of `rules`, whose unique table the compiler has filled and whose derived
/// tables are made.
pub(crate) fn compile(rules: &Ruleset) -> Compiled {
    let table = rules.uniques();
    let mut cx = Compiler::new(rules);
    let mut problems = Vec::new();

    // Each table has one entry per handle, in handle order.
    let (units, dead_units): (Vec<_>, Vec<_>) =
        table.unit_filters.iter().map(|(_, &t)| cx.unit(table.text(t))).unzip();
    let mut tiles = Vec::new();
    let mut dead_tiles = Vec::new();
    let mut dead_terrains = Vec::new();
    for (_, &text) in table.tile_filters.iter() {
        let (f, dead, dead_terrain) = cx.tile(table.text(text));
        tiles.push(f);
        dead_tiles.push(dead);
        dead_terrains.push(dead_terrain);
    }
    let (cities, dead_cities): (Vec<_>, Vec<_>) =
        table.city_filters.iter().map(|(_, &t)| cx.city(table.text(t))).unzip();
    let (civs, dead_civs): (Vec<_>, Vec<_>) =
        table.civ_filters.iter().map(|(_, &t)| cx.civ(table.text(t))).unzip();
    let (combatants, dead_combatants): (Vec<_>, Vec<_>) =
        table.combatant_filters.iter().map(|(_, &t)| cx.combatant(table.text(t))).unzip();

    let mut members = Vec::new();
    let mut dead_sets = Vec::new();
    // Whether each static filter selects nothing.
    let mut empty_sets = Vec::new();
    for (id, s) in table.sets.iter() {
        if s.fixed {
            dead_sets.push(Dead::default());
            empty_sets.push(s.members.is_empty());
            continue;
        }
        let (set, dead) = cx.set(s.domain, table.text(s.text));
        empty_sets.push(set.is_empty());
        members.push((id, set));
        dead_sets.push(dead);
    }

    // An object filter drops the kinds its text never selects: a tile tree that folds to `false`,
    // an empty set. A kind it still selects despite a term that matches nothing of that kind
    // (`non-[Temple]` as tiles is every tile) reads the text as meaning something of that kind it
    // does not name, and does not load. Its tiles are read as `tile_terrain_matches` beside
    // improvements (`workers.py:201-202`), as `tile_matches` beside buildings.
    let mut objects = Vec::new();
    let mut object_problems = Vec::new();
    for (_, o) in table.objects.iter() {
        let mut o = *o;
        let text = table.text(o.text);
        let mut unclear = Vec::new();
        let mut too_deep = None;
        if let Some(t) = o.tiles {
            let i = usize::from(t.0);
            let (tree, dead) = if o.kind == ParamKind::ImprovementOrTerrainFilter {
                (&tiles[i].terrain, &dead_terrains[i])
            } else {
                (&tiles[i].full, &dead_tiles[i])
            };
            if dead.too_deep {
                too_deep = Some(dead.describe(text, "tile"));
            }
            if tree.constant() == Some(false) {
                o.tiles = None;
            } else if !dead.is_empty() {
                unclear.push((dead, "tile"));
            }
        }
        for (slot, what) in [(&mut o.buildings, "building"), (&mut o.improvements, "improvement")] {
            if let Some(s) = *slot {
                let i = usize::from(s.0);
                if dead_sets[i].too_deep && too_deep.is_none() {
                    too_deep = Some(dead_sets[i].describe(text, what));
                }
                if empty_sets[i] {
                    *slot = None;
                } else if !dead_sets[i].is_empty() {
                    unclear.push((&dead_sets[i], what));
                }
            }
        }
        let none_left = o.tiles.is_none()
            && o.buildings.is_none()
            && o.improvements.is_none()
            && o.specialist.is_none();
        let problem = if let Some(deep) = too_deep {
            Some(deep)
        } else if !unclear.is_empty() {
            let parts: Vec<String> = unclear
                .iter()
                .map(|(dead, what)| {
                    let terms: Vec<String> = dead.terms.iter().map(|t| format!("{t:?}")).collect();
                    format!(
                        "{} {} no {what}, yet the filter still selects {what}s",
                        terms.join(" and "),
                        if terms.len() == 1 { "matches" } else { "match" }
                    )
                })
                .collect();
            Some(format!(
                "the filter [{text}] is unclear: {}; name the objects of each kind it means",
                parts.join(", and ")
            ))
        } else if none_left {
            Some(format!("the filter [{text}] matches nothing of any kind its parameter allows"))
        } else {
            None
        };
        objects.push(o);
        object_problems.push(problem);
    }

    // Every filter a unique reads directly must match something, reported once, at its first use.
    let mut reported: DetSet<Use> = DetSet::default();
    for (id, u) in table.iter() {
        let meta = table.meta(id);
        if matches!(meta.source, Source::Temporary(_)) {
            // A variant shares its original's parameters.
            continue;
        }
        let mut found = Vec::new();
        let terrain_form = meta.ty.is_some_and(reads_tile_terrain);
        for p in u.data.params() {
            uses(p, terrain_form, &mut found);
        }
        for c in table.conds(u) {
            for p in c.data.params() {
                uses(p, false, &mut found);
            }
        }
        if let Some(t) = meta.trigger {
            for p in t.params() {
                uses(p, false, &mut found);
            }
        }
        for x in found {
            if reported.contains(&x) {
                continue;
            }
            let message = match x {
                Use::Unit(f) => {
                    dead_or_none(&dead_units[usize::from(f.0)], table.unit_filter(f), "unit")
                }
                Use::Tile(f) => {
                    dead_or_none(&dead_tiles[usize::from(f.0)], table.tile_filter(f), "tile")
                }
                Use::TileTerrain(f) => dead_or_none(
                    &dead_terrains[usize::from(f.0)],
                    table.tile_filter(f),
                    "tile by its terrain alone, as this unique reads it",
                ),
                Use::City(f) => {
                    dead_or_none(&dead_cities[usize::from(f.0)], table.city_filter(f), "city")
                }
                Use::Civ(f) => {
                    dead_or_none(&dead_civs[usize::from(f.0)], table.civ_filter(f), "civilization")
                }
                Use::Combatant(f) => dead_or_none(
                    &dead_combatants[usize::from(f.0)],
                    table.combatant_filter(f),
                    "unit or city",
                ),
                Use::Set(s) => {
                    let sf = table.set(s);
                    dead_or_none(
                        &dead_sets[usize::from(s.0)],
                        table.text(sf.text),
                        domain_words(sf.domain),
                    )
                }
                Use::Object(o) => object_problems[usize::from(o.0)].clone(),
            };
            if let Some(message) = message {
                reported.insert(x);
                problems.push(Problem { site: Some(id), message });
            }
        }
    }

    Compiled {
        filters: Filters {
            units: IdVec::from_vec(units),
            tiles: IdVec::from_vec(tiles),
            cities: IdVec::from_vec(cities),
            civs: IdVec::from_vec(civs),
            combatants: IdVec::from_vec(combatants),
        },
        members,
        objects,
        problems,
    }
}

fn dead_or_none(dead: &Dead, text: &str, what: &str) -> Option<String> {
    (!dead.is_empty()).then(|| dead.describe(text, what))
}

/// What a static domain's objects are called, for messages.
const fn domain_words(d: StaticDomain) -> &'static str {
    match d {
        StaticDomain::BaseUnit => "unit",
        StaticDomain::Building => "building",
        StaticDomain::Terrain => "terrain",
        StaticDomain::Improvement => "improvement",
        StaticDomain::Resource => "resource",
        StaticDomain::Tech => "technology",
        StaticDomain::Era => "era",
        StaticDomain::Policy => "policy",
        StaticDomain::Promotion => "promotion",
        StaticDomain::Nation => "nation",
    }
}

/// A filter a unique reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Use {
    Unit(UnitFilterId),
    /// A tile filter read in full, `tile_matches`.
    Tile(TileFilterId),
    /// A tile filter read from the tile's terrain alone, `tile_terrain_matches`.
    TileTerrain(TileFilterId),
    City(CityFilterId),
    Civ(CivFilterId),
    Combatant(CombatantFilterId),
    Set(SetRef),
    Object(ObjectFilterId),
}

/// Whether a unique of this type reads its tile filter from the tile's terrain alone
/// (`tile_terrain_matches`), so that its terms must match in that form: a building's `Must be on`
/// and `Must not be on` (`cities.py:1231-1233`) and `[stats] in cities on [terrainFilter] tiles`
/// (`cities.py:386`). Every other tile filter a unique reads is read in full, but map
/// generation's, which `rules::gen_tables` holds to the terrain.
const fn reads_tile_terrain(ty: UniqueType) -> bool {
    matches!(
        ty,
        UniqueType::MustBeOn | UniqueType::MustNotBeOn | UniqueType::StatsFromCitiesOnSpecificTiles
    )
}

/// The filters a parameter reads, a countable's included; its tile filter read from the terrain
/// alone when `terrain_form`.
fn uses(p: Param, terrain_form: bool, out: &mut Vec<Use>) {
    out.push(match p {
        Param::UnitFilter(f) => Use::Unit(f),
        Param::TileFilter(f) if terrain_form => Use::TileTerrain(f),
        Param::TileFilter(f) => Use::Tile(f),
        Param::CityFilter(f) => Use::City(f),
        Param::CivFilter(f) => Use::Civ(f),
        Param::CombatantFilter(f) => Use::Combatant(f),
        Param::Set(s) => Use::Set(s),
        Param::Object(o) => Use::Object(o),
        Param::Countable(c) => match c {
            Countable::UnitsMatching(f) => Use::Unit(f),
            Countable::CitiesMatching(f) => Use::City(f),
            Countable::RemainingCivs(f) => Use::Civ(f),
            Countable::BuildingsMatching(s) => Use::Set(s),
            _ => return,
        },
        _ => return,
    });
}

/// The terrains a list of terrain filters names together: an improvement's
/// `terrainsCanBeBuiltOn` (`workers.py:44-46`).
///
/// # Errors
/// A sentence naming the terms that match no terrain, or a filter nested too deep.
pub(crate) fn terrain_list(rules: &Ruleset, texts: &[String]) -> Result<TerrainSet, String> {
    let mut st = Statics::new(rules);
    let mut out = BitSet::new();
    for text in texts {
        let mut dead = Dead::default();
        match parse::parse(text) {
            Err(_) => dead.too_deep = true,
            Ok(tree) => {
                for t in tree.leaves() {
                    if st.term(StaticDomain::Terrain, t).is_empty() {
                        dead.terms.push((*t).into());
                    }
                }
            }
        }
        if !dead.is_empty() {
            return Err(dead.describe(text, "terrain"));
        }
        out.union_with(&st.filter(StaticDomain::Terrain, text).unwrap_or_else(|_| BitSet::new()));
    }
    Ok(statics::typed(&out))
}

/// The compiled filters, beside the texts the unique table keeps.
impl UniqueTable {
    /// Every dynamic filter, compiled.
    #[must_use]
    pub fn filters(&self) -> &Filters {
        &self.filters
    }

    /// Whether the static filter `s` selects object `id`. The id's type must be the filter's
    /// domain's, which debug builds check.
    #[must_use]
    #[inline]
    pub fn in_set<I: StaticId>(&self, s: SetRef, id: I) -> bool {
        self.sets[s].contains(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A map whose every tile has a river, and nothing else.
    struct Rivers;

    impl TileFacts for Rivers {
        fn tile_terrains(&self, _: TileIdx) -> TerrainSet {
            TerrainSet::new()
        }
        fn tile_river(&self, _: TileIdx) -> bool {
            true
        }
        fn tile_fresh_water(&self, _: TileIdx) -> bool {
            false
        }
        fn tile_next_to_coast(&self, _: TileIdx) -> bool {
            false
        }
    }

    /// Filters holding one tile filter, a single leaf.
    fn one(leaf: TileLeaf, terrain_level: bool) -> Filters {
        let e = Expr::Leaf(leaf);
        let f =
            TileFilter { full: e.clone(), terrain: e, terrains: TerrainSet::new(), terrain_level };
        Filters { tiles: IdVec::from_vec(vec![f]), ..Filters::default() }
    }

    #[test]
    fn map_generation_reads_a_filter_from_the_terrain() {
        let f = one(TileLeaf::River, true);
        assert!(f.gen_matches(GenFilter::new(TileFilterId(0)), &Rivers, TileIdx(0)));
        let f = one(TileLeaf::FreshWater, true);
        assert!(!f.gen_matches(GenFilter::new(TileFilterId(0)), &Rivers, TileIdx(0)));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "terrain-level")]
    fn map_generation_never_reads_a_filter_that_asks_more() {
        // `Worked` has no answer from the terrain: map generation would read it as no.
        let f = one(TileLeaf::Worked, false);
        assert!(!f.gen_matches(GenFilter::new(TileFilterId(0)), &Rivers, TileIdx(0)));
    }
}
