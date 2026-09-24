//! Synthetic game states: every field of `State` filled, deterministically from a seed, and
//! consistent enough to load (package 1a-09).
//!
//! The save and digest tests round-trip them, the golden set `states` checks three of them in,
//! and the digest benchmark times a gargantuan one. Nothing here plays a game: the values are
//! random within what `State::from_parts` and `save::validate` accept, and chosen to reach every
//! form the save has (every enum variant, `-0.0` and subnormal floats, driver memories, carried
//! units, text outside ASCII).

use std::collections::BTreeMap;

use citar_engine::base::ids::{
    AbilityKey, BarbarianLevelId, BaseUnitId, BeliefId, BuildingId, CampId, CityId,
    CityStateTypeId, DealId, DifficultyId, EraId, FeatureId, ImprovementId, MapSizeId, MapTypeId,
    MessageId, NationId, NegotiationId, PlayerId, PolicyId, QuestKindId, ReligionId, ResourceId,
    RuinId, RulesReligionId, SpeedId, TechId, TerrainId, TextId, TileIdx, UniqueId, UnitId,
    VictoryId,
};
use citar_engine::base::sets::{BitSet, FeatureSet, IdSet, MAX_SPECIALISTS, PlayerSet, PlayerVec};
use citar_engine::base::stats::Stat;
use citar_engine::rules::Ruleset;
use citar_engine::rules::defs::{CityStatePersonality, QuestScope};
use citar_engine::state::chronicle::{
    ActionRecord, ChronicleHeads, CivStats, EngineEvent, Event, EventData, EventType, FrameRecord,
    HostHeads, Message, NameRef, RefKind, StatsRow, Thought,
};
use citar_engine::state::cities::{Cities, City, CityFocus, Constructible, Perpetual};
use citar_engine::state::config::{
    AiBaseValues, GameConfig, HostOnly, MapEdges, MapSource, ResourceKindOptions, ResourceRule,
};
use citar_engine::state::diplo::{
    Deal, DealItem, Diplomacy, NegAction, NegEntry, NegStatus, Negotiation, Ongoing, OpinionKey,
    Side, Terms,
};
use citar_engine::state::map::{BuildQueue, BuildStep, MapInfo, RouteBits, Tile, Tiles, WATER};
use citar_engine::state::memory::{CityMemory, TileMemory, TileMemoryLayer};
use citar_engine::state::players::{
    AutoOverrides, Controller, CsPair, DriverMemory, Handicap, Player, PlayerKind, Quest,
    QuestTarget, ReligionProgress, Rgb, Seat, SeatOverrides, Spy, SpyAction, TempUnique, WarQuest,
};
use citar_engine::state::units::{Activity, Unit, Units};
use citar_engine::state::world::{Camp, Religion, ReligionName, UnResult, World};
use citar_engine::state::{IdCounters, Phase, State, StateParts, TileClaim, TurnClock};
use serde_json::json;

/// How big a synthetic state is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    pub width: u16,
    pub height: u16,
    pub majors: u8,
    pub city_states: u8,
    pub cities: u32,
    pub units: u32,
    /// Percent of tiles each major has explored and remembers.
    pub explored: u32,
}

impl Shape {
    /// The smallest map, a few of everything.
    pub const TINY: Self =
        Self { width: 8, height: 8, majors: 2, city_states: 1, cities: 3, units: 6, explored: 40 };
    /// A duel map at mid-game.
    pub const DUEL: Self = Self {
        width: 44,
        height: 28,
        majors: 2,
        city_states: 4,
        cities: 14,
        units: 60,
        explored: 60,
    };
    /// A standard map at mid-game.
    pub const STANDARD: Self = Self {
        width: 76,
        height: 48,
        majors: 6,
        city_states: 12,
        cities: 60,
        units: 300,
        explored: 70,
    };
    /// Gargantuan, late: 24 majors and 32 city-states on 160 by 100 tiles, most of it explored.
    pub const GARGANTUAN: Self = Self {
        width: 160,
        height: 100,
        majors: 24,
        city_states: 32,
        cities: 400,
        units: 2_500,
        explored: 80,
    };
}

/// splitmix64: enough randomness for test data, and the same on every target.
pub struct Gen(u64);

impl Gen {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x5eed_c17a_5a7e_0001)
    }

    pub fn word(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Below `n`, which is at least 1.
    pub fn below(&mut self, n: u64) -> u64 {
        self.word() % n.max(1)
    }

    /// An index below `n`.
    pub fn idx(&mut self, n: usize) -> usize {
        self.below(n as u64) as usize
    }

    /// True `pct` percent of the time.
    pub fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }

    pub fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.idx(xs.len())]
    }

    /// A small integer in `lo..=hi`.
    pub fn int(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo + 1) as u64) as i64
    }

    /// A finite float, often an awkward one: zeros of both signs, a subnormal, a huge one, a
    /// fraction with no short decimal form.
    pub fn float(&mut self) -> f64 {
        match self.below(10) {
            0 => 0.0,
            1 => -0.0,
            2 => f64::from_bits(1),
            3 => 1.0e300 * if self.chance(50) { 1.0 } else { -1.0 },
            4 => 0.1 + 0.2,
            5 | 6 => self.int(-500, 500) as f64,
            _ => (self.word() >> 11) as f64 / (1u64 << 53) as f64 * 1000.0 - 100.0,
        }
    }

    pub fn text(&mut self) -> Box<str> {
        const WORDS: &[&str] =
            &["Rome", "Ñandú", "Hawaiʻi", "東京", "O'Brien", "\"quoted\"", "back\\slash", "🐍", ""];
        let a = self.pick(WORDS);
        let b = self.pick(WORDS);
        format!("{a} {b}").trim().to_owned().into()
    }
}

