//! A city's life from turn to turn (`cities.py:2208-2377`, UnCiv's `CityTurnManager`): the start
//! of its turn (stage S5: what it finished, resistance, We Love The King Day and the luxuries it
//! demands, an idle queue) and the end (stage E4: production, border growth, growth or
//! starvation, razing, healing), and a city destroyed.
//!
//! Citizens are placed again by the settle that follows (DESIGN.md 6.7), where Python placed
//! them at once. What waits for other packages, marked where it happens: religion's end of a
//! city's turn and a population change's followers (1b-08), a spy's city gone (1c-05), and the
//! elimination a destroyed city may bring (1c-08).

use smallvec::SmallVec;

use super::super::derive::rev::{CityTouch, PlayerTouch};
use super::super::events::Mention;
use super::super::{Game, Porting, pending};
use super::borders::{culture_to_next_tile, expand_borders};
use super::construction::{construct_if_enough, end_turn_production};
use super::founding::{add_building, capital_indicator};
use super::queue::auto_pick_production;
use super::stats::{food_to_next_pop, is_capital, max_health, work_range};
use crate::base::ids::{CityId, PlayerId, ResourceId};
use crate::base::num;
use crate::base::rng::{Purpose, Rng};
use crate::base::sets::{PlayerSet, ResourceSet};
use crate::base::stats::Stat;
use crate::game::core::has_type;
use crate::game::economy;
use crate::rules::defs::ResourceType;
use crate::state::TileClaim;
use crate::state::chronicle::{EngineEvent, EventData};
use crate::unique::{Ctx, UniqueData, UniqueType, uq};

/// A city's owner, name and tile, for its announcements.
fn about(g: &Game, c: CityId) -> Option<(PlayerId, String, crate::base::ids::TileIdx)> {
    g.city(c).map(|x| (x.owner(), x.name.to_string(), x.tile()))
}

/// Tells a city's owner something about it.
fn tell(g: &mut Game, c: CityId, kind: EngineEvent, text: &str) {
    let Some((owner, _, at)) = about(g, c) else { return };
    g.emit(kind, text, Some(PlayerSet::single(owner)), Some(at), EventData::default(), &[]);
}

/// The stream a city's draws of a purpose come from this turn (`g.state_rng(key, city, turn)`).
fn stream(g: &Game, purpose: Purpose, c: CityId) -> Rng {
    let turn = u64::try_from(g.turn()).unwrap_or(0);
    Rng::keyed(g.state().seed(), purpose, &[u64::from(c.get()), turn])
}

/// Changes a city's population by `n`, never below one (`cities.add_population`,
/// `cities.py:906-921`); its citizens are placed again when the call settles.
pub fn add_population(g: &mut Game, c: CityId, n: i32) {
    let Some(pop) = g.city(c).map(|x| i32::from(x.pop)) else { return };
    let n = n.max(1 - pop);
    if let Some(x) = g.city_mut(c, CityTouch::CORE) {
        x.pop = u16::try_from(pop + n).unwrap_or(u16::MAX);
    }
    if g.religion_enabled() {
        // religion.on_population_change (cities.py:914-916).
        pending(Porting::Pending("1b-08"));
    }
}

/// Stage S5, a city starts its turn (`cities.start_turn`, `cities.py:2211-2236`): it finishes
/// what the production stored pays for, is no longer under attack, and has bought nothing yet;
/// a luxury it demanded and now has starts We Love The King Day; its countdowns move; a puppet
/// works for gold; an empty queue is filled by the advisor where the city lets it, or reported.
pub fn start_turn(g: &mut Game, c: CityId) {
    construct_if_enough(g, c);
    let Some(city) = g.city(c) else { return };
    let owner = city.owner();
    if (city.attacked || !city.bought_this_turn.is_empty())
        && let Some(x) = g.city_mut(c, CityTouch::CORE)
    {
        x.attacked = false;
        x.bought_this_turn.clear();
    }
    if let Some(res) = g.city(c).filter(|x| x.wltkd <= 0).and_then(|x| x.demanded_resource)
        && economy::resource_amount(g, owner, res) > 0
    {
        let turns = num::round_half_even_i32(20.0 * g.speed().modifier) + 1;
        if let Some(x) = g.city_mut(c, CityTouch::CORE) {
            x.wltkd = i16::try_from(turns).unwrap_or(i16::MAX);
            x.demand_countdown = 0;
        }
        let name = g.city(c).map(|x| x.name.to_string()).unwrap_or_default();
        let text = format!(
            "Because they have {}, the citizens of {name} are celebrating We Love The King Day!",
            g.rules().resources()[res].name
        );
        tell(g, c, EngineEvent::Wltkd, &text);
    }
    next_turn_flags(g, c);
    let Some(city) = g.city(c) else { return };
    if city.puppet
        && (city.focus != crate::state::cities::CityFocus::Gold || !city.locked.is_empty())
        && let Some(x) = g.city_mut(c, CityTouch::WORK)
    {
        // assign_citizens(reset=True): its locks go.
        x.focus = crate::state::cities::CityFocus::Gold;
        x.locked.clear();
    }
    let Some(city) = g.city(c) else { return };
    let (puppet, auto, empty) = (city.puppet, city.auto_production, city.queue.is_empty());
    if empty && g.player(owner).is_some_and(crate::state::players::Player::is_major) {
        let picked = if puppet || auto { auto_pick_production(g, c) } else { None };
        let name = g.city(c).map(|x| x.name.to_string()).unwrap_or_default();
        match picked {
            Some(item) if !puppet => {
                let text = format!(
                    "{name} started {} (automatic production).",
                    super::construction::item_name(g.rules(), item)
                );
                tell(g, c, EngineEvent::CityAutoProduction, &text);
            }
            Some(_) => {}
            None => {
                let bots = g.player(owner).is_some_and(|x| x.seat().controller().is_bot_managed());
                if !bots {
                    tell(g, c, EngineEvent::CityIdle, &format!("{name} has nothing to produce."));
                }
            }
        }
    }
    let Some(city) = g.city(c) else { return };
    if city.demanded_resource.is_none() && city.demand_countdown <= 0 && city.wltkd <= 0 {
        set_demand_cooldown(g, c, true);
    }
}

