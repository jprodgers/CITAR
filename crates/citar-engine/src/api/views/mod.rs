//! The views: the game as one player may see it, in the shapes the web client and the query
//! tools read (DESIGN.md 8.1, 8.4; package 1d-02).
//!
//! Replaces `citar/engine/views.py` (the `*_info` builders and `client_view`), the view-side
//! event scrubbing of `game.py:880-990` as JSON, `espionage.espionage_view`
//! (`espionage.py:501-515`), `briefing.alert_items` (`briefing.py:168-316`, which the client view
//! carries and package 1d-03's briefing reads), and the facade's `empire_summary`, `standings`,
//! `path_preview` and `replay_data` (`engine_api.py:483-497, 526-537, 735-746, 812-827`).
//!
//! Every builder takes `&Game` and a viewer: `Some(pid)` for a player, `None` for a spectator,
//! who sees everything. They read the memos as the rules do and never change the game, its
//! revision or its digest (property P8). Their answers are `serde_json::Value`s with Python's
//! keys, and numbers as Python wrote them: yields rounded with `round(x, n)`
//! ([`num::round_ndigits`]), gold and culture cut to integers, and each number an int or a
//! float as Python's was ([`PyNum`] where Python's kind varied). Refcheck's comparator takes 3
//! and 3.0 as equal, so `crates/citar-refcheck/tests/query_tools.rs` checks the kinds apart. The
//! one exception is a city's food stored, always a float, where Python's was an int after some
//! resets and a float otherwise, a history no state keeps.
//!
//! A unit's and a city's summary are typed as well ([`units::UnitView`], [`cities::CityView`]),
//! so that [`Game::view_json`] writes the client view straight to JSON bytes: its tiles, units
//! and cities, most of it, from typed values, the rest from the builders' values.
//!
//! - [`units`]: `unit_info`, with the owner's detail (`views.py:50-157`);
//! - [`cities`]: `city_info`, with the owner's detail (`views.py:162-249`);
//! - [`tiles`]: `tile_info` and a tile as the viewer knows it (`views.py:255-302`);
//! - [`empire`]: `empire_info`, `empire_summary` and `standings` (`views.py:308-346`);
//! - [`players`]: `players_overview`, the agreements, `trade_options`, `diplomacy_info` and
//!   `city_states_info` (`views.py:349-502`);
//! - [`info`]: `tech_tree`, `policies_info`, `religion_info`, `great_people_info`,
//!   `victory_info` and `espionage_view` (`views.py:505-617`);
//! - [`events`]: an event as JSON for a viewer, the UN tally named (`game.py:880-990`);
//! - [`alerts`]: what needs attention this turn (`briefing.alert_items`);
//! - [`client`]: `client_view` and [`Game::view_json`] (`views.py:716-763`), and `path_preview`;
//! - [`replay`]: [`Game::replay_data`] in the full and the delta formats (DESIGN.md 4.11).

use serde_json::{Map, Value, json};

use crate::base::ids::{PlayerId, TileIdx};
use crate::base::num;
use crate::base::stats::{Stat, Stats};
use crate::game::Game;
use crate::unique::table::SourceUniques;

pub mod alerts;
pub mod cities;
pub mod client;
pub mod empire;
pub mod events;
pub mod info;
pub mod players;
pub mod replay;
pub mod tiles;
pub mod units;

pub use self::client::{ClientView, PathPreview};
pub use self::empire::{EmpireSummary, Standing};
pub use self::replay::ReplayFormat;

/// JSON as Python's `json.dumps` wrote the views' numbers: a float as its `repr` (`2.0`,
/// `1e+16`, `5e-05`, through [`PyFloat`](crate::base::fmt::PyFloat)), where `serde_json` would
/// write `1e16` and `5e-5`.
#[derive(Clone, Copy, Debug, Default)]
pub struct PyJson;

impl serde_json::ser::Formatter for PyJson {
    fn write_f64<W: ?Sized + std::io::Write>(&mut self, w: &mut W, x: f64) -> std::io::Result<()> {
        // Most of a view's floats are whole: written at once, as `repr` writes them.
        if x.fract() == 0.0 && x.abs() < 1e15 {
            #[allow(clippy::cast_possible_truncation, reason = "a whole number below 1e15")]
            let n = x as i64;
            if n == 0 && x.is_sign_negative() {
                return w.write_all(b"-0.0");
            }
            return write!(w, "{n}.0");
        }
        write!(w, "{}", crate::base::fmt::PyFloat(x))
    }
}

/// A number of the kind Python's value had: `json.dumps` wrote an int as `4` and a float as
/// `4.0`, and the few answers whose kind was not always the same keep it (a city-state's
/// happiness, the luxuries' happiness).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PyNum {
    Int(i64),
    Float(f64),
}