/// Everything a generator needs to know of the ruleset: its tables' sizes.
struct Tables {
    techs: usize,
    units: usize,
    buildings: usize,
    promotions: usize,
    terrains: usize,
    features: usize,
    resources: usize,
    improvements: usize,
    beliefs: usize,
    policies: usize,
    nations: usize,
    eras: usize,
    specialists: usize,
    speeds: usize,
    difficulties: usize,
    victories: usize,
    cs_types: usize,
    religions: usize,
    quests: usize,
    ruins: usize,
    uniques: usize,
    abilities: usize,
    map_sizes: usize,
    map_types: usize,
    barbarian_levels: usize,
}

impl Tables {
    fn of(r: &Ruleset) -> Self {
        Self {
            techs: r.techs().len(),
            units: r.base_units().len(),
            buildings: r.buildings().len(),
            promotions: r.promotions().len(),
            terrains: r.terrains().len(),
            features: r.derived().features.len(),
            resources: r.resources().len(),
            improvements: r.improvements().len(),
            beliefs: r.beliefs().len(),
            policies: r.policies().len(),
            nations: r.nations().len(),
            eras: r.eras().len(),
            specialists: r.specialists().len().min(MAX_SPECIALISTS),
            speeds: r.speeds().len(),
            difficulties: r.difficulties().len(),
            victories: r.victories().len(),
            cs_types: r.city_state_types().len(),
            religions: r.religions().len(),
            quests: r.quests().len(),
            ruins: r.ruins().len(),
            uniques: r.uniques().len(),
            abilities: r.uniques().ability_count(),
            map_sizes: r.map_sizes().len(),
            map_types: r.constants().map_types.len(),
            barbarian_levels: r.constants().barbarian_levels.len(),
        }
    }
}

/// A random subset of a table, about `pct` percent of it.
fn subset<I: citar_engine::base::ids::Id, const W: usize>(
    g: &mut Gen,
    len: usize,
    pct: u64,
) -> IdSet<I, W> {
    (0..len.min(64 * W)).filter(|_| g.chance(pct)).filter_map(I::from_index).collect()
}

fn opt<T>(g: &mut Gen, pct: u64, f: impl FnOnce(&mut Gen) -> T) -> Option<T> {
    if g.chance(pct) { Some(f(g)) } else { None }
}

fn constructible(g: &mut Gen, t: &Tables) -> Constructible {
    match g.below(3) {
        0 => Constructible::Building(BuildingId(g.below(t.buildings as u64) as u16)),
        1 => Constructible::Unit(BaseUnitId(g.below(t.units as u64) as u16)),
        _ => Constructible::Perpetual(g.pick(&Perpetual::ALL)),
    }
}

