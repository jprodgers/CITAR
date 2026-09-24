//! Searches over steps (DESIGN.md 6.10): the best path to a tile (`movement.find_path`,
//! `movement.py:420-468`), the turns a path takes (`path_turns`, `movement.py:471-482`) and
//! what a unit can reach this turn (`reachable_this_turn`, `movement.py:485-509`).
//!
//! **Labels.** Python's search ordered states by `(turns, -moves_left)`: fewer turns first, then
//! more movement left. A [`Label`] is that pair, and [`Label::key`] packs it into one `u64`,
//! `turns << 32 | (u32::MAX - left)`, which orders the same way and cannot underflow however
//! far a unit's moves exceed its full moves (the earlier `t·(F+1) + (F−l)` could). A step keeps
//! Python's overdraw rule: with movement left it costs at most what is left; with none, a new
//! turn begins with full moves first.
//!
//! **The search** is A* over those keys. Its bound (`Heur`) plays the steps a path to the target
//! must still take at the least each kind may cost ([`Floors`]): a step can be a road step only
//! between two tiles with routes and a rail step only between two railroads, so from a tile `r`
//! steps from the nearest route the first `r` steps cost at least the terrain's floor, and the
//! same at the target's end; the rest, up to the hex distance, cost at least a road or rail step
//! ([`super::RouteNet`] holds those distances). Each of its three readings (no route used,
//! roads, railroads) grows along every step (each is consistent), and so does the least of them,
//! which is the bound. So each tile is expanded once, with its final label, and stale heap
//! entries are skipped; the heap orders equal bounds by label, since the bound saturates (a step
//! that overdraws takes 10 left or 30 left to the same place), and the tile that would give a
//! neighbour its best label must come out before it. So its labels are exactly those of Python's
//! Dijkstra, which kept one label per tile and grew it from the best label of its neighbours;
//! that is not always the truly best arrival, since a unit may not end a turn stacked on its own
//! kind and a worse label that still has moves could step past where the best one may not stop,
//! but it is the answer Python gave.
//!
//! **The path** is Python's too. Among the tiles whose label steps to a tile's label, Python kept
//! the first it expanded, in `(key, tile)` order. Each such tile has a smaller label and no
//! greater bound than the tile, so it is out of the heap before the target is; the walk back
//! chooses the least `(key, tile)` among them. Two routes with the same label but different step
//! costs (a hill or a meadow, when the step takes the last move either way) are therefore told
//! apart as Python told them apart.

use core::cell::RefCell;

use super::ALL;
use super::class::Mover;
use super::cost::Facts;
use super::node::stack_reason;
use crate::base::ids::TileIdx;

/// Where a search stands on a tile: whole turns spent, and movement left in the turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Label {
    pub turns: u32,
    pub left: i32,
}

impl Label {
    /// The search's order in one number: fewer turns first, then more movement left.
    #[must_use]
    #[inline]
    pub fn key(self) -> u64 {
        let left = u32::try_from(self.left.max(0)).unwrap_or(0);
        (u64::from(self.turns) << 32) | u64::from(u32::MAX - left)
    }

    /// The label a key packs.
    #[must_use]
    #[inline]
    pub fn from_key(k: u64) -> Self {
        let low = u32::try_from(k & 0xFFFF_FFFF).unwrap_or(u32::MAX);
        let left = i32::try_from(u32::MAX - low).unwrap_or(i32::MAX);
        Self { turns: u32::try_from(k >> 32).unwrap_or(u32::MAX), left }
    }

    /// One step costing `cost`, for a unit whose full moves are `full` (`movement.py:447-452`):
    /// with nothing left a new turn starts first; a step takes what it costs, or all that is
    /// left.
    #[must_use]
    #[inline]
    pub fn step(self, cost: i32, full: i32) -> Self {
        let (turns, l) = if self.left <= 0 {
            (self.turns.saturating_add(1), full)
        } else {
            (self.turns, self.left)
        };
        Self { turns, left: if cost >= l { 0 } else { l - cost } }
    }
}

/// Where a search stands, as the bound works it out: whole turns spent, and movement left.
type Pos = (u64, u32);

/// Steps of one cost, for a unit of given full moves: where `k` of them take a search, as `k`
/// [`Label::step`]s would, worked out at once (the steps a whole turn holds once per search).
#[derive(Clone, Copy, Debug)]
struct Bound {
    c: u32,
    full: u32,
    per_turn: u32,
}

