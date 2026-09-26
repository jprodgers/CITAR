//! What a tool call names, looked up as the tools looked it up (`tools._idx`, `_own_unit` and
//! `_own_city`, `tools.py:143-175`): a tile by its coordinates, one of the caller's units, one of
//! the caller's cities. Every action and query tool that takes one goes through these, so the
//! refusals are the same wherever a model meets them.
//!
//! Each refusal says what is valid: the map's size, or the units or cities the caller does have,
//! since a model that has lost track of one recovers faster from a list than from "no such
//! unit". A list too long to read in a refusal (a late empire's hundred units) ends with how
//! many more there are and the tool that lists them all, so the text stays within the 600
//! characters a refusal may take (property P5, DESIGN.md 8.5), where Python listed them all
//! (`refusal-lists-capped`).

use crate::base::ids::{CityId, PlayerId, TileIdx, UnitId};
use crate::game::Game;
use crate::game::error::{ActionError, ErrCode};

/// A tile by its coordinates (`tools._idx`, `tools.py:143-151`); the refusal names the map's
/// size, because the usual cause is a caller that has assumed another.
///
/// # Errors
/// Coordinates off the map.
pub fn tile_at(g: &Game, x: i64, y: i64) -> Result<TileIdx, ActionError> {
    let fits = |n: i64| i32::try_from(n).ok();
    fits(x).zip(fits(y)).and_then(|(x, y)| g.grid().idx(x, y)).ok_or_else(|| {
        let grid = g.grid();
        ActionError::new(
            ErrCode::OffMap,
            format!("({x},{y}) is off the map (map is {}x{}).", grid.width(), grid.height()),
        )
    })
}

/// One of the caller's own units (`tools._own_unit`, `tools.py:154-166`); the refusal lists the
/// units they do have.
///
/// # Errors
/// No unit of the caller's with that id.
pub fn own_unit(g: &Game, pid: PlayerId, unit_id: i64) -> Result<UnitId, ActionError> {
    let found = u32::try_from(unit_id)
        .ok()
        .and_then(UnitId::new)
        .filter(|&u| g.unit(u).is_some_and(|x| x.owner() == pid));
    found.ok_or_else(|| {
        let r = g.rules();
        let ids: Vec<String> = g
            .player_units(pid)
            .map(|x| format!("#{} {}", x.id().get(), r.name(x.base).unwrap_or("")))
            .collect();
        ActionError::new(
            ErrCode::NoSuchUnit,
            format!(
                "You have no unit with id {unit_id} (units are used up by some actions and lost \
                 when killed). Your units now: {}.",
                listing(&ids, "get_units")
            ),
        )
    })
}

/// One of the caller's own cities (`tools._own_city`, `tools.py:169-175`); the refusal lists the
/// cities they do have.
///
/// # Errors
/// No city of the caller's with that id.
pub fn own_city(g: &Game, pid: PlayerId, city_id: i64) -> Result<CityId, ActionError> {
    let found = u32::try_from(city_id)
        .ok()
        .and_then(CityId::new)
        .filter(|&c| g.city(c).is_some_and(|x| x.owner() == pid));
    found.ok_or_else(|| {
        let ids: Vec<String> =
            g.player_cities(pid).map(|x| format!("#{} {}", x.id().get(), x.name)).collect();
        ActionError::new(
            ErrCode::NoSuchCity,
            format!(
                "You have no city with id {city_id}. Your cities: {}.",
                listing(&ids, "get_cities")
            ),
        )
    })
}

/// The characters a list in a refusal may take, its tail included: with the sentence around it
/// the refusal stays within 600.
const LIST_ROOM: usize = 400;

/// `entries` joined with commas, or `none`; a list longer than [`LIST_ROOM`] characters keeps
/// the entries that fit and says how many more `tool` lists.
// refcheck: refusal-lists-capped
pub(crate) fn listing(entries: &[String], tool: &str) -> String {
    if entries.is_empty() {
        return "none".to_owned();
    }
    let all = entries.join(", ");
    if all.chars().count() <= LIST_ROOM {
        return all;
    }
    // The longest tail there could be, so that whatever is kept leaves room for it.
    let tail_room = format!(", and {} more ({tool} lists them all)", entries.len()).len();
    let mut out = String::new();
    let mut kept = 0;
    for e in entries {
        let sep = if kept == 0 { 0 } else { 2 };
        if out.chars().count() + sep + e.chars().count() + tail_room > LIST_ROOM {
            break;
        }
        if kept > 0 {
            out.push_str(", ");
        }
        out.push_str(e);
        kept += 1;
    }
    let more = entries.len() - kept;
    if kept == 0 {
        return format!("{more} of them ({tool} lists them all)");
    }
    format!("{out}, and {more} more ({tool} lists them all)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_list_keeps_what_fits_and_says_how_many_more() {
        assert_eq!(listing(&[], "get_units"), "none");
        let few: Vec<String> = (1..=3).map(|i| format!("#{i} Worker")).collect();
        assert_eq!(listing(&few, "get_units"), "#1 Worker, #2 Worker, #3 Worker");
        let many: Vec<String> = (1..=200).map(|i| format!("#{i} Mechanized Infantry")).collect();
        let text = listing(&many, "get_units");
        assert!(text.chars().count() <= LIST_ROOM, "{text}");
        assert!(text.starts_with("#1 Mechanized Infantry, #2 "), "{text}");
        let kept = text.matches("Mechanized").count();
        assert!(text.ends_with(&format!(", and {} more (get_units lists them all)", 200 - kept)));
        let huge = vec!["x".repeat(500)];
        assert_eq!(listing(&huge, "get_cities"), "1 of them (get_cities lists them all)");
    }
}
