//! What each civilization sees: the vision sources, their footprints and the counts
//! (DESIGN.md 6.9). Replaces `visibility.compute_visible` and the `_vis` sets of `refresh`
//! (`visibility.py:96-150`), which recomputed every civilization's sight from all its units and
//! cities after every unit step.
//!
//! A source is a unit, a city, a city-state's city seen by its ally, or a set-up spy
//! ([`SourceKey`]). Each has a footprint, the sorted tiles it sees ([`VisSource`]); each
//! civilization counts, per tile, how many of its sources see it. `Visibility::set` replaces
//! one source's footprint: it adds the new tiles first and takes the old ones away after, so a
//! tile both see never goes dark, and it reports every count that left or reached zero
//! ([`Transition`]). Only the source's owner's counts move.
//!
//! The counts are derived: never saved, rebuilt on load, and checked against a rebuild from
//! scratch by the cache oracle (`vis::verify`).

use core::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use super::los::{Heights, LosCache};
use crate::base::hex::HexGrid;
use crate::base::ids::{CityId, PlayerId, TileIdx, UniqueId, UnitId};
use crate::base::sets::{BitSet, PlayerVec, TerrainSet};
use crate::game::derive::rev::Rev;
use crate::rules::Ruleset;
use crate::state::State;
use crate::state::map::Tile;
use crate::unique::{CondDeps, UniqueType};

/// A vision source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceKey {
    /// A unit sees round itself (`visibility.unit_viewable`).
    Unit(UnitId),
    /// A city sees its tiles and one ring beyond them (`visibility.py:100-104`).
    City(CityId),
    /// A city-state's city, seen by the civilization it is allied with, or by a city-state
    /// allied with that city-state (`visibility.py:107-111`): the viewer, then the city.
    AllyCity(PlayerId, CityId),
    /// A spy set up in a city sees it and the ring round it (`espionage.visible_tiles`,
    /// `espionage.py:444-451`): its civilization, then its place in the civilization's list.
    Spy(PlayerId, u8),
}

/// How a unit sees (`visibility._unit_viewable`, `visibility.py:85-93`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sight {
    /// `No Sight`: its own tile alone.
    Blind,
    /// `Can see over obstacles`: every tile within the radius.
    Clear(u32),
    /// The elevation walk with this radius.
    Walk(u32),
}

impl Sight {
    /// The tiles beyond which nothing this sight sees can matter: its radius, one more for the
    /// mountains a walk sees past it.
    #[must_use]
    pub const fn reach(self) -> u32 {
        match self {
            Self::Blind => 0,
            Self::Clear(r) => r,
            Self::Walk(r) => r.saturating_add(1),
        }
    }
}

/// One source's part in its owner's sight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisSource {
    /// Whose sight it is.
    pub owner: PlayerId,
    /// Where it stands: a unit's tile, a city's.
    pub at: TileIdx,
    /// How a unit sees; `None` for every other source.
    pub sight: Option<Sight>,
    /// The tiles it sees, sorted and without repeats.
    pub footprint: Arc<[TileIdx]>,
}

/// A tile that came into or went out of a civilization's sight.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Transition {
    /// Whose sight.
    pub civ: PlayerId,
    /// The tile.
    pub tile: TileIdx,
    /// Into sight (a count left zero), or out of it (a count reached zero).
    pub up: bool,
}

/// What the ruleset's sight uniques read, which decides what can change a unit's sight besides
/// the unit itself (DESIGN.md 6.5, `SightMods`): read once from the ruleset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SightRules {
    /// A civilization-wide source (a nation, a policy, a building, a belief...) carries a
    /// `[n] Sight`: the civilization's index can change its units' sight. (`No Sight` and `Can
    /// see over obstacles` are read from the unit alone, `unit_has` without the civilization.)
    pub civ_sources: bool,
    /// A resource carries one: its supply can.
    pub resource_sources: bool,
    /// The terrains that carry a `[n] Sight` for the units standing on them.
    pub terrains: TerrainSet,
    /// The civilization-level classes their conditionals read.
    pub civ_deps: CondDeps,
    /// They read the tile in context: a change to a tile can change the sight of the units on it.
    pub tile: bool,
    /// They read a city, a fight or the whole map: anything can change any unit's sight.
    pub anywhere: bool,
}