/// A state of this shape under `r`, the same for the same seed.
///
/// # Panics
/// If the parts it makes do not fit, which is a bug in this generator.
#[must_use]
pub fn build(r: &'static Ruleset, seed: u64, shape: &Shape) -> State {
    let mut g = Gen::new(seed);
    let t = Tables::of(r);
    let (w, h) = (shape.width, shape.height);
    let size = u32::from(w) * u32::from(h);
    let n = shape.majors + shape.city_states + 1;
    let players: Vec<PlayerId> = (0..n).map(PlayerId).collect();
    let kind = |p: PlayerId| {
        if p.0 < shape.majors {
            PlayerKind::Major
        } else if p.0 < n - 1 {
            PlayerKind::CityState
        } else {
            PlayerKind::Barbarian
        }
    };
    // The last major is eliminated, so it holds nothing.
    let dead = PlayerId(shape.majors - 1);
    let living: Vec<PlayerId> = players.iter().copied().filter(|&p| p != dead).collect();
    let tile = |g: &mut Gen| TileIdx(g.below(u64::from(size)) as u32);

    // Cities first: the tiles they claim follow from them.
    let mut city_ids = Vec::new();
    let mut next = 1u32;
    for _ in 0..shape.cities {
        next += 1 + g.below(3) as u32;
        city_ids.push(CityId::new(next).unwrap_or(CityId::FIRST));
    }
    let city_counter = next + 1 + g.below(4) as u32;
    let mut used_tiles = std::collections::BTreeSet::new();
    let mut city_spots = Vec::new();
    for &id in &city_ids {
        let mut at = tile(&mut g);
        while !used_tiles.insert(at) {
            at = TileIdx((at.0 + 1) % size);
        }
        let owner = g.pick(&living[..living.len() - 1]);
        city_spots.push((id, owner, at));
    }
    let religions = 1 + g.below(6) as usize;
    let camp_counter = 2 + g.below(20) as u32;

    // ---- Tiles
    let mut tiles = Vec::with_capacity(size as usize);
    for _ in 0..size {
        let mut features = FeatureSet::EMPTY;
        for f in 0..t.features {
            if g.chance(15) {
                features.insert(FeatureId(f as u8));
            }
        }
        let res = opt(&mut g, 20, |g| ResourceId(g.below(t.resources as u64) as u8));
        let owner = opt(&mut g, 20, |g| PlayerId(g.below(u64::from(n)) as u8));
        tiles.push(
            Tile::new(TerrainId(g.below(t.terrains as u64) as u8))
                .with_wonder(opt(&mut g, 2, |g| TerrainId(g.below(t.terrains as u64) as u8)))
                .with_resource(res, g.below(8) as u8)
                .with_improvement(opt(&mut g, 25, |g| {
                    ImprovementId(g.below(t.improvements as u64) as u8)
                }))
                .with_route_bits(RouteBits::from_bits(g.below(16) as u8))
                .with_claim(TileClaim { owner, city: None })
                .with_river(g.below(64) as u8)
                .with_features(features),
        );
    }
    for &(id, owner, at) in &city_spots {
        let claim = TileClaim::city(owner, id);
        let i = at.0 as usize;
        tiles[i] = tiles[i].with_claim(claim);
        for k in 1..4u32 {
            let j = ((at.0 + k * 7) % size) as usize;
            if !used_tiles.contains(&TileIdx(j as u32)) {
                tiles[j] = tiles[j].with_claim(claim);
            }
        }
    }
    let mut builds = BTreeMap::new();
    for _ in 0..(size / 50).max(1) {
        let steps: BuildQueue = (0..1 + g.below(3))
            .map(|_| BuildStep {
                improvement: ImprovementId(g.below(t.improvements as u64) as u8),
                turns_left: g.int(-1, 12) as i16,
            })
            .collect();
        builds.insert(tile(&mut g), steps);
    }
    let tiles = Tiles::from_parts(tiles, builds).expect("the build queues are on the map");
    let map = MapInfo {
        width: w,
        height: h,
        wrap_x: g.chance(50),
        wrap_y: h % 2 == 0 && g.chance(30),
        continents: if g.chance(80) {
            (0..size).map(|_| if g.chance(40) { WATER } else { g.below(12) as u16 }).collect()
        } else {
            Vec::new()
        },
    };

    // ---- Cities
    let mut cities = Vec::new();
    for &(id, owner, at) in &city_spots {
        let mut c = City::new(id, g.text(), owner, at, g.int(1, 300) as i32);
        c.founder = g.pick(&players);
        c.previous_owner = opt(&mut g, 30, |g| g.pick(&players));
        c.turn_acquired = g.int(1, 400) as i32;
        c.original_capital = g.chance(20);
        c.pop = g.int(1, 40) as u16;
        c.food = g.float();
        c.culture = g.float();
        c.tiles_claimed = g.below(40) as u16;
        c.tiles_bought = g.below(10) as u16;
        c.buildings = subset(&mut g, t.buildings, 20);
        c.free_buildings = subset(&mut g, t.buildings, 3);
        c.queue = (0..g.below(4)).map(|_| constructible(&mut g, &t)).collect();
        for _ in 0..g.below(4) {
            c.progress.insert(constructible(&mut g, &t), g.float());
        }
        c.overflow = g.float();
        c.bought_this_turn = (0..g.below(2)).map(|_| constructible(&mut g, &t)).collect();
        c.auto_production = g.chance(50);
        let mut worked: Vec<TileIdx> = (0..g.below(8)).map(|_| tile(&mut g)).collect();
        worked.sort();
        worked.dedup();
        c.locked = worked.iter().copied().filter(|_| g.chance(30)).collect();
        c.worked = worked;
        for s in c.specialists.iter_mut().take(t.specialists) {
            *s = g.below(3) as u8;
        }
        c.manual_specialists = g.chance(20);
        c.focus = g.pick(&CityFocus::ALL);
        c.avoid_growth = g.chance(10);
        c.citizens_settled = g.chance(80);
        c.health = g.int(0, 300) as i32;
        c.damaged_turn = g.int(-1, 300) as i32;
        c.attacked = g.chance(10);
        c.sacked_turn = g.pick(&[-1000, 5, 90]);
        c.puppet = g.chance(10);
        c.resistance = g.below(5) as i16;
        c.razing = g.chance(5);
        for rel in 0..religions {
            if g.chance(40) {
                c.set_pressure(Some(ReligionId(rel as u8)), g.int(1, 900) as i32);
            }
        }
        c.religions_adopted =
            (0..religions).filter(|_| g.chance(20)).map(|x| ReligionId(x as u8)).collect();
        c.holy_city_of = opt(&mut g, 5, |g| ReligionId(g.below(religions as u64) as u8));
        c.wltkd = g.below(10) as i16;
        c.demanded_resource = opt(&mut g, 30, |g| ResourceId(g.below(t.resources as u64) as u8));
        c.demand_countdown = g.below(20) as i16;
        cities.push(c);
    }
    let cities = Cities::from_cities(cities).expect("ascending city ids");

    // ---- Units
    let mut units = Vec::new();
    let mut next = 1u32;
    let mut carriers = Vec::new();
    for i in 0..shape.units {
        next += 1 + g.below(2) as u32;
        let id = UnitId::new(next).unwrap_or(UnitId::FIRST);
        let owner = g.pick(&living);
        // Every tenth unit rides on the unit before it.
        let at = match units.last() {
            Some(prev) if i % 10 == 9 => {
                carriers.push((id, Unit::id(prev)));
                Unit::tile(prev)
            }
            _ => tile(&mut g),
        };
        let mut u = Unit::new(
            id,
            BaseUnitId(g.below(t.units as u64) as u16),
            owner,
            at,
            g.int(1, 400) as i32,
        );
        u.hp = g.int(1, 100) as i16;
        u.moves = g.int(0, 300) as i32;
        u.xp = g.int(0, 200) as i32;
        u.promotions = subset(&mut g, t.promotions, 3);
        u.promotion_count = g.below(6) as u8;
        u.pending_promotions = g.below(2) as u8;
        u.fortify = g.below(3) as u8;
        u.activity = opt(&mut g, 60, |g| g.pick(&Activity::ALL));
        u.goto = opt(&mut g, 20, |g| TileIdx(g.below(u64::from(size)) as u32));
        u.path = (0..g.below(5)).map(|_| tile(&mut g)).collect();
        u.order_wait = g.below(3) as u8;
        u.attacks = g.below(2) as u8;
        u.interceptions = g.below(2) as u8;
        u.acted = g.chance(50);
        u.set_up = g.chance(10);
        u.name = opt(&mut g, 10, Gen::text);
        u.camp = opt(&mut g, 10, |g| {
            CampId::new(1 + g.below(u64::from(camp_counter - 1)) as u32).unwrap_or(CampId::FIRST)
        });
        u.religion = opt(&mut g, 10, |g| ReligionId(g.below(religions as u64) as u8));
        u.religious_strength = g.below(1000) as i16;
        u.religious_strength_lost = g.below(100) as i16;
        for _ in 0..g.below(3) {
            if t.abilities > 0 {
                u.use_ability(AbilityKey(g.below(t.abilities as u64) as u16));
            }
        }
        u.origin_city = opt(&mut g, 40, |g| {
            CityId::new(1 + g.below(u64::from(city_counter - 1)) as u32).unwrap_or(CityId::FIRST)
        });
        u.original_owner = opt(&mut g, 10, |g| g.pick(&players));
        u.return_offer = opt(&mut g, 5, |g| g.pick(&players));
        u.explore.target = opt(&mut g, 10, |g| TileIdx(g.below(u64::from(size)) as u32));
        u.explore.recent = (0..g.below(5)).map(|_| tile(&mut g)).collect();
        units.push(u);
    }
    let unit_counter = next + 1 + g.below(5) as u32;
    let mut units = Units::from_units(units, size).expect("units on the map, ascending");
    for (u, carrier) in carriers {
        // A carrier that is itself carried, or cargo that carries, is refused: skip those.
        let _boarded = units.board(u, carrier).ok();
    }

    // ---- Players
    let mut plist = Vec::new();
    for &p in &players {
        let k = kind(p);
        let controller = match k {
            PlayerKind::Major => g.pick(&[
                Controller::Human,
                Controller::Llm,
                Controller::Mcp,
                Controller::Bot,
                Controller::Hybrid,
            ]),
            PlayerKind::CityState => Controller::Minor,
            PlayerKind::Barbarian => Controller::Barbarian,
        };
        let overrides = SeatOverrides {
            handicap: opt(&mut g, 30, |g| g.pick(&[Handicap::Human, Handicap::Ai])),
            auto: AutoOverrides {
                un_vote: opt(&mut g, 30, |g| g.chance(50)),
                conquest: opt(&mut g, 30, |g| g.chance(50)),
                free_picks: opt(&mut g, 30, |g| g.chance(50)),
            },
        };
        let driver = opt(&mut g, 40, |g| {
            let bytes: Vec<u8> = (0..g.below(40)).map(|_| g.word() as u8).collect();
            DriverMemory::new(g.below(4) as u16, g.below(3) as u16, bytes).expect("small")
        });
        let seat = Seat::restore(
            controller,
            opt(&mut g, 50, |g| g.pick(&[Handicap::Human, Handicap::Ai])),
            AutoOverrides {
                un_vote: opt(&mut g, 20, |g| g.chance(50)),
                ..AutoOverrides::default()
            },
            overrides,
            opt(&mut g, 30, |g| DifficultyId(g.below(t.difficulties as u64) as u8)),
            driver,
        );
        let color = Rgb([g.word() as u8, g.word() as u8, g.word() as u8]);
        let mut pl = Player::new(
            p,
            k,
            g.text(),
            NationId(g.below(t.nations as u64) as u16),
            color,
            seat,
            size,
        )
        .restore(p != dead, (p == dead).then_some(g.int(2, 200) as i32));
        pl.leader = g.text();
        let own: Vec<CityId> =
            city_spots.iter().filter(|(_, o, _)| *o == p).map(|(c, _, _)| *c).collect();
        pl.capital = own.first().copied().filter(|_| g.chance(80));
        pl.original_capital = opt(&mut g, 50, |g| city_ids[g.idx(city_ids.len())]);
        pl.founded_city = !own.is_empty();
        pl.city_counter = own.len() as u16;
        pl.start_tile = opt(&mut g, 80, |g| TileIdx(g.below(u64::from(size)) as u32));
        if k != PlayerKind::Barbarian {
            pl.explored =
                (0..size).filter(|_| g.chance(u64::from(shape.explored))).collect::<BitSet>();
        }
        let e = &mut pl.econ;
        e.gold = g.float();
        e.culture = g.float();
        e.faith = g.float();
        e.golden_age_points = g.float();
        e.golden_age_turns = g.below(10) as i32;
        e.golden_ages = g.below(4) as i32;
        e.total_culture = g.int(0, 1 << 40);
        e.total_faith = g.int(0, 1 << 20);
        for x in e.culture_hist.iter_mut().chain(e.science_hist.iter_mut()) {
            *x = g.int(-5, 300) as i32;
        }
        e.last_gold_rate = g.float();
        e.happiness_seen = g.int(-30, 30) as i32;
        let tech = &mut pl.tech;
        tech.known = subset(&mut g, t.techs, 40);
        tech.queue = (0..g.below(4)).map(|_| TechId(g.below(t.techs as u64) as u16)).collect();
        tech.goal = tech.queue.last().copied();
        for _ in 0..g.below(4) {
            tech.progress.insert(TechId(g.below(t.techs as u64) as u16), g.float());
        }
        tech.overflow = g.float();
        tech.free_techs = g.below(3) as i32;
        tech.future_techs = g.below(3) as i32;
        tech.ra_bonus = g.below(500) as i32;
        pl.policy.adopted = subset(&mut g, t.policies, 15);
        pl.policy.adopted_count = pl.policy.adopted.len() as i32;
        pl.policy.free_policies = g.below(2) as i32;
        let gp = &mut pl.gp;
        for _ in 0..g.below(4) {
            gp.points.insert(BaseUnitId(g.below(t.units as u64) as u16), g.float());
            gp.combat_points.insert(BaseUnitId(g.below(t.units as u64) as u16), g.float());
            gp.combat_threshold.insert(BaseUnitId(g.below(t.units as u64) as u16), g.int(0, 900));
        }
        gp.pool_threshold.insert(None, g.int(100, 900));
        if t.uniques > 0 {
            let text: TextId = r.uniques().meta(UniqueId(g.below(t.uniques as u64) as u16)).text;
            gp.pool_threshold.insert(Some(text), g.int(100, 900));
        }
        gp.free = g.below(2) as i32;
        gp.earned = g.below(9) as i32;
        gp.prophets_earned = g.below(3) as i32;
        gp.maya_limited = g.below(2) as i32;
        gp.long_count_pool =
            (0..g.below(3)).map(|_| BaseUnitId(g.below(t.units as u64) as u16)).collect();
        let rel = &mut pl.religion;
        rel.progress = g.pick(&ReligionProgress::ALL);
        rel.founded = opt(&mut g, 30, |g| ReligionId(g.below(religions as u64) as u8));
        for f in &mut rel.free_beliefs {
            *f = g.below(2) as u8;
        }
        rel.choose_pantheon_belief = g.chance(20);
        let civ = &mut pl.civ;
        for _ in 0..g.below(3) {
            if t.uniques > 0 {
                civ.temp_uniques.push(TempUnique {
                    unique: UniqueId(g.below(t.uniques as u64) as u16),
                    turns: g.below(30) as i16,
                });
            }
        }
        for _ in 0..g.below(3) {
            civ.built_increasing.insert(constructible(&mut g, &t), g.below(9) as u16);
            civ.bought_increasing.insert(constructible(&mut g, &t), g.below(9) as u16);
        }
        let mut stat_free: Vec<(Stat, CityId)> = (0..g.below(3))
            .map(|_| (g.pick(&Stat::ALL), city_ids[g.idx(city_ids.len())]))
            .collect();
        stat_free.sort();
        stat_free.dedup();
        civ.free_stat_buildings = stat_free;
        let mut specific: Vec<(BuildingId, CityId)> = (0..g.below(3))
            .map(|_| {
                (BuildingId(g.below(t.buildings as u64) as u16), city_ids[g.idx(city_ids.len())])
            })
            .collect();
        specific.sort();
        specific.dedup();
        civ.free_specific_buildings = specific;
        civ.natural_wonders = subset(&mut g, t.terrains, 5);
        civ.units_gained = subset(&mut g, t.units, 10);
        civ.revolt_in = opt(&mut g, 20, |g| g.below(5) as i16);
        civ.last_ruins = [
            opt(&mut g, 30, |g| RuinId(g.below(t.ruins as u64) as u16)),
            opt(&mut g, 30, |g| RuinId(g.below(t.ruins as u64) as u16)),
        ];
        let mut skip: Vec<TileIdx> = (0..g.below(6)).map(|_| tile(&mut g)).collect();
        skip.sort();
        skip.dedup();
        civ.explore_skip = skip;
        if let Some(m) = pl.major.as_mut() {
            let mut mem = Vec::with_capacity(size as usize);
            for tile in tiles.as_slice() {
                mem.push(if g.chance(u64::from(shape.explored)) {
                    TileMemory::of(tile)
                } else {
                    TileMemory::NONE
                });
            }
            let mut seen = BTreeMap::new();
            for &(_, owner, at) in &city_spots {
                if g.chance(30) {
                    seen.insert(at, CityMemory { name: g.text(), pop: g.below(20) as u16, owner });
                }
            }
            m.memory = TileMemoryLayer::from_parts(mem, seen).expect("on the map");
            m.spies = (0..g.below(3))
                .map(|_| Spy {
                    name: g.text(),
                    rank: g.below(3) as u8,
                    city: opt(&mut g, 60, |g| city_ids[g.idx(city_ids.len())]),
                    action: g.pick(&SpyAction::ALL),
                    turns: g.below(10) as i16,
                    progress: g.below(500) as i32,
                })
                .collect();
            m.spy_eras_earned = subset(&mut g, t.eras, 30);
            for _ in 0..g.below(3) {
                m.spaceship.insert(BaseUnitId(g.below(t.units as u64) as u16), g.below(3) as u16);
            }
            m.notes = g.text();
            m.cs_attacks = g.below(4) as u16;
            m.cs_gp_gift = opt(&mut g, 20, |g| g.below(30) as i16);
        }
        if let Some(cs) = pl.city_state.as_mut() {
            cs.cs_type = opt(&mut g, 90, |g| CityStateTypeId(g.below(t.cs_types as u64) as u8));
            cs.personality = opt(&mut g, 90, |g| g.pick(&CityStatePersonality::ALL));
            cs.resource = opt(&mut g, 30, |g| ResourceId(g.below(t.resources as u64) as u8));
            cs.unique_unit = opt(&mut g, 30, |g| BaseUnitId(g.below(t.units as u64) as u16));
            cs.influence = (0..g.below(u64::from(n) + 1)).map(|_| g.float()).collect();
            cs.protectors = players.iter().copied().filter(|_| g.chance(20)).collect::<PlayerSet>();
            cs.quests = (0..g.below(4))
                .map(|i| Quest {
                    kind: QuestKindId(g.below(t.quests as u64) as u16),
                    assignee: g.pick(&players),
                    turn: g.int(1, 300) as i32,
                    scope: if g.chance(50) { QuestScope::Individual } else { QuestScope::Global },
                    target: match i % 10 {
                        0 => QuestTarget::None,
                        1 => QuestTarget::Tile(tile(&mut g)),
                        2 => QuestTarget::Resource(ResourceId(g.below(t.resources as u64) as u8)),
                        3 => QuestTarget::Building(BuildingId(g.below(t.buildings as u64) as u16)),
                        4 => QuestTarget::UnitType(BaseUnitId(g.below(t.units as u64) as u16)),
                        5 => QuestTarget::Player(g.pick(&players)),
                        6 => {
                            QuestTarget::NaturalWonder(TerrainId(g.below(t.terrains as u64) as u8))
                        }
                        7 => QuestTarget::Religion(ReligionId(g.below(religions as u64) as u8)),
                        8 => QuestTarget::Baseline(g.int(-5, 1 << 40)),
                        _ => QuestTarget::Percent(g.below(100) as i16),
                    },
                    influence: g.below(60) as i32,
                    duration: g.below(40) as i32,
                })
                .collect();
            cs.timers.global = g.int(-1, 20) as i16;
            for _ in 0..g.below(3) {
                cs.timers.individual.insert(g.pick(&players), g.int(-1, 20) as i16);
            }
            cs.pairs = (0..g.below(u64::from(n) + 1))
                .map(|_| CsPair {
                    bullied: g.below(20) as i16,
                    pledged: g.below(20) as i16,
                    withdrew: g.below(20) as i16,
                    border_conflict: g.below(5) as i16,
                    anger_free: g.below(5) as i16,
                    recently_attacked: g.below(5) as i16,
                    marriage_cooldown: g.below(5) as i16,
                    unit_timer: opt(&mut g, 30, |g| g.below(20) as i16),
                    wary: g.chance(10),
                })
                .collect();
            for _ in 0..g.below(2) {
                let mut kills = BTreeMap::new();
                kills.insert(g.pick(&players), g.below(4) as u16);
                cs.war_quests
                    .insert(g.pick(&players), WarQuest { needed: g.below(5) as u16, kills });
            }
            cs.election_in = opt(&mut g, 30, |g| g.below(20) as i16);
            cs.barb_help_cd = g.below(10) as i16;
            cs.recently_bullied = g.below(10) as i16;
        }
        plist.push(pl);
    }
    let barbarians = PlayerSet::single(PlayerId(n - 1));

    // ---- Diplomacy
    let mut diplo = Diplomacy::new(n, barbarians);
    for a in 0..n {
        for b in (a + 1)..n {
            if !g.chance(40) {
                continue;
            }
            let (pa, pb) = (PlayerId(a), PlayerId(b));
            let _changed = diplo
                .update(pa, pb, |rel| {
                    rel.met = true;
                    rel.war = g.chance(20);
                    rel.war_declared_by = rel.war.then(|| if g.chance(50) { pa } else { pb });
                    rel.since = g.int(1, 300) as i32;
                    rel.treaty_until = g.int(0, 300) as i32;
                    rel.friendship_until = g.int(0, 300) as i32;
                    rel.pact_until = g.int(0, 300) as i32;
                    rel.ra_until = g.int(0, 300) as i32;
                    rel.ra_science = [g.below(500) as i32, g.below(500) as i32];
                    rel.embassy = [g.chance(50), g.chance(50)];
                    rel.denounced_until = [g.int(0, 99) as i32, g.int(0, 99) as i32];
                    rel.open_borders_until = [g.int(0, 99) as i32, g.int(0, 99) as i32];
                })
                .expect("a pair");
        }
    }
    for _ in 0..(n as usize * 2) {
        let (h, a) = (g.pick(&players), g.pick(&players));
        diplo.opinions.set(h, a, g.pick(&OpinionKey::ALL), g.float());
    }
    let item = |g: &mut Gen| -> DealItem {
        match g.below(13) {
            0 => DealItem::Gold { amount: g.int(0, 900) as i32 },
            1 => DealItem::GoldPerTurn { amount: g.int(0, 30) as i32, turns: 30 },
            2 => DealItem::Resource {
                resource: ResourceId(g.below(t.resources as u64) as u8),
                amount: 1,
                turns: 30,
            },
            3 => DealItem::OpenBorders { turns: 30 },
            4 => DealItem::Embassy,
            5 => DealItem::PeaceTreaty,
            6 => DealItem::DeclarationOfFriendship,
            7 => DealItem::ResearchAgreement,
            8 => DealItem::DefensivePact,
            9 => DealItem::DeclareWar { target: PlayerId(g.below(u64::from(n)) as u8) },
            10 => DealItem::City { city_id: city_ids[g.idx(city_ids.len())] },
            11 => DealItem::ShareMap,
            _ => DealItem::Tech { tech: TechId(g.below(t.techs as u64) as u16) },
        }
    };
    let terms = |g: &mut Gen, a: PlayerId, b: PlayerId| Terms {
        sides: [
            Side { giver: a, items: (0..g.below(3)).map(|_| item(g)).collect() },
            Side { giver: b, items: (0..g.below(3)).map(|_| item(g)).collect() },
        ],
    };
    let deals = g.below(4) as u32;
    for i in 1..=deals {
        let (a, b) = (PlayerId(0), PlayerId(1));
        diplo.deals.push(Deal {
            id: DealId::new(i).unwrap_or(DealId::FIRST),
            turn: g.int(1, 300) as i32,
            parties: [a, b],
            terms: terms(&mut g, a, b),
            ongoing: vec![Ongoing {
                item: DealItem::GoldPerTurn { amount: 3, turns: 30 },
                from: a,
                to: b,
                until: g.int(1, 300) as i32,
            }],
            active: g.chance(70),
            summary: g.text(),
        });
    }
    let negs = g.below(3) as u32;
    for i in 1..=negs {
        let (a, b) = (PlayerId(1), PlayerId(0));
        diplo.negotiations.push(Negotiation {
            id: NegotiationId::new(i).unwrap_or(NegotiationId::FIRST),
            initiator: a,
            responder: b,
            turn: g.int(1, 300) as i32,
            status: g.pick(&NegStatus::ALL),
            awaiting: opt(&mut g, 50, |g| g.pick(&[a, b])),
            proposal: opt(&mut g, 50, |g| terms(g, a, b)),
            proposal_by: opt(&mut g, 50, |g| g.pick(&[a, b])),
            history: (1..=g.below(4) as u16)
                .map(|seq| NegEntry {
                    seq,
                    by: opt(&mut g, 80, |g| g.pick(&[a, b])),
                    action: g.pick(&NegAction::ALL),
                    message: g.text(),
                    proposal: opt(&mut g, 30, |g| terms(g, a, b)),
                    turn: g.int(1, 300) as i32,
                    note: opt(&mut g, 20, Gen::text),
                })
                .collect(),
            deal: (deals > 0 && g.chance(30)).then(|| DealId::new(1).unwrap_or(DealId::FIRST)),
        });
    }

    // ---- World
    let mut world = World::default();
    for i in 0..religions {
        world.religions.push(Religion {
            name: if i % 2 == 0 {
                ReligionName::Pantheon(BeliefId(g.below(t.beliefs as u64) as u16))
            } else {
                ReligionName::Religion(RulesReligionId(g.below(t.religions as u64) as u8))
            },
            display: g.text(),
            founder: g.pick(&players),
            founder_beliefs: subset(&mut g, t.beliefs, 5),
            follower_beliefs: subset(&mut g, t.beliefs, 5),
            blocked_holy: g.chance(10),
        });
    }
    for _ in 0..g.below(5) {
        world.wonders_built.insert(
            BuildingId(g.below(t.buildings as u64) as u16),
            city_ids[g.idx(city_ids.len())],
        );
    }
    world.un.next_vote = opt(&mut g, 50, |g| g.int(1, 400) as i32);
    for _ in 0..g.below(4) {
        world.un.votes.insert(g.pick(&players), opt(&mut g, 80, |g| g.pick(&players)));
    }
    world.un.results = opt(&mut g, 50, |g| UnResult {
        turn: g.int(1, 400) as i32,
        tally: vec![(g.pick(&players), g.below(9) as u16), (g.pick(&players), g.below(9) as u16)],
        votes_needed: g.below(9) as u16,
        winner: opt(g, 50, |g| g.pick(&players)),
    });
    world.un.won = players.iter().copied().filter(|_| g.chance(10)).collect();
    world.un.processed_turn = opt(&mut g, 50, |g| g.int(1, 400) as i32);
    for c in 1..camp_counter {
        if g.chance(60) {
            let mut camp = Camp::new(tile(&mut g));
            camp.countdown = g.below(10) as i16;
            camp.spawned = g.int(-1, 5) as i16;
            camp.destroyed = g.chance(20);
            world.camps.insert(CampId::new(c).unwrap_or(CampId::FIRST), camp);
        }
    }

    // ---- Settings, clock, counters, heads
    let map_source = if g.chance(70) {
        MapSource::Generated {
            size: MapSizeId(g.below(t.map_sizes as u64) as u8),
            map_type: MapTypeId(g.below(t.map_types as u64) as u8),
            edges: g.pick(&MapEdges::ALL),
            dims: g.chance(30).then_some((w, h)),
        }
    } else {
        MapSource::Editor { id: g.text(), size: MapSizeId(g.below(t.map_sizes as u64) as u8) }
    };
    let mut config = GameConfig::new(
        g.word(),
        map_source,
        SpeedId(g.below(t.speeds as u64) as u8),
        DifficultyId(g.below(t.difficulties as u64) as u8),
        EraId(g.below(t.eras as u64) as u8),
        BarbarianLevelId(g.below(t.barbarian_levels as u64) as u8),
        g.int(100, 600) as i32,
    );
    config.barbarian_difficulty = DifficultyId(g.below(t.difficulties as u64) as u8);
    config.ai_base_values =
        if g.chance(50) { AiBaseValues::Unciv } else { AiBaseValues::Monotonic };
    config.barbarian_aggression = opt(&mut g, 30, |g| g.below(101) as u8);
    config.disabled_victories =
        (0..t.victories).filter(|_| g.chance(20)).map(|v| VictoryId(v as u8)).collect();
    config.city_states = shape.city_states;
    config.religion = g.chance(90);
    config.espionage = g.chance(90);
    config.nuclear_weapons = g.chance(90);
    config.tech_trading = g.chance(90);
    config.ruins = g.chance(90);
    config.river_density = g.float().abs();
    config.resources.density = 1.5;
    config.resources.luxury = ResourceKindOptions {
        density: 0.5,
        each: vec![
            (ResourceId(0), ResourceRule::Off),
            (ResourceId(1), ResourceRule::Cap(3.0)),
            (ResourceId(2), ResourceRule::Share(25.5)),
        ],
    };
    config.diplomacy.max_chat_messages = opt(&mut g, 50, |g| g.below(40) as u16);
    config.host.insert("on_disconnect".to_owned(), json!("pause"));
    config.host.insert("lobby".to_owned(), json!({"players": [1, 2.5, null, "x"], "b": true}));

    let clock = TurnClock {
        turn: g.int(1, 500) as i32,
        current: g.pick(&living),
        turn_started: g.chance(50),
        phase: if g.chance(80) { Phase::Playing } else { Phase::Over },
        winner: opt(&mut g, 20, |g| g.pick(&players)),
        victory: opt(&mut g, 20, |g| VictoryId(g.below(t.victories as u64) as u8)),
    };
    let ids = IdCounters {
        unit: unit_counter,
        city: city_counter,
        camp: camp_counter,
        deal: deals + 1,
        negotiation: negs + 1,
        combat_seq: g.word() >> 8,
    };
    let mut hash = [0u8; 32];
    for b in &mut hash {
        *b = g.word() as u8;
    }
    let last_stats = opt(&mut g, 80, |g| StatsRow {
        turn: g.int(1, 500) as i32,
        civs: (0..shape.majors)
            .map(|p| CivStats {
                player: PlayerId(p),
                alive: PlayerId(p) != dead,
                score: g.int(0, 3000) as i32,
                cities: g.below(30) as u16,
                population: g.below(300) as u32,
                land: g.below(900) as u32,
                techs: g.below(80) as u16,
                policies: g.below(60) as u16,
                military: g.int(0, 1 << 30),
                gold: g.int(-100, 1 << 30),
                gold_per_turn: g.float(),
                science: g.float(),
                culture: g.float(),
                faith: g.float(),
                production: g.float(),
                happiness: g.int(-30, 30) as i32,
                era: EraId(g.below(t.eras as u64) as u8),
                units: g.below(300) as u32,
                golden_age: g.chance(20),
            })
            .collect(),
    });
    let chronicle = ChronicleHeads {
        engine_events: g.below(10_000) as u32,
        messages: g.below(100) as u32,
        stats: g.below(500) as u32,
        hash,
        last_stats,
    };
    let host = HostHeads {
        next_event_id: 1 + g.below(10_000) as u32,
        host_events: g.below(10) as u32,
        thoughts: g.below(100) as u32,
        actions: g.below(1000) as u32,
        frames: g.below(500) as u32,
        journal_seq: g.below(500) as u32,
    };

    let parts = StateParts {
        config,
        map,
        tiles,
        players: PlayerVec::from_vec(plist),
        units,
        cities,
        diplo,
        world,
        clock,
        ids,
        chronicle,
        host: HostOnly(host),
    };
    let mut st = State::from_parts(parts).expect("the generated parts fit");
    for p in players.iter().copied().filter(|&p| kind(p) == PlayerKind::CityState) {
        if g.chance(50) {
            let ally = g.pick(&living);
            let _changed = st.set_ally(p, Some(ally)).expect("a city-state");
        }
    }
    st
}

