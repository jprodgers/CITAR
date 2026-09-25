//! Espionage: spies, their work in foreign and own cities, and stealing technology
//! (`espionage.py:1-257, 331-390, 454-488`; UnCiv's `EspionageManager` and `Spy`).
//!
//! Spies are not map units: each major keeps a list ([`Spy`]), each spy in its hideout or in a
//! city, doing one [`SpyAction`] with a countdown. A spy moves to a city in a turn, establishes a
//! network in three, then steals technology in a major's city (keyed RNG, `Purpose::Spy`, by the
//! spy's index, its owner, the city's tile and the turn, where Python keyed by the spy's name),
//! rigs elections in a city-state's, or guards its own city. A set-up spy sees its city and the
//! ring around it (`game::vis`), which the `SPIES` touch marks dirty.
//!
//! The city-state side is package 1c-06's (`espionage.py:265-328, 393-441, 491-498`): each
//! city-state holds an election every so many turns ([`city_state_election_tick`]), won by one of
//! the spies rigging it in its capital (weighted by their civilizations' influence and the spies'
//! skill) or by nobody; a spy there may stage a coup ([`StageCoup`]) against the city-state's
//! ally, which makes its civilization the ally or gets the spy killed. The first election's delay
//! is drawn from `Purpose::ElectionDelay` keyed by the city-state, the winner from
//! `Purpose::Election` keyed by the city-state and the turn, and a coup's roll from the spy's own
//! stream.

use serde_json::{Value, json};

use crate::base::ids::{CityId, PlayerId, TechId, TileIdx};
use crate::base::rng::{KeyPart, Purpose, Rng};
use crate::base::sets::PlayerSet;
use crate::base::stats::Stat;
use crate::base::{num, py};
use crate::game::Game;
use crate::game::action::{OutcomeSpec, Rule};
use crate::game::city_states::influence::{self as cs_influence, data as cs_data};
use crate::game::derive::rev::PlayerTouch;
use crate::game::diplomacy::relations::{add_opinion, name};
use crate::game::error::ActionError;
use crate::game::research::{self, TechSource};
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::diplo::OpinionKey;
use crate::state::players::{Player, Spy, SpyAction};
use crate::unique::{Ctx, UniqueData, UniqueType, uq};

/// A civilization's spies.
#[must_use]
pub fn spies(g: &Game, p: PlayerId) -> &[Spy] {
    g.player(p).and_then(|x| x.major.as_deref()).map_or(&[], |m| m.spies.as_slice())
}

/// A civilization's spy, by its place in the list.
fn spy(g: &Game, p: PlayerId, i: usize) -> Option<&Spy> {
    spies(g, p).get(i)
}

/// Edits a spy, after marking its owner's spies' sight dirty.
fn spy_mut(g: &mut Game, p: PlayerId, i: usize) -> Option<&mut Spy> {
    g.player_mut(p, PlayerTouch::SPIES)?.major.as_deref_mut()?.spies.get_mut(i)
}

/// Tells a civilization about its spies, and only it.
fn tell(g: &mut Game, p: PlayerId, text: &str, tile: Option<TileIdx>) {
    g.emit(EngineEvent::Spy, text, Some(PlayerSet::single(p)), tile, EventData::default(), &[]);
}

/// Whether a civilization takes part in espionage: a major, in a game with espionage
/// (`espionage.py:61-63`).
#[must_use]
pub fn spies_play(g: &Game, p: PlayerId) -> bool {
    g.espionage_enabled() && g.player(p).is_some_and(Player::is_major)
}

/// The city a spy is in, or `None` at the hideout (`spy_city`).
fn spy_city(g: &Game, s: &Spy) -> Option<CityId> {
    s.city.filter(|&c| g.city(c).is_some())
}

/// The rank new spies start at (`starting_rank`, `espionage.py:34-36`): 1, and what the
/// civilization's `New spies start with [n] level(s)` add.
#[must_use]
pub fn starting_rank(g: &Game, p: PlayerId) -> u8 {
    let v = g.view();
    let bonus =
        uq::sum_i32(uq::civ(&v, p, UniqueType::SpyStartingLevel, &Ctx::civ(p)), |d| match d {
            UniqueData::SpyStartingLevel(x) => Some(x.levels),
            _ => None,
        });
    u8::try_from(1i32.saturating_add(bonus).clamp(0, 255)).unwrap_or(1)
}

/// The first `Agent n` none of a civilization's spies is called (`_spy_name`,
/// `espionage.py:39-45`).
fn spy_name(g: &Game, p: PlayerId) -> String {
    let taken = |name: &str| spies(g, p).iter().any(|s| &*s.name == name);
    let mut n = 1u32;
    while taken(&format!("Agent {n}")) {
        n += 1;
    }
    format!("Agent {n}")
}