/// How long before a city demands a luxury again (`cities._set_demand_cooldown`,
/// `cities.py:2239-2245`): 15 to 24 turns, ten more for a new capital.
fn set_demand_cooldown(g: &mut Game, c: CityId, new_city: bool) {
    let mut d = 15 + i16::try_from(stream(g, Purpose::Demand, c).below(10)).unwrap_or(0);
    if new_city && is_capital(g, c) {
        d += 10;
    }
    if let Some(x) = g.city_mut(c, CityTouch::CORE) {
        x.demand_countdown = d;
    }
}

/// A city's countdowns (`cities._next_turn_flags`, `cities.py:2248-2262`): its next demand, We
/// Love The King Day and resistance.
fn next_turn_flags(g: &mut Game, c: CityId) {
    let Some(city) = g.city(c) else { return };
    let name = city.name.to_string();
    let (countdown, wltkd, resistance) = (city.demand_countdown, city.wltkd, city.resistance);
    if countdown > 0 {
        if let Some(x) = g.city_mut(c, CityTouch::CORE) {
            x.demand_countdown -= 1;
        }
        if countdown == 1 && wltkd <= 0 {
            demand_new_resource(g, c);
        }
    }
    if wltkd > 0 {
        if let Some(x) = g.city_mut(c, CityTouch::CORE) {
            x.wltkd -= 1;
        }
        if wltkd == 1 {
            tell(
                g,
                c,
                EngineEvent::WltkdEnd,
                &format!("We Love The King Day in {name} has ended."),
            );
            demand_new_resource(g, c);
        }
    }
    if resistance > 0 {
        if let Some(x) = g.city_mut(c, CityTouch::CORE) {
            x.resistance -= 1;
        }
        if resistance == 1 {
            tell(g, c, EngineEvent::ResistanceEnd, &format!("The resistance in {name} has ended!"));
        }
    }
}

/// The luxuries somewhere on the map (`Game.resources_on_map`, `game.py:599-605`).
fn resources_on_map(g: &Game) -> ResourceSet {
    let mut out = ResourceSet::new();
    for (_, t) in g.state().tiles().iter() {
        if let Some(r) = t.resource() {
            out.insert(r);
        }
    }
    out
}