/// A history to journal: `n` entries of every kind, in a mixed order, appended through a
/// `save::journal::Record` as the engine appends them: events' ids taken from `host`, the
/// engine's entries folded into `heads`, the host's counted in `host`.
pub fn history(
    r: &'static Ruleset,
    seed: u64,
    n: usize,
    heads: &mut ChronicleHeads,
    host: &mut HostHeads,
    chron: &mut citar_engine::state::chronicle::Chronicle,
) {
    let mut next_message = heads.messages + 1;
    let mut rec = citar_engine::save::journal::Record::new(heads, host, chron);
    let mut g = Gen::new(seed);
    let t = Tables::of(r);
    let players: Vec<PlayerId> = (0..4).map(PlayerId).collect();
    for i in 0..n {
        let turn = g.int(1, 400) as i32;
        match g.below(6) {
            0 => {
                let Some(id) = rec.take_event_id() else { return };
                let engine = g.chance(85);
                let kind = if engine {
                    EventType::Engine(g.pick(EngineEvent::ALL))
                } else {
                    EventType::Host(g.pick(&["agent_error", "game_paused"]).into())
                };
                let text = g.text();
                let len = text.len() as u32;
                let e = Event {
                    id,
                    turn,
                    kind,
                    text,
                    audience: opt(&mut g, 30, |g| {
                        players.iter().copied().filter(|_| g.chance(50)).collect()
                    }),
                    tile: opt(&mut g, 50, |g| TileIdx(g.below(64) as u32)),
                    data: opt(&mut g, 50, |g| {
                        Box::new(EventData {
                            player: Some(g.pick(&players)),
                            tech: Some(TechId(g.below(t.techs as u64) as u16)),
                            item: Some(constructible(g, &t)),
                            policy: opt(g, 50, |g| PolicyId(g.below(t.policies as u64) as u16)),
                            gold: opt(g, 50, |g| g.int(-50, 500) as i32),
                            status: opt(g, 30, |g| g.pick(&NegStatus::ALL)),
                            ..EventData::default()
                        })
                    }),
                    refs: if len > 0 && g.chance(50) {
                        std::iter::once(NameRef {
                            start: 0,
                            end: len,
                            player: g.pick(&players),
                            kind: g.pick(&[RefKind::Civ, RefKind::Leader, RefKind::City]),
                        })
                        .collect()
                    } else {
                        Default::default()
                    },
                };
                rec.event(e).expect("finite");
            }
            1 => {
                let m = Message {
                    id: MessageId::new(next_message).unwrap_or(MessageId::FIRST),
                    turn,
                    from: g.pick(&players),
                    to: players.iter().copied().filter(|_| g.chance(50)).collect(),
                    text: g.text(),
                };
                next_message += 1;
                rec.message(m).expect("finite");
            }
            2 => {
                rec.thought(Thought {
                    turn,
                    player: g.pick(&players),
                    text: g.text(),
                    kind: opt(&mut g, 50, Gen::text),
                });
            }
            3 => {
                let s = StatsRow {
                    turn,
                    civs: vec![CivStats {
                        player: PlayerId(0),
                        alive: true,
                        score: i as i32,
                        cities: 1,
                        population: 2,
                        land: 3,
                        techs: 4,
                        policies: 5,
                        military: 6,
                        gold: 7,
                        gold_per_turn: g.float(),
                        science: g.float(),
                        culture: g.float(),
                        faith: g.float(),
                        production: g.float(),
                        happiness: -1,
                        era: EraId(0),
                        units: 9,
                        golden_age: false,
                    }],
                };
                rec.stats(s).expect("finite");
            }
            4 => {
                rec.action(ActionRecord {
                    turn,
                    player: g.pick(&players),
                    tool: "move_unit".into(),
                    args: r#"{"unit":3,"x":4}"#.into(),
                });
            }
            _ => {
                let bytes: Vec<u8> = (0..g.below(30)).map(|_| g.word() as u8).collect();
                rec.frame(FrameRecord { turn, keyframe: g.chance(20), bytes: bytes.into() });
            }
        }
    }
}
