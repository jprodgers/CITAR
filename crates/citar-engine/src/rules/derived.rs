//! Tables derived from the ruleset at load (DESIGN.md 5.3), which Python computed in
//! `Rules._prepare` and `Rules._index` (`rules.py:100-228`) or at each use.
//!
//! A few of them depend on a unique's type: whether a unit is a great person, whether a nation
//! may be chosen for new games, what stats a building raises. They read the compiled uniques by
//! type, as Python's `has_tag` and `get` read placeholders (conditionals and all), so they are
//! derived after the compiler has run. The terrain features' layers come first, before the
//! compiler, which names features by them (`feature_layers`).

use super::Ruleset;
use super::defs::{BuilderClass, ImprovementKind, NationKind, Route, TerrainType};
use super::errors::{Problems, RulesetErrorKind};
use crate::base::ids::{
    BaseUnitId, BuildingId, DifficultyId, EraId, FeatureId, Id, IdVec, ImprovementId, NationId,
    ObjectFilterId, ResourceId, TechId, TerrainId,
};
use crate::base::sets::{FeatureSet, TerrainSet};
use crate::base::stats::{Stat, StatMask};
use crate::unique::{SourceUniques, UniqueData, UniqueTable, UniqueType};

// The objects the engine names (`workers.py:22-25`, `state.py:98`, `cities.py:2147, 2358`,
// `barbarians.py:17`, `automation.py:197`).
const HILL: &str = "Hill";
const FALLOUT: &str = "Fallout";
const ROAD: &str = "Road";
const RAILROAD: &str = "Railroad";
const REMOVE: &str = "Remove ";
const REPAIR: &str = "Repair";
const CANCEL: &str = "Cancel improvement order";
const CITY_CENTER: &str = "City center";
/// The difficulty whose base values the easier AIs play on with `ai_base_values = monotonic`
/// (`economy.py:44-49`).
const PRINCE: &str = "Prince";
const CITY_RUINS: &str = "City ruins";
const ANCIENT_RUINS: &str = "Ancient ruins";
const BARBARIAN_CAMP: &str = "Barbarian encampment";

/// What a technology makes available (`rules.py:192-205`): the units, buildings and
/// improvements it unlocks for everyone (not those unique to one nation), and the resources it
/// reveals.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unlocks {
    pub units: Vec<BaseUnitId>,
    pub buildings: Vec<BuildingId>,
    pub improvements: Vec<ImprovementId>,
    pub reveals: Vec<ResourceId>,
}

/// What is unique to one nation (`rules.py:210-221`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NationUniques {
    /// (the unit replaced, the nation's unit); a unit that replaces nothing replaces itself.
    pub units: Vec<(BaseUnitId, BaseUnitId)>,
    /// (the building replaced, the nation's building), likewise.
    pub buildings: Vec<(BuildingId, BuildingId)>,
    pub improvements: Vec<ImprovementId>,
}

/// The objects the engine refers to by name, resolved once.
///
/// So far the terrain features and improvements that this module's own tables need, and Prince
/// (package 1b-05). Python names more (Worker, Palace, The Wheel, Ancient era, the victories,
/// ...; DESIGN.md 5.3 lists them with their lines): the package that ports such code adds the
/// object here, required or optional as Python treated it, rather than comparing names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Known {
    pub hill: FeatureId,
    pub fallout: FeatureId,
    pub road: ImprovementId,
    pub railroad: ImprovementId,
    pub repair: Option<ImprovementId>,
    pub cancel: Option<ImprovementId>,
    pub city_center: Option<ImprovementId>,
    pub city_ruins: Option<ImprovementId>,
    pub ancient_ruins: Option<ImprovementId>,
    pub barbarian_camp: Option<ImprovementId>,
    /// Prince, the difficulty the easier AIs take their base values from under
    /// `ai_base_values = monotonic` (`economy.py:44-49`); a ruleset without it has no such
    /// floor, as Python's `difficulty_index` read a missing name as the first.
    pub prince: Option<DifficultyId>,
}

