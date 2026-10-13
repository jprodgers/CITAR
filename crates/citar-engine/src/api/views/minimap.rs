//! The minimap's block of a major civilization's own view (design/phase3-features.md P3.7.2,
//! package 3-17): how far into the game the seat is, where its capital stands, and the box of
//! the tiles it has explored on its home continent. The browser bounds a person's minimap with
//! them (`citar/web/js/minimap.js`).
//!
//! The Python engine's views had no such block (refcheck `minimap-view-block`); it replaces the
//! browser's whole-map minimap, which needed no help from the engine (`game.js` before 3-17).
//!
//! The home box counts only the tiles of the home continent the seat has explored and has seen
//! joined to its home: those a search from the anchor reaches through explored tiles of that
//! continent. So it tells the seat nothing its own view's tiles do not. A continent's unexplored
//! coast never widens it, and neither does land a scout explored that only unexplored land joins
//! to home: the continents are the map's, which the view does not carry, and counting every
//! explored tile of the home continent would tell the seat which far land is home and which is
//! not. The search visits only the tiles it reaches, in seat views only; the god view and a
//! spectator's have no block.

use serde::Serialize;

use crate::base::ids::{EraId, PlayerId, TileIdx};
use crate::base::sets::BitSet;
use crate::game::Game;
use crate::game::units;
use crate::rules::Ruleset;
use crate::unique::UniqueType;

/// How far the minimap opens out, by the seat's era ([`stage_of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    /// Before the Renaissance: a window around the capital, as far as explored.
    Early,
    /// From the Renaissance: the explored tiles near the home continent.
    Renaissance,
    /// From the Information era: the whole map.
    Information,
}

/// A box of tiles, its corners inclusive. On an axis that wraps, the box may cross the seam:
/// then `x0 > x1` (or `y0 > y1`), and it runs from `x0` to the map's edge and on from column 0
/// to `x1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct TileBox {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

/// The minimap's block (`view.minimap`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Minimap {
    pub stage: Stage,
    /// The capital's tile as `[x, y]`, or `None` without one.
    pub capital: Option<[i32; 2]>,
    /// The box of the explored tiles of the home continent joined to the anchor through tiles the
    /// seat has explored. The anchor is the capital; without a capital, the seat's first city,
    /// else its first unit that founds cities. `None` with none of them, or one at sea.
    pub home: Option<TileBox>,
}

/// The stage a civilization in `era` has reached: the Information era's and the Renaissance's
/// as the ruleset names them, else by their positions ([`MinimapEras`]).
///
/// [`MinimapEras`]: crate::rules::derived::MinimapEras
#[must_use]
pub fn stage_of(r: &Ruleset, era: EraId) -> Stage {
    let k = r.derived().known.minimap;
    if k.information.is_some_and(|i| era >= i) {
        Stage::Information
    } else if k.renaissance.is_some_and(|i| era >= i) {
        Stage::Renaissance
    } else {
        Stage::Early
    }
}

/// The shortest interval of the positions `0..on.len()` that covers every one marked, as
/// `(lo, hi)` inclusive, or `None` when none is. On a circle (`wrap`) it may cross the seam,
/// `lo > hi`: it leaves out the longest run of unmarked positions. Of two runs as long, the one
/// across the seam is left out first (the interval then does not cross it), else the first.
/// The browser's `exploredBox` chooses the same way (`citar/web/js/minimap.js: covering`).
#[must_use]
pub fn covering(on: &[bool], wrap: bool) -> Option<(usize, usize)> {
    let first = on.iter().position(|&b| b)?;
    let last = on.iter().rposition(|&b| b)?;
    if !wrap {
        return Some((first, last));
    }
    // the run across the seam: after the last marked position and before the first
    let (mut gap, mut after) = (on.len() - 1 - last + first, last);
    let mut prev = first;
    for (i, _) in on.iter().enumerate().take(last + 1).skip(first + 1).filter(|&(_, &b)| b) {
        if i - prev - 1 > gap {
            (gap, after) = (i - prev - 1, prev);
        }
        prev = i;
    }
    if after == last {
        return Some((first, last));
    }
    let next = on.iter().enumerate().skip(after + 1).find(|&(_, &b)| b).map_or(first, |(i, _)| i);
    Some((next, after))
}

