//! Static filters: filters over rule objects, whose answer never changes in a game, evaluated once
//! at load into a set of objects (DESIGN.md 5.7).
//!
//! Ports the single-term predicates of `uniques.py` for the nine static domains, and the one the
//! civilization filter asks of nations:
//! - base units, `_base_unit_single` (`uniques.py:490-525`);
//! - buildings, `_building_single` (`uniques.py:626-646`);
//! - terrains, `_terrain_single` (`uniques.py:344-360`);
//! - improvements, `improvement_matches` (`uniques.py:363-370`);
//! - resources, `resource_matches` (`uniques.py:373-388`);
//! - techs, `tech_matches` (`uniques.py:607-610`);
//! - eras, `era_matches` (`uniques.py:594-604`), which Python applied to one term only and which
//!   takes the grammar here like every other domain;
//! - policies, `policy_matches` (`uniques.py:649-655`);
//! - promotions, the promotion lines of `_unit_single` (`uniques.py:552-557`);
//! - nations, the nation lines of `civ_matches` (`uniques.py:587-589`).
//!
//! Python evaluated these 9.7 million times in a game through `multi_filter`, with a per-object
//! `_filter_cache` (`rules.py:127, 147`); here each term is evaluated once over every object,
//! `{a} {b}` and `non-[a]` combine the sets, and a lookup at runtime is one bit test.
//!
//! `has_tag(term)` reads the source's tags ([`SourceUniques::tags`] and `cond_tags`), which the
//! compiler made for every parameterless text a filter names (DESIGN.md 5.6).

use super::super::table::{SourceUniques, StaticDomain};
use super::expr::Expr;
use super::parse::{self, TooDeep};
use crate::base::collections::DetMap;
use crate::base::ids::{Id, TagId};
use crate::base::sets::{BitSet, IdSet};
use crate::base::stats::Stat;
use crate::rules::Ruleset;
use crate::rules::defs::{
    BaseUnitDef, Domain, ImprovementKind, PolicyKind, ResourceType, TerrainType,
};
use crate::unique::UniqueType;

/// `ALL` (`uniques.py:333`).
fn is_all(s: &str) -> bool {
    matches!(s, "All" | "all")
}

impl StaticDomain {
    /// Every domain.
    pub const ALL: [Self; 10] = [
        Self::BaseUnit,
        Self::Building,
        Self::Terrain,
        Self::Improvement,
        Self::Resource,
        Self::Tech,
        Self::Era,
        Self::Policy,
        Self::Promotion,
        Self::Nation,
    ];

    /// How many objects of this domain the ruleset has: the size of its sets.
    #[must_use]
    pub fn size(self, r: &Ruleset) -> usize {
        match self {
            Self::BaseUnit => r.base_units().len(),
            Self::Building => r.buildings().len(),
            Self::Terrain => r.terrains().len(),
            Self::Improvement => r.improvements().len(),
            Self::Resource => r.resources().len(),
            Self::Tech => r.techs().len(),
            Self::Era => r.eras().len(),
            Self::Policy => r.policies().len(),
            Self::Promotion => r.promotions().len(),
            Self::Nation => r.nations().len(),
        }
    }

    /// The domain's name, as reports and the recorded truth tables write it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::BaseUnit => "BaseUnit",
            Self::Building => "Building",
            Self::Terrain => "Terrain",
            Self::Improvement => "Improvement",
            Self::Resource => "Resource",
            Self::Tech => "Tech",
            Self::Era => "Era",
            Self::Policy => "Policy",
            Self::Promotion => "Promotion",
            Self::Nation => "Nation",
        }
    }
}

/// The objects of `domain` that `text` selects, by index.
///
/// # Errors
/// [`TooDeep`] for a filter nested deeper than [`parse::MAX_DEPTH`].
pub fn members(rules: &Ruleset, domain: StaticDomain, text: &str) -> Result<BitSet, TooDeep> {
    Statics::new(rules).filter(domain, text)
}