/// The tables derived at load that belong to no single object.
#[derive(Clone, Debug, PartialEq)]
pub struct Derived {
    /// Techs by tree column, then by name as UTF-8 bytes, which is Python's code point order
    /// (`rules.py:112`).
    pub tech_order: Vec<TechId>,
    pub unlocks: IdVec<TechId, Unlocks>,
    /// The units that upgrade to each unit (`rules.py:206-209`).
    pub upgrade_from: IdVec<BaseUnitId, Vec<BaseUnitId>>,
    pub nation_uniques: IdVec<NationId, NationUniques>,
    /// Major civilizations a new game may pick (`rules.py:222-223`).
    pub major_nations: Vec<NationId>,
    /// City-states a new game may pick (`rules.py:224-225`).
    pub city_state_nations: Vec<NationId>,
    pub great_person_units: Vec<BaseUnitId>,
    /// One entry per unit that is a spaceship part.
    pub spaceship_parts: Vec<BaseUnitId>,
    /// The terrain features in layer order, Hill lowest and Fallout highest, the rest in file
    /// order: `FeatureId` is a position here, so a `FeatureSet`'s top bit is the top feature.
    pub features: IdVec<FeatureId, TerrainId>,
    /// The improvement filters of each builder class (`BuilderClass` indexes this): the
    /// parameters of its units' `Can build [...] improvements on tiles`, in order.
    pub builder_classes: Vec<Box<[ObjectFilterId]>>,
    /// The `Remove ...` improvements that clear a feature or an improvement, in file order
    /// (`workers.py:55-57`).
    pub feature_removals: Vec<ImprovementId>,
    /// The improvement that removes each feature, if one does.
    pub removal_of: IdVec<FeatureId, Option<ImprovementId>>,
    /// The terrains that are a source of fresh water, a lake or an oasis: those carrying
    /// `Fresh water` (`tiles._is_fresh_source`, `tiles.py:120-124`), which Python looked up per
    /// tile and cached.
    pub fresh_water: TerrainSet,
    /// For each building, the other buildings that count as it in a city
    /// (`cities.contains_building`, `cities.py:107-111`, UnCiv's `containsBuildingOrEquivalent`):
    /// those that replace it, and those that carry its name as a tag, with or without
    /// conditionals. Python compared the names at each ask.
    pub building_equivalents: IdVec<BuildingId, Box<[BuildingId]>>,
    pub known: Known,
}

impl Derived {
    /// Nothing derived yet: what the loader starts from before `derive` fills it in.
    pub(crate) fn empty() -> Self {
        Self {
            tech_order: Vec::new(),
            unlocks: IdVec::new(),
            upgrade_from: IdVec::new(),
            nation_uniques: IdVec::new(),
            major_nations: Vec::new(),
            city_state_nations: Vec::new(),
            great_person_units: Vec::new(),
            spaceship_parts: Vec::new(),
            features: IdVec::new(),
            builder_classes: Vec::new(),
            feature_removals: Vec::new(),
            removal_of: IdVec::new(),
            fresh_water: TerrainSet::new(),
            building_equivalents: IdVec::new(),
            known: Known {
                hill: FeatureId(0),
                fallout: FeatureId(0),
                road: ImprovementId(0),
                railroad: ImprovementId(0),
                repair: None,
                cancel: None,
                city_center: None,
                city_ruins: None,
                ancient_ruins: None,
                barbarian_camp: None,
                prince: None,
            },
        }
    }
}

/// Whether an object has a unique of type `ty`, whatever its conditionals: Python's `has_tag`
/// (`uniques.py:182-188`).
fn has(table: &UniqueTable, uniques: &SourceUniques, ty: UniqueType) -> bool {
    uniques.ids().any(|id| table.meta(id).ty == Some(ty))
}

/// The terrain features in layer order, with Hill's and Fallout's places; see
/// [`feature_layers`].
pub(crate) struct Layers {
    features: IdVec<FeatureId, TerrainId>,
    hill: FeatureId,
    fallout: FeatureId,
}

/// Numbers the terrain features by layer and records each feature's number on its terrain. Runs
/// before the unique compiler, which names features by them.
pub(crate) fn feature_layers(r: &mut Ruleset, p: &mut Problems) -> Option<Layers> {
    let (features, hill, fallout) = layers(r, p)?;
    for (f, &t) in features.iter() {
        r.terrains[t].feature = Some(f);
    }
    Some(Layers { features, hill, fallout })
}

