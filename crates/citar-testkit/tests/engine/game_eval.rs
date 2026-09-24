//! The evaluator's view of a real game (`game::EvalView`, package 1b-01) on the kitchen-sink
//! ruleset, which carries every conditional the engine supports: every unique's conditionals
//! evaluate in a game without panicking, in the contexts of a civilization, a city, a unit, a
//! tile and a fight; the civilization-level and local halves agree with the whole; the revisions
//! each unique's conditionals read exist; and none of it changes the game.

use citar_engine::base::ids::{
    BarbarianLevelId, BaseUnitId, BuildingId, CityId, DifficultyId, EraId, MapSizeId, MapTypeId,
    NationId, PlayerId, SpeedId, TerrainId, TileIdx, UnitId,
};
use citar_engine::base::sets::PlayerVec;
use citar_engine::game::Game;
use citar_engine::rules::Ruleset;
use citar_engine::state::chronicle::Chronicle;
use citar_engine::state::cities::{Cities, City};
use citar_engine::state::config::{GameConfig, MapEdges, MapSource};
use citar_engine::state::map::{MapInfo, Tile, Tiles};
use citar_engine::state::players::{Controller, Player, PlayerKind, Rgb, Seat, SeatOverrides};
use citar_engine::state::units::{Unit, Units};
use citar_engine::state::{IdCounters, State, TileClaim};
use citar_engine::unique::filter::Combatant;
use citar_engine::unique::{CombatCtx, CondDeps, Ctx, applies, applies_scoped};
use citar_testkit::rulesets::kitchen_sink;

const W: u16 = 12;
const H: u16 = 10;
const ROME: PlayerId = PlayerId(0);
const GREECE: PlayerId = PlayerId(1);

fn id<I: citar_engine::rules::Named>(r: &Ruleset, name: &str) -> I {
    r.lookup::<I>(name).unwrap_or_else(|| panic!("the kitchen sink has {name}"))
}

fn cid(n: u32) -> CityId {
    CityId::new(n).unwrap_or(CityId::FIRST)
}

fn uid(n: u32) -> UnitId {
    UnitId::new(n).unwrap_or(UnitId::FIRST)
}