impl Bound {
    fn new(c: i32, full: i32) -> Self {
        let c = u32::try_from(c.max(0)).unwrap_or(0);
        let full = u32::try_from(full.max(1)).unwrap_or(1);
        Self { c, full, per_turn: if c == 0 { 0 } else { full.div_ceil(c) } }
    }

    /// Where `k` steps costing `c` each take a search standing at `p`.
    #[inline]
    fn run(&self, p: Pos, k: u32) -> Pos {
        if k == 0 {
            return p;
        }
        let (mut t, mut left) = p;
        if left == 0 {
            t += 1;
            left = self.full;
        }
        let c = self.c;
        if c == 0 {
            return (t, left);
        }
        if (k - 1).saturating_mul(c) < left {
            // Every step fits in this turn's movement, the last perhaps overdrawing.
            return (t, left.saturating_sub(k.saturating_mul(c)));
        }
        let rest = k - left.div_ceil(c);
        let turns = rest.div_ceil(self.per_turn);
        let last = rest - (turns - 1) * self.per_turn;
        (t + u64::from(turns), self.full.saturating_sub(last.saturating_mul(c)))
    }
}

/// The least a step may cost, by where it runs (the search's bound): off the routes, between two
/// tiles with routes, and between two railroads. Each is at most the one before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Floors {
    pub off: i32,
    pub road: i32,
    pub rail: i32,
}

/// The search's bound toward one target (see the module doc).
pub(crate) struct Heur<'n> {
    off: Bound,
    road: Bound,
    rail: Bound,
    /// How far each tile is from the routes, and the target's distances to them.
    net: Option<(&'n super::RouteNet, u32, u32)>,
    /// Its civilization's forests and jungles in its own land are roads to it: every tile may
    /// be one.
    forest: bool,
}

impl<'n> Heur<'n> {
    /// No bound at all: a search with no target.
    fn none(full: i32) -> Self {
        let b = Bound::new(0, full);
        Self { off: b, road: b, rail: b, net: None, forest: false }
    }

    pub(crate) fn new(
        f: Floors,
        full: i32,
        net: &'n super::RouteNet,
        target: TileIdx,
        forest: bool,
    ) -> Self {
        let i = target.0 as usize;
        let rt = if forest { 0 } else { net.road.get(i).map_or(0, |&r| u32::from(r)) };
        let at = net.rail.get(i).map_or(0, |&r| u32::from(r));
        Self {
            off: Bound::new(f.off, full),
            road: Bound::new(f.road, full),
            rail: Bound::new(f.rail, full),
            net: Some((net, rt, at)),
            forest,
        }
    }

    /// The best key a search standing at label `l` on tile `v`, `d` tiles from the target, can
    /// reach it with: the cheapest steps of each kind in the order a path must take them.
    #[inline]
    pub(crate) fn key(&self, l: Label, v: TileIdx, d: u32) -> u64 {
        if d == 0 {
            return l.key();
        }
        let p: Pos = (u64::from(l.turns), u32::try_from(l.left.max(0)).unwrap_or(0));
        let (t, left) = match self.net {
            Some((net, rt, at)) => {
                let i = v.0 as usize;
                let rv = if self.forest { 0 } else { net.road.get(i).map_or(0, |&r| u32::from(r)) };
                let av = net.rail.get(i).map_or(0, |&r| u32::from(r));
                if av + at < d {
                    // Off the routes to the nearest, along roads to the nearest railroad, the
                    // rest by rail, and the same at the far end.
                    let p = self.off.run(p, rv);
                    let p = self.road.run(p, av - rv);
                    let p = self.rail.run(p, d - av - at);
                    let p = self.road.run(p, at - rt);
                    self.off.run(p, rt)
                } else if rv + rt < d {
                    let p = self.off.run(p, rv);
                    let p = self.road.run(p, d - rv - rt);
                    self.off.run(p, rt)
                } else {
                    self.off.run(p, d)
                }
            }
            None => self.off.run(p, d),
        };
        (t << 32) | u64::from(u32::MAX - left)
    }
}

// ---- Scratch --------------------------------------------------------------------------------------

