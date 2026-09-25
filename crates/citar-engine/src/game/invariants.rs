//! What must hold of a game at every settle point (DESIGN.md 9.4).
//!
//! [`check`] reads the game and returns every [`Violation`] it finds, each under the code of the
//! invariant it breaks. Settle runs it when `DebugOptions::invariants` is on (the default in
//! debug builds and with the `checks` feature), and testkit after every public call. It only
//! reads, so a game plays the same with the checks on or off.
//!
//! Python had no invariants; a few of these catch states its bugs produced (DESIGN.md 4.4-4.6).
//! Two bounds of
//! DESIGN.md 9.4 do not hold in play, Python's or this engine's, and are left out (package 1c-02):
//! a unit's movement may exceed its allowance (movement gained from a unique, or transferred by a
//! unit it has since left), so it is held to a sanity cap instead; and units stack where a move
//! that passes through its own units stops on one (an enemy comes into view), where a great
//! person is born in a garrisoned city, and where the editor puts them.

use core::fmt;

use super::Game;
use crate::base::ids::{CityId, TileIdx};
use crate::state::Phase;
use crate::state::cities::{City, Constructible};
use crate::state::diplo::NegStatus;

/// An invariant of DESIGN.md 9.4.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Code {
    /// Ids are unique and every live id is below its counter; deals and negotiations are kept in
    /// ascending id order.
    Id1,
    /// The occupancy and owner indexes match the units; carried units share their carrier's tile.
    Occ1,
    /// A unit's owner is alive, its tile on the map, its health, moves and experience in range.
    Unit1,
    /// A city stands on a tile that is its own, with population and health in range and a queue
    /// of things that exist.
    City1,
    /// A city employs no more than its citizens and keeps its tile lists sorted; a city whose
    /// citizens the engine assigned works only tiles of its owner in its range, no city centre,
    /// and none another city works.
    City2,
    /// A tile's city exists and has the tile's owner.
    Tile1,
    /// Every float is finite, and every stock but gold is not negative.
    Player1,
    /// A living major with cities has one capital among them; a dead player has no units or
    /// cities.
    Player2,
    /// War and contact are symmetric and match the masks; war excludes open borders,
    /// friendship and pacts.
    Diplo1,
    /// An open negotiation awaits one of its parties; its entries are numbered without gaps and
    /// within the cap.
    Neg1,
    /// What a civilization sees, it has explored.
    Vis1,
    /// The current player is alive unless the game is over, and a winner is set only when it
    /// is.
    Turn1,
    /// Nothing is pending at a settle point.
    Pend1,
    /// Settle converged within its pass cap, and a chain of one-time effects stayed within its
    /// depth (`game::triggers::TRIGGER_DEPTH`).
    Settle1,
    /// Every cache equals a cold recompute (the cache oracle).
    Cache1,
}

impl Code {
    /// The code as DESIGN.md 9.4 writes it: `CITY-2`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Id1 => "ID-1",
            Self::Occ1 => "OCC-1",
            Self::Unit1 => "UNIT-1",
            Self::City1 => "CITY-1",
            Self::City2 => "CITY-2",
            Self::Tile1 => "TILE-1",
            Self::Player1 => "PLAYER-1",
            Self::Player2 => "PLAYER-2",
            Self::Diplo1 => "DIPLO-1",
            Self::Neg1 => "NEG-1",
            Self::Vis1 => "VIS-1",
            Self::Turn1 => "TURN-1",
            Self::Pend1 => "PEND-1",
            Self::Settle1 => "SETTLE-1",
            Self::Cache1 => "CACHE-1",
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A broken invariant: its code, and what exactly is wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub code: Code,
    pub message: String,
}

