//! What a turn's decisions share (`BasicBot.context`, basic.py:777-835), and the city reads
//! beside it: `city_defense`, `in_danger`, `needs_garrison` and `_breaks_space_reserve`
//! (836-871).
//!
//! The context is built twice a turn, as Python built it: at the start, and again once the units
//! have moved. It holds ids, read back from the game when a decision needs a unit or a city, so
//! a unit disbanded since is simply not found.
//!
//! The enemies a civilization sees, their weight near each city, a city's defence and the cities
//! that want a garrison are the production advisor's own functions (`game::advisor`), so the
//! bot's danger and the advisor's are one computation, as they were one in Python.

use citar_engine::base::ids::{CityId, PlayerId, TileIdx, UnitId};
use citar_engine::base::num;
use citar_engine::base::sets::ResourceSet;
use citar_engine::base::stats::Stat;
use citar_engine::game::advisor::{self, AdvisorParams};
use citar_engine::game::economy;
use citar_engine::game::{Game, query, tiles};
use citar_engine::state::cities::Constructible;
use smallvec::SmallVec;

use super::Seat;

/// The facts of a turn (`BasicBot.context`).
#[derive(Clone, Debug)]
pub(crate) struct Context {
    /// The civilization's cities, in the game's order.
    pub cities: Vec<CityId>,
    /// Its units, in the game's order.
    pub units: Vec<UnitId>,
    /// Its military units that are not scouts.
    pub military: Vec<UnitId>,
    /// The enemy military units it sees.
    pub hostile: Vec<UnitId>,
    /// The enemy weight near each city, as `cities` lists them.
    pub threat: Vec<f64>,
    /// The enemies within `threat_radius` of each city, as `cities` lists them.
    #[expect(
        dead_code,
        reason = "package 2-03's defence of threatened cities reads it (basic.py:2205)"
    )]
    pub near_enemies: Vec<SmallVec<[UnitId; 4]>>,
    /// Gold per turn.
    pub gpt: f64,
    /// Happiness.
    pub hap: i32,
    /// Its era, by index.
    pub era: usize,
    /// The living majors it has met and is at war with, by id.
    pub wars: Vec<PlayerId>,
    pub supply: i32,
    pub army_target: i32,
    /// At war, or preparing one.
    pub offense: bool,
    /// With `garrison_mode = exposed`, the cities that want a garrison; `None` with `all`.
    pub exposed: Option<Vec<CityId>>,
    /// The luxuries it has some of.
    pub lux_owned: ResourceSet,
    /// The resources in its borders, seen, that lack their improvement.
    pub pending_res: ResourceSet,
}

impl Context {
    /// The enemy weight near city `c` (0 for a city it does not list).
    pub fn threat(&self, c: CityId) -> f64 {
        self.cities.iter().position(|&x| x == c).map_or(0.0, |i| self.threat[i])
    }
}

/// The context of `pid`'s turn as the game is now (`BasicBot.context`, basic.py:777-835).
pub(crate) fn build(g: &Game, pid: PlayerId, s: &Seat<'_>) -> Context {
    let pp = &s.advisor;
    let p = s.params;
    let cities: Vec<CityId> = g.player_cities(pid).map(|c| c.id()).collect();
    let units: Vec<UnitId> = g.player_units(pid).map(|u| u.id()).collect();
    let hostile = advisor::hostile_units(g, pid);
    let radius = pp.threat_radius;
    let mut threat = Vec::with_capacity(cities.len());
    let mut near_enemies = Vec::with_capacity(cities.len());
    for &c in &cities {
        let at = g.city(c).map_or(TileIdx(0), |x| x.tile());
        threat.push(advisor::threat_at(g, &hostile, at, pp));
        near_enemies.push(
            hostile
                .iter()
                .copied()
                .filter(|&u| g.unit(u).is_some_and(|x| g.grid().distance(x.tile(), at) <= radius))
                .collect(),
        );
    }
    let military: Vec<UnitId> = units
        .iter()
        .copied()
        .filter(|&u| g.unit(u).is_some_and(|x| advisor::is_army(g, x.base)))
        .collect();
    let wars: Vec<PlayerId> = g
        .state()
        .players()
        .iter()
        .filter(|&(q, x)| {
            q != pid && x.alive() && x.is_major() && g.has_met(pid, q) && g.at_war(pid, q)
        })
        .map(|(q, _)| q)
        .collect();
    let preparing = s.memory.war_prep.is_some();
    let n = f64::from(u32::try_from(cities.len()).unwrap_or(u32::MAX));
    let threatened =
        f64::from(u32::try_from(threat.iter().filter(|&&t| t > 0.0).count()).unwrap_or(0));
    let war_extra = if !wars.is_empty() || preparing {
        n * p.army_war_per_city + p.army_war_extra
    } else {
        0.0
    };
    let army_target = num::trunc_i32(
        n * (p.army_per_city + p.army_per_city_aggr * s.spec.aggression)
            + p.army_base
            + war_extra
            + p.army_per_threatened_city * threatened,
    );
    let happiness = query::happiness(g, pid);
    let lux_owned: ResourceSet = happiness.luxury_types.iter().copied().collect();
    let mut pending_res = ResourceSet::new();
    for &c in &cities {
        for t in economy::city_tiles(g, c) {
            let Some(tile) = g.tile(t) else { continue };
            let Some(res) = tile.resource() else { continue };
            let seen = g.has_tech(pid, g.rules().resources()[res].revealed_by);
            let improved =
                tile.improvement().is_some_and(|i| tiles::resource_improved_by(g, res, i));
            if seen && !improved {
                pending_res.insert(res);
            }
        }
    }
    let exposed = (pp.garrison_mode == advisor::GarrisonMode::Exposed)
        .then(|| advisor::exposed_cities(g, pid, &cities, &threat, pp));
    Context {
        gpt: query::civ_stats(g, pid).total[Stat::Gold],
        hap: happiness.total,
        era: usize::from(query::era(g, pid).0),
        offense: !wars.is_empty() || preparing,
        wars,
        supply: economy::unit_supply(g, pid),
        army_target,
        exposed,
        lux_owned,
        pending_res,
        cities,
        units,
        military,
        hostile,
        threat,
        near_enemies,
    }
}

/// How well a city is defended (`BasicBot.city_defense`, basic.py:836-844).
pub(crate) fn city_defense(g: &Game, c: CityId) -> f64 {
    advisor::city_defense(g, c)
}

/// Whether the enemies near a city outweigh its defence enough to drop everything for it
/// (`BasicBot.in_danger`, basic.py:869-871).
pub(crate) fn in_danger(g: &Game, ctx: &Context, c: CityId, pp: &AdvisorParams) -> bool {
    ctx.threat(c) > city_defense(g, c) * pp.danger_ratio
}

/// Whether a city should keep a unit in it (`BasicBot.needs_garrison`, basic.py:865-867):
/// always with `garrison_mode = all`.
pub(crate) fn needs_garrison(ctx: &Context, c: CityId) -> bool {
    ctx.exposed.as_ref().is_none_or(|e| e.contains(&c))
}

/// Whether building or upgrading to `item` would use a resource kept for the spaceship
/// (`BasicBot._breaks_space_reserve`, basic.py:846-863).
pub(crate) fn breaks_space_reserve(
    g: &Game,
    pid: PlayerId,
    item: Constructible,
    ctx: &Context,
    pp: &AdvisorParams,
) -> bool {
    advisor::breaks_space_reserve(g, pid, item, ctx.era, pp)
}