/// An entry of the open set: its priority (the bound's key), then the label the tile had when
/// it was pushed, then the tile. Those three order entries totally (two equal entries are the
/// same tile with the same label), so no push sequence is needed to make the pop order a function
/// of the pushes, as `MinHeap` needs one (DESIGN.md 6.10).
///
/// The label comes before the tile for a reason: the bound saturates (a step that overdraws
/// takes 10 left or 30 left to the same place), so a tile and the neighbour that would give it a
/// better label can have the same priority, and the neighbour, with the smaller label, must be
/// expanded first for the tile to be closed with its best label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    prio: u64,
    label: u64,
    tile: u32,
}

/// The open set: a four-ary min-heap of [`Entry`]s (shallower than a binary one, for pops).
#[derive(Debug, Default)]
struct Open(Vec<Entry>);

impl Open {
    #[inline]
    fn entry(prio: u64, label: u64, t: TileIdx) -> Entry {
        Entry { prio, label, tile: t.0 }
    }

    /// The priority and tile of an entry.
    #[inline]
    fn parts(e: Entry) -> (u64, TileIdx) {
        (e.prio, TileIdx(e.tile))
    }

    fn clear(&mut self) {
        self.0.clear();
    }

    #[inline]
    fn push(&mut self, e: Entry) {
        let h = &mut self.0;
        let mut i = h.len();
        h.push(e);
        while i > 0 {
            let p = (i - 1) / 4;
            if h[p] <= e {
                break;
            }
            h[i] = h[p];
            i = p;
        }
        h[i] = e;
    }

    #[inline]
    fn pop(&mut self) -> Option<Entry> {
        let h = &mut self.0;
        let last = h.pop()?;
        let Some(&top) = h.first() else { return Some(last) };
        let n = h.len();
        let mut i = 0;
        loop {
            let first = 4 * i + 1;
            if first >= n {
                break;
            }
            let mut c = first;
            for j in first + 1..(first + 4).min(n) {
                if h[j] < h[c] {
                    c = j;
                }
            }
            if h[c] >= last {
                break;
            }
            h[i] = h[c];
            i = c;
        }
        h[i] = last;
        Some(top)
    }
}

/// What a search holds of one tile, together, so that looking at a neighbour touches one cache
/// line: its label, whether it is closed, and what the search found of the tile.
#[derive(Clone, Copy, Debug, Default)]
struct Cell {
    /// The search that last wrote the cell; for any other the cell is empty.
    generation: u32,
    /// [`LABELLED`], [`CLOSED`] and [`SEEN`].
    flags: u8,
    /// Whether the mover may route through the tile on its way elsewhere.
    pass: bool,
    /// The hex distance to the search's target, 0 with none.
    dist: u16,
    key: u64,
    facts: Facts,
}

/// The tile has a label.
const LABELLED: u8 = 1;
/// The tile is expanded, its label final.
const CLOSED: u8 = 2;
/// The search has looked at the tile: `pass`, `dist` and `facts` hold.
const SEEN: u8 = 4;

/// The per-tile cells a search works in, reused from search to search: a generation stamp
/// empties them without clearing (DESIGN.md 6.10).
#[derive(Debug, Default)]
pub struct PathScratch {
    generation: u32,
    cells: Vec<Cell>,
    heap: Open,
    target: Option<TileIdx>,
    /// The tiles closed so far, in the order they were: what a tree reads back, without a walk
    /// over the map.
    closed: Vec<TileIdx>,
}

impl Clone for PathScratch {
    /// A clone starts empty: scratch holds nothing between searches.
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PathScratch {
    fn begin(&mut self, size: usize, target: Option<TileIdx>) {
        self.target = target;
        if self.cells.len() != size {
            self.cells = vec![Cell::default(); size];
            self.generation = 0;
        }
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.cells.fill(Cell::default());
            self.generation = 1;
        }
        self.heap.clear();
        self.closed.clear();
    }

    /// The cell of tile `t`, emptied first if an earlier search wrote it.
    #[inline]
    fn cell(&mut self, t: TileIdx) -> &mut Cell {
        let generation = self.generation;
        let c = &mut self.cells[t.0 as usize];
        if c.generation != generation {
            c.generation = generation;
            c.flags = 0;
        }
        c
    }

    /// The cell of tile `t` if this search wrote it.
    #[inline]
    fn this(&self, t: TileIdx) -> Option<&Cell> {
        self.cells.get(t.0 as usize).filter(|c| c.generation == self.generation)
    }