/// Fills in every derived field of `r`'s objects and returns the derived tables. The objects'
/// references are resolved and their uniques compiled.
pub(crate) fn derive(r: &mut Ruleset, layers: Layers, p: &mut Problems) -> Option<Derived> {
    let Layers { features, hill, fallout } = layers;
    for t in r.terrains.as_mut_slice() {
        t.rough = has(&r.uniques, &t.uniques, UniqueType::RoughTerrain);
    }
    let mut fresh_water = TerrainSet::new();
    for (id, t) in r.terrains.iter() {
        if has(&r.uniques, &t.uniques, UniqueType::FreshWater) {
            fresh_water.insert(id);
        }
    }

    let improvement_names: Vec<Box<str>> =
        r.improvements.as_slice().iter().map(|i| i.name.clone()).collect();
    let find_improvement = |name: &str| {
        improvement_names.iter().position(|n| &**n == name).and_then(ImprovementId::from_index)
    };
    let feature_named =
        |name: &str| features.iter().find(|&(_, &t)| &*r.terrains[t].name == name).map(|(f, _)| f);
    let mut kinds = Vec::with_capacity(improvement_names.len());
    for name in &improvement_names {
        let kind = match &**name {
            ROAD => ImprovementKind::Route(Route::Road),
            RAILROAD => ImprovementKind::Route(Route::Railroad),
            REPAIR => ImprovementKind::Repair,
            CANCEL => ImprovementKind::Cancel,
            n => match n.strip_prefix(REMOVE) {
                None => ImprovementKind::Normal,
                Some(ROAD) => ImprovementKind::RemoveRoute(Route::Road),
                Some(RAILROAD) => ImprovementKind::RemoveRoute(Route::Railroad),
                Some(what) => {
                    if let Some(f) = feature_named(what) {
                        ImprovementKind::RemoveFeature(f)
                    } else if let Some(i) = find_improvement(what) {
                        ImprovementKind::RemoveImprovement(i)
                    } else {
                        p.push(
                            RulesetErrorKind::UnknownReference,
                            "ruleset/improvements.json",
                            n,
                            format!("{what:?} is no feature, route or improvement to remove"),
                        );
                        ImprovementKind::Normal
                    }
                }
            },
        };
        kinds.push(kind);
    }
    let mut feature_removals = Vec::new();
    let mut removal_of = IdVec::from_elem(None, features.len());
    for ((id, imp), kind) in r.improvements.iter_mut().zip(kinds) {
        imp.kind = kind;
        imp.great = has(&r.uniques, &imp.uniques, UniqueType::GreatImprovement);
        match kind {
            ImprovementKind::RemoveFeature(f) => {
                feature_removals.push(id);
                removal_of[f] = Some(id);
            }
            ImprovementKind::RemoveImprovement(_) => feature_removals.push(id),
            _ => {}
        }
    }
    let road = required(find_improvement(ROAD), ROAD, p);
    let railroad = required(find_improvement(RAILROAD), RAILROAD, p);
    let (Some(road), Some(railroad)) = (road, railroad) else { return None };
    let known = Known {
        hill,
        fallout,
        road,
        railroad,
        repair: find_improvement(REPAIR),
        cancel: find_improvement(CANCEL),
        city_center: find_improvement(CITY_CENTER),
        city_ruins: find_improvement(CITY_RUINS),
        ancient_ruins: find_improvement(ANCIENT_RUINS),
        barbarian_camp: find_improvement(BARBARIAN_CAMP),
        prince: r.difficulties.iter().find(|(_, d)| &*d.name == PRINCE).map(|(id, _)| id),
    };

    let UnitLists { great_person_units, spaceship_parts, builder_classes } = derive_units(r, p);
    derive_buildings(r);
    let building_equivalents = building_equivalents(r);

    let mut tech_order: Vec<TechId> = r.techs.ids().collect();
    tech_order.sort_by(|&a, &b| {
        let (ta, tb) = (&r.techs[a], &r.techs[b]);
        (ta.column, ta.name.as_bytes()).cmp(&(tb.column, tb.name.as_bytes()))
    });

    let mut unlocks: IdVec<TechId, Unlocks> = IdVec::from_elem(Unlocks::default(), r.techs.len());
    for (id, u) in r.base_units.iter() {
        if let (Some(t), None) = (u.required_tech, u.unique_to) {
            unlocks[t].units.push(id);
        }
    }
    for (id, b) in r.buildings.iter() {
        if let (Some(t), None) = (b.required_tech, b.unique_to) {
            unlocks[t].buildings.push(id);
        }
    }
    for (id, i) in r.improvements.iter() {
        if let (Some(t), None) = (i.tech_required, i.unique_to) {
            unlocks[t].improvements.push(id);
        }
    }
    for (id, res) in r.resources.iter() {
        if let Some(t) = res.revealed_by {
            unlocks[t].reveals.push(id);
        }
    }

    let mut upgrade_from: IdVec<BaseUnitId, Vec<BaseUnitId>> =
        IdVec::from_elem(Vec::new(), r.base_units.len());
    for (id, u) in r.base_units.iter() {
        if let Some(to) = u.upgrades_to {
            upgrade_from[to].push(id);
        }
    }

    let mut nation_uniques: IdVec<NationId, NationUniques> =
        IdVec::from_elem(NationUniques::default(), r.nations.len());
    for (id, u) in r.base_units.iter() {
        if let Some(n) = u.unique_to {
            put(&mut nation_uniques[n].units, u.replaces.unwrap_or(id), id);
        }
    }
    for (id, b) in r.buildings.iter() {
        if let Some(n) = b.unique_to {
            put(&mut nation_uniques[n].buildings, b.replaces.unwrap_or(id), id);
        }
    }
    for (id, i) in r.improvements.iter() {
        if let Some(n) = i.unique_to {
            nation_uniques[n].improvements.push(id);
        }
    }

    let mut major_nations = Vec::new();
    let mut city_state_nations = Vec::new();
    for (id, n) in r.nations.iter() {
        if has(&r.uniques, &n.uniques, UniqueType::WillNotBeChosenForNewGames) {
            continue;
        }
        match n.kind {
            NationKind::Major => major_nations.push(id),
            NationKind::CityState => city_state_nations.push(id),
            NationKind::Barbarian => {}
        }
    }

    Some(Derived {
        tech_order,
        unlocks,
        upgrade_from,
        nation_uniques,
        major_nations,
        city_state_nations,
        great_person_units,
        spaceship_parts,
        features,
        builder_classes,
        feature_removals,
        removal_of,
        fresh_water,
        building_equivalents,
        known,
    })
}

