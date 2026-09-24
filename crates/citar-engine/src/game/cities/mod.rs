//! Cities (`cities.py`).
//!
//! Package 1b-05 ports the uniques that apply in a city ([`uniques`], `cities.py:48-111`).
//! Package 1b-06 ports a city's yields and happiness ([`stats`], `cities.py:139-700`), its
//! citizens ([`citizens`], `cities.py:706-926`), the links to its capital ([`connections`],
//! `cities.py:1967-2072`), and, as far as its scripts need them, founding a city and adding
//! buildings ([`founding`]). Package 1b-07 ports the rest: borders, construction, purchase, the
//! queue, free buildings and the city's life (DESIGN.md 3.3).

pub mod citizens;
pub mod connections;
pub mod founding;
pub mod stats;
pub mod uniques;
