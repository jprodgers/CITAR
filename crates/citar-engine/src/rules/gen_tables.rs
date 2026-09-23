//! Tables for map generation, the AI and victory (DESIGN.md 5.10), built at load from the compiled
//! uniques.
//!
//! - **Map generation.** The 23 map-generation unique types compile into [`TerrainGen`],
//!   [`ResourceGen`] and [`NaturalWonderGen`], with their conditions as [`GenCond`]. Python read
//!   them from each object's unique map while it generated (`mapgen.py:398-526, 546-742, 883-903,
//!   948-1012, 1106-1300`: `_Map.cond`, `occurs`, `_fits`, `_convert_terrains`, `_fertility`,
//!   `_try_wonder`, `_place_wonder`, `_never_generates`, `_natural_on`, `_set_resource`,
//!   `_weighted`, `_strategic`, `_bonus`, `_luxuries`); here they are read once, and never reach a
//!   unique index.
//! - **The AI.** `AiChoiceWeight` becomes a weight list per object the AI chooses (`religion.py:
//!   789-797` read the beliefs'), for the advisor and the bot.
//! - **Victory.** Victory milestones compile to [`Milestone`] (`victory.py:249-270`), where Python
//!   matched their placeholders at every check and answered no for one it did not know.
//! - **Inert uniques**, which nothing reads, are listed for reports; `unique_supported.toml` says
//!   why for each type.
//!
//! What Python let through quietly does not load: a map-generation unique on an object map
//! generation does not read it from, a condition map generation cannot evaluate, or a filter
//! map generation reads that asks more of a tile than its terrain.

use super::Ruleset;
use super::defs::{StartBias, TerrainType};
use super::errors::RulesetErrorKind;
use crate::base::ids::{
    BaseUnitId, BeliefId, BuildingId, IdVec, PolicyId, PromotionId, ResourceId, TechId, TerrainId,
    TileFilterId, TileIdx, UniqueId,
};
use crate::unique::params::RegionType;
use crate::unique::world::TileFacts;
use crate::unique::{
    CondData, Expr, Filters, GenFilter, Role, Source, TileLeaf, Unique, UniqueData,
};

// ---- Map generation -----------------------------------------------------------------------------

/// The conditions of a map-generation unique (`_Map.cond`, `mapgen.py:488-503`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GenCond {
    /// `<in [filter] tiles>`: the tile passes every one.
    pub tiles: Vec<GenFilter>,
    /// `<in tiles without [filter]>`: the tile passes none.
    pub without: Vec<GenFilter>,
    /// `<in [region] Regions>`.
    pub regions: Vec<RegionType>,
    /// `<in all except [region] Regions>`.
    pub except_regions: Vec<RegionType>,
}

impl GenCond {
    /// Whether there are no conditions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
            && self.without.is_empty()
            && self.regions.is_empty()
            && self.except_regions.is_empty()
    }

    /// Whether the conditions hold on tile `t`. Map generation builds no start regions, so no tile
    /// is in one: `in [region] Regions` never holds and `in all except [region] Regions` always
    /// does, as Python skipped the one and ignored the other.
    pub fn holds<W: TileFacts + ?Sized>(&self, filters: &Filters, w: &W, t: TileIdx) -> bool {
        self.regions.is_empty()
            && self.tiles.iter().all(|&f| filters.gen_matches(f, w, t))
            && !self.without.iter().any(|&f| filters.gen_matches(f, w, t))
    }
}

/// `Occurs at temperature between [a] and [b] and humidity between [c] and [d]`: a climate a
/// terrain may be generated in, as written (`mapgen.py:505-525` widens the lower bounds of -1 and
/// 0 slightly when it compares).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Climate {
    pub temperature: (f64, f64),
    pub humidity: (f64, f64),
}

/// A terrain's fertility for start placement (`mapgen.py:948-964`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fertility {
    /// `[n] to Fertility for Map Generation`, summed.
    pub add: i32,
    /// `Always Fertility [n] for Map Generation`: replaces the sum; the last one counts.
    pub fixed: Option<i32>,
}

/// Where a terrain change applies (`mapgen.py:883-900`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Near {
    /// `[River]`: a river runs along the tile itself.
    River,
    /// A neighbour passes the filter.
    Tiles(GenFilter),
}

/// `Becomes [terrain] when adjacent to [filter]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerrainChange {
    pub into: TerrainId,
    pub near: Near,
}