/// Gives a civilization a new spy, at its starting rank, in its hideout (`add_spy`,
/// `espionage.py:48-54`). Returns its place in the list.
pub fn add_spy(g: &mut Game, p: PlayerId) -> Option<usize> {
    let rank = starting_rank(g, p);
    let name = spy_name(g, p);
    let m = g.player_mut(p, PlayerTouch::SPIES)?.major.as_deref_mut()?;
    m.spies.push(Spy {
        name: name.clone().into(),
        rank,
        city: None,
        action: SpyAction::None,
        turns: 0,
        progress: 0,
    });
    let i = m.spies.len() - 1;
    tell(g, p, &format!("We have recruited {name} as a spy!"), None);
    Some(i)
}

/// Promotes a spy by `amount`, to the ruleset's highest rank at most (`level_up`,
/// `espionage.py:81-89`).
pub fn level_up(g: &mut Game, p: PlayerId, i: usize, amount: i32) {
    let most = g.rules().constants().formulas.max_spy_rank;
    let Some(rank) = spy(g, p, i).map(|s| i32::from(s.rank)) else { return };
    if rank >= most {
        return;
    }
    let n = amount.min(most - rank);
    let Some(s) = spy_mut(g, p, i) else { return };
    s.rank = u8::try_from(rank + n).unwrap_or(s.rank);
    let text = if n == 1 {
        format!("Your spy {} has leveled up!", s.name)
    } else {
        format!("Your spy {} has leveled up {n} times!", s.name)
    };
    tell(g, p, &text, None);
}

/// A spy's rank with what the city it is in adds for what it does there (`effective_rank`,
/// `espionage.py:92-103`): `Spies in [cities] cities act as though they have [n] levels for
/// [action]`, between 1 and the highest rank.
#[must_use]
pub fn effective_rank(g: &Game, p: PlayerId, s: &Spy) -> i32 {
    let most = g.rules().constants().formulas.max_spy_rank;
    let mut r = i32::from(s.rank);
    if let Some(c) = spy_city(g, s) {
        let t = g.rules().uniques();
        let v = g.view();
        for h in uq::city(&v, c, UniqueType::CounterIntelligenceSpyRankBonus, &Ctx::city(&v, c)) {
            if let UniqueData::CounterIntelligenceSpyRankBonus(x) = *h.data()
                && x.action == s.action
                && t.filters().city_matches(x.cities, &v, c, Some(p))
            {
                r = r.saturating_add(x.levels.saturating_mul(i32::from(h.n)));
            }
        }
    }
    r.clamp(1, most.max(1))
}

/// A spy's skill as a percentage, from its effective rank (`skill_percent`).
#[must_use]
pub fn skill_percent(g: &Game, p: PlayerId, s: &Spy) -> i32 {
    effective_rank(g, p, s)
        .saturating_mul(g.rules().constants().formulas.spy_rank_skill_percent_bonus)
}

/// How fast a spy works where it is (`efficiency`, `espionage.py:111-126`): its civilization's
/// `[n]% spy effectiveness [cities]` (the city's own, with its owner's, in its owner's city),
/// and in a foreign city what the city's `[n]% enemy spy effectiveness [cities]` take away; never
/// below 0.
#[must_use]
pub fn efficiency(g: &Game, p: PlayerId, s: &Spy) -> f64 {
    let t = g.rules().uniques();
    let v = g.view();
    let c = spy_city(g, s);
    let own_city = c.filter(|&c| g.city(c).is_some_and(|x| x.owner() == p));
    let mut friendly = 0i64;
    let mut enemy = 0i64;
    let add = |sum: &mut i64, percent: i32, n: u16| {
        *sum = sum.saturating_add(i64::from(percent) * i64::from(n));
    };
    match (c, own_city) {
        (None, _) => {
            for h in uq::civ(&v, p, UniqueType::SpyEffectiveness, &Ctx::civ(p)) {
                if let UniqueData::SpyEffectiveness(x) = *h.data() {
                    add(&mut friendly, x.percent, h.n);
                }
            }
        }
        (Some(c), Some(_)) => {
            for h in uq::city(&v, c, UniqueType::SpyEffectiveness, &Ctx::city(&v, c)) {
                if let UniqueData::SpyEffectiveness(x) = *h.data()
                    && t.filters().city_matches(x.cities, &v, c, Some(p))
                {
                    add(&mut friendly, x.percent, h.n);
                }
            }
        }
        (Some(c), None) => {
            for h in uq::civ(&v, p, UniqueType::SpyEffectiveness, &Ctx::civ(p)) {
                if let UniqueData::SpyEffectiveness(x) = *h.data()
                    && t.filters().city_matches(x.cities, &v, c, Some(p))
                {
                    add(&mut friendly, x.percent, h.n);
                }
            }
            for h in uq::city(&v, c, UniqueType::EnemySpyEffectiveness, &Ctx::city(&v, c)) {
                if let UniqueData::EnemySpyEffectiveness(x) = *h.data()
                    && t.filters().city_matches(x.cities, &v, c, None)
                {
                    add(&mut enemy, x.percent, h.n);
                }
            }
        }
    }
    #[allow(clippy::cast_precision_loss, reason = "sums of percentages, far below 2^53")]
    let (f, e) = (friendly as f64, enemy as f64);
    ((100.0 + f) / 100.0 * ((100.0 + e) / 100.0)).max(0.0)
}

