//! The unique indexes (DESIGN.md 5.12): which uniques hold for a civilization, in a city, for a
//! religion's followers and for a unit, gathered once from their sources and looked up by type.
//!
//! Ports the composition of `economy.civ_umaps` and `civ_index` (`economy.py:77-147`),
//! `cities.local_umaps` (`cities.py:48-66`), `religion.follower_umap` (`religion.py:72-82`) and
//! `units.unit_umap` (`units.py:21-33`). Python rebuilt a placeholder dict of lists for each
//! (`civ_index` 49,582 times in 100 turns) and dropped them through a dozen hooks; here each is a
//! [`Csr`], built from plain inputs by a pure function, which the game keeps in a memo and rebuilds
//! when an input's revision moves (DESIGN.md 6.5).
//!
//! A [`Csr`] holds a source's standing uniques (effects, flags and typed tags) at their own type,
//! and its triggered uniques at their trigger's type, so that one lookup answers both a query for
//! `[]% Strength` and a trigger firing `upon turn start`. Entries are sorted by (type, id), and a
//! unique met more than once (five Monuments) is one entry that counts its copies, so its
//! conditionals are evaluated once. Unique ids follow Python's `civ_umaps` order (DESIGN.md 5.5),
//! so within a type the entries come in Python's order, and the build is the same whatever order
//! its inputs are given in.

use super::generated::UniqueType;
use super::table::{SourceUniques, UFlags, UniqueTable};
use crate::base::ids::{
    BaseUnitId, BeliefId, BuildingId, CityStateTypeId, EraId, NationId, UniqueId,
};
use crate::base::sets::{BuildingSet, PolicySet, PromotionSet, ResourceSet, TechSet};
use crate::rules::Ruleset;

/// One unique in an index, with the number of its sources the index holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Entry {
    pub id: UniqueId,
    /// How many copies: the number of cities with the building that carries it, or of times a
    /// timed unique was granted. At least 1.
    pub n: u16,
}

/// A unique index in compressed sparse rows: the entries of each type are one run, sorted by id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Csr {
    /// The run of type `t` is `start[t]..start[t + 1]`; `UniqueType::COUNT + 1` offsets.
    start: Box<[u16]>,
    entries: Vec<Entry>,
}

impl Default for Csr {
    fn default() -> Self {
        Self { start: vec![0; UniqueType::COUNT + 1].into(), entries: Vec::new() }
    }
}

impl Csr {
    /// The entries of type `ty`: standing uniques of that type, or triggered uniques whose trigger
    /// it is.
    #[must_use]
    #[inline]
    pub fn get(&self, ty: UniqueType) -> &[Entry] {
        let t = ty as usize;
        &self.entries[usize::from(self.start[t])..usize::from(self.start[t + 1])]
    }

    /// Every entry, by type and then id.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// How many entries it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether it holds a unique of type `ty`, conditionals not evaluated (`UniqueMap.has_tag`).
    #[must_use]
    pub fn has(&self, ty: UniqueType) -> bool {
        !self.get(ty).is_empty()
    }

    /// The two indexes as one: every entry of both, by type and then id, the copies of a unique
    /// in both added up. What a civilization's index with its resource layer is
    /// (`economy.civ_umaps`, `economy.py:77-88`), made without gathering its sources again.
    #[must_use]
    pub fn merged(&self, other: &Self) -> Self {
        if other.is_empty() {
            return self.clone();
        }
        let mut start = vec![0u16; UniqueType::COUNT + 1];
        let mut entries = Vec::with_capacity(self.entries.len() + other.entries.len());
        fn run(c: &Csr, t: usize) -> &[Entry] {
            &c.entries[usize::from(c.start[t])..usize::from(c.start[t + 1])]
        }
        for t in 0..UniqueType::COUNT {
            let (a, b) = (run(self, t), run(other, t));
            let (mut i, mut j) = (0, 0);
            while i < a.len() || j < b.len() {
                let next = match (a.get(i), b.get(j)) {
                    (Some(x), Some(y)) if x.id == y.id => {
                        i += 1;
                        j += 1;
                        Entry { id: x.id, n: x.n.saturating_add(y.n) }
                    }
                    (Some(x), Some(y)) if x.id < y.id => {
                        i += 1;
                        *x
                    }
                    (Some(x), None) => {
                        i += 1;
                        *x
                    }
                    (_, Some(y)) => {
                        j += 1;
                        *y
                    }
                    (None, None) => break,
                };
                entries.push(next);
            }
            start[t + 1] = u16::try_from(entries.len()).unwrap_or(u16::MAX);
        }
        Self { start: start.into(), entries }
    }

