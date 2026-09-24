//! Sight brought up to date, and what it reveals (DESIGN.md 6.4, 6.7, 6.9): `visibility.refresh`
//! (`visibility.py:128-168`), `_discover_natural_wonders` (`visibility.py:171-198`) and
//! `reveal_tiles` (`visibility.py:232-243`).
//!
//! Python recomputed every civilization's sight after every unit step and compared it with the
//! last. Here a write marks the sources it made stale (`pending::SightSource`), and
//! `Game::sync_sight` brings those alone up to date, at the next settle or when a rule needs
//! sight current mid-call. What changed is a list of transitions, tiles whose count left or
//! reached zero, netted over the whole sync:
//! - a tile coming into sight is explored, by every player with sight (the barbarians keep no
//!   explored tiles);
//! - a tile going out of it is remembered as it looks now, by a living major only (Python kept
//!   memories for city-states too, and never read them);
//! - the viewer meets the tile's owner and the owners of the units on it, and a major discovers a
//!   natural wonder on it.
//!
//! First contact is Python's rule, checked where it can change instead of over every visible tile
//! on every refresh: on a tile coming into sight; on a unit arriving on a tile, or changing hands
//! (every civilization that sees the tile meets its owner); on a tile or a city changing hands
//! (every civilization that sees it meets the new owner: border growth, a tile bought, a city
//! founded in sight, a capture, a liberation); and, for a player revived or two players who forgot
//! each other, everything the player sees and everything of its that others see. Nobody meets
//! itself, the barbarians, a dead player or one it has met, and two city-states never meet
//! (`visibility.py:160-165`). Meetings and discoveries are queued as effects, which settle applies
//! in their order: the meetings viewer first, so they go viewer by viewer as Python's did, then
//! the discoveries.

use std::collections::{BTreeMap, BTreeSet};

use super::sight::{ally_sources, city_source, has_sight, sight_mods, spy_sources, unit_source};
use super::visibility::{SourceKey, Transition, VisSource, Visibility};
use crate::base::ids::{CityId, PlayerId, TileIdx, UnitId};
use crate::base::stats::{Stat, Stats};
use crate::game::derive::rev::{PlayerTouch, Rev};
use crate::game::pending::{Effect, SightSource};
use crate::game::{Game, Porting, pending};
use crate::state::chronicle::{EngineEvent, EventData};
use crate::state::map::Tile;
use crate::state::memory::CityMemory;
use crate::unique::{CondDeps, Ctx, UniqueData, UniqueType, uq};

// ---- The rule of first contact -----------------------------------------------------------------

/// Whether `viewer`, seeing something of `q`'s, meets `q` (`visibility.py:160-165`).
fn may_meet(g: &Game, viewer: PlayerId, q: PlayerId) -> bool {
    viewer != q
        && has_sight(g, viewer)
        && g.player(q).is_some_and(|x| x.alive() && !x.is_barbarian())
        && !g.has_met(viewer, q)
        && !(g.is_city_state(viewer) && g.is_city_state(q))
}

/// The players a civilization seeing tile `t` sees something of: its owner, then the owners of
/// the units on it (`visibility.py:153-159`).
fn owners_on(g: &Game, t: TileIdx) -> impl Iterator<Item = PlayerId> + '_ {
    g.tile(t).and_then(Tile::owner).into_iter().chain(g.units_at(t).map(|u| u.owner()))
}

/// Whether major `p` has a natural wonder to discover on tile `t` (`visibility.py:175-178`).
fn undiscovered(g: &Game, p: PlayerId, t: TileIdx) -> bool {
    let Some(w) = g.tile(t).and_then(Tile::wonder) else { return false };
    g.player(p).is_some_and(|x| x.is_major() && x.alive() && !x.civ.natural_wonders.contains(w))
}