impl Default for SightRules {
    fn default() -> Self {
        Self {
            civ_sources: false,
            resource_sources: false,
            terrains: TerrainSet::new(),
            civ_deps: CondDeps::empty(),
            tile: false,
            anywhere: false,
        }
    }
}

impl SightRules {
    /// The types that make a unit's sight.
    pub const TYPES: [UniqueType; 3] =
        [UniqueType::Sight, UniqueType::NoSight, UniqueType::CanSeeOverObstacles];

    /// What the sight uniques of `rules` read.
    #[must_use]
    pub fn new(rules: &Ruleset) -> Self {
        let t = rules.uniques();
        // The uniques of the sources a unit's own profile or its tile are made of.
        let mut own = vec![false; t.len()];
        let mark = |range: &core::ops::Range<u16>, v: &mut Vec<bool>| {
            for i in range.clone() {
                if let Some(x) = v.get_mut(usize::from(i)) {
                    *x = true;
                }
            }
        };
        for (_, d) in rules.base_units().iter() {
            mark(&d.uniques.all, &mut own);
        }
        for (_, d) in rules.unit_types().iter() {
            mark(&d.uniques.all, &mut own);
        }
        for (_, d) in rules.promotions().iter() {
            mark(&d.uniques.all, &mut own);
        }
        for (_, d) in rules.terrains().iter() {
            mark(&d.uniques.all, &mut own);
        }
        let mut resource = vec![false; own.len()];
        for (_, d) in rules.resources().iter() {
            mark(&d.uniques.all, &mut resource);
        }
        let mut out = Self::default();
        let mut deps = CondDeps::empty();
        for (id, u) in t.iter() {
            let Some(ty) = t.meta(id).ty.filter(|ty| Self::TYPES.contains(ty)) else { continue };
            deps |= u.deps();
            if ty != UniqueType::Sight {
                continue;
            }
            let i = usize::from(id.0);
            if resource.get(i).copied().unwrap_or(false) {
                out.resource_sources = true;
            } else if !own.get(i).copied().unwrap_or(false) {
                out.civ_sources = true;
            }
        }
        for (tid, d) in rules.terrains().iter() {
            if d.uniques.ids().any(|u| t.meta(u).ty == Some(UniqueType::Sight)) {
                out.terrains.insert(tid);
            }
        }
        if deps.contains(CondDeps::RESOURCES) {
            out.resource_sources = true;
        }
        out.civ_deps = deps.intersection(CondDeps::CIV_LEVEL).difference(CondDeps::RESOURCES);
        out.tile = deps.intersects(CondDeps::TILE | CondDeps::CITY);
        out.anywhere = deps.intersects(CondDeps::CITY | CondDeps::COMBAT | CondDeps::MAP);
        out
    }

    /// Whether anything but a unit's own changes can change its sight.
    #[must_use]
    pub fn civ_level(&self) -> bool {
        self.civ_sources || self.resource_sources || !self.civ_deps.is_empty() || self.anywhere
    }
}

/// One civilization's sight.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CivVis {
    /// How many of its sources see each tile; empty until the first does.
    count: Vec<u16>,
    /// The tiles it sees: those counted.
    visible: BitSet,
    /// When its units' sight was last found current against what the civilization gives them.
    stamp: Rev,
    /// The `[n] Sight` uniques its index held then, with their copies: what its units' sight
    /// reads of it.
    mods: Vec<(UniqueId, u16)>,
}

