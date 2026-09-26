//! Everything the recap needs (`EngineGame.replay_data`, `engine_api.py:812-827`; DESIGN.md
//! 4.11): the map, the players, and the whole game's frames, statistics, events, messages,
//! thoughts, negotiations and deals.
//!
//! The frames come in two formats ([`ReplayFormat`]):
//! - `Full` is Python's frame shape, which `replay.js` decodes today (`replay.js:91-118`): each
//!   frame's owner, improvement, route and feature layers and each major's explored tiles in
//!   full, base64 of a byte per tile, with its cities, units (by type name) and event range. The
//!   improvement and feature layers number into the replay's `improvement_ids` and
//!   `feature_ids`, which are the current ruleset's; a frame recorded under another ruleset is
//!   renumbered by name. It is kept for the transition: expanding every frame recreates the
//!   payload the journal's deltas removed.
//! - `Delta` is the frames as the journal stores them: keyframes and deltas, each record's bytes
//!   in base64 (`save::journal`'s module doc has the layout), for the client that decodes them
//!   itself (Phase 3).
//!
//! A frame that does not decode (a journal chunk lost before a load) is left out of `Full`, and
//! the frames after it until the next keyframe with it.

use serde_json::{Map, Value, json};

use super::client::stats_row_json;
use super::events::event_json;
use super::players::kind_name;
use crate::base::codec::b64_encode;
use crate::game::Game;
use crate::rules::Ruleset;
use crate::save::journal::{FrameDecoder, FullFrame};
use crate::state::Phase;
use crate::state::chronicle::FrameRecord;
use crate::state::diplo::Deal;

/// The frames' format in [`Game::replay_data`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ReplayFormat {
    /// Every frame whole, in Python's shape.
    #[default]
    Full,
    /// Keyframes and deltas, as stored.
    Delta,
}

/// Where each name of `from` sits in `to`, plus 1 (0 for none): a frame's layer numbering
/// renumbered into the current ruleset's.
fn renumber(from: &[Box<str>], to: &[Box<str>]) -> Vec<u8> {
    let mut map = vec![0u8];
    map.extend(from.iter().map(|n| {
        to.iter().position(|x| x == n).and_then(|i| u8::try_from(i + 1).ok()).unwrap_or(0)
    }));
    map
}

/// One layer in the current numbering.
fn layer(bytes: &[u8], map: Option<&[u8]>) -> String {
    match map {
        None => b64_encode(bytes),
        Some(m) => {
            let out: Vec<u8> =
                bytes.iter().map(|&b| m.get(usize::from(b)).copied().unwrap_or(0)).collect();
            b64_encode(&out)
        }
    }
}

/// A frame in Python's shape (`victory.record_frame`), numbered as `palette` numbers them.
#[must_use]
pub fn full_frame_json(f: &FullFrame, palette: &crate::save::journal::FramePalette) -> Value {
    let same_imp = f.palette.improvement == palette.improvement;
    let same_feat = f.palette.feature == palette.feature;
    let imp_map = (!same_imp).then(|| renumber(&f.palette.improvement, &palette.improvement));
    let feat_map = (!same_feat).then(|| renumber(&f.palette.feature, &palette.feature));
    let tiles = f.owner.len();
    let explored: Map<String, Value> = f
        .explored
        .iter()
        .map(|(p, set)| {
            let mut bytes = vec![0u8; tiles];
            for i in set.iter() {
                if let Some(b) = bytes.get_mut(i as usize) {
                    *b = 1;
                }
            }
            (p.0.to_string(), json!(b64_encode(&bytes)))
        })
        .collect();
    let cities: Vec<Value> = f
        .cities
        .iter()
        .map(|c| json!([c.id, &*c.name, c.owner, c.tile, c.pop, c.capital]))
        .collect();
    let units: Vec<Value> = f
        .units
        .iter()
        .map(|u| {
            let name = f.palette.unit.get(usize::from(u.base)).map_or("", |n| &**n);
            json!([name, u.owner, u.tile, u.hp])
        })
        .collect();
    json!({
        "turn": f.turn,
        "owner": b64_encode(&f.owner),
        "improvement": layer(&f.improvement, imp_map.as_deref()),
        "route": b64_encode(&f.route),
        "feature": layer(&f.feature, feat_map.as_deref()),
        "cities": cities,
        "units": units,
        "explored": explored,
        "event_range": [f.event_range.0, f.event_range.1],
    })
}

/// The chronicle's frames decoded, in order; a frame that does not decode is left out, and so
/// are the deltas after it until the next keyframe.
#[must_use]
pub fn decode_frames(records: &[FrameRecord]) -> Vec<FullFrame> {
    let mut dec = FrameDecoder::new();
    let mut out = Vec::with_capacity(records.len());
    for rec in records {
        match dec.apply(rec) {
            Ok(f) => out.push(f),
            Err(_) => dec = FrameDecoder::new(),
        }
    }
    out
}