/// A civilization's spy in a city, by its place in the list (`spy_in_city`).
#[must_use]
pub fn spy_in_city(g: &Game, p: PlayerId, c: CityId) -> Option<usize> {
    spies(g, p).iter().position(|s| s.city == Some(c))
}

/// Every spy in a city, with whose it is, in player order (`spies_in_city`).
#[must_use]
pub fn spies_in_city(g: &Game, c: CityId) -> Vec<(PlayerId, usize)> {
    let mut out = Vec::new();
    for (p, pl) in g.state().players().iter() {
        let Some(m) = pl.major.as_deref() else { continue };
        for (i, s) in m.spies.iter().enumerate() {
            if s.city == Some(c) {
                out.push((p, i));
            }
        }
    }
    out
}

/// The techs `p` could steal from `other`: known to `other`, and ones `p` could research now
/// (`techs_to_steal`, `espionage.py:139-142`), in id order.
#[must_use]
pub fn techs_to_steal(g: &Game, p: PlayerId, other: PlayerId) -> Vec<TechId> {
    let Some(known) = g.player(other).map(|x| x.tech.known) else { return Vec::new() };
    known.iter().filter(|&t| !g.has_tech(p, Some(t)) && research::can_research(g, p, t)).collect()
}

/// Sets a spy's action and countdown (`_set`).
fn set(g: &mut Game, p: PlayerId, i: usize, action: SpyAction, turns: i16) {
    if let Some(s) = spy_mut(g, p, i) {
        s.action = action;
        s.turns = turns;
    }
}

/// Moves a spy to a city, where it arrives next turn, or back to its hideout (`move_to`,
/// `espionage.py:156-163`).
pub fn move_to(g: &mut Game, p: PlayerId, i: usize, city: Option<CityId>) {
    if let Some(s) = spy_mut(g, p, i) {
        s.city = city;
        (s.action, s.turns) =
            if city.is_some() { (SpyAction::Moving, 1) } else { (SpyAction::None, 0) };
    }
}

/// Why a spy cannot go to a city, or `None` (`can_move_to`, `espionage.py:166-178`): not while
/// dead, only to a city its civilization has explored and not the barbarians', and only one spy
/// of a civilization to a city.
#[must_use]
pub fn can_move_to(g: &Game, p: PlayerId, s: &Spy, c: CityId) -> Option<String> {
    if s.action == SpyAction::Dead {
        return Some(format!("{} is dead; a replacement is being recruited.", s.name));
    }
    if s.city == Some(c) {
        return None;
    }
    let city = g.city(c)?;
    if !g.player(p).is_some_and(|x| x.explored.contains(city.tile().0)) {
        return Some("You have not explored that city.".to_owned());
    }
    if g.is_barbarian(city.owner()) {
        return Some("Spies cannot be sent to barbarian cities.".to_owned());
    }
    if spy_in_city(g, p, c).is_some() {
        return Some("You already have a spy in that city.".to_owned());
    }
    None
}

/// The spies in a city just captured or destroyed flee to their hideouts (`city_removed`,
/// `espionage.py:181-186`). `city_name` is the city's, which may be gone.
pub fn city_removed(g: &mut Game, c: CityId, city_name: &str) {
    for (p, i) in spies_in_city(g, c) {
        let Some(s) = spy(g, p, i).map(|s| s.name.clone()) else { continue };
        let text = format!(
            "After the city of {city_name} was captured, your spy {s} has fled back to our \
             hideout."
        );
        tell(g, p, &text, None);
        move_to(g, p, i, None);
    }
}

/// Every spy of a civilization goes back to its hideout, as when it is eliminated
/// (`remove_all_spies`, `espionage.py:189-192`).
pub fn remove_all_spies(g: &mut Game, p: PlayerId) {
    for i in 0..spies(g, p).len() {
        move_to(g, p, i, None);
    }
}

/// Whether a spy can steal technology where it is (`_can_steal`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CanSteal {
    /// Its city's owner knows nothing the spy's civilization could take.
    Nothing,
    /// Its city makes no science.
    NoScience,
    Yes,
}

fn can_steal(g: &Game, p: PlayerId, c: CityId) -> CanSteal {
    let Some(owner) = g.city(c).map(crate::state::cities::City::owner) else {
        return CanSteal::Nothing;
    };
    if techs_to_steal(g, p, owner).is_empty() {
        return CanSteal::Nothing;
    }
    if crate::game::derive::stats::city_stats(g, c).total[Stat::Science] <= 0.0 {
        return CanSteal::NoScience;
    }
    CanSteal::Yes
}