/// What one sync found to do: meetings and discoveries.
#[derive(Default)]
struct Found {
    /// Each pair that meets, the lower id first, with the viewer that meets the other: the
    /// lowest id of those that saw something of the other's. Python's refresh looped over the
    /// viewers in id order and met each with what it saw, viewer first (`visibility.py:152-165`),
    /// so the lowest viewer made the meeting, and its announcement names it first.
    meet: BTreeMap<(PlayerId, PlayerId), PlayerId>,
    discover: BTreeSet<(PlayerId, TileIdx)>,
}

impl Found {
    fn meet(&mut self, g: &Game, viewer: PlayerId, q: PlayerId) {
        if may_meet(g, viewer, q) {
            let v = self.meet.entry((viewer.min(q), viewer.max(q))).or_insert(viewer);
            *v = (*v).min(viewer);
        }
    }

    /// The meetings as effects, each viewer first: the queue applies them in Python's order,
    /// viewer by viewer.
    fn meetings(&self) -> impl Iterator<Item = Effect> + '_ {
        self.meet
            .iter()
            .map(|(&(lo, hi), &v)| Effect::Meet { a: v, b: if v == lo { hi } else { lo } })
    }

    /// What a player seeing tile `t` newly meets and discovers.
    fn seen(&mut self, g: &Game, viewer: PlayerId, t: TileIdx) {
        for q in owners_on(g, t) {
            self.meet(g, viewer, q);
        }
        if undiscovered(g, viewer, t) {
            self.discover.insert((viewer, t));
        }
    }

    /// Every contact and discovery of `p`'s that Python's refresh would make: what it sees, and
    /// who sees its tiles and units.
    fn everything_of(&mut self, g: &Game, vis: &Visibility, p: PlayerId) {
        if let Some(seen) = vis.visible(p) {
            for t in seen.iter() {
                self.seen(g, p, TileIdx(t));
            }
        }
        if !g.player(p).is_some_and(|x| x.alive() && !x.is_barbarian()) {
            return;
        }
        let mut theirs: Vec<TileIdx> =
            crate::game::economy::owned_tiles(g, p).map(|v| v.clone()).unwrap_or_default();
        theirs.extend(g.player_units(p).map(crate::state::units::Unit::tile));
        for t in theirs {
            for q in vis.seers(t) {
                self.meet(g, q, p);
            }
        }
    }
}

// ---- Sight mods (DESIGN.md 6.5) -----------------------------------------------------------------

/// The latest revision of what a civilization gives its units' sight, as the ruleset's sight
/// uniques read it: its index, when a civilization-wide source carries one; its supply, when a
/// resource does; the civilization-level classes their conditionals read; and everything, when
/// they read a city, a fight or the map.
fn civ_sight_rev(g: &Game, p: PlayerId) -> Rev {
    let rules = g.dv.vis.sight_rules();
    let revs = &g.dv.revs;
    if rules.anywhere {
        return revs.now();
    }
    let mut r = Rev::START;
    if rules.civ_sources {
        r = r.max(revs.civ(p).index);
    }
    if !rules.civ_deps.is_empty() {
        r = r.max(revs.cond_civ(Some(p), rules.civ_deps));
    }
    if rules.resource_sources {
        r = r.max(crate::game::derive::civ::cond(g, CondDeps::RESOURCES, &Ctx::civ(p)));
    }
    r
}

/// A tile as a major remembers it: where, the tile, and the city on it.
type Snapshot = (TileIdx, Tile, Option<CityMemory>);

/// A snapshot of tile `t` as a major remembers it (`visibility.snapshot`,
/// `visibility.py:118-125`).
fn snapshot(g: &Game, t: TileIdx) -> Option<Snapshot> {
    let tile = *g.tile(t)?;
    let city =
        g.city_at(t).map(|c| CityMemory { name: c.name.clone(), pop: c.pop, owner: c.owner() });
    Some((t, tile, city))
}