/// Evaluates static terms and filters against one ruleset, remembering each term's set.
pub(crate) struct Statics<'r> {
    rules: &'r Ruleset,
    terms: DetMap<(StaticDomain, Box<str>), BitSet>,
}

impl<'r> Statics<'r> {
    pub(crate) fn new(rules: &'r Ruleset) -> Self {
        Self { rules, terms: DetMap::default() }
    }

    pub(crate) fn rules(&self) -> &'r Ruleset {
        self.rules
    }

    /// The objects a whole filter selects: its terms' sets, combined as the grammar says.
    pub(crate) fn filter(&mut self, domain: StaticDomain, text: &str) -> Result<BitSet, TooDeep> {
        let tree = parse::parse(text)?;
        let n = domain.size(self.rules);
        Ok(self.combine(domain, &tree, n))
    }

    fn combine(&mut self, domain: StaticDomain, e: &Expr<&str>, n: usize) -> BitSet {
        match e {
            Expr::Const(true) => full(n),
            Expr::Const(false) => BitSet::new(),
            Expr::Leaf(t) => self.term(domain, t),
            Expr::Not(x) => {
                let mut all = full(n);
                all.difference_with(&self.combine(domain, x, n));
                all
            }
            Expr::All(xs) => {
                let mut out = full(n);
                for x in xs {
                    out.intersect_with(&self.combine(domain, x, n));
                }
                out
            }
            Expr::Any(xs) => {
                let mut out = BitSet::new();
                for x in xs {
                    out.union_with(&self.combine(domain, x, n));
                }
                out
            }
        }
    }

    /// The objects of `domain` one term selects.
    pub(crate) fn term(&mut self, domain: StaticDomain, term: &str) -> BitSet {
        if let Some(set) = self.terms.get(&(domain, term.into())) {
            return set.clone();
        }
        let set = self.compute(domain, term);
        self.terms.insert((domain, term.into()), set.clone());
        set
    }

    /// Whether a source carries the tag `term` names, under conditionals or not: Python's
    /// `has_tag` (`uniques.py:182-188`).
    pub(crate) fn tagged(tag: Option<TagId>, u: &SourceUniques) -> bool {
        tag.is_some_and(|t| u.tags.contains(t) || u.cond_tags.contains(t))
    }

    /// The tag a term names, if it is one.
    pub(crate) fn tag(&self, term: &str) -> Option<TagId> {
        self.rules.uniques().tag_named(term)
    }

    #[allow(clippy::too_many_lines, reason = "one arm per domain, each a port of one predicate")]
    fn compute(&mut self, domain: StaticDomain, s: &str) -> BitSet {
        let r = self.rules;
        let tag = self.tag(s);
        let mut out = BitSet::new();
        let mut put = |i: usize, yes: bool| {
            if yes {
                out.insert(u32::try_from(i).unwrap_or(u32::MAX));
            }
        };
        match domain {
            StaticDomain::BaseUnit => {
                // `[x] units` also reads as `X` (`uniques.py:521-524`), repeatedly: iterate, so
                // that a long chain does not recurse.
                let mut cur = s.to_owned();
                let mut acc = BitSet::new();
                loop {
                    acc.union_with(&self.base_unit_direct(&cur));
                    match units_alias(&cur) {
                        Some(base) if base != cur => cur = base,
                        _ => break,
                    }
                }
                return acc;
            }
            StaticDomain::Building => {
                let techs = self.term(StaticDomain::Tech, s);
                for (id, b) in r.buildings().iter() {
                    let yes = if is_all(s) {
                        true
                    } else {
                        match s {
                            "Building" | "Buildings" => !b.any_wonder,
                            "Wonder" | "Wonders" => b.any_wonder,
                            "National Wonder" | "National" => b.is_national_wonder,
                            "World Wonder" | "World" => b.is_wonder,
                            _ => {
                                *b.name == *s
                                    || b.replaces.is_some_and(|x| *r.buildings()[x].name == *s)
                                    || b.required_tech.is_some_and(|t| techs.contains(bit(t)))
                                    || match Stat::from_name(s) {
                                        Some(stat) => b.stat_related.contains(stat),
                                        None => Self::tagged(tag, &b.uniques),
                                    }
                            }
                        }
                    };
                    put(id.index(), yes);
                }
            }
            StaticDomain::Terrain => {
                for (id, t) in r.terrains().iter() {
                    let yes = if is_all(s) || s == "Terrain" {
                        true
                    } else {
                        match s {
                            "Impassable" => t.impassable,
                            "Open terrain" => !t.rough,
                            "Rough terrain" => t.rough,
                            "Natural Wonder" => t.kind == TerrainType::NaturalWonder,
                            "Terrain Feature" => t.kind == TerrainType::TerrainFeature,
                            _ => {
                                *t.name == *s
                                    || terrain_type_name(t.kind) == s
                                    || Self::tagged(tag, &t.uniques)
                            }
                        }
                    };
                    put(id.index(), yes);
                }
            }
            StaticDomain::Improvement => {
                for (id, i) in r.improvements().iter() {
                    let yes = is_all(s)
                        || s == "Improvement"
                        || (s == "All Road" && matches!(i.kind, ImprovementKind::Route(_)))
                        || (matches!(s, "Great Improvement" | "Great") && i.great)
                        || *i.name == *s
                        || Self::tagged(tag, &i.uniques);
                    put(id.index(), yes);
                }
            }
            StaticDomain::Resource => {
                for (id, x) in r.resources().iter() {
                    let kind = resource_type_name(x.kind);
                    let yes = *x.name == *s
                        || matches!(s, "any" | "all" | "All")
                        || s == kind
                        || s.strip_suffix(" resource") == Some(kind)
                        // The stats improving it changes, by name: Python compared the keys of
                        // `improvementStats`, which the ruleset writes only for non-zero values.
                        || x.improvement_stats.nonzero().any(|(stat, _)| stat.name() == s)
                        || Self::tagged(tag, &x.uniques);
                    put(id.index(), yes);
                }
            }
            StaticDomain::Tech => {
                let eras = self.term(StaticDomain::Era, s);
                for (id, t) in r.techs().iter() {
                    let yes = is_all(s)
                        || *t.name == *s
                        || eras.contains(bit(t.era))
                        || Self::tagged(tag, &t.uniques);
                    put(id.index(), yes);
                }
            }
            StaticDomain::Era => {
                let named = |inner: &str| r.lookup::<crate::base::ids::EraId>(inner);
                let pre = s.strip_prefix("pre-[").and_then(|x| x.strip_suffix(']'));
                let post = s.strip_prefix("post-[").and_then(|x| x.strip_suffix(']'));
                for (id, e) in r.eras().iter() {
                    let yes = if s == "any era" || *e.name == *s {
                        true
                    } else if let Some(x) = pre {
                        named(x).is_some_and(|x| id < x)
                    } else if let Some(x) = post {
                        named(x).is_some_and(|x| id > x)
                    } else {
                        false
                    };
                    put(id.index(), yes);
                }
            }
            StaticDomain::Policy => {
                for (id, p) in r.policies().iter() {
                    let branch = match &p.kind {
                        PolicyKind::Branch { .. } => &p.name,
                        PolicyKind::Member { branch, .. } => &r.policies()[*branch].name,
                    };
                    let yes = is_all(s)
                        || *p.name == *s
                        || s.strip_prefix('[').and_then(|x| x.strip_suffix("] branch"))
                            == Some(&**branch)
                        || Self::tagged(tag, &p.uniques);
                    put(id.index(), yes);
                }
            }
            StaticDomain::Promotion => {
                for (id, p) in r.promotions().iter() {
                    put(id.index(), *p.name == *s || Self::tagged(tag, &p.uniques));
                }
            }
            StaticDomain::Nation => {
                for (id, n) in r.nations().iter() {
                    put(id.index(), *n.name == *s || Self::tagged(tag, &n.uniques));
                }
            }
        }
        out
    }

    /// `_base_unit_single` for one term, without the `[x] units` reading.
    fn base_unit_direct(&mut self, s: &str) -> BitSet {
        let r = self.rules;
        let tag = self.tag(s);
        let techs = self.term(StaticDomain::Tech, s);
        let mut out = BitSet::new();
        for (id, u) in r.base_units().iter() {
            let has = |ty: UniqueType| {
                let table = r.uniques();
                u.uniques
                    .ids()
                    .chain(r.unit_types()[u.unit_type].uniques.ids())
                    .any(|x| table.meta(x).ty == Some(ty))
            };
            let yes = if is_all(s) {
                true
            } else {
                match s {
                    "Melee" => u.melee,
                    "Ranged" => u.ranged,
                    "Civilian" => !u.military,
                    "Military" => u.military,
                    "Land" => u.domain == Domain::Land,
                    "Water" => u.domain == Domain::Water,
                    "Air" => u.domain == Domain::Air,
                    "non-air" => u.domain != Domain::Air,
                    "Nuclear Weapon" => has(UniqueType::NuclearWeapon),
                    "Great Person" => u.great_person,
                    "Religious" => has(UniqueType::ReligiousUnit),
                    _ => {
                        named_unit(r, u, s)
                            || u.required_tech.is_some_and(|t| techs.contains(bit(t)))
                            || Self::tagged(tag, &u.uniques)
                    }
                }
            };
            if yes {
                out.insert(bit(id));
            }
        }
        out
    }
}