/// What map generation reads of a terrain.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TerrainGen {
    /// Where it may be generated; anywhere when empty.
    pub climates: Vec<Climate>,
    /// `Occurs in chains at high elevations`: a mountain.
    pub chains: bool,
    /// `Occurs in groups around high elevations`: a hill.
    pub groups: bool,
    /// `Rare feature`.
    pub rare: bool,
    /// `Vegetation`.
    pub vegetation: bool,
    /// `Coastal Water`.
    pub coastal_water: bool,
    /// `Doesn't generate naturally`, unconditionally.
    pub never: bool,
    /// `Doesn't generate naturally` where the conditions hold.
    pub not_where: Vec<GenCond>,
    pub fertility: Fertility,
    /// `Becomes [terrain] when adjacent to [filter]`.
    pub changes: Vec<TerrainChange>,
    /// `Every [n] tiles with this terrain will receive a major deposit of a strategic resource.`
    pub major_deposits: Vec<i32>,
    /// `Never receives any resources` where the conditions hold.
    pub blocks_resources: Vec<GenCond>,
}

/// A value that holds where its conditions do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenValue {
    pub value: i32,
    pub cond: GenCond,
}

/// `Deposits in [filter] tiles always provide [n] resources`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepositAmount {
    pub tiles: GenFilter,
    pub amount: i32,
}

/// What map generation reads of a resource.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourceGen {
    /// `Doesn't generate naturally`, unconditionally (`_never_generates`, `mapgen.py:1215-1221`).
    pub never: bool,
    /// `Doesn't generate naturally` where the conditions hold (`_natural_on`).
    pub not_where: Vec<GenCond>,
    /// `Generated on every [n] tiles`.
    pub frequencies: Vec<GenValue>,
    /// `Generated with weight [n]`: a major deposit's weight.
    pub weights: Vec<GenValue>,
    /// `Minor deposits generated with weight [n]`.
    pub minor_weights: Vec<GenValue>,
    /// `Generated near City States with weight [n]`.
    pub city_state_weight: Option<i32>,
    /// `Deposits in [filter] tiles always provide [n] resources`.
    pub amounts: Vec<DepositAmount>,
}

/// `Must be adjacent to [n] [filter] tiles`, or `[min] to [max]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NeighbourCount {
    pub min: i32,
    pub max: i32,
    pub tiles: GenFilter,
}

/// What map generation reads of a natural wonder (`_fits`, `_try_wonder`, `_place_wonder`,
/// `mapgen.py:720-740, 1143-1190`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NaturalWonderGen {
    /// How many neighbours must pass each filter.
    pub neighbours: Vec<NeighbourCount>,
    /// `Must not be on [n] largest landmasses`.
    pub not_on_largest: Vec<i32>,
    /// `Occurs on latitudes from [min] to [max] percent of distance equator to pole`.
    pub latitudes: Vec<(i32, i32)>,
    /// `Occurs in groups of [min] to [max] tiles`; the last one counts.
    pub group: Option<(i32, i32)>,
    /// `Neighboring tiles will convert to [terrain]`, on the neighbours where the conditions
    /// hold.
    pub converts: Vec<(TerrainId, GenCond)>,
}

// ---- The AI ---------------------------------------------------------------------------------------

/// `[n]% weight to this choice for AI decisions`, under its unique's conditionals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AiWeight {
    pub percent: i32,
    /// The unique, whose conditionals decide whether the weight holds.
    pub unique: UniqueId,
}

/// The AI's weights on its choices, per object it chooses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AiWeights {
    pub techs: IdVec<TechId, Vec<AiWeight>>,
    pub policies: IdVec<PolicyId, Vec<AiWeight>>,
    pub beliefs: IdVec<BeliefId, Vec<AiWeight>>,
    pub promotions: IdVec<PromotionId, Vec<AiWeight>>,
    pub buildings: IdVec<BuildingId, Vec<AiWeight>>,
    pub units: IdVec<BaseUnitId, Vec<AiWeight>>,
}

// ---- Victory ------------------------------------------------------------------------------------

/// A victory milestone (`victory.py:249-270`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Milestone {
    /// `Build [building]`: the civilization has built it.
    Build(BuildingId),
    /// `Anyone should build [building]`: some civilization has.
    AnyoneBuilds(BuildingId),
    /// `Add all [spaceship parts] in capital`: the spaceship is complete.
    SpaceshipComplete,
    /// `Complete [n] Policy branches`.
    CompletePolicyBranches(u16),
    /// `Capture all capitals`: it holds every major civilization's original capital.
    CaptureAllCapitals,
    /// `Destroy all players`: it is the last major civilization.
    DestroyAllPlayers,
    /// `Win diplomatic vote`.
    WinDiplomaticVote,
    /// `Have highest score after max turns`.
    HighestScoreAfterMaxTurns,
}