impl Violation {
    /// A violation of `code`.
    #[must_use]
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// Every invariant the game breaks, in code order, then in the order found.
#[must_use]
pub fn check(g: &Game) -> Vec<Violation> {
    let mut out = Out(Vec::new());
    ids(g, &mut out);
    occupancy(g, &mut out);
    units(g, &mut out);
    cities(g, &mut out);
    tiles(g, &mut out);
    players(g, &mut out);
    diplomacy(g, &mut out);
    negotiations(g, &mut out);
    vis(g, &mut out);
    clock(g, &mut out);
    if !g.pending.is_empty() || !g.fx.is_empty() {
        out.push(Code::Pend1, "work is pending at a settle point".to_owned());
    }
    out.0
}

struct Out(Vec<Violation>);

impl Out {
    fn push(&mut self, code: Code, message: String) {
        self.0.push(Violation { code, message });
    }
}

fn ids(g: &Game, out: &mut Out) {
    let st = &g.st;
    let ids = st.ids();
    let past = |what: &str, next: u32, last: Option<u32>, out: &mut Out| {
        if let Some(l) = last
            && l >= next
        {
            out.push(Code::Id1, format!("{what} {l} is at or past the next {what} id {next}"));
        }
    };
    past("unit", ids.unit, st.units().store().last_id().map(|u| u.get()), out);
    past("city", ids.city, st.cities().store().last_id().map(CityId::get), out);
    past("camp", ids.camp, st.world().camps.keys().next_back().map(|c| c.get()), out);
    past("deal", ids.deal, st.diplo().deals.iter().map(|d| d.id.get()).max(), out);
    let negs = st.diplo().negotiations.iter().map(|n| n.id.get()).max();
    past("negotiation", ids.negotiation, negs, out);
    for u in st.units().iter() {
        if st.units().get(u.id()).map(crate::state::units::Unit::id) != Some(u.id()) {
            out.push(Code::Id1, format!("unit {} is not stored under its id", u.id()));
        }
    }
    for c in st.cities().iter() {
        if st.cities().get(c.id()).map(City::id) != Some(c.id()) {
            out.push(Code::Id1, format!("city {} is not stored under its id", c.id()));
        }
    }
    // Ascending, so no two share an id and a lookup may search them.
    if !st.diplo().deals.windows(2).all(|w| w[0].id < w[1].id) {
        out.push(Code::Id1, "the deals are not in ascending id order".to_owned());
    }
    if !st.diplo().negotiations.windows(2).all(|w| w[0].id < w[1].id) {
        out.push(Code::Id1, "the negotiations are not in ascending id order".to_owned());
    }
    let next_event = st.host().next_event_id;
    if let Some(e) = g.chron.events().last()
        && e.id.get() >= next_event
    {
        out.push(Code::Id1, format!("event {} is at or past the next event id {next_event}", e.id));
    }
}

fn occupancy(g: &Game, out: &mut Out) {
    if let Err(e) = g.st.units().verify() {
        out.push(Code::Occ1, e.to_string());
    }
    if let Err(e) = g.st.cities().verify() {
        out.push(Code::Occ1, e.to_string());
    }
}

fn units(g: &Game, out: &mut Out) {
    let st = &g.st;
    let size = st.map().size();
    for u in st.units().iter() {
        let id = u.id();
        if !st.player(u.owner()).is_some_and(crate::state::players::Player::alive) {
            out.push(
                Code::Unit1,
                format!("unit {id} belongs to player {}, who is not alive", u.owner()),
            );
        }
        if u.tile().0 >= size {
            out.push(Code::Unit1, format!("unit {id} is on tile {}, off the map", u.tile()));
        }
        if !(1..=100).contains(&u.hp) {
            out.push(Code::Unit1, format!("unit {id} has {} health", u.hp));
        }
        if u.moves < 0 {
            out.push(Code::Unit1, format!("unit {id} has {} moves", u.moves));
        }
        if u.xp < 0 {
            out.push(Code::Unit1, format!("unit {id} has {} experience", u.xp));
        }
    }
    // Movement: no unit holds more than a hundred movement points. A unit's allowance is no
    // bound, since movement gained from a unique, or taken from a unit it has since left, may put
    // it above it (DESIGN.md 6.10); this catches movement nothing in the rules could give.
    let cap = g.rules().constants().move_scale.saturating_mul(MOVES_CAP);
    for u in st.units().iter() {
        if u.moves > cap {
            out.push(Code::Unit1, format!("unit {} has {} moves", u.id(), u.moves));
        }
    }
}

/// The most movement points a unit may hold (UNIT-1).
const MOVES_CAP: i32 = 100;

fn cities(g: &Game, out: &mut Out) {
    let st = &g.st;
    let r = g.rules;
    let mut worked_by: std::collections::BTreeMap<TileIdx, CityId> =
        std::collections::BTreeMap::new();
    let work_range = u32::try_from(r.constants().formulas.city_work_range).unwrap_or(0);
    for c in st.cities().iter() {
        let id = c.id();
        match st.tiles().get(c.tile()) {
            None => {
                out.push(Code::City1, format!("city {id} stands on tile {}, off the map", c.tile()))
            }
            Some(t) => {
                if t.city() != Some(id) || t.owner() != Some(c.owner()) {
                    out.push(
                        Code::City1,
                        format!(
                            "city {id}'s tile {} is not its own (city {:?}, owner {:?})",
                            c.tile(),
                            t.city(),
                            t.owner()
                        ),
                    );
                }
            }
        }
        if c.pop < 1 {
            out.push(Code::City1, format!("city {id} has population {}", c.pop));
        }
        if c.health <= 0 {
            out.push(Code::City1, format!("city {id} has {} health", c.health));
        }
        let most = super::cities::stats::max_health(g, id);
        if c.health > most {
            out.push(Code::City1, format!("city {id} has {} health of {most} at most", c.health));
        }
        for item in &c.queue {
            let known = match *item {
                Constructible::Building(b) => r.buildings().get(b).is_some(),
                Constructible::Unit(u) => r.base_units().get(u).is_some(),
                Constructible::Perpetual(_) => true,
            };
            if !known {
                out.push(
                    Code::City1,
                    format!("city {id}'s queue holds {item:?}, which the ruleset lacks"),
                );
            }
        }
        let specialists: u32 = c.specialists.iter().map(|&n| u32::from(n)).sum();
        let employed =
            u32::try_from(c.worked.len()).unwrap_or(u32::MAX).saturating_add(specialists);
        if employed > u32::from(c.pop) {
            out.push(
                Code::City2,
                format!(
                    "city {id} works {} tiles with {specialists} specialists and {} citizens",
                    c.worked.len(),
                    c.pop
                ),
            );
        }
        if !strictly_sorted(&c.worked) || !strictly_sorted(&c.locked) {
            out.push(Code::City2, format!("city {id}'s worked or locked tiles are not sorted"));
        }
        // Which tiles a city may work is the engine's own rule for the cities it has assigned;
        // a converted city keeps Python's worked tiles until then (DESIGN.md 6.8), and Python let
        // two cities work a tile neither owned (cities.py:170-193).
        if !c.citizens_settled {
            continue;
        }
        for &t in &c.worked {
            let tile = st.tiles().get(t);
            let workable = tile.is_some_and(|x| x.owner() == Some(c.owner()))
                && st.city_at(t).is_none()
                && g.grid().distance(c.tile(), t) <= work_range;
            if !workable {
                out.push(Code::City2, format!("city {id} works tile {t}, which it may not work"));
            }
            if let Some(other) = worked_by.insert(t, id) {
                out.push(Code::City2, format!("cities {other} and {id} both work tile {t}"));
            }
        }
    }
    for c in st.cities().iter().filter(|c| !c.citizens_settled) {
        for &t in &c.worked {
            if let Some(&other) = worked_by.get(&t) {
                out.push(Code::City2, format!("cities {other} and {} both work tile {t}", c.id()));
            }
        }
    }
}

fn strictly_sorted(v: &[TileIdx]) -> bool {
    v.windows(2).all(|w| w[0] < w[1])
}

fn tiles(g: &Game, out: &mut Out) {
    let st = &g.st;
    for (t, tile) in st.tiles().iter() {
        let Some(c) = tile.city() else { continue };
        match st.cities().get(c) {
            None => {
                out.push(Code::Tile1, format!("tile {t} belongs to city {c}, which does not exist"))
            }
            Some(city) if tile.owner() != Some(city.owner()) => out.push(
                Code::Tile1,
                format!(
                    "tile {t} belongs to city {c} of player {} but is owned by {:?}",
                    city.owner(),
                    tile.owner()
                ),
            ),
            Some(_) => {}
        }
    }
}

/// Reports `v` if it is not finite. What it is gets written only then: the check runs at every
/// settle over every stock, progress, influence and opinion.
fn finite(v: f64, what: impl FnOnce() -> String, out: &mut Out) {
    if !v.is_finite() {
        out.push(Code::Player1, format!("{} is {v}", what()));
    }
}

fn players(g: &Game, out: &mut Out) {
    let st = &g.st;
    for (p, pl) in st.players().iter() {
        let e = &pl.econ;
        for (name, v) in [
            ("gold", e.gold),
            ("culture", e.culture),
            ("faith", e.faith),
            ("golden age points", e.golden_age_points),
            ("last gold rate", e.last_gold_rate),
            ("research overflow", pl.tech.overflow),
        ] {
            finite(v, || format!("player {p}'s {name}"), out);
        }
        for (name, v) in
            [("culture", e.culture), ("faith", e.faith), ("golden age points", e.golden_age_points)]
        {
            if v < 0.0 {
                out.push(Code::Player1, format!("player {p}'s {name} is {v}, below 0"));
            }
        }
        for (&t, &v) in &pl.tech.progress {
            finite(v, || format!("player {p}'s progress on tech {t:?}"), out);
        }
        for (&u, &v) in pl.gp.points.iter().chain(&pl.gp.combat_points) {
            finite(v, || format!("player {p}'s great person points toward {u:?}"), out);
        }
        if let Some(cs) = pl.city_state.as_deref() {
            for (q, &v) in cs.influence.iter() {
                finite(v, || format!("player {p}'s influence with {q}"), out);
            }
        }
        let cities = st.cities().of(p);
        let units = st.units().of(p);
        if pl.alive() {
            if pl.is_major()
                && !cities.is_empty()
                && !pl.capital.is_some_and(|c| cities.contains(&c))
            {
                out.push(
                    Code::Player2,
                    format!("player {p} has cities but its capital is {:?}", pl.capital),
                );
            }
        } else if !cities.is_empty() || !units.is_empty() {
            out.push(
                Code::Player2,
                format!(
                    "player {p} is dead but holds {} cities and {} units",
                    cities.len(),
                    units.len()
                ),
            );
        }
    }
    for c in st.cities().iter() {
        for (name, v) in [("food", c.food), ("culture", c.culture), ("overflow", c.overflow)] {
            finite(v, || format!("city {}'s {name}", c.id()), out);
        }
        for (item, &v) in &c.progress {
            finite(v, || format!("city {}'s progress on {item:?}", c.id()), out);
        }
    }
    for ((holder, about), values) in st.diplo().opinions.iter() {
        for &v in values {
            finite(v, || format!("{holder}'s opinion of {about}"), out);
        }
    }
}

fn diplomacy(g: &Game, out: &mut Out) {
    if let Err(e) = g.st.diplo().verify() {
        out.push(Code::Diplo1, e);
    }
    let turn = g.turn();
    for (a, b, r) in g.st.diplo().relations().pairs() {
        if !r.war {
            continue;
        }
        let open = r.open_borders_until.iter().any(|&u| u >= turn);
        if open || r.friendship_until >= turn || r.pact_until >= turn {
            out.push(
                Code::Diplo1,
                format!(
                    "{a} and {b} are at war with open borders, a friendship or a pact in force"
                ),
            );
        }
    }
}

fn negotiations(g: &Game, out: &mut Out) {
    let st = &g.st;
    let cap = match st.config().diplomacy.max_chat_messages {
        Some(n) if n > 0 => usize::from(n.max(2)),
        _ => usize::try_from(g.rules.constants().diplomacy.max_chat_messages).unwrap_or(usize::MAX),
    };
    for n in &st.diplo().negotiations {
        let id = n.id;
        if n.status == NegStatus::Open
            && !n.awaiting.is_some_and(|w| w == n.initiator || w == n.responder)
        {
            out.push(
                Code::Neg1,
                format!("open negotiation {id} awaits {:?}, not one of its parties", n.awaiting),
            );
        }
        if n.history.iter().enumerate().any(|(i, e)| usize::from(e.seq) != i + 1) {
            out.push(
                Code::Neg1,
                format!("negotiation {id}'s entries are not numbered from 1 without gaps"),
            );
        }
        // The message that would pass the cap closes the chat instead, and the closing entry is
        // the one past it (diplomacy.py:852-858).
        let limit = if n.status == NegStatus::Open { cap } else { cap.saturating_add(1) };
        if n.history.len() > limit {
            out.push(
                Code::Neg1,
                format!(
                    "negotiation {id} holds {} entries, past its cap of {cap}",
                    n.history.len()
                ),
            );
        }
    }
}

fn vis(g: &Game, out: &mut Out) {
    for (p, pl) in g.st.players().iter() {
        if pl.is_barbarian() {
            continue;
        }
        let Some(visible) = g.dv.vis.visible(p) else { continue };
        if !visible.is_subset(&pl.explored) {
            let mut extra = visible.clone();
            extra.difference_with(&pl.explored);
            let first = extra.iter().next().unwrap_or_default();
            out.push(Code::Vis1, format!("player {p} sees tile {first} but has not explored it"));
        }
    }
}

fn clock(g: &Game, out: &mut Out) {
    let c = g.st.clock();
    let over = c.phase == Phase::Over;
    if !over && !g.st.player(c.current).is_some_and(crate::state::players::Player::alive) {
        out.push(Code::Turn1, format!("it is player {}'s turn, who is not alive", c.current));
    }
    // A game may end with no winner: at its turn limit with the Time victory off
    // (`victory.py:363-365`). A winner in a game that goes on is a bug.
    if !over && c.winner.is_some() {
        out.push(Code::Turn1, format!("the game goes on with winner {:?}", c.winner));
    }
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests;
