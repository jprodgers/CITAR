//! A save's headline facts, read without loading it (DESIGN.md 4.9).
//!
//! A host lists saves and scenarios with their turn, map and civilizations. Reading those needs
//! no ruleset and no game: [`summary`] reads the few parts it needs and skips the rest unparsed,
//! and the scores come from the newest stats row the state keeps (`chronicle.last_stats`).
//!
//! Replaces `engine_api.state_summary` (`citar/engine_api.py:158-178`), keeping its keys:
//! `turn`, `phase`, `turn_limit`, `map_size`, `map_type` (`custom` for an editor map), `winner`
//! (a name) and `winner_id`, `names` (every player by id), `majors` and `scores` (each major's
//! score in the newest stats row; empty before the first).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{LoadError, json, migrate};
use crate::state::players::PlayerKind;

/// What [`summary`] reads of a save.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub turn: i32,
    /// `playing` or `over`.
    pub phase: String,
    pub turn_limit: i32,
    /// The lobby size's key: `standard`.
    pub map_size: String,
    /// The map type's key, or `custom` for an editor map.
    pub map_type: String,
    /// The winner's name, once there is one.
    pub winner: Option<String>,
    pub winner_id: Option<u8>,
    /// Every player's name, by id.
    pub names: BTreeMap<u8, String>,
    pub majors: Vec<MajorSummary>,
    /// Each major's score in the newest stats row, by id; empty before the first row.
    pub scores: BTreeMap<u8, i32>,
}

/// A major civilization in a [`Summary`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MajorSummary {
    pub id: u8,
    pub name: String,
    /// The nation's name.
    pub nation: String,
    pub alive: bool,
}

#[derive(Deserialize)]
struct Head {
    format: String,
    version: u32,
    clock: ClockHead,
    config: ConfigHead,
    players: Vec<PlayerHead>,
    chronicle: ChronicleHead,
}

#[derive(Deserialize)]
struct ClockHead {
    turn: i32,
    phase: String,
    winner: Option<u8>,
}

#[derive(Deserialize)]
struct ConfigHead {
    turn_limit: i32,
    map: MapHead,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum MapHead {
    Generated { size: String, map_type: String },
    Editor { size: String },
}

#[derive(Deserialize)]
struct PlayerHead {
    id: u8,
    kind: PlayerKind,
    name: String,
    nation: String,
    alive: bool,
}

#[derive(Deserialize)]
struct ChronicleHead {
    last_stats: Option<RowHead>,
}

#[derive(Deserialize)]
struct RowHead {
    civs: Vec<CivHead>,
}

#[derive(Deserialize)]
struct CivHead {
    player: u8,
    score: i32,
}

/// The headline facts of the save `bytes`, without loading it or needing its ruleset.
pub fn summary(bytes: &[u8]) -> Result<Summary, LoadError> {
    let head: Head = serde_json::from_slice(bytes).map_err(|e| LoadError::Json(e.to_string()))?;
    if head.format != json::FORMAT {
        return Err(LoadError::Json(format!("not a {} document", json::FORMAT)));
    }
    if !(migrate::OLDEST..=migrate::CURRENT).contains(&head.version) {
        return Err(LoadError::Version(head.version));
    }
    let (map_size, map_type) = match head.config.map {
        MapHead::Generated { size, map_type } => (size, map_type),
        MapHead::Editor { size } => (size, "custom".to_owned()),
    };
    let winner_id = head.clock.winner;
    let winner =
        winner_id.and_then(|w| head.players.iter().find(|p| p.id == w)).map(|p| p.name.clone());
    let names = head.players.iter().map(|p| (p.id, p.name.clone())).collect();
    let majors: Vec<MajorSummary> = head
        .players
        .iter()
        .filter(|p| p.kind == PlayerKind::Major)
        .map(|p| MajorSummary {
            id: p.id,
            name: p.name.clone(),
            nation: p.nation.clone(),
            alive: p.alive,
        })
        .collect();
    let scores = match &head.chronicle.last_stats {
        None => BTreeMap::new(),
        Some(row) => majors
            .iter()
            .map(|m| {
                let score = row.civs.iter().find(|c| c.player == m.id).map_or(0, |c| c.score);
                (m.id, score)
            })
            .collect(),
    };
    Ok(Summary {
        turn: head.clock.turn,
        phase: head.clock.phase,
        turn_limit: head.config.turn_limit,
        map_size,
        map_type,
        winner,
        winner_id,
        names,
        majors,
        scores,
    })
}