/// A spy's progress toward a theft this turn, and the turns left (`_steal_progress`,
/// `espionage.py:206-219`): its city's science, by its rank and efficiency, toward the dearest
/// tech it could take, scaled by the ruleset and the speed.
fn steal_progress(g: &mut Game, p: PlayerId, i: usize, c: CityId) -> i16 {
    let Some(s) = spy(g, p, i).cloned() else { return 0 };
    let Some(owner) = g.city(c).map(crate::state::cities::City::owner) else { return 0 };
    let f = g.rules().constants().formulas.clone();
    let mut prog = crate::game::derive::stats::city_stats(g, c).total[Stat::Science];
    // In floats, as Python's numbers were: a ruleset's bonus is not bounded, and no product of
    // it may overflow.
    prog *= (f64::from(s.rank) * f64::from(f.spy_rank_steal_percent_bonus) + 75.0) / 100.0;
    prog *= efficiency(g, p, &s);
    let progress = s.progress.saturating_add(num::trunc_i32(prog));
    if let Some(x) = spy_mut(g, p, i) {
        x.progress = progress;
    }
    let techs = g.rules().techs();
    let dearest = techs_to_steal(g, p, owner).into_iter().map(|t| techs[t].cost).max().unwrap_or(0);
    let cost =
        f64::from(dearest) * f.spy_tech_steal_cost_modifier * g.speed().science_cost_modifier;
    let remaining = cost - f64::from(progress);
    if remaining <= 0.0 {
        return 0;
    }
    // Python's `-(-remaining // prog)`; a spy that makes no progress waits as long as a count
    // can say, where Python's number grew without bound.
    let turns = (remaining / prog.max(1e-9)).ceil();
    i16::try_from(num::trunc_i64(turns)).unwrap_or(i16::MAX)
}

/// A theft completes, and the spy may be caught (`_steal_tech`, `espionage.py:222-255`): a tech
/// is drawn among those it could take, and a roll below 300 less its skill (plus a defending
/// spy's) decides: below 0 the theft goes unnoticed, under 100 the victim learns of it but not
/// who, under 200 who, else the spy is killed. Seen or killed, the victim thinks less of the
/// thief.
fn steal_tech(g: &mut Game, p: PlayerId, i: usize, c: CityId) {
    let Some(s) = spy(g, p, i).cloned() else { return };
    let Some((other, at, city)) = g.city(c).map(|x| (x.owner(), x.tile(), x.name.to_string()))
    else {
        return;
    };
    let mut rng =
        Rng::keyed(g.state().seed(), Purpose::Spy, &[i as u64, p.key(), at.key(), g.turn().key()]);
    let options = techs_to_steal(g, p, other);
    let stolen = rng.pick(&options).copied();
    let roll = i32::try_from(rng.below(300)).unwrap_or(0);
    let mut result = roll - skill_percent(g, p, &s);
    let defender = spy_in_city(g, other, c);
    if let Some(d) = defender.and_then(|d| spy(g, other, d).cloned()) {
        result += skill_percent(g, other, &d);
    }
    let me = name(g, p);
    let tech = stolen.and_then(|t| g.rules().name(t)).unwrap_or_default().to_owned();
    if result >= 200 {
        let text =
            format!("A spy from {me} was found and killed trying to steal technology in {city}!");
        tell(g, other, &text, Some(at));
    } else if stolen.is_some() && (0..100).contains(&result) {
        let text = format!("An unidentified spy stole the technology {tech} from {city}!");
        tell(g, other, &text, Some(at));
    } else if stolen.is_some() && result >= 100 {
        let text = format!("A spy from {me} stole the technology {tech} from {city}!");
        tell(g, other, &text, Some(at));
    }
    if result < 200
        && let Some(t) = stolen
    {
        research::add_tech(g, p, t, TechSource::Espionage);
        let text = format!("Your spy {} stole the technology {tech} from {city}!", s.name);
        tell(g, p, &text, Some(at));
        level_up(g, p, i, 1);
    }
    if result >= 200 {
        let text = format!("Your spy {} was killed trying to steal technology in {city}!", s.name);
        tell(g, p, &text, Some(at));
        if let Some(d) = defender {
            level_up(g, other, d, 1);
        }
        kill(g, p, i);
    } else {
        if let Some(x) = spy_mut(g, p, i) {
            x.progress = 0;
        }
        set(g, p, i, SpyAction::StealingTech, 0);
    }
    if result >= 100 {
        add_opinion(g, other, p, OpinionKey::SpiedOnUs, -15.0);
    }
}