/// Sets `replaced -> unique` as Python's dict assignment did: a later unique for the same
/// replaced object wins, in the first one's place.
fn put<K: PartialEq, V>(map: &mut Vec<(K, V)>, key: K, value: V) {
    match map.iter_mut().find(|(k, _)| *k == key) {
        Some(slot) => slot.1 = value,
        None => map.push((key, value)),
    }
}

fn required<T>(found: Option<T>, name: &str, p: &mut Problems) -> Option<T> {
    if found.is_none() {
        p.push(
            RulesetErrorKind::Missing,
            "ruleset/improvements.json",
            name,
            format!("the engine needs an improvement called {name:?}"),
        );
    }
    found
}

/// The features in layer order, with Hill's and Fallout's ids.
fn layers(
    r: &Ruleset,
    p: &mut Problems,
) -> Option<(IdVec<FeatureId, TerrainId>, FeatureId, FeatureId)> {
    let file = "ruleset/terrains.json";
    let features: Vec<TerrainId> = r
        .terrains
        .iter()
        .filter(|(_, t)| t.kind == TerrainType::TerrainFeature)
        .map(|(id, _)| id)
        .collect();
    let named = |name: &str| features.iter().copied().find(|&t| &*r.terrains[t].name == name);
    let (Some(hill), Some(fallout)) = (named(HILL), named(FALLOUT)) else {
        for name in [HILL, FALLOUT] {
            if named(name).is_none() {
                p.push(
                    RulesetErrorKind::Missing,
                    file,
                    name,
                    format!("the engine needs a terrain feature called {name:?}"),
                );
            }
        }
        return None;
    };
    if features.len() > FeatureSet::CAPACITY {
        p.push(
            RulesetErrorKind::Capacity,
            file,
            "",
            format!(
                "{} terrain features, more than a FeatureSet holds ({}); widen \
                 base::sets::FeatureSet",
                features.len(),
                FeatureSet::CAPACITY
            ),
        );
        return None;
    }
    let mut order = vec![hill];
    order.extend(features.iter().copied().filter(|&t| t != hill && t != fallout));
    order.push(fallout);
    // Hill is first and Fallout last, and there are at most 16 features.
    let top = FeatureId(u8::try_from(order.len() - 1).unwrap_or(u8::MAX));
    Some((order.into_iter().collect(), FeatureId(0), top))
}