    #[inline]
    fn label(&self, t: TileIdx) -> Option<u64> {
        self.this(t).filter(|c| c.flags & LABELLED != 0).map(|c| c.key)
    }

    #[inline]
    fn set(&mut self, t: TileIdx, k: u64) {
        let c = self.cell(t);
        c.flags |= LABELLED;
        c.key = k;
    }

    #[inline]
    fn is_closed(&self, t: TileIdx) -> bool {
        self.this(t).is_some_and(|c| c.flags & CLOSED != 0)
    }

    #[inline]
    fn close(&mut self, t: TileIdx) {
        self.cell(t).flags |= CLOSED;
        self.closed.push(t);
    }

    /// Whether mover `m` may route through tile `t` on its way elsewhere, and the facts of `t`
    /// its steps read: worked out on the first look, then remembered for the search.
    #[inline]
    fn look(&mut self, m: &Mover<'_>, t: TileIdx) -> (bool, Facts) {
        let target = self.target;
        let c = self.cell(t);
        if c.flags & SEEN == 0 {
            let (pass, facts) = m.look(t).unwrap_or_default();
            c.pass = pass;
            c.facts = facts;
            c.dist = target
                .map_or(0, |x| u16::try_from(m.game().grid().distance(t, x)).unwrap_or(u16::MAX));
            c.flags |= SEEN;
        }
        (c.pass, c.facts)
    }

    /// The distance of tile `t` to the target, once [`look`](Self::look) has seen it.
    #[inline]
    fn dist(&self, t: TileIdx) -> u32 {
        self.this(t).map_or(0, |c| u32::from(c.dist))
    }
}

/// What a path search is asked: the unit where it stands with the moves it has, the target and
/// the turn limit. Two asks with the same key at the same revision have the same answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PathKey {
    pub unit: crate::base::ids::UnitId,
    pub from: TileIdx,
    pub moves: i32,
    pub target: TileIdx,
    pub max_turns: u32,
}

/// Paths found at one revision (DESIGN.md 6.10): a caller that asks for a path and then moves
/// along it, or asks again for its turns, finds it here. Any write moves the revision and empties
/// it, so an answer is never stale; it holds at most [`PathCache::CAP`] answers.
#[derive(Debug, Default)]
pub struct PathCache {
    rev: u64,
    found: Vec<(PathKey, Option<Vec<TileIdx>>)>,
}

impl Clone for PathCache {
    /// A clone starts empty: its game moves on at its own revisions.
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PathCache {
    /// The most answers kept at one revision.
    pub const CAP: usize = 64;

    /// The answer to `key` found at revision `rev`, if one was.
    #[must_use]
    pub fn get(&self, rev: u64, key: &PathKey) -> Option<Option<Vec<TileIdx>>> {
        if self.rev != rev {
            return None;
        }
        self.found.iter().find(|(k, _)| k == key).map(|(_, p)| p.clone())
    }

    /// Keeps the answer to `key` at revision `rev`.
    pub fn put(&mut self, rev: u64, key: PathKey, path: Option<Vec<TileIdx>>) {
        if self.rev != rev {
            self.rev = rev;
            self.found.clear();
        }
        if self.found.len() >= Self::CAP {
            self.found.remove(0);
        }
        self.found.push((key, path));
    }
}

/// Runs `f` with the game's search scratch, or a fresh one if a search is already running.
pub(crate) fn with_scratch<R>(
    cell: &RefCell<PathScratch>,
    f: impl FnOnce(&mut PathScratch) -> R,
) -> R {
    match cell.try_borrow_mut() {
        Ok(mut s) => f(&mut s),
        Err(_) => f(&mut PathScratch::default()),
    }
}

// ---- Searches -------------------------------------------------------------------------------------

/// What a search starts from.
#[derive(Clone, Copy, Debug)]
pub struct Start {
    pub tile: TileIdx,
    /// Movement left now.
    pub moves: i32,
    /// Full moves, what a new turn gives.
    pub full: i32,
}

impl Mover<'_> {
    /// Where the unit starts a search from: its tile, its movement left and its full moves.
    /// `None` for a type with no unit.
    #[must_use]
    pub fn start(&self) -> Option<Start> {
        let u = self.g.unit(self.unit?)?;
        Some(Start {
            tile: u.tile(),
            moves: u.moves,
            full: crate::game::movement::max_moves(self.g, u.id()),
        })
    }