/// The sources a sync looks at again.
#[derive(Default)]
struct Work {
    /// Units whose sight or place may have changed.
    units: BTreeSet<UnitId>,
    /// Units whose footprint read heights that changed.
    fresh: BTreeSet<UnitId>,
    cities: BTreeSet<CityId>,
    allies: bool,
    spies: BTreeSet<PlayerId>,
    /// Players to look at whole: every source, every contact and discovery.
    whole: BTreeSet<PlayerId>,
    /// Units that arrived or changed hands: those who see them meet their owners.
    arrived: BTreeSet<UnitId>,
    /// Tiles that changed hands: those who see them meet their owners.
    claimed: BTreeSet<TileIdx>,
    /// Tiles whose terrains changed: a natural wonder may have appeared.
    reshaped: BTreeSet<TileIdx>,
}

impl Game {
    /// Brings every dirty vision source up to date and queues what it reveals (DESIGN.md 6.9):
    /// the civilizations whose sight uniques changed have their units marked first; explored
    /// tiles and memories are written now, meetings and discoveries queued as effects. A no-op
    /// when nothing is dirty.
    pub(crate) fn sync_sight(&mut self) {
        self.check_sight_mods();
        if !self.pending.any_sight() {
            return;
        }
        let todo = self.pending.take_sight();
        // A path found before the sight caught up read the units it saw then; what it sees may
        // change here without a write, so the paths found at this revision are forgotten.
        self.dv.forget_paths();
        let mut work = Work::default();
        // The heights and the line-of-sight cache followed each terrain change as it happened
        // (`Game::changed`); what is left is to look again at what those tiles hold.
        for s in &todo {
            if let SightSource::Area(t) = *s {
                work.reshaped.insert(t);
            }
        }
        // Every new source is worked out from the game as it is, before any is registered: the
        // sources read the game's own sight, never a half-updated one.
        let plan = {
            let g: &Game = self;
            work.gather(g, &g.dv.vis, &todo);
            work.plan(g, &g.dv.vis)
        };
        let size = u32::try_from(self.st.tiles().len()).unwrap_or(u32::MAX);
        let mut tr = Vec::new();
        for (k, s) in plan {
            self.dv.vis.set(size, k, s, &mut tr);
        }
        let g: &Game = self;
        let vis = &g.dv.vis;
        let net = net(vis, tr);
        let mut found = Found::default();
        for x in net.iter().filter(|x| x.up) {
            found.seen(g, x.civ, x.tile);
        }
        for &u in &work.arrived {
            if let Some(unit) = g.unit(u) {
                for q in vis.seers(unit.tile()) {
                    found.meet(g, q, unit.owner());
                }
            }
        }
        for &t in &work.claimed {
            if let Some(o) = g.tile(t).and_then(Tile::owner) {
                for q in vis.seers(t) {
                    found.meet(g, q, o);
                }
            }
        }
        for &c in &work.cities {
            if let Some(city) = g.city(c) {
                for q in vis.seers(city.tile()) {
                    found.meet(g, q, city.owner());
                }
            }
        }
        for &p in &work.whole {
            found.everything_of(g, vis, p);
        }
        for &t in &work.reshaped {
            for q in vis.seers(t) {
                if undiscovered(g, q, t) {
                    found.discover.insert((q, t));
                }
            }
        }
        // What each civilization explores now, and what each major remembers of what it no
        // longer sees.
        let mut explore: BTreeMap<PlayerId, Vec<TileIdx>> = BTreeMap::new();
        let mut remember: BTreeMap<PlayerId, Vec<Snapshot>> = BTreeMap::new();
        for x in &net {
            let Some(pl) = g.player(x.civ) else { continue };
            if x.up {
                if !pl.explored.contains(x.tile.0) {
                    explore.entry(x.civ).or_default().push(x.tile);
                }
            } else if pl.is_major()
                && pl.alive()
                && let Some(snap) = snapshot(g, x.tile)
            {
                remember.entry(x.civ).or_default().push(snap);
            }
        }
        let effects: Vec<Effect> = found
            .meetings()
            .chain(found.discover.iter().map(|&(civ, tile)| Effect::Wonder { civ, tile }))
            .collect();
        self.dv.vis.note_seen(net.iter().filter(|x| x.up).map(|x| (x.civ, x.tile)));
        for (p, tiles) in explore {
            if let Some(pl) = self.player_mut(p, PlayerTouch::OTHER) {
                for t in tiles {
                    pl.explored.insert(t.0);
                }
            }
        }
        for (p, snaps) in remember {
            if let Some(m) =
                self.player_mut(p, PlayerTouch::OTHER).and_then(|x| x.major.as_deref_mut())
            {
                for (t, tile, city) in snaps {
                    m.memory.remember(t, &tile, city);
                }
            }
        }
        for e in effects {
            self.fx.push(e);
        }
    }