/// The palette of the current ruleset: what `Full` frames number into.
fn current_palette(r: &Ruleset) -> crate::save::journal::FramePalette {
    let features = &r.derived().features;
    crate::save::journal::FramePalette {
        improvement: r.improvements().as_slice().iter().map(|i| i.name.clone()).collect(),
        feature: features.as_slice().iter().map(|&t| Box::from(r.name(t).unwrap_or(""))).collect(),
        unit: r.base_units().as_slice().iter().map(|u| u.name.clone()).collect(),
    }
}

/// A concluded deal as Python kept it: `id`, `turn`, `parties`, `terms` by player, the
/// `ongoing` payments (each item with `from`, `to` and `until`) and whether it is `active`.
#[must_use]
pub fn deal_json(g: &Game, d: &Deal) -> Value {
    let r = g.rules();
    let ongoing: Vec<Value> = d
        .ongoing
        .iter()
        .map(|o| {
            let mut v = o.item.to_json(r).unwrap_or_else(|| json!({}));
            if let Some(m) = v.as_object_mut() {
                m.insert("from".into(), json!(o.from.0));
                m.insert("to".into(), json!(o.to.0));
                m.insert("until".into(), json!(o.until));
            }
            v
        })
        .collect();
    json!({
        "id": d.id.get(),
        "turn": d.turn,
        "parties": [d.parties[0].0, d.parties[1].0],
        "terms": d.terms.to_json(r),
        "ongoing": ongoing,
        "active": d.active,
        "summary": &*d.summary,
    })
}

impl Game {
    /// Everything the recap needs (`EngineGame.replay_data`), as JSON bytes: the map's shape and
    /// terrain, the players, the frames in `format`, and the whole game's statistics, events (as
    /// they happened), messages, thoughts, negotiations and deals, with where the game stands.
    #[must_use]
    pub fn replay_data(&self, format: ReplayFormat) -> Vec<u8> {
        serde_json::to_vec(&self.replay_json(format)).unwrap_or_default()
    }

    /// [`Game::replay_data`] as a value.
    #[must_use]
    pub fn replay_json(&self, format: ReplayFormat) -> Value {
        let g = self;
        let r = g.rules();
        let st = g.state();
        let hill = r.derived().known.hill;
        let terrain: Vec<Value> = st
            .tiles()
            .iter()
            .map(|(_, t)| {
                let hills = t.features().contains(hill);
                json!([
                    r.name(t.terrain()),
                    u8::from(hills),
                    t.river_mask(),
                    t.resource().and_then(|x| r.name(x)),
                    t.wonder().and_then(|x| r.name(x)),
                ])
            })
            .collect();
        let palette = current_palette(r);
        let players: Vec<Value> = st
            .players()
            .iter()
            .map(|(id, p)| {
                json!({
                    "id": id.0, "name": &*p.name, "leader": &*p.leader, "color": p.color.to_hex(),
                    "kind": kind_name(p.kind), "alive": p.alive(), "eliminated_turn": p.eliminated_turn(),
                })
            })
            .collect();
        let chron = g.chronicle();
        let frames: Vec<Value> = match format {
            ReplayFormat::Full => decode_frames(&chron.frames().frames)
                .iter()
                .map(|f| full_frame_json(f, &palette))
                .collect(),
            ReplayFormat::Delta => chron
                .frames()
                .frames
                .iter()
                .map(|f| json!({"turn": f.turn, "keyframe": f.keyframe, "bytes": b64_encode(&f.bytes)}))
                .collect(),
        };
        let events: Vec<Value> =
            chron.events().iter().map(|e| event_json(g, e, &g.event_view(e, None), None)).collect();
        let messages: Vec<Value> = chron
            .messages()
            .iter()
            .map(|m| {
                let to: Vec<u8> = m.to.iter().map(|p| p.0).collect();
                json!({"id": m.id.get(), "turn": m.turn, "from": m.from.0, "to": to, "text": &*m.text})
            })
            .collect();
        let thoughts: Vec<Value> = chron
            .thoughts()
            .iter()
            .map(|t| json!({"turn": t.turn, "player": t.player.0, "text": &*t.text, "kind": t.kind.as_deref()}))
            .collect();
        let negotiations: Vec<Value> =
            g.negotiations().iter().map(|n| super::stored_negotiation(g, n)).collect();
        let deals: Vec<Value> = st.diplo().deals.iter().map(|d| deal_json(g, d)).collect();
        let clock = st.clock();
        let map = st.map();
        json!({
            "format": match format { ReplayFormat::Full => "full", ReplayFormat::Delta => "delta" },
            "width": map.width,
            "height": map.height,
            "wrap_x": map.wrap_x,
            "wrap_y": map.wrap_y,
            "terrain": terrain,
            "improvement_ids": palette.improvement,
            "feature_ids": palette.feature,
            "unit_ids": palette.unit,
            "players": players,
            "frames": frames,
            "stats": chron.stats().iter().map(stats_row_json).collect::<Vec<_>>(),
            "events": events,
            "messages": messages,
            "thoughts": thoughts,
            "negotiations": negotiations,
            "deals": deals,
            "winner": clock.winner.map(|p| p.0),
            "victory": crate::game::victory::won_by(g).map(|w| w.name(r)),
            "phase": match clock.phase { Phase::Playing => "playing", Phase::Over => "over" },
            "turn": clock.turn,
            "config": super::client::config_json(g),
        })
    }
}