impl Game {
    /// The minimap's block of `p`'s own view, or `None` for a player that is no major
    /// civilization of this game.
    #[must_use]
    pub fn minimap(&self, p: PlayerId) -> Option<Minimap> {
        let pl = self.player(p).filter(|x| x.is_major())?;
        let capital = pl.capital.and_then(|c| self.city(c)).filter(|c| c.owner() == p);
        let anchor = capital
            .map(|c| c.tile())
            .or_else(|| self.player_cities(p).next().map(|c| c.tile()))
            .or_else(|| {
                self.player_units(p)
                    .find(|u| units::type_has(self, u.base, UniqueType::FoundCity))
                    .map(|u| u.tile())
            });
        let home = anchor.and_then(|t| self.home_box(p, t));
        Some(Minimap {
            stage: stage_of(self.rules(), crate::game::query::era(self, p)),
            capital: capital.map(|c| {
                let (x, y) = self.xy(c.tile());
                [x, y]
            }),
            home,
        })
    }

    /// The box of the tiles `p` has explored that join `anchor` through explored tiles of its
    /// continent; `None` for an anchor at sea or one `p` has not explored (its own city's or
    /// unit's tile always is).
    fn home_box(&self, p: PlayerId, anchor: TileIdx) -> Option<TileBox> {
        let continent = self.continent(anchor)?;
        let explored = &self.player(p)?.explored;
        if !explored.contains(anchor.0) {
            return None;
        }
        let (map, grid) = (self.state().map(), self.grid());
        let (w, h) = (usize::from(map.width), usize::from(map.height));
        let (mut cols, mut rows) = (vec![false; w], vec![false; h]);
        let mut reached = BitSet::with_capacity(map.size());
        reached.insert(anchor.0);
        let mut todo = vec![anchor];
        while let Some(t) = todo.pop() {
            let (x, y) = self.xy(t);
            if let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y))
                && let (Some(c), Some(r)) = (cols.get_mut(x), rows.get_mut(y))
            {
                (*c, *r) = (true, true);
            }
            for n in grid.neighbors(t) {
                if explored.contains(n.0)
                    && map.continent(n) == Some(continent)
                    && reached.insert(n.0)
                {
                    todo.push(n);
                }
            }
        }
        let (x0, x1) = covering(&cols, map.wrap_x)?;
        let (y0, y1) = covering(&rows, map.wrap_y)?;
        let at = |i: usize| i32::try_from(i).unwrap_or(i32::MAX);
        Some(TileBox { x0: at(x0), y0: at(y0), x1: at(x1), y1: at(y1) })
    }
}

#[cfg(test)]
mod tests {
    use super::covering;

    fn marks(n: usize, at: &[usize]) -> Vec<bool> {
        (0..n).map(|i| at.contains(&i)).collect()
    }

    #[test]
    fn the_covering_interval_on_a_line_is_the_first_to_the_last() {
        assert_eq!(covering(&marks(10, &[]), false), None);
        assert_eq!(covering(&marks(10, &[4]), false), Some((4, 4)));
        assert_eq!(covering(&marks(10, &[0, 1, 8, 9]), false), Some((0, 9)));
    }

    #[test]
    fn the_covering_interval_on_a_circle_leaves_out_the_longest_gap() {
        assert_eq!(covering(&marks(10, &[]), true), None);
        assert_eq!(covering(&marks(10, &[6]), true), Some((6, 6)));
        // across the seam
        assert_eq!(covering(&marks(10, &[0, 1, 8, 9]), true), Some((8, 1)));
        assert_eq!(covering(&marks(10, &[9, 0]), true), Some((9, 0)));
        // not across it
        assert_eq!(covering(&marks(10, &[2, 3, 4]), true), Some((2, 4)));
        // every position
        assert_eq!(covering(&[true; 10], true), Some((0, 9)));
        // two gaps as long: the one across the seam is left out first ...
        assert_eq!(covering(&marks(8, &[2, 5]), true), Some((2, 5)));
        // ... and of two inside, the first
        assert_eq!(covering(&marks(12, &[0, 4, 8, 11]), true), Some((4, 0)));
    }
}