/// What every civilization sees now (DESIGN.md 6.9): derived, never saved.
#[derive(Debug, Default)]
pub struct Visibility {
    civ: PlayerVec<CivVis>,
    sources: BTreeMap<SourceKey, VisSource>,
    heights: Heights,
    los: RefCell<LosCache>,
    rules: SightRules,
    /// The revision at which every civilization's sight uniques were last found current.
    pub(crate) checked: Rev,
    /// The tiles that came into sight since the last settle, by civilization, in the order they
    /// did: what a move that stops when an enemy comes into view reads.
    newly_seen: Vec<(PlayerId, TileIdx)>,
}

impl Clone for Visibility {
    fn clone(&self) -> Self {
        Self {
            civ: self.civ.clone(),
            sources: self.sources.clone(),
            heights: self.heights.clone(),
            los: RefCell::new(self.los.borrow().clone()),
            rules: self.rules,
            checked: self.checked,
            newly_seen: self.newly_seen.clone(),
        }
    }
}

impl Visibility {
    /// Nobody sees anything yet: the counts of `st` before any source is registered.
    #[must_use]
    pub fn new(rules: &Ruleset, st: &State) -> Self {
        Self {
            civ: st.players().ids().map(|_| CivVis::default()).collect(),
            sources: BTreeMap::new(),
            heights: Heights::new(rules, st.tiles()),
            los: RefCell::new(LosCache::default()),
            rules: SightRules::new(rules),
            checked: Rev::NEVER,
            newly_seen: Vec::new(),
        }
    }

    /// Whether `p` sees tile `t` now.
    #[must_use]
    #[inline]
    pub fn sees(&self, p: PlayerId, t: TileIdx) -> bool {
        self.civ.get(p).is_some_and(|v| v.visible.contains(t.0))
    }

    /// The tiles `p` sees now.
    #[must_use]
    pub fn visible(&self, p: PlayerId) -> Option<&BitSet> {
        self.civ.get(p).map(|v| &v.visible)
    }

    /// How many of `p`'s sources see tile `t`.
    #[must_use]
    pub fn count(&self, p: PlayerId, t: TileIdx) -> u16 {
        self.civ.get(p).and_then(|v| v.count.get(t.0 as usize)).copied().unwrap_or(0)
    }