    /// The least a step of the unit may cost off the routes, along roads and by rail (the
    /// search's bound): nothing costs less.
    #[must_use]
    pub fn floors(&self) -> Floors {
        let sc = self.rules.scale;
        let mut off = if self.prof.all_1 || self.prof.ignores_terrain {
            sc
        } else {
            let t = self.rules.terrain_floor.saturating_mul(sc);
            if self.prof.doubles.is_empty() { t } else { t / 2 }
        };
        // Embarking or disembarking comes before everything else a step checks.
        if self.def.domain == crate::rules::defs::Domain::Land && !self.prof.on_water {
            off = off.min(self.prof.embark.unwrap_or(ALL)).min(self.prof.disembark.unwrap_or(ALL));
        }
        let off = off.max(0);
        if self.prof.all_1 {
            // Every step costs one move, routes or not.
            return Floors { off, road: off, rail: off };
        }
        let road = off.min(if self.civ.road_speed { sc / 3 } else { sc / 2 });
        Floors { off, road, rail: road.min(sc / 10) }
    }

    /// The best path from `s` to `target`, both ends included, within `max_turns` turns
    /// (`movement.find_path`); `None` if there is none. Python's answer: see the module doc.
    #[must_use]
    pub fn find_path_from(
        &self,
        s: Start,
        target: TileIdx,
        max_turns: u32,
    ) -> Option<Vec<TileIdx>> {
        let g = self.g;
        if !g.grid().contains(target) || !g.grid().contains(s.tile) {
            return None;
        }
        if target == s.tile {
            return Some(vec![s.tile]);
        }
        if self.is_air() {
            return None;
        }
        let known =
            self.barbarian || g.player(self.pid).is_some_and(|p| p.explored.contains(target.0));
        if known && self.terrain_reason(target).is_some() {
            return None;
        }
        let net = g.derived().route_net(g);
        let heur = Heur::new(self.floors(), s.full, &net, target, self.civ.forest_roads);
        with_scratch(g.derived().path_scratch(), |sc| {
            let end = self.search(sc, s, Some(target), max_turns, &heur)?;
            Some(self.walk_back(sc, s, target, end, max_turns))
        })
    }

    /// The search itself: labels every tile it expands, and returns the target's key.
    fn search(
        &self,
        sc: &mut PathScratch,
        s: Start,
        target: Option<TileIdx>,
        max_turns: u32,
        heur: &Heur<'_>,
    ) -> Option<u64> {
        let g = self.g;
        let grid = g.grid();
        sc.begin(g.state().map().size() as usize, target);
        let first = Label { turns: 0, left: s.moves.max(0) };
        sc.set(s.tile, first.key());
        let _ = sc.look(self, s.tile);
        let d0 = sc.dist(s.tile);
        sc.heap.push(Open::entry(heur.key(first, s.tile, d0), first.key(), s.tile));
        let mut found: Option<u64> = None;
        // Once the target is out, what may still come is an entry with its priority and label
        // (a free step, if a ruleset has one), which may be a tile the walk back chooses.
        let mut stop_at: Option<(u64, u64)> = None;
        while let Some(e) = sc.heap.pop() {
            let (p, v) = Open::parts(e);
            if stop_at.is_some_and(|c| (p, e.label) > c) {
                break;
            }
            if sc.is_closed(v) {
                continue;
            }
            sc.close(v);
            let Some(k) = sc.label(v) else { continue };
            if Some(v) == target {
                found = Some(k);
                stop_at = Some((p, k));
                continue;
            }
            let here = Label::from_key(k);
            if here.turns > max_turns {
                continue;
            }
            let (_, fv) = sc.look(self, v);
            for (d, &n) in grid.neighbor_table(v).iter().enumerate() {
                if n == crate::base::hex::NO_TILE {
                    continue;
                }
                let nb = TileIdx(n);
                if sc.is_closed(nb) {
                    continue;
                }
                let is_target = Some(nb) == target;
                let (pass, fnb) = sc.look(self, nb);
                // The target alone may hold a civilian it would capture.
                if !(if is_target { self.passable(nb, target) } else { pass }) {
                    continue;
                }
                let next = here.step(self.cost_from(v, d, &fv, &fnb, true), s.full);
                let nk = next.key();
                if sc.label(nb).is_some_and(|old| old <= nk) {
                    continue;
                }
                if !is_target {
                    // Python never expanded a label past the turn limit.
                    if next.turns > max_turns {
                        continue;
                    }
                    // It may pass through its own units, but not end its turn on one of its kind.
                    if next.left == 0
                        && self.own_unit_at(nb)
                        && stack_reason(g, self.pid, self.base, nb, self.ignore).is_some()
                    {
                        continue;
                    }
                }
                let dn = sc.dist(nb);
                let np = heur.key(next, nb, dn);
                if target.is_some() && (np >> 32) > u64::from(max_turns) + 1 {
                    continue;
                }
                sc.set(nb, nk);
                sc.heap.push(Open::entry(np, nk, nb));
            }
        }
        found
    }