/// A spy dies: it leaves its city, and a replacement is recruited in five turns (`kill`,
/// `espionage.py:258-262`).
pub fn kill(g: &mut Game, p: PlayerId, i: usize) {
    move_to(g, p, i, None);
    set(g, p, i, SpyAction::Dead, 5);
    if let Some(s) = spy_mut(g, p, i) {
        s.rank = 1;
    }
}

/// Turns to a city-state's next election, which rigging counts toward (`_election_turns`).
fn election_turns(g: &Game, cs: PlayerId) -> i16 {
    g.player(cs).and_then(|x| x.city_state.as_deref()).and_then(|d| d.election_in).unwrap_or(1)
}

/// One spy's action advances a turn (`spy_end_turn`, `espionage.py:331-382`).
fn spy_end_turn(g: &mut Game, p: PlayerId, i: usize) {
    let Some(s) = spy(g, p, i).cloned() else { return };
    let a = s.action;
    if matches!(a, SpyAction::Moving | SpyAction::EstablishingNetwork | SpyAction::Dead) {
        let left = s.turns.saturating_sub(1);
        if let Some(x) = spy_mut(g, p, i) {
            x.turns = left;
        }
        if left > 0 {
            return;
        }
    }
    if a == SpyAction::None {
        return;
    }
    let c = spy_city(g, &s);
    let owner = c.and_then(|c| g.city(c)).map(crate::state::cities::City::owner);
    match (a, c, owner) {
        (SpyAction::Dead, ..) => {
            let old = s.name.clone();
            let new = spy_name(g, p);
            let rank = starting_rank(g, p);
            if let Some(x) = spy_mut(g, p, i) {
                x.name = new.clone().into();
                x.action = SpyAction::None;
                x.turns = 0;
                x.rank = rank;
            }
            let text = format!("We have recruited a new spy named {new} after {old} was killed.");
            tell(g, p, &text, None);
        }
        (_, Some(c), Some(owner)) => work(g, p, i, a, c, owner),
        _ => move_to(g, p, i, None),
    }
}

/// A spy in a city goes on with what it does there (`espionage.py:344-382`).
fn work(g: &mut Game, p: PlayerId, i: usize, a: SpyAction, c: CityId, owner: PlayerId) {
    match a {
        SpyAction::Moving => {
            if owner == p {
                set(g, p, i, SpyAction::CounterIntelligence, 10);
            } else {
                set(g, p, i, SpyAction::EstablishingNetwork, 3);
            }
        }
        SpyAction::EstablishingNetwork => {
            if g.is_city_state(owner) {
                let turns = (election_turns(g, owner) - 1).max(0);
                set(g, p, i, SpyAction::RiggingElections, turns);
            } else if owner == p {
                set(g, p, i, SpyAction::CounterIntelligence, 10);
            } else {
                if let Some(x) = spy_mut(g, p, i) {
                    x.progress = 0;
                }
                set(g, p, i, SpyAction::StealingTech, 0);
            }
        }
        SpyAction::ObservingCity => {
            if g.player(owner).is_some_and(Player::is_major) && can_steal(g, p, c) == CanSteal::Yes
            {
                set(g, p, i, SpyAction::StealingTech, 0);
            }
        }
        SpyAction::StealingTech => {
            let r = can_steal(g, p, c);
            if r != CanSteal::Yes {
                set(g, p, i, SpyAction::ObservingCity, 0);
                if r == CanSteal::Nothing {
                    let name = spy(g, p, i).map(|s| s.name.to_string()).unwrap_or_default();
                    let text = format!(
                        "Your spy {name} cannot steal any more techs from {} as we've already \
                         researched all the technology they know!",
                        crate::game::diplomacy::relations::name(g, owner)
                    );
                    tell(g, p, &text, None);
                }
                return;
            }
            let turns = steal_progress(g, p, i, c);
            if let Some(x) = spy_mut(g, p, i) {
                x.turns = turns;
            }
            if turns == 0 {
                steal_tech(g, p, i, c);
            }
        }
        SpyAction::RiggingElections => {
            let turns = election_turns(g, owner) - 1;
            if let Some(x) = spy_mut(g, p, i) {
                x.turns = turns;
            }
        }
        SpyAction::Coup => initiate_coup(g, p, i),
        SpyAction::CounterIntelligence => {
            if let Some(x) = spy_mut(g, p, i) {
                x.turns = x.turns.saturating_sub(1);
            }
        }
        SpyAction::None | SpyAction::Dead => {}
    }
}

/// Stage E3: every spy of a major advances a turn, in a game with espionage (`end_turn`,
/// `espionage.py:385-390`).
pub(crate) fn end_turn(g: &mut Game, p: PlayerId) {
    if !g.espionage_enabled() {
        return;
    }
    for i in 0..spies(g, p).len() {
        spy_end_turn(g, p, i);
    }
}

// ---- Elections and coups (espionage.py:265-328, 393-441) --------------------------------------