/// Why a milestone's text did not read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BadMilestone {
    /// A building it names is not there; the lookup reported that itself.
    NoBuilding,
    /// Anything else, as a sentence.
    Invalid(String),
}

impl Milestone {
    /// Reads a milestone's text, naming buildings through `building`, which reports a name it
    /// cannot find.
    ///
    /// # Errors
    /// [`BadMilestone::NoBuilding`] when `building` found none; otherwise a sentence: a milestone
    /// the engine does not know, or a count that is not one.
    pub(crate) fn parse(
        text: &str,
        building: &mut dyn FnMut(&str) -> Option<BuildingId>,
    ) -> Result<Self, BadMilestone> {
        let (ph, params) = crate::unique::text::placeholder(text);
        let mut named = |what: &str| building(what).ok_or(BadMilestone::NoBuilding);
        Ok(match (ph.as_str(), params.as_slice()) {
            ("Build []", &[b]) => Self::Build(named(b)?),
            ("Anyone should build []", &[b]) => Self::AnyoneBuilds(named(b)?),
            ("Add all [] in capital", &[_]) => Self::SpaceshipComplete,
            ("Complete [] Policy branches", &[n]) => {
                Self::CompletePolicyBranches(n.parse().map_err(|_| {
                    BadMilestone::Invalid(format!("{n:?} is not a count of branches"))
                })?)
            }
            ("Capture all capitals", []) => Self::CaptureAllCapitals,
            ("Destroy all players", []) => Self::DestroyAllPlayers,
            ("Win diplomatic vote", []) => Self::WinDiplomaticVote,
            ("Have highest score after max turns", []) => Self::HighestScoreAfterMaxTurns,
            _ => {
                return Err(BadMilestone::Invalid(format!(
                    "{text:?} is no milestone the engine knows: Build [building], Anyone should \
                     build [building], Add all [spaceship parts] in capital, Complete [n] Policy \
                     branches, Capture all capitals, Destroy all players, Win diplomatic vote or \
                     Have highest score after max turns"
                )));
            }
        })
    }
}

/// A victory's milestone, compiled, with its text for views.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MilestoneDef {
    pub milestone: Milestone,
    /// As the ruleset wrote it.
    pub text: Box<str>,
}

// ---- The tables ---------------------------------------------------------------------------------

/// Every table of this module.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GenTables {
    /// What map generation reads of each terrain, natural wonders included.
    pub terrains: IdVec<TerrainId, TerrainGen>,
    /// What map generation reads of each resource.
    pub resources: IdVec<ResourceId, ResourceGen>,
    /// What map generation reads of each natural wonder; empty for other terrains.
    pub wonders: IdVec<TerrainId, NaturalWonderGen>,
    pub ai: AiWeights,
    /// Every inert unique, in id order: compiled and checked, read by nothing.
    pub inert: Vec<Inert>,
    /// Every map-generation unique the tables hold, in id order.
    pub placed: Vec<UniqueId>,
}

/// An inert unique, and why nothing reads it (`unique_supported.toml`'s `reason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inert {
    pub unique: UniqueId,
    pub reason: &'static str,
}

/// How the builder reports a problem: the source it is on, its kind and a sentence. The loader
/// names the file and object.
pub(crate) type Report<'a> = dyn FnMut(Source, RulesetErrorKind, String) + 'a;