    /// The index with the entries of `x` added: what [`merged`](Self::merged) with an index of
    /// them gives, made by putting the few entries in their places rather than walking every
    /// type's run.
    #[must_use]
    pub fn plus(&self, x: &Extra) -> Self {
        let mut entries = self.entries.clone();
        let mut start = self.start.clone();
        for &(t, e) in &*x.0 {
            let t = usize::from(t);
            let (lo, hi) = (usize::from(start[t]), usize::from(start[t + 1]));
            match entries[lo..hi].binary_search_by_key(&e.id, |y| y.id) {
                Ok(i) => {
                    let y = &mut entries[lo + i];
                    y.n = y.n.saturating_add(e.n);
                }
                Err(i) => {
                    entries.insert(lo + i, e);
                    for s in &mut start[t + 1..] {
                        *s = s.saturating_add(1);
                    }
                }
            }
        }
        Self { start, entries }
    }
}

/// A few entries to add to an index, each with the type it is indexed at, by type and then id:
/// what one more copy of a source gives ([`building_extra`]), for [`Csr::plus`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Extra(Box<[(u16, Entry)]>);

impl Extra {
    /// Whether it adds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether it adds an entry of type `ty`.
    #[must_use]
    pub fn has(&self, ty: UniqueType) -> bool {
        self.0.iter().any(|&(t, _)| t == ty as u16)
    }

    /// The types it adds entries at, in order, once per entry.
    pub fn types(&self) -> impl Iterator<Item = UniqueType> + '_ {
        self.0.iter().filter_map(|&(t, _)| UniqueType::ALL.get(usize::from(t)).copied())
    }
}

/// The type a unique is indexed at: its trigger's, if it has one, otherwise its own. A tag of no
/// UnCiv type is not indexed: filters read tags from their sources (DESIGN.md 5.6).
fn slot(t: &UniqueTable, id: UniqueId) -> Option<UniqueType> {
    let m = t.meta(id);
    match m.trigger {
        Some(tr) => Some(tr.ty()),
        None => m.ty,
    }
}

/// Collects entries in any order, then sorts and merges them into a [`Csr`].
struct Builder<'r> {
    table: &'r UniqueTable,
    raw: Vec<(u16, UniqueId, u16)>,
}

impl<'r> Builder<'r> {
    fn new(rules: &'r Ruleset) -> Self {
        Self { table: rules.uniques(), raw: Vec::new() }
    }

    fn add(&mut self, id: UniqueId, n: u16) {
        if n == 0 {
            return;
        }
        if let Some(ty) = slot(self.table, id) {
            self.raw.push((ty as u16, id, n));
        }
    }

    /// A source's standing uniques and its triggered ones, `n` times. With `local`, only those
    /// marked [`UFlags::LOCAL`] of the ones Python split (a building's, a resource's: its `local`
    /// partition and its local triggered uniques); without, only the rest.
    fn source(&mut self, s: &SourceUniques, n: u16, local: bool) {
        let standing = if local { &s.local } else { &s.civ };
        for &id in standing.iter() {
            self.add(id, n);
        }
        for &id in s.triggered.iter() {
            if self.table.get(id).flags().contains(UFlags::LOCAL) == local {
                self.add(id, n);
            }
        }
    }

    /// A source's standing and triggered uniques, `n` times, whatever their LOCAL bit: every
    /// source but a building or a resource, whose `in this city` means the city in context.
    fn whole(&mut self, s: &SourceUniques, n: u16) {
        for &id in s.civ.iter().chain(s.triggered.iter()) {
            self.add(id, n);
        }
    }

    /// The entries by type and then id, a unique met more than once counted once with its
    /// copies, with the type of each.
    fn sorted(mut self) -> (Vec<Entry>, Vec<u16>) {
        self.raw.sort_by_key(|&(ty, id, _)| (ty, id));
        let mut entries: Vec<Entry> = Vec::with_capacity(self.raw.len());
        let mut types: Vec<u16> = Vec::with_capacity(self.raw.len());
        for (ty, id, n) in self.raw {
            match entries.last_mut() {
                Some(last) if last.id == id => last.n = last.n.saturating_add(n),
                _ => {
                    entries.push(Entry { id, n });
                    types.push(ty);
                }
            }
        }
        (entries, types)
    }