    /// Whether the registered sources are what a sync would register now: no source is marked,
    /// and no civilization's sight uniques can have changed since the last look.
    pub(crate) fn sight_current(&self) -> bool {
        !self.pending.any_sight()
            && (self.dv.vis.checked == self.dv.revs.now() || !self.dv.vis.sight_rules().civ_level())
    }

    /// Marks the units of every civilization whose sight uniques changed since the last look
    /// (`SightMods`, DESIGN.md 6.5): a civilization's index holds different ones, or a class
    /// their conditionals read moved. Nothing to do at a revision already looked at.
    fn check_sight_mods(&mut self) {
        let now = self.dv.revs.now();
        if self.dv.vis.checked == now {
            return;
        }
        let rules = *self.dv.vis.sight_rules();
        if !rules.civ_level() {
            self.dv.vis.checked = now;
            return;
        }
        let conditional = !rules.civ_deps.is_empty() || rules.anywhere;
        let mut stamps = Vec::new();
        let mut marked: Vec<UnitId> = Vec::new();
        for (p, pl) in self.st.players().iter() {
            if !pl.alive() || pl.is_barbarian() || self.st.units().of(p).is_empty() {
                continue;
            }
            let rev = civ_sight_rev(self, p);
            let (stamp, mods) = self.dv.vis.stamp(p);
            if rev <= stamp {
                continue;
            }
            let now_mods = if rules.civ_sources || rules.resource_sources {
                sight_mods(self, p)
            } else {
                Vec::new()
            };
            if conditional || now_mods.as_slice() != mods {
                marked.extend(self.st.units().of(p).iter().copied());
            }
            stamps.push((p, rev, now_mods));
        }
        for (p, rev, mods) in stamps {
            self.dv.vis.set_stamp(p, rev, mods);
        }
        for u in marked {
            self.pending.flag_sight(SightSource::Unit(u));
        }
        self.dv.vis.checked = self.dv.revs.now();
    }

    /// Rebuilds what every civilization sees from the state, with no effect: a loaded save is
    /// at a settle point, where everything sight reveals was already recorded (DESIGN.md 6.9).
    pub(crate) fn rebuild_sight(&mut self) {
        let vis = cold(self);
        self.dv.vis = vis;
    }

    /// Marks every player's sight for the next settle to build from nothing: what a game over a
    /// state built or converted by hand does first, so that everything it sees is explored and
    /// met as Python's first refresh did.
    pub(crate) fn sight_from_scratch(&mut self) {
        for p in self.st.players().ids() {
            self.pending.flag_sight(SightSource::Civ(p));
        }
    }

    /// The tiles that came into `p`'s sight since the last settle or the last take, in the order
    /// they did, forgetting them. A move reads them through [`step_seeing`](Self::step_seeing),
    /// which takes them on both sides of a step.
    pub(crate) fn take_newly_seen(&mut self, p: PlayerId) -> Vec<TileIdx> {
        self.dv.vis.take_newly_seen(p)
    }

