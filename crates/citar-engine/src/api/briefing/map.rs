//! The ASCII hex map a model reads in place of a screen (`briefing.ascii_map`,
//! `briefing.py:10-142`), and the places a briefing centres on and points out
//! (`briefing._anchor`, `_where` and `_points_of_interest`, `briefing.py:145-162, 409-430`).
//!
//! Each cell is three characters: the terrain, the top feature (or a river), and a marker for a
//! city, a unit, a camp, ruins, a resource, an improvement or a road ([`MAP_LEGEND`]). Odd rows
//! are shifted right by half a cell, as the hexes are. What the viewer has not explored is blank,
//! and what it remembers but does not see now is in lower case.
//!
//! [`MAP_LEGEND`]: crate::api::text::MAP_LEGEND

use crate::api::views::tiles::{Known, resource_seen};
use crate::base::ids::{IdVec, PlayerId, TerrainId, TileIdx};
use crate::game::Game;
use crate::game::cities::borders::within_order;
use crate::game::units;
use crate::game::vis::sight::unit_visible_to;
use crate::unique::UniqueType;

use super::civ_name;

/// The widest a map's rows reach above and below its centre (`briefing.py:62`).
pub const MAX_RADIUS: i64 = 20;
/// The narrowest.
pub const MIN_RADIUS: i64 = 2;
/// How many entries "In this window" lists at most (`briefing.py:141`).
const ENTRIES_SHOWN: usize = 120;
/// How many points of interest a briefing names (`briefing.py:430`).
const POINTS_SHOWN: usize = 12;

/// The character of a base terrain (`briefing.TERRAIN_CHAR`); `?` for one the table does not
/// name, as a mod's would be.
fn terrain_char(name: &str) -> u8 {
    match name {
        "Grassland" => b'G',
        "Plains" => b'P',
        "Desert" => b'D',
        "Tundra" => b'T',
        "Snow" => b'S',
        "Mountain" => b'M',
        "Coast" => b'c',
        "Ocean" => b'o',
        "Lakes" => b'l',
        _ => b'?',
    }
}

/// The character of a feature (`briefing.FEATURE_CHAR`), if the table names it.
fn feature_char(name: &str) -> Option<u8> {
    Some(match name {
        "Hill" => b'h',
        "Forest" => b'f',
        "Jungle" => b'j',
        "Marsh" => b'm',
        "Flood plains" => b'F',
        "Oasis" => b'O',
        "Ice" => b'i',
        "Atoll" => b'a',
        "Fallout" => b'x',
        _ => return None,
    })
}

/// Each terrain's characters as a base and as a feature, looked up by name once per map rather
/// than once per cell.
fn glyphs(g: &Game) -> IdVec<TerrainId, (u8, Option<u8>)> {
    g.rules()
        .terrains()
        .iter()
        .map(|(_, d)| (terrain_char(&d.name), feature_char(&d.name)))
        .collect()
}

/// The tile a briefing centres on (`briefing._anchor`): the capital, else the first unit that
/// can found a city, else the first unit; `None` for a civilization with neither.
#[must_use]
pub fn anchor(g: &Game, p: PlayerId) -> Option<TileIdx> {
    let pl = g.player(p)?;
    if let Some(cap) = pl.capital.and_then(|c| g.city(c)).filter(|c| c.owner() == p) {
        return Some(cap.tile());
    }
    let mut first = None;
    for u in g.player_units(p) {
        if units::type_has(g, u.base, UniqueType::FoundCity) {
            return Some(u.tile());
        }
        first = first.or(Some(u.tile()));
    }
    first
}

/// Where one tile is from another, for a model (`briefing._where`): `(12,8) 3 tiles NE`, or
/// `(12,8) here`.
#[must_use]
pub fn where_from(g: &Game, from: TileIdx, to: TileIdx) -> String {
    let (x, y) = g.xy(to);
    let d = g.grid().distance(from, to);
    if d == 0 {
        format!("({x},{y}) here")
    } else {
        format!("({x},{y}) {d} tiles {}", g.grid().direction_name(from, to))
    }
}

/// The marker of the units on a tile the viewer sees (`briefing._unit_code`): `B` for any
/// barbarian, `E` for any enemy's, else `U` for one of its own military units, `w` for its
/// civilian and `N` for a neutral's, `U` winning.
fn unit_code(g: &Game, viewer: PlayerId, t: TileIdx) -> Option<u8> {
    let r = g.rules();
    let mut best = None;
    for u in g.units_at(t) {
        if !unit_visible_to(g, viewer, u.id()) {
            continue;
        }
        let owner = u.owner();
        if g.is_barbarian(owner) {
            return Some(b'B');
        }
        let code = if owner == viewer {
            if r.base_units()[u.base].military { b'U' } else { b'w' }
        } else if g.at_war(viewer, owner) {
            return Some(b'E');
        } else {
            b'N'
        };
        if best.is_none() || code == b'U' {
            best = Some(code);
        }
    }
    best
}

