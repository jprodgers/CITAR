//! [`PathTree`]: one bounded search from a unit that answers the best path to any tile within
//! its bound, as [`Mover::find_path`] would with that turn limit (DESIGN.md 6.10).
//!
//! A barbarian weighing a handful of targets asked for a path to each (`barbarians._seek`,
//! `barbarians.py:777-795`); one tree answers them all. A tree's labels are the search's with no
//! target. A path to a tile ends with the tile's label as a target, which may differ from its
//! label on the way elsewhere: a target may be a tile where the unit could not end its turn among
//! its own units, and a tile with a foreign civilian it would capture. So the target's label is
//! worked out from its neighbours' when asked, and the rest of the path from theirs.

use super::astar::{Label, Start};
use super::class::Mover;
use crate::base::collections::LookupMap;
use crate::base::ids::TileIdx;

/// The labels of one bounded search from a unit, with the tile that gave each its label.
#[derive(Clone, Debug)]
pub struct PathTree {
    start: Start,
    max_turns: u32,
    labels: LookupMap<TileIdx, (u64, Option<TileIdx>)>,
    len: usize,
}

impl PathTree {
    /// A search from where the mover's unit stands, labelling every tile it can reach within
    /// `max_turns` turns. `None` for a type with no unit.
    #[must_use]
    pub fn build(m: &Mover<'_>, max_turns: u32) -> Option<Self> {
        let start = m.start()?;
        let found = if m.is_air() { Vec::new() } else { m.labels_within(start, max_turns) };
        let len = found.len();
        let mut labels = LookupMap::new();
        for (t, k, parent) in found {
            labels.insert(t, (k, parent));
        }
        Some(Self { start, max_turns, labels, len })
    }

    /// How many tiles it labelled.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether it labelled nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Where the search stood on a tile on its way elsewhere.
    #[must_use]
    pub fn label(&self, t: TileIdx) -> Option<Label> {
        self.labels.get(&t).map(|&(k, _)| Label::from_key(k))
    }

    /// The best path to `target`, as [`Mover::find_path`] finds it with the tree's turn limit;
    /// `m` must be the mover the tree was built with, on the same game.
    #[must_use]
    pub fn path_to(&self, m: &Mover<'_>, target: TileIdx) -> Option<Vec<TileIdx>> {
        let g = m.game();
        let s = self.start;
        if target == s.tile {
            return Some(vec![s.tile]);
        }
        if m.is_air() || !g.grid().contains(target) {
            return None;
        }
        let known = m.barbarian || g.player(m.pid).is_some_and(|p| p.explored.contains(target.0));
        if (known && m.terrain_reason(target).is_some()) || !m.passable(target, Some(target)) {
            return None;
        }
        // The target's label: the best its expanded neighbours step to.
        let expanded = |u: TileIdx| {
            self.labels
                .get(&u)
                .map(|&(k, _)| k)
                .filter(|&k| Label::from_key(k).turns <= self.max_turns)
        };
        let end = g
            .grid()
            .neighbors(target)
            .filter_map(|u| {
                let k = expanded(u)?;
                Some(Label::from_key(k).step(m.edge_cost(u, target), s.full).key())
            })
            .min()?;
        let mut path = vec![target];
        let (mut v, mut vk) = (target, end);
        let mut guard = self.len + 1;
        while v != s.tile && guard > 0 {
            guard -= 1;
            let mut best: Option<(u64, TileIdx)> = None;
            for u in g.grid().neighbors(v) {
                if u == target {
                    continue;
                }
                let Some(uk) = expanded(u) else { continue };
                // From the target, which is no candidate, any earlier label may do.
                if v != target && (uk, u.0) >= (vk, v.0) {
                    continue;
                }
                if Label::from_key(uk).step(m.edge_cost(u, v), s.full).key() != vk {
                    continue;
                }
                if best.is_none_or(|(bk, bt)| (uk, u.0) < (bk, bt.0)) {
                    best = Some((uk, u));
                }
            }
            let Some((uk, u)) = best else {
                // Free steps: back along the tiles that gave each its label (see
                // `astar`'s module doc).
                let mut at = v;
                while at != s.tile && guard > 0 {
                    guard -= 1;
                    at = self.labels.get(&at).and_then(|&(_, p)| p)?;
                    path.push(at);
                }
                break;
            };
            path.push(u);
            v = u;
            vk = uk;
        }
        path.reverse();
        (path.first() == Some(&s.tile)).then_some(path)
    }
}