/// Every index below `n`.
pub(crate) fn full(n: usize) -> BitSet {
    (0..u32::try_from(n).unwrap_or(u32::MAX)).collect()
}

/// A set of indices as a set of ids. The loader holds every table to its set's width.
pub(crate) fn typed<I: Id, const W: usize>(b: &BitSet) -> IdSet<I, W> {
    b.iter().filter_map(|i| I::from_index(i as usize)).collect()
}

/// An id as a set index. Rule ids are at most 16 bits wide.
pub(crate) fn bit<I: Id>(id: I) -> u32 {
    u32::try_from(id.index()).unwrap_or(u32::MAX)
}

/// The unit is called `s`, is of the unit type `s`, or replaces the unit `s`
/// (`uniques.py:514`).
fn named_unit(r: &Ruleset, u: &BaseUnitDef, s: &str) -> bool {
    *u.name == *s
        || *r.unit_types()[u.unit_type].name == *s
        || u.replaces.is_some_and(|x| *r.base_units()[x].name == *s)
}

/// `X units` read as its stem, lower-cased and capitalised as Python's `str.capitalize` does:
/// `Military units` is `Military`, `Great Person units` is `Great person` (`uniques.py:521-524`).
fn units_alias(s: &str) -> Option<String> {
    let stem = s.strip_suffix(" units")?.to_lowercase();
    let mut chars = stem.chars();
    Some(match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    })
}