impl PyNum {
    /// `x` as an int when Python's value was one: `whole` says whether its inputs were whole,
    /// and `x` must be whole and within the integers a float holds exactly.
    #[must_use]
    pub fn int_if(whole: bool, x: f64) -> Self {
        const EXACT: f64 = 9_007_199_254_740_992.0; // 2^53
        if whole && x.fract() == 0.0 && x.abs() < EXACT {
            #[allow(clippy::cast_possible_truncation, reason = "a whole number below 2^53")]
            let n = x as i64;
            Self::Int(n)
        } else {
            Self::Float(x)
        }
    }

    /// The value, whatever its kind.
    #[must_use]
    #[allow(clippy::cast_precision_loss, reason = "an int from a float below 2^53")]
    pub const fn as_f64(self) -> f64 {
        match self {
            Self::Int(n) => n as f64,
            Self::Float(x) => x,
        }
    }
}

impl serde::Serialize for PyNum {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match *self {
            Self::Int(n) => s.serialize_i64(n),
            Self::Float(x) => s.serialize_f64(x),
        }
    }
}

/// `v` as JSON bytes, its floats as Python wrote them ([`PyJson`]).
#[must_use]
pub fn to_py_json<T: serde::Serialize + ?Sized>(v: &T) -> Vec<u8> {
    let mut out = Vec::with_capacity(4096);
    let mut ser = serde_json::Serializer::with_formatter(&mut out, PyJson);
    // A view holds text, numbers, lists and maps with text keys, none of which fails to
    // serialise; the empty view stands in should that ever change.
    if v.serialize(&mut ser).is_err() {
        return Vec::new();
    }
    out
}

/// A tile as `[x, y]` (`views._xy`).
pub(crate) fn xy(g: &Game, t: TileIdx) -> Value {
    let (x, y) = g.xy(t);
    json!([x, y])
}

/// Stats rounded for display, the zero ones dropped (`views._round`): the `keys` given, in that
/// order, each `round(x, nd)`.
pub(crate) fn rounded(s: &Stats, keys: &[Stat], nd: i32) -> Value {
    let m: Map<String, Value> = keys
        .iter()
        .filter(|&&k| s[k] != 0.0)
        .map(|&k| (k.key().to_owned(), json!(num::round_ndigits(s[k], nd))))
        .collect();
    Value::Object(m)
}

/// The six yields a city shows, in Python's order (`views.py:187`).
pub(crate) const CITY_YIELDS: [Stat; 6] =
    [Stat::Food, Stat::Production, Stat::Gold, Stat::Science, Stat::Culture, Stat::Faith];

/// The yields an empire reports per turn (`views.py:322`).
pub(crate) const EMPIRE_YIELDS: [Stat; 4] = [Stat::Gold, Stat::Science, Stat::Culture, Stat::Faith];

/// The rule text shown to players: an object's uniques as the ruleset wrote them, UnCiv's AI
/// weighting hints left out (`views._uniques`).
pub(crate) fn rule_text(g: &Game, u: &SourceUniques) -> Vec<String> {
    let t = g.rules().uniques();
    u.ids()
        .map(|id| t.text_of(id))
        .filter(|s| !s.contains("for AI decisions"))
        .map(str::to_owned)
        .collect()
}

/// A negotiation as Python kept it in the state (`diplomacy.py:782`), which a spectator's view and
/// the replay hand out: `negotiation_json`'s keys, with `exchanges` (the entries in its history)
/// and a history entry's `note` only where it has one.
#[must_use]
pub fn stored_negotiation(g: &Game, n: &crate::state::diplo::Negotiation) -> Value {
    let mut v = crate::game::diplomacy::negotiation::negotiation_json(g, n);
    if let Some(m) = v.as_object_mut() {
        m.insert("exchanges".into(), json!(n.history.len()));
        if let Some(Value::Array(h)) = m.get_mut("history") {
            for e in h.iter_mut().filter_map(Value::as_object_mut) {
                if e.get("note").is_some_and(Value::is_null) {
                    e.shift_remove("note");
                }
            }
        }
    }
    v
}

/// A message between civilizations as Python kept it (`diplomacy.py:306-310`).
pub(crate) fn message_json(m: &crate::state::chronicle::Message) -> Value {
    let to: Vec<u8> = m.to.iter().map(|p| p.0).collect();
    json!({
        "id": m.id.get(),
        "turn": m.turn,
        "from": m.from.0,
        "to": to,
        "text": &*m.text,
    })
}

/// A seat's recorded thought as Python kept it (`engine_api.py:642-645`).
pub(crate) fn thought_json(t: &crate::state::chronicle::Thought) -> Value {
    json!({
        "turn": t.turn,
        "player": t.player.0,
        "text": &*t.text,
        "kind": t.kind.as_deref(),
    })
}

/// A player's name, or `""` for one the game lacks.
pub(crate) fn name_of(g: &Game, p: PlayerId) -> &str {
    g.player(p).map_or("", |x| &x.name)
}

/// A name, or `unknown` when `viewer` has not met its owner (`views.py:572-573, 607-608`).
pub(crate) fn known_name(g: &Game, viewer: PlayerId, p: PlayerId) -> Value {
    if p == viewer || g.has_met(viewer, p) { json!(name_of(g, p)) } else { json!("unknown") }
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests;