    /// Runs one step of a move of `p`'s and returns what it did with the tiles it brought into
    /// `p`'s sight: what `move_toward` reads to stop when an enemy comes into view
    /// (`movement.py:627-640`, with [`super::enemy_spotted`]). Python took what it saw before the
    /// step after a refresh, stepped, refreshed and compared; so sight is settled and what came
    /// into view earlier (a border that grew in the same stage, a sync an event ran) is dropped
    /// before the step, and sight is settled again after it.
    #[allow(dead_code, reason = "move_toward steps through it from package 1c-02")]
    pub(crate) fn step_seeing<R>(
        &mut self,
        p: PlayerId,
        step: impl FnOnce(&mut Self) -> R,
    ) -> (R, Vec<TileIdx>) {
        self.settle_sight();
        drop(self.take_newly_seen(p));
        let r = step(self);
        self.settle_sight();
        (r, self.take_newly_seen(p))
    }

    /// Discovers a natural wonder (`visibility._discover_natural_wonders`,
    /// `visibility.py:171-198`): the first major to find it gets its `Grants [stats] to the
    /// first civilization to discover it`, and every one its own `[stats] for discovering a
    /// Natural Wonder (bonus enhanced to [stats] if first to discover it)`. Nothing happens if
    /// the tile has no wonder any more, or `p` is no living major or found it already.
    pub(crate) fn discover_wonder(&mut self, p: PlayerId, t: TileIdx) {
        if !undiscovered(self, p, t) {
            return;
        }
        let Some(w) = self.tile(t).and_then(Tile::wonder) else { return };
        let first = !self.majors(true).any(|q| q.id() != p && q.civ.natural_wonders.contains(w));
        let r = self.rules;
        let t_uniques = r.uniques();
        // In the order Python's dict gained them: the wonder's own grant, then the
        // civilization's bonuses.
        let mut gained: Vec<(Stat, f64)> = Vec::new();
        let mut add = |s: &Stats, times: u16| {
            for _ in 0..times {
                for (stat, v) in s.nonzero() {
                    match gained.iter_mut().find(|e| e.0 == stat) {
                        Some(e) => e.1 += v,
                        None => gained.push((stat, v)),
                    }
                }
            }
        };
        if first {
            for u in r.terrains()[w].uniques.ids() {
                if let UniqueData::GrantsStatsToFirstToDiscover(x) = t_uniques.get(u).data {
                    add(t_uniques.stats(x.stats), 1);
                }
            }
        }
        {
            let v = self.view();
            let ctx = Ctx::civ(p);
            for h in uq::civ(&v, p, UniqueType::StatBonusWhenDiscoveringNaturalWonder, &ctx) {
                if let UniqueData::StatBonusWhenDiscoveringNaturalWonder(x) = h.data() {
                    add(t_uniques.stats(if first { x.first } else { x.stats }), h.n);
                }
            }
        }
        // The natural wonders a civilization found count in its stats (`economy.py:636-641`),
        // with its stocks.
        if let Some(pl) = self.player_mut(p, PlayerTouch::STOCKS) {
            pl.civ.natural_wonders.insert(w);
        }
        for &(stat, v) in &gained {
            self.add_stat(p, stat, v);
        }
        let mut text = format!("We have discovered {}!", r.terrains()[w].name);
        if !gained.is_empty() {
            let parts: Vec<String> = gained
                .iter()
                // Python's `int(v)`: toward zero.
                .map(|&(s, v)| {
                    #[allow(clippy::cast_possible_truncation, reason = "int() of a stat bonus")]
                    let n = v as i64;
                    format!("+{n} {}", s.key())
                })
                .collect();
            text.push_str(&format!(" ({})", parts.join(", ")));
        }
        let audience = core::iter::once(p).collect();
        self.emit(
            EngineEvent::NaturalWonder,
            &text,
            Some(audience),
            Some(t),
            EventData::default(),
            &[],
        );
    }

