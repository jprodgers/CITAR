//! Tile references written as text: an anchor of the map (`A`), coordinates (`(3,4)`), or a walk
//! from either (`A>e>ne`), one step per direction in odd-r offset coordinates, where odd rows are
//! shifted right. Selectors, which ask the game (`find_tiles`), are the runner's.

use std::collections::BTreeMap;

/// Offset steps on even rows, in the order e, ne, nw, w, sw, se (`hexmap.py:14`).
const EVEN: [(i32, i32); 6] = [(1, 0), (0, -1), (-1, -1), (-1, 0), (-1, 1), (0, 1)];
/// Offset steps on odd rows (`hexmap.py:15`).
const ODD: [(i32, i32); 6] = [(1, 0), (1, -1), (0, -1), (-1, 0), (0, 1), (1, 1)];
/// The directions' names, in that order.
const DIRS: [&str; 6] = ["e", "ne", "nw", "w", "sw", "se"];

/// A map's anchors and size, which references are resolved against.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub anchors: BTreeMap<String, (i32, i32)>,
    pub width: i32,
    pub height: i32,
}

impl Frame {
    /// The coordinates `reference` names.
    pub fn resolve(&self, reference: &str) -> Result<(i32, i32), String> {
        let mut parts = reference.split('>');
        let base = parts.next().unwrap_or_default().trim();
        let mut at = self.base(base).ok_or_else(|| {
            format!(
                "`{reference}` is no tile: {base:?} is neither an anchor ({}) nor (x,y)",
                self.anchors.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        })?;
        for step in parts {
            let step = step.trim();
            let d = DIRS.iter().position(|&n| n == step).ok_or_else(|| {
                format!("`{reference}`: {step:?} is no direction ({})", DIRS.join(", "))
            })?;
            let (dx, dy) = if at.1 % 2 == 0 { EVEN[d] } else { ODD[d] };
            at = (at.0 + dx, at.1 + dy);
            if !(0..self.width).contains(&at.0) || !(0..self.height).contains(&at.1) {
                return Err(format!("`{reference}` walks off the map"));
            }
        }
        Ok(at)
    }

    fn base(&self, text: &str) -> Option<(i32, i32)> {
        if let Some(&xy) = self.anchors.get(text) {
            return Some(xy);
        }
        let inner = text.strip_prefix('(')?.strip_suffix(')')?;
        let (x, y) = inner.split_once(',')?;
        Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_in_odd_r() -> Result<(), String> {
        let f = Frame {
            anchors: [("A".to_owned(), (5, 5))].into_iter().collect(),
            width: 24,
            height: 16,
        };
        assert_eq!(f.resolve("A")?, (5, 5));
        assert_eq!(f.resolve("(3, 4)")?, (3, 4));
        // Row 5 is odd: north-east is (x + 1, y - 1); row 4 is even: north-east is (x, y - 1).
        assert_eq!(f.resolve("A>ne")?, (6, 4));
        assert_eq!(f.resolve("A>ne>ne")?, (6, 3));
        assert_eq!(f.resolve("A>w>w>e>e")?, (5, 5));
        assert!(f.resolve("(0,0)>w").is_err());
        assert!(f.resolve("Q").is_err());
        assert!(f.resolve("A>up").is_err());
        Ok(())
    }
}
