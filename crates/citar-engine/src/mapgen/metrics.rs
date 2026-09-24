//! Measures of a generated map, for the properties map generation must keep (package 1b-04,
//! gate 1): the rivers reach water and never cross, the ice stays in its band, every strategic
//! resource is present, the luxuries are varied enough, and the starts are about as good as
//! one another.
//!
//! Python's `tests/test_mapgen.py` checked a few seeds of these; here they are measures any
//! test or report can take of any map.

use super::generate::GeneratedMap;
use super::landmass::max_depth;
use super::map::{GenMap, Kit};
use super::options::{IceSides, MapOptions};
use super::resources::{luxury_types_wanted, natural_on};
use super::rivers::Corners;
use super::starts::start_scores;
use crate::base::ids::{IdVec, ResourceId, TileIdx};
use crate::rules::Ruleset;
use crate::rules::defs::ResourceType;
use crate::state::config::{MapEdges, ResourceRule};

/// What the rivers of a map look like, as a graph over the corners where tiles meet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RiverReport {
    /// River edges: pairs of neighbours with a river between them.
    pub edges: usize,
    /// River systems: connected sets of river edges.
    pub systems: usize,
    /// Systems that touch no water at any corner: rivers that end nowhere.
    pub dry: usize,
    /// Systems with a loop: two channels that meet twice, which a river that merged instead of
    /// crossing never has.
    pub loops: usize,
    /// River edges along a water tile.
    pub along_water: usize,
    /// River edges marked on one side only.
    pub one_sided: usize,
}

impl RiverReport {
    /// Whether the rivers are sound: every system reaches water, none loops, and every edge is
    /// between two land tiles and marked on both.
    #[must_use]
    pub const fn sound(&self) -> bool {
        self.dry == 0 && self.loops == 0 && self.along_water == 0 && self.one_sided == 0
    }
}

/// A map's measures need its rules as generation read them.
fn gen_map<'r>(kit: &'r Kit<'r>, opts: &'r MapOptions, map: &GeneratedMap) -> Option<GenMap<'r>> {
    let grid = map.grid().ok()?;
    let mut m = GenMap::new(kit, opts, grid, map.tiles.clone(), vec![false; map.tiles.len()]);
    m.continent.clone_from(&map.continents);
    Some(m)
}

/// The rivers of `map` (`None` for a map whose size the grid refuses).
#[must_use]
pub fn rivers(rules: &Ruleset, map: &GeneratedMap) -> Option<RiverReport> {
    let kit = Kit::new(rules)?;
    let opts = MapOptions::default();
    let m = gen_map(&kit, &opts, map)?;
    let corners = Corners::new(&m.grid);
    let mut report = RiverReport::default();
    // Union-find over the corners, joined along each river edge.
    let mut parent: Vec<u32> = (0..u32::try_from(corners.len()).unwrap_or(0)).collect();
    fn root(parent: &mut [u32], mut x: u32) -> u32 {
        while parent[x as usize] != x {
            let up = parent[parent[x as usize] as usize];
            parent[x as usize] = up;
            x = up;
        }
        x
    }
    let mut on_river = vec![false; corners.len()];
    let mut edge_count: Vec<(u32, u32)> = Vec::new();
    for (v, steps) in corners.steps.iter().enumerate() {
        let v = u32::try_from(v).unwrap_or(u32::MAX);
        for &(u, a, b) in steps {
            // Each edge once: from its lower corner (or from its one corner at the map's edge).
            if u != u32::MAX && u < v {
                continue;
            }
            let (ta, tb) = (TileIdx(a), TileIdx(b));
            let Some(d) = super::rivers::dir(&m.grid, ta, tb) else { continue };
            let here = m.tile(ta).river(d);
            let there = m.tile(tb).river(d.opposite());
            if !here && !there {
                continue;
            }
            report.edges += 1;
            if here != there {
                report.one_sided += 1;
            }
            if m.water(ta) || m.water(tb) {
                report.along_water += 1;
            }
            on_river[v as usize] = true;
            if u != u32::MAX {
                on_river[u as usize] = true;
                edge_count.push((v, u));
                let (rv, ru) = (root(&mut parent, v), root(&mut parent, u));
                if rv != ru {
                    parent[rv as usize] = ru;
                }
            }
        }
    }
    // Per system: corners, edges, and whether a corner touches water.
    let n = corners.len();
    let mut nodes = vec![0usize; n];
    let mut links = vec![0usize; n];
    let mut wet = vec![false; n];
    for (v, _) in on_river.iter().enumerate().filter(|&(_, &on)| on) {
        let r = root(&mut parent, u32::try_from(v).unwrap_or(0)) as usize;
        nodes[r] += 1;
        if corners.tiles[v].iter().any(|&t| m.water(TileIdx(t))) {
            wet[r] = true;
        }
    }
    for &(v, _) in &edge_count {
        let r = root(&mut parent, v) as usize;
        links[r] += 1;
    }
    for r in 0..n {
        if nodes[r] == 0 {
            continue;
        }
        report.systems += 1;
        if !wet[r] {
            report.dry += 1;
        }
        if links[r] >= nodes[r] {
            report.loops += 1;
        }
    }
    Some(report)
}