/// Whether a spy is in a position to stage a coup (`can_coup`, `espionage.py:265-269`): set up in
/// a city-state's city, which is not its civilization's ally.
#[must_use]
pub fn can_coup(g: &Game, p: PlayerId, s: &Spy) -> bool {
    let Some(owner) = spy_city(g, s).and_then(|c| g.city(c)).map(crate::state::cities::City::owner)
    else {
        return false;
    };
    g.is_city_state(owner)
        && s.action.is_set_up()
        && cs_data(g, owner).and_then(crate::state::players::CityStateData::ally) != Some(p)
}

/// The chance a coup succeeds, 0 to 0.85 (`coup_chance`, `espionage.py:272-283`): half the gap
/// between the ally's influence (60 without an ally) and the spy's civilization's, off 50%, and
/// half the gap in skill with the ally's spy there, which the spy's owner does not see
/// (`include_unknown` false).
#[must_use]
pub fn coup_chance(g: &Game, p: PlayerId, s: &Spy, include_unknown: bool) -> f64 {
    let Some((c, cs)) = spy_city(g, s).and_then(|c| g.city(c)).map(|x| (x.id(), x.owner())) else {
        return 0.0;
    };
    let ally = cs_data(g, cs).and_then(crate::state::players::CityStateData::ally);
    let mut diff = ally.map_or(60.0, |a| cs_influence::influence(g, cs, a));
    diff -= cs_influence::influence(g, cs, p);
    let mut pct = 50.0 - diff / 2.0;
    let defender = ally
        .filter(|_| include_unknown)
        .and_then(|a| spy_in_city(g, a, c).and_then(|d| spy(g, a, d).map(|x| (a, x.clone()))));
    let ranks = skill_percent(g, p, s) - defender.map_or(0, |(a, d)| skill_percent(g, a, &d));
    pct += f64::from(ranks) / 2.0;
    pct.clamp(0.0, 85.0) / 100.0
}

/// A coup is resolved at the end of its spy's turn (`_initiate_coup`, `espionage.py:286-323`):
/// on success the spy's civilization takes the ally's influence (80 without an ally), the old
/// ally loses 20 and the others who know the city-state 10, and the spy goes back to rigging; on
/// failure its civilization loses 20 influence and the spy is killed. A spy no longer placed to
/// stage one goes back to rigging.
fn initiate_coup(g: &mut Game, p: PlayerId, i: usize) {
    let Some(s) = spy(g, p, i).cloned() else { return };
    if !can_coup(g, p, &s) {
        set(g, p, i, SpyAction::RiggingElections, 10);
        return;
    }
    let Some((c, cs, at)) =
        spy_city(g, &s).and_then(|c| g.city(c)).map(|x| (x.id(), x.owner(), x.tile()))
    else {
        return;
    };
    let ally = cs_data(g, cs).and_then(crate::state::players::CityStateData::ally);
    let chance = coup_chance(g, p, &s, true);
    let mut rng =
        Rng::keyed(g.state().seed(), Purpose::Spy, &[i as u64, p.key(), at.key(), g.turn().key()]);
    let (me, csn) = (name(g, p), name(g, cs));
    let refused = |r: Result<(), crate::state::StateError>| {
        debug_assert!(r.is_ok(), "a city-state's influence refused: {r:?}");
    };
    if rng.unit() <= chance {
        let prev = ally.map_or(80.0, |a| cs_influence::influence(g, cs, a));
        refused(cs_influence::set_influence(g, cs, p, prev));
        tell(g, p, &format!("Your spy {} successfully staged a coup in {csn}!", s.name), Some(at));
        if let Some(a) = ally {
            refused(cs_influence::add_influence(g, cs, a, -20.0));
            let text =
                format!("A spy from {me} successfully staged a coup in our former ally {csn}!");
            tell(g, a, &text, Some(at));
            add_opinion(g, a, p, OpinionKey::SpiedOnUs, -15.0);
        }
        let others: Vec<PlayerId> = g
            .majors(true)
            .map(Player::id)
            .filter(|&q| Some(q) != ally && q != p && g.has_met(q, cs))
            .collect();
        for q in others {
            tell(g, q, &format!("A spy from {me} successfully staged a coup in {csn}!"), Some(at));
            refused(cs_influence::add_influence(g, cs, q, -10.0));
        }
        set(g, p, i, SpyAction::RiggingElections, 10);
        refused(cs_influence::update_ally(g, cs));
    } else {
        let defender = ally.and_then(|a| spy_in_city(g, a, c).map(|d| (a, d)));
        refused(cs_influence::add_influence(g, cs, p, -20.0));
        if let Some(a) = ally {
            let text =
                format!("A spy from {me} failed to stage a coup in our ally {csn} and was killed!");
            tell(g, a, &text, Some(at));
            add_opinion(g, a, p, OpinionKey::SpiedOnUs, -10.0);
        }
        let text = format!("Our spy {} failed to stage a coup in {csn} and was killed!", s.name);
        tell(g, p, &text, Some(at));
        kill(g, p, i);
        if let Some((a, d)) = defender {
            level_up(g, a, d, 1);
        }
    }
}

