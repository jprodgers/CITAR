//! `base::hex` against the Python `HexGrid`, as `scripts/refcheck/hex_vectors.py` recorded it
//! (package 1a-02, gate 3): duel, standard and gargantuan maps, with and without each wrap.
//!
//! Neighbours, distances and lines must match exactly; `within` and `ring` as sets, since the
//! Rust port lists a ring in its own documented order. Samples are compared in full; checksums
//! cover every tile.

use citar_engine::base::hex::HexGrid;
use citar_engine::base::ids::TileIdx;
use serde_json::Value;

const VECTORS: &str = include_str!("../../data/hex_vectors.json");

/// The recorder's checksum: h = (h * 1000003 + v + 2) mod 2^64 per value, v = -1 closing a list.
#[derive(Default)]
struct Checksum(u64);

impl Checksum {
    fn value(&mut self, v: i64) {
        self.0 = self.0.wrapping_mul(1_000_003).wrapping_add((v + 2).cast_unsigned());
    }

    fn seq(&mut self, xs: impl IntoIterator<Item = TileIdx>) {
        for x in xs {
            self.value(i64::from(x.0));
        }
        self.value(-1);
    }

    fn seq_u32(&mut self, xs: impl IntoIterator<Item = u32>) {
        for x in xs {
            self.value(i64::from(x));
        }
        self.value(-1);
    }

    fn hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

fn tile(v: &Value) -> TileIdx {
    TileIdx(v.as_u64().and_then(|n| u32::try_from(n).ok()).expect("a tile index"))
}

fn tiles(v: &Value) -> Vec<TileIdx> {
    v.as_array().expect("a list of tiles").iter().map(tile).collect()
}

fn num(v: &Value) -> u32 {
    v.as_u64().and_then(|n| u32::try_from(n).ok()).expect("a small number")
}

fn sorted(mut v: Vec<TileIdx>) -> Vec<TileIdx> {
    v.sort();
    v
}

/// Every mismatch in one grid, described.
fn check_grid(g: &Value) -> Vec<String> {
    let name = g["name"].as_str().expect("a name");
    let width = u16::try_from(num(&g["width"])).expect("a width");
    let height = u16::try_from(num(&g["height"])).expect("a height");
    let wrap_x = g["wrap_x"].as_bool().expect("wrap_x");
    let wrap_y = g["wrap_y"].as_bool().expect("wrap_y");
    let grid = HexGrid::new(width, height, wrap_x, wrap_y).expect("a valid grid");
    let mut bad = Vec::new();

    let samples = tiles(&g["tiles"]);
    for (t, want) in samples.iter().zip(g["neighbors"].as_array().expect("neighbours")) {
        let got: Vec<TileIdx> = grid.neighbors(*t).collect();
        if got != tiles(want) {
            bad.push(format!("{name}: neighbours of {t:?}: Python {want}, Rust {got:?}"));
        }
    }
    for row in g["distance"].as_array().expect("distances") {
        let (a, b, want) = (tile(&row[0]), tile(&row[1]), num(&row[2]));
        let got = grid.distance(a, b);
        if got != want {
            bad.push(format!("{name}: distance {a:?}-{b:?}: Python {want}, Rust {got}"));
        }
    }
    for row in g["line"].as_array().expect("lines") {
        let (a, b, want) = (tile(&row[0]), tile(&row[1]), tiles(&row[2]));
        let got = grid.line(a, b);
        if got != want {
            bad.push(format!("{name}: line {a:?}-{b:?}: Python {want:?}, Rust {got:?}"));
        }
    }
    for row in g["within"].as_array().expect("within") {
        let (t, r, want) = (tile(&row[0]), num(&row[1]), tiles(&row[2]));
        let got = grid.within(t, r);
        let dists: Vec<u32> = got.iter().map(|&x| grid.distance(t, x)).collect();
        if !dists.windows(2).all(|w| w[0] <= w[1]) || dists.last().is_some_and(|&d| d > r) {
            bad.push(format!("{name}: within({t:?}, {r}) is not nearest first within {r}"));
        }
        let got = sorted(got);
        if got.windows(2).any(|w| w[0] == w[1]) {
            bad.push(format!("{name}: within({t:?}, {r}) lists a tile twice"));
        }
        if got != want {
            bad.push(format!("{name}: within({t:?}, {r}): Python {want:?}, Rust {got:?}"));
        }
    }
    for row in g["ring"].as_array().expect("rings") {
        let (t, r, want) = (tile(&row[0]), num(&row[1]), tiles(&row[2]));
        let got = sorted(grid.ring(t, r));
        if got != want {
            bad.push(format!("{name}: ring({t:?}, {r}): Python {want:?}, Rust {got:?}"));
        }
    }

    let sums = &g["sums"];
    let mut check_sum = |key: &str, c: &Checksum| {
        if sums[key].as_str() != Some(c.hex().as_str()) {
            bad.push(format!("{name}: checksum `{key}` over every tile differs"));
        }
    };
    let mut c = Checksum::default();
    for t in grid.tiles() {
        c.seq(grid.neighbors(t));
    }
    check_sum("neighbors", &c);
    for r in [1, 2] {
        let mut c = Checksum::default();
        for t in grid.tiles() {
            c.seq(sorted(grid.within(t, r)));
        }
        check_sum(&format!("within{r}"), &c);
    }
    for r in [1, 2, 3] {
        let mut c = Checksum::default();
        for t in grid.tiles() {
            c.seq(sorted(grid.ring(t, r)));
        }
        check_sum(&format!("ring{r}"), &c);
    }
    let mut c = Checksum::default();
    for &src in &samples[..8] {
        c.seq_u32(grid.tiles().map(|t| grid.distance(src, t)));
    }
    check_sum("distance", &c);
    let mut c = Checksum::default();
    for src in [samples[0], samples[8]] {
        for t in grid.tiles() {
            c.seq(grid.line(src, t));
        }
    }
    check_sum("line", &c);
    bad
}

#[test]
fn hex_grid_matches_python() {
    let doc: Value = serde_json::from_str(VECTORS).expect("hex_vectors.json is JSON");
    let grids = doc["grids"].as_array().expect("grids");
    assert_eq!(grids.len(), 12, "3 sizes x 4 wraps");
    let bad: Vec<String> = grids.iter().flat_map(check_grid).collect();
    assert!(bad.is_empty(), "{} mismatches:\n{}", bad.len(), bad[..bad.len().min(20)].join("\n"));
}

#[test]
fn ring_is_within_at_that_distance() {
    for (wx, wy) in [(false, false), (true, false), (false, true), (true, true)] {
        let grid = HexGrid::new(12, 10, wx, wy).expect("a valid grid");
        for t in grid.tiles() {
            for r in 0..9 {
                let ring = sorted(grid.ring(t, r));
                let from_within: Vec<TileIdx> = sorted(
                    grid.within(t, r).into_iter().filter(|&x| grid.distance(t, x) == r).collect(),
                );
                assert_eq!(ring, from_within, "ring({t:?}, {r}) with wraps {wx} {wy}");
            }
        }
    }
}