    /// Adds to a civilization's stockpile of a stat (`Game.add_stat`, `game.py:636-650`): gold,
    /// culture and faith; happiness to its golden age points; science to its research.
    pub(crate) fn add_stat(&mut self, p: PlayerId, stat: Stat, amount: f64) {
        match stat {
            Stat::Science => {
                // research.add_science (research.py:206-219): progress toward the current
                // research, with its completion and overflow.
                pending(Porting::Pending("1b-07"));
            }
            Stat::Gold | Stat::Culture | Stat::Faith | Stat::Happiness => {
                if let Some(pl) = self.player_mut(p, PlayerTouch::STOCKS) {
                    let e = &mut pl.econ;
                    match stat {
                        Stat::Gold => e.gold += amount,
                        Stat::Culture => e.culture += amount,
                        Stat::Faith => e.faith += amount,
                        _ => e.golden_age_points += amount,
                    }
                }
            }
            Stat::Food | Stat::Production => {}
        }
    }

    /// Marks tiles explored for `p`, as a map trade, a ruin or an embassy does
    /// (`visibility.reveal_tiles`, `visibility.py:232-243`); a major remembers those it does not
    /// see as they look now. Returns how many were not explored before.
    #[allow(dead_code, reason = "map trades, ruins and embassies call it from 1b-08 and 1c-05")]
    pub(crate) fn reveal_tiles(&mut self, p: PlayerId, tiles: &[TileIdx]) -> u32 {
        self.sync_sight();
        let Some(pl) = self.player(p) else { return 0 };
        let major = pl.is_major();
        // A whole map is revealed at once (`triggers.py:197`): repeats are counted once.
        let mut fresh = crate::base::sets::BitSet::new();
        let mut snaps = Vec::new();
        for &t in tiles {
            if !self.grid().contains(t) {
                continue;
            }
            if !pl.explored.contains(t.0) {
                fresh.insert(t.0);
            }
            if major
                && !self.dv.vis.sees(p, t)
                && let Some(s) = snapshot(self, t)
            {
                snaps.push(s);
            }
        }
        let n = u32::try_from(fresh.len()).unwrap_or(u32::MAX);
        if fresh.is_empty() && snaps.is_empty() {
            return n;
        }
        if let Some(pl) = self.player_mut(p, PlayerTouch::OTHER) {
            pl.explored.union_with(&fresh);
            if let Some(m) = pl.major.as_deref_mut() {
                for (t, tile, city) in snaps {
                    m.memory.remember(t, &tile, city);
                }
            }
        }
        n
    }
}