/// A city-state's election countdown, at the end of its turn (`city_state_election_tick`,
/// `espionage.py:393-402`): the first is drawn up to the ruleset's interval away, then it counts
/// down and holds the election at 0.
pub fn city_state_election_tick(g: &mut Game, cs: PlayerId) {
    let n = g.rules().constants().formulas.city_state_election_turns;
    let Some(left) = cs_data(g, cs).map(|d| d.election_in) else { return };
    let next = match left {
        None => {
            let mut rng = Rng::keyed(g.state().seed(), Purpose::ElectionDelay, &[cs.key()]);
            let first = rng.below(u64::try_from(n.max(0)).unwrap_or(0) + 1);
            i16::try_from(first).unwrap_or(i16::MAX)
        }
        Some(x) => x - 1,
    };
    if let Some(d) = cs_influence::data_mut(g, cs) {
        d.election_in = Some(next);
    }
    if left.is_some() && next <= 0 {
        hold_elections(g, cs);
    }
}

/// A city-state's election (`hold_elections`, `espionage.py:405-441`): among the spies rigging it
/// in its capital, and nobody (weight 20), weighted by half the civilization's influence plus the
/// spy's skill by its efficiency. The winner gains 20 influence and every other major that knows
/// the city-state loses 5; if nobody wins, the riggers lose 5.
pub fn hold_elections(g: &mut Game, cs: PlayerId) {
    let n = g.rules().constants().formulas.city_state_election_turns;
    if let Some(d) = cs_influence::data_mut(g, cs) {
        d.election_in = Some(i16::try_from(n).unwrap_or(i16::MAX));
    }
    let Some((cap, cap_name, at)) = g
        .player(cs)
        .and_then(|p| p.capital)
        .and_then(|c| g.city(c))
        .map(|c| (c.id(), c.name.to_string(), c.tile()))
    else {
        return;
    };
    let riggers: Vec<(PlayerId, usize)> = spies_in_city(g, cap)
        .into_iter()
        .filter(|&(p, i)| spy(g, p, i).is_some_and(|s| s.action == SpyAction::RiggingElections))
        .collect();
    if riggers.is_empty() {
        return;
    }
    let mut weights: Vec<f64> = riggers
        .iter()
        .map(|&(p, i)| {
            let s = spy(g, p, i).cloned();
            s.map_or(0.0, |s| {
                f64::max(
                    0.0,
                    cs_influence::influence(g, cs, p) / 2.0
                        + f64::from(skill_percent(g, p, &s)) * efficiency(g, p, &s),
                )
            })
        })
        .collect();
    weights.push(20.0);
    let mut rng = Rng::keyed(g.state().seed(), Purpose::Election, &[cs.key(), g.turn().key()]);
    let winner = if weights.iter().sum::<f64>() > 0.0 {
        rng.weighted(&weights).and_then(|w| riggers.get(w)).map(|&(p, _)| p)
    } else {
        None
    };
    let refused = |r: Result<(), crate::state::StateError>| {
        debug_assert!(r.is_ok(), "a city-state's influence refused: {r:?}");
    };
    let Some(winner) = winner else {
        for &(p, _) in &riggers {
            refused(cs_influence::add_influence(g, cs, p, -5.0));
            tell(g, p, &format!("Your spy lost the election in {cap_name}!"), Some(at));
        }
        return;
    };
    let ally = cs_data(g, cs).and_then(crate::state::players::CityStateData::ally);
    let csn = name(g, cs);
    let wn = name(g, winner);
    let majors: Vec<PlayerId> = g.majors(true).map(Player::id).collect();
    for q in majors {
        if !g.has_met(cs, q) {
            continue;
        }
        refused(cs_influence::add_influence(g, cs, q, if q == winner { 20.0 } else { -5.0 }));
        if q == winner {
            tell(g, q, &format!("Your spy successfully rigged the election in {csn}!"), Some(at));
        } else if riggers.iter().any(|&(p, _)| p == q) {
            tell(g, q, &format!("Your spy lost the election in {csn} to {wn}!"), Some(at));
        } else if Some(q) == ally {
            tell(g, q, &format!("The election in {csn} was rigged by {wn}!"), Some(at));
        }
    }
}

// ---- The tool (espionage.py:457-488) ----------------------------------------------------------

