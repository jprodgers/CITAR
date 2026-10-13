//! Everything the recap needs (`EngineGame.replay_data`, `engine_api.py:812-827`; DESIGN.md
//! 4.11): the map, the players, and the whole game's frames, statistics, events, messages,
//! thoughts, negotiations and deals.
//!
//! The frames are sent as the journal stores them: keyframes and deltas, each record's bytes in
//! base64 (`save::journal`'s module doc has the layout), which the browser decodes itself
//! (`citar/web/js/replaydecode.js`). Python's format, every frame expanded on the server with each
//! layer and each major's explored tiles whole, went with package 3-13 (design/phase3-features.md
//! P3.5.8): it recreated on every request the payload the journal's deltas removed.
//!
//! Only the records that decode are sent, in order: a frame that does not (a journal chunk lost
//! before a load) is left out, and so are the deltas after it until the next keyframe, which a
//! decoder could not apply. Each delta names its base by a hash the browser does not compute, so
//! the server, which can, sends a chain that holds.

use serde_json::{Value, json};

use super::client::stats_row_json;
use super::events::event_json;
use super::players::kind_name;
use crate::base::codec::b64_encode;
use crate::game::Game;
use crate::rules::Ruleset;
use crate::save::journal::{FrameDecoder, FramePalette};
use crate::state::Phase;
use crate::state::chronicle::FrameRecord;
use crate::state::diplo::Deal;

/// The records of `records` that decode, in order; a record that does not decode is left out,
/// and so are the deltas after it until the next keyframe.
#[must_use]
pub fn decodable_frames(records: &[FrameRecord]) -> Vec<&FrameRecord> {
    let mut dec = FrameDecoder::new();
    let mut out = Vec::with_capacity(records.len());
    for rec in records {
        match dec.apply(rec) {
            Ok(_) => out.push(rec),
            Err(_) => dec = FrameDecoder::new(),
        }
    }
    out
}

/// The palette of the current ruleset: the improvement, feature and unit type names it numbers
/// (a frame's keyframe carries the palette it was recorded under, which names a frame of another
/// ruleset).
fn current_palette(r: &Ruleset) -> FramePalette {
    let features = &r.derived().features;
    FramePalette {
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
    /// terrain, the current ruleset's palettes, the players, the frames as stored (keyframes and
    /// deltas), and the whole game's statistics, events (as they happened), messages, thoughts,
    /// negotiations and deals, with where the game stands.
    #[must_use]
    pub fn replay_data(&self) -> Vec<u8> {
        super::to_py_json(&self.replay_json())
    }

    /// [`Game::replay_data`] as a value.
    #[must_use]
    pub fn replay_json(&self) -> Value {
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
                    "id": id.0,
                    "name": &*p.name,
                    "leader": &*p.leader,
                    "color": p.color.to_hex(),
                    "kind": kind_name(p.kind),
                    "alive": p.alive(),
                    "eliminated_turn": p.eliminated_turn(),
                })
            })
            .collect();
        let chron = g.chronicle();
        let frames: Vec<Value> = decodable_frames(&chron.frames().frames)
            .into_iter()
            .map(|f| json!({"turn": f.turn, "keyframe": f.keyframe, "bytes": b64_encode(&f.bytes)}))
            .collect();
        let events: Vec<Value> =
            chron.events().iter().map(|e| event_json(g, e, &g.event_view(e, None), None)).collect();
        let messages: Vec<Value> = chron.messages().iter().map(super::message_json).collect();
        let thoughts: Vec<Value> = chron.thoughts().iter().map(super::thought_json).collect();
        let negotiations: Vec<Value> =
            g.negotiations().iter().map(|n| super::stored_negotiation(g, n)).collect();
        let deals: Vec<Value> = st.diplo().deals.iter().map(|d| deal_json(g, d)).collect();
        let clock = st.clock();
        let map = st.map();
        json!({
            "format": "delta",
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

#[cfg(test)]
mod tests {
    use super::decodable_frames;
    use crate::save::journal::{FramePalette, FrameWriter, FullFrame};
    use crate::state::chronicle::FrameRecord;

    fn names(xs: &[&str]) -> Vec<Box<str>> {
        xs.iter().map(|&x| Box::from(x)).collect()
    }

    /// A frame of four tiles, with `owner` on the first.
    fn frame(turn: i32, owner: u8) -> FullFrame {
        FullFrame {
            turn,
            palette: FramePalette {
                improvement: names(&["Farm", "Mine"]),
                feature: names(&["Forest"]),
                unit: names(&["Warrior"]),
            },
            owner: vec![owner, 255, 255, 255],
            improvement: vec![0; 4],
            route: vec![0; 4],
            feature: vec![0; 4],
            ..FullFrame::default()
        }
    }

    fn turns(recs: &[&FrameRecord]) -> Vec<i32> {
        recs.iter().map(|r| r.turn).collect()
    }

    #[test]
    fn a_frame_that_does_not_decode_is_left_out_until_the_next_keyframe() {
        let frames: Vec<FullFrame> = (1..=5_u8).map(|t| frame(i32::from(t), t)).collect();
        // Three frames from one writer, then two from a fresh one, as after a load.
        let mut w = FrameWriter::new();
        let mut recs: Vec<FrameRecord> = frames[..3].iter().map(|f| w.push(f)).collect();
        let mut w = FrameWriter::new();
        recs.extend(frames[3..].iter().map(|f| w.push(f)));
        let keys: Vec<bool> = recs.iter().map(|r| r.keyframe).collect();
        assert_eq!(keys, [true, false, false, true, false]);
        assert_eq!(turns(&decodable_frames(&recs)), [1, 2, 3, 4, 5], "every record decodes");

        // The second record cut short: it and the delta taken from it are left out.
        let mut cut = recs.clone();
        let half = cut[1].bytes.len() / 2;
        cut[1].bytes = cut[1].bytes[..half].into();
        assert_eq!(turns(&decodable_frames(&cut)), [1, 4, 5], "a record cut short");
        // The second record lost: the third is a delta from a frame the decoder never saw.
        let mut lost = recs.clone();
        lost.remove(1);
        assert_eq!(turns(&decodable_frames(&lost)), [1, 4, 5], "a record lost");
        // No keyframe after the loss: nothing more decodes.
        assert_eq!(turns(&decodable_frames(&lost[..2])), [1], "no keyframe to resume at");
        // What is sent is the record as stored.
        assert!(decodable_frames(&recs).iter().zip(&recs).all(|(a, b)| *a == b));
    }
}
