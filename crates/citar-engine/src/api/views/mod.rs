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
//! ([`num::round_ndigits`]), gold and culture cut to integers. [`Game::view_json`] writes the client
//! view straight to JSON bytes, its largest part (the tiles) from typed rows.
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

/// A player's name, or `""` for one the game lacks.
pub(crate) fn name_of(g: &Game, p: PlayerId) -> &str {
    g.player(p).map_or("", |x| &x.name)
}

/// A name, or `unknown` when `viewer` has not met its owner (`views.py:572-573, 607-608`).
pub(crate) fn known_name(g: &Game, viewer: PlayerId, p: PlayerId) -> Value {
    if p == viewer || g.has_met(viewer, p) { json!(name_of(g, p)) } else { json!("unknown") }
}