/// A terrain type as the ruleset writes it.
pub(crate) const fn terrain_type_name(t: TerrainType) -> &'static str {
    match t {
        TerrainType::Land => "Land",
        TerrainType::Water => "Water",
        TerrainType::TerrainFeature => "TerrainFeature",
        TerrainType::NaturalWonder => "NaturalWonder",
    }
}

/// A resource type as the ruleset writes it.
pub(crate) const fn resource_type_name(t: ResourceType) -> &'static str {
    match t {
        ResourceType::Bonus => "Bonus",
        ResourceType::Luxury => "Luxury",
        ResourceType::Strategic => "Strategic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_read_as_their_capitalised_stem() {
        assert_eq!(units_alias("Military units").as_deref(), Some("Military"));
        assert_eq!(units_alias("Great Person units").as_deref(), Some("Great person"));
        assert_eq!(units_alias("MOUNTED units").as_deref(), Some("Mounted"));
        assert_eq!(units_alias(" units").as_deref(), Some(""));
        assert_eq!(units_alias("Military"), None);
    }

    #[test]
    fn full_sets_count_to_n() {
        assert_eq!(full(0).len(), 0);
        assert_eq!(full(130).len(), 130);
        assert!(full(130).contains(129) && !full(130).contains(130));
    }
}
