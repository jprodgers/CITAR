//! Countables: the quantities a unique compares or multiplies by, such as `[Cities]` or
//! `Remaining [City-State] Civilizations` (`uniques.py:738-772`).
//!
//! This package compiles them; evaluating one against a game is package 1a-07's
//! (DESIGN.md 5.9). Python read the text again at every evaluation and answered `None` for a
//! text it did not know, which made the conditional false; here such a text does not load.

use crate::base::ids::{CityFilterId, CivFilterId, SetRef, UnitFilterId};
use crate::base::stats::Stat;

/// A compiled countable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Countable {
    /// A number: `2`.
    Int(i32),
    /// `turns`: the game's turn.
    Turns,
    /// `Cities`: the civilization's cities.
    Cities,
    /// `Units`: the civilization's units.
    Units,
    /// `Completed Policy branches`.
    CompletedBranches,
    /// A stat's name: the civilization's stock of it (a city's food, for `Food` in a city).
    Stat(Stat),
    /// `[filter] Units`: the civilization's units that match.
    UnitsMatching(UnitFilterId),
    /// `[filter] Cities`: the civilization's cities that match.
    CitiesMatching(CityFilterId),
    /// `Remaining [filter] Civilizations`: the living civilizations that match.
    RemainingCivs(CivFilterId),
    /// `[filter] Buildings`: the buildings in the civilization's cities that match.
    BuildingsMatching(SetRef),
}

/// How a countable's text reads, before its filter is interned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CountableText<'a> {
    Int(i32),
    Turns,
    Cities,
    Units,
    CompletedBranches,
    Stat(Stat),
    UnitsMatching(&'a str),
    CitiesMatching(&'a str),
    RemainingCivs(&'a str),
    BuildingsMatching(&'a str),
}

/// Reads a countable's text as `uniques.py:738-772` did, in the same order: a number first, then
/// the fixed words, then a stat's name, then the forms with a filter. `None` for anything else.
pub(crate) fn parse(text: &str) -> Option<CountableText<'_>> {
    // Python's int() takes surrounding space and a sign; the ruleset writes neither space nor
    // anything but digits, and a strict read keeps it that way.
    if let Ok(n) = text.parse::<i32>() {
        return Some(CountableText::Int(n));
    }
    match text {
        "turns" => return Some(CountableText::Turns),
        "Cities" => return Some(CountableText::Cities),
        "Units" => return Some(CountableText::Units),
        "Completed Policy branches" => return Some(CountableText::CompletedBranches),
        _ => {}
    }
    if let Some(stat) = Stat::from_name(text) {
        return Some(CountableText::Stat(stat));
    }
    let (placeholder, params) = super::text::placeholder(text);
    let &[filter] = params.as_slice() else { return None };
    match placeholder.as_str() {
        "[] Units" => Some(CountableText::UnitsMatching(filter)),
        "[] Cities" => Some(CountableText::CitiesMatching(filter)),
        "Remaining [] Civilizations" => Some(CountableText::RemainingCivs(filter)),
        "[] Buildings" => Some(CountableText::BuildingsMatching(filter)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countables_read_as_python_read_them() {
        assert_eq!(parse("2"), Some(CountableText::Int(2)));
        assert_eq!(parse("-3"), Some(CountableText::Int(-3)));
        assert_eq!(parse("turns"), Some(CountableText::Turns));
        assert_eq!(parse("Cities"), Some(CountableText::Cities));
        assert_eq!(parse("Completed Policy branches"), Some(CountableText::CompletedBranches));
        assert_eq!(parse("Gold"), Some(CountableText::Stat(Stat::Gold)));
        assert_eq!(parse("[Military] Units"), Some(CountableText::UnitsMatching("Military")));
        assert_eq!(parse("[Holy] Cities"), Some(CountableText::CitiesMatching("Holy")));
        assert_eq!(
            parse("Remaining [City-State] Civilizations"),
            Some(CountableText::RemainingCivs("City-State"))
        );
        assert_eq!(parse("[Temple] Buildings"), Some(CountableText::BuildingsMatching("Temple")));
        assert_eq!(parse("Turns"), None, "Python's word is lower case");
        assert_eq!(parse("gold"), None);
        assert_eq!(parse("[non-[Air]] Units"), Some(CountableText::UnitsMatching("non-[Air]")));
        assert_eq!(parse("[a] [b] Units"), None, "one filter");
        assert_eq!(parse(""), None);
    }
}