impl Work {
    /// What the dirty marks name: the sources to look at again, and where contact or discovery
    /// must be checked.
    fn gather(&mut self, g: &Game, vis: &Visibility, todo: &[SightSource]) {
        for &s in todo {
            match s {
                SightSource::Unit(u) => {
                    self.units.insert(u);
                    self.arrived.insert(u);
                }
                SightSource::City(c) => {
                    self.cities.insert(c);
                    // What its allies see of it, if it is or was a city-state's.
                    let cs = g.city(c).is_none_or(|x| g.is_city_state(x.owner()));
                    if cs || vis.allied_views().any(|(_, x)| x == c) {
                        self.allies = true;
                    }
                    // The spies in it see through it, or no longer.
                    for p in g.majors(false) {
                        let spies = p.major.as_deref().map_or(&[][..], |m| m.spies.as_slice());
                        if spies.iter().any(|s| s.city == Some(c)) {
                            self.spies.insert(p.id());
                        }
                    }
                }
                SightSource::Allies(_) => self.allies = true,
                SightSource::Spies(p) => {
                    self.spies.insert(p);
                }
                SightSource::Civ(p) => {
                    self.whole.insert(p);
                    self.units.extend(g.st.units().of(p).iter().copied());
                    self.cities.extend(g.st.cities().of(p).iter().copied());
                    for (k, src) in vis.sources() {
                        if src.owner != p {
                            continue;
                        }
                        match k {
                            SourceKey::Unit(u) => {
                                self.units.insert(u);
                            }
                            SourceKey::City(c) => {
                                self.cities.insert(c);
                            }
                            SourceKey::AllyCity(..) | SourceKey::Spy(..) => {}
                        }
                    }
                    self.spies.insert(p);
                    self.allies = true;
                }
                SightSource::Contact(p) => {
                    self.whole.insert(p);
                }
                SightSource::Tile(t) => {
                    // Its owner is met by those who see it; the units on it see anew, since
                    // whether a city stands there decides whether they are embarked, which a
                    // sight unique's conditional may read, as the tile's other inputs may.
                    self.claimed.insert(t);
                    self.units.extend(g.units_at(t).map(crate::state::units::Unit::id));
                }
                SightSource::Area(_) => {}
            }
        }
        // A unit that walks the heights near a tile whose heights changed sees anew, and a unit
        // on it may see further or less far.
        if !self.reshaped.is_empty() {
            let grid = g.grid();
            for (k, src) in vis.sources() {
                let SourceKey::Unit(u) = k else { continue };
                let reach = src.sight.map_or(0, super::visibility::Sight::reach);
                let near = |t: TileIdx| {
                    src.at == t
                        || (matches!(src.sight, Some(super::visibility::Sight::Walk(_)))
                            && grid.distance(src.at, t) <= reach)
                };
                if self.reshaped.iter().any(|&t| near(t)) {
                    self.fresh.insert(u);
                }
            }
        }
    }

    /// Each source's new value, or `None` to drop it.
    fn plan(&self, g: &Game, vis: &Visibility) -> Vec<(SourceKey, Option<VisSource>)> {
        let mut plan = Vec::new();
        for &u in self.units.union(&self.fresh) {
            plan.push((SourceKey::Unit(u), unit_source(g, vis, u, !self.fresh.contains(&u))));
        }
        for &c in &self.cities {
            plan.push((SourceKey::City(c), city_source(g, c)));
        }
        if self.allies {
            let want = ally_sources(g);
            for (v, c) in vis.allied_views() {
                let k = SourceKey::AllyCity(v, c);
                if want.binary_search_by_key(&k, |e| e.0).is_err() {
                    plan.push((k, None));
                }
            }
            plan.extend(want.into_iter().map(|(k, s)| (k, Some(s))));
        }
        for &p in &self.spies {
            let want = spy_sources(g, p);
            let lo = SourceKey::Spy(p, 0);
            let hi = SourceKey::Spy(p, u8::MAX);
            for (k, _) in vis.sources_in(lo..=hi) {
                if !want.iter().any(|e| e.0 == k) {
                    plan.push((k, None));
                }
            }
            plan.extend(want.into_iter().map(|(k, s)| (k, Some(s))));
        }
        plan
    }
}

/// The transitions of one sync, netted: a tile that went dark and came back (or the other way)
/// is no transition. Sorted by tile, then civilization.
fn net(vis: &Visibility, mut tr: Vec<Transition>) -> Vec<Transition> {
    // A stable sort keeps each tile's transitions in the order they happened: the first says
    // whether the tile was seen before.
    tr.sort_by_key(|x| (x.civ, x.tile));
    let mut out: Vec<Transition> = Vec::new();
    let mut i = 0;
    while i < tr.len() {
        let first = tr[i];
        while i < tr.len() && tr[i].civ == first.civ && tr[i].tile == first.tile {
            i += 1;
        }
        let before = !first.up;
        let now = vis.sees(first.civ, first.tile);
        if now != before {
            out.push(Transition { up: now, ..first });
        }
    }
    out.sort_by_key(|x| (x.tile, x.civ));
    out
}