    /// The path to `target`, walked back from its label `end` choosing, at each tile, the
    /// neighbour Python would have expanded first among those whose label steps to it.
    fn walk_back(
        &self,
        sc: &mut PathScratch,
        s: Start,
        target: TileIdx,
        end: u64,
        max_turns: u32,
    ) -> Vec<TileIdx> {
        let g = self.g;
        let grid = g.grid();
        let mut path = vec![target];
        let (mut v, mut vk) = (target, end);
        // Each step back goes to a strictly earlier (key, tile), so this ends; the bound is a
        // guard against a bug.
        let mut guard = g.state().map().size();
        while v != s.tile && guard > 0 {
            guard -= 1;
            let mut best: Option<(u64, TileIdx)> = None;
            let (_, fv) = sc.look(self, v);
            for u in grid.neighbors(v) {
                if u == target || !sc.is_closed(u) {
                    continue;
                }
                let Some(uk) = sc.label(u) else { continue };
                let lu = Label::from_key(uk);
                if lu.turns > max_turns || (uk, u.0) >= (vk, v.0) {
                    continue;
                }
                let Some(d) = grid.neighbor_table(u).iter().position(|&n| n == v.0) else {
                    continue;
                };
                let (_, fu) = sc.look(self, u);
                if lu.step(self.cost_from(u, d, &fu, &fv, true), s.full).key() != vk {
                    continue;
                }
                if best.is_none_or(|(bk, bt)| (uk, u.0) < (bk, bt.0)) {
                    best = Some((uk, u));
                }
            }
            let Some((uk, u)) = best else { break };
            path.push(u);
            v = u;
            vk = uk;
        }
        path.reverse();
        path
    }

    /// Every tile the unit can reach this turn, with the movement it would have left there, by
    /// tile (`movement.reachable_this_turn`): only tiles it has explored (the barbarians know every tile, as Python gave them all), and only those it may
    /// end its move on. Its own tile is not listed.
    #[must_use]
    pub fn reachable_from(&self, s: Start) -> Vec<(TileIdx, i32)> {
        let g = self.g;
        if self.is_air() || s.moves <= 0 {
            return Vec::new();
        }
        let grid = g.grid();
        let Some(explored) = g.player(self.pid).map(|p| &p.explored) else { return Vec::new() };
        with_scratch(g.derived().path_scratch(), |sc| {
            sc.begin(g.state().map().size() as usize, None);
            // Keys here are movement left alone: more is better, so the heap pops the most.
            let key = |l: i32| u64::from(u32::MAX - u32::try_from(l.max(0)).unwrap_or(0));
            sc.set(s.tile, key(s.moves));
            sc.heap.push(Open::entry(key(s.moves), 0, s.tile));
            let mut out = Vec::new();
            while let Some(e) = sc.heap.pop() {
                let (k, cur) = Open::parts(e);
                if sc.label(cur) != Some(k) || sc.is_closed(cur) {
                    continue;
                }
                sc.close(cur);
                let left =
                    i32::try_from(u32::MAX - u32::try_from(k).unwrap_or(u32::MAX)).unwrap_or(0);
                if cur != s.tile {
                    out.push((cur, left));
                }
                if left <= 0 {
                    continue;
                }
                let (_, fc) = sc.look(self, cur);
                for (d, &n) in grid.neighbor_table(cur).iter().enumerate() {
                    if n == crate::base::hex::NO_TILE {
                        continue;
                    }
                    let nb = TileIdx(n);
                    if !(self.barbarian || explored.contains(n)) {
                        continue;
                    }
                    let (pass, fnb) = sc.look(self, nb);
                    if !pass {
                        continue;
                    }
                    let cost = self.cost_from(cur, d, &fc, &fnb, true);
                    let l2 = if cost >= left { 0 } else { left - cost };
                    let nk = key(l2);
                    if sc.label(nb).is_some_and(|old| old <= nk) {
                        continue;
                    }
                    sc.set(nb, nk);
                    sc.heap.push(Open::entry(nk, 0, nb));
                }
            }
            out.retain(|&(t, _)| stack_reason(g, self.pid, self.base, t, self.ignore).is_none());
            out.sort_by_key(|&(t, _)| t);
            out
        })
    }