/// One axis of the window: clamped at an edge, or running across a wrapping seam
/// (`briefing.py:65-69`).
const fn span(c: i32, r: i32, n: i32, wraps: bool) -> (i32, i32) {
    if wraps && 2 * r + 1 < n {
        (c - r, c + r)
    } else {
        let lo = if c - r > 0 { c - r } else { 0 };
        let hi = if c + r < n - 1 { c + r } else { n - 1 };
        (lo, hi)
    }
}

/// The area around a point as ASCII, for a player with no screen (`briefing.ascii_map`): the
/// window's rows, `radius` above and below the centre and twice as many columns each side, then
/// what the viewer sees in it: cities, units, natural wonders and resources.
///
/// `centre` is the viewer's anchor when not given; `radius` is held between [`MIN_RADIUS`] and
/// [`MAX_RADIUS`]. Empty for a player the game lacks.
#[must_use]
pub fn ascii_map(g: &Game, pid: PlayerId, centre: Option<(i32, i32)>, radius: i64) -> String {
    let (mut out, entries) = ascii_map_parts(g, pid, centre, radius);
    if !entries.is_empty() {
        out.push_str("\nIn this window:\n");
        out.push_str(&entries.join("\n"));
    }
    out
}

/// [`ascii_map`] as its map, headed by where it is, and the entries of what is in it, each
/// indented as the map lists it, at most [`ENTRIES_SHOWN`].
#[allow(clippy::too_many_lines, reason = "the rows, then the entries, as Python drew them")]
pub(super) fn ascii_map_parts(
    g: &Game,
    pid: PlayerId,
    centre: Option<(i32, i32)>,
    radius: i64,
) -> (String, Vec<String>) {
    let Some(pl) = g.player(pid) else { return (String::new(), Vec::new()) };
    let r = g.rules();
    let grid = g.grid();
    let vis = g.derived().vis();
    let (cx, cy) = centre.unwrap_or_else(|| g.xy(anchor(g, pid).unwrap_or(TileIdx(0))));
    // Within 2..=20, so the casts are exact.
    let radius = i32::try_from(radius.clamp(MIN_RADIUS, MAX_RADIUS)).unwrap_or(8);
    let (w, h) = (i32::from(grid.width()), i32::from(grid.height()));
    let (x0, x1) = span(cx, radius * 2, w, grid.wrap_x());
    let (y0, y1) = span(cy, radius, h, grid.wrap_y());
    let glyph = glyphs(g);
    let known = &r.derived().known;
    let memory = pl.major.as_deref().map(|m| &m.memory);

    let mut out = String::new();
    let mut head = String::from("      ");
    for x in x0..=x1 {
        if x.rem_euclid(5) == 0 {
            head.push_str(&format!("{:<3}", x.rem_euclid(w)));
        } else {
            head.push_str("   ");
        }
    }
    let mut lines = vec![head.trim_end().to_owned()];
    let mut window: Vec<TileIdx> = Vec::new();
    for y in y0..=y1 {
        let mut row = format!("y={:<3} ", y.rem_euclid(h));
        if y & 1 != 0 {
            row.push(' ');
        }
        for x in x0..=x1 {
            let Some(t) = grid.wrap(x, y) else {
                row.push_str("   ");
                continue;
            };
            window.push(t);
            let Some(tile) = g.tile(t).filter(|_| pl.explored.contains(t.0)) else {
                row.push_str("   ");
                continue;
            };
            let visible = vis.sees(pid, t);
            let k = Known::of(g, t, tile, Some(pid), visible);
            let mut tc = if tile.wonder().is_some() { b'*' } else { glyph[tile.terrain()].0 };
            // The top feature the table names, so a forested hill shows its forest.
            let mut fc = k
                .features
                .iter()
                .filter_map(|f| r.derived().features.get(f))
                .filter_map(|&ft| glyph[ft].1)
                .last()
                .unwrap_or(b'.');
            if fc == b'.' && tile.river_mask() != 0 {
                fc = b'r';
            }
            let city = g.city_at(t);
            let known_city =
                city.filter(|_| visible || memory.is_some_and(|m| m.city(t).is_some()));
            let marker = if let Some(c) = known_city {
                if c.owner() == pid { b'@' } else { b'C' }
            } else if let Some(u) = visible.then(|| unit_code(g, pid, t)).flatten() {
                u
            } else if k.improvement.is_some() && k.improvement == known.barbarian_camp {
                b'X'
            } else if k.improvement.is_some() && k.improvement == known.ancient_ruins {
                b'!'
            } else if tile.resource().is_some_and(|res| resource_seen(g, Some(pid), res)) {
                b'$'
            } else if k.improvement.is_some() {
                b'+'
            } else if k.route.is_some() {
                b'='
            } else {
                b' '
            };
            if !visible {
                tc = tc.to_ascii_lowercase();
            }
            row.extend([char::from(tc), char::from(fc), char::from(marker)]);
        }
        lines.push(row.trim_end().to_owned());
    }
    window.sort();
    window.dedup();
    let seen_here = |t: TileIdx| window.binary_search(&t).is_ok() && vis.sees(pid, t);

    let mut ents: Vec<String> = Vec::new();
    for c in g.state().cities().iter() {
        if seen_here(c.tile()) {
            let (x, y) = g.xy(c.tile());
            ents.push(format!(
                "  city '{}' #{} ({x},{y}) owner {} pop {}",
                c.name,
                c.id().get(),
                civ_name(g, pid, c.owner()),
                c.pop
            ));
        }
    }
    for u in g.state().units().iter() {
        if seen_here(u.tile()) && unit_visible_to(g, pid, u.id()) {
            let (x, y) = g.xy(u.tile());
            let owner = if u.owner() == pid { "yours" } else { civ_name(g, pid, u.owner()) };
            ents.push(format!(
                "  unit #{} {} ({x},{y}) {owner} hp {}",
                u.id().get(),
                r.base_units()[u.base].name,
                u.hp
            ));
        }
    }
    if let Some(c) = grid.idx(cx, cy) {
        // In Python's `within` order, which it listed them in.
        let mut near = grid.within(c, u32::try_from(radius * 2).unwrap_or(0));
        near.sort_by_key(|&t| within_order(g, c, t));
        for t in near {
            let Some(tile) = g.tile(t) else { continue };
            if window.binary_search(&t).is_err() || !pl.explored.contains(t.0) {
                continue;
            }
            let (x, y) = g.xy(t);
            if let Some(wonder) = tile.wonder() {
                ents.push(format!("  natural wonder {} ({x},{y})", r.terrains()[wonder].name));
            }
            if let Some(res) = tile.resource().filter(|&res| resource_seen(g, Some(pid), res)) {
                let owner = match tile.owner() {
                    Some(o) if o == pid => " (yours)".to_owned(),
                    Some(o) => format!(" (owned by {})", civ_name(g, pid, o)),
                    None => String::new(),
                };
                let improved = match tile.improvement() {
                    Some(i) if vis.sees(pid, t) && !tile.improvement_pillaged() => {
                        format!(", improved: {}", r.improvements()[i].name)
                    }
                    _ => String::new(),
                };
                let amount = match tile.resource_amount() {
                    0 => String::new(),
                    n => format!(" x{n}"),
                };
                ents.push(format!(
                    "  resource {}{amount} ({x},{y}){owner}{improved}",
                    r.resources()[res].name
                ));
            }
        }
    }
    let seam = x0 < 0 || x1 >= w || y0 < 0 || y1 >= h;
    out.push_str(&format!(
        "Map around ({cx},{cy}), x {}-{}, y {}-{}{}:\n{}",
        x0.rem_euclid(w),
        x1.rem_euclid(w),
        y0.rem_euclid(h),
        y1.rem_euclid(h),
        if seam { " (the map wraps: this window runs across its edge)" } else { "" },
        lines.join("\n")
    ));
    ents.truncate(ENTRIES_SHOWN);
    (out, ents)
}