    fn extra(self) -> Extra {
        let (entries, types) = self.sorted();
        Extra(types.into_iter().zip(entries).collect())
    }

    fn finish(self) -> Csr {
        let (entries, types) = self.sorted();
        let mut start = vec![0u16; UniqueType::COUNT + 1];
        for &ty in &types {
            start[usize::from(ty) + 1] += 1;
        }
        for t in 1..start.len() {
            start[t] += start[t - 1];
        }
        Csr { start: start.into(), entries }
    }
}

/// A city-state's gift to a major civilization (`city_states.bonus_umaps`,
/// `city_states.py:160-173`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CityStateBonus {
    /// Its friend bonuses: influence at the friend level or above.
    Friend,
    /// Its ally bonuses.
    Ally,
}

/// Everything that gives a civilization uniques (`economy.civ_umaps`, `economy.py:77-129`), as
/// `game::derive::civ` gathers it from the state. Lists may come in any order: the index is the
/// same.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CivSources {
    pub nation: NationId,
    /// Each building of the civilization's cities, with the number of its cities that have it.
    /// Their uniques that hold in their own city alone go to the city's index instead.
    pub buildings: Vec<(BuildingId, u16)>,
    /// The branches and policies it has adopted.
    pub policies: PolicySet,
    pub techs: TechSet,
    /// The variant of each timed unique it holds, once per grant still running.
    pub temporary: Vec<UniqueId>,
    pub era: EraId,
    /// For a major civilization, each city-state it has met that counts it a friend or its ally,
    /// by the city-state's type.
    pub city_states: Vec<(CityStateTypeId, CityStateBonus)>,
    /// The founder beliefs of the religion it founded.
    pub founder_beliefs: Vec<BeliefId>,
    /// The resources it has: the resource layer (DESIGN.md 6.6). Empty for the index the supply
    /// itself is computed from.
    pub resources: ResourceSet,
}

impl CivSources {
    /// A civilization of `nation` in `era` with nothing else.
    #[must_use]
    pub fn new(nation: NationId, era: EraId) -> Self {
        Self {
            nation,
            buildings: Vec::new(),
            policies: PolicySet::new(),
            techs: TechSet::new(),
            temporary: Vec::new(),
            era,
            city_states: Vec::new(),
            founder_beliefs: Vec::new(),
            resources: ResourceSet::new(),
        }
    }
}

/// A civilization's unique index.
pub struct CivIndex;

impl CivIndex {
    /// The index of every unique `src` gives the civilization, in `civ_umaps`'s order within each
    /// type: nation, buildings, policies, techs, temporary uniques, era, city-state bonuses,
    /// founder beliefs, resources and the global uniques.
    #[must_use]
    pub fn build(rules: &Ruleset, src: &CivSources) -> Csr {
        let mut b = Builder::new(rules);
        b.whole(&rules.nations()[src.nation].uniques, 1);
        for &(building, n) in &src.buildings {
            b.source(&rules.buildings()[building].uniques, n, false);
        }
        for p in src.policies.iter() {
            b.whole(&rules.policies()[p].uniques, 1);
        }
        for t in src.techs.iter() {
            b.whole(&rules.techs()[t].uniques, 1);
        }
        for &id in &src.temporary {
            b.add(id, 1);
        }
        b.whole(&rules.eras()[src.era].uniques, 1);
        for &(cs, bonus) in &src.city_states {
            let def = &rules.city_state_types()[cs];
            b.whole(
                match bonus {
                    CityStateBonus::Friend => &def.friend,
                    CityStateBonus::Ally => &def.ally,
                },
                1,
            );
        }
        for &belief in &src.founder_beliefs {
            b.whole(&rules.beliefs()[belief].uniques, 1);
        }
        for r in src.resources.iter() {
            b.source(&rules.resources()[r].uniques, 1, false);
        }
        b.whole(rules.global_uniques(), 1);
        b.finish()
    }
}

/// The resource layer of a civilization's index (`economy.resource_umap`, `economy.py:323-336`):
/// the uniques of the resources it has that hold wherever it counts. Their uniques that hold in
/// one city alone go to the index of each city that has the improved resource instead
/// ([`city_local`]).
#[must_use]
pub fn resource_layer(rules: &Ruleset, resources: &ResourceSet) -> Csr {
    let mut b = Builder::new(rules);
    for r in resources.iter() {
        b.source(&rules.resources()[r].uniques, 1, false);
    }
    b.finish()
}

