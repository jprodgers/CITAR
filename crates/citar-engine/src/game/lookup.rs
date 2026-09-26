//! What a tool call names, looked up as the tools looked it up (`tools._idx`, `_own_unit` and
//! `_own_city`, `tools.py:143-175`): a tile by its coordinates, one of the caller's units, one of
//! the caller's cities. Every action and query tool that takes one goes through these, so the
//! refusals are the same wherever a model meets them.
//!
//! Each refusal says what is valid: the map's size, or the units or cities the caller does have,
//! since a model that has lost track of one recovers faster from a list than from "no such
//! unit". A refusal whose list would take it past the 600 characters a refusal may take
//! (property P5, DESIGN.md 8.5), a late empire's hundred units, keeps the entries that fit and
//! ends with how many more there are and the tool that lists them all, where Python listed them
//! all (`refusal-lists-capped`).

use crate::base::ids::{CityId, PlayerId, TileIdx, UnitId};
use crate::game::Game;
use crate::game::error::{ActionError, ErrCode};

/// The most characters a refusal takes (property P5, DESIGN.md 8.5).
pub const REFUSAL_CHARS: usize = 600;

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
        let head = format!(
            "You have no unit with id {unit_id} (units are used up by some actions and lost when \
             killed). Your units now: "
        );
        ActionError::new(ErrCode::NoSuchUnit, with_list(&head, &ids, ".", "get_units"))
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
        let head = format!("You have no city with id {city_id}. Your cities: ");
        ActionError::new(ErrCode::NoSuchCity, with_list(&head, &ids, ".", "get_cities"))
    })
}

/// `head`, then `entries` joined with commas (`none` if there are none), then `tail`. A text
/// longer than [`REFUSAL_CHARS`] keeps the entries that fit and says how many more `tool` lists.
// refcheck: refusal-lists-capped
pub(crate) fn with_list(head: &str, entries: &[String], tail: &str, tool: &str) -> String {
    if entries.is_empty() {
        return format!("{head}none{tail}");
    }
    let whole = format!("{head}{}{tail}", entries.join(", "));
    let chars = |s: &str| s.chars().count();
    if chars(&whole) <= REFUSAL_CHARS {
        return whole;
    }
    let room = REFUSAL_CHARS.saturating_sub(chars(head) + chars(tail));
    // The longest note there could be, so that whatever is kept leaves room for it.
    let note_room = chars(&format!(", and {} more ({tool} lists them all)", entries.len()));
    let mut list = String::new();
    let mut kept = 0;
    for e in entries {
        let sep = if kept == 0 { 0 } else { 2 };
        if chars(&list) + sep + chars(e) + note_room > room {
            break;
        }
        if kept > 0 {
            list.push_str(", ");
        }
        list.push_str(e);
        kept += 1;
    }
    let more = entries.len() - kept;
    if kept == 0 {
        return format!("{head}{more} of them ({tool} lists them all){tail}");
    }
    format!("{head}{list}, and {more} more ({tool} lists them all){tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_is_cut_only_when_the_refusal_would_run_long() {
        let head = "Your units: ";
        assert_eq!(with_list(head, &[], ".", "get_units"), "Your units: none.");
        let few: Vec<String> = (1..=3).map(|i| format!("#{i} Worker")).collect();
        assert_eq!(
            with_list(head, &few, ".", "get_units"),
            "Your units: #1 Worker, #2 Worker, #3 Worker."
        );
        // Exactly as long as a refusal may be: kept whole.
        let exact = vec!["x".repeat(REFUSAL_CHARS - head.len() - 1)];
        assert_eq!(with_list(head, &exact, ".", "get_units").chars().count(), REFUSAL_CHARS);
        let many: Vec<String> = (1..=200).map(|i| format!("#{i} Mechanized Infantry")).collect();
        let text = with_list(head, &many, ".", "get_units");
        assert!(text.chars().count() <= REFUSAL_CHARS, "{text}");
        assert!(text.starts_with("Your units: #1 Mechanized Infantry, #2 "), "{text}");
        let kept = text.matches("Mechanized").count();
        let note = format!(", and {} more (get_units lists them all).", 200 - kept);
        assert!(text.ends_with(&note), "{text}");
        let huge = vec!["x".repeat(700)];
        assert_eq!(
            with_list("Cities: ", &huge, ".", "get_cities"),
            "Cities: 1 of them (get_cities lists them all)."
        );
    }
}