/// What every civilization sees, rebuilt from the state with no effect: every source registered
/// afresh (DESIGN.md 6.9, "On load, the counts are rebuilt from scratch").
pub(crate) fn cold(g: &Game) -> Visibility {
    let mut vis = Visibility::new(g.rules, &g.st);
    let size = u32::try_from(g.st.tiles().len()).unwrap_or(u32::MAX);
    // What each civilization gives its units' sight first: their sight reads it.
    for (p, pl) in g.st.players().iter() {
        if pl.alive() && !pl.is_barbarian() && !g.st.units().of(p).is_empty() {
            let mods = if vis.sight_rules().civ_sources || vis.sight_rules().resource_sources {
                sight_mods(g, p)
            } else {
                Vec::new()
            };
            vis.set_stamp(p, civ_sight_rev(g, p), mods);
        }
    }
    let mut tr = Vec::new();
    let mut plan: Vec<(SourceKey, VisSource)> = Vec::new();
    for u in g.st.units().iter() {
        if let Some(s) = unit_source(g, &vis, u.id(), false) {
            plan.push((SourceKey::Unit(u.id()), s));
        }
    }
    for c in g.st.cities().iter() {
        if let Some(s) = city_source(g, c.id()) {
            plan.push((SourceKey::City(c.id()), s));
        }
    }
    plan.extend(ally_sources(g));
    for p in g.st.players().ids() {
        plan.extend(spy_sources(g, p));
    }
    for (k, s) in plan {
        vis.set(size, k, Some(s), &mut tr);
        tr.clear();
    }
    vis.checked = g.dv.revs.now();
    vis
}

/// The cache oracle for sight (DESIGN.md 9.4): the counts and sources against a rebuild from the
/// state, and the met sets and discoveries against Python's rule over what each civilization
/// sees. One line for each difference.
#[must_use]
pub fn verify(g: &Game) -> Vec<String> {
    let rebuilt = cold(g);
    let mut out = g.dv.vis.differences(&rebuilt);
    out.extend(g.dv.vis.stale_line_of_sight(g.grid()));
    // The sight a step reads from the kept sight uniques is the one the index gives.
    for (k, s) in rebuilt.sources() {
        if let SourceKey::Unit(u) = k
            && s.sight != super::sight_of(g, u)
        {
            let direct = super::sight_of(g, u);
            out.push(format!("unit {}: its sight {:?} is not {direct:?}", u.get(), s.sight));
        }
    }
    for (v, _) in g.st.players().iter() {
        if !has_sight(g, v) {
            continue;
        }
        let Some(seen) = rebuilt.visible(v) else { continue };
        for t in seen.iter().map(TileIdx) {
            for q in owners_on(g, t) {
                if may_meet(g, v, q) {
                    out.push(format!(
                        "player {} sees player {} on tile {} and has not met them",
                        v.0, q.0, t.0
                    ));
                }
            }
            if undiscovered(g, v, t) {
                out.push(format!(
                    "player {} sees a natural wonder on tile {} it has not discovered",
                    v.0, t.0
                ));
            }
        }
    }
    out
}

/// What a test or benchmark does that no rule may.
#[cfg(feature = "test-ops")]
impl Game {
    /// Moves a unit to `to` without movement rules and brings sight up to date with what it
    /// reveals, as one step of a move does (`movement.step`, then `visibility.refresh`): what
    /// the `vis_step` benchmark times. Returns the tiles that came into its owner's sight.
    pub fn step_unit_for_test(
        &mut self,
        u: UnitId,
        to: TileIdx,
    ) -> Result<Vec<TileIdx>, crate::state::StateError> {
        let Some(owner) = self.unit(u).map(crate::state::units::Unit::owner) else {
            return Err(crate::state::units::UnitsError::NoSuchUnit(u).into());
        };
        let (moved, seen) = self.step_seeing(owner, |g| g.relocate_unit(u, to));
        moved.map(|()| seen)
    }
}