/// Builds the tables from `r`'s compiled uniques and filters, reporting what does not fit.
pub(crate) fn build(r: &Ruleset, report: &mut Report<'_>) -> GenTables {
    let t = r.uniques();
    let mut g = GenTables {
        terrains: IdVec::from_elem(TerrainGen::default(), r.terrains().len()),
        resources: IdVec::from_elem(ResourceGen::default(), r.resources().len()),
        wonders: IdVec::from_elem(NaturalWonderGen::default(), r.terrains().len()),
        ai: AiWeights {
            techs: IdVec::from_elem(Vec::new(), r.techs().len()),
            policies: IdVec::from_elem(Vec::new(), r.policies().len()),
            beliefs: IdVec::from_elem(Vec::new(), r.beliefs().len()),
            promotions: IdVec::from_elem(Vec::new(), r.promotions().len()),
            buildings: IdVec::from_elem(Vec::new(), r.buildings().len()),
            units: IdVec::from_elem(Vec::new(), r.base_units().len()),
        },
        inert: Vec::new(),
        placed: Vec::new(),
    };
    for (id, u) in t.iter() {
        let m = t.meta(id);
        if matches!(m.source, Source::Temporary(_)) {
            continue;
        }
        match m.role {
            Role::Mapgen => {
                if place(r, &mut g, id, u, m.source, report) {
                    g.placed.push(id);
                }
            }
            Role::Ai => weigh(&mut g.ai, id, u, m.source, report),
            Role::Inert => {
                let reason = m.ty.and_then(|ty| ty.info().support).and_then(|s| s.reason);
                g.inert.push(Inert { unique: id, reason: reason.unwrap_or("") });
            }
            _ => {}
        }
    }
    // Start biases are filters map generation reads (`mapgen.py:1000-1012`).
    for (id, n) in r.nations().iter() {
        for b in &n.start_bias {
            if let StartBias::Prefer(f) | StartBias::Avoid(f) = *b {
                gen_filter(r, f.id(), Source::Nation(id), report);
            }
        }
    }
    g
}

/// The filter as map generation reads it, or a report that it cannot: it asks more of a tile than
/// its terrain.
fn gen_filter(
    r: &Ruleset,
    f: TileFilterId,
    at: Source,
    report: &mut Report<'_>,
) -> Option<GenFilter> {
    let t = r.uniques();
    if t.filters().tile(f).terrain_level {
        return Some(GenFilter::new(f));
    }
    report(
        at,
        RulesetErrorKind::Filter,
        format!(
            "map generation reads a tile's terrain alone, and the filter [{}] asks more of a tile \
             or names no terrain",
            t.tile_filter(f)
        ),
    );
    None
}

/// The conditions of a map-generation unique, or why map generation cannot read them.
fn gen_cond(r: &Ruleset, u: &Unique, at: Source, report: &mut Report<'_>) -> Option<GenCond> {
    let t = r.uniques();
    let mut c = GenCond::default();
    let mut ok = true;
    for x in t.conds(u) {
        match x.data {
            CondData::ConditionalInTiles(p) => match gen_filter(r, p.tiles, at, report) {
                Some(f) => c.tiles.push(f),
                None => ok = false,
            },
            CondData::ConditionalInTilesNot(p) => match gen_filter(r, p.tiles, at, report) {
                Some(f) => c.without.push(f),
                None => ok = false,
            },
            CondData::ConditionalInRegionOfType(p) => c.regions.push(p.region),
            CondData::ConditionalInRegionExceptOfType(p) => c.except_regions.push(p.region),
            _ => {
                report(
                    at,
                    RulesetErrorKind::UniqueModifier,
                    format!(
                        "<{}> is a condition map generation cannot read: it reads tiles and \
                         regions only",
                        t.text(x.text)
                    ),
                );
                ok = false;
            }
        }
    }
    ok.then_some(c)
}