    /// Every player that sees tile `t`, in id order.
    pub fn seers(&self, t: TileIdx) -> impl Iterator<Item = PlayerId> + '_ {
        self.civ.iter().filter(move |(_, v)| v.visible.contains(t.0)).map(|(p, _)| p)
    }

    /// A source, if it is registered.
    #[must_use]
    pub fn source(&self, k: SourceKey) -> Option<&VisSource> {
        self.sources.get(&k)
    }

    /// Every registered source, in key order.
    pub fn sources(&self) -> impl Iterator<Item = (SourceKey, &VisSource)> {
        self.sources.iter().map(|(&k, s)| (k, s))
    }

    /// The registered sources in a range of keys, in key order.
    pub(crate) fn sources_in(
        &self,
        range: core::ops::RangeInclusive<SourceKey>,
    ) -> impl Iterator<Item = (SourceKey, &VisSource)> {
        self.sources.range(range).map(|(&k, s)| (k, s))
    }

    /// Every city-state city seen by an ally: the viewer, then the city, in key order.
    pub(crate) fn allied_views(&self) -> impl Iterator<Item = (PlayerId, CityId)> + '_ {
        use core::ops::Bound;
        let lo = SourceKey::AllyCity(PlayerId(0), CityId::FIRST);
        let hi = SourceKey::Spy(PlayerId(0), 0);
        self.sources.range((Bound::Included(lo), Bound::Excluded(hi))).filter_map(|(k, _)| match *k
        {
            SourceKey::AllyCity(v, c) => Some((v, c)),
            _ => None,
        })
    }

    /// The tiles' heights for sight.
    #[must_use]
    pub const fn heights(&self) -> &Heights {
        &self.heights
    }

    /// What the ruleset's sight uniques read.
    #[must_use]
    pub const fn sight_rules(&self) -> &SightRules {
        &self.rules
    }

    /// Whether a change to a tile's yield inputs can change what units on it see: the sight
    /// uniques read the tile in context.
    #[must_use]
    pub const fn tile_sensitive(&self) -> bool {
        self.rules.tile
    }

    /// The tiles visible from `idx` at `sight`, sorted, from the line-of-sight cache.
    pub fn line_of_sight(
        &self,
        grid: &HexGrid,
        idx: TileIdx,
        sight: u32,
        for_attack: bool,
    ) -> Arc<[TileIdx]> {
        self.los.borrow_mut().get(grid, &self.heights, idx, sight, for_attack)
    }

    /// How many line-of-sight answers the cache holds.
    #[must_use]
    pub fn los_cached(&self) -> usize {
        self.los.borrow().len()
    }

    /// Reads tile `t`'s heights again after its terrains changed, and forgets the line-of-sight
    /// answers that read them.
    pub(crate) fn height_changed(
        &mut self,
        rules: &Ruleset,
        grid: &HexGrid,
        t: TileIdx,
        tile: &Tile,
    ) {
        self.heights.update(rules, t, tile);
        self.los.get_mut().evict_near(grid, t);
    }

    /// A civilization's sight stamp and the sight uniques its index held then.
    pub(crate) fn stamp(&self, p: PlayerId) -> (Rev, &[(UniqueId, u16)]) {
        self.civ.get(p).map_or((Rev::NEVER, &[][..]), |v| (v.stamp, v.mods.as_slice()))
    }

    /// Records that `p`'s units' sight was found current at `stamp`, with these sight uniques.
    pub(crate) fn set_stamp(&mut self, p: PlayerId, stamp: Rev, mods: Vec<(UniqueId, u16)>) {
        if let Some(v) = self.civ.get_mut(p) {
            v.stamp = stamp;
            v.mods = mods;
        }
    }

    /// Registers, replaces or (with `None`) drops a source, and appends to `out` every tile
    /// whose count left or reached zero, in the order they did. The new tiles are counted
    /// before the old ones are taken away.
    pub(crate) fn set(
        &mut self,
        size: u32,
        key: SourceKey,
        new: Option<VisSource>,
        out: &mut Vec<Transition>,
    ) {
        let old = self.sources.remove(&key);
        match (&old, &new) {
            (Some(o), Some(n)) if o.owner == n.owner => {
                if !Arc::ptr_eq(&o.footprint, &n.footprint) && o.footprint != n.footprint {
                    self.diff(size, n.owner, &o.footprint, &n.footprint, out);
                }
            }
            _ => {
                if let Some(n) = &new {
                    for &t in n.footprint.iter() {
                        self.inc(size, n.owner, t, out);
                    }
                }
                if let Some(o) = &old {
                    for &t in o.footprint.iter() {
                        self.dec(o.owner, t, out);
                    }
                }
            }
        }
        if let Some(n) = new {
            self.sources.insert(key, n);
        }
    }

    /// Counts the tiles only `new` has, then takes away those only `old` has: both sorted.
    fn diff(
        &mut self,
        size: u32,
        p: PlayerId,
        old: &[TileIdx],
        new: &[TileIdx],
        out: &mut Vec<Transition>,
    ) {
        let (mut i, mut j) = (0, 0);
        while j < new.len() {
            if i < old.len() && old[i] < new[j] {
                i += 1;
            } else if i < old.len() && old[i] == new[j] {
                i += 1;
                j += 1;
            } else {
                self.inc(size, p, new[j], out);
                j += 1;
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < old.len() {
            if j < new.len() && new[j] < old[i] {
                j += 1;
            } else if j < new.len() && new[j] == old[i] {
                i += 1;
                j += 1;
            } else {
                self.dec(p, old[i], out);
                i += 1;
            }
        }
    }

    fn inc(&mut self, size: u32, p: PlayerId, t: TileIdx, out: &mut Vec<Transition>) {
        let Some(v) = self.civ.get_mut(p) else { return };
        if v.count.is_empty() {
            v.count = vec![0; size as usize];
        }
        let Some(c) = v.count.get_mut(t.0 as usize) else { return };
        debug_assert!(
            *c < u16::MAX,
            "more sources of player {} see tile {} than a count holds",
            p.0,
            t.0
        );
        *c = c.saturating_add(1);
        if *c == 1 {
            v.visible.insert(t.0);
            out.push(Transition { civ: p, tile: t, up: true });
        }
    }

    fn dec(&mut self, p: PlayerId, t: TileIdx, out: &mut Vec<Transition>) {
        let Some(c) = self.civ.get_mut(p).and_then(|v| v.count.get_mut(t.0 as usize)) else {
            debug_assert!(false, "player {} lost sight of tile {} it never counted", p.0, t.0);
            return;
        };
        debug_assert!(*c > 0, "player {} lost sight of tile {} it never counted", p.0, t.0);
        *c = c.saturating_sub(1);
        if *c == 0 {
            if let Some(v) = self.civ.get_mut(p) {
                v.visible.remove(t.0);
            }
            out.push(Transition { civ: p, tile: t, up: false });
        }
    }

    /// Records tiles that came into sight, for [`take_newly_seen`](Self::take_newly_seen).
    pub(crate) fn note_seen(&mut self, seen: impl IntoIterator<Item = (PlayerId, TileIdx)>) {
        self.newly_seen.extend(seen);
    }

    /// The tiles that came into `p`'s sight since the last settle or the last take, in the
    /// order they did, forgetting them.
    pub(crate) fn take_newly_seen(&mut self, p: PlayerId) -> Vec<TileIdx> {
        let mut out = Vec::new();
        self.newly_seen.retain(|&(q, t)| {
            if q == p {
                out.push(t);
                false
            } else {
                true
            }
        });
        out
    }

    /// Forgets what came into sight: a settle point.
    pub(crate) fn clear_newly_seen(&mut self) {
        self.newly_seen.clear();
    }

    /// Where these counts and sources disagree with `cold`, one line each (the cache oracle).
    #[must_use]
    pub fn differences(&self, cold: &Self) -> Vec<String> {
        let mut out = Vec::new();
        for (p, v) in self.civ.iter() {
            let c = cold.civ.get(p);
            if Some(&v.visible) != c.map(|c| &c.visible) {
                out.push(format!("player {}: the visible tiles differ from a rebuild", p.0));
                continue;
            }
            let zero = |x: &[u16]| x.iter().all(|&n| n == 0);
            let same = match c {
                Some(c) if v.count.is_empty() || c.count.is_empty() => {
                    zero(&v.count) && zero(&c.count)
                }
                Some(c) => v.count == c.count,
                None => zero(&v.count),
            };
            if !same {
                out.push(format!("player {}: the sight counts differ from a rebuild", p.0));
            }
        }
        if self.civ.len() != cold.civ.len() {
            out.push("the players with sight differ from a rebuild".to_owned());
        }
        for (k, s) in &self.sources {
            match cold.sources.get(k) {
                None => out.push(format!("{k:?} is a vision source a rebuild does not have")),
                Some(c) if c != s => out.push(format!(
                    "{k:?} differs from a rebuild: {:?} at {} seeing {} tiles, not {:?} at {} \
                     seeing {}",
                    s.sight,
                    s.at.0,
                    s.footprint.len(),
                    c.sight,
                    c.at.0,
                    c.footprint.len()
                )),
                Some(_) => {}
            }
        }
        for k in cold.sources.keys() {
            if !self.sources.contains_key(k) {
                out.push(format!("{k:?} is a vision source that is not registered"));
            }
        }
        if self.heights != cold.heights {
            out.push("the sight heights differ from the tiles'".to_owned());
        }
        out
    }

    /// Lets `p` see `t` without a source, as a test arranges what a civilization sees.
    #[cfg(any(test, feature = "test-ops"))]
    pub fn reveal_for_test(&mut self, p: PlayerId, t: TileIdx) {
        if let Some(v) = self.civ.get_mut(p) {
            v.visible.insert(t.0);
        }
    }
}