/// The uniques of a civilization's sources that Python's `civ_umaps` held and its [`CivIndex`]
/// leaves out, with their copies: what happens once when the source is gained, unit actions,
/// the AI's weights, map generation's, requirements and inert uniques, tags of no type, and a
/// resource's uniques that hold in one city ([`city_local`] holds them). A building's that hold
/// in its own city Python left out too (`economy.py:103-108`). For the reference checks, which
/// count Python's lists (`economy.civ_index`, `economy.py:135-147`) by placeholder.
#[must_use]
pub fn unindexed(rules: &Ruleset, src: &CivSources) -> Vec<(UniqueId, u16)> {
    let t = rules.uniques();
    let mut out = Vec::new();
    // `taken` holds what the index took of the source; `skip_local` drops what Python dropped.
    let mut add = |s: &SourceUniques, n: u16, local_split: bool, skip_local: bool| {
        for id in s.ids() {
            let local = t.get(id).flags().contains(UFlags::LOCAL);
            if skip_local && local {
                continue;
            }
            let taken = if local_split {
                !local && (s.civ.contains(&id) || s.triggered.contains(&id))
            } else {
                s.civ.contains(&id) || s.triggered.contains(&id)
            };
            if !taken || slot(t, id).is_none() {
                out.push((id, n));
            }
        }
    };
    add(&rules.nations()[src.nation].uniques, 1, false, false);
    for &(building, n) in &src.buildings {
        add(&rules.buildings()[building].uniques, n, true, true);
    }
    for p in src.policies.iter() {
        add(&rules.policies()[p].uniques, 1, false, false);
    }
    for tech in src.techs.iter() {
        add(&rules.techs()[tech].uniques, 1, false, false);
    }
    // The temporary uniques are indexed whole, but a tag's variant, which no index holds.
    let temporary: Vec<UniqueId> =
        src.temporary.iter().copied().filter(|&id| slot(t, id).is_none()).collect();
    add(&rules.eras()[src.era].uniques, 1, false, false);
    for &(cs, bonus) in &src.city_states {
        let def = &rules.city_state_types()[cs];
        let s = match bonus {
            CityStateBonus::Friend => &def.friend,
            CityStateBonus::Ally => &def.ally,
        };
        add(s, 1, false, false);
    }
    for &belief in &src.founder_beliefs {
        add(&rules.beliefs()[belief].uniques, 1, false, false);
    }
    for r in src.resources.iter() {
        add(&rules.resources()[r].uniques, 1, true, false);
    }
    add(rules.global_uniques(), 1, false, false);
    out.extend(temporary.into_iter().map(|id| (id, 1)));
    out
}

/// The index of what holds in one city alone (`cities.local_umaps` without the religion, which
/// is [`follower`]'s): its buildings' local uniques, and those of the resources on the improved
/// tiles it owns (the Marble decision, DESIGN.md 5.12).
#[must_use]
pub fn city_local(rules: &Ruleset, buildings: &BuildingSet, resources: &ResourceSet) -> Csr {
    let mut b = Builder::new(rules);
    for building in buildings.iter() {
        b.source(&rules.buildings()[building].uniques, 1, true);
    }
    for r in resources.iter() {
        b.source(&rules.resources()[r].uniques, 1, true);
    }
    b.finish()
}

/// What one more copy of building `b` adds to an index: to its city's own ([`city_local`]) with
/// `local`, else to its owner's ([`CivIndex`]). Added ([`Csr::plus`]) to the index a city or
/// civilization has, it gives the index with the building, as a rebuild would, since an index is
/// the same whatever order its sources come in and counts the copies of each unique.
#[must_use]
pub fn building_extra(rules: &Ruleset, b: BuildingId, local: bool) -> Extra {
    let mut out = Builder::new(rules);
    out.source(&rules.buildings()[b].uniques, 1, local);
    out.extra()
}

/// The index of what a religion gives the cities that follow it: its follower beliefs'
/// (`religion.follower_umap`). Beliefs may come in any order.
#[must_use]
pub fn follower(rules: &Ruleset, beliefs: &[BeliefId]) -> Csr {
    let mut b = Builder::new(rules);
    for &belief in beliefs {
        b.whole(&rules.beliefs()[belief].uniques, 1);
    }
    b.finish()
}