/// Puts one map-generation unique in its table. False, with a report, if it fits none.
#[allow(clippy::too_many_lines, reason = "one arm per map-generation type")]
fn place(
    r: &Ruleset,
    g: &mut GenTables,
    id: UniqueId,
    u: &Unique,
    at: Source,
    report: &mut Report<'_>,
) -> bool {
    use UniqueData as D;
    let t = r.uniques();
    let text = t.text_of(id);
    let Some(cond) = gen_cond(r, u, at, report) else { return false };
    let takes_cond = matches!(
        u.data,
        D::NoNaturalGeneration
            | D::BlocksResources
            | D::ResourceFrequency(_)
            | D::ResourceWeighting(_)
            | D::MinorDepositWeighting(_)
            | D::NaturalWonderConvertNeighbors(_)
    );
    if !takes_cond && !cond.is_empty() {
        report(
            at,
            RulesetErrorKind::UniqueModifier,
            format!("{text:?}: map generation reads this unique without conditions"),
        );
        return false;
    }
    let terrain = match at {
        Source::Terrain(x) => Some(x),
        _ => None,
    };
    let wonder = terrain.filter(|&x| r.terrains()[x].kind == TerrainType::NaturalWonder);
    let resource = match at {
        Source::Resource(x) => Some(x),
        _ => None,
    };
    // The object the unique must be on, or a report that it is not.
    macro_rules! on {
        ($host:expr, $what:literal) => {
            match $host {
                Some(x) => x,
                None => {
                    report(
                        at,
                        RulesetErrorKind::Invalid,
                        format!(
                            concat!("{:?}: map generation reads this unique on ", $what, " only"),
                            text
                        ),
                    );
                    return false;
                }
            }
        };
    }
    match u.data {
        D::AddFertility(x) => {
            let f = &mut g.terrains[on!(terrain, "a terrain")].fertility;
            f.add = f.add.saturating_add(x.fertility);
        }
        D::OverrideFertility(x) => {
            g.terrains[on!(terrain, "a terrain")].fertility.fixed = Some(x.fertility);
        }
        D::BlocksResources => {
            g.terrains[on!(terrain, "a terrain")].blocks_resources.push(cond);
        }
        D::ChangesTerrain(x) => {
            let tr = on!(terrain, "a terrain");
            // `[River]` is the tile's own river, not a neighbour's (`mapgen.py:888-891`).
            let near = if t.filters().tile(x.next_to).terrain == Expr::Leaf(TileLeaf::River) {
                Near::River
            } else {
                let Some(f) = gen_filter(r, x.next_to, at, report) else { return false };
                Near::Tiles(f)
            };
            g.terrains[tr].changes.push(TerrainChange { into: x.into, near });
        }
        D::CoastalWater => g.terrains[on!(terrain, "a terrain")].coastal_water = true,
        D::MajorStrategicFrequency(x) => {
            g.terrains[on!(terrain, "a terrain")].major_deposits.push(x.frequency);
        }
        D::OccursInChains => g.terrains[on!(terrain, "a terrain")].chains = true,
        D::OccursInGroups => g.terrains[on!(terrain, "a terrain")].groups = true,
        D::RareFeature => g.terrains[on!(terrain, "a terrain")].rare = true,
        D::Vegetation => g.terrains[on!(terrain, "a terrain")].vegetation = true,
        D::TileGenerationConditions(x) => {
            let frac = |f| r.fracs().get(f).copied().unwrap_or(0.0);
            g.terrains[on!(terrain, "a terrain")].climates.push(Climate {
                temperature: (frac(x.temperature_min), frac(x.temperature_max)),
                humidity: (frac(x.humidity_min), frac(x.humidity_max)),
            });
        }
        D::NoNaturalGeneration => {
            let (never, not_where) = match (terrain, resource) {
                (Some(x), _) => {
                    let tg = &mut g.terrains[x];
                    (&mut tg.never, &mut tg.not_where)
                }
                (_, Some(x)) => {
                    let rg = &mut g.resources[x];
                    (&mut rg.never, &mut rg.not_where)
                }
                (None, None) => {
                    report(
                        at,
                        RulesetErrorKind::Invalid,
                        format!(
                            "{text:?}: map generation reads this unique on a terrain or a \
                             resource only"
                        ),
                    );
                    return false;
                }
            };
            if cond.is_empty() {
                *never = true;
            } else {
                not_where.push(cond);
            }
        }
        D::LuxuryWeightingForCityStates(x) => {
            g.resources[on!(resource, "a resource")].city_state_weight = Some(x.weight);
        }
        D::MinorDepositWeighting(x) => {
            let value = GenValue { value: x.weight, cond };
            g.resources[on!(resource, "a resource")].minor_weights.push(value);
        }
        D::ResourceAmountOnTiles(x) => {
            let res = on!(resource, "a resource");
            let Some(tiles) = gen_filter(r, x.tiles, at, report) else { return false };
            g.resources[res].amounts.push(DepositAmount { tiles, amount: x.amount });
        }
        D::ResourceFrequency(x) => {
            let value = GenValue { value: x.frequency, cond };
            g.resources[on!(resource, "a resource")].frequencies.push(value);
        }
        D::ResourceWeighting(x) => {
            let value = GenValue { value: x.weight, cond };
            g.resources[on!(resource, "a resource")].weights.push(value);
        }
        D::NaturalWonderConvertNeighbors(x) => {
            g.wonders[on!(wonder, "a natural wonder")].converts.push((x.terrain, cond));
        }
        D::NaturalWonderGroups(x) => {
            g.wonders[on!(wonder, "a natural wonder")].group = Some((x.min, x.max));
        }
        D::NaturalWonderLatitude(x) => {
            g.wonders[on!(wonder, "a natural wonder")].latitudes.push((x.min, x.max));
        }
        D::NaturalWonderNeighborCount(x) => {
            let w = on!(wonder, "a natural wonder");
            let Some(tiles) = gen_filter(r, x.terrain, at, report) else { return false };
            let n = NeighbourCount { min: x.count, max: x.count, tiles };
            g.wonders[w].neighbours.push(n);
        }
        D::NaturalWonderNeighborsRange(x) => {
            let w = on!(wonder, "a natural wonder");
            let Some(tiles) = gen_filter(r, x.terrain, at, report) else { return false };
            let n = NeighbourCount { min: x.min, max: x.max, tiles };
            g.wonders[w].neighbours.push(n);
        }
        D::NaturalWonderSmallerLandmass(x) => {
            g.wonders[on!(wonder, "a natural wonder")].not_on_largest.push(x.count);
        }
        _ => {
            report(
                at,
                RulesetErrorKind::Invalid,
                format!("{text:?}: no map-generation table reads this type yet"),
            );
            return false;
        }
    }
    true
}

