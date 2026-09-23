//! Countables: the quantities a unique compares or multiplies by, such as `[Cities]` or
//! `Remaining [City-State] Civilizations` (`uniques.py:738-772`).
//!
//! The compiler reads them (`parse`); [`Countable::eval`] counts one in a context (DESIGN.md
//! 5.9). Python read the text again at every evaluation and answered `None` for a text it did not
//! know, which made the conditional false; here such a text does not load. `None` remains the
//! answer where Python gave it for a known text: a count of a civilization's things with no
//! civilization in context.

use super::filter::{Filters, UnitScope};
use super::table::{CondDeps, UniqueTable};
use super::world::{Ctx, EvalWorld};
use crate::base::ids::{BuildingId, CityFilterId, CivFilterId, SetRef, UnitFilterId};
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

impl Countable {
    /// The count in `ctx` (`countable`, `uniques.py:738-772`), or `None` for a count of a
    /// civilization's things with no civilization in context. A stat is the civilization's stock
    /// of it, truncated as Python's `int()` truncated it; in a city, `Food` is the city's stored
    /// food and `Production` none.
    pub fn eval<W: EvalWorld>(self, w: &W, ctx: &Ctx) -> Option<i64> {
        let t = w.rules().uniques();
        let f = t.filters();
        let count = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
        let civ = ctx.civ;
        Some(match self {
            Self::Int(n) => i64::from(n),
            Self::Turns => i64::from(w.turn()),
            Self::Cities => count(w.civ_cities(civ?).count()),
            Self::Units => count(w.civ_units(civ?).count()),
            Self::CompletedBranches => i64::from(w.civ_completed_branches(civ?)),
            Self::Stat(s @ (Stat::Food | Stat::Production)) if ctx.city.is_some() => {
                match (s, ctx.city) {
                    (Stat::Food, Some(c)) => truncate(w.city_food(c)),
                    _ => 0,
                }
            }
            Self::Stat(s) => truncate(w.civ_stock(civ?, s)),
            Self::UnitsMatching(x) => count(
                w.civ_units(civ?)
                    .filter(|&u| f.unit_matches(x, w, u, UnitScope::default()))
                    .count(),
            ),
            Self::CitiesMatching(x) => {
                count(w.civ_cities(civ?).filter(|&c| f.city_matches(x, w, c, None)).count())
            }
            Self::RemainingCivs(x) => {
                count(w.civs().filter(|&p| f.civ_matches(x, w, p, civ)).count())
            }
            Self::BuildingsMatching(s) => count(
                w.civ_cities(civ?).map(|c| buildings_in(t, s, w.city_buildings(c).iter())).sum(),
            ),
        })
    }

    /// What the count reads (DESIGN.md 5.8), its filter's leaves included.
    #[must_use]
    pub fn deps(self, filters: &Filters) -> CondDeps {
        match self {
            Self::Int(_) | Self::Stat(Stat::Production) => CondDeps::empty(),
            Self::Turns => CondDeps::TURN,
            Self::Cities => CondDeps::CITY_COUNT,
            Self::Units => CondDeps::UNIT_SET,
            Self::CompletedBranches => CondDeps::POLICIES,
            Self::Stat(Stat::Food) => CondDeps::CITY,
            Self::Stat(_) => CondDeps::STOCKS,
            Self::UnitsMatching(x) => CondDeps::UNIT_SET | filters.unit(x).deps(),
            Self::CitiesMatching(x) => CondDeps::CITY_COUNT | filters.city(x).deps(),
            Self::RemainingCivs(x) => CondDeps::CITY_COUNT | filters.civ(x).deps(),
            Self::BuildingsMatching(_) => CondDeps::CIV_BUILDINGS,
        }
    }
}

/// How many of `buildings` the static filter `s` selects.
fn buildings_in(t: &UniqueTable, s: SetRef, buildings: impl Iterator<Item = BuildingId>) -> usize {
    buildings.filter(|&b| t.in_set(s, b)).count()
}

/// Python's `int()` of a stock: toward zero, saturating.
#[allow(
    clippy::cast_possible_truncation,
    reason = "saturating by definition of `as`, as documented"
)]
fn truncate(x: f64) -> i64 {
    // `as` truncates toward zero and saturates, NaN to 0; stocks are finite.
    x as i64
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