/// What `derive_units` finds besides the units' own fields.
struct UnitLists {
    great_person_units: Vec<BaseUnitId>,
    spaceship_parts: Vec<BaseUnitId>,
    builder_classes: Vec<Box<[ObjectFilterId]>>,
}

/// Unit flags, great persons, spaceship parts and builder classes (`rules.py:115-129, 226-227`).
fn derive_units(r: &mut Ruleset, p: &mut Problems) -> UnitLists {
    let table = &r.uniques;
    let mut great = Vec::new();
    let mut parts_list = Vec::new();
    let mut classes: Vec<Box<[ObjectFilterId]>> = Vec::new();
    for (id, u) in r.base_units.iter_mut() {
        let unit_type = &r.unit_types[u.unit_type];
        // A unit has its own uniques and its type's (`rules.py:116-118`).
        let all: Vec<_> = u.uniques.ids().chain(unit_type.uniques.ids()).collect();
        let is = |ty: UniqueType| all.iter().any(|&x| table.meta(x).ty == Some(ty));
        u.domain = unit_type.domain;
        u.ranged = u.ranged_strength > 0;
        u.melee = !u.ranged && u.strength > 0;
        u.military = u.ranged || u.melee;
        u.era = u.required_tech.map_or(EraId(0), |t| r.techs[t].era);
        u.great_person = is(UniqueType::GreatPerson);
        if u.great_person {
            great.push(id);
        }
        if is(UniqueType::SpaceshipPart) {
            parts_list.push(id);
        }
        // The same filter text is one ObjectFilterId, so this dedups by text as Python did.
        let mut filters: Vec<ObjectFilterId> = Vec::new();
        for &x in &all {
            if let UniqueData::BuildImprovements(b) = table.get(x).data
                && !filters.contains(&b.object)
            {
                filters.push(b.object);
            }
        }
        u.builder = if filters.is_empty() {
            None
        } else {
            let filters: Box<[ObjectFilterId]> = filters.into();
            let pos = match classes.iter().position(|c| *c == filters) {
                Some(pos) => pos,
                None => {
                    classes.push(filters);
                    classes.len() - 1
                }
            };
            match u8::try_from(pos) {
                Ok(c) => Some(BuilderClass(c)),
                Err(_) => {
                    p.push(
                        RulesetErrorKind::Capacity,
                        "ruleset/units.json",
                        &u.name,
                        "more than 256 builder classes; widen BuilderClass",
                    );
                    None
                }
            }
        };
    }
    UnitLists { great_person_units: great, spaceship_parts: parts_list, builder_classes: classes }
}

/// For each building, the others that count as it in a city: see
/// [`Derived::building_equivalents`].
fn building_equivalents(r: &Ruleset) -> IdVec<BuildingId, Box<[BuildingId]>> {
    let table = &r.uniques;
    r.buildings
        .iter()
        .map(|(b, def)| {
            let tag = table.tag_named(&def.name);
            r.buildings
                .iter()
                .filter(|&(x, d)| {
                    x != b
                        && (d.replaces == Some(b)
                            || tag.is_some_and(|t| {
                                d.uniques.tags.contains(t) || d.uniques.cond_tags.contains(t)
                            }))
                })
                .map(|(x, _)| x)
                .collect()
        })
        .collect()
}

/// Wonder flags and the stats each building raises (`rules.py:144-149, 169-180`).
fn derive_buildings(r: &mut Ruleset) {
    let table = &r.uniques;
    for b in r.buildings.as_mut_slice() {
        b.any_wonder = b.is_wonder || b.is_national_wonder;
        let mut mask = StatMask::EMPTY;
        for s in Stat::ALL {
            if b.stats[s] > 0.0 || b.percent_stat_bonus[s] > 0.0 {
                mask.insert(s);
            }
        }
        for id in b.uniques.ids() {
            let stats = match table.get(id).data {
                UniqueData::Stats(x) => x.stats,
                UniqueData::StatsFromTiles(x) => x.stats,
                UniqueData::StatsPerPopulation(x) => x.stats,
                _ => continue,
            };
            for (st, v) in table.stats(stats).iter() {
                if v > 0.0 {
                    mask.insert(st);
                }
            }
        }
        if has(table, &b.uniques, UniqueType::RemovesAnnexUnhappiness) {
            mask.insert(Stat::Happiness);
        }
        b.stat_related = mask;
    }
}