/// What is worth mentioning near the anchor (`briefing._points_of_interest`): the camps and
/// ruins the viewer knows of, natural wonders, and the cities on tiles it has explored of the
/// civilizations and city-states it has met, nearest first, then by their text; `None` when
/// there is nothing.
#[must_use]
pub fn points_of_interest(g: &Game, pid: PlayerId, anchor: TileIdx) -> Option<String> {
    let pl = g.player(pid)?;
    let r = g.rules();
    let known = &r.derived().known;
    let vis = g.derived().vis();
    let grid = g.grid();
    let mut pois: Vec<(u32, String)> = Vec::new();
    for (t, tile) in g.state().tiles().iter() {
        if !pl.explored.contains(t.0) {
            continue;
        }
        let k = Known::of(g, t, tile, Some(pid), vis.sees(pid, t));
        let d = grid.distance(anchor, t);
        if k.improvement.is_some() && k.improvement == known.barbarian_camp {
            pois.push((d, format!("barbarian camp {}", where_from(g, anchor, t))));
        } else if k.improvement.is_some() && k.improvement == known.ancient_ruins {
            pois.push((d, format!("ancient ruins {}", where_from(g, anchor, t))));
        }
        if let Some(w) = tile.wonder() {
            pois.push((d, format!("{} {}", r.terrains()[w].name, where_from(g, anchor, t))));
        }
    }
    // A city whose owner the viewer has not met is one it has never seen, since seeing a city's
    // tile makes contact; it stands on a tile explored before the city was there. Python named it
    // and its owner all the same.
    // refcheck: briefing-lists-only-cities-it-could-have-seen
    for c in g.state().cities().iter() {
        let owner = c.owner();
        if owner != pid && g.has_met(pid, owner) && pl.explored.contains(c.tile().0) {
            pois.push((
                grid.distance(anchor, c.tile()),
                format!(
                    "{} city {} {}",
                    civ_name(g, pid, owner),
                    c.name,
                    where_from(g, anchor, c.tile())
                ),
            ));
        }
    }
    if pois.is_empty() {
        return None;
    }
    pois.sort();
    let shown: Vec<&str> = pois.iter().take(POINTS_SHOWN).map(|(_, s)| s.as_str()).collect();
    Some(format!("Known points of interest (from your capital/first unit): {}", shown.join("; ")))
}