/// The index of a unit's profile (`units.unit_umap`): its base unit's uniques, its unit type's
/// (which Python copied onto each unit, `rules.py:116-118`) and its promotions', their unit
/// actions included, which a unit's action uniques are found among (`units.usable_action`).
#[must_use]
pub fn unit_profile(rules: &Ruleset, base: BaseUnitId, promotions: &PromotionSet) -> Csr {
    let mut b = Builder::new(rules);
    let def = &rules.base_units()[base];
    let mut add = |s: &SourceUniques| {
        b.whole(s, 1);
        for &id in s.actions.iter() {
            b.add(id, 1);
        }
    };
    add(&def.uniques);
    add(&rules.unit_types()[def.unit_type].uniques);
    for p in promotions.iter() {
        add(&rules.promotions()[p].uniques);
    }
    b.finish()
}

/// How many uniques an index holds of each placeholder, copies counted, for the reference checks'
/// comparison with Python's `civ_index` (`economy.py:132-147`). A triggered unique counts under its
/// own placeholder, as Python's lists held it; the uniques that happen once when their source is
/// gained are not in an index, where Python's lists held them too. In type order.
#[must_use]
pub fn placeholder_counts(rules: &Ruleset, csr: &Csr) -> Vec<(&'static str, u32)> {
    let t = rules.uniques();
    let mut counts = vec![0u32; UniqueType::COUNT];
    for e in csr.entries() {
        if let Some(ty) = t.meta(e.id).ty {
            counts[ty as usize] += u32::from(e.n);
        }
    }
    UniqueType::ALL
        .into_iter()
        .filter(|&ty| counts[ty as usize] > 0)
        .map(|ty| (ty.placeholder(), counts[ty as usize]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_index_answers_every_type_with_nothing() {
        let c = Csr::default();
        assert!(UniqueType::ALL.into_iter().all(|t| c.get(t).is_empty()));
        assert!(c.is_empty());
        assert_eq!(c.merged(&c), c);
    }

    fn csr(entries: &[(UniqueType, u16, u16)]) -> Csr {
        let mut raw: Vec<(u16, UniqueId, u16)> =
            entries.iter().map(|&(t, id, n)| (t as u16, UniqueId(id), n)).collect();
        raw.sort_by_key(|&(t, id, _)| (t, id));
        let mut start = vec![0u16; UniqueType::COUNT + 1];
        for &(t, _, _) in &raw {
            start[usize::from(t) + 1] += 1;
        }
        for t in 1..start.len() {
            start[t] += start[t - 1];
        }
        Csr {
            start: start.into(),
            entries: raw.into_iter().map(|(_, id, n)| Entry { id, n }).collect(),
        }
    }

    #[test]
    fn a_merge_keeps_the_order_and_adds_the_copies() {
        let (s, f) = (UniqueType::Stats, UniqueType::StatPercentBonus);
        let a = csr(&[(s, 3, 1), (s, 9, 2), (f, 4, 1)]);
        let b = csr(&[(s, 5, 1), (s, 9, 1), (f, 1, 1)]);
        let m = a.merged(&b);
        assert_eq!(m, csr(&[(s, 3, 1), (s, 5, 1), (s, 9, 3), (f, 1, 1), (f, 4, 1)]));
        assert_eq!(m, b.merged(&a));
        assert_eq!(a.merged(&Csr::default()), a);
    }

    #[test]
    fn a_few_entries_added_give_what_a_merge_gives() {
        let (s, f, g) = (UniqueType::Stats, UniqueType::StatPercentBonus, UniqueType::FoundCity);
        let a = csr(&[(s, 3, 1), (s, 9, 2), (f, 4, 1)]);
        let x = Extra(Box::new([
            (s as u16, Entry { id: UniqueId(1), n: 1 }),
            (s as u16, Entry { id: UniqueId(9), n: 2 }),
            (f as u16, Entry { id: UniqueId(7), n: 1 }),
            (g as u16, Entry { id: UniqueId(2), n: 1 }),
        ]));
        let b = csr(&[(s, 1, 1), (s, 9, 2), (f, 7, 1), (g, 2, 1)]);
        assert_eq!(a.plus(&x), a.merged(&b));
        assert_eq!(Csr::default().plus(&x), b);
        assert_eq!(a.plus(&Extra::default()), a);
        assert!(x.has(g) && !x.has(UniqueType::Strength) && !x.is_empty());
    }
}