/// A city picks a luxury to demand (`cities._demand_new_resource`, `cities.py:2265-2280`): one on
/// the map, not near it, not a city-state's own, and not its last; one its owner lacks, by name,
/// if there is any, with a new countdown and a word to its owner; else any of them, quietly.
fn demand_new_resource(g: &mut Game, c: CityId) {
    let r = g.rules();
    let Some(city) = g.city(c) else { return };
    let owner = city.owner();
    let last = city.demanded_resource;
    let on_map = resources_on_map(g);
    let near: SmallVec<[ResourceId; 8]> = g
        .grid()
        .within(city.tile(), work_range(g))
        .into_iter()
        .filter_map(|t| g.tile(t).and_then(crate::state::map::Tile::resource))
        .collect();
    let cands: Vec<ResourceId> = r
        .resources()
        .iter()
        .filter(|&(id, d)| {
            d.kind == ResourceType::Luxury
                && !has_type(r, &d.uniques, UniqueType::CityStateOnlyResource)
                && Some(id) != last
                && on_map.contains(id)
                && !near.contains(&id)
        })
        .map(|(id, _)| id)
        .collect();
    let mut missing: Vec<ResourceId> =
        cands.iter().copied().filter(|&x| economy::resource_amount(g, owner, x) <= 0).collect();
    let mut rng = stream(g, Purpose::DemandNew, c);
    if missing.is_empty() {
        let pick = rng.pick(&cands).copied();
        if let Some(x) = g.city_mut(c, CityTouch::CORE) {
            x.demanded_resource = pick;
        }
        return;
    }
    missing.sort_by(|a, b| r.resources()[*a].name.cmp(&r.resources()[*b].name));
    let pick = rng.pick(&missing).copied();
    if let Some(x) = g.city_mut(c, CityTouch::CORE) {
        x.demanded_resource = pick;
    }
    set_demand_cooldown(g, c, false);
    if let Some(res) = pick {
        let name = g.city(c).map(|x| x.name.to_string()).unwrap_or_default();
        let text = format!("{name} demands {}!", r.resources()[res].name);
        tell(g, c, EngineEvent::CityDemand, &text);
    }
}

/// Stage E4, a city ends its turn (`cities.end_turn`, `cities.py:2283-2315`): its production goes
/// into what it builds, its culture into its borders, then it grows (or starves), or is razed a
/// little more, and heals.
pub fn end_turn(g: &mut Game, c: CityId) {
    let total = super::super::derive::stats::city_stats(g, c).total;
    end_turn_production(g, c, total[Stat::Production]);
    if let Some(x) = g.city_mut(c, CityTouch::STOCKS) {
        x.culture += total[Stat::Culture].trunc();
    }
    let cost = f64::from(culture_to_next_tile(g, c));
    if g.city(c).is_some_and(|x| x.culture >= cost) && expand_borders(g, c).is_some() {
        if let Some(x) = g.city_mut(c, CityTouch::STOCKS) {
            x.culture -= cost;
        }
        let name = g.city(c).map(|x| x.name.to_string()).unwrap_or_default();
        tell(g, c, EngineEvent::Borders, &format!("{name} has expanded its borders."));
    }
    let Some(city) = g.city(c) else { return };
    let owner = city.owner();
    if city.razing {
        let removed = {
            let v = g.view();
            let mut n = 1;
            for h in uq::civ(&v, owner, UniqueType::CitiesAreRazedXTimesFaster, &Ctx::civ(owner)) {
                if let UniqueData::CitiesAreRazedXTimesFaster(x) = *h.data() {
                    n += (x.times - 1) * i32::from(h.n);
                }
            }
            n
        };
        if i32::from(city.pop) <= removed {
            let (name, at) = (city.name.to_string(), city.tile());
            g.emit(
                EngineEvent::CityRazed,
                &format!("{name} has been razed to the ground!"),
                None,
                Some(at),
                EventData::default(),
                &[],
            );
            destroy_city(g, c);
            return;
        }
        add_population(g, c, -removed);
        let need = f64::from(food_to_next_pop(g, c));
        if g.city(c).is_some_and(|x| x.food >= need)
            && let Some(x) = g.city_mut(c, CityTouch::STOCKS)
        {
            x.food = need - 1.0;
        }
    } else {
        grow(g, c, num::round_half_even_i32(total[Stat::Food]));
    }
    if g.religion_enabled() {
        // religion.city_end_turn (cities.py:2309-2311).
        pending(Porting::Pending("1b-08"));
    }
    if g.city(c).is_some() {
        let most = max_health(g, c);
        if let Some(x) = g.city_mut(c, CityTouch::CORE) {
            x.health = most.min(x.health + 20);
        }
    }
}