/// Two majors, a city-state and the barbarians; a city each for the majors, Rome's with a
/// Temple and a worked tile, and a warrior each beside them, at war.
fn game(r: &'static Ruleset) -> Game {
    let size = u32::from(W) * u32::from(H);
    let player = |n: u8, kind: PlayerKind, name: &str| {
        let controller = match kind {
            PlayerKind::Major => Controller::Bot,
            PlayerKind::CityState => Controller::Minor,
            PlayerKind::Barbarian => Controller::Barbarian,
        };
        let seat = Seat::new(controller, SeatOverrides::default(), None);
        Player::new(
            PlayerId(n),
            kind,
            name.into(),
            id::<NationId>(r, name),
            Rgb::default(),
            seat,
            size,
        )
    };
    let players: PlayerVec<Player> = [
        player(0, PlayerKind::Major, "Rome"),
        player(1, PlayerKind::Major, "Greece"),
        player(2, PlayerKind::CityState, "Geneva"),
        player(3, PlayerKind::Barbarian, "Barbarians"),
    ]
    .into_iter()
    .collect();
    let map = MapInfo { width: W, height: H, wrap_x: false, wrap_y: false, continents: Vec::new() };
    let grass = id::<TerrainId>(r, "Grassland");
    let mut tiles = vec![Tile::new(grass); size as usize];
    tiles[23] = tiles[23].with_river(1);
    let src = MapSource::Generated {
        size: MapSizeId(0),
        map_type: MapTypeId(0),
        edges: MapEdges::IceCaps,
        dims: None,
    };
    let cfg = GameConfig::new(
        11,
        src,
        id::<SpeedId>(r, "Standard"),
        id::<DifficultyId>(r, "Prince"),
        EraId(0),
        BarbarianLevelId(1),
        500,
    );
    let st = State::new(cfg, map, Tiles::new(tiles), players).expect("a new state");
    let mut parts = st.into_parts();
    let mut tiles: Vec<Tile> = parts.tiles.as_slice().to_vec();
    for (t, owner, c) in [(22, ROME, 1), (23, ROME, 1), (40, GREECE, 2), (41, GREECE, 2)] {
        tiles[t] = tiles[t].with_claim(TileClaim::city(owner, cid(c)));
    }
    parts.tiles = Tiles::new(tiles);
    let mut roma = City::new(cid(1), "Roma".into(), ROME, TileIdx(22), 1);
    roma.pop = 2;
    roma.worked = vec![TileIdx(23)];
    roma.buildings.insert(id::<BuildingId>(r, "Temple"));
    let athens = City::new(cid(2), "Athens".into(), GREECE, TileIdx(40), 1);
    parts.cities = Cities::from_cities([roma, athens]).expect("two cities");
    let warrior = id::<BaseUnitId>(r, "Warrior");
    let mut wounded = Unit::new(uid(3), warrior, ROME, TileIdx(23), 1);
    wounded.hp = 60;
    let enemy = Unit::new(uid(4), warrior, GREECE, TileIdx(41), 1);
    parts.units = Units::from_units([wounded, enemy], size).expect("two units");
    parts.ids = IdCounters::starting_at(5);
    for (p, c) in [(ROME, 1), (GREECE, 2)] {
        if let Some(pl) = parts.players.get_mut(p) {
            pl.capital = Some(cid(c));
            pl.original_capital = Some(cid(c));
        }
    }
    // A state still in parts has no caches to tell of the change.
    let changed = parts
        .diplo
        .update(ROME, GREECE, |x| {
            x.met = true;
            x.war = true;
        })
        .expect("a pair");
    assert_eq!(changed.len(), 2, "contact, then war");
    let st = State::from_parts(parts).expect("the parts fit");
    Game::from_state(r, st, Chronicle::new()).expect("a sound state")
}

#[test]
fn every_kitchen_sink_conditional_evaluates_in_a_game() {
    let r = kitchen_sink();
    let g = game(r);
    assert!(g.check_invariants().is_empty(), "{:?}", g.check_invariants());
    let before = (g.digest().ok(), g.rev());
    let v = g.view();
    let fight = CombatCtx {
        our: Combatant::Unit(uid(3)),
        their: Some(Combatant::Unit(uid(4))),
        attacked_tile: Some(TileIdx(41)),
        action: Some(citar_engine::unique::world::CombatAction::Attack),
    };
    let ctxs = [
        Ctx::civ(ROME),
        Ctx::city(&v, cid(1)),
        Ctx::unit(&v, uid(3)),
        Ctx::tile(Some(ROME), TileIdx(23)),
        Ctx::fight(&v, fight),
        Ctx::default(),
    ];
    let t = r.uniques();
    let (mut held, mut evaluated) = (0usize, 0usize);
    for (u, x) in t.iter() {
        if x.conds.is_empty() {
            continue;
        }
        for ctx in &ctxs {
            let whole = applies(u, ctx, &v);
            let halves = applies_scoped(u, ctx, &v, CondDeps::CIV_LEVEL)
                && applies_scoped(u, ctx, &v, CondDeps::LOCAL);
            assert_eq!(whole, halves, "{} in {ctx:?}", t.text_of(u));
            // What the conditionals read has revisions a memo can validate against.
            let rev = g.derived().revs().cond(g.state(), x.deps(), ctx);
            assert!(rev.get() >= 1);
            held += usize::from(whole);
            evaluated += 1;
        }
    }
    assert!(evaluated > 1000 && held > 0 && held < evaluated, "{held} of {evaluated}");
    assert_eq!((g.digest().ok(), g.rev()), before, "evaluating changed nothing");
}