/// Puts one AI weight on its object's list.
fn weigh(ai: &mut AiWeights, id: UniqueId, u: &Unique, at: Source, report: &mut Report<'_>) {
    let UniqueData::AiChoiceWeight(x) = u.data else {
        report(at, RulesetErrorKind::Invalid, "no AI table reads this type yet".into());
        return;
    };
    let w = AiWeight { percent: x.percent, unique: id };
    match at {
        Source::Tech(x) => ai.techs[x].push(w),
        Source::Policy(x) => ai.policies[x].push(w),
        Source::Belief(x) => ai.beliefs[x].push(w),
        Source::Promotion(x) => ai.promotions[x].push(w),
        Source::Building(x) => ai.buildings[x].push(w),
        Source::Unit(x) => ai.units[x].push(w),
        _ => report(
            at,
            RulesetErrorKind::Invalid,
            "an AI weight goes on something the AI chooses: a tech, a policy, a belief, a \
             promotion, a building or a unit"
                .into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Milestone, BadMilestone> {
        Milestone::parse(text, &mut |n| (n == "Apollo Program").then_some(BuildingId(7)))
    }

    fn invalid(text: &str) -> String {
        match parse(text) {
            Err(BadMilestone::Invalid(e)) => e,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn milestones_read_as_victory_read_them() {
        assert_eq!(parse("Build [Apollo Program]"), Ok(Milestone::Build(BuildingId(7))));
        assert_eq!(
            parse("Anyone should build [Apollo Program]"),
            Ok(Milestone::AnyoneBuilds(BuildingId(7)))
        );
        assert_eq!(parse("Add all [spaceship parts] in capital"), Ok(Milestone::SpaceshipComplete));
        assert_eq!(parse("Complete [5] Policy branches"), Ok(Milestone::CompletePolicyBranches(5)));
        assert_eq!(parse("Capture all capitals"), Ok(Milestone::CaptureAllCapitals));
        assert_eq!(parse("Destroy all players"), Ok(Milestone::DestroyAllPlayers));
        assert_eq!(parse("Win diplomatic vote"), Ok(Milestone::WinDiplomaticVote));
        assert_eq!(
            parse("Have highest score after max turns"),
            Ok(Milestone::HighestScoreAfterMaxTurns)
        );
        assert_eq!(parse("Build [Pyramid]"), Err(BadMilestone::NoBuilding));
        assert!(invalid("Complete [five] Policy branches").contains("not a count"));
        assert!(invalid("Win the game").contains("no milestone"));
    }

    #[test]
    fn a_map_without_regions_never_holds_a_region_condition() {
        struct Flat;
        impl TileFacts for Flat {
            fn tile_terrains(&self, _: TileIdx) -> crate::base::sets::TerrainSet {
                crate::base::sets::TerrainSet::new()
            }
            fn tile_river(&self, _: TileIdx) -> bool {
                false
            }
            fn tile_fresh_water(&self, _: TileIdx) -> bool {
                false
            }
            fn tile_next_to_coast(&self, _: TileIdx) -> bool {
                false
            }
        }
        let filters = Filters::default();
        let t = TileIdx(0);
        assert!(GenCond::default().holds(&filters, &Flat, t));
        let only = GenCond { regions: vec![RegionType::Hybrid], ..GenCond::default() };
        assert!(!only.holds(&filters, &Flat, t));
        let except = GenCond { except_regions: vec![RegionType::Hybrid], ..GenCond::default() };
        assert!(except.holds(&filters, &Flat, t));
    }
}