/// A city's food store takes the turn's surplus (`cities._grow`, `cities.py:2318-2343`): a
/// deficit that empties it costs a citizen; a full store brings one, keeping what
/// `[n]% of food is carried over after population increases [cities]` keeps, unless growth is
/// nullified or the city avoids it.
fn grow(g: &mut Game, c: CityId, food: i32) {
    if let Some(x) = g.city_mut(c, CityTouch::STOCKS) {
        x.food += f64::from(food);
    }
    let Some(name) = g.city(c).map(|x| x.name.to_string()) else { return };
    if food < 0 {
        tell(g, c, EngineEvent::CityStarving, &format!("{name} is starving!"));
    }
    if g.city(c).is_some_and(|x| x.food < 0.0) {
        if g.city(c).is_some_and(|x| x.pop > 1) {
            add_population(g, c, -1);
        }
        if let Some(x) = g.city_mut(c, CityTouch::STOCKS) {
            x.food = 0.0;
        }
    }
    let need = f64::from(food_to_next_pop(g, c));
    let Some(city) = g.city(c) else { return };
    if city.food < need {
        return;
    }
    let (nullified, carry) = {
        let v = g.view();
        let ctx = Ctx::city(&v, c);
        let nullified = uq::any(uq::city(&v, c, UniqueType::NullifiesGrowth, &ctx));
        let filters = g.rules().uniques().filters();
        let mut carry = 0;
        for h in uq::city(&v, c, UniqueType::CarryOverFood, &ctx) {
            if let UniqueData::CarryOverFood(x) = *h.data()
                && filters.city_matches(x.cities, &v, c, None)
            {
                carry += x.percent * i32::from(h.n);
            }
        }
        (nullified, carry.min(95))
    };
    if nullified {
        return;
    }
    if city.avoid_growth {
        if let Some(x) = g.city_mut(c, CityTouch::STOCKS) {
            x.food = need;
        }
        return;
    }
    if let Some(x) = g.city_mut(c, CityTouch::STOCKS) {
        x.food -= need;
        x.food += (need * f64::from(carry) / 100.0).trunc();
    }
    add_population(g, c, 1);
    let pop = g.city(c).map_or(0, |x| x.pop);
    tell(g, c, EngineEvent::CityGrowth, &format!("{name} grew to size {pop}."));
}

/// A city is destroyed (`cities.destroy_city`, `cities.py:2349-2377`): its tiles are released, its
/// centre left as ruins, and a capital's palace moves to its owner's largest city. A world wonder
/// it held stays built.
pub fn destroy_city(g: &mut Game, c: CityId) {
    let Some(city) = g.city(c) else { return };
    let (owner, centre, name) = (city.owner(), city.tile(), city.name.to_string());
    let range = u32::try_from(g.rules().constants().formulas.city_expand_range).unwrap_or(0);
    let released: Vec<_> = g
        .grid()
        .within(centre, range)
        .into_iter()
        .filter(|&t| g.tile(t).and_then(crate::state::map::Tile::city) == Some(c))
        .collect();
    for t in released {
        if let Err(e) = g.set_tile_owner(t, TileClaim::NONE) {
            debug_assert!(false, "a tile of the map could not be released: {e}");
        }
    }
    let ruins = g.rules().derived().known.city_ruins;
    if let Err(e) = g.set_improvement(centre, ruins) {
        debug_assert!(false, "a tile of the map could not be ruined: {e}");
    }
    if let Err(e) = g.remove_city(c) {
        debug_assert!(false, "a city of the game could not be removed: {e}");
        return;
    }
    // espionage.city_removed: the spies in it go home (cities.py:2364-2365).
    pending(Porting::Pending("1c-05"));
    if g.player(owner).is_some_and(|x| x.capital == Some(c)) {
        if let Some(x) = g.player_mut(owner, PlayerTouch::CAPITAL) {
            x.capital = None;
        }
        // The first of the largest, as Python's max over its cities kept.
        let mut newcap: Option<(CityId, u16)> = None;
        for x in g.player_cities(owner) {
            if newcap.is_none_or(|(_, pop)| x.pop > pop) {
                newcap = Some((x.id(), x.pop));
            }
        }
        if let Some((nc, _)) = newcap {
            if let Some(ind) = capital_indicator(g, owner) {
                add_building(g, nc, ind, false);
            }
            if let Some(x) = g.player_mut(owner, PlayerTouch::CAPITAL) {
                x.capital = Some(nc);
            }
        }
    }
    let data = EventData { owner: Some(owner), ..EventData::default() };
    g.emit(
        EngineEvent::CityDestroyed,
        &format!("{name} has been destroyed."),
        None,
        Some(centre),
        data,
        &[Mention::city(&name, owner)],
    );
    // victory.check_elimination (cities.py:2376-2377).
    pending(Porting::Pending("1c-08"));
}

/// Stage S5: every city of the civilization starts its turn, in id order (`turns.py:52-54`).
pub(crate) fn start_turn_stage(g: &mut Game, p: PlayerId) {
    let cities: Vec<CityId> = g.state().cities().of(p).to_vec();
    for c in cities {
        if g.city(c).is_some() {
            start_turn(g, c);
        }
    }
}

/// Stage E4: every city of the civilization ends its turn, razing ones first, then in id order
/// (`turns.py:107-109`, Python's stable sort on `not c.razing`).
pub(crate) fn end_turn_stage(g: &mut Game, p: PlayerId) {
    let mut cities: Vec<(bool, CityId)> = g.player_cities(p).map(|x| (!x.razing, x.id())).collect();
    cities.sort();
    for (_, c) in cities {
        if g.city(c).is_some() {
            end_turn(g, c);
        }
    }
}