/// One of the civilization's spies by name, in any case (`_get_spy`, `espionage.py:457-466`).
///
/// # Errors
/// No spies at all, or none by that name.
pub fn find_spy(g: &Game, p: PlayerId, name: &Value) -> Result<usize, ActionError> {
    let list = spies(g, p);
    if list.is_empty() {
        return Err(ActionError::rule(
            "You have no spies. Spies are recruited when a civilization enters a new era (from \
             the Renaissance) and from some wonders.",
        ));
    }
    let raw = py::str_of(name);
    let wanted = py::strip(&raw).to_lowercase();
    list.iter().position(|s| s.name.to_lowercase() == wanted).ok_or_else(|| {
        let names: Vec<&str> = list.iter().map(|s| &*s.name).collect();
        ActionError::rule(format!("No spy named '{raw}'. Your spies: {}.", names.join(", ")))
    })
}

/// `move_spy`: a spy goes to a city its civilization has explored, or back to its hideout
/// (`tools.move_spy`, `espionage.move_spy`, `espionage.py:469-488`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MoveSpy {
    pub spy: Value,
    /// A city id, or `"hideout"`.
    pub city_id: Value,
}

/// Where a spy is sent.
pub enum SpyMove {
    Hideout(usize),
    /// It is there already.
    Stay(usize, CityId),
    To(usize, CityId),
}

impl Rule for MoveSpy {
    type Plan = SpyMove;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<SpyMove, ActionError> {
        if !g.espionage_enabled() {
            return Err(ActionError::rule("Espionage is disabled in this game."));
        }
        let i = find_spy(g, pid, &self.spy)?;
        let s = &spies(g, pid)[i];
        let where_ = py::str_of(&self.city_id).to_lowercase();
        if self.city_id.is_null() || ["hideout", "none", ""].contains(&where_.as_str()) {
            if s.action == SpyAction::Dead {
                return Err(ActionError::rule(format!("{} is dead.", s.name)));
            }
            return Ok(SpyMove::Hideout(i));
        }
        let c = py::int_of(&self.city_id)
            .and_then(|n| u32::try_from(n).ok())
            .and_then(CityId::new)
            .filter(|&c| g.city(c).is_some())
            .ok_or_else(|| {
                ActionError::rule(format!("No city with id {}.", py::str_of(&self.city_id)))
            })?;
        if let Some(why) = can_move_to(g, pid, s, c) {
            return Err(ActionError::rule(why));
        }
        Ok(if s.city == Some(c) { SpyMove::Stay(i, c) } else { SpyMove::To(i, c) })
    }

    fn apply(self, g: &mut Game, pid: PlayerId, plan: SpyMove) -> OutcomeSpec {
        let spy_name = |g: &Game, i: usize| spy(g, pid, i).map(|s| s.name.to_string());
        let city_name = |g: &Game, c: CityId| g.city(c).map(|x| x.name.to_string());
        OutcomeSpec::value(match plan {
            SpyMove::Hideout(i) => {
                move_to(g, pid, i, None);
                json!({"spy": spy_name(g, i), "location": "hideout"})
            }
            SpyMove::Stay(i, c) => {
                let action = spy(g, pid, i).map(|s| s.action.name());
                json!({"spy": spy_name(g, i), "location": city_name(g, c), "action": action})
            }
            SpyMove::To(i, c) => {
                move_to(g, pid, i, Some(c));
                json!({"spy": spy_name(g, i), "moving_to": city_name(g, c), "arrives_in_turns": 1})
            }
        })
    }
}

/// `stage_coup`: a spy in a city-state's capital stages a coup at the end of its civilization's
/// turn (`tools.stage_coup`, `espionage.stage_coup`, `espionage.py:491-498`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StageCoup {
    pub spy: Value,
}

impl Rule for StageCoup {
    type Plan = usize;

    fn check(&self, g: &Game, pid: PlayerId) -> Result<usize, ActionError> {
        let i = find_spy(g, pid, &self.spy)?;
        if !can_coup(g, pid, &spies(g, pid)[i]) {
            return Err(ActionError::rule(
                "A coup needs a set-up spy in the capital of a city-state that is not allied with \
                 you.",
            ));
        }
        Ok(i)
    }

    fn apply(self, g: &mut Game, pid: PlayerId, i: usize) -> OutcomeSpec {
        let chance = spy(g, pid, i).map_or(0.0, |s| coup_chance(g, pid, s, false));
        set(g, pid, i, SpyAction::Coup, 0);
        let name = spy(g, pid, i).map(|s| s.name.to_string());
        OutcomeSpec::value(json!({
            "spy": name,
            "coup_at_end_of_turn": true,
            "estimated_success_chance": num::round_half_even_i64(chance * 100.0),
        }))
    }
}

/// A civilization's spies as scripts read them: `name`, `rank`, `city` (an id, or null at the
/// hideout), `action`, `turns` and `progress`.
#[must_use]
pub fn spies_json(g: &Game, p: PlayerId) -> Value {
    Value::Array(
        spies(g, p)
            .iter()
            .map(|s| {
                json!({
                    "name": &*s.name, "rank": s.rank, "city": s.city.map(CityId::get),
                    "action": s.action.name(), "turns": s.turns, "progress": s.progress,
                })
            })
            .collect(),
    )
}