/// How many tiles carry ice outside the polar band the edges allow: deeper than the band ever
/// reaches from a capped side, or anywhere on a map without caps.
#[must_use]
pub fn ice_outside_band(rules: &Ruleset, map: &GeneratedMap, edges: MapEdges) -> usize {
    let Some(ice) = rules.derived().known.map.ice.and_then(|i| rules.terrains()[i].feature) else {
        return 0;
    };
    let sides = IceSides::of(edges);
    let (w, h) = (u32::from(map.width), u32::from(map.height));
    let (ns, ew) = (max_depth(h), max_depth(w));
    map.tiles
        .iter()
        .enumerate()
        .filter(|(i, t)| {
            if !t.features().contains(ice) {
                return false;
            }
            let i = u32::try_from(*i).unwrap_or(u32::MAX);
            let (x, y) = (i % w, i / w);
            let in_band = (sides.north && y < ns)
                || (sides.south && y >= h - ns)
                || (sides.west && x < ew)
                || (sides.east && x >= w - ew);
            !in_band
        })
        .count()
}

/// How many tiles hold each resource.
#[must_use]
pub fn resource_counts(rules: &Ruleset, map: &GeneratedMap) -> IdVec<ResourceId, u32> {
    let mut out = IdVec::from_elem(0, rules.resources().len());
    for t in &map.tiles {
        if let Some(r) = t.resource() {
            out[r] += 1;
        }
    }
    out
}

/// The strategic resources that generate naturally but are missing from the map; none should
/// be, unless the lobby turned them off or their density to 0.
#[must_use]
pub fn missing_strategic(rules: &Ruleset, map: &GeneratedMap) -> Vec<ResourceId> {
    let counts = resource_counts(rules, map);
    let g = rules.gen_tables();
    rules
        .resources()
        .iter()
        .filter(|(id, x)| x.kind == ResourceType::Strategic && !g.resources[*id].never)
        .map(|(id, _)| id)
        .filter(|&id| counts[id] == 0)
        .collect()
}

/// The luxury variety of a map: how many luxury types it holds, and how many it should, out of
/// those that can go on it (natural somewhere on it, not only a city-state's, not turned off).
#[must_use]
pub fn luxury_variety(rules: &Ruleset, map: &GeneratedMap, opts: &MapOptions) -> (usize, usize) {
    let Some(kit) = Kit::new(rules) else { return (0, 0) };
    let Some(m) = gen_map(&kit, opts, map) else { return (0, 0) };
    let counts = resource_counts(rules, map);
    let types: Vec<ResourceId> = kit
        .luxury
        .iter()
        .copied()
        .filter(|&r| !kit.city_state_only[r])
        .filter(|&r| opts.rule(ResourceType::Luxury, r) != Some(ResourceRule::Off))
        .filter(|&r| counts[r] > 0 || m.all().any(|t| natural_on(&m, r, t)))
        .collect();
    let present = types.iter().filter(|&&r| counts[r] > 0).count();
    (present, luxury_types_wanted(m.grid.size(), types.len()))
}

/// Each major start's score (`_start_score`): the fertility round it, as generation weighed it.
#[must_use]
pub fn start_quality(rules: &Ruleset, map: &GeneratedMap) -> Vec<f64> {
    let Some(kit) = Kit::new(rules) else { return Vec::new() };
    let opts = MapOptions::default();
    let Some(m) = gen_map(&kit, &opts, map) else { return Vec::new() };
    let scores = start_scores(&m);
    map.starts.iter().map(|t| scores[t.0 as usize]).collect()
}