    /// How many turns a path takes from `s` (`movement.path_turns`): 1 for a path this turn.
    #[must_use]
    pub fn path_turns_from(&self, s: Start, path: &[TileIdx]) -> u32 {
        let mut turns = 1u32;
        let mut left = s.moves;
        for w in path.windows(2) {
            if left <= 0 {
                turns = turns.saturating_add(1);
                left = s.full;
            }
            let cost = self.edge_cost(w[0], w[1]);
            left = if cost >= left { 0 } else { left - cost };
        }
        turns
    }

    /// The best path from the unit to `target` within `max_turns` turns; `None` for no path, an
    /// aircraft, or a type with no unit.
    #[must_use]
    pub fn find_path(&self, target: TileIdx, max_turns: u32) -> Option<Vec<TileIdx>> {
        self.find_path_from(self.start()?, target, max_turns)
    }

    /// The turns a path takes the unit from where it stands with the moves it has.
    #[must_use]
    pub fn path_turns(&self, path: &[TileIdx]) -> u32 {
        self.start().map_or(1, |s| self.path_turns_from(s, path))
    }

    /// What the unit can reach this turn.
    #[must_use]
    pub fn reachable(&self) -> Vec<(TileIdx, i32)> {
        self.start().map_or_else(Vec::new, |s| self.reachable_from(s))
    }

    /// Runs a bounded search from `s` with no target, labelling every tile within `max_turns`
    /// turns, for a [`super::tree::PathTree`]; returns the labels by tile.
    pub(crate) fn labels_within(&self, s: Start, max_turns: u32) -> Vec<(TileIdx, u64)> {
        let g = self.g;
        with_scratch(g.derived().path_scratch(), |sc| {
            let _found = self.search(sc, s, None, max_turns, &Heur::none(s.full));
            sc.closed.iter().filter_map(|&t| Some((t, sc.label(t)?))).collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_order_as_turns_then_movement_left() {
        let a = Label { turns: 0, left: 120 };
        let b = Label { turns: 0, left: 60 };
        let c = Label { turns: 1, left: 180 };
        assert!(a.key() < b.key() && b.key() < c.key());
        for l in [a, b, c, Label { turns: 3, left: 0 }] {
            assert_eq!(Label::from_key(l.key()), l);
        }
        // Moves above the full moves order and round-trip too.
        let big = Label { turns: 0, left: 10_000 };
        assert!(big.key() < a.key());
        assert_eq!(Label::from_key(big.key()), big);
    }

    #[test]
    fn a_step_overdraws_then_starts_a_new_turn() {
        let full = 120;
        let l = Label { turns: 0, left: 60 };
        assert_eq!(l.step(120, full), Label { turns: 0, left: 0 });
        assert_eq!(Label { turns: 0, left: 0 }.step(60, full), Label { turns: 1, left: 60 });
        assert_eq!(Label { turns: 2, left: 30 }.step(10, full), Label { turns: 2, left: 20 });
    }

    #[test]
    fn the_bound_is_that_many_cheapest_steps() {
        for full in [60, 120, 180, 200] {
            for c in [0, 6, 20, 30, 60, 120] {
                for start in [0, 1, 59, 60, 120, 400] {
                    let x = Label { turns: 1, left: start };
                    let mut sim = x;
                    for d in 0..40 {
                        let p = Bound::new(c, full).run((1, u32::try_from(start).unwrap()), d);
                        assert_eq!(
                            p,
                            (u64::from(sim.turns), u32::try_from(sim.left).unwrap()),
                            "full {full}, c {c}, start {start}, d {d}"
                        );
                        sim = sim.step(c, full);
                    }
                }
            }
        }
    }
}
